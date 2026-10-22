//! 往来对账单（对标金蝶「客户对账单 / 往来对账」）
//!
//! 与 [`crate::settle`] 里的**催款单**是两回事，别混：
//!
//! | | 催款单（dunning） | 对账单（statement，本模块） |
//! |---|---|---|
//! | 目的 | 催收欠款 | 定期发给客户对账 |
//! | 口径 | 某一时点的**未核销**欠款快照 | 某期间的**完整流水**（含已核销） |
//! | 金额 | 一个欠款总额 | 期初 + 本期发生 + 期末，带滚动余额 |
//! | 闭环 | 无 | 客户**回签确认**（confirmed） |
//!
//! 口径说明：
//! - 只取**已记账**分录（`status='posted'`，全仓 H-3 定案），与试算平衡、账簿、
//!   往来核销一致。草稿不进对账单——对账单是要给客户看的法律凭据。
//! - 余额带符号：**正 = 客户欠款**（ar 借方净额），负 = 预收/我方欠款。
//! - **创建时快照**：期初/本期发生/期末与明细行全部算好落库。之后再改凭证、
//!   再核销，都不影响已发出的对账单——这是对账单的意义所在（金蝶的「反审核单据
//!   进行反核销」也是同一个道理：已确认的单据不能被后台操作改掉）。
//! - 往来期初（`arap_opening` 影子挂账）计入期初余额，口径与催款单一致。

use fincore::{Money, Period};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::{Db, DbResult};

const ST_COLS: &str = "id,no,kind,account,party_code,party_name,period_from,period_to,\
     begin_balance,period_debit,period_credit,end_balance,line_count,status,memo,\
     created_by,created_at,sent_at,confirmed_by,confirmed_at,lines_json";

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

/// 对账单明细行（快照）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatementLine {
    /// 日期（期初行是期初日期）
    pub date: String,
    /// 凭证号（期初行为「期初」）
    pub doc_no: String,
    pub summary: String,
    /// 本期增加（ar 借方 / ap 贷方——站在对方角度看，「他欠我/我欠他」都记在借方侧）
    pub increase: Money,
    /// 本期减少
    pub decrease: Money,
    /// 截至本行的滚动余额（正=对方欠款）
    pub balance: Money,
}

/// 对账单
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Statement {
    pub id: i64,
    pub no: String,
    /// ar 应收对账单 / ap 应付对账单
    pub kind: String,
    pub account: String,
    pub party_code: String,
    pub party_name: String,
    pub period_from: Period,
    pub period_to: Period,
    /// 期初余额（正=对方欠款）
    pub begin_balance: Money,
    /// 本期增加
    pub period_increase: Money,
    /// 本期减少
    pub period_decrease: Money,
    /// 期末 = 期初 + 增加 - 减少
    pub end_balance: Money,
    pub line_count: i64,
    /// draft / sent / confirmed / cancelled
    pub status: String,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
    pub sent_at: String,
    /// 客户回签确认人（金蝶：客户盖章/签字确认）
    pub confirmed_by: String,
    pub confirmed_at: String,
    pub lines: Vec<StatementLine>,
}

fn map(r: &rusqlite::Row) -> rusqlite::Result<Statement> {
    let lines_json: String = r.get(20)?;
    Ok(Statement {
        id: r.get(0)?,
        no: r.get(1)?,
        kind: r.get(2)?,
        account: r.get(3)?,
        party_code: r.get(4)?,
        party_name: r.get(5)?,
        period_from: Period::from_ymm(r.get(6)?),
        period_to: Period::from_ymm(r.get(7)?),
        begin_balance: m(&r.get::<_, String>(8)?),
        period_increase: m(&r.get::<_, String>(9)?),
        period_decrease: m(&r.get::<_, String>(10)?),
        end_balance: m(&r.get::<_, String>(11)?),
        line_count: r.get(12)?,
        status: r.get(13)?,
        memo: r.get(14)?,
        created_by: r.get(15)?,
        created_at: r.get(16)?,
        sent_at: r.get(17)?,
        confirmed_by: r.get(18)?,
        confirmed_at: r.get(19)?,
        lines: serde_json::from_str(&lines_json).unwrap_or_default(),
    })
}

