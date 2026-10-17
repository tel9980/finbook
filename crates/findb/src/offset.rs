//! 往来对冲单（应收 ↔ 应付相互抵销，对标金蝶「应付冲应收」/ 用友「往来对冲」）。
//!
//! 现实场景：某公司既向我们买货（应收），我们又向它买货（应付）。月末两家约定
//! 「对抵」——把应收与应付轧差，只结净额。当前 [`crate::settle`] 的核销**只认同科目
//! 同辅助**（`settle_in_tx` 的硬约束），应收 `1122/customer=` 与应付 `2202/supplier=`
//! 永远对不到一起，所以只能靠本模块出一张对冲凭证来打通。
//!
//! 一张对冲单 = 一张单据 + 一张凭证：**借 应付 / 贷 应收**（金额相等），
//! 随后对两端各自做 FIFO 核销——应收贷方冲借方挂账、应付借方冲贷方挂账——
//! 两边往来同时变清，差额（净额）继续挂着。与 [`crate::receipt`]、[`crate::transfer`]
//! 同构：建单草稿 → 审核出凭证 → 撤审 / 删除链。
//!
//! 口径：
//! - 应收 / 应付科目默认取账套配置 `biz_accounts.ar / ap`，可按需覆盖；
//! - 核销走 [`crate::settle::open_entries`]（H-3：只认已记账），FIFO 按日期；
//! - 只对**同一往来单位**的未清挂账分摊（辅助核算 `key_contains`，兼容部门 / 项目多维）；
//! - 审核前校验对冲额 ≤ 两端各自未核销合计，避免把某侧冲成异常余额。

use std::collections::BTreeMap;

use chrono::NaiveDate;
use fincore::{AuxRef, Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 往来对冲单
#[derive(Clone, Debug, serde::Serialize)]
pub struct ArapOffset {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    /// 应收方往来单位（客户辅助编码，如 C01）
    pub customer: String,
    /// 应付方往来单位（供应商辅助编码，如 S01）
    pub supplier: String,
    /// 应收科目（默认账套配置 ar）
    pub ar_account: String,
    /// 应付科目（默认账套配置 ap）
    pub ap_account: String,
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

fn map_doc(r: &rusqlite::Row) -> rusqlite::Result<ArapOffset> {
    let d: String = r.get(3)?;
    Ok(ArapOffset {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1977, 1, 1).unwrap()),
        customer: r.get(4)?,
        supplier: r.get(5)?,
        ar_account: r.get(6)?,
        ap_account: r.get(7)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
        memo: r.get(9)?,
        voucher_id: r.get(10)?,
        created_by: r.get(11)?,
        created_at: r.get(12)?,
        status: r.get(13)?,
    })
}

const O_COLS: &str = "id,no,period,date,customer,supplier,ar_account,ap_account,amount,memo,\
     voucher_id,created_by,created_at,status";

pub fn offset_list(db: &Db) -> DbResult<Vec<ArapOffset>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {O_COLS} FROM arap_offset ORDER BY date DESC, id DESC"
    ))?;
    let rows = st.query_map([], map_doc)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn offset_get(db: &Db, id: i64) -> DbResult<Option<ArapOffset>> {
    db.conn()
        .query_row(
            &format!("SELECT {O_COLS} FROM arap_offset WHERE id=?1"),
            rusqlite::params![id],
            map_doc,
        )
        .optional()
        .map_err(Into::into)
}

/// 科目一级根：覆盖该科目的全部下级叶子（按账套编码方案第一级长度截断）。
fn root_of(db: &Db, code: &str) -> String {
    let code = code.trim();
    let root_len = db
        .options()
        .code_scheme
        .first()
        .map(|s| *s as usize)
        .unwrap_or(4);
    if code.len() >= root_len {
        code[..root_len].to_string()
    } else {
        code.to_string()
    }
}

