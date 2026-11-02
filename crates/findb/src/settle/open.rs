//! 往来单据：未清分录（OpenEntry）的读取与重新加载。
//!
//! 分册说明：本文件原属 `settle.rs` 单文件（3,437 行），按小节标题切开，
//! 函数体与注释一字未改；跨文件共用的内部函数标 `pub(super)`，对外 API 由
//! `settle/mod.rs` 统一 `pub use` 出去，外部调用路径 `findb::settle::xxx` 不变。

use super::*;

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
    /// 所属凭证是否已记账。
    ///
    /// 核销 / 账龄 / 催款只认已记账（全仓 H-3 口径），所以默认查询不会返回
    /// 未记账行，这个字段恒为 true。列表页勾了「含未记账」时才会出现 false 的行，
    /// 用来在前端标注「仅供查看、不可核销」。
    pub posted: bool,
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

pub(super) fn entry_of(conn: &Connection, entry_id: i64) -> DbResult<Option<OpenEntry>> {
    conn.query_row(
        "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                    e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit,
                    v.status
             FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
             WHERE e.id=?1",
        rusqlite::params![entry_id],
        |r| {
            let d: String = r.get(3)?;
            let status: String = r.get(13)?;
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
                posted: status == "posted",
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// 取某科目（含下级）截至某日的全部**已记账**往来分录，并填充已核销额。
///
/// 口径与全仓 H-3 定案一致：**只取已记账**。早先写的是 `status != 'void'`，
/// 把草稿/已审核未记账也算成未核销往来，于是自动核销、账龄、催款单都能对着
/// 一张还没入账的凭证做事——那张凭证随时可删，核销记录会凭空消失。
///
/// 核销、账龄、催款**一律走本函数**，不要用 [`open_entries_with`] 放宽。
pub fn open_entries(
    db: &Db,
    account: &str,
    upto: Period,
    include_all: bool,
) -> DbResult<Vec<OpenEntry>> {
    open_entries_with(db, account, upto, include_all, true, None)
}

/// 同 [`open_entries`]，但可显式要求**连未记账的一起返回**（`posted_only = false`）。
///
/// 只给往来核销**列表页的「含未记账」只读开关**用：让用户看得见待记账的往来，
/// 避免以为数据丢了。金蝶的单据驱动模型里，未审核单据同样能在列表里看到、
/// 只是不参与核销——这里复刻的就是那一层。返回行带 [`OpenEntry::posted`] = false，
/// 前端据此标注「仅供查看」；核销入口（自动核销 / 手工核销）不读这个函数，
/// 仍走 `open_entries`，所以未记账的行不可能被核销。
/// 同 [`open_entries_with`]，但可在 SQL 层面按辅助核算键过滤（如 `customer=C01`），
/// 避免把全量未核销分录取到 Rust 侧再 `filter`。`aux_key_filter` 为 `None` 时退化为
/// 原行为（不过滤辅助核算，供账龄等全量场景复用）。
pub fn open_entries_with(
    db: &Db,
    account: &str,
    upto: Period,
    include_all: bool,
    posted_only: bool,
    aux_key_filter: Option<&str>,
) -> DbResult<Vec<OpenEntry>> {
    // 已记账分支固定用字面量 'posted'（走索引），放宽分支才用参数
    let status_clause = if posted_only {
        "= 'posted'"
    } else {
        "!= 'void'"
    };
    // `?3 IS NULL OR ...`：aux_key_filter 为 None 时绑定 NULL，条件恒真，SQL 与
    // 原版一致；为 Some 时在 SQL 层面按 aux_key 做 LIKE 过滤，减少 Rust 侧扫描。
    let sql = format!(
        "SELECT e.id, v.id, v.period, v.date, v.word, v.no, e.line, e.summary,
                e.account_code, e.aux_key, COALESCE(e.settle_no,''), e.debit, e.credit,
                v.status
         FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
         WHERE (e.account_code = ?1 OR e.account_code LIKE ?1||'%')
           AND v.period <= ?2 AND v.status {status_clause}
           AND (e.debit <> '0' OR e.credit <> '0')
           AND (?3 IS NULL OR e.aux_key LIKE '%' || ?3 || '%')
         ORDER BY v.date, v.no, e.line"
    );
    let mut st = db.conn().prepare(&sql)?;
    let mut rows = st
        .query_map(
            rusqlite::params![account, upto.ymm(), aux_key_filter],
            |r| {
                let d: String = r.get(3)?;
                let status: String = r.get(13)?;
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
                    posted: status == "posted",
                })
            },
        )?
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
