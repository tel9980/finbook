//! 资金调拨（账户间转账，对标金蝶「资金调拨单」/ 用友「内部结算」）。
//!
//! 一笔调拨 = 一张单据 + 一张凭证：**借 转入账户 / 贷 转出账户**，金额相等、同币种。
//! 与 [`crate::receipt`] 的收付款单**同构**（建单草稿 → 审核出凭证 → 撤审 / 删除链），
//! 只有两点不同：
//!
//! - 没有往来单位，**不产生核销**：钱只是从一个账户挪到另一个账户，往来账不变；
//! - 两端都是资金账户，转出 / 转入必填且**不能相同**（同户调拨是空动作）。
//!
//! 口径：
//! - 资金账户 = 现金（1001）/ 银行存款（1002 系）科目，判定复用科目表的
//!   `is_cash / is_bank`（与资金日报 `funds_daily` 同一集合，见 `crate::funds`）；
//! - 资金腿辅助核算取 `crate::funds::fund_account_aux`（1002 系挂 `bank=B01`），
//!   与收付款单共用一处，避免「调拨在银行对账里查不到」这种静默漏检；
//! - 凭证走 `vouchers::save_in`，草稿状态（H-3：草稿不入余额），由会计记账。

use chrono::NaiveDate;
use fincore::{Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 资金调拨单
#[derive(Clone, Debug, serde::Serialize)]
pub struct FundTransfer {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    /// 转出资金账户（末级，如 100201）
    pub from_account: String,
    /// 转入资金账户（末级，如 1001）
    pub to_account: String,
    /// 资金账户辅助编码（银行 B01/B02 或微信 WECHAT，空则按账户推断默认 B01）
    pub bank: String,
    pub amount: Money,
    pub memo: String,
    /// 生成的记账凭证（草稿阶段为 None，审核时生成）
    #[serde(default)]
    pub voucher_id: Option<i64>,
    /// draft 待审核 / audited 已审核
    #[serde(default)]
    pub status: String,
    pub created_by: String,
    pub created_at: String,
}

fn map_doc(r: &rusqlite::Row) -> rusqlite::Result<FundTransfer> {
    let d: String = r.get(3)?;
    Ok(FundTransfer {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1977, 1, 1).unwrap()),
        from_account: r.get(4)?,
        to_account: r.get(5)?,
        bank: r.get(6)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
        memo: r.get(8)?,
        voucher_id: r.get(9)?,
        created_by: r.get(10)?,
        created_at: r.get(11)?,
        status: r.get(12)?,
    })
}

const T_COLS: &str = "id,no,period,date,from_account,to_account,bank,amount,memo,\
     voucher_id,created_by,created_at,status";

pub fn transfer_list(db: &Db) -> DbResult<Vec<FundTransfer>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {T_COLS} FROM fund_transfer ORDER BY date DESC, id DESC"
    ))?;
    let rows = st.query_map([], map_doc)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn transfer_get(db: &Db, id: i64) -> DbResult<Option<FundTransfer>> {
    db.conn()
        .query_row(
            &format!("SELECT {T_COLS} FROM fund_transfer WHERE id=?1"),
            rusqlite::params![id],
            map_doc,
        )
        .optional()
        .map_err(Into::into)
}

/// 校验某个编码是现金 / 银行科目（资金账户）。
///
/// 与资金日报同一集合 `is_cash || is_bank`；对自定义账套再放宽到 1001 / 1002 前缀
/// —— 内置科目表里 1002 的**一级**科目刻意不标 `is_bank`（防重复统计，见
/// `fincore::chart`），若只认标记，用户写「1002」会被无理由拦下。
fn ensure_fund_account(db: &Db, code: &str, label: &str) -> DbResult<()> {
    let code = code.trim();
    if code.is_empty() {
        return Err(fincore::FinError::validate(format!("{label}不能为空")).into());
    }
    match crate::accounts::get(db, code)? {
        None => Err(fincore::FinError::validate(format!("{label}科目 {code} 不存在")).into()),
        Some(a) => {
            let money =
                a.is_cash || a.is_bank || code.starts_with("1001") || code.starts_with("1002");
            if money {
                Ok(())
            } else {
                Err(fincore::FinError::validate(format!(
                    "{label} {code} 不是现金/银行科目，资金调拨只能在资金账户之间做"
                ))
                .into())
            }
        }
    }
}

