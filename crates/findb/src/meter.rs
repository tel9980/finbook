//! 电费 / 水费充值台账：充值是预付，抄表才转成本。
//!
//! 加工厂的电费大多不是每月出票、按月付，而是**先往电卡里充钱**，电力公司按
//! 实际用量从卡里扣。会计上这三件事必须分开记，否则账上会出现两种典型错法：
//!
//! 1. 充值就记费用 —— 月底卡里还剩两万，费用虚高、利润虚低；
//! 2. 抄表就记应付 —— 钱早就付了，又挂一笔应付，欠款表多出一家不缺钱的债主。
//!
//! 正确的三步（本模块就是把这个流程做成台账）：
//!
//! | 时点 | 事情 | 分录 |
//! |------|------|------|
//! | 充值时 | 钱从银行进了电卡 | 借 112301 预付电费 / 贷 银行存款 |
//! | 月底抄表 | 本月实际用掉多少度 | 进 [`meter_reading`]，先只登记不动账 |
//! | 结转 | 把用量转成费用 | 借 510103 水电费（不含税）/ 借 22210101 进项 / 贷 112301 |
//!
//! 结转这一步才拆进项，因为**能不能抵税取决于电力公司开的票**：专票按 13%
//! （部分情况 9%），票上的税额就是可抵进项。台账里每行都带税率，结转时按行汇总。
//!
//! 充值与余额不另建表：112301 的借方发生额就是本期充值、贷方发生额就是本期结转、
//! 期末借方余额就是卡里还剩多少 —— 从账上直接看，不会出现「台账与总账两套数」。
//!
//! 表结构见 `schema.rs` 的 `meter_reading`（v47 起）。

use chrono::NaiveDate;
use fincore::{AuxRef, Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::OptionalExtension;

use crate::balances::{BalanceQuery, BalanceSnapshot};
use crate::{Db, DbResult, FinError};

/// 电费/水费的分册。水费同构（充值少见，但工业园按吨抄表的也不少）。
pub const KIND_ELECTRIC: &str = "electric";
pub const KIND_WATER: &str = "water";

pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        KIND_WATER => "水费",
        _ => "电费",
    }
}

/// 结转时进哪个费用科目：两者都进制造费用-水电费，电水在生产上不分开管。
pub const COST_ACCOUNT: &str = "510103";
/// 预付卡余额科目（默认用它；账套里没有就退回 1123 预付账款，
/// 这时 1123 核算供应商，结转必须指明预付给了哪家 —— 见 [`resolve_prepaid`]）
pub const DEFAULT_PREPAID: &str = "112301";
pub const FALLBACK_PREPAID: &str = "1123";
/// 进项税额
pub const INPUT_TAX_ACCOUNT: &str = "22210101";

/// 结转时贷记哪个科目。
///
/// 优先 `112301 预付电费`（演示账与本模块的推荐做法：挂在预付账款下的专用明细，
/// 不核算供应商 —— 电卡是预付给电力公司，但一家厂通常只有一家电力公司，
/// 再按供应商细分没意义）。账套里没建这个明细时退回 `1123 预付账款`，
/// 但 1123 默认核算供应商，调用方必须传 `supplier`。
pub fn resolve_prepaid(db: &Db, want: Option<&str>) -> DbResult<String> {
    let code = want.map(|s| s.trim()).filter(|s| !s.is_empty());
    let pick = match code {
        Some(c) => c.to_string(),
        None => {
            if crate::accounts::get(db, DEFAULT_PREPAID)?.is_some() {
                DEFAULT_PREPAID.to_string()
            } else {
                FALLBACK_PREPAID.to_string()
            }
        }
    };
    crate::accounts::get(db, &pick)?.ok_or_else(|| {
        FinError::msg(format!(
            "科目 {pick} 不存在，请在科目表里建一个预付电费明细"
        ))
    })?;
    let kids: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM account WHERE code LIKE ?1 || '%' AND code <> ?1",
        [&pick],
        |r| r.get(0),
    )?;
    if kids > 0 {
        return Err(FinError::msg(format!(
            "科目 {pick} 还有下级科目，结转要用末级明细（建议建 112301 预付电费 这样的明细）"
        ))
        .into());
    }
    Ok(pick)
}

