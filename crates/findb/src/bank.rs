//! 银行对账（出纳模块）
//!
//! 流程：导入银行对账单 → 自动勾对 → 手工补勾 → 生成余额调节表。
//!
//! 自动勾对的判定顺序很关键，实务上按这个优先级最不容易勾错：
//! 1. 结算号相同（最可靠的锚点）
//! 2. 金额完全相同 + 方向相反 + 业务日期在 ±N 天内
//! 3. 金额完全相同 + 方向相反（日期不限）
//!
//! 一对多（一笔银行流水对多张凭证）在自动阶段不做，留给手工。

use chrono::NaiveDate;
use fincore::{Money, Period};

use crate::{balances, Db, DbResult};

/// 银行对账单流水
#[derive(Clone, Debug)]
pub struct Statement {
    pub id: i64,
    pub period: Period,
    pub account_code: String,
    pub biz_date: NaiveDate,
    pub summary: String,
    pub settle_no: String,
    /// 银行口径进账
    pub debit: Money,
    /// 银行口径支出
    pub credit: Money,
    /// 该笔后的银行余额
    pub balance: Money,
    pub entry_id: Option<i64>,
    pub matched_at: Option<String>,
    pub matched_by: Option<String>,
}

impl Statement {
    /// 带符号金额（进账为正）
    pub fn signed(&self) -> Money {
        self.debit - self.credit
    }
    pub fn matched(&self) -> bool {
        self.entry_id.is_some()
    }
}

/// 账面（凭证）这一侧的一笔银行收支
#[derive(Clone, Debug)]
pub struct BookEntry {
    pub entry_id: i64,
    pub voucher_id: i64,
    pub date: NaiveDate,
    pub word: String,
    pub no: i32,
    pub summary: String,
    pub settle_no: String,
    pub debit: Money,
    pub credit: Money,
}

impl BookEntry {
    pub fn signed(&self) -> Money {
        self.debit - self.credit
    }
    pub fn voucher_label(&self) -> String {
        format!("{}-{}", self.word, self.no)
    }
}

fn map_stmt(r: &rusqlite::Row) -> rusqlite::Result<Statement> {
    let d: String = r.get(3)?;
    Ok(Statement {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        account_code: r.get(2)?,
        biz_date: NaiveDate::parse_from_str(&d, "%Y-%m-%d").unwrap_or_else(|_| {
            NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
        }),
        summary: r.get(4)?,
        settle_no: r.get(5)?,
        debit: Money::parse_or_zero(&r.get::<_, String>(6)?),
        credit: Money::parse_or_zero(&r.get::<_, String>(7)?),
        balance: Money::parse_or_zero(&r.get::<_, String>(8)?),
        entry_id: r.get(9)?,
        matched_at: r.get(10)?,
        matched_by: r.get(11)?,
    })
}

const COLS: &str = "id,period,account_code,biz_date,summary,settle_no,debit,credit,balance,
     entry_id,matched_at,matched_by";

