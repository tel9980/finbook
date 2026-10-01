//! 往来核销与账龄分析
//!
//! 核销是"单据级"的：一张应收单可以分多次收款核销，也可以一笔款核销多张单。
//! 所以这里存的是**分录 ↔ 分录 + 金额**，而不是"客户整体结清"。

use chrono::NaiveDate;
use fincore::engine::aging::{AgingBucket, AgingItem, AgingLine};
use fincore::{Money, Period};
use rusqlite::{Connection, OptionalExtension};

use crate::{Db, DbResult};

/// 一条核销记录
#[derive(Clone, Debug)]
pub struct SettleRecord {
    pub id: i64,
    pub period: Period,
    pub account_code: String,
    pub aux_key: String,
    /// 被核销的分录（原单据）
    pub from_entry: i64,
    /// 核销方分录（收款 / 付款）
    pub to_entry: i64,
    pub amount: Money,
    pub settled_by: String,
    pub settled_at: String,
}

fn map_rec(r: &rusqlite::Row) -> rusqlite::Result<SettleRecord> {
    Ok(SettleRecord {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        account_code: r.get(2)?,
        aux_key: r.get(3)?,
        from_entry: r.get(4)?,
        to_entry: r.get(5)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(6)?),
        settled_by: r.get(7)?,
        settled_at: r.get(8)?,
    })
}

const COLS: &str = "id,period,account_code,aux_key,from_entry,to_entry,amount,settled_by,settled_at";

/// 某科目（含下级）的全部核销记录
///
/// 口径与 [`open_entries`]、[`aging`] 一致：父级科目能查到下级科目的记录，
/// 否则界面默认按 1122 查询时会出现"未核销已清、记录却为空"的矛盾。
pub fn list(db: &Db, account: &str) -> DbResult<Vec<SettleRecord>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {COLS} FROM settle_record WHERE (account_code=?1 OR account_code LIKE ?1||'%') ORDER BY period, id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![account], map_rec)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 涉及某分录的全部核销
pub fn list_for_entry(db: &Db, entry_id: i64) -> DbResult<Vec<SettleRecord>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {COLS} FROM settle_record WHERE from_entry=?1 OR to_entry=?1 ORDER BY id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![entry_id], map_rec)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 某分录已被核销掉的金额
///
/// 收 `&Connection` 而非 `&Db`：核销要在同一事务里「读未核销额 → 校验 → 写回」，
/// 需要能借给事务句柄（`Transaction` 会 Deref 到 `Connection`）。
pub fn settled_of(conn: &Connection, entry_id: i64) -> DbResult<Money> {
    // 金额在库里是 TEXT，SUM 会走浮点，精度不可控；这里取行在 Rust 侧用定点累加
    let mut st = conn.prepare(
        "SELECT amount FROM settle_record WHERE from_entry=?1 OR to_entry=?1",
    )?;
    let rows = st
        .query_map(rusqlite::params![entry_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut sum = Money::ZERO;
    for s in rows {
        sum += Money::parse_or_zero(&s);
    }
    Ok(sum)
}

/// 批量取多个分录的已核销额（避免 N+1 查询）
pub fn settled_map(db: &Db, entry_ids: &[i64]) -> DbResult<std::collections::HashMap<i64, Money>> {
    let mut out = std::collections::HashMap::new();
    if entry_ids.is_empty() {
        return Ok(out);
    }
    let ph = entry_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    // 同样不能用 SUM()：TEXT 列求和会走 REAL，分位精度不可控
    let sql = format!(
        "SELECT k, amount FROM (
            SELECT from_entry AS k, id, amount FROM settle_record WHERE from_entry IN ({ph})
            UNION ALL
            SELECT to_entry AS k, id, amount FROM settle_record WHERE to_entry IN ({ph})
         ) ORDER BY k"
    );
    let mut st = db.conn().prepare(&sql)?;
    let ids2: Vec<&dyn rusqlite::ToSql> = entry_ids
        .iter()
        .flat_map(|i| [i as &dyn rusqlite::ToSql, i as &dyn rusqlite::ToSql])
        .collect();
    let rows = st
        .query_map(ids2.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, Money::parse_or_zero(&r.get::<_, String>(1)?)))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (k, v) in rows {
        *out.entry(k).or_insert(Money::ZERO) += v;
    }
    Ok(out)
}

fn settled_map_stub(db: &Db, entry_ids: &[i64]) -> DbResult<std::collections::HashMap<i64, Money>> {
    settled_map(db, entry_ids)
}

/// 手工核销。会自动校验：两边分录同科目、同往来单位、金额不超过各自未核销额。
pub fn settle(
    db: &Db,
    from_entry: i64,
    to_entry: i64,
    amount: Money,
    who: &str,
) -> DbResult<i64> {
    // 「读未核销额 → 校验 → 累加写回」必须整体原子。BEGIN IMMEDIATE 在开头就取得
    // 写锁，期间不会有别的写事务插进来；否则两笔并发核销会各自通过超额校验，
    // 并把对方的累计额覆盖掉（唯一索引只保证行唯一，管不住金额）。
    let tx = db.write_tx()?;
    let id = settle_in_tx(&tx, from_entry, to_entry, amount, who)?;
    tx.commit()?;
    Ok(id)
}