/// 这个科目要不要供应商辅助：要就把 `supplier` 填上，不要就别乱给辅助。
fn prepaid_aux(db: &Db, code: &str, supplier: Option<&str>) -> DbResult<AuxRef> {
    let acc = crate::accounts::get(db, code)?
        .ok_or_else(|| FinError::msg(format!("科目 {code} 不存在")))?;
    if !acc.aux.contains(fincore::AuxKind::Supplier) {
        return Ok(AuxRef::default());
    }
    let sup = supplier.map(|s| s.trim()).filter(|s| !s.is_empty());
    match sup {
        Some(s) => Ok(AuxRef {
            supplier: Some(s.to_string()),
            ..Default::default()
        }),
        None => Err(FinError::msg(format!(
            "科目 {code} 核算供应商，结转时要说明预付给了哪家（一般是电力公司）"
        ))
        .into()),
    }
}

/// 一行抄表记录
#[derive(Clone, Debug, Default)]
pub struct Reading {
    pub id: i64,
    /// `electric` / `water`
    pub kind: String,
    /// 表号 / 户号，一个厂多块表时用它区分
    pub meter_no: String,
    /// 归属期间（yyyymm）
    pub period: i32,
    pub read_date: String,
    /// 本期用量（度 / 吨）
    pub qty: Money,
    /// 含税单价
    pub unit_price: Money,
    /// 本期金额（含税）：票上的价税合计，或 用量 × 单价
    pub amount_incl: Money,
    /// 税率，如 "0.13"；无票填 "0"
    pub tax_rate: String,
    /// 结转凭证；0 表示还没结转
    pub voucher_id: i64,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
}

impl Reading {
    /// 本行的不含税金额与税额：与全系统含税拆分同一口径
    /// （不含税 = 含税 ÷ (1+税率) 四舍五入，税额 = 含税 − 不含税）。
    pub fn split(&self) -> Result<(Money, Money), String> {
        let gross = self.amount_incl.round2();
        let rate = self.tax_rate.trim();
        if rate.is_empty() || rate == "0" || rate == "0.00" {
            return Ok((gross, Money::ZERO));
        }
        let r = Money::parse(rate).map_err(|_| format!("税率「{rate}」不是数字"))?;
        let one = Money::parse("1").map_err(|_| "1 解析失败".to_string())?;
        let divisor = one.checked_add(r).ok_or("税率过大")?;
        if divisor.is_zero() {
            return Err("税率不能是 -100%".into());
        }
        let net = gross
            .checked_div(divisor)
            .ok_or_else(|| "含税金额除税率失败".to_string())?
            .round2();
        Ok((net, (gross - net).round2()))
    }

    /// 结转了吗
    pub fn is_closed(&self) -> bool {
        self.voucher_id != 0
    }
}

fn row_to_reading(r: &rusqlite::Row<'_>) -> rusqlite::Result<Reading> {
    Ok(Reading {
        id: r.get(0)?,
        kind: r.get(1)?,
        meter_no: r.get(2)?,
        period: r.get(3)?,
        read_date: r.get(4)?,
        qty: Money::parse_or_zero(&r.get::<_, String>(5)?),
        unit_price: Money::parse_or_zero(&r.get::<_, String>(6)?),
        amount_incl: Money::parse_or_zero(&r.get::<_, String>(7)?),
        tax_rate: r.get(8)?,
        voucher_id: r.get(9)?,
        memo: r.get(10)?,
        created_by: r.get(11)?,
        created_at: r.get(12)?,
    })
}

const COLS: &str = "id, kind, meter_no, period, read_date, qty, unit_price, amount_incl, tax_rate, voucher_id, memo, created_by, created_at";