pub fn get(db: &Db, id: i64) -> DbResult<Option<Statement>> {
    db.conn()
        .query_row(
            &format!("SELECT {ST_COLS} FROM ar_statement WHERE id=?1"),
            [id],
            map,
        )
        .optional()
        .map_err(Into::into)
}

pub fn list(db: &Db, kind: Option<&str>) -> DbResult<Vec<Statement>> {
    let mut out = Vec::new();
    match kind.filter(|k| !k.trim().is_empty()) {
        Some(k) => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {ST_COLS} FROM ar_statement WHERE kind=?1 ORDER BY period_to DESC, id DESC"
            ))?;
            for r in st.query_map(rusqlite::params![k], map)? {
                out.push(r?);
            }
        }
        None => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {ST_COLS} FROM ar_statement ORDER BY period_to DESC, id DESC"
            ))?;
            for r in st.query_map([], map)? {
                out.push(r?);
            }
        }
    }
    Ok(out)
}

/// 单号：DB{期间}{序号}，循环跳号避免与历史撞号
fn next_no(db: &Db, period: Period) -> DbResult<String> {
    let mut n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM ar_statement WHERE period_to=?1",
        rusqlite::params![period.ymm()],
        |r| r.get(0),
    )?;
    loop {
        n += 1;
        let no = format!("DB{}{:03}", period.ymm(), n);
        let exists: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM ar_statement WHERE no=?1",
            rusqlite::params![no],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Ok(no);
        }
    }
}

/// 客商在某科目下的辅助维度取值
fn party_of(kind: &str, aux_key: &str) -> Option<String> {
    let aux = fincore::voucher::AuxRef::from_key(aux_key);
    if kind == "ar" {
        aux.customer
    } else {
        aux.supplier
    }
}