/// 同事务版核销：供收付款单在建单事务内复用。校验与 [`settle`] 完全一致，**不 commit**。
pub fn settle_in_tx(
    tx: &rusqlite::Transaction,
    from_entry: i64,
    to_entry: i64,
    amount: Money,
    who: &str,
) -> DbResult<i64> {
    if from_entry == to_entry {
        return Err(fincore::FinError::msg("不能把分录核销到自己身上").into());
    }
    if amount <= Money::ZERO {
        return Err(fincore::FinError::msg("核销金额必须大于零").into());
    }
    let f = match entry_of(tx, from_entry)? {
        Some(e) => e,
        None => return Err(fincore::FinError::not_found("被核销分录").into()),
    };
    let t = match entry_of(tx, to_entry)? {
        Some(e) => e,
        None => return Err(fincore::FinError::not_found("核销方分录").into()),
    };
    if f.account_code != t.account_code {
        return Err(fincore::FinError::msg(format!(
            "两条分录不在同一科目（{} vs {}），不能核销",
            f.account_code, t.account_code
        ))
        .into());
    }
    if f.aux_key != t.aux_key {
        return Err(fincore::FinError::msg(
            "两条分录的往来单位不一致，不能核销",
        )
        .into());
    }
    // 检查超额
    let open_f = f.signed().abs() - settled_of(tx, from_entry)?;
    let open_t = t.signed().abs() - settled_of(tx, to_entry)?;
    if amount > open_f {
        return Err(fincore::FinError::msg(format!(
            "核销金额 {amount} 超过被核销方未核销额 {open_f}"
        ))
        .into());
    }
    if amount > open_t {
        return Err(fincore::FinError::msg(format!(
            "核销金额 {amount} 超过核销方未核销额 {open_t}"
        ))
        .into());
    }

    // 读取现有累计核销额（TEXT 列，避免在 SQL 侧做字符串加法）
    let existing = tx
        .query_row(
            "SELECT amount FROM settle_record WHERE from_entry=?1 AND to_entry=?2",
            rusqlite::params![from_entry, to_entry],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_default();
    let old_amount = Money::parse_or_zero(&existing);
    let new_amount = old_amount + amount;

    tx.execute(
        "INSERT INTO settle_record(period,account_code,aux_key,from_entry,to_entry,amount,
            settled_by,settled_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(from_entry,to_entry) DO UPDATE SET
             amount=excluded.amount, settled_by=excluded.settled_by, settled_at=excluded.settled_at",
        rusqlite::params![
            f.period.ymm(),
            f.account_code,
            f.aux_key,
            from_entry,
            to_entry,
            crate::money_param(new_amount), // 无千分位的定点数，保证后续计算正确
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    let id = tx.last_insert_rowid();
    Ok(id)
}

pub fn unsettle(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM settle_record WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 清除某分录的全部核销（删改凭证时用）
pub fn unsettle_entry(db: &Db, entry_id: i64) -> DbResult<usize> {
    Ok(db.conn().execute(
        "DELETE FROM settle_record WHERE from_entry=?1 OR to_entry=?1",
        rusqlite::params![entry_id],
    )?)
}

// ---------------- 往来单据 ----------------

/// 一条往来分录（含已核销额）
#[derive(Clone, Debug)]
pub struct OpenEntry {
    pub entry_id: i64,
    pub voucher_id: i64,
    pub period: Period,
    pub date: NaiveDate,
    pub word: String,
    pub no: i32,
    pub line: i32,
    pub summary: String,
    pub account_code: String,
    pub aux_key: String,
    pub settle_no: String,
    pub debit: Money,
    pub credit: Money,
    pub settled: Money,
}

impl OpenEntry {
    /// 带符号金额（借方为正）
    pub fn signed(&self) -> Money {
        self.debit - self.credit
    }
    /// 未核销额（带符号）
    pub fn open(&self) -> Money {
        let o = self.signed().abs() - self.settled;
        if o < Money::parse("0.005").unwrap() {
            Money::ZERO
        } else {
            o
        }
    }
    pub fn is_open(&self) -> bool {
        !self.open().is_zero()
    }
    /// 方向标签
    pub fn dir_label(&self) -> &'static str {
        if self.debit > Money::ZERO {
            "借"
        } else {
            "贷"
        }
    }
}

fn entry_of(conn: &Connection, entry_id: i64) -> DbResult<Option<OpenEntry>> {
    conn.query_row(
            "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                    e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit
             FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
             WHERE e.id=?1",
            rusqlite::params![entry_id],
            |r| {
                let d: String = r.get(3)?;
                Ok(OpenEntry {
                    entry_id: r.get(0)?,
                    voucher_id: r.get(1)?,
                    period: Period::from_ymm(r.get(2)?),
                    date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                    word: r.get(4)?,
                    no: r.get(5)?,
                    line: r.get(6)?,
                    summary: r.get(7)?,
                    account_code: r.get(8)?,
                    aux_key: r.get(9)?,
                    settle_no: r.get(10)?,
                    debit: Money::parse_or_zero(&r.get::<_, String>(11)?),
                    credit: Money::parse_or_zero(&r.get::<_, String>(12)?),
                    settled: Money::ZERO,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

/// 取某科目（含下级）截至某日的全部已记账往来分录，并填充已核销额
pub fn open_entries(
    db: &Db,
    account: &str,
    upto: Period,
    include_all: bool,
) -> DbResult<Vec<OpenEntry>> {
    let mut st = db.conn().prepare(
        "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
         WHERE (e.account_code = ?1 OR e.account_code LIKE ?1||'%')
           AND v.period <= ?2 AND v.status != 'void'
           AND (e.debit <> '0' OR e.credit <> '0')
         ORDER BY v.date, v.no, e.line",
    )?;
    let mut rows = st
        .query_map(rusqlite::params![account, upto.ymm()], |r| {
            let d: String = r.get(3)?;
            Ok(OpenEntry {
                entry_id: r.get(0)?,
                voucher_id: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                word: r.get(4)?,
                no: r.get(5)?,
                line: r.get(6)?,
                summary: r.get(7)?,
                account_code: r.get(8)?,
                aux_key: r.get(9)?,
                settle_no: r.get(10)?,
                debit: Money::parse_or_zero(&r.get::<_, String>(11)?),
                credit: Money::parse_or_zero(&r.get::<_, String>(12)?),
                settled: Money::ZERO,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let ids: Vec<i64> = rows.iter().map(|r| r.entry_id).collect();
    let sm = settled_map_stub(db, &ids)?;
    for r in rows.iter_mut() {
        r.settled = sm.get(&r.entry_id).copied().unwrap_or(Money::ZERO);
    }
    if !include_all {
        rows.retain(|r| r.is_open());
    }
    Ok(rows)
}

// ---------------- 自动核销 ----------------

/// 自动核销结果
#[derive(Clone, Debug, Default)]
pub struct AutoSettleResult {
    pub pairs: usize,
    pub amount: Money,
    /// 同金额精确匹配的笔数
    pub exact: usize,
    /// 尾数清零（余额小于阈值直接抹平）的笔数
    pub written_off: usize,
}

/// 自动核销
///
/// 策略（保守优先，宁可少勾也不错勾）：
/// 1. 同一往来单位内，先找**金额完全相等的**一借一贷配对（最可靠）
/// 2. 剩余部分尝试用一笔收款逐条吃掉最早的单据（FIFO），吃掉后余额小于阈值则抹平
/// 3. 匹配不上的全部留空，交给手工
pub fn auto_settle(
    db: &Db,
    account: &str,
    upto: Period,
    tolerance: Money,
    who: &str,
) -> DbResult<AutoSettleResult> {
    let mut res = AutoSettleResult::default();
    let all = open_entries(db, account, upto, false)?;

    // 按往来单位分组
    let mut groups: std::collections::BTreeMap<String, Vec<OpenEntry>> =
        std::collections::BTreeMap::new();
    for e in all {
        groups.entry(e.aux_key.clone()).or_default().push(e);
    }

    for (_key, mut g) in groups {
        // 第一轮：精确等额配对
        let mut i = 0usize;
        while i < g.len() {
            if g[i].open().is_zero() {
                i += 1;
                continue;
            }
            let dir_pos = g[i].debit > Money::ZERO;
            let amt = g[i].open();
            let hit = g
                .iter()
                .position(|x| {
                    x.entry_id != g[i].entry_id
                        && (x.debit > Money::ZERO) != dir_pos
                        && x.open() == amt
                        && !x.open().is_zero()
                });
            if let Some(j) = hit {
                let (from, to) = if dir_pos { (g[i].entry_id, g[j].entry_id) } else { (g[j].entry_id, g[i].entry_id) };
                let id = settle(db, from, to, amt, who)?;
                let _ = id;
                res.pairs += 1;
                res.exact += 1;
                res.amount += amt;
                // 重新读一次，保证 open() 是最新的
                g = open_entries_of(db, &g)?;
                i = 0;
                continue;
            }
            i += 1;
        }

        // 第二轮：FIFO 逐笔吃掉，余额小于容差则抹平
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 200 {
                break;
            }
            g = open_entries_of(db, &g)?;
            let debtors: Vec<&OpenEntry> = g
                .iter()
                .filter(|x| x.debit > Money::ZERO && x.is_open())
                .collect();
            let credits: Vec<&OpenEntry> = g
                .iter()
                .filter(|x| x.credit > Money::ZERO && x.is_open())
                .collect();
            if debtors.is_empty() || credits.is_empty() {
                break;
            }
            let d = debtors[0];
            let c = credits[0];
            let amt = d.open().min(c.open());
            if amt.is_zero() {
                break;
            }
            let id = settle(db, d.entry_id, c.entry_id, amt, who)?;
            let _ = id;
            res.pairs += 1;
            res.amount += amt;
            // 吃掉后若剩余小于容差，直接抹平
            let d_left = d.open() - amt;
            let c_left = c.open() - amt;
            for (left, id) in [(d_left, d.entry_id), (c_left, c.entry_id)] {
                if !left.is_zero() && left <= tolerance {
                    // 找反向未结清分录把尾数吃掉
                    if let Some(other) = g.iter().find(|x| {
                        x.entry_id != id && x.is_open() && (x.debit > Money::ZERO) != (d.debit > Money::ZERO)
                    }) {
                        let _ = settle(db, id, other.entry_id, left, who);
                        res.pairs += 1;
                        res.written_off += 1;
                        res.amount += left;
                    }
                }
            }
        }
    }
    Ok(res)
}

/// 用当前库里的数据刷新一批分录的已核销额（保留原顺序与范围）
fn open_entries_of(db: &Db, old: &[OpenEntry]) -> DbResult<Vec<OpenEntry>> {
    let ids: Vec<i64> = old.iter().map(|o| o.entry_id).collect();
    let sm = settled_map(db, &ids)?;
    let mut out = Vec::with_capacity(old.len());
    let mut st = db.conn().prepare(
        "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id WHERE e.id=?1",
    )?;
    for id in ids {
        let mut e: OpenEntry = st.query_row(rusqlite::params![id], |r| {
            let d: String = r.get(3)?;
            Ok(OpenEntry {
                entry_id: r.get(0)?,
                voucher_id: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: NaiveDate::parse_from_str(&d, "%Y-%m-%d")
                    .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
                word: r.get(4)?,
                no: r.get(5)?,
                line: r.get(6)?,
                summary: r.get(7)?,
                account_code: r.get(8)?,
                aux_key: r.get(9)?,
                settle_no: r.get(10)?,
                debit: Money::parse_or_zero(&r.get::<_, String>(11)?),
                credit: Money::parse_or_zero(&r.get::<_, String>(12)?),
                settled: Money::ZERO,
            })
        })?;
        e.settled = sm.get(&id).copied().unwrap_or(Money::ZERO);
        out.push(e);
    }
    Ok(out)
}

// ---------------- 账龄 ----------------

/// 账龄分析
pub fn aging(
    db: &Db,
    account: &str,
    upto: Period,
    as_of: NaiveDate,
    buckets: &[AgingBucket],
) -> DbResult<Vec<AgingLine>> {
    let entries = open_entries(db, account, upto, false)?;
    let mut items: Vec<AgingItem> = entries
        .iter()
        .map(|e| AgingItem {
            key: e.aux_key.clone(),
            date: e.date,
            amount: e.signed(),
            settled: e.settled,
            doc_no: format!("{}-{}", e.word, e.no),
        })
        .collect();
    // 往来期初明细（迁移数据，影子行）：按科目方向取对应类型（1开头=应收 / 2开头=应付），
    // 单据日期期间不晚于 upto；**只进账龄展示、不参与 FIFO 核销**（核销 v2，避免伪 entry 外键）。
    let want = if account.starts_with('1') {
        Some("ar")
    } else if account.starts_with('2') {
        Some("ap")
    } else {
        None
    };
    if let Some(w) = want {
        for o in arap_opening_list(db, Some(w))? {
            let d = NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d")
                .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
            if Period::from_date(d).ymm() > upto.ymm() {
                continue;
            }
            items.push(AgingItem {
                key: o.party_code.clone(),
                date: d,
                amount: if o.kind == "ar" { o.amount } else { o.amount.negated() },
                settled: Money::ZERO,
                doc_no: format!(
                    "期初 {}",
                    if o.doc_no.is_empty() { format!("#{}", o.id) } else { o.doc_no.clone() }
                ),
            });
        }
    }
    Ok(fincore::engine::aging::analyze(&items, as_of, buckets)?)
}

// ---------------- 往来期初明细（按单据，平台迁移导入 v2） ----------------

/// 往来期初明细（应收/应付逐单据）。**影子挂账**：进账龄展示、不参与 FIFO 核销（v2）——
/// 金额与客商总额仍由「科目期初」负责，本表是迁移来的逐单欠款凭证信息。
#[derive(Clone, Debug, serde::Serialize)]
pub struct ArapOpening {
    pub id: i64,
    /// ar 应收 / ap 应付
    pub kind: String,
    pub party_code: String,
    pub party_name: String,
    pub doc_no: String,
    pub doc_date: String,
    pub amount: Money,
    pub memo: String,
    pub created_by: String,
}

const ARO_COLS: &str = "id,kind,party_code,party_name,doc_no,doc_date,amount,memo,created_by";

fn map_arap_opening(r: &rusqlite::Row) -> rusqlite::Result<ArapOpening> {
    Ok(ArapOpening {
        id: r.get(0)?,
        kind: r.get(1)?,
        party_code: r.get(2)?,
        party_name: r.get(3)?,
        doc_no: r.get(4)?,
        doc_date: r.get(5)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(6)?),
        memo: r.get(7)?,
        created_by: r.get(8)?,
    })
}

pub fn arap_opening_list(db: &Db, kind: Option<&str>) -> DbResult<Vec<ArapOpening>> {
    let mut out = Vec::new();
    match kind {
        Some(k) => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {ARO_COLS} FROM arap_opening WHERE kind=?1 ORDER BY doc_date, id"
            ))?;
            out = st
                .query_map([k], map_arap_opening)?
                .collect::<Result<Vec<_>, _>>()?;
        }
        None => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {ARO_COLS} FROM arap_opening ORDER BY kind, doc_date, id"
            ))?;
            out = st
                .query_map([], map_arap_opening)?
                .collect::<Result<Vec<_>, _>>()?;
        }
    }
    Ok(out)
}

/// 是否已有同 kind+客商+单据号（幂等重导跳过）
pub fn arap_opening_exists(db: &Db, kind: &str, party: &str, doc_no: &str) -> DbResult<bool> {
    let n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM arap_opening WHERE kind=?1 AND party_code=?2 AND doc_no=?3",
        rusqlite::params![kind, party, doc_no],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn arap_opening_insert(db: &Db, o: &ArapOpening, who: &str) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO arap_opening(kind,party_code,party_name,doc_no,doc_date,amount,memo,created_by,created_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        rusqlite::params![
            o.kind,
            o.party_code,
            o.party_name,
            o.doc_no,
            o.doc_date,
            crate::exact_param(o.amount),
            o.memo,
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn arap_opening_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM arap_opening WHERE id=?1", [id])?;
    Ok(())
}

// ---------------- 催款单 / 对账函（应收催收闭环） ----------------

fn dunning_now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 催款单明细行（快照：凭证/期初单据 + 未核销额）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DunningItem {
    pub doc_no: String,
    pub date: String,
    pub summary: String,
    pub amount: Money,
}

/// 催款单 / 对账函
#[derive(Clone, Debug, serde::Serialize)]
pub struct Dunning {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    /// ar 催款 / ap 对账函
    pub kind: String,
    pub account: String,
    pub party_code: String,
    pub party_name: String,
    pub amount: Money,
    pub item_count: i64,
    /// draft/sent/settled/cancelled
    pub status: String,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
    pub sent_at: String,
    pub detail: Vec<DunningItem>,
}

fn map_dunning(r: &rusqlite::Row) -> rusqlite::Result<Dunning> {
    let date: String = r.get(3)?;
    let detail_json: String = r.get(14)?;
    Ok(Dunning {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()),
        kind: r.get(4)?,
        account: r.get(5)?,
        party_code: r.get(6)?,
        party_name: r.get(7)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
        item_count: r.get(9)?,
        status: r.get(10)?,
        memo: r.get(11)?,
        created_by: r.get(12)?,
        created_at: r.get(13)?,
        sent_at: r.get(15)?,
        detail: serde_json::from_str(&detail_json).unwrap_or_default(),
    })
}

const DUN_COLS: &str = "id,no,period,date,kind,account,party_code,party_name,amount,item_count,status,memo,created_by,created_at,detail_json,sent_at";

/// 催款单号：CK + 期间 + 3 位序号（按期间递增，撞号顺延）
pub fn dunning_next_no(db: &Db, period: Period) -> DbResult<String> {
    let mut n: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM dunning WHERE period=?1",
        rusqlite::params![period.ymm()],
        |r| r.get(0),
    )?;
    loop {
        n += 1;
        let no = format!("CK{}{:03}", period.ymm(), n);
        let exists: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM dunning WHERE no=?1",
            rusqlite::params![no],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Ok(no);
        }
    }
}

pub fn dunning_list(db: &Db, kind: Option<&str>) -> DbResult<Vec<Dunning>> {
    let mut out = Vec::new();
    match kind.filter(|k| !k.trim().is_empty()) {
        Some(k) => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {DUN_COLS} FROM dunning WHERE kind=?1 ORDER BY date DESC, id DESC"
            ))?;
            let rows = st.query_map(rusqlite::params![k], map_dunning)?;
            for r in rows {
                out.push(r?);
            }
        }
        None => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {DUN_COLS} FROM dunning ORDER BY date DESC, id DESC"
            ))?;
            let rows = st.query_map([], map_dunning)?;
            for r in rows {
                out.push(r?);
            }
        }
    }
    Ok(out)
}