/// 新建往来对冲单（**草稿，仅台账**）：凭证与两端 FIFO 核销在「审核」时生成。
///
/// 守卫：客户 / 供应商必填、两端科目不同、金额 > 0。未传科目时用账套配置的应收 / 应付。
/// 返回单据 id。
#[allow(clippy::too_many_arguments)]
pub fn offset_create(
    db: &Db,
    date: NaiveDate,
    customer: &str,
    supplier: &str,
    ar_account: &str,
    ap_account: &str,
    amount: Money,
    memo: &str,
    who: &str,
) -> DbResult<i64> {
    let customer = customer.trim();
    let supplier = supplier.trim();
    if customer.is_empty() || supplier.is_empty() {
        return Err(fincore::FinError::validate(
            "应收方客户与应付方供应商都必须填写（决定与谁对冲）",
        )
        .into());
    }
    if !amount.is_positive() {
        return Err(fincore::FinError::validate("对冲金额必须大于 0").into());
    }
    let biz = db.options().biz_accounts.clone();
    let ar = if ar_account.trim().is_empty() {
        biz.ar
    } else {
        ar_account.trim().to_string()
    };
    let ap = if ap_account.trim().is_empty() {
        biz.ap
    } else {
        ap_account.trim().to_string()
    };
    if root_of(db, &ar) == root_of(db, &ap) {
        return Err(fincore::FinError::validate("应收与应付不能是同一科目").into());
    }
    let period = Period::from_date(date);
    let tx = db.write_tx()?;
    let doc_no = format!("DC{}", chrono::Local::now().format("%y%m%d%H%M%S"));
    tx.execute(
        "INSERT INTO arap_offset(no,period,date,customer,supplier,ar_account,ap_account,amount,memo,created_by,created_at,status)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'draft')",
        rusqlite::params![
            doc_no,
            period.ymm(),
            date.format("%Y-%m-%d").to_string(),
            customer,
            supplier,
            ar,
            ap,
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

/// 一条对冲分摊：(被核销分录 id, 科目, 完整辅助核算 key, 金额)
type Take = (i64, String, String, Money);

/// 按往来单位把未清挂账做 FIFO 分摊，返回分摊明细与「(科目,辅助) → 金额/辅助」分组。
fn fifo_allocate(
    opens: &[crate::settle::OpenEntry],
    amount: Money,
) -> (Vec<Take>, BTreeMap<String, (Money, AuxRef)>) {
    let mut rem = amount;
    let mut takes: Vec<Take> = Vec::new();
    let mut slots: BTreeMap<String, (Money, AuxRef)> = BTreeMap::new();
    for e in opens {
        if rem.is_zero() {
            break;
        }
        let take = rem.min(e.open());
        if take.is_positive() {
            takes.push((e.entry_id, e.account_code.clone(), e.aux_key.clone(), take));
            let slot = slots
                .entry(format!("{}\u{1f}{}", e.account_code, e.aux_key))
                .or_insert((Money::ZERO, AuxRef::from_key(&e.aux_key)));
            slot.0 += take;
            rem -= take;
        }
    }
    (takes, slots)
}

/// 审核往来对冲单：同事务生成对冲凭证（借 应付 / 贷 应收）+ 两端 FIFO 核销 → 已审核。
/// 仅 draft 可审核（条件更新防并发重复）。
///
/// 返回 `(凭证 id, 自动核销笔数)`。
pub fn offset_audit(db: &Db, id: i64, who: &str) -> DbResult<(i64, usize)> {
    let d = offset_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("往来对冲单"))?;
    if d.status != "draft" {
        return Err(fincore::FinError::state("仅待审核的单据可审核").into());
    }
    if d.voucher_id.is_some() {
        return Err(fincore::FinError::state("单据已生成凭证").into());
    }
    let customer = d.customer.trim().to_string();
    let supplier = d.supplier.trim().to_string();
    if customer.is_empty() || supplier.is_empty() {
        return Err(fincore::FinError::validate("往来单位为空的单据不能审核").into());
    }
    let amount = d.amount;
    let date = d.date;
    let period = d.period;
    let memo = d.memo.clone();
    let ar_root = root_of(db, &d.ar_account);
    let ap_root = root_of(db, &d.ap_account);
    if ar_root == ap_root {
        return Err(fincore::FinError::validate("应收与应付不能是同一科目").into());
    }
    let aux_ar = AuxRef {
        customer: Some(customer.clone()),
        ..Default::default()
    };
    let aux_ap = AuxRef {
        supplier: Some(supplier.clone()),
        ..Default::default()
    };

    // 未清挂账：应收取借方、应付取贷方；只认同一往来单位（含多维辅助的包含匹配）
    let mut opens_ar = crate::settle::open_entries(db, &ar_root, period, false)?;
    opens_ar.retain(|e| e.debit > Money::ZERO && AuxRef::key_contains(&e.aux_key, &aux_ar.key()));
    let mut opens_ap = crate::settle::open_entries(db, &ap_root, period, false)?;
    opens_ap.retain(|e| e.credit > Money::ZERO && AuxRef::key_contains(&e.aux_key, &aux_ap.key()));

    let ar_sum = opens_ar.iter().fold(Money::ZERO, |a, e| a + e.open());
    let ap_sum = opens_ap.iter().fold(Money::ZERO, |a, e| a + e.open());
    if amount > ar_sum {
        return Err(fincore::FinError::validate(format!(
            "对冲金额 {} 超过客户 {} 的未核销应收 {}",
            amount.fmt_money(),
            customer,
            ar_sum.fmt_money()
        ))
        .into());
    }
    if amount > ap_sum {
        return Err(fincore::FinError::validate(format!(
            "对冲金额 {} 超过供应商 {} 的未核销应付 {}",
            amount.fmt_money(),
            supplier,
            ap_sum.fmt_money()
        ))
        .into());
    }

    let (takes_ar, slots_ar) = fifo_allocate(&opens_ar, amount);
    let (takes_ap, slots_ap) = fifo_allocate(&opens_ap, amount);

    let tx = db.write_tx()?;
    let mut v = Voucher::new(
        period,
        date,
        "记",
        crate::vouchers::next_no_of(&tx, period, "记")?,
    );
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = if memo.trim().is_empty() {
        format!("往来对冲 {customer} ↔ {supplier}")
    } else {
        memo.trim().to_string()
    };
    let summary = v.memo.clone();
    let mut line: i32 = 1;
    // 借 应付（按科目 + 辅助拆腿）
    for (slot_key, (amt, aux)) in &slots_ap {
        if amt.is_zero() {
            continue;
        }
        let acct = slot_key.split('\u{1f}').next().unwrap_or(slot_key);
        v.push_entry(Entry {
            debit: *amt,
            aux: aux.clone(),
            ..Entry::new(line, acct, summary.as_str())
        });
        line += 1;
    }
    // 贷 应收
    for (slot_key, (amt, aux)) in &slots_ar {
        if amt.is_zero() {
            continue;
        }
        let acct = slot_key.split('\u{1f}').next().unwrap_or(slot_key);
        v.push_entry(Entry {
            credit: *amt,
            aux: aux.clone(),
            ..Entry::new(line, acct, summary.as_str())
        });
        line += 1;
    }
    let vid = crate::vouchers::save_in(&tx, &mut v)?;

    // 取回凭证分录 id，按 (科目, 辅助 key) 精确匹配到对应腿
    let entries: Vec<(i64, String, String)> = {
        let mut st =
            tx.prepare("SELECT id, account_code, aux_key FROM voucher_entry WHERE voucher_id=?1")?;
        let rows = st
            .query_map([vid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let id_of = |acct: &str, aux_key: &str| -> Option<i64> {
        entries
            .iter()
            .find(|(_, a, k)| a == acct && k == aux_key)
            .map(|(id, _, _)| *id)
    };
    let mut settled_n = 0usize;
    for (from_id, acct, aux_key, take) in takes_ar.iter().chain(takes_ap.iter()) {
        if let Some(to_id) = id_of(acct, aux_key) {
            crate::settle::settle_in_tx(&tx, *from_id, to_id, *take, who)?;
            settled_n += 1;
        }
    }

    let n = tx.execute(
        "UPDATE arap_offset SET voucher_id=?2, status='audited' WHERE id=?1 AND status='draft'",
        rusqlite::params![id, vid],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("单据状态已变化，请刷新后重试").into());
    }
    tx.commit()?;
    Ok((vid, settled_n))
}

/// 撤销审核：删除其**未记账**凭证（含核销配对清理）→ 单据回到草稿。
/// 凭证已记账时拒绝（先反记账再撤审）。
pub fn offset_unaudit(db: &Db, id: i64) -> DbResult<()> {
    let d = offset_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("往来对冲单"))?;
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
        "UPDATE arap_offset SET status='draft', voucher_id=NULL WHERE id=?1 AND status='audited'",
        rusqlite::params![id],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("单据状态已变化，请刷新后重试").into());
    }
    crate::vouchers::delete(db, vid)?;
    tx.commit()?;
    Ok(())
}

/// 删除往来对冲单：其凭证须先作废或删除，随后可删
pub fn offset_delete(db: &Db, id: i64) -> DbResult<()> {
    let d = offset_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("往来对冲单"))?;
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
    db.conn()
        .execute("DELETE FROM arap_offset WHERE id=?1", rusqlite::params![id])?;
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

    /// 挂账一张：应收（借 112201）或应付（贷 220201），并记账。
    fn ar_ap_voucher(db: &Db, p: Period, day: u32, ar: bool, party: &str, amount: Money) {
        let date = NaiveDate::from_ymd_opt(p.year(), p.month(), day).unwrap();
        let mut v = Voucher::new(
            p,
            date,
            "记",
            crate::vouchers::next_no(db, p, "记").unwrap(),
        );
        let aux = if ar {
            AuxRef {
                customer: Some(party.to_string()),
                ..Default::default()
            }
        } else {
            AuxRef {
                supplier: Some(party.to_string()),
                ..Default::default()
            }
        };
        if ar {
            v.push_entry(Entry {
                debit: amount,
                aux,
                ..Entry::new(1, "112201", "应收挂账")
            });
            v.push_entry(Entry {
                credit: amount,
                ..Entry::new(2, "600101", "收入")
            });
        } else {
            v.push_entry(Entry {
                debit: amount,
                ..Entry::new(1, "1001", "付款")
            });
            v.push_entry(Entry {
                credit: amount,
                aux,
                ..Entry::new(2, "220201", "应付挂账")
            });
        }
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, vid, "u").unwrap();
    }

    /// 审核对冲：借 应付 / 贷 应收，且两端各自与挂账核销干净。
    #[test]
    fn offset_audit_clears_both_sides() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        ar_ap_voucher(&db, p, 5, true, "C01", m("1000")); // 客户 C01 欠我们 1000
        ar_ap_voucher(&db, p, 6, false, "S01", m("600")); // 我们欠供应商 S01 600

        let id = offset_create(
            &db,
            d(2026, 1, 20),
            "C01",
            "S01",
            "",
            "",
            m("600"),
            "月末对冲",
            "u",
        )
        .unwrap();
        {
            let doc = offset_get(&db, id).unwrap().unwrap();
            assert_eq!(doc.status, "draft");
            assert!(doc.voucher_id.is_none(), "草稿不应生成凭证");
            assert_eq!(doc.ar_account, "112201", "未传科目时取账套配置");
            assert_eq!(doc.ap_account, "220201");
        }
        let (vid, n) = offset_audit(&db, id, "u").unwrap();
        assert_eq!(n, 2, "两端各一笔核销");
        assert_eq!(offset_get(&db, id).unwrap().unwrap().status, "audited");
        assert!(offset_audit(&db, id, "u").is_err(), "重复审核应拒绝");

        // 凭证：借 220201 / 贷 112201，各 600
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.entries[0].account_code, "220201");
        assert_eq!(v.entries[0].debit, m("600"));
        assert_eq!(v.entries[1].account_code, "112201");
        assert_eq!(v.entries[1].credit, m("600"));

        // 应收只剩 400、应付全清：账龄净额 = 未核销合计
        let ar = crate::settle::open_entries(&db, "1122", p, false).unwrap();
        let ar_open = ar.iter().fold(Money::ZERO, |a, e| a + e.open());
        assert_eq!(ar_open, m("400"), "应收对冲 600 后应剩 400");
        let ap = crate::settle::open_entries(&db, "2202", p, false).unwrap();
        let ap_open = ap.iter().fold(Money::ZERO, |a, e| a + e.open());
        assert_eq!(ap_open, Money::ZERO, "应付应被全额冲销");
    }

    /// 守卫：对冲额超过任一端未核销合计 → 拒绝；空往来单位 → 建单即拦。
    #[test]
    fn offset_amount_guard() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        ar_ap_voucher(&db, p, 5, true, "C01", m("1000"));
        ar_ap_voucher(&db, p, 6, false, "S01", m("600"));

        // 超过应付 600
        let id =
            offset_create(&db, d(2026, 1, 20), "C01", "S01", "", "", m("800"), "", "u").unwrap();
        assert!(
            offset_audit(&db, id, "u").is_err(),
            "超过应付未核销额应拒绝"
        );

        // 空往来单位
        assert!(offset_create(&db, d(2026, 1, 20), "", "S01", "", "", m("1"), "", "u").is_err());
        assert!(offset_create(&db, d(2026, 1, 20), "C01", "", "", "", m("1"), "", "u").is_err());
        // 零金额
        assert!(offset_create(
            &db,
            d(2026, 1, 20),
            "C01",
            "S01",
            "",
            "",
            Money::ZERO,
            "",
            "u"
        )
        .is_err());
        // 应收=应付科目
        assert!(offset_create(
            &db,
            d(2026, 1, 20),
            "C01",
            "S01",
            "112201",
            "112201",
            m("1"),
            "",
            "u"
        )
        .is_err());
    }

    /// 撤审 / 删除链：已审核有凭证 → 拒删；撤审删凭证回草稿 → 可删，且核销被一并清理。
    #[test]
    fn offset_unaudit_and_delete_chain() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        ar_ap_voucher(&db, p, 5, true, "C01", m("1000"));
        ar_ap_voucher(&db, p, 6, false, "S01", m("600"));
        let id =
            offset_create(&db, d(2026, 1, 20), "C01", "S01", "", "", m("600"), "", "u").unwrap();
        let (vid, _) = offset_audit(&db, id, "u").unwrap();
        assert!(offset_delete(&db, id).is_err(), "凭证存在时不能删单");

        offset_unaudit(&db, id).unwrap();
        let doc = offset_get(&db, id).unwrap().unwrap();
        assert_eq!(doc.status, "draft");
        assert!(doc.voucher_id.is_none());
        assert!(crate::vouchers::get(&db, vid).unwrap().is_none());
        // 核销记录随凭证删除被清理，应收恢复 1000
        let ar = crate::settle::open_entries(&db, "1122", p, false).unwrap();
        let ar_open = ar.iter().fold(Money::ZERO, |a, e| a + e.open());
        assert_eq!(ar_open, m("1000"), "撤审后应收应恢复未核销");

        offset_delete(&db, id).unwrap();
        assert!(offset_list(&db).unwrap().iter().all(|x| x.id != id));
    }
}