/// 生成对账单（快照）：期初 + 本期流水 + 期末，逐行带滚动余额
pub fn create(
    db: &Db,
    kind: &str,
    account: &str,
    party_code: &str,
    party_name: &str,
    from: Period,
    to: Period,
    memo: &str,
    who: &str,
) -> DbResult<Statement> {
    let kind = match kind.trim() {
        "ar" => "ar",
        "ap" => "ap",
        _ => {
            return Err(
                fincore::FinError::msg("类型只能是 ar（应收对账单）或 ap（应付对账单）").into(),
            )
        }
    };
    let party = party_code.trim();
    if party.is_empty() {
        return Err(fincore::FinError::msg("客商编码不能为空").into());
    }
    if to < from {
        return Err(fincore::FinError::msg("截止期间不能早于起始期间").into());
    }
    let account = if account.trim().is_empty() {
        if kind == "ar" {
            "1122"
        } else {
            "2202"
        }
    } else {
        account.trim()
    };

    // 取该科目下全部**已记账**分录（含已核销——对账单要对全流水），再按期间切分。
    // include_all=true 才会带上已核销的行。
    let all = crate::settle::open_entries_with(db, account, to, true, true)?;

    // 站在「对方欠我」的角度统一看：
    // - ar：借方=债权增加
    // - ap：贷方=债务增加（负债类科目，必须用 credit - debit）
    // 期初和本期必须用同一个方向，否则应付对账单的期初会变成负数，
    // 而本期仍按贷方为正，整张表的滚动余额自相矛盾。
    let signed = |e: &crate::settle::OpenEntry| {
        if kind == "ar" {
            e.signed()
        } else {
            e.credit - e.debit
        }
    };

    let mut begin = Money::ZERO;
    let mut lines: Vec<StatementLine> = Vec::new();

    // 期初：起始期间之前的分录净额（只留净额，不逐行铺开，否则单据多时对账单没法看）
    for e in &all {
        if e.period >= from {
            break;
        }
        if party_of(kind, &e.aux_key).as_deref() != Some(party) {
            continue;
        }
        begin += signed(e);
    }
    // 往来期初影子挂账（导入 / 手工登记的期初往来）
    for o in crate::settle::arap_opening_list(db, Some(kind))? {
        if o.party_code != party {
            continue;
        }
        let d = chrono::NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d")
            .unwrap_or_else(|_| chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
        if Period::from_date(d) >= from {
            continue;
        }
        begin += o.amount;
    }

    if !begin.is_zero() {
        lines.push(StatementLine {
            date: from.first_day().format("%Y-%m-%d").to_string(),
            doc_no: "期初".to_string(),
            summary: "上期结转".to_string(),
            increase: Money::ZERO,
            decrease: Money::ZERO,
            balance: begin,
        });
    }

    // 本期逐行：增加/减少 + 滚动余额
    let mut run = begin;
    let (mut inc, mut dec) = (Money::ZERO, Money::ZERO);
    for e in &all {
        if e.period < from || e.period > to {
            continue;
        }
        if party_of(kind, &e.aux_key).as_deref() != Some(party) {
            continue;
        }
        let amount = signed(e);
        if amount.is_zero() {
            continue;
        }
        let (increase, decrease) = if amount.is_positive() {
            (amount, Money::ZERO)
        } else {
            (Money::ZERO, amount.abs())
        };
        run += amount;
        inc += increase;
        dec += decrease;
        lines.push(StatementLine {
            date: e.date.format("%Y-%m-%d").to_string(),
            doc_no: format!("{}-{:04}", e.word, e.no),
            summary: e.summary.clone(),
            increase,
            decrease,
            balance: run,
        });
    }

    if lines.is_empty() {
        return Err(fincore::FinError::msg(
            "该客商在所选科目与期间内无已记账往来（或期初），无需对账",
        )
        .into());
    }

    // 勾稽自检：期初 + 增 - 减 必须等于期末，且末行余额必须等于期末。
    // 对账单是要发给客户的凭据，内部算不平就不能出。
    let end = begin + inc - dec;
    if end != run {
        return Err(fincore::FinError::msg(format!(
            "对账勾稽不平：期初 {begin} + 增 {inc} - 减 {dec} ≠ 末行余额 {run}"
        ))
        .into());
    }

    let no = next_no(db, to)?;
    let lines_json = serde_json::to_string(&lines)
        .map_err(|e| fincore::FinError::msg(format!("明细序列化失败：{e}")))?;
    db.conn().execute(
        "INSERT INTO ar_statement(no,kind,account,party_code,party_name,period_from,period_to,
         begin_balance,period_debit,period_credit,end_balance,line_count,
         status,memo,created_by,created_at,sent_at,confirmed_by,confirmed_at,lines_json)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'draft',?13,?14,?15,'','','',?16)",
        rusqlite::params![
            no,
            kind,
            account,
            party,
            party_name.trim(),
            from.ymm(),
            to.ymm(),
            crate::money_param(begin),
            crate::money_param(inc),
            crate::money_param(dec),
            crate::money_param(end),
            lines.len() as i64,
            memo,
            who,
            now(),
            lines_json
        ],
    )?;
    let id = db.conn().last_insert_rowid();
    get(db, id)?.ok_or_else(|| fincore::FinError::msg("对账单创建失败").into())
}