pub fn dunning_get(db: &Db, id: i64) -> DbResult<Option<Dunning>> {
    db.conn()
        .query_row(
            &format!("SELECT {DUN_COLS} FROM dunning WHERE id=?1"),
            rusqlite::params![id],
            map_dunning,
        )
        .optional()
        .map_err(Into::into)
}

/// 生成催款单（快照）：按客商汇总所选往来科目下**未核销分录** + 往来期初影子挂账；
/// 无任何欠款行 → 拒绝。金额与明细服务端计算，不信前端。
#[allow(clippy::too_many_arguments)]
pub fn dunning_create(
    db: &Db,
    kind: &str,
    account: &str,
    party_code: &str,
    party_name: &str,
    date: NaiveDate,
    memo: &str,
    who: &str,
) -> DbResult<Dunning> {
    let kind = match kind.trim() {
        "ar" => "ar",
        "ap" => "ap",
        _ => return Err(fincore::FinError::msg("类型只能是 ar（催款）或 ap（对账函）").into()),
    };
    let party = party_code.trim();
    if party.is_empty() {
        return Err(fincore::FinError::msg("客商编码不能为空").into());
    }
    let account = if account.trim().is_empty() {
        if kind == "ar" { "1122" } else { "2202" }
    } else {
        account.trim()
    };
    let upto = Period::from_date(date);
    let mut items: Vec<DunningItem> = Vec::new();
    let mut amount = Money::ZERO;
    for e in open_entries(db, account, upto, false)? {
        let aux = fincore::voucher::AuxRef::from_key(&e.aux_key);
        let p = if kind == "ar" {
            aux.customer.clone()
        } else {
            aux.supplier.clone()
        };
        if p.as_deref() != Some(party) {
            continue;
        }
        let amt = e.open();
        if amt.is_zero() {
            continue;
        }
        amount = amount + amt;
        items.push(DunningItem {
            doc_no: format!("{}-{}", e.word, e.no),
            date: e.date.format("%Y-%m-%d").to_string(),
            summary: e.summary.clone(),
            amount: amt,
        });
    }
    for o in arap_opening_list(db, Some(kind))? {
        if o.party_code != party {
            continue;
        }
        let d = NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
        if Period::from_date(d).ymm() > upto.ymm() {
            continue;
        }
        amount = amount + o.amount;
        items.push(DunningItem {
            doc_no: format!(
                "期初 {}",
                if o.doc_no.is_empty() {
                    format!("#{}", o.id)
                } else {
                    o.doc_no.clone()
                }
            ),
            date: o.doc_date.clone(),
            summary: "往来期初".to_string(),
            amount: o.amount,
        });
    }
    if items.is_empty() {
        return Err(
            fincore::FinError::msg("该客商在所选科目下无未核销单据（或期初），无需催款").into(),
        );
    }
    let no = dunning_next_no(db, upto)?;
    let detail = serde_json::to_string(&items)
        .map_err(|e| fincore::FinError::msg(format!("明细序列化失败：{e}")))?;
    db.conn().execute(
        "INSERT INTO dunning(no,period,date,kind,account,party_code,party_name,amount,item_count,
         status,memo,created_by,created_at,sent_at,detail_json)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'draft',?10,?11,?12,'',?13)",
        rusqlite::params![
            no,
            upto.ymm(),
            date.format("%Y-%m-%d").to_string(),
            kind,
            account,
            party,
            party_name.trim(),
            crate::money_param(amount),
            items.len() as i64,
            memo,
            who,
            dunning_now(),
            detail
        ],
    )?;
    let id = db.conn().last_insert_rowid();
    dunning_get(db, id)?.ok_or_else(|| fincore::FinError::msg("催款单创建失败").into())
}