/// 新建资金调拨单（**草稿，仅台账**）：凭证在「审核」时生成（对标金蝶录单 → 审核）。
///
/// 守卫：转出 / 转入必填且不同、都是现金 / 银行科目、金额 > 0。返回单据 id。
#[allow(clippy::too_many_arguments)]
pub fn transfer_create(
    db: &Db,
    date: NaiveDate,
    from_account: &str,
    to_account: &str,
    bank: &str,
    amount: Money,
    memo: &str,
    who: &str,
) -> DbResult<i64> {
    let from = from_account.trim();
    let to = to_account.trim();
    ensure_fund_account(db, from, "转出账户")?;
    ensure_fund_account(db, to, "转入账户")?;
    if from == to {
        return Err(fincore::FinError::validate("转出与转入不能是同一个账户").into());
    }
    if !amount.is_positive() {
        return Err(fincore::FinError::validate("调拨金额必须大于 0").into());
    }
    let period = Period::from_date(date);
    let tx = db.write_tx()?;
    let doc_no = format!("DB{}", chrono::Local::now().format("%y%m%d%H%M%S"));
    tx.execute(
        "INSERT INTO fund_transfer(no,period,date,from_account,to_account,bank,amount,memo,created_by,created_at,status)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'draft')",
        rusqlite::params![
            doc_no,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            from,
            to,
            bank,
            crate::money_param(amount),
            memo,
            who,
            now()
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(id)
}

/// 审核资金调拨单：同事务生成凭证（借 转入 / 贷 转出）→ 已审核。仅 draft 可审核
/// （条件更新防并发重复）。
///
/// 返回凭证 id。
pub fn transfer_audit(db: &Db, id: i64, who: &str) -> DbResult<i64> {
    let d = transfer_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("资金调拨单"))?;
    if d.status != "draft" {
        return Err(fincore::FinError::state("仅待审核的单据可审核").into());
    }
    if d.voucher_id.is_some() {
        return Err(fincore::FinError::state("单据已生成凭证").into());
    }
    let from = d.from_account.clone();
    let to = d.to_account.clone();
    let bank = if d.bank.trim().is_empty() {
        None
    } else {
        Some(d.bank.trim())
    };
    let amount = d.amount;
    let date = d.date;
    let period = d.period;
    let memo = d.memo.clone();

    let tx = db.write_tx()?;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = if memo.trim().is_empty() {
        format!("资金调拨 {from}→{to}")
    } else {
        memo.trim().to_string()
    };
    let summary = v.memo.clone();
    // 借 转入账户 / 贷 转出账户：钱从 from 流向 to，两腿金额相等
    v.push_entry(Entry {
        debit: amount,
        aux: crate::funds::fund_account_aux(&to, bank.as_deref()),
        ..Entry::new(1, to.as_str(), summary.as_str())
    });
    v.push_entry(Entry {
        credit: amount,
        aux: crate::funds::fund_account_aux(&from, bank.as_deref()),
        ..Entry::new(2, from.as_str(), summary.as_str())
    });
    let vid = crate::vouchers::save_in(&tx, &mut v)?;

    // 状态推进：仅 draft → audited（条件更新，防并发重复审核 / 重复出凭证）
    let n = tx.execute(
        "UPDATE fund_transfer SET voucher_id=?2, status='audited' WHERE id=?1 AND status='draft'",
        rusqlite::params![id, vid],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("单据状态已变化，请刷新后重试").into());
    }
    tx.commit()?;
    Ok(vid)
}

/// 撤销审核：删除其**未记账**凭证 → 单据回到草稿（可改可删）。
/// 凭证已记账时拒绝（先反记账再撤审）。
pub fn transfer_unaudit(db: &Db, id: i64) -> DbResult<()> {
    let d = transfer_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("资金调拨单"))?;
    if d.status != "audited" {
        return Err(fincore::FinError::state("仅已审核的单据可撤销审核").into());
    }
    let vid = d
        .voucher_id
        .ok_or_else(|| fincore::FinError::state("单据未关联凭证，请刷新"))?;
    let v = crate::vouchers::get(db, vid)?
        .ok_or_else(|| fincore::FinError::not_found("关联凭证已不存在，请刷新"))?;
    if v.status == fincore::VoucherStatus::Posted {
        return Err(fincore::FinError::state("凭证已记账，请先反记账后再撤销审核").into());
    }
    let tx = db.write_tx()?;
    let n = tx.execute(
        "UPDATE fund_transfer SET status='draft', voucher_id=NULL WHERE id=?1 AND status='audited'",
        rusqlite::params![id],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("单据状态已变化，请刷新后重试").into());
    }
    // 同连接事务内删除凭证（失败整体回滚）
    crate::vouchers::delete(db, vid)?;
    tx.commit()?;
    Ok(())
}