/// 状态流转：draft → sent；sent → confirmed/cancelled；confirmed 是终态。
///
/// 条件 UPDATE 防并发重复推进（和催款单同一套路）。
pub fn set_status(db: &Db, id: i64, status: &str, who: &str) -> DbResult<()> {
    let cur = get(db, id)?.ok_or_else(|| fincore::FinError::not_found("对账单"))?;
    let allowed = match status {
        // 发出：只能从草稿
        "sent" => cur.status == "draft",
        // 客户回签确认：草稿也能直接确认（线下补签），已发出后确认
        "confirmed" => cur.status == "draft" || cur.status == "sent",
        // 作废：已确认的不能作废（客户已认可的单据）
        "cancelled" => cur.status == "draft" || cur.status == "sent",
        _ => false,
    };
    if !allowed {
        return Err(fincore::FinError::state(format!(
            "对账单当前状态「{}」不能变更为「{}」",
            cur.status_label(),
            status_label(status)
        ))
        .into());
    }
    let (sent_at, cb, ca) = match status {
        "sent" => (now(), String::new(), String::new()),
        "confirmed" => (cur.sent_at.clone(), who.trim().to_string(), now()),
        _ => (cur.sent_at.clone(), String::new(), String::new()),
    };
    let n = db.conn().execute(
        "UPDATE ar_statement SET status=?2, sent_at=?3, confirmed_by=?4, confirmed_at=?5
         WHERE id=?1 AND status=?6",
        rusqlite::params![id, status, sent_at, cb, ca, cur.status],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("对账单状态已变化，请刷新后重试").into());
    }
    Ok(())
}

impl Statement {
    pub fn status_label(&self) -> &'static str {
        match self.status.as_str() {
            "draft" => "草稿",
            "sent" => "已发出",
            "confirmed" => "已回签确认",
            "cancelled" => "已作废",
            _ => "未知",
        }
    }
    /// 状态标签（UI 用）
    pub fn kind_label(&self) -> &'static str {
        if self.kind == "ar" {
            "应收对账单"
        } else {
            "应付对账单"
        }
    }
}

