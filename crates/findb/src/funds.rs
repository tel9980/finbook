//! 资金管理：资金日报（按日）、票据、融资、现金盘点、支票簿、出纳日记账/日清、员工借支、资金预算、资金预测
//!
//! 对标金蝶/用友资金模块。金额一律 TEXT 存储、Rust 侧 Decimal 累加。
//! - 票据/融资：状态流转**同事务自动生成台账凭证**（与总账联动）
//! - 资金日报：按日上日结余/本日收支/日末结存（仅已记账 H-3）；期间口径见 `funds_daily`
//! - 资金预测：现金结存 + 应收票据 − 应付票据 + 放款 − 借款，给出资金头寸

use chrono::NaiveDate;
use fincore::{AuxRef, Entry, Money, Period, Voucher, VoucherSource};
use rusqlite::OptionalExtension;

use crate::{Db, DbError, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}
fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

// ===========================================================================
// 票据
// ===========================================================================

/// 票据状态
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BillStatus {
    /// 在库（持有，未流转）
    InHand,
    /// 已背书（转让给他人）
    Endorsed,
    /// 已贴现
    Discounted,
    /// 已到期（未兑付）
    Matured,
    /// 已兑付 / 已付清
    Settled,
}

impl BillStatus {
    pub fn label(self) -> &'static str {
        match self {
            BillStatus::InHand => "在库",
            BillStatus::Endorsed => "已背书",
            BillStatus::Discounted => "已贴现",
            BillStatus::Matured => "已到期",
            BillStatus::Settled => "已兑付",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            BillStatus::InHand => "in_hand",
            BillStatus::Endorsed => "endorsed",
            BillStatus::Discounted => "discounted",
            BillStatus::Matured => "matured",
            BillStatus::Settled => "settled",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "endorsed" => BillStatus::Endorsed,
            "discounted" => BillStatus::Discounted,
            "matured" => BillStatus::Matured,
            "settled" => BillStatus::Settled,
            _ => BillStatus::InHand,
        }
    }
    pub const ALL: &'static [BillStatus] = &[
        BillStatus::InHand,
        BillStatus::Endorsed,
        BillStatus::Discounted,
        BillStatus::Matured,
        BillStatus::Settled,
    ];
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Bill {
    pub id: i64,
    pub kind: String, // receivable / payable
    pub no: String,
    pub period: Period,
    pub issue_date: NaiveDate,
    pub due_date: NaiveDate,
    pub counterpart: String,
    pub bank: String,
    pub amount: Money,
    pub status: String,
    pub handled_date: Option<NaiveDate>,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
    /// 已生成的台账凭证（贴现/背书/兑付等资金动作，一票一张）
    #[serde(default)]
    pub voucher_id: Option<i64>,
}

impl Bill {
    /// 应收票据为正、应付票据为负（用于资金头寸）
    pub fn signed(&self) -> Money {
        if self.kind == "receivable" {
            self.amount
        } else {
            -self.amount
        }
    }
}