pub fn list(c: &impl crate::AsConn, period: Period, account: &str) -> DbResult<Vec<Statement>> {
    let mut st = c.conn_ref().prepare(&format!(
        "SELECT {COLS} FROM bank_statement WHERE period=?1 AND account_code=?2
         ORDER BY biz_date, id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![period.ymm(), account], map_stmt)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 本期本科目已导入的流水数
pub fn count(c: &impl crate::AsConn, period: Period, account: &str) -> DbResult<i64> {
    Ok(c.conn_ref().query_row(
        "SELECT COUNT(*) FROM bank_statement WHERE period=?1 AND account_code=?2",
        rusqlite::params![period.ymm(), account],
        |r| r.get(0),
    )?)
}

pub fn insert(c: &impl crate::AsConn, s: &Statement) -> DbResult<i64> {
    c.conn_ref().execute(
        "INSERT INTO bank_statement(period,account_code,biz_date,summary,settle_no,
            debit,credit,balance,entry_id,matched_at,matched_by)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        rusqlite::params![
            s.period.ymm(),
            s.account_code,
            s.biz_date.format("%Y-%m-%d").to_string(),
            s.summary,
            s.settle_no,
            crate::money_param(s.debit),
            crate::money_param(s.credit),
            crate::money_param(s.balance),
            s.entry_id,
            s.matched_at,
            s.matched_by
        ],
    )?;
    Ok(c.conn_ref().last_insert_rowid())
}

pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM bank_statement WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 清空某科目某期的对账单（重新导入前用）
pub fn clear(db: &Db, period: Period, account: &str) -> DbResult<usize> {
    Ok(db.conn().execute(
        "DELETE FROM bank_statement WHERE period=?1 AND account_code=?2",
        rusqlite::params![period.ymm(), account],
    )?)
}

/// 勾对：一条银行流水 ↔ 一条凭证分录
pub fn link(c: &impl crate::AsConn, stmt_id: i64, entry_id: i64, who: &str) -> DbResult<()> {
    c.conn_ref().execute(
        "UPDATE bank_statement SET entry_id=?2, matched_at=?3, matched_by=?4 WHERE id=?1",
        rusqlite::params![
            stmt_id,
            entry_id,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            who
        ],
    )?;
    Ok(())
}

pub fn unlink(db: &Db, stmt_id: i64) -> DbResult<()> {
    db.conn().execute(
        "UPDATE bank_statement SET entry_id=NULL, matched_at=NULL, matched_by=NULL WHERE id=?1",
        rusqlite::params![stmt_id],
    )?;
    Ok(())
}

/// 取消某科目某期的全部勾对
pub fn unlink_all(db: &Db, period: Period, account: &str) -> DbResult<usize> {
    Ok(db.conn().execute(
        "UPDATE bank_statement SET entry_id=NULL, matched_at=NULL, matched_by=NULL
         WHERE period=?1 AND account_code=?2",
        rusqlite::params![period.ymm(), account],
    )?)
}

// ---------------- 账面侧 ----------------

/// 取出某科目某期所有已记账的银行收支分录（含是否已被勾对）
pub fn book_side(c: &impl crate::AsConn, period: Period, account: &str) -> DbResult<Vec<BookEntry>> {
    let mut st = c.conn_ref().prepare(
        "SELECT e.id, v.id, v.date, v.word, v.no, e.summary,
                COALESCE(e.settle_no,''), e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON v.id = e.voucher_id
         WHERE e.account_code = ?1 AND v.period = ?2 AND v.status = 'posted'
           AND (e.debit <> '0' OR e.credit <> '0')
         ORDER BY v.date, v.no, e.line",
    )?;
    let rows = st
        .query_map(rusqlite::params![account, period.ymm()], |r| {
            let d: String = r.get(2)?;
            Ok(BookEntry {
                entry_id: r.get(0)?,
                voucher_id: r.get(1)?,
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                word: r.get(3)?,
                no: r.get(4)?,
                summary: r.get(5)?,
                settle_no: r.get(6)?,
                debit: Money::parse_or_zero(&r.get::<_, String>(7)?),
                credit: Money::parse_or_zero(&r.get::<_, String>(8)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 账面侧已被勾对的分录 id
pub fn linked_entry_ids(c: &impl crate::AsConn, period: Period, account: &str) -> DbResult<Vec<i64>> {
    let mut st = c.conn_ref().prepare(
        "SELECT entry_id FROM bank_statement
         WHERE period=?1 AND account_code=?2 AND entry_id IS NOT NULL",
    )?;
    let rows = st
        .query_map(rusqlite::params![period.ymm(), account], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------------- 自动勾对 ----------------

/// 自动勾对结果
#[derive(Clone, Debug, Default)]
pub struct MatchResult {
    /// 成功勾对的对数
    pub matched: usize,
    /// 按结算号勾上的
    pub by_no: usize,
    /// 按金额+日期勾上的
    pub by_amount_date: usize,
    /// 按金额勾上的
    pub by_amount: usize,
    /// 一笔银行流水对上多条凭证的（只标记不自动处理）
    pub ambiguous: usize,
}

/// 自动勾对。
///
/// `date_tolerance` 是日期容差天数，默认 3 天（跨月的银行流水很常见）。
pub fn auto_match(
    db: &Db,
    period: Period,
    account: &str,
    date_tolerance: i64,
    who: &str,
) -> DbResult<MatchResult> {
    // 读快照与写勾对必须在同一事务：勾对本质是「查哪些没勾 → 逐条写 entry_id」，
    // 两个并发 auto_match（或自动勾对撞上手工勾对）会各自基于旧快照判定，把同一
    // 条流水/分录勾到别处。BEGIN IMMEDIATE 让整个过程独占写锁。
    let tx = db.write_tx()?;
    let mut stmts = list(&tx, period, account)?;
    let books = book_side(&tx, period, account)?;
    let linked = linked_entry_ids(&tx, period, account)?;
    let linked: std::collections::HashSet<i64> = linked.into_iter().collect();

    let mut res = MatchResult::default();
    let mut used: std::collections::HashSet<i64> = std::collections::HashSet::new();
    // 本轮内已勾对成功的流水：stmts 里的 matched() 反映的是「进入本函数时」的状态，
    // link() 只写库不更新内存，若不单独记录，后续轮次会重复勾对同一笔流水并覆盖首次结果。
    let mut done_stmt: std::collections::HashSet<i64> = std::collections::HashSet::new();

    // 第一轮：结算号精确匹配
    for s in &stmts {
        if s.matched() || done_stmt.contains(&s.id) {
            continue;
        }
        if s.settle_no.trim().is_empty() {
            continue;
        }
        let cands: Vec<&BookEntry> = books
            .iter()
            .filter(|b| {
                !used.contains(&b.entry_id)
                    && !linked.contains(&b.entry_id)
                    && !b.settle_no.trim().is_empty()
                    && b.settle_no.trim() == s.settle_no.trim()
                    && b.signed() == s.signed()
            })
            .collect();
        if cands.len() == 1 {
            used.insert(cands[0].entry_id);
            done_stmt.insert(s.id);
            link(&tx, s.id, cands[0].entry_id, who)?;
            res.by_no += 1;
        } else if cands.len() > 1 {
            res.ambiguous += 1;
        }
    }

    // 第二轮：金额 + 方向 + 日期容差
    for s in &stmts {
        if s.matched() || done_stmt.contains(&s.id) {
            continue;
        }
        let cands: Vec<&BookEntry> = books
            .iter()
            .filter(|b| {
                !used.contains(&b.entry_id)
                    && !linked.contains(&b.entry_id)
                    && b.signed() == s.signed()
                    && (b.debit + b.credit).round2() == (s.debit + s.credit).round2()
                    && (b.date - s.biz_date).num_days().abs() <= date_tolerance
            })
            .collect();
        if cands.len() == 1 {
            used.insert(cands[0].entry_id);
            done_stmt.insert(s.id);
            link(&tx, s.id, cands[0].entry_id, who)?;
            res.by_amount_date += 1;
        } else if cands.len() > 1 {
            res.ambiguous += 1;
        }
    }

    // 第三轮：只看金额 + 方向（日期不限）
    for s in &stmts {
        if s.matched() || done_stmt.contains(&s.id) {
            continue;
        }
        let cands: Vec<&BookEntry> = books
            .iter()
            .filter(|b| {
                !used.contains(&b.entry_id)
                    && !linked.contains(&b.entry_id)
                    && b.signed() == s.signed()
                    && (b.debit + b.credit).round2() == (s.debit + s.credit).round2()
            })
            .collect();
        if cands.len() == 1 {
            used.insert(cands[0].entry_id);
            done_stmt.insert(s.id);
            link(&tx, s.id, cands[0].entry_id, who)?;
            res.by_amount += 1;
        } else if cands.len() > 1 {
            res.ambiguous += 1;
        }
    }

    // 重新读一次，统计最终勾对数（含本次之前已勾的）
    stmts = list(&tx, period, account)?;
    res.matched = stmts.iter().filter(|s| s.matched()).count();
    tx.commit()?;
    Ok(res)
}

// ---------------- 余额调节表 ----------------

/// 余额调节表
#[derive(Clone, Debug)]
pub struct Reconciliation {
    pub period: Period,
    pub account_code: String,
    /// 银行对账单期末余额
    pub bank_balance: Money,
    /// 企业账面期末余额（带符号，借方为正）
    pub book_balance: Money,
    /// 企业已收、银行未收（账面有、对账单没有的进账）
    pub book_only_in: Vec<BookEntry>,
    /// 企业已付、银行未付
    pub book_only_out: Vec<BookEntry>,
    /// 银行已收、企业未记
    pub bank_only_in: Vec<Statement>,
    /// 银行已付、企业未记
    pub bank_only_out: Vec<Statement>,
    /// 银行侧调节后余额
    pub bank_adjusted: Money,
    /// 企业侧调节后余额
    pub book_adjusted: Money,
}

impl Reconciliation {
    /// 两侧调节后余额是否一致（容差 0.01，避免分位尾差误报）
    pub fn balanced(&self) -> bool {
        (self.bank_adjusted - self.book_adjusted).abs() < Money::parse("0.01").unwrap()
    }
    /// 差额
    pub fn diff(&self) -> Money {
        self.bank_adjusted - self.book_adjusted
    }
}

pub fn reconcile(db: &Db, period: Period, account: &str) -> DbResult<Reconciliation> {
    let stmts = list(db, period, account)?;
    let books = book_side(db, period, account)?;
    let linked: std::collections::HashSet<i64> = linked_entry_ids(db, period, account)?
        .into_iter()
        .collect();

    let bank_balance = stmts.last().map(|s| s.balance).unwrap_or(Money::ZERO);

    let snap = balances::BalanceSnapshot::load(db, &balances::BalanceQuery::period(period))?;
    let book_balance = snap.for_account(account, None).end();

    let mut book_only_in = Vec::new();
    let mut book_only_out = Vec::new();
    for b in books {
        if linked.contains(&b.entry_id) {
            continue;
        }
        if b.signed().is_positive() {
            book_only_in.push(b);
        } else if b.signed().is_negative() {
            book_only_out.push(b);
        }
    }

    let mut bank_only_in = Vec::new();
    let mut bank_only_out = Vec::new();
    for s in stmts {
        if s.matched() {
            continue;
        }
        if s.signed().is_positive() {
            bank_only_in.push(s);
        } else if s.signed().is_negative() {
            bank_only_out.push(s);
        }
    }

    let sum_in: Money = book_only_in.iter().map(|b| b.signed()).sum();
    let sum_out: Money = book_only_out.iter().map(|b| b.signed().abs()).sum();
    let bank_in: Money = bank_only_in.iter().map(|s| s.signed()).sum();
    let bank_out: Money = bank_only_out.iter().map(|s| s.signed().abs()).sum();

    Ok(Reconciliation {
        period,
        account_code: account.to_string(),
        bank_balance,
        book_balance,
        bank_adjusted: bank_balance + sum_in - sum_out,
        book_adjusted: book_balance + bank_in - bank_out,
        book_only_in,
        book_only_out,
        bank_only_in,
        bank_only_out,
    })
}

// ---------------- CSV 导入 ----------------

/// 解析一行 CSV（支持逗号 / 制表符 / 分号分隔），返回字段向量
pub fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_q && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_q = !in_q;
                }
            }
            ',' | '\t' | ';' if !in_q => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// 日期解析：支持 `2026-01-05` `2026/1/5` `20260105` `2026年1月5日`
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    for f in ["%Y-%m-%d", "%Y/%m/%d", "%Y.%m.%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return Some(d);
        }
    }
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) {
        let y = s[0..4].parse().ok()?;
        let m = s[4..6].parse().ok()?;
        let d = s[6..8].parse().ok()?;
        return NaiveDate::from_ymd_opt(y, m, d);
    }
    // 2026年1月5日
    let t = s.replace(['年', '月'], "-").replace('日', "");
    NaiveDate::parse_from_str(&t, "%Y-%m-%d").ok()
}

/// 金额解析：去掉千分位、货币符号、括号负数
pub fn parse_money(s: &str) -> Money {
    let mut t = s.trim().replace([',', '￥', '¥', '$', ' '], "");
    let neg = t.starts_with('(') && t.ends_with(')');
    if neg {
        t = t.trim_start_matches('(').trim_end_matches(')').to_string();
    }
    let v = Money::parse_or_zero(&t);
    if neg {
        v.negated()
    } else {
        v
    }
}

/// 导入对账单。
///
/// 列顺序（首行是表头则自动跳过）：业务日期, 摘要, 结算号, 借方发生额, 贷方发生额, 余额
/// 也兼容"单金额列"格式：业务日期, 摘要, 结算号, 金额, 余额（负数表示支出）。
/// 返回 (导入条数, 跳过的行数)。
pub fn import_csv(
    db: &Db,
    period: Period,
    account: &str,
    text: &str,
) -> DbResult<(usize, Vec<String>)> {
    let mut warns: Vec<String> = Vec::new();
    // 先把能解析的行全部解析出来，再一次事务写库：逐行自动提交的话，导入中途
    // 失败会留下半份对账单，重新导入同一文件就会重复。
    let mut rows: Vec<Statement> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let f = split_csv(line);
        // 表头：第一列不是合法日期就跳过
        if i == 0 && parse_date(&f[0]).is_none() {
            continue;
        }
        let date = match parse_date(f.first().unwrap_or(&String::new())) {
            Some(d) => d,
            None => {
                warns.push(format!("第 {} 行：日期无法识别，已跳过", i + 1));
                continue;
            }
        };
        if f.len() < 3 {
            warns.push(format!("第 {} 行：列数不足，已跳过", i + 1));
            continue;
        }
        let summary = f.get(1).cloned().unwrap_or_default();
        let settle_no = f.get(2).cloned().unwrap_or_default();

        let (debit, credit, balance) = if f.len() >= 6 {
            let d = parse_money(f.get(3).unwrap_or(&String::new()));
            let c = parse_money(f.get(4).unwrap_or(&String::new()));
            (d, c, parse_money(f.get(5).unwrap_or(&String::new())))
        } else {
            // 单金额列
            let v = parse_money(f.get(3).unwrap_or(&String::new()));
            let bal = parse_money(f.get(4).unwrap_or(&String::new()));
            if v.is_negative() {
                (Money::ZERO, v.abs(), bal)
            } else {
                (v, Money::ZERO, bal)
            }
        };

        if debit.is_zero() && credit.is_zero() {
            warns.push(format!("第 {} 行：金额为零，已跳过", i + 1));
            continue;
        }

        rows.push(Statement {
            id: 0,
            period,
            account_code: account.to_string(),
            biz_date: date,
            summary,
            settle_no,
            debit,
            credit,
            balance,
            entry_id: None,
            matched_at: None,
            matched_by: None,
        });
    }
    let n = rows.len();
    let tx = db.write_tx()?;
    for s in &rows {
        insert(&tx, s)?;
    }
    tx.commit()?;
    Ok((n, warns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::voucher::{Entry, Voucher, VoucherStatus};
    use fincore::Period;

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_bank_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }

    #[allow(dead_code)]
    fn stmt(date: &str, summary: &str, no: &str, signed: &str, bal: &str) -> Statement {
        let v = Money::parse(signed).unwrap();
        Statement {
            id: 0,
            period: Period::new(2026, 1).unwrap(),
            account_code: "100201".into(),
            biz_date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            summary: summary.into(),
            settle_no: no.into(),
            debit: if v.is_positive() { v } else { Money::ZERO },
            credit: if v.is_negative() { v.abs() } else { Money::ZERO },
            balance: Money::parse(bal).unwrap(),
            entry_id: None,
            matched_at: None,
            matched_by: None,
        }
    }

    #[test]
    fn csv_helpers() {
        assert_eq!(
            parse_date("2026-01-05").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
        );
        assert_eq!(
            parse_date("2026/1/5").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
        );
        assert_eq!(
            parse_date("20260105").unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()
        );
        assert_eq!(parse_money("1,234.56"), Money::parse("1234.56").unwrap());
        assert_eq!(parse_money("(500)"), Money::parse("-500").unwrap());
        assert_eq!(parse_money("￥88.80"), Money::parse("88.80").unwrap());
        let f = split_csv("a,\"b,c\",d");
        assert_eq!(f, vec!["a", "b,c", "d"]);
    }

    #[test]
    fn import_and_auto_match() {
        let db = tmpdb("match");
        let p = Period::new(2026, 1).unwrap();

        // 100201 / 1122 在内置科目表里已有，直接用
        // 一张已记账的收款凭证
        let d = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let mut v = Voucher::new(p, d, "记", 1);
        v.push_entry(Entry {
            debit: Money::parse("1000").unwrap(),
            settle_no: Some("SN001".into()),
            aux: fincore::voucher::AuxRef {
                bank: Some("BANK01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "100201", "收货款")
        });
        v.push_entry(Entry {
            credit: Money::parse("1000").unwrap(),
            aux: fincore::voucher::AuxRef {
                customer: Some("C01".into()),
                ..Default::default()
            },
            ..Entry::new(2, "112201", "收货款")
        });
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "poster").unwrap();
        let _ = VoucherStatus::Posted;

        // 导入对账单（6 列格式）
        let csv = "业务日期,摘要,结算号,借方发生额,贷方发生额,余额\n\
                   2026-01-06,收到货款,SN001,1000.00,0.00,101000.00\n\
                   2026-01-20,支付手续费,,0.00,15.00,100985.00\n";
        let (n, warns) = import_csv(&db, p, "100201", csv).unwrap();
        assert_eq!(n, 2, "warnings={warns:?}");
        assert!(warns.is_empty());

        let res = auto_match(&db, p, "100201", 3, "tester").unwrap();
        assert_eq!(res.by_no, 1);
        assert_eq!(res.matched, 1);

        let rows = list(&db, p, "100201").unwrap();
        assert!(rows[0].matched());
        assert!(!rows[1].matched());

        // 余额调节表：银行侧 100985 + 0 - 0；企业侧 1000 + 0 - 15
        let rec = reconcile(&db, p, "100201").unwrap();
        assert_eq!(rec.bank_balance, Money::parse("100985").unwrap());
        assert_eq!(rec.book_balance, Money::parse("1000").unwrap());
        assert_eq!(rec.bank_only_out.len(), 1); // 银行已付企业未记：手续费 15
        assert_eq!(rec.bank_adjusted, Money::parse("100985").unwrap());
        assert_eq!(rec.book_adjusted, Money::parse("985").unwrap());
        assert!(!rec.balanced()); // 还没记手续费，自然不平
        assert_eq!(rec.diff(), Money::parse("100000").unwrap());

        let _ = vid;
    }

    #[test]
    fn import_single_amount_column() {
        let db = tmpdb("single");
        let p = Period::new(2026, 1).unwrap();
        let csv = "2026-01-06,收到货款,SN001,1000.00,101000.00\n\
                   2026-01-20,支付手续费,,-15.00,100985.00\n";
        let (n, _) = import_csv(&db, p, "100201", csv).unwrap();
        assert_eq!(n, 2);
        let rows = list(&db, p, "100201").unwrap();
        assert_eq!(rows[0].debit, Money::parse("1000").unwrap());
        assert_eq!(rows[1].credit, Money::parse("15").unwrap());
    }
}