pub fn status_label(s: &str) -> &'static str {
    match s {
        "draft" => "草稿",
        "sent" => "已发出",
        "confirmed" => "已回签确认",
        "cancelled" => "已作废",
        _ => "未知",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use fincore::{AuxRef, Entry, Voucher};

    fn m_(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    /// 对方科目（不核算任何辅助维度）。不能用内置 600101——它核算部门，会被校验拦下。
    fn ensure_counter(db: &Db) {
        use fincore::account::{Account, AcctCategory};
        if crate::accounts::get(db, "600199").unwrap().is_none() {
            crate::accounts::insert(
                db,
                &Account::new("600199", "对账单对方科目", AcctCategory::Income),
            )
            .unwrap();
        }
    }

    /// 造一张 ar 凭证，返回 112201 分录 id。post=false 时停在草稿。
    fn ar(db: &Db, p: Period, day: u32, no: i32, amt: &str, post_it: bool) -> i64 {
        ensure_counter(db);
        let d = chrono::NaiveDate::from_ymd_opt(p.year(), p.month(), day).unwrap();
        let mut v = Voucher::new(p, d, "记", no);
        v.prepared_by = "u".into();
        let a = m_(amt);
        v.push_entry(Entry {
            debit: a,
            aux: AuxRef {
                customer: Some("C01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "销售应收")
        });
        v.push_entry(Entry {
            credit: a,
            ..Entry::new(2, "600199", "收入")
        });
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        if post_it {
            crate::vouchers::post(db, vid, "u").unwrap();
        }
        crate::vouchers::entries_of(db, vid).unwrap()[0].id
    }

    /// 造一张 ap 凭证：贷 220201（供应商），借对方科目。
    fn ap(db: &Db, p: Period, day: u32, no: i32, amt: &str, post_it: bool) -> i64 {
        ensure_counter(db);
        let d = chrono::NaiveDate::from_ymd_opt(p.year(), p.month(), day).unwrap();
        let mut v = Voucher::new(p, d, "记", no);
        v.prepared_by = "u".into();
        let a = m_(amt);
        v.push_entry(Entry {
            credit: a,
            aux: AuxRef {
                supplier: Some("S01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "220201", "采购应付")
        });
        v.push_entry(Entry {
            debit: a,
            ..Entry::new(2, "600199", "采购")
        });
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        if post_it {
            crate::vouchers::post(db, vid, "u").unwrap();
        }
        crate::vouchers::entries_of(db, vid).unwrap()[0].id
    }

    /// 应付对账单的期初必须与本期同向：贷方余额为正，不能出现负的期初。
    #[test]
    fn ap_statement_begin_is_credit_positive() {
        let db = mem();
        let p1 = Period::new(2026, 1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        ap(&db, p1, 10, 1, "1000", true); // 1 月形成 1000 应付
        ap(&db, p2, 10, 2, "500", true); //  2 月再增 500

        let s = create(&db, "ap", "220201", "S01", "供应商甲", p2, p2, "", "u").unwrap();
        assert_eq!(s.begin_balance, m_("1000"), "期初应付应为正数");
        assert_eq!(s.period_increase, m_("500"));
        assert_eq!(s.period_decrease, m_("0"));
        assert_eq!(s.end_balance, m_("1500"));
        let rows: Vec<Money> = s.lines.iter().map(|l| l.balance).collect();
        assert_eq!(rows, vec![m_("1000"), m_("1500")]);
    }

    /// 对账单的期初/本期/期末与逐行滚动余额必须自洽
    #[test]
    fn statement_rolls_forward_and_balances() {
        let db = mem();
        let p1 = Period::new(2026, 1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        ar(&db, p1, 10, 1, "1000", true); // 1 月形成 1000 欠款
        ar(&db, p2, 10, 2, "500", true); //  2 月再增 500

        let s = create(&db, "ar", "112201", "C01", "客户甲", p1, p2, "1-2月", "u").unwrap();
        assert!(s.no.starts_with(&format!("DB{}", p2.ymm())));
        assert_eq!(s.begin_balance, m_("0"), "账套首期无期初");
        assert_eq!(s.period_increase, m_("1500"));
        assert_eq!(s.period_decrease, m_("0"));
        assert_eq!(s.end_balance, m_("1500"));
        // 逐行滚动：1000 → 1500
        let rows: Vec<Money> = s.lines.iter().map(|l| l.balance).collect();
        assert_eq!(rows, vec![m_("1000"), m_("1500")]);
        // 勾稽：末行余额 == 期末
        assert_eq!(s.lines.last().unwrap().balance, s.end_balance);
    }

    /// 起始期间之后的凭证不计入期初；起始期间之前的才进期初
    #[test]
    fn statement_begin_balance_uses_prior_periods_only() {
        let db = mem();
        let p1 = Period::new(2026, 1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        let p3 = Period::new(2026, 3).unwrap();
        ar(&db, p1, 10, 1, "1000", true); // 期初侧
        ar(&db, p2, 10, 2, "200", true); // 本期侧
        ar(&db, p3, 10, 3, "700", true); // 期末之后，不该进

        let s = create(&db, "ar", "112201", "C01", "", p2, p2, "", "u").unwrap();
        assert_eq!(s.begin_balance, m_("1000"), "1 月应为期初");
        assert_eq!(s.period_increase, m_("200"));
        assert_eq!(s.end_balance, m_("1200"), "3 月那笔不得出现");
    }

    /// 未记账（草稿）不进对账单——对账单是给客户看的凭据
    #[test]
    fn statement_excludes_drafts() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        ar(&db, p, 10, 1, "1000", true);
        ar(&db, p, 11, 2, "500", false); // 草稿
        let s = create(&db, "ar", "112201", "C01", "", p, p, "", "u").unwrap();
        assert_eq!(s.end_balance, m_("1000"), "草稿不得进对账单");
        assert!(
            !s.lines
                .iter()
                .any(|l| l.summary.contains("销售应收") && l.increase == m_("500")),
            "明细行也不该出现草稿那笔"
        );
    }

    /// 减少方向（收款）应体现在「本期减少」，余额同向下降
    #[test]
    fn statement_handles_decrease_direction() {
        let db = mem();
        let p1 = Period::new(2026, 1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        ar(&db, p1, 10, 1, "1000", true);
        // 2 月收款：借 库存现金 400 / 贷 应收 400（贷应收才是减少欠款）
        let d = chrono::NaiveDate::from_ymd_opt(2026, 2, 10).unwrap();
        let mut v = Voucher::new(p2, d, "记", 2);
        v.prepared_by = "u".into();
        v.push_entry(Entry {
            debit: m_("400"),
            ..Entry::new(1, "1001", "收现")
        });
        v.push_entry(Entry {
            credit: m_("400"),
            aux: AuxRef {
                customer: Some("C01".into()),
                ..Default::default()
            },
            ..Entry::new(2, "112201", "收货款")
        });
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "u").unwrap();

        let s = create(&db, "ar", "112201", "C01", "", p1, p2, "", "u").unwrap();
        assert_eq!(s.begin_balance, m_("0"));
        assert_eq!(s.period_increase, m_("1000"));
        assert_eq!(s.period_decrease, m_("400"));
        assert_eq!(s.end_balance, m_("600"), "收款后余额应降到 600");
    }

    /// 无往来 / 非法参数要明确拒绝，别出一张空对账单
    #[test]
    fn statement_rejects_bad_input() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 该客商无任何往来
        assert!(create(&db, "ar", "112201", "NOBODY", "", p, p, "", "u").is_err());
        // 客商编码为空
        assert!(create(&db, "ar", "112201", "  ", "", p, p, "", "u").is_err());
        // 类型非法
        assert!(create(&db, "xx", "112201", "C01", "", p, p, "", "u").is_err());
        // 截止早于起始
        let p2 = Period::new(2026, 2).unwrap();
        ar(&db, p, 10, 1, "100", true);
        assert!(create(&db, "ar", "112201", "C01", "", p2, p, "", "u").is_err());
    }

    /// 状态机：草稿→已发出→已回签确认；已确认是终态，不可作废
    #[test]
    fn statement_status_flow() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        ar(&db, p, 10, 1, "100", true);
        let s = create(&db, "ar", "112201", "C01", "", p, p, "", "u").unwrap();
        assert_eq!(s.status, "draft");

        // 草稿可作废（还没发出去）
        set_status(&db, s.id, "cancelled", "u").unwrap();
        assert_eq!(get(&db, s.id).unwrap().unwrap().status, "cancelled");
        // 作废是终态，不能再发出
        assert!(set_status(&db, s.id, "sent", "u").is_err());

        let s2 = create(&db, "ar", "112201", "C01", "", p, p, "", "u").unwrap();
        set_status(&db, s2.id, "sent", "u").unwrap();
        assert_eq!(get(&db, s2.id).unwrap().unwrap().status, "sent");
        // 已发出后不能再发出
        assert!(set_status(&db, s2.id, "sent", "u").is_err());
        // 客户回签
        set_status(&db, s2.id, "confirmed", "客户王五").unwrap();
        let done = get(&db, s2.id).unwrap().unwrap();
        assert_eq!(done.status, "confirmed");
        assert_eq!(done.confirmed_by, "客户王五");
        assert!(!done.confirmed_at.is_empty());
        // 已确认是终态：既不能重复确认也不能作废
        assert!(set_status(&db, s2.id, "confirmed", "x").is_err());
        assert!(set_status(&db, s2.id, "cancelled", "x").is_err());
    }

    /// 快照语义：出账后改凭证/再记账，已发出的对账单数字不变
    #[test]
    fn statement_is_a_snapshot() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        ar(&db, p, 10, 1, "1000", true);
        let before = create(&db, "ar", "112201", "C01", "", p, p, "", "u").unwrap();
        assert_eq!(before.end_balance, m_("1000"));

        // 出账后再补一笔已记账业务
        ar(&db, p, 20, 2, "500", true);
        let after = get(&db, before.id).unwrap().unwrap();
        assert_eq!(
            after.end_balance,
            m_("1000"),
            "已生成的对账单不得被后续业务改动"
        );
        assert_eq!(after.lines.len(), before.lines.len());

        // 新建的对账单则反映最新数据
        let fresh = create(&db, "ar", "112201", "C01", "", p, p, "", "u").unwrap();
        assert_eq!(fresh.end_balance, m_("1500"));
    }
}