/// 删除资金调拨单：其凭证须先作废或删除，随后可删
pub fn transfer_delete(db: &Db, id: i64) -> DbResult<()> {
    let d = transfer_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("资金调拨单"))?;
    if let Some(vid) = d.voucher_id {
        let status = crate::vouchers::get(db, vid)?
            .map(|v| v.status)
            .unwrap_or(fincore::VoucherStatus::Void);
        if status != fincore::VoucherStatus::Void {
            return Err(
                fincore::FinError::state("该单已生成凭证，请先作废或删除凭证后再删单").into(),
            );
        }
    }
    db.conn().execute(
        "DELETE FROM fund_transfer WHERE id=?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    fn d(y: i32, mo: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, mo, dd).unwrap()
    }

    /// 审核出凭证：借 转入 / 贷 转出，两腿辅助核算各按账户规则取（银行挂 B01）。
    #[test]
    fn transfer_audit_creates_balanced_voucher() {
        let db = mem();
        // 默认科目表：1001 库存现金（现金）、100201 工行基本户（银行）
        let id = transfer_create(
            &db,
            d(2026, 1, 5),
            "100201",
            "1001",
            "",
            m("500"),
            "提现备用",
            "u",
        )
        .unwrap();
        // 草稿：不出凭证
        {
            let doc = transfer_get(&db, id).unwrap().unwrap();
            assert_eq!(doc.status, "draft");
            assert!(doc.voucher_id.is_none(), "草稿不应生成凭证");
        }
        let vid = transfer_audit(&db, id, "u").unwrap();
        assert_eq!(transfer_get(&db, id).unwrap().unwrap().status, "audited");
        assert!(transfer_audit(&db, id, "u").is_err(), "重复审核应拒绝");

        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert_eq!(v.entries.len(), 2);
        // 借 转入（1001 现金，无辅助）
        assert_eq!(v.entries[0].account_code, "1001");
        assert_eq!(v.entries[0].debit, m("500"));
        assert!(v.entries[0].aux.bank.is_none(), "现金腿不应挂银行辅助");
        // 贷 转出（100201 银行，挂 bank=B01）
        assert_eq!(v.entries[1].account_code, "100201");
        assert_eq!(v.entries[1].credit, m("500"));
        assert_eq!(
            v.entries[1].aux.bank.as_deref(),
            Some("B01"),
            "银行腿必须挂银行辅助（否则银行对账漏检）"
        );
    }

    /// 守卫：同户调拨 / 零金额 / 非资金科目 / 不存在的科目，建单即拦。
    #[test]
    fn transfer_create_guards() {
        let db = mem();
        let day = d(2026, 1, 5);
        // 转出=转入
        assert!(transfer_create(&db, day, "100201", "100201", "", m("1"), "", "u").is_err());
        // 零金额
        assert!(transfer_create(&db, day, "100201", "1001", "", Money::ZERO, "", "u").is_err());
        // 转入不是资金科目（6602 管理费用）
        assert!(transfer_create(&db, day, "100201", "6602", "", m("1"), "", "u").is_err());
        // 科目不存在
        assert!(transfer_create(&db, day, "999999", "1001", "", m("1"), "", "u").is_err());
        // 空
        assert!(transfer_create(&db, day, "", "1001", "", m("1"), "", "u").is_err());
    }

    /// 撤审 / 删除链：已审核有凭证 → 拒删；撤审删凭证回草稿 → 可删；已记账凭证撤审被拒。
    #[test]
    fn transfer_unaudit_and_delete_chain() {
        let db = mem();
        let id =
            transfer_create(&db, d(2026, 1, 5), "100201", "1001", "", m("300"), "", "u").unwrap();
        let vid = transfer_audit(&db, id, "u").unwrap();
        assert!(transfer_delete(&db, id).is_err(), "凭证存在时不能删单");

        // 撤审：删除未记账凭证 → 回草稿
        transfer_unaudit(&db, id).unwrap();
        let doc = transfer_get(&db, id).unwrap().unwrap();
        assert_eq!(doc.status, "draft");
        assert!(doc.voucher_id.is_none());
        // 凭证已被撤审删除
        assert!(crate::vouchers::get(&db, vid).unwrap().is_none());
        // 草稿可直接删
        transfer_delete(&db, id).unwrap();
        assert!(transfer_list(&db).unwrap().iter().all(|x| x.id != id));
    }
}