fn map_bill(r: &rusqlite::Row) -> rusqlite::Result<Bill> {
    let issue: String = r.get(3)?;
    let due: String = r.get(4)?;
    let handled: Option<String> = r.get(10)?;
    Ok(Bill {
        id: r.get(0)?,
        kind: r.get(1)?,
        no: r.get(2)?,
        period: Period::from_ymm(r.get(6)?),
        issue_date: NaiveDate::parse_from_str(&issue, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        due_date: NaiveDate::parse_from_str(&due, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        counterpart: r.get(7)?,
        bank: r.get(8)?,
        amount: m(&r.get::<_, String>(9)?),
        status: r.get(5)?,
        handled_date: handled.map(|s| {
            NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
        }),
        memo: r.get(11)?,
        created_by: r.get(12)?,
        created_at: r.get(13)?,
        voucher_id: r.get(14)?,
    })
}

const B_COLS: &str = "id,kind,no,issue_date,due_date,status,period,counterpart,bank,amount,\
     handled_date,memo,created_by,created_at,voucher_id";

pub fn bill_list(db: &Db, kind: Option<&str>) -> DbResult<Vec<Bill>> {
    let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match kind {
        Some(k) => (
            format!("SELECT {B_COLS} FROM bill WHERE kind=?1 ORDER BY due_date, id DESC"),
            vec![Box::new(k.to_string())],
        ),
        None => (
            format!("SELECT {B_COLS} FROM bill ORDER BY due_date, id DESC"),
            vec![],
        ),
    };
    let mut st = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = st
        .query_map(refs.as_slice(), map_bill)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn bill_get(db: &Db, id: i64) -> DbResult<Option<Bill>> {
    db.conn()
        .query_row(
            &format!("SELECT {B_COLS} FROM bill WHERE id=?1"),
            rusqlite::params![id],
            map_bill,
        )
        .optional()
        .map_err(Into::into)
}

pub fn bill_save(db: &Db, b: &mut Bill) -> DbResult<i64> {
    let id = if b.id > 0 {
        db.conn().execute(
            "UPDATE bill SET kind=?2, no=?3, issue_date=?4, due_date=?5, status=?6, period=?7,
             counterpart=?8, bank=?9, amount=?10, handled_date=?11, memo=?12 WHERE id=?1",
            rusqlite::params![
                b.id, b.kind, b.no,
                b.issue_date.format("%Y-%m-%d").to_string(),
                b.due_date.format("%Y-%m-%d").to_string(),
                b.status, b.period.ymm(), b.counterpart, b.bank, crate::money_param(b.amount),
                b.handled_date.map(|d| d.format("%Y-%m-%d").to_string()), b.memo
            ],
        )?;
        b.id
    } else {
        db.conn().execute(
            "INSERT INTO bill(kind,no,issue_date,due_date,status,period,counterpart,bank,amount,
             handled_date,memo,created_by,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![
                b.kind, b.no,
                b.issue_date.format("%Y-%m-%d").to_string(),
                b.due_date.format("%Y-%m-%d").to_string(),
                b.status, b.period.ymm(), b.counterpart, b.bank, crate::money_param(b.amount),
                b.handled_date.map(|d| d.format("%Y-%m-%d").to_string()), b.memo,
                b.created_by, now()
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    b.id = id;
    Ok(id)
}

pub fn bill_delete(db: &Db, id: i64) -> DbResult<()> {
    let vid: Option<i64> = db
        .conn()
        .query_row(
            "SELECT voucher_id FROM bill WHERE id=?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found("票据"))?;
    if vid.is_some() {
        return Err(
            fincore::FinError::state("该票据已生成台账凭证，不能删除（可先作废凭证）").into(),
        );
    }
    db.conn()
        .execute("DELETE FROM bill WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 资金动作状态（会触发账务联动的流转目标）
fn bill_cash_state(to: BillStatus) -> bool {
    matches!(
        to,
        BillStatus::Endorsed | BillStatus::Discounted | BillStatus::Settled
    )
}

/// 按流转结果构造台账凭证分录：(借科目, 借辅助, 贷科目, 贷辅助, 动作名)。
///
/// 口径：贴现/兑付按面值全额入银行（贴现利息不分拆，可在生成的草稿里手工调整）；
/// 应收背书抵付应付账款、应付背书转为应付账款；承兑叶子默认「银行承兑汇票」，
/// 商业承兑可在生成的草稿中改科目。任一状态返回 None = 无需出凭证。
fn bill_entries(
    b: &Bill,
    to: BillStatus,
) -> Option<(&'static str, AuxRef, &'static str, AuxRef, &'static str)> {
    let bank_aux = || AuxRef {
        bank: Some("B01".into()),
        ..Default::default()
    };
    let sup_aux = || AuxRef {
        supplier: Some(b.counterpart.clone()),
        ..Default::default()
    };
    match (b.kind.as_str(), to) {
        (k, BillStatus::Discounted) if k == "receivable" => {
            Some(("100201", bank_aux(), "112101", AuxRef::default(), "贴现"))
        }
        (k, BillStatus::Discounted) if k == "payable" => {
            Some(("220101", AuxRef::default(), "100201", bank_aux(), "贴现"))
        }
        (k, BillStatus::Endorsed) if k == "receivable" => {
            Some(("220201", sup_aux(), "112101", AuxRef::default(), "背书"))
        }
        (k, BillStatus::Endorsed) if k == "payable" => {
            Some(("220101", AuxRef::default(), "220201", sup_aux(), "背书"))
        }
        (k, BillStatus::Settled) if k == "receivable" => {
            Some(("100201", bank_aux(), "112101", AuxRef::default(), "兑付"))
        }
        (k, BillStatus::Settled) if k == "payable" => {
            Some(("220101", AuxRef::default(), "100201", bank_aux(), "兑付"))
        }
        _ => None,
    }
}

/// 在事务内构建票据台账凭证（草稿），返回凭证 id。
/// 与流转同事务写入，任何一步失败整体回滚，不留孤儿凭证。
fn bill_voucher_in(
    tx: &rusqlite::Transaction,
    b: &Bill,
    to: BillStatus,
    date: NaiveDate,
    who: &str,
) -> Result<i64, DbError> {
    let (dr, dr_aux, cr, cr_aux, what) = bill_entries(b, to)
        .ok_or_else(|| fincore::FinError::state("当前状态无需生成台账凭证"))?;
    let period = Period::from_date(date);
    let no = crate::vouchers::next_no_of(tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    let summary = format!("票据{} {}", what, b.no);
    v.memo = summary.clone();
    v.push_entry(Entry {
        debit: b.amount,
        aux: dr_aux,
        ..Entry::new(1, dr, summary.as_str())
    });
    v.push_entry(Entry {
        credit: b.amount,
        aux: cr_aux,
        ..Entry::new(2, cr, summary.as_str())
    });
    v.renumber();
    crate::vouchers::save_in(tx, &mut v)
}

/// 票据状态流转：背书 / 贴现 / 到期 / 兑付，落 handled_date。
///
/// 资金动作（背书/贴现/兑付）在**同一事务**自动生成台账凭证草稿（H-3：草稿不入
/// 余额，需人工核对后记账），保证资金台账与总账不脱节；一张票据至多一张台账凭证
/// （先背书再兑付的二次流转只流转状态、不重复出凭证）。返回本次新生成的凭证 id。
pub fn bill_transition(
    db: &Db,
    id: i64,
    to: BillStatus,
    date: NaiveDate,
    who: &str,
) -> DbResult<Option<i64>> {
    let tx = db.write_tx()?;
    let mut b: Bill = tx
        .query_row(
            &format!("SELECT {B_COLS} FROM bill WHERE id=?1"),
            rusqlite::params![id],
            map_bill,
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found("票据"))?;
    if b.status == BillStatus::Settled.code() || b.status == BillStatus::Endorsed.code() {
        // 已背书/已兑付的票据为终态，不允许再流转（贴现需从在库发起）
        if to != BillStatus::Settled {
            return Err(fincore::FinError::state("该票据已背书或已兑付，不能继续流转").into());
        }
    }
    tx.execute(
        "UPDATE bill SET status=?2, handled_date=?3 WHERE id=?1",
        rusqlite::params![id, to.code(), date.format("%Y-%m-%d").to_string()],
    )?;
    b.status = to.code().to_string();
    b.handled_date = Some(date);
    let mut vid = None;
    if bill_cash_state(to) && b.voucher_id.is_none() {
        let v = bill_voucher_in(&tx, &b, to, date, who)?;
        tx.execute(
            "UPDATE bill SET voucher_id=?2 WHERE id=?1 AND voucher_id IS NULL",
            rusqlite::params![id, v],
        )?;
        vid = Some(v);
    }
    tx.commit()?;
    Ok(vid)
}

/// 票据补出凭证（流转时未生成的存量台账回填用；新流转由 bill_transition 自动生成）。
pub fn bill_voucher(db: &Db, id: i64, who: &str) -> DbResult<i64> {
    let b = bill_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("票据"))?;
    let to = BillStatus::parse(&b.status);
    if bill_entries(&b, to).is_none() {
        return Err(
            fincore::FinError::state("当前状态无需生成凭证（在库/已到期请先流转）").into(),
        );
    }
    if b.voucher_id.is_some() {
        return Err(fincore::FinError::state("该票据已生成过凭证").into());
    }
    let date = b
        .handled_date
        .ok_or_else(|| fincore::FinError::state("票据缺少处理日期，请重新流转"))?;
    let tx = db.write_tx()?;
    let vid = bill_voucher_in(&tx, &b, to, date, who)?;
    let taken = tx.execute(
        "UPDATE bill SET voucher_id=?2 WHERE id=?1 AND voucher_id IS NULL",
        rusqlite::params![id, vid],
    )?;
    if taken == 0 {
        return Err(fincore::FinError::state("该票据已生成过凭证").into());
    }
    tx.commit()?;
    Ok(vid)
}

// ===========================================================================
// 融资（借款 / 放款）
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Loan {
    pub id: i64,
    pub kind: String, // borrow / lend
    pub no: String,
    pub bank: String,
    pub principal: Money,
    /// 年利率（%）
    pub rate_pct: Money,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub status: String, // active / settled
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
    /// 到账凭证（借款到账 / 放款放出）
    #[serde(default)]
    pub voucher_id: Option<i64>,
    /// 还本/收回凭证（结清时生成）
    #[serde(default)]
    pub settle_voucher_id: Option<i64>,
    /// 结清日期（结清动作落库，还本凭证的会计日期）
    #[serde(default)]
    pub settle_date: Option<NaiveDate>,
}

impl Loan {
    /// 借款为负（未来要还）、放款为正（未来收回）
    pub fn signed(&self) -> Money {
        if self.kind == "lend" {
            self.principal
        } else {
            -self.principal
        }
    }
}

fn map_loan(r: &rusqlite::Row) -> rusqlite::Result<Loan> {
    let s: String = r.get(5)?;
    let e: String = r.get(6)?;
    Ok(Loan {
        id: r.get(0)?,
        kind: r.get(1)?,
        no: r.get(2)?,
        bank: r.get(3)?,
        principal: m(&r.get::<_, String>(4)?),
        rate_pct: m(&r.get::<_, String>(8)?),
        start_date: NaiveDate::parse_from_str(&s, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        end_date: NaiveDate::parse_from_str(&e, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        status: r.get(7)?,
        memo: r.get(9)?,
        created_by: r.get(10)?,
        created_at: r.get(11)?,
        voucher_id: r.get(12)?,
        settle_voucher_id: r.get(13)?,
        settle_date: r
            .get::<_, Option<String>>(14)?
            .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
    })
}

const L_COLS: &str = "id,kind,no,bank,principal,start_date,end_date,status,rate_pct,memo,created_by,created_at,voucher_id,settle_voucher_id,settle_date";

pub fn loan_list(db: &Db, kind: Option<&str>) -> DbResult<Vec<Loan>> {
    let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match kind {
        Some(k) => (
            format!("SELECT {L_COLS} FROM loan WHERE kind=?1 ORDER BY end_date, id DESC"),
            vec![Box::new(k.to_string())],
        ),
        None => (
            format!("SELECT {L_COLS} FROM loan ORDER BY end_date, id DESC"),
            vec![],
        ),
    };
    let mut st = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = st
        .query_map(refs.as_slice(), map_loan)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn loan_get(db: &Db, id: i64) -> DbResult<Option<Loan>> {
    db.conn()
        .query_row(
            &format!("SELECT {L_COLS} FROM loan WHERE id=?1"),
            rusqlite::params![id],
            map_loan,
        )
        .optional()
        .map_err(Into::into)
}

pub fn loan_save(db: &Db, l: &mut Loan) -> DbResult<i64> {
    let id = if l.id > 0 {
        db.conn().execute(
            "UPDATE loan SET kind=?2, no=?3, bank=?4, principal=?5, start_date=?6, end_date=?7,
             status=?8, rate_pct=?9, memo=?10 WHERE id=?1",
            rusqlite::params![
                l.id, l.kind, l.no, l.bank, crate::money_param(l.principal),
                l.start_date.format("%Y-%m-%d").to_string(),
                l.end_date.format("%Y-%m-%d").to_string(),
                l.status, crate::exact_param(l.rate_pct), l.memo
            ],
        )?;
        l.id
    } else {
        db.conn().execute(
            "INSERT INTO loan(kind,no,bank,principal,start_date,end_date,status,rate_pct,memo,created_by,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            rusqlite::params![
                l.kind, l.no, l.bank, crate::money_param(l.principal),
                l.start_date.format("%Y-%m-%d").to_string(),
                l.end_date.format("%Y-%m-%d").to_string(),
                l.status, crate::exact_param(l.rate_pct), l.memo, l.created_by, now()
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    l.id = id;
    Ok(id)
}

pub fn loan_delete(db: &Db, id: i64) -> DbResult<()> {
    let (a, b): (Option<i64>, Option<i64>) = db
        .conn()
        .query_row(
            "SELECT voucher_id, settle_voucher_id FROM loan WHERE id=?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found("融资"))?;
    if a.is_some() || b.is_some() {
        return Err(
            fincore::FinError::state("该融资已生成台账凭证，不能删除（可先作废凭证）").into(),
        );
    }
    db.conn()
        .execute("DELETE FROM loan WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 在事务内构建融资凭证草稿（drawdown=到账/放出，settle=还本/收回），返回凭证 id。
fn loan_voucher_in(
    tx: &rusqlite::Transaction,
    l: &Loan,
    phase: &str,
    date: NaiveDate,
    who: &str,
) -> Result<i64, DbError> {
    let bank_aux = AuxRef {
        bank: Some("B01".into()),
        ..Default::default()
    };
    // 放款/收回的对方（借款人）没有独立字段，用机构字段占位作员工辅助（仅查非空）
    let emp_aux = AuxRef {
        employee: Some(if l.bank.is_empty() {
            "其他".to_string()
        } else {
            l.bank.clone()
        }),
        ..Default::default()
    };
    let (dr, dr_aux, cr, cr_aux, memo) = match (l.kind.as_str(), phase) {
        ("borrow", "drawdown") => (
            "100201",
            bank_aux,
            "2001",
            AuxRef::default(),
            format!("融资到账 {}", l.no),
        ),
        ("lend", "drawdown") => (
            "122105",
            emp_aux,
            "100201",
            bank_aux,
            format!("融资放出 {}", l.no),
        ),
        ("borrow", "settle") => (
            "2001",
            AuxRef::default(),
            "100201",
            bank_aux,
            format!("融资还本 {}（利息另行入账）", l.no),
        ),
        ("lend", "settle") => (
            "100201",
            bank_aux,
            "122105",
            emp_aux,
            format!("融资收回 {}（收益另行入账）", l.no),
        ),
        _ => return Err(fincore::FinError::state("未知融资类型").into()),
    };
    let period = Period::from_date(date);
    let no = crate::vouchers::next_no_of(tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    v.memo = memo.clone();
    v.push_entry(Entry {
        debit: l.principal,
        aux: dr_aux,
        ..Entry::new(1, dr, memo.as_str())
    });
    v.push_entry(Entry {
        credit: l.principal,
        aux: cr_aux,
        ..Entry::new(2, cr, memo.as_str())
    });
    v.renumber();
    crate::vouchers::save_in(tx, &mut v)
}

/// 结清融资：置 settled + 落结清日期，并在**同一事务**自动生成还本/收回凭证草稿。
/// 幂等：重复结清不重复出凭证；返回本次新生成的凭证 id（已存在则 None）。
pub fn loan_settle(db: &Db, id: i64, date: NaiveDate, who: &str) -> DbResult<Option<i64>> {
    let tx = db.write_tx()?;
    let mut l: Loan = tx
        .query_row(
            &format!("SELECT {L_COLS} FROM loan WHERE id=?1"),
            rusqlite::params![id],
            map_loan,
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found("融资"))?;
    if l.status != "settled" {
        tx.execute(
            "UPDATE loan SET status='settled', settle_date=?2 WHERE id=?1",
            rusqlite::params![id, date.format("%Y-%m-%d").to_string()],
        )?;
        l.status = "settled".to_string();
        l.settle_date = Some(date);
    }
    let mut vid = None;
    if l.settle_voucher_id.is_none() {
        // 存量已结清记录没有 settle_date 时回退到本次传入日期
        let d = l.settle_date.unwrap_or(date);
        let v = loan_voucher_in(&tx, &l, "settle", d, who)?;
        tx.execute(
            "UPDATE loan SET settle_voucher_id=?2 WHERE id=?1 AND settle_voucher_id IS NULL",
            rusqlite::params![id, v],
        )?;
        vid = Some(v);
    }
    tx.commit()?;
    Ok(vid)
}

/// 融资台账出凭证（回填/主动出凭证）：
/// - 存续中 → 到账/放出凭证（记 start_date）；
/// - 已结清 → 还本/收回凭证（记 settle_date，缺失回退 end_date）。
pub fn loan_voucher(db: &Db, id: i64, who: &str) -> DbResult<i64> {
    let l = loan_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("融资"))?;
    let tx = db.write_tx()?;
    let (vid, col) = if l.status == "active" {
        if l.voucher_id.is_some() {
            return Err(fincore::FinError::state("该融资已生成过到账凭证").into());
        }
        let v = loan_voucher_in(&tx, &l, "drawdown", l.start_date, who)?;
        let taken = tx.execute(
            "UPDATE loan SET voucher_id=?2 WHERE id=?1 AND voucher_id IS NULL",
            rusqlite::params![id, v],
        )?;
        if taken == 0 {
            return Err(fincore::FinError::state("该融资已生成过到账凭证").into());
        }
        (v, "voucher_id")
    } else {
        if l.settle_voucher_id.is_some() {
            return Err(fincore::FinError::state("该融资已生成过还本凭证").into());
        }
        let d = l.settle_date.unwrap_or(l.end_date);
        let v = loan_voucher_in(&tx, &l, "settle", d, who)?;
        let taken = tx.execute(
            "UPDATE loan SET settle_voucher_id=?2 WHERE id=?1 AND settle_voucher_id IS NULL",
            rusqlite::params![id, v],
        )?;
        if taken == 0 {
            return Err(fincore::FinError::state("该融资已生成过还本凭证").into());
        }
        (v, "settle_voucher_id")
    };
    let _ = col;
    tx.commit()?;
    Ok(vid)
}

// ===========================================================================
// 资金日报 / 资金预测
// ===========================================================================

/// 单个现金/银行科目的资金日报行
#[derive(Clone, Debug, serde::Serialize)]
pub struct FundsDailyRow {
    pub account_code: String,
    pub account_name: String,
    /// 期初结存（带符号，借方为正）
    pub begin: Money,
    /// 本期收入
    pub income: Money,
    /// 本期支出
    pub expense: Money,
    /// 期末结存
    pub end: Money,
}

/// 资金日报：各现金/银行科目的期初、收入、支出、结存
pub fn funds_daily(db: &Db, period: Period) -> DbResult<Vec<FundsDailyRow>> {
    let accounts = crate::accounts::list(db)?;
    let cash_bank: Vec<_> = accounts
        .iter()
        .filter(|a| a.is_cash || a.is_bank)
        .collect();
    let snap = crate::balances::BalanceSnapshot::load(
        db,
        &crate::balances::BalanceQuery::period(period),
    )?;
    let mut out = Vec::new();
    for a in cash_bank {
        let row = snap.for_account(&a.code, None);
        out.push(FundsDailyRow {
            account_code: a.code.clone(),
            account_name: a.name.clone(),
            begin: row.begin,
            income: row.debit,
            expense: row.credit,
            end: row.end(),
        });
    }
    Ok(out)
}

/// 资金日报（按日）行
#[derive(Clone, Debug, serde::Serialize)]
pub struct FundsDateRow {
    pub account_code: String,
    pub account_name: String,
    /// 上日结余（账套期初 + 该日之前全部已记账发生额，跨期累计；借正）
    pub begin: Money,
    /// 本日收入（借方发生）
    pub income: Money,
    /// 本日支出（贷方发生）
    pub expense: Money,
    /// 日末结存 = 上日结余 + 收入 − 支出
    pub end: Money,
}

/// 资金日报（按日）：指定日期各现金/银行科目的上日结余、本日收支、日末结存。
///
/// - 口径：仅已记账（H-3），与账簿/报表一致；金额 Rust 侧 Decimal 逐笔累加（绝不 SQL SUM）。
/// - 科目集合与期间口径的 `funds_daily` 相同；上级科目按编码前缀汇总（语义同账簿 LIKE）。
/// - 日期为 ISO 文本（`YYYY-MM-DD` 字典序即时间序），直接字符串比较。
pub fn funds_daily_by_date(db: &Db, date: NaiveDate) -> DbResult<Vec<FundsDateRow>> {
    let day = date.format("%Y-%m-%d").to_string();
    let accounts = crate::accounts::list(db)?;
    let cash_bank: Vec<_> = accounts
        .iter()
        .filter(|a| a.is_cash || a.is_bank)
        .collect();
    if cash_bank.is_empty() {
        return Ok(Vec::new());
    }
    // 根科目（不被其他现金/银行科目前缀包含者）：取数按其前缀覆盖全部下级，
    // 与账簿/余额的 LIKE 汇总语义一致
    let roots: Vec<&String> = cash_bank
        .iter()
        .map(|a| &a.code)
        .filter(|c| {
            !cash_bank
                .iter()
                .any(|o| &o.code != *c && o.code.len() < c.len() && c.starts_with(&o.code))
        })
        .collect();
    let mut conds = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(day.clone())];
    for r in &roots {
        conds.push("e.account_code LIKE ?");
        params.push(Box::new(format!("{}%", r)));
    }
    let sql = format!(
        "SELECT e.account_code, v.date, e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON v.id = e.voucher_id
         WHERE v.status = 'posted' AND v.date <= ?1 AND ({})",
        conds.join(" OR ")
    );
    // 逐笔聚合：末级科目 → (该日前累计净额, 当日借, 当日贷)
    let mut agg: std::collections::BTreeMap<String, (Money, Money, Money)> =
        std::collections::BTreeMap::new();
    {
        let mut stmt = db.conn().prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut rr = stmt.query(refs.as_slice())?;
        while let Some(r) = rr.next()? {
            let code: String = r.get(0)?;
            let dstr: String = r.get(1)?;
            let debit = m(&r.get::<_, String>(2)?);
            let credit = m(&r.get::<_, String>(3)?);
            let e = agg
                .entry(code)
                .or_insert((Money::ZERO, Money::ZERO, Money::ZERO));
            if dstr == day {
                e.1 += debit;
                e.2 += credit;
            } else {
                e.0 += debit - credit;
            }
        }
    }
    // 按现金/银行科目出日报行（上级科目前缀汇总其下级）
    let mut out = Vec::new();
    for a in &cash_bank {
        let (mut begin, mut income, mut expense) = (Money::ZERO, Money::ZERO, Money::ZERO);
        for (leaf, v) in &agg {
            if leaf.starts_with(&a.code) {
                begin += v.0;
                income += v.1;
                expense += v.2;
            }
        }
        let end = begin + income - expense;
        out.push(FundsDateRow {
            account_code: a.code.clone(),
            account_name: a.name.clone(),
            begin,
            income,
            expense,
            end,
        });
    }
    Ok(out)
}

/// 资金预算 vs 执行（现金/银行科目）：预算取科目预算表当期行（当前版本），
/// 实际 = 当期发生净额（`budget_vs_actual` 走 `BalanceQuery`，仅已记账 H-3）。
/// 过滤规则：预算科目本身是现金/银行科目，或是其上级科目。
pub fn funds_budget(db: &Db, period: Period) -> DbResult<Vec<crate::mgmt::BudgetRow>> {
    let cash_bank: Vec<String> = crate::accounts::list(db)?
        .iter()
        .filter(|a| a.is_cash || a.is_bank)
        .map(|a| a.code.clone())
        .collect();
    let rows = crate::mgmt::budget_vs_actual(db, period, period)?;
    Ok(rows
        .into_iter()
        .filter(|r| {
            cash_bank.iter().any(|c| *c == r.account_code)
                || cash_bank.iter().any(|c| c.starts_with(&r.account_code))
        })
        .collect())
}

/// 资金预测（头寸）
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct FundsForecast {
    /// 现金/银行结存
    pub cash_balance: Money,
    /// 在库应收票据
    pub receivable_bills: Money,
    /// 在库应付票据
    pub payable_bills: Money,
    /// 放款（可收回）
    pub lend: Money,
    /// 借款（需偿还）
    pub borrow: Money,
    /// 预计可用资金头寸
    pub position: Money,
}

/// 资金预测：结存 + 应收票据 − 应付票据 + 放款 − 借款
pub fn funds_forecast(db: &Db, period: Period) -> DbResult<FundsForecast> {
    let daily = funds_daily(db, period)?;
    let cash_balance: Money = daily.iter().map(|d| d.end).sum();

    let mut fc = FundsForecast {
        cash_balance,
        ..Default::default()
    };
    for b in bill_list(db, None)? {
        if b.status != BillStatus::InHand.code() {
            continue;
        }
        if b.kind == "receivable" {
            fc.receivable_bills += b.amount;
        } else {
            fc.payable_bills += b.amount;
        }
    }
    for l in loan_list(db, None)? {
        if l.status != "active" {
            continue;
        }
        if l.kind == "lend" {
            fc.lend += l.principal;
        } else {
            fc.borrow += l.principal;
        }
    }
    fc.position = cash_balance + fc.receivable_bills - fc.payable_bills + fc.lend - fc.borrow;
    Ok(fc)
}

/// 滚动资金预测行（按期间展开：票据到期 + 融资到/还款 → 期末结存）
#[derive(Clone, Debug, serde::Serialize)]
pub struct RollingRow {
    pub period: Period,
    pub bill_in: Money,
    pub bill_out: Money,
    pub loan_in: Money,
    pub loan_out: Money,
    pub net: Money,
    pub balance: Money,
}

/// 滚动资金预测：从 `from` 起 N 期，按**到期日/起止日**展开在库票据与存续融资，
/// 逐期结存 = 上期结存 + 本期净流。口径声明：不含未到期未核销往来（应收/应付按账龄
/// 分布预测留待 v2）；起始结存 = 该期现金/银行科目期末（已记账 H-3）。
pub fn funds_forecast_rolling(db: &Db, from: Period, periods: usize) -> DbResult<Vec<RollingRow>> {
    let daily = funds_daily(db, from)?;
    let mut balance: Money = daily.iter().map(|d| d.end).sum();
    let n = periods.clamp(1, 24);
    let mut out = Vec::with_capacity(n);
    let mut p = from;
    for _ in 0..n {
        let mut bill_in = Money::ZERO;
        let mut bill_out = Money::ZERO;
        for b in bill_list(db, None)? {
            if b.status != BillStatus::InHand.code() {
                continue;
            }
            if Period::from_date(b.due_date).ymm() != p.ymm() {
                continue;
            }
            if b.kind == "receivable" {
                bill_in = bill_in + b.amount;
            } else {
                bill_out = bill_out + b.amount;
            }
        }
        let mut loan_in = Money::ZERO;
        let mut loan_out = Money::ZERO;
        for l in loan_list(db, None)? {
            if l.status != "active" {
                continue;
            }
            // 放款（lend）在 start 日出账、end 日收回；借款（borrow）反之
            if Period::from_date(l.start_date).ymm() == p.ymm() {
                if l.kind == "lend" {
                    loan_out = loan_out + l.principal;
                } else {
                    loan_in = loan_in + l.principal;
                }
            }
            if Period::from_date(l.end_date).ymm() == p.ymm() {
                if l.kind == "lend" {
                    loan_in = loan_in + l.principal;
                } else {
                    loan_out = loan_out + l.principal;
                }
            }
        }
        let net = bill_in - bill_out + loan_in - loan_out;
        balance = balance + net;
        out.push(RollingRow {
            period: p,
            bill_in,
            bill_out,
            loan_in,
            loan_out,
            net,
            balance,
        });
        p = p.next();
    }
    Ok(out)
}

// ===========================================================================
// 现金盘点（出纳）
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CashCount {
    pub id: i64,
    pub period: Period,
    pub date: NaiveDate,
    pub account_code: String,
    /// 盘点时账面余额快照（借正，仅已记账口径，保存时按资金日报口径重算）
    pub book_amount: Money,
    /// 实盘金额
    pub counted: Money,
    /// 差异 = 实盘 − 账面（正=盘盈 负=盘亏）
    pub diff: Money,
    pub memo: String,
    /// 盘盈盘亏凭证
    #[serde(default)]
    pub voucher_id: Option<i64>,
    pub created_by: String,
    pub created_at: String,
}

fn map_count(r: &rusqlite::Row) -> rusqlite::Result<CashCount> {
    let date: String = r.get(2)?;
    Ok(CashCount {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        date: NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1977, 1, 1).unwrap()),
        account_code: r.get(3)?,
        book_amount: m(&r.get::<_, String>(4)?),
        counted: m(&r.get::<_, String>(5)?),
        diff: m(&r.get::<_, String>(6)?),
        memo: r.get(7)?,
        voucher_id: r.get(8)?,
        created_by: r.get(9)?,
        created_at: r.get(10)?,
    })
}

const C_COLS: &str =
    "id,period,date,account_code,book_amount,counted,diff,memo,voucher_id,created_by,created_at";

pub fn cash_count_list(db: &Db) -> DbResult<Vec<CashCount>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {C_COLS} FROM cash_count ORDER BY date DESC, id DESC"))?;
    let rows = st.query_map([], map_count)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn cash_count_get(db: &Db, id: i64) -> DbResult<Option<CashCount>> {
    db.conn()
        .query_row(
            &format!("SELECT {C_COLS} FROM cash_count WHERE id=?1"),
            rusqlite::params![id],
            map_count,
        )
        .optional()
        .map_err(Into::into)
}

/// 保存现金盘点：账面余额按「资金日报（按日）」口径重算快照（仅已记账），差异随之计算。
pub fn cash_count_save(db: &Db, c: &mut CashCount) -> DbResult<i64> {
    c.period = Period::from_date(c.date);
    let book = funds_daily_by_date(db, c.date)?
        .into_iter()
        .find(|r| r.account_code == c.account_code)
        .map(|r| r.end)
        .unwrap_or(Money::ZERO);
    c.book_amount = book;
    c.diff = c.counted - book;
    let id = if c.id > 0 {
        db.conn().execute(
            "UPDATE cash_count SET period=?2, date=?3, account_code=?4, book_amount=?5,
             counted=?6, diff=?7, memo=?8 WHERE id=?1",
            rusqlite::params![
                c.id, c.period.ymm(), c.date.format("%Y-%m-%d").to_string(),
                c.account_code, crate::money_param(c.book_amount),
                crate::money_param(c.counted), crate::money_param(c.diff), c.memo
            ],
        )?;
        c.id
    } else {
        db.conn().execute(
            "INSERT INTO cash_count(period,date,account_code,book_amount,counted,diff,memo,
             voucher_id,created_by,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,NULL,?8,?9)",
            rusqlite::params![
                c.period.ymm(), c.date.format("%Y-%m-%d").to_string(),
                c.account_code, crate::money_param(c.book_amount),
                crate::money_param(c.counted), crate::money_param(c.diff), c.memo,
                c.created_by, now()
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    c.id = id;
    Ok(id)
}

/// 盘盈盘亏差异生成凭证：盘盈 借盘点科目 / 贷 1901；盘亏 借 1901 / 贷盘点科目。
/// （1901 待处理财产损溢的后续处理由手工凭证完成）
pub fn cash_count_voucher(db: &Db, id: i64, who: &str) -> DbResult<i64> {
    let c = cash_count_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("盘点记录"))?;
    if c.voucher_id.is_some() {
        return Err(fincore::FinError::state("该盘点记录已生成过凭证").into());
    }
    if c.diff.is_zero() {
        return Err(fincore::FinError::state("账实相符，无需生成凭证").into());
    }
    let tx = db.write_tx()?;
    let period = c.period;
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, c.date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    let profit = c.diff.is_positive(); // 正=盘盈
    let memo = if profit {
        format!("现金盘盈 {} {}", c.account_code, c.date.format("%Y-%m-%d"))
    } else {
        format!("现金盘亏 {} {}", c.account_code, c.date.format("%Y-%m-%d"))
    };
    v.memo = memo.clone();
    let amt = c.diff.abs();
    if profit {
        v.push_entry(Entry {
            debit: amt,
            ..Entry::new(1, c.account_code.as_str(), memo.as_str())
        });
        v.push_entry(Entry {
            credit: amt,
            ..Entry::new(2, "1901", memo.as_str())
        });
    } else {
        v.push_entry(Entry {
            debit: amt,
            ..Entry::new(1, "1901", memo.as_str())
        });
        v.push_entry(Entry {
            credit: amt,
            ..Entry::new(2, c.account_code.as_str(), memo.as_str())
        });
    }
    v.renumber();
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    let taken = tx.execute(
        "UPDATE cash_count SET voucher_id=?2 WHERE id=?1 AND voucher_id IS NULL",
        rusqlite::params![id, vid],
    )?;
    if taken == 0 {
        return Err(fincore::FinError::state("该盘点记录已生成过凭证").into());
    }
    tx.commit()?;
    Ok(vid)
}

/// 删除盘点记录（已挂凭证则拒绝）
pub fn cash_count_delete(db: &Db, id: i64) -> DbResult<()> {
    let vid: Option<i64> = db
        .conn()
        .query_row(
            "SELECT voucher_id FROM cash_count WHERE id=?1",
            rusqlite::params![id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found("盘点记录"))?;
    if vid.is_some() {
        return Err(
            fincore::FinError::state("该盘点记录已生成凭证，不能删除（可先作废凭证）").into(),
        );
    }
    db.conn()
        .execute("DELETE FROM cash_count WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

// ===========================================================================
// 日清标记（出纳日记账）
// ===========================================================================

/// 某科目在 [from, to] 内已日清的日期（YYYY-MM-DD 文本集合）
pub fn day_clear_dates(
    db: &Db,
    account: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> DbResult<std::collections::BTreeSet<String>> {
    let mut st = db.conn().prepare(
        "SELECT date FROM day_clear WHERE account_code=?1 AND date >= ?2 AND date <= ?3",
    )?;
    let rows = st.query_map(
        rusqlite::params![
            account,
            from.format("%Y-%m-%d").to_string(),
            to.format("%Y-%m-%d").to_string()
        ],
        |r| r.get::<_, String>(0),
    )?;
    let mut out = std::collections::BTreeSet::new();
    for d in rows {
        out.insert(d?);
    }
    Ok(out)
}

/// 日清标记 / 取消（出纳确认当日账实、账账相符）
pub fn day_clear_set(
    db: &Db,
    account: &str,
    date: NaiveDate,
    clear: bool,
    who: &str,
) -> DbResult<()> {
    let ds = date.format("%Y-%m-%d").to_string();
    if clear {
        db.conn().execute(
            "INSERT OR REPLACE INTO day_clear(account_code, date, cleared_by, cleared_at)
             VALUES(?1,?2,?3,?4)",
            rusqlite::params![account, ds, who, now()],
        )?;
    } else {
        db.conn().execute(
            "DELETE FROM day_clear WHERE account_code=?1 AND date=?2",
            rusqlite::params![account, ds],
        )?;
    }
    Ok(())
}

// ===========================================================================
// 支票登记簿（出纳备查簿，不入账）
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CheckRow {
    pub id: i64,
    pub no: String,
    /// cash=现金支票 / transfer=转账支票
    pub kind: String,
    /// 付款银行科目（如 100201）
    pub bank_account: String,
    pub payee: String,
    pub amount: Money,
    pub issued_date: NaiveDate,
    /// issued=已开出 / void=已作废
    pub status: String,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
}

fn map_check(r: &rusqlite::Row) -> rusqlite::Result<CheckRow> {
    let d: String = r.get(6)?;
    Ok(CheckRow {
        id: r.get(0)?,
        no: r.get(1)?,
        kind: r.get(2)?,
        bank_account: r.get(3)?,
        payee: r.get(4)?,
        amount: m(&r.get::<_, String>(5)?),
        issued_date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        status: r.get(7)?,
        memo: r.get(8)?,
        created_by: r.get(9)?,
        created_at: r.get(10)?,
    })
}

const CK_COLS: &str =
    "id,no,kind,bank_account,payee,amount,issued_date,status,memo,created_by,created_at";

pub fn check_list(db: &Db) -> DbResult<Vec<CheckRow>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {CK_COLS} FROM check_register ORDER BY issued_date DESC, id DESC"))?;
    let rows = st.query_map([], map_check)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn check_save(db: &Db, c: &mut CheckRow) -> DbResult<i64> {
    let id = if c.id > 0 {
        db.conn().execute(
            "UPDATE check_register SET no=?2, kind=?3, bank_account=?4, payee=?5, amount=?6,
             issued_date=?7, status=?8, memo=?9 WHERE id=?1",
            rusqlite::params![
                c.id, c.no, c.kind, c.bank_account, c.payee, crate::money_param(c.amount),
                c.issued_date.format("%Y-%m-%d").to_string(), c.status, c.memo
            ],
        )?;
        c.id
    } else {
        db.conn().execute(
            "INSERT INTO check_register(no,kind,bank_account,payee,amount,issued_date,status,memo,
             created_by,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                c.no, c.kind, c.bank_account, c.payee, crate::money_param(c.amount),
                c.issued_date.format("%Y-%m-%d").to_string(), c.status, c.memo,
                c.created_by, now()
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    c.id = id;
    Ok(id)
}

/// 支票状态：issued ↔ void（作废可恢复）
pub fn check_set_status(db: &Db, id: i64, status: &str) -> DbResult<()> {
    if status != "issued" && status != "void" {
        return Err(fincore::FinError::state("支票状态只能是已开出/已作废").into());
    }
    let n = db
        .conn()
        .execute(
            "UPDATE check_register SET status=?2 WHERE id=?1",
            rusqlite::params![id, status],
        )?;
    if n == 0 {
        return Err(fincore::FinError::not_found("支票").into());
    }
    Ok(())
}

pub fn check_delete(db: &Db, id: i64) -> DbResult<()> {
    let n = db
        .conn()
        .execute("DELETE FROM check_register WHERE id=?1", rusqlite::params![id])?;
    if n == 0 {
        return Err(fincore::FinError::not_found("支票").into());
    }
    Ok(())
}

// ===========================================================================
// 员工借支（出纳：预借 → 支付 → 冲账核销）
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Advance {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    pub employee: String,
    pub purpose: String,
    pub amount: Money,
    /// 支付账户（1001/100201...）
    pub pay_account: String,
    /// approved → paid → settled
    pub status: String,
    pub paid_date: Option<NaiveDate>,
    pub paid_voucher_id: Option<i64>,
    pub settle_date: Option<NaiveDate>,
    pub settle_voucher_id: Option<i64>,
    /// 冲账费用科目（核销时使用）
    pub expense_account: String,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
}

fn opt_date(s: Option<String>) -> Option<NaiveDate> {
    s.and_then(|x| NaiveDate::parse_from_str(&x, "%Y-%m-%d").ok())
}

fn map_advance(r: &rusqlite::Row) -> rusqlite::Result<Advance> {
    let d: String = r.get(3)?;
    Ok(Advance {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        employee: r.get(4)?,
        purpose: r.get(5)?,
        amount: m(&r.get::<_, String>(6)?),
        pay_account: r.get(7)?,
        status: r.get(8)?,
        paid_date: opt_date(r.get(9)?),
        paid_voucher_id: r.get(10)?,
        settle_date: opt_date(r.get(11)?),
        settle_voucher_id: r.get(12)?,
        expense_account: r.get(13)?,
        memo: r.get(14)?,
        created_by: r.get(15)?,
        created_at: r.get(16)?,
    })
}

const A_COLS: &str = "id,no,period,date,employee,purpose,amount,pay_account,status,paid_date,\
     paid_voucher_id,settle_date,settle_voucher_id,expense_account,memo,created_by,created_at";

pub fn advance_list(db: &Db) -> DbResult<Vec<Advance>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {A_COLS} FROM advance ORDER BY date DESC, id DESC"))?;
    let rows = st.query_map([], map_advance)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn advance_get(db: &Db, id: i64) -> DbResult<Option<Advance>> {
    db.conn()
        .query_row(
            &format!("SELECT {A_COLS} FROM advance WHERE id=?1"),
            rusqlite::params![id],
            map_advance,
        )
        .optional()
        .map_err(Into::into)
}

pub fn advance_save(db: &Db, a: &mut Advance) -> DbResult<i64> {
    if a.employee.trim().is_empty() {
        return Err(fincore::FinError::validate("借支人必填").into());
    }
    if !a.amount.is_positive() {
        return Err(fincore::FinError::validate("借支金额必须大于 0").into());
    }
    a.period = Period::from_date(a.date);
    let id = if a.id > 0 {
        db.conn().execute(
            "UPDATE advance SET no=?2, period=?3, date=?4, employee=?5, purpose=?6, amount=?7,
             pay_account=?8, expense_account=?9, memo=?10 WHERE id=?1",
            rusqlite::params![
                a.id, a.no, a.period.ymm(), a.date.format("%Y-%m-%d").to_string(),
                a.employee, a.purpose, crate::money_param(a.amount), a.pay_account,
                a.expense_account, a.memo
            ],
        )?;
        a.id
    } else {
        db.conn().execute(
            "INSERT INTO advance(no,period,date,employee,purpose,amount,pay_account,status,
             expense_account,memo,created_by,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'approved',?8,?9,?10,?11)",
            rusqlite::params![
                a.no, a.period.ymm(), a.date.format("%Y-%m-%d").to_string(),
                a.employee, a.purpose, crate::money_param(a.amount), a.pay_account,
                a.expense_account, a.memo, a.created_by, now()
            ],
        )?;
        db.conn().last_insert_rowid()
    };
    a.id = id;
    Ok(id)
}

/// 支付借支：借 122105 其他应收款（员工辅助）/ 贷 支付账户；状态→paid，同事务挂凭证。
/// 幂等：已支付/已核销返回 Ok(None) 不重复出凭证。
pub fn advance_pay(db: &Db, id: i64, date: NaiveDate, who: &str) -> DbResult<Option<i64>> {
    let tx = db.write_tx()?;
    let mut a: Advance = tx
        .query_row(
            &format!("SELECT {A_COLS} FROM advance WHERE id=?1"),
            rusqlite::params![id],
            map_advance,
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found("借支单"))?;
    if a.status != "approved" {
        return Ok(None); // 已支付/已核销：幂等
    }
    let period = Period::from_date(date);
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    let memo = format!("借支支付 {} {}", a.no, a.employee);
    v.memo = memo.clone();
    let bank_like = a.pay_account.starts_with("1002");
    v.push_entry(Entry {
        debit: a.amount,
        aux: AuxRef {
            employee: Some(a.employee.clone()),
            ..Default::default()
        },
        ..Entry::new(1, "122105", memo.as_str())
    });
    v.push_entry(Entry {
        credit: a.amount,
        aux: if bank_like {
            AuxRef {
                bank: Some("B01".into()),
                ..Default::default()
            }
        } else {
            AuxRef::default()
        },
        ..Entry::new(2, a.pay_account.as_str(), memo.as_str())
    });
    v.renumber();
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    let taken = tx.execute(
        "UPDATE advance SET status='paid', paid_date=?2, paid_voucher_id=?3
         WHERE id=?1 AND status='approved'",
        rusqlite::params![id, date.format("%Y-%m-%d").to_string(), vid],
    )?;
    if taken == 0 {
        return Err(fincore::FinError::state("该借支已支付过").into());
    }
    tx.commit()?;
    Ok(Some(vid))
}

/// 核销借支：借 冲账费用(expense) + 借 退回现金(refund) / 贷 122105（全额），状态→settled。
/// expense + refund = 借支金额；refund < 0 拒绝。幂等：已核销返回 Ok(None)。
pub fn advance_settle(
    db: &Db,
    id: i64,
    expense_account: &str,
    expense: Money,
    date: NaiveDate,
    who: &str,
) -> DbResult<Option<i64>> {
    let tx = db.write_tx()?;
    let mut a: Advance = tx
        .query_row(
            &format!("SELECT {A_COLS} FROM advance WHERE id=?1"),
            rusqlite::params![id],
            map_advance,
        )
        .optional()?
        .ok_or_else(|| fincore::FinError::not_found("借支单"))?;
    if a.status == "settled" {
        return Ok(None);
    }
    if a.status != "paid" {
        return Err(fincore::FinError::state("只有已支付的借支才能核销").into());
    }
    if expense.is_negative() {
        return Err(fincore::FinError::validate("冲账金额不能为负").into());
    }
    if expense > a.amount {
        return Err(fincore::FinError::validate(format!(
            "冲账金额 {} 不能超过借支金额 {}",
            expense, a.amount
        ))
        .into());
    }
    let refund = a.amount - expense;
    let period = Period::from_date(date);
    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = VoucherSource::Business;
    let memo = format!("借支核销 {} {}", a.no, a.employee);
    v.memo = memo.clone();
    let recv_aux = AuxRef {
        employee: Some(a.employee.clone()),
        ..Default::default()
    };
    let mut line = 1;
    if !expense.is_zero() {
        v.push_entry(Entry {
            debit: expense,
            ..Entry::new(line, expense_account, memo.as_str())
        });
        line += 1;
    }
    if !refund.is_zero() {
        let bank_like = a.pay_account.starts_with("1002");
        v.push_entry(Entry {
            debit: refund,
            aux: if bank_like {
                AuxRef {
                    bank: Some("B01".into()),
                    ..Default::default()
                }
            } else {
                AuxRef::default()
            },
            ..Entry::new(line, a.pay_account.as_str(), memo.as_str())
        });
        line += 1;
    }
    v.push_entry(Entry {
        credit: a.amount,
        aux: recv_aux,
        ..Entry::new(line, "122105", memo.as_str())
    });
    v.renumber();
    let vid = crate::vouchers::save_in(&tx, &mut v)?;
    let taken = tx.execute(
        "UPDATE advance SET status='settled', settle_date=?2, settle_voucher_id=?3,
         expense_account=?4 WHERE id=?1 AND status='paid' AND settle_voucher_id IS NULL",
        rusqlite::params![
            id,
            date.format("%Y-%m-%d").to_string(),
            vid,
            expense_account
        ],
    )?;
    if taken == 0 {
        return Err(fincore::FinError::state("该借支已核销过").into());
    }
    tx.commit()?;
    Ok(Some(vid))
}

/// 删除借支单：已支付/已核销（有凭证）则拒绝
pub fn advance_delete(db: &Db, id: i64) -> DbResult<()> {
    let a = advance_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("借支单"))?;
    if a.paid_voucher_id.is_some() || a.settle_voucher_id.is_some() {
        return Err(
            fincore::FinError::state("该借支已生成凭证，不能删除（可先作废凭证）").into(),
        );
    }
    db.conn()
        .execute("DELETE FROM advance WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

// ===========================================================================
// 出纳交接班（现金/银行/票据/日清快照 + 接班人确认）
// ===========================================================================

/// 交班单（对标金蝶出纳交接：交班快照 + 接班确认，防止责任不清）
#[derive(Clone, Debug, serde::Serialize)]
pub struct CashShift {
    pub id: i64,
    pub period: Period,
    pub date: NaiveDate,
    pub from_user: String,
    pub to_user: String,
    /// 当日现金科目日末结存（已记账 H-3 口径，下同）
    pub cash_balance: Money,
    /// 当日银行科目日末结存
    pub bank_balance: Money,
    /// 在库票据张数（应收 + 应付）
    pub bill_count: i64,
    /// 在库票据面值合计
    pub bill_amount: Money,
    /// 当日未日清的现金/银行账户数
    pub uncleared: i64,
    pub memo: String,
    /// open / confirmed / cancelled
    pub status: String,
    pub created_at: String,
    pub confirmed_by: String,
    pub confirmed_at: String,
}

fn map_shift(r: &rusqlite::Row) -> rusqlite::Result<CashShift> {
    let date: String = r.get(2)?;
    let confirmed_at: Option<String> = r.get(14)?;
    Ok(CashShift {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        date: NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        from_user: r.get(3)?,
        to_user: r.get(4)?,
        cash_balance: m(&r.get::<_, String>(5)?),
        bank_balance: m(&r.get::<_, String>(6)?),
        bill_count: r.get(7)?,
        bill_amount: m(&r.get::<_, String>(8)?),
        uncleared: r.get(9)?,
        memo: r.get(10)?,
        status: r.get(11)?,
        created_at: r.get(12)?,
        confirmed_by: r.get(13)?,
        confirmed_at: confirmed_at.unwrap_or_default(),
    })
}

const S_COLS: &str = "id,period,date,from_user,to_user,cash_balance,bank_balance,bill_count,bill_amount,uncleared,memo,status,created_at,confirmed_by,confirmed_at";

pub fn cash_shift_list(db: &Db, limit: usize) -> DbResult<Vec<CashShift>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {S_COLS} FROM cash_shift ORDER BY date DESC, id DESC LIMIT ?1"
    ))?;
    let rows = st
        .query_map(rusqlite::params![limit.clamp(1, 500) as i64], map_shift)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn cash_shift_get(db: &Db, id: i64) -> DbResult<Option<CashShift>> {
    db.conn()
        .query_row(
            &format!("SELECT {S_COLS} FROM cash_shift WHERE id=?1"),
            rusqlite::params![id],
            map_shift,
        )
        .optional()
        .map_err(Into::into)
}

/// 创建交班单：服务端快照当日数据（现金/银行结存按已记账口径、在库票据、未日清账户数），
/// 不信前端传值；账期随交班日期。
pub fn cash_shift_create(
    db: &Db,
    date: NaiveDate,
    from_user: &str,
    to_user: &str,
    memo: &str,
) -> DbResult<i64> {
    let rows = funds_daily_by_date(db, date)?;
    let accounts = crate::accounts::list(db)?;
    let day = date.format("%Y-%m-%d").to_string();
    let mut cash = Money::ZERO;
    let mut bank = Money::ZERO;
    let mut uncleared = 0i64;
    for row in &rows {
        let Some(acc) = accounts.iter().find(|a| a.code == row.account_code) else {
            continue;
        };
        if !(acc.is_cash || acc.is_bank) {
            continue;
        }
        if acc.is_cash {
            cash = cash + row.end;
        }
        if acc.is_bank {
            bank = bank + row.end;
        }
        // 日清状态：该账户当日是否已标记（出纳确认账实相符）
        let cleared = day_clear_dates(db, &row.account_code, date, date)?;
        if !cleared.contains(&day) {
            uncleared += 1;
        }
    }
    let bills = bill_list(db, None)?;
    let in_hand: Vec<_> = bills.iter().filter(|b| b.status == "in_hand").collect();
    let bill_count = in_hand.len() as i64;
    let bill_amount = in_hand.iter().fold(Money::ZERO, |acc, b| acc + b.amount);
    let period = Period::from_date(date);
    db.conn().execute(
        "INSERT INTO cash_shift(period,date,from_user,to_user,cash_balance,bank_balance,
         bill_count,bill_amount,uncleared,memo,status,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'open',?11)",
        rusqlite::params![
            period.ymm(),
            day,
            from_user,
            to_user,
            crate::money_param(cash),
            crate::money_param(bank),
            bill_count,
            crate::money_param(bill_amount),
            uncleared,
            memo,
            now()
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

/// 接班确认：仅 open 状态；交班人不能自我确认（防自交自接）；条件更新防并发。
pub fn cash_shift_confirm(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let s = cash_shift_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("交班单"))?;
    if s.status != "open" {
        return Err(fincore::FinError::state("该交班单已确认或已取消").into());
    }
    if who == s.from_user {
        return Err(
            fincore::FinError::state("交班人不能确认自己的交班单，请由接班人确认").into(),
        );
    }
    let n = db.conn().execute(
        "UPDATE cash_shift SET status='confirmed', confirmed_by=?2, confirmed_at=?3
         WHERE id=?1 AND status='open'",
        rusqlite::params![id, who, now()],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("交班单已被处理").into());
    }
    Ok(())
}

/// 取消交班单（仅 open 状态）
pub fn cash_shift_cancel(db: &Db, id: i64) -> DbResult<()> {
    let n = db.conn().execute(
        "UPDATE cash_shift SET status='cancelled' WHERE id=?1 AND status='open'",
        rusqlite::params![id],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("仅待确认的交班单可取消").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_fund_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }
    fn d(y: i32, mo: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, mo, dd).unwrap()
    }
    fn mon(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    #[test]
    fn bill_flow_and_forecast() {
        let db = tmpdb("bill");
        let p = Period::new(2026, 1).unwrap();
        let mut b = Bill {
            id: 0, kind: "receivable".into(), no: "PJ001".into(), period: p,
            issue_date: d(2026, 1, 5), due_date: d(2026, 4, 5),
            counterpart: "客户甲".into(), bank: "工行".into(), amount: mon("10000"),
            status: "in_hand".into(), handled_date: None, memo: String::new(),
            created_by: "u".into(), created_at: String::new(), voucher_id: None,
        };
        bill_save(&db, &mut b).unwrap();
        assert_eq!(bill_list(&db, None).unwrap().len(), 1);

        // 背书后不再计入应收票据；背书是资金动作，同事务自动生成台账凭证
        let vid = bill_transition(&db, b.id, BillStatus::Endorsed, d(2026, 2, 1), "u").unwrap();
        assert!(vid.is_some(), "背书应自动生成台账凭证");
        let fc = funds_forecast(&db, p).unwrap();
        assert_eq!(fc.receivable_bills, mon("0"));

        // 已背书不可再流转
        assert!(bill_transition(&db, b.id, BillStatus::Discounted, d(2026, 2, 2), "u").is_err());

        // 凭证为草稿且分录正确（应收背书：抵应付账款 / 转出应收票据）
        let v = crate::vouchers::get(&db, vid.unwrap()).unwrap().unwrap();
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.entries[0].account_code, "220201");
        assert_eq!(v.entries[1].account_code, "112101");
        assert_eq!(v.entries[0].debit, mon("10000"));

        // 已生成凭证的票据不可删除
        assert!(bill_delete(&db, b.id).is_err());
    }

    #[test]
    fn loan_signed_and_settle() {
        let db = tmpdb("loan");
        let mut l = Loan {
            id: 0, kind: "borrow".into(), no: "DK001".into(), bank: "建行".into(),
            principal: mon("500000"), rate_pct: mon("4.35"),
            start_date: d(2026, 1, 1), end_date: d(2027, 1, 1),
            status: "active".into(), memo: String::new(),
            created_by: "u".into(), created_at: String::new(),
            voucher_id: None, settle_voucher_id: None, settle_date: None,
        };
        loan_save(&db, &mut l).unwrap();
        assert!(l.signed().is_negative());

        // 到账凭证：借 100201 / 贷 2001
        let dv = loan_voucher(&db, l.id, "u").unwrap();
        let v = crate::vouchers::get(&db, dv).unwrap().unwrap();
        assert_eq!(v.entries[0].account_code, "100201");
        assert_eq!(v.entries[1].account_code, "2001");
        assert_eq!(v.entries[0].debit, mon("500000"));
        // 幂等：重复出到账凭证被拒
        assert!(loan_voucher(&db, l.id, "u").is_err());

        // 结清 → 自动生成还本凭证（借 2001 / 贷 100201），幂等
        let sv = loan_settle(&db, l.id, d(2026, 6, 1), "u").unwrap();
        assert!(sv.is_some(), "结清应自动生成还本凭证");
        let v = crate::vouchers::get(&db, sv.unwrap()).unwrap().unwrap();
        assert_eq!(v.entries[0].account_code, "2001");
        assert_eq!(v.entries[1].account_code, "100201");
        assert!(loan_settle(&db, l.id, d(2026, 6, 2), "u").unwrap().is_none());
        assert_eq!(loan_get(&db, l.id).unwrap().unwrap().status, "settled");
        // 已出凭证的融资不可删除
        assert!(loan_delete(&db, l.id).is_err());
    }

    #[test]
    fn funds_daily_by_date_single_day() {
        let db = tmpdb("bydate");
        let p = Period::new(2026, 1).unwrap();
        let d10 = d(2026, 1, 10);
        let mut v = Voucher::new(p, d10, "记", crate::vouchers::next_no(&db, p, "记").unwrap());
        v.push_entry(fincore::Entry {
            debit: mon("1000"),
            ..fincore::Entry::new(1, "1001", "收款")
        });
        v.push_entry(fincore::Entry {
            credit: mon("1000"),
            ..fincore::Entry::new(2, "2001", "借款")
        });
        let id = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, id, "u").unwrap();

        let one = |rows: &Vec<FundsDateRow>, code: &str| -> FundsDateRow {
            rows.iter().find(|r| r.account_code == code).unwrap().clone()
        };
        // 前一日：无发生
        let r = one(&funds_daily_by_date(&db, d(2026, 1, 9)).unwrap(), "1001");
        assert_eq!(r.begin, mon("0"));
        assert_eq!(r.income, mon("0"));
        assert_eq!(r.end, mon("0"));
        // 当日：收入 1000、日末 1000
        let r = one(&funds_daily_by_date(&db, d10).unwrap(), "1001");
        assert_eq!(r.begin, mon("0"));
        assert_eq!(r.income, mon("1000"));
        assert_eq!(r.expense, mon("0"));
        assert_eq!(r.end, mon("1000"));
        // 次日：上日结余结转
        let r = one(&funds_daily_by_date(&db, d(2026, 1, 11)).unwrap(), "1001");
        assert_eq!(r.begin, mon("1000"));
        assert_eq!(r.income, mon("0"));
        assert_eq!(r.end, mon("1000"));
        // 银行科目行仍在（无发生 = 全零）；行集与 funds_daily 同源（accounts::list 过滤）
        let r = one(&funds_daily_by_date(&db, d10).unwrap(), "100201");
        assert_eq!(r.end, mon("0"));
    }

    #[test]
    fn advance_pay_and_settle_flow() {
        let db = tmpdb("advance");
        let mut a = Advance {
            id: 0,
            no: "JZ001".into(),
            period: Period::new(2026, 1).unwrap(),
            date: d(2026, 1, 8),
            employee: "张三".into(),
            purpose: "出差预借".into(),
            amount: mon("2000"),
            pay_account: "1001".into(),
            status: "approved".into(),
            paid_date: None,
            paid_voucher_id: None,
            settle_date: None,
            settle_voucher_id: None,
            expense_account: "660201".into(),
            memo: String::new(),
            created_by: "u".into(),
            created_at: String::new(),
        };
        advance_save(&db, &mut a).unwrap();
        assert_eq!(a.status, "approved");

        // 未支付不能核销
        assert!(advance_settle(&db, a.id, "660201", mon("500"), d(2026, 1, 20), "u").is_err());

        // 支付：借 122105（员工）/ 贷 1001
        let pvid = advance_pay(&db, a.id, d(2026, 1, 9), "u").unwrap().unwrap();
        let v = crate::vouchers::get(&db, pvid).unwrap().unwrap();
        assert_eq!(v.entries[0].account_code, "122105");
        assert_eq!(
            v.entries[0].aux.employee.as_deref(),
            Some("张三"),
            "其他应收款挂员工辅助"
        );
        assert_eq!(v.entries[1].account_code, "1001");
        // 幂等：重复支付不再出凭证
        assert!(advance_pay(&db, a.id, d(2026, 1, 9), "u").unwrap().is_none());
        assert_eq!(advance_get(&db, a.id).unwrap().unwrap().status, "paid");

        // 核销超额被拒
        assert!(advance_settle(&db, a.id, "660201", mon("2500"), d(2026, 1, 20), "u").is_err());
        // 核销：冲账 1500 + 退回现金 500 = 2000
        let svid = advance_settle(&db, a.id, "660201", mon("1500"), d(2026, 1, 20), "u")
            .unwrap()
            .unwrap();
        let v = crate::vouchers::get(&db, svid).unwrap().unwrap();
        assert_eq!(v.entries.len(), 3);
        assert_eq!(v.entries[0].account_code, "660201");
        assert_eq!(v.entries[0].debit, mon("1500"));
        assert_eq!(v.entries[1].account_code, "1001");
        assert_eq!(v.entries[1].debit, mon("500"));
        assert_eq!(v.entries[2].account_code, "122105");
        assert_eq!(v.entries[2].credit, mon("2000"));
        // 幂等 + 状态 + 守卫
        assert!(advance_settle(&db, a.id, "660201", mon("1500"), d(2026, 1, 21), "u")
            .unwrap()
            .is_none());
        assert_eq!(advance_get(&db, a.id).unwrap().unwrap().status, "settled");
        assert!(advance_delete(&db, a.id).is_err(), "已出凭证不可删除");

        // 全额退回（expense=0）：借 1001 / 贷 122105 单腿对
        let mut a2 = Advance {
            id: 0,
            no: "JZ002".into(),
            period: Period::new(2026, 1).unwrap(),
            date: d(2026, 1, 8),
            employee: "李四".into(),
            purpose: "备用金".into(),
            amount: mon("800"),
            pay_account: "1001".into(),
            status: "approved".into(),
            paid_date: None,
            paid_voucher_id: None,
            settle_date: None,
            settle_voucher_id: None,
            expense_account: "660201".into(),
            memo: String::new(),
            created_by: "u".into(),
            created_at: String::new(),
        };
        advance_save(&db, &mut a2).unwrap();
        advance_pay(&db, a2.id, d(2026, 1, 9), "u").unwrap();
        let svid = advance_settle(&db, a2.id, "660201", mon("0"), d(2026, 1, 21), "u")
            .unwrap()
            .unwrap();
        let v = crate::vouchers::get(&db, svid).unwrap().unwrap();
        assert_eq!(v.entries.len(), 2, "全额退回时无费用腿：{:?}", v.entries);
        assert_eq!(v.entries[0].account_code, "1001");
        assert_eq!(v.entries[1].account_code, "122105");
        // 未支付的可以删除
        let mut a3 = a2.clone();
        a3.id = 0;
        a3.no = "JZ003".into();
        advance_save(&db, &mut a3).unwrap();
        advance_delete(&db, a3.id).unwrap();
    }

    #[test]
    fn day_clear_toggle_and_check_book() {
        let db = tmpdb("cashier");
        // 日清：标记 → 按科目隔离查询 → 取消
        day_clear_set(&db, "1001", d(2026, 1, 10), true, "u").unwrap();
        let set = day_clear_dates(&db, "1001", d(2026, 1, 1), d(2026, 1, 31)).unwrap();
        assert!(set.contains("2026-01-10"));
        assert!(!day_clear_dates(&db, "100201", d(2026, 1, 1), d(2026, 1, 31))
            .unwrap()
            .contains("2026-01-10"));
        day_clear_set(&db, "1001", d(2026, 1, 10), false, "u").unwrap();
        assert!(day_clear_dates(&db, "1001", d(2026, 1, 1), d(2026, 1, 31))
            .unwrap()
            .is_empty());

        // 支票簿：新增 → 作废 → 恢复 → 非法状态拒绝 → 删除
        let mut c = CheckRow {
            id: 0,
            no: "ZP001".into(),
            kind: "transfer".into(),
            bank_account: "100201".into(),
            payee: "供应商甲".into(),
            amount: mon("3000"),
            issued_date: d(2026, 1, 15),
            status: "issued".into(),
            memo: String::new(),
            created_by: "u".into(),
            created_at: String::new(),
        };
        check_save(&db, &mut c).unwrap();
        assert_eq!(check_list(&db).unwrap().len(), 1);
        assert_eq!(check_list(&db).unwrap()[0].amount, mon("3000"));
        check_set_status(&db, c.id, "void").unwrap();
        assert_eq!(check_list(&db).unwrap()[0].status, "void");
        assert!(check_set_status(&db, c.id, "bad").is_err());
        check_set_status(&db, c.id, "issued").unwrap();
        check_delete(&db, c.id).unwrap();
        assert!(check_list(&db).unwrap().is_empty());
        assert!(check_delete(&db, 999).is_err(), "删除不存在的应报未找到");
    }

    #[test]
    fn cash_count_book_snapshot_and_voucher() {
        let db = tmpdb("count");
        let p = Period::new(2026, 1).unwrap();
        let d10 = d(2026, 1, 10);
        // 记一笔现金收入 800（已记账）
        let mut v = Voucher::new(p, d10, "记", crate::vouchers::next_no(&db, p, "记").unwrap());
        v.push_entry(fincore::Entry {
            debit: mon("800"),
            ..fincore::Entry::new(1, "1001", "收款")
        });
        v.push_entry(fincore::Entry {
            credit: mon("800"),
            ..fincore::Entry::new(2, "2001", "借款")
        });
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "u").unwrap();

        // 盘盈：实盘 850 > 账面 800 → diff +50；凭证 借1001/贷1901
        let mut c = CashCount {
            id: 0,
            period: p,
            date: d(2026, 1, 12),
            account_code: "1001".into(),
            book_amount: Money::ZERO,
            counted: mon("850"),
            diff: Money::ZERO,
            memo: String::new(),
            voucher_id: None,
            created_by: "u".into(),
            created_at: String::new(),
        };
        cash_count_save(&db, &mut c).unwrap();
        assert_eq!(c.book_amount, mon("800"), "账面快照应取该日已记账结存");
        assert_eq!(c.diff, mon("50"));
        let cvid = cash_count_voucher(&db, c.id, "u").unwrap();
        let v = crate::vouchers::get(&db, cvid).unwrap().unwrap();
        assert_eq!(v.entries[0].account_code, "1001");
        assert_eq!(v.entries[1].account_code, "1901");
        assert!(cash_count_voucher(&db, c.id, "u").is_err(), "不可重复出凭证");
        assert!(cash_count_delete(&db, c.id).is_err(), "已挂凭证不可删除");

        // 盘亏：实盘 700 < 账面 800 → 借1901/贷1001
        let mut c2 = CashCount {
            id: 0,
            period: p,
            date: d(2026, 1, 13),
            account_code: "1001".into(),
            book_amount: Money::ZERO,
            counted: mon("700"),
            diff: Money::ZERO,
            memo: String::new(),
            voucher_id: None,
            created_by: "u".into(),
            created_at: String::new(),
        };
        cash_count_save(&db, &mut c2).unwrap();
        assert_eq!(c2.diff, mon("-100"));
        let v2 = crate::vouchers::get(&db, cash_count_voucher(&db, c2.id, "u").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(v2.entries[0].account_code, "1901");
        assert_eq!(v2.entries[1].account_code, "1001");

        // 账实相符：差异为 0，不能出凭证
        let mut c3 = CashCount {
            id: 0,
            period: p,
            date: d(2026, 1, 14),
            account_code: "1001".into(),
            book_amount: Money::ZERO,
            counted: mon("800"),
            diff: Money::ZERO,
            memo: String::new(),
            voucher_id: None,
            created_by: "u".into(),
            created_at: String::new(),
        };
        cash_count_save(&db, &mut c3).unwrap();
        assert!(c3.diff.is_zero());
        assert!(cash_count_voucher(&db, c3.id, "u").is_err());
        // 相符的记录可以删除
        cash_count_delete(&db, c3.id).unwrap();
    }

    #[test]
    fn funds_budget_filters_cash_accounts() {
        let db = tmpdb("fbudget");
        let p = Period::new(2026, 1).unwrap();
        // 现金凭证 1000（已记账）
        let d10 = d(2026, 1, 10);
        let mut v = Voucher::new(p, d10, "记", crate::vouchers::next_no(&db, p, "记").unwrap());
        v.push_entry(fincore::Entry {
            debit: mon("1000"),
            ..fincore::Entry::new(1, "1001", "收款")
        });
        v.push_entry(fincore::Entry {
            credit: mon("1000"),
            ..fincore::Entry::new(2, "2001", "借款")
        });
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "u").unwrap();

        // 预算：资金科目1001=1500、非资金科目660201=9999（后者应被过滤）
        for (code, amt) in [("1001", "1500"), ("660201", "9999")] {
            crate::mgmt::budget_upsert(
                &db,
                &crate::mgmt::Budget {
                    id: 0,
                    period: p,
                    account_code: code.into(),
                    dept: String::new(),
                    amount: mon(amt),
                    memo: String::new(),
                    version: String::new(),
                },
            )
            .unwrap();
        }
        let rows = funds_budget(&db, p).unwrap();
        assert_eq!(rows.len(), 1, "只保留资金科目预算：{:?}", rows);
        assert_eq!(rows[0].account_code, "1001");
        assert_eq!(rows[0].budget, mon("1500"));
        assert_eq!(rows[0].actual, mon("1000"), "实际=当期已记账净额");
    }

    #[test]
    fn funds_daily_sums_cash_bank() {
        let db = tmpdb("daily");
        let p = Period::new(2026, 1).unwrap();
        let rows = funds_daily(&db, p).unwrap();
        // 内置科目表里 1001 现金 / 1002 银行存款 标记了 is_cash / is_bank
        assert!(rows.iter().any(|r| r.account_code == "1001"));
        assert!(rows.iter().any(|r| r.account_code.starts_with("1002")));
    }
}