/// 登记一条抄表记录。
///
/// 金额留空时按 `用量 × 单价` 算（多数时候会计只拿到一张电费单，上面写着度数、
/// 单价、金额，照着填就行）。已有结转凭证的行不能改金额 —— 账已经出了，
/// 改台账不改正账，两边立刻对不上，所以直接拒绝并让人先作废凭证。
pub fn add(db: &Db, r: &mut Reading, who: &str) -> DbResult<i64> {
    if r.id != 0 {
        return Err(FinError::msg("新增抄表记录时 id 必须为 0").into());
    }
    if r.kind.trim().is_empty() {
        r.kind = KIND_ELECTRIC.to_string();
    }
    if r.meter_no.trim().is_empty() {
        return Err(FinError::msg("请填写表号/户号（一个厂多块表时靠它区分）").into());
    }
    if r.read_date.trim().is_empty() {
        return Err(FinError::msg("请填写抄表日期").into());
    }
    if r.amount_incl.is_zero() {
        if r.qty.is_zero() || r.unit_price.is_zero() {
            return Err(FinError::msg(
                "用量、单价、金额三者至少要有两个：金额可以不填，但用量和单价要填",
            )
            .into());
        }
        r.amount_incl = r
            .qty
            .checked_mul(r.unit_price)
            .ok_or_else(|| FinError::msg("用量 × 单价 溢出"))?
            .round2();
    }
    if r.amount_incl.is_negative() {
        return Err(FinError::msg("抄表金额不能是负数").into());
    }
    // 先把税率和拆分试一遍，坏税率不要留到结转那天才炸
    r.split().map_err(FinError::msg)?;
    if r.tax_rate.trim().is_empty() {
        r.tax_rate = "0".to_string();
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let tx = db.write_tx()?;
    tx.execute(
        "INSERT INTO meter_reading(kind, meter_no, period, read_date, qty, unit_price, amount_incl, tax_rate, voucher_id, memo, created_by, created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0,?9,?10,?11)",
        rusqlite::params![
            r.kind,
            r.meter_no,
            r.period,
            r.read_date,
            r.qty.fmt_qty(),
            r.unit_price.fmt_qty(),
            r.amount_incl.fmt_qty(),
            r.tax_rate,
            r.memo,
            who,
            now
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    r.id = id;
    r.created_by = who.to_string();
    r.created_at = now;
    Ok(id)
}

/// 列出台账：可按期间、按电/水过滤，最近的排在前面
pub fn list(db: &Db, period: Option<i32>, kind: Option<&str>) -> DbResult<Vec<Reading>> {
    let mut sql = format!("SELECT {COLS} FROM meter_reading WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(p) = period {
        sql.push_str(" AND period=?");
        args.push(Box::new(p));
    }
    if let Some(k) = kind {
        if !k.trim().is_empty() {
            sql.push_str(" AND kind=?");
            args.push(Box::new(k.to_string()));
        }
    }
    sql.push_str(" ORDER BY read_date DESC, id DESC");
    let mut st = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|b| b.as_ref()).collect();
    let rows = st
        .query_map(refs.as_slice(), row_to_reading)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 删一行。已结转的不许删：凭证还在，删台账等于让账证不符。
pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    let r = get(db, id)?.ok_or_else(|| FinError::msg("抄表记录不存在"))?;
    if r.is_closed() {
        return Err(FinError::msg(format!(
            "这行已经结转（凭证 #{}），要改先作废那张凭证，再删台账",
            r.voucher_id
        ))
        .into());
    }
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM meter_reading WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(())
}

pub fn get(db: &Db, id: i64) -> DbResult<Option<Reading>> {
    let row = db
        .conn()
        .query_row(
            &format!("SELECT {COLS} FROM meter_reading WHERE id=?1"),
            [id],
            row_to_reading,
        )
        .optional()?;
    Ok(row)
}

/// 台账概况：充了多少、用了多少、卡里还剩多少、还有多少没结转
#[derive(Clone, Debug, Default)]
pub struct Summary {
    /// 本期充值（112301 借方发生额）
    pub recharged: Money,
    /// 本期已结转进费用的金额（112301 贷方发生额）
    pub consumed: Money,
    /// 卡里余额（112301 期末借方余额）
    pub balance: Money,
    /// 未结转行合计（含税）
    pub unclosed: Money,
    /// 未结转行数
    pub unclosed_count: usize,
    /// 未结转行的不含税金额
    pub unclosed_net: Money,
    /// 未结转行的可抵进项
    pub unclosed_tax: Money,
}

/// 概览。期间的充值/结转/余额取账上的数，未结转部分取台账。
pub fn summary(
    db: &Db,
    period: Period,
    kind: Option<&str>,
    prepaid: Option<&str>,
) -> DbResult<Summary> {
    let prepaid = resolve_prepaid(db, prepaid)?;
    let snap = BalanceSnapshot::load(db, &BalanceQuery::period(period).with_posted_only(true))?;
    let acc = snap.for_account(&prepaid, None);
    let mut s = Summary {
        recharged: acc.debit,
        consumed: acc.credit,
        balance: acc.end_dir_amount().1,
        ..Default::default()
    };
    for r in list(db, Some(period.ymm()), kind)? {
        if r.is_closed() {
            continue;
        }
        let (net, tax) = r.split().map_err(FinError::msg)?;
        s.unclosed += r.amount_incl;
        s.unclosed_net += net;
        s.unclosed_tax += tax;
        s.unclosed_count += 1;
    }
    Ok(s)
}

/// 结转：把本期还没结转的抄表行做成一张凭证。
///
/// 借 510103 水电费（不含税）/ 借 22210101 进项税额 / 贷 112301 预付电费（价税合计）。
///
/// 没有待结转的行就返回 `Ok(None)` —— 重复点「结转」不该凭空造一张零金额凭证。
pub fn close(
    db: &Db,
    period: Period,
    kind: Option<&str>,
    prepaid: Option<&str>,
    supplier: Option<&str>,
    date: NaiveDate,
    who: &str,
) -> DbResult<Option<i64>> {
    let prepaid_code = resolve_prepaid(db, prepaid)?;
    let prepaid_aux = prepaid_aux(db, &prepaid_code, supplier)?;
    let mut net = Money::ZERO;
    let mut tax = Money::ZERO;
    let mut gross = Money::ZERO;
    let mut ids: Vec<i64> = Vec::new();
    for r in list(db, Some(period.ymm()), kind)? {
        if r.is_closed() {
            continue;
        }
        let (n, t) = r.split().map_err(FinError::msg)?;
        net += n;
        tax += t;
        gross += r.amount_incl;
        ids.push(r.id);
    }
    if ids.is_empty() || gross.is_zero() {
        return Ok(None);
    }
    let label = kind_label(kind.unwrap_or(KIND_ELECTRIC));
    let tx = db.write_tx()?;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = format!("{label}抄表结转 {} 行", ids.len());
    v.push_entry(Entry {
        debit: net,
        ..Entry::new(1, COST_ACCOUNT, format!("{label}（不含税）"))
    });
    if tax.is_positive() {
        v.push_entry(Entry {
            debit: tax,
            ..Entry::new(2, INPUT_TAX_ACCOUNT, format!("{label}进项税额"))
        });
    }
    v.push_entry(Entry {
        credit: gross,
        aux: prepaid_aux,
        ..Entry::new(3, &prepaid_code, format!("{label}预付卡扣减"))
    });
    v.renumber();
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    // 保存完立刻记账：结转凭证不记账，112301 的贷方发生额就是 0，
    // 台账显示「已结转」而账上卡里余额一分没少 —— 这种半截状态最难查。
    crate::vouchers::post_tx(&tx, vid, who)?;
    // 回写：一行一条 UPDATE，行数不多（一个厂一个月一两张表）
    for id in &ids {
        tx.execute(
            "UPDATE meter_reading SET voucher_id=?2 WHERE id=?1",
            rusqlite::params![id, vid],
        )?;
    }
    tx.commit()?;
    Ok(Some(vid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    /// 照演示账的做法建「112301 预付电费」明细：挂在 1123 下、不核算供应商。
    fn setup_prepaid(db: &Db) {
        let parent = crate::accounts::get(db, "1123")
            .unwrap()
            .expect("默认科目表有 1123");
        let acc = fincore::Account::new("112301", "预付电费", parent.category);
        crate::accounts::insert(db, &acc).unwrap();
    }

    fn reading(period: Period, kind: &str, qty: &str, price: &str, rate: &str) -> Reading {
        Reading {
            kind: kind.to_string(),
            meter_no: "DB-01".to_string(),
            period: period.ymm(),
            read_date: "2026-11-30".to_string(),
            qty: m(qty),
            unit_price: m(price),
            amount_incl: Money::ZERO,
            tax_rate: rate.to_string(),
            ..Default::default()
        }
    }

    /// 电费走完三步：充值 → 抄表 → 结转，费用与进项都要对得上。
    ///
    /// 15,000 度 × 1.2 元 = 18,000 元含税，13% 票 → 不含税 15,929.20、进项 2,070.80。
    #[test]
    fn close_books_recharge_to_cost() {
        let db = mem();
        setup_prepaid(&db);
        let p = Period::new(2026, 11).unwrap();
        let mut r = reading(p, KIND_ELECTRIC, "15000", "1.2", "0.13");
        let id = add(&db, &mut r, "u1").unwrap();

        let mut water = reading(p, KIND_WATER, "10", "100", "0.09");
        water.kind = KIND_WATER.to_string();
        add(&db, &mut water, "u1").unwrap();

        let s = summary(&db, p, None, None).unwrap();
        assert_eq!(s.unclosed_count, 2);
        assert_eq!(s.unclosed.fmt_money(), "19,000.00");
        assert_eq!(s.unclosed_tax.fmt_money(), "2,153.37", "2,070.80 + 82.57");

        let vid = close(
            &db,
            p,
            None,
            None,
            None,
            NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(),
            "u1",
        )
        .unwrap()
        .expect("有未结转行就该出凭证");
        let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        assert_eq!(v.entries.len(), 3, "成本 / 进项 / 预付 三行");
        assert_eq!(v.entries[0].account_code, COST_ACCOUNT);
        assert_eq!(v.entries[0].debit.fmt_money(), "16,846.63");
        assert_eq!(v.entries[1].account_code, INPUT_TAX_ACCOUNT);
        assert_eq!(v.entries[1].debit.fmt_money(), "2,153.37");
        assert_eq!(v.entries[2].account_code, "112301");
        assert_eq!(v.entries[2].credit.fmt_money(), "19,000.00");
        assert_eq!(
            v.status,
            fincore::VoucherStatus::Posted,
            "结转凭证必须顺手记账，否则账上余额不动"
        );

        // 结转后这行不能再删，也不能重复结转
        assert!(delete(&db, id).is_err(), "已结转的行不能删");
        assert!(
            close(
                &db,
                p,
                None,
                None,
                None,
                NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(),
                "u1"
            )
            .unwrap()
            .is_none(),
            "没有待结转行了就不该再出凭证"
        );
    }

    /// 金额不填时按 用量 × 单价 补，单价里的小数不能丢
    #[test]
    fn amount_comes_from_qty_times_price() {
        let db = mem();
        let p = Period::new(2026, 11).unwrap();
        let mut r = reading(p, KIND_ELECTRIC, "1234.5", "1.23", "0.13");
        add(&db, &mut r, "u1").unwrap();
        let got = get(&db, r.id).unwrap().unwrap();
        assert_eq!(
            got.amount_incl.fmt_money(),
            "1,518.44",
            "1234.5 × 1.23 = 1518.435"
        );
        let (net, tax) = got.split().unwrap();
        assert_eq!((net + tax).fmt_money(), "1,518.44", "拆税后必须回到含税");
    }

    /// 没填用量也没填金额的空白行要拒绝：这种行结转时会变成零金额凭证
    #[test]
    fn empty_reading_is_rejected() {
        let db = mem();
        let p = Period::new(2026, 11).unwrap();
        let mut r = reading(p, KIND_ELECTRIC, "0", "0", "0.13");
        assert!(add(&db, &mut r, "u1").is_err());
        // 但「只填金额不填用量」是允许的：电费单上只有金额的情况很常见
        let mut r2 = reading(p, KIND_ELECTRIC, "0", "0", "0.13");
        r2.amount_incl = m("500");
        assert!(add(&db, &mut r2, "u1").is_ok());
    }
}