/// 状态流转：draft → sent/settled/cancelled；sent → settled/cancelled（条件更新防并发）
pub fn dunning_status(db: &Db, id: i64, status: &str) -> DbResult<()> {
    let cur = dunning_get(db, id)?.ok_or_else(|| fincore::FinError::not_found("催款单"))?;
    let allowed = match status {
        "sent" => cur.status == "draft",
        "settled" => cur.status == "draft" || cur.status == "sent",
        "cancelled" => cur.status == "draft" || cur.status == "sent",
        _ => false,
    };
    if !allowed {
        return Err(fincore::FinError::msg(format!(
            "状态不能从 {} 变更为 {}",
            cur.status, status
        ))
        .into());
    }
    let ts = dunning_now();
    let n = db.conn().execute(
        "UPDATE dunning SET status=?2,
         sent_at=CASE WHEN ?2='sent' THEN ?3 ELSE sent_at END
         WHERE id=?1 AND status=?4",
        rusqlite::params![id, status, ts, cur.status],
    )?;
    if n == 0 {
        return Err(fincore::FinError::msg("催款单已被处理，请刷新").into());
    }
    Ok(())
}

/// 计提坏账准备：按应收（1122/1221）账龄与默认坏账比例计算**目标余额**，
/// 与账上 1231 现有余额比对，只按差额计提/冲回，辅助核算保留往来对象。
///
/// 这样同期间重复执行、跨期data变化后的再执行都是幂等的：
/// 差额为 0 时返回 `Ok(None)`，不会重复全额计提；应提额减少时自动冲回。
pub fn bad_debt_provision_voucher(
    db: &Db,
    period: Period,
    date: NaiveDate,
    who: &str,
) -> DbResult<Option<i64>> {
    use fincore::engine::aging::{buckets_by_year, default_bad_debt_rates};
    let buckets = buckets_by_year();
    let rates = default_bad_debt_rates();

    let mut lines = Vec::new();
    for acct in ["1122", "1221"] {
        lines.extend(aging(db, acct, period, date, &buckets)?);
    }

    // 目标余额按往来对象汇总（同一对象可能同时有应收/其他应收）
    let mut targets: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    for l in &lines {
        let prov: Money = l
            .amounts
            .iter()
            .zip(rates.iter())
            .map(|(a, r)| (*a * *r).round2())
            .sum();
        if prov.is_zero() {
            continue;
        }
        *targets.entry(l.key.clone()).or_insert(Money::ZERO) += prov;
    }

    // 差额 = 目标 − 现有（口径与余额表一致：非作废凭证，含未记账草稿）
    let tx = db.write_tx()?;
    let mut existing: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    {
        let mut st = tx.prepare(
            "SELECT e.aux_key, e.credit, e.debit
             FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
             WHERE e.account_code='1231' AND e.period <= ?1 AND v.status <> 'void'",
        )?;
        let mut rows = st.query(rusqlite::params![period.ymm()])?;
        while let Some(r) = rows.next()? {
            let key: String = r.get(0)?;
            let c = Money::parse_or_zero(&r.get::<_, String>(1)?);
            let d = Money::parse_or_zero(&r.get::<_, String>(2)?);
            *existing.entry(key).or_insert(Money::ZERO) += c - d;
        }
    }

    let mut keys: Vec<String> = targets.keys().chain(existing.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    let mut deltas: Vec<(String, Money)> = Vec::new();
    let (mut inc, mut dec) = (Money::ZERO, Money::ZERO);
    for k in keys {
        let t = targets.get(&k).copied().unwrap_or(Money::ZERO);
        let e = existing.get(&k).copied().unwrap_or(Money::ZERO);
        let d = t - e;
        if d.is_positive() {
            inc += d;
            deltas.push((k, d));
        } else if d.is_negative() {
            dec += d.abs();
            deltas.push((k, d));
        }
    }
    if inc.is_zero() && dec.is_zero() {
        return Ok(None); // 已按目标计提，无差额可调
    }

    let no = crate::vouchers::next_no_of(&tx, period, "记")?;
    let mut v = fincore::Voucher::new(period, date, "记", no);
    v.prepared_by = who.to_string();
    v.source = fincore::voucher::VoucherSource::Business;
    v.memo = if dec > inc { "冲回坏账准备".to_string() } else { "计提坏账准备".to_string() };
    let mut line_no = 1;
    for (key, d) in &deltas {
        let aux = fincore::voucher::AuxRef::from_key(key);
        if d.is_positive() {
            v.push_entry(fincore::voucher::Entry {
                credit: *d,
                aux,
                ..fincore::voucher::Entry::new(line_no, "1231", "计提坏账准备")
            });
        } else {
            v.push_entry(fincore::voucher::Entry {
                debit: d.abs(),
                aux,
                ..fincore::voucher::Entry::new(line_no, "1231", "冲回坏账准备")
            });
        }
        line_no += 1;
    }
    // 净额腿：增提走借方 6701，冲回走贷方 6701，保证借贷恒等
    if inc > dec {
        v.push_entry(fincore::voucher::Entry {
            debit: inc - dec,
            ..fincore::voucher::Entry::new(line_no, "6701", "计提坏账准备")
        });
    } else if dec > inc {
        v.push_entry(fincore::voucher::Entry {
            credit: dec - inc,
            ..fincore::voucher::Entry::new(line_no, "6701", "冲回坏账准备")
        });
    }
    v.renumber();
    let id = crate::vouchers::save_in(&tx, &mut v)?;
    tx.commit()?;
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::voucher::{AuxRef, Entry, Voucher};

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_settle_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }

    /// 建一张已记账的往来凭证
    fn ar_voucher(
        db: &Db,
        period: Period,
        date: NaiveDate,
        no: i32,
        cust: &str,
        amount: &str,
        dir_debit: bool,
        counterpart: &str,
    ) -> (i64, i64) {
        let mut v = Voucher::new(Period::from_date(date), date, "记", no);
        let a = Money::parse(amount).unwrap();
        let e1 = Entry {
            debit: if dir_debit { a } else { Money::ZERO },
            credit: if dir_debit { Money::ZERO } else { a },
            aux: AuxRef {
                customer: Some(cust.into()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "往来")
        };
        let e2 = Entry {
            debit: if dir_debit { Money::ZERO } else { a },
            credit: if dir_debit { a } else { Money::ZERO },
            aux: if counterpart == "100201" {
                AuxRef {
                    bank: Some("BANK01".into()),
                    ..Default::default()
                }
            } else {
                AuxRef::default()
            },
            ..Entry::new(2, counterpart, "往来")
        };
        let _ = period;
        v.push_entry(e1);
        v.push_entry(e2);
        let vid = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, vid, "poster").unwrap();
        let entries = crate::vouchers::entries_of(db, vid).unwrap();
        (vid, entries[0].id)
    }

    #[test]
    fn manual_settle_flow() {
        let db = tmpdb("manual");
        let p = Period::new(2026, 1).unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
        let (_, e_ar) = ar_voucher(&db, p, d1, 1, "C01", "1000", true, "600101");
        let (_, e_cash) = ar_voucher(&db, p, d2, 2, "C01", "600", false, "100201");

        settle(&db, e_ar, e_cash, Money::parse("600").unwrap(), "u1").unwrap();
        assert_eq!(settled_of(db.conn(), e_ar).unwrap(), Money::parse("600").unwrap());

        // 超额核销要拦
        assert!(settle(&db, e_ar, e_cash, Money::parse("500").unwrap(), "u1").is_err());
        // 自己核销自己要拦
        assert!(settle(&db, e_ar, e_ar, Money::parse("10").unwrap(), "u1").is_err());

        let opens = open_entries(&db, "1122", p, false).unwrap();
        assert_eq!(opens.len(), 1); // 收款那条已被核完
        assert_eq!(opens[0].open(), Money::parse("400").unwrap());

        // 取消核销后恢复
        let recs = list_for_entry(&db, e_ar).unwrap();
        unsettle(&db, recs[0].id).unwrap();
        assert_eq!(settled_of(db.conn(), e_ar).unwrap(), Money::ZERO);
    }

    #[test]
    fn entry_rewrite_clears_dangling_refs() {
        let db = tmpdb("dangling");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let (vid, e_ar) = ar_voucher(&db, p, d, 1, "C01", "1000", true, "600101");
        // 收款凭证（贷 112201），提供同科目的核销方分录
        let (_, e_pay) = ar_voucher(&db, p, d, 2, "C01", "600", false, "100201");

        settle(&db, e_ar, e_pay, Money::parse("600").unwrap(), "u1").unwrap();
        // 银行勾对也挂在将被重写的这张凭证的分录上
        db.conn()
            .execute(
                "INSERT INTO bank_statement(period,account_code,biz_date,summary,settle_no,debit,credit,balance,entry_id,matched_at,matched_by)
                 VALUES(?1,'112201','2026-01-05','测试','','1000','0','0',?2,'now','u')",
                rusqlite::params![p.ymm(), e_ar],
            )
            .unwrap();
        assert_eq!(list_for_entry(&db, e_ar).unwrap().len(), 1);

        // 反记账后重写凭证（save 会整表 DELETE 旧分录再插入）：
        // 核销记录应级联删除、银行勾对应置空，不能留下指向已删分录的悬空引用
        crate::vouchers::unpost(&db, vid).unwrap();
        let mut v = crate::vouchers::get(&db, vid).unwrap().unwrap();
        v.memo = "改过摘要".to_string();
        crate::vouchers::save(&db, &mut v).unwrap();

        assert_eq!(
            list_for_entry(&db, e_ar).unwrap().len(),
            0,
            "重写分录后核销记录不应悬空"
        );
        let linked: Option<i64> = db
            .conn()
            .query_row(
                "SELECT entry_id FROM bank_statement WHERE period=?1 AND account_code='112201'",
                rusqlite::params![p.ymm()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(linked.is_none(), "银行勾对应被置空");
    }

    #[test]
    fn cross_account_rejected() {
        let db = tmpdb("cross");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let (_, e1) = ar_voucher(&db, p, d, 1, "C01", "100", true, "600101");
        let (_, e2) = ar_voucher(&db, p, d, 2, "C02", "100", false, "100201");
        // 往来单位不同
        assert!(settle(&db, e1, e2, Money::parse("100").unwrap(), "u").is_err());
    }

    #[test]
    fn auto_settle_exact_then_fifo() {
        let db = tmpdb("auto");
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        // 两笔应收各 500，一笔收款 800
        ar_voucher(&db, p, d, 1, "C01", "500", true, "600101");
        ar_voucher(&db, p, d, 2, "C01", "500", true, "600101");
        ar_voucher(&db, p, d, 3, "C01", "800", false, "100201");

        let res = auto_settle(&db, "1122", p, Money::parse("0.01").unwrap(), "u").unwrap();
        assert!(res.pairs >= 2, "{res:?}");
        let opens = open_entries(&db, "1122", p, false).unwrap();
        let total: Money = opens.iter().map(|o| o.open()).sum();
        assert_eq!(total, Money::parse("200").unwrap());
    }

    #[test]
    fn aging_report() {
        let db = tmpdb("aging");
        let p = Period::new(2026, 3).unwrap();
        let d1 = NaiveDate::from_ymd_opt(2026, 3, 20).unwrap();
        let d2 = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
        ar_voucher(&db, p, d1, 1, "C01", "1000", true, "600101");
        ar_voucher(&db, p, d2, 2, "C01", "2000", true, "600101");

        let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        let b = fincore::engine::aging::buckets_by_days();
        let lines = aging(&db, "1122", p, as_of, &b).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].total, Money::parse("3000").unwrap());
        assert_eq!(lines[0].amounts[0], Money::parse("1000").unwrap()); // 11 天
        // 2025-06-01 → 2026-03-31 约 303 天，落在 181-365 档
        assert_eq!(lines[0].amounts[4], Money::parse("2000").unwrap());
    }

    #[test]
    fn bad_debt_provision_generates_voucher() {
        let db = tmpdb("baddebt");
        let p = Period::new(2026, 3).unwrap();
        // 一笔 3 年以上的老应收 10000，一笔 1 年以内 2000
        ar_voucher(&db, p, NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(), 1, "C01", "2000", true, "600101");
        ar_voucher(&db, p, NaiveDate::from_ymd_opt(2023, 1, 5).unwrap(), 2, "C01", "10000", true, "600101");

        let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        let id = bad_debt_provision_voucher(&db, p, as_of, "u1").unwrap().expect("应生成坏账准备凭证");
        let v = crate::vouchers::get(&db, id).unwrap().unwrap();
        // 借 资产减值损失 / 贷 坏账准备，借贷平衡
        assert!(v.balanced());
        assert!(v.entries.iter().any(|e| e.account_code == "6701"));
        assert!(v.entries.iter().any(|e| e.account_code == "1231"));
        // 3-4 年老账 10000 × 30% + 1 年内新账 2000 × 5% = 3000 + 100 = 3100
        let credit: Money = v.entries.iter().filter(|e| e.account_code == "1231").map(|e| e.credit).sum();
        assert_eq!(credit, Money::parse("3100").unwrap());
        assert_eq!(v.source, fincore::voucher::VoucherSource::Business);
    }

    #[test]
    fn bad_debt_provision_adjusts_target_and_reverses() {
        let db = tmpdb("baddebt_diff");
        let p1 = Period::new(2026, 3).unwrap();
        let (_, e_new) = ar_voucher(
            &db,
            p1,
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            1,
            "C01",
            "2000",
            true,
            "600101",
        );
        let (_, e_old) = ar_voucher(
            &db,
            p1,
            NaiveDate::from_ymd_opt(2023, 1, 5).unwrap(),
            2,
            "C01",
            "10000",
            true,
            "600101",
        );
        let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        bad_debt_provision_voucher(&db, p1, as_of, "u1").unwrap().expect("首次应计提");
        // 目标未变：重复执行差额为 0，不再生成
        assert!(
            bad_debt_provision_voucher(&db, p1, as_of, "u1").unwrap().is_none(),
            "目标不变时不应重复计提"
        );

        // 期后全部收回 → 应提额降为 0，应自动冲回
        let p2 = Period::new(2026, 4).unwrap();
        let (_, e_pay) = ar_voucher(
            &db,
            p2,
            NaiveDate::from_ymd_opt(2026, 4, 10).unwrap(),
            3,
            "C01",
            "12000",
            false,
            "100201",
        );
        settle(&db, e_new, e_pay, Money::parse("2000").unwrap(), "u1").unwrap();
        settle(&db, e_old, e_pay, Money::parse("10000").unwrap(), "u1").unwrap();

        let rev = bad_debt_provision_voucher(
            &db,
            p2,
            NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
            "u1",
        )
        .unwrap()
        .expect("应生成冲回凭证");
        let v = crate::vouchers::get(&db, rev).unwrap().unwrap();
        assert!(v.balanced());
        let debit: Money = v
            .entries
            .iter()
            .filter(|e| e.account_code == "1231")
            .map(|e| e.debit)
            .sum();
        let credit: Money = v
            .entries
            .iter()
            .filter(|e| e.account_code == "6701")
            .map(|e| e.credit)
            .sum();
        assert_eq!(debit, Money::parse("3100").unwrap());
        assert_eq!(credit, Money::parse("3100").unwrap());
        // 已到目标余额：再执行不再生成
        assert!(
            bad_debt_provision_voucher(
                &db,
                p2,
                NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
                "u1"
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn bad_debt_provision_skip_when_none() {
        let db = tmpdb("baddebt0");
        let p = Period::new(2026, 3).unwrap();
        let r = bad_debt_provision_voucher(&db, p, NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(), "u1").unwrap();
        assert!(r.is_none(), "无应收时不应生成凭证");
    }
}
