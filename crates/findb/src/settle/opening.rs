//! 往来期初明细（按单据）：平台迁移导入 v2 用，期初欠款精确到单。
//!
//! v1.8.125 起期初明细**参与核销**（不再只是影子挂账）：`opening_settle` 记
//! 「期初单 ↔ 收付款分录」的核销额，账龄、客户余额、自动核销都按它扣减。
//!
//! 分册说明：本文件原属 `settle.rs` 单文件（3,437 行），按小节标题切开，
//! 函数体与注释一字未改；跨文件共用的内部函数标 `pub(super)`，对外 API 由
//! `settle/mod.rs` 统一 `pub use` 出去，外部调用路径 `findb::settle::xxx` 不变。

use super::*;

// ---------------- 往来期初明细（按单据，平台迁移导入 v2） ----------------

/// 往来期初明细（应收/应付逐单据）。
///
/// v1.8.125 起参与核销：核销额记在 `opening_settle`，账龄/客户余额/自动核销都扣减。
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
    let out = match kind {
        Some(k) => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {ARO_COLS} FROM arap_opening WHERE kind=?1 ORDER BY doc_date, id"
            ))?;
            let rows = st.query_map([k], map_arap_opening)?;
            rows.collect::<Result<Vec<_>, _>>()?
        }
        None => {
            let mut st = db.conn().prepare(&format!(
                "SELECT {ARO_COLS} FROM arap_opening ORDER BY kind, doc_date, id"
            ))?;
            let rows = st.query_map([], map_arap_opening)?;
            rows.collect::<Result<Vec<_>, _>>()?
        }
    };
    Ok(out)
}

/// 单行读取（修改前确认目标还在，顺便给 Web 层一个 404 的依据）
pub fn arap_opening_get(db: &Db, id: i64) -> DbResult<Option<ArapOpening>> {
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {ARO_COLS} FROM arap_opening WHERE id=?1"))?;
    let mut rows = st.query_map([id], map_arap_opening)?;
    Ok(rows.next().transpose()?)
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

/// 修改往来期初明细（导错可直接改，不必删掉重录）。
///
/// 校验口径与 CSV 导入（`crate::imports::import_arap_opening_rows`）保持一致：
/// kind 只允许 ar/ap、客商编码与单据号不能空、单据日期必须 YYYY-MM-DD、金额必须正数。
/// 另加一条导入没有的：改完之后同 kind+客商+单据号不能撞上**另一行**（导入按这个
/// 三元组幂等跳过，撞号会让下次重导被误判成「已经有了」而漏数据）。
/// `created_by` / `created_at` 是原始录入痕迹，故意不覆盖。
pub fn arap_opening_update(db: &Db, id: i64, o: &ArapOpening) -> DbResult<()> {
    let exists: i64 =
        db.conn()
            .query_row("SELECT COUNT(*) FROM arap_opening WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
    if exists == 0 {
        return Err(fincore::FinError::not_found("往来期初明细").into());
    }
    if o.kind != "ar" && o.kind != "ap" {
        return Err(fincore::FinError::validate("类型只能是应收 ar 或应付 ap").into());
    }
    if o.party_code.trim().is_empty() || o.doc_no.trim().is_empty() {
        return Err(fincore::FinError::validate("客商编码和单据号不能为空").into());
    }
    if chrono::NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d").is_err() {
        return Err(fincore::FinError::validate("单据日期须为 YYYY-MM-DD").into());
    }
    if !o.amount.is_positive() {
        return Err(
            fincore::FinError::validate("期初金额必须是正数（预收/预付请填科目期初）").into(),
        );
    }
    let dup: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM arap_opening WHERE kind=?1 AND party_code=?2 AND doc_no=?3 AND id<>?4",
        rusqlite::params![o.kind, o.party_code, o.doc_no, id],
        |r| r.get(0),
    )?;
    if dup > 0 {
        return Err(fincore::FinError::validate("同一客商下已有这张单据号，请换个单据号").into());
    }
    let n = db.conn().execute(
        "UPDATE arap_opening SET kind=?2,party_code=?3,party_name=?4,doc_no=?5,doc_date=?6,amount=?7,memo=?8 WHERE id=?1",
        rusqlite::params![
            id,
            o.kind,
            o.party_code,
            o.party_name,
            o.doc_no,
            o.doc_date,
            crate::exact_param(o.amount),
            o.memo
        ],
    )?;
    if n == 0 {
        return Err(fincore::FinError::not_found("往来期初明细").into());
    }
    Ok(())
}

// ---------------- 期初明细的核销（v1.8.125，schema v58） ----------------

/// 一条期初核销记录（期初单 ↔ 收付款分录）
#[derive(Clone, Debug)]
pub struct OpeningSettle {
    pub id: i64,
    pub period: Period,
    pub account_code: String,
    pub aux_key: String,
    pub opening_id: i64,
    pub to_entry: i64,
    pub amount: Money,
    pub settled_by: String,
    pub settled_at: String,
}

const OS_COLS: &str =
    "id,period,account_code,aux_key,opening_id,to_entry,amount,settled_by,settled_at";

fn map_os(r: &rusqlite::Row) -> rusqlite::Result<OpeningSettle> {
    Ok(OpeningSettle {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        account_code: r.get(2)?,
        aux_key: r.get(3)?,
        opening_id: r.get(4)?,
        to_entry: r.get(5)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(6)?),
        settled_by: r.get(7)?,
        settled_at: r.get(8)?,
    })
}

/// 期初明细的「已核销额」映射（批量，避免 N+1）。
///
/// 金额在库里是 TEXT，SUM 会走浮点，所以取行在 Rust 侧定点累加——与
/// [`settled_map`] 同一条规矩。
pub fn opening_settled_map(
    db: &Db,
    opening_ids: &[i64],
) -> DbResult<std::collections::HashMap<i64, Money>> {
    let mut out = std::collections::HashMap::new();
    if opening_ids.is_empty() {
        return Ok(out);
    }
    let ph = opening_ids
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT opening_id, amount FROM opening_settle WHERE opening_id IN ({ph})");
    let mut st = db.conn().prepare(&sql)?;
    let ids: Vec<&dyn rusqlite::ToSql> = opening_ids
        .iter()
        .map(|i| i as &dyn rusqlite::ToSql)
        .collect();
    let rows = st
        .query_map(ids.as_slice(), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                Money::parse_or_zero(&r.get::<_, String>(1)?),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (k, v) in rows {
        *out.entry(k).or_insert(Money::ZERO) += v;
    }
    Ok(out)
}

/// 单张期初单的已核销额（事务内用，收 `&Connection`）
pub fn opening_settled_of(conn: &Connection, opening_id: i64) -> DbResult<Money> {
    let mut st = conn.prepare("SELECT amount FROM opening_settle WHERE opening_id=?1")?;
    let rows = st
        .query_map(rusqlite::params![opening_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut sum = Money::ZERO;
    for s in rows {
        sum += Money::parse_or_zero(&s);
    }
    Ok(sum)
}

/// 涉及某张期初单的全部核销记录
pub fn opening_settle_list(db: &Db, opening_id: i64) -> DbResult<Vec<OpeningSettle>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {OS_COLS} FROM opening_settle WHERE opening_id=?1 ORDER BY id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![opening_id], map_os)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 某科目的全部期初核销记录（核销台账页用）
pub fn opening_settle_of_account(db: &Db, account: &str) -> DbResult<Vec<OpeningSettle>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {OS_COLS} FROM opening_settle WHERE account_code=?1 OR account_code LIKE ?1||'%' ORDER BY period, id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![account], map_os)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 期初单对应的科目与辅助核算（核销校验用）。
///
/// 期初单自己不存科目——`arap_opening` 只有 kind（ar/ap），科目是账套参数里的
/// 应收/应付科目。这样改账套参数时期初单自动跟着走，不会留一批挂在旧科目上的孤儿。
fn opening_account_of(conn: &Connection, kind: &str) -> DbResult<(String, fincore::AuxKind)> {
    let opts = crate::options_of(conn);
    Ok(match kind {
        "ap" => (opts.biz_accounts.ap.clone(), fincore::AuxKind::Supplier),
        _ => (opts.biz_accounts.ar.clone(), fincore::AuxKind::Customer),
    })
}

/// 期初单核销（事务内版）：把一张期初欠款单与一条收付款分录配掉 `amount`。
///
/// 校验与 [`settle_in_tx`] 对齐：
/// ① 核销金额必须大于零；② 不能超过期初单未核销额；③ 不能超过分录未核销额；
/// ④ 分录必须与期初单同科目、同往来单位；⑤ 分录必须已记账（与「被核销方必须
/// 已记账」同一条 H-3 口径的镜像——期初单本身就是「已入账」的历史欠款）。
pub fn opening_settle_in_tx(
    tx: &rusqlite::Transaction<'_>,
    opening_id: i64,
    to_entry: i64,
    amount: Money,
    who: &str,
) -> DbResult<i64> {
    if amount <= Money::ZERO {
        return Err(fincore::FinError::msg("核销金额必须大于零").into());
    }
    let o = match arap_opening_get_tx(tx, opening_id)? {
        Some(o) => o,
        None => return Err(fincore::FinError::not_found("往来期初明细").into()),
    };
    let (account, kind) = opening_account_of(tx, &o.kind)?;
    let e = match super::entry_of(tx, to_entry)? {
        Some(e) => e,
        None => return Err(fincore::FinError::not_found("核销方分录").into()),
    };
    if e.account_code != account {
        return Err(fincore::FinError::msg(format!(
            "分录科目 {} 与期初单科目 {} 不一致，不能核销",
            e.account_code, account
        ))
        .into());
    }
    let want_key = crate::balances::aux_key_frag(&o.party_code, kind);
    if e.aux_key != want_key {
        return Err(fincore::FinError::msg("分录的往来单位与期初单不一致，不能核销").into());
    }
    if !e.posted {
        return Err(
            fincore::FinError::msg("核销方必须是已记账的往来分录；未记账凭证请先记账").into(),
        );
    }
    let open_o = o.amount - opening_settled_of(tx, opening_id)?;
    if amount > open_o {
        return Err(fincore::FinError::msg(format!(
            "核销金额 {amount} 超过期初单未核销额 {open_o}"
        ))
        .into());
    }
    let open_e = e.signed().abs() - super::settled_of(tx, to_entry)?;
    if amount > open_e {
        return Err(fincore::FinError::msg(format!(
            "核销金额 {amount} 超过核销方未核销额 {open_e}"
        ))
        .into());
    }
    let existing: String = tx
        .query_row(
            "SELECT amount FROM opening_settle WHERE opening_id=?1 AND to_entry=?2",
            rusqlite::params![opening_id, to_entry],
            |r| r.get(0),
        )
        .unwrap_or_default();
    let new_amount = Money::parse_or_zero(&existing) + amount;
    let rec_period = crate::periods::current_period_of(tx)?;
    tx.execute(
        "INSERT INTO opening_settle(period,account_code,aux_key,opening_id,to_entry,amount,
            settled_by,settled_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(opening_id,to_entry) DO UPDATE SET
             amount=excluded.amount, settled_by=excluded.settled_by, settled_at=excluded.settled_at",
        rusqlite::params![
            rec_period.ymm(),
            account,
            e.aux_key,
            opening_id,
            to_entry,
            crate::money_param(new_amount),
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// 期初单核销（自动提交）
pub fn opening_settle(
    db: &Db,
    opening_id: i64,
    to_entry: i64,
    amount: Money,
    who: &str,
) -> DbResult<i64> {
    let tx = db.write_tx()?;
    let id = opening_settle_in_tx(&tx, opening_id, to_entry, amount, who)?;
    tx.commit()?;
    Ok(id)
}

/// 撤销一条期初核销记录（全额）。已结账期间的记录不允许撤销，与 [`unsettle`] 同规矩。
pub fn opening_unsettle(db: &Db, id: i64) -> DbResult<()> {
    let tx = db.write_tx()?;
    let (period, _): (i32, i64) = tx
        .query_row(
            "SELECT period, opening_id FROM opening_settle WHERE id=?1",
            rusqlite::params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| fincore::FinError::not_found("期初核销记录"))?;
    if let Some(upto) = crate::periods::closed_upto_of(&tx)? {
        let p = Period::from_ymm(period);
        if p <= upto {
            return Err(fincore::FinError::msg(format!(
                "这条期初核销记在 {}，该期间已结账（已结至 {}）——已结账期间的核销不能撤销。\
                 确需更正请先反结账，再改动。",
                p.label(),
                upto.label()
            ))
            .into());
        }
    }
    tx.execute(
        "DELETE FROM opening_settle WHERE id=?1",
        rusqlite::params![id],
    )?;
    tx.commit()?;
    Ok(())
}

/// 事务内的期初单读取（`arap_opening_get` 收 `&Db`，核销在事务里要另一份）
fn arap_opening_get_tx(conn: &Connection, id: i64) -> DbResult<Option<ArapOpening>> {
    let mut st = conn.prepare(&format!("SELECT {ARO_COLS} FROM arap_opening WHERE id=?1"))?;
    let mut rows = st.query_map([id], map_arap_opening)?;
    Ok(rows.next().transpose()?)
}

/// 把期初明细包装成「未清分录」参与自动核销 / FIFO。
///
/// 期初单没有凭证分录，但核销算法（`auto_settle` 的等额配对与 FIFO）都是按
/// `OpenEntry` 走的。这里用**负 entry_id**（`-opening.id`）当占位：正数是真分录，
/// 负数是期初单，核销时按符号分派到 `settle` / `opening_settle`。
/// 这样不用给 `settle_record` 加伪外键行，也不用复制一份 FIFO 算法。
pub fn opening_open_entries(db: &Db, account: &str, upto: Period) -> DbResult<Vec<OpenEntry>> {
    let opts = db.options();
    // 科目匹配要双向：核销页常按父级科目查（`1122`），而账套参数里存的是明细
    // 科目（`112201`）。只判 `account.starts_with(ar)` 会让父级查询一条期初都看不到。
    let covers = |code: &str| -> bool {
        let c = code.trim();
        !c.is_empty() && (account == c || account.starts_with(c) || c.starts_with(account))
    };
    let kind = if covers(&opts.biz_accounts.ap) {
        "ap"
    } else if covers(&opts.biz_accounts.ar) {
        "ar"
    } else {
        return Ok(Vec::new());
    };
    let (code, aux_kind) = opening_account_of(db.conn(), kind)?;
    let rows = arap_opening_list(db, Some(kind))?;
    let ids: Vec<i64> = rows.iter().map(|o| o.id).collect();
    let settled = opening_settled_map(db, &ids)?;
    let mut out = Vec::new();
    for o in rows {
        let d = NaiveDate::parse_from_str(&o.doc_date, "%Y-%m-%d")
            .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
        if Period::from_date(d).ymm() > upto.ymm() {
            continue;
        }
        let s = settled.get(&o.id).copied().unwrap_or(Money::ZERO);
        if (o.amount - s) <= Money::ZERO {
            continue;
        }
        let (debit, credit) = if kind == "ap" {
            (Money::ZERO, o.amount)
        } else {
            (o.amount, Money::ZERO)
        };
        out.push(OpenEntry {
            entry_id: -o.id,
            voucher_id: 0,
            period: Period::from_date(d),
            date: d,
            word: "期初".into(),
            no: o.id as i32,
            line: 0,
            summary: o.memo.clone(),
            account_code: code.clone(),
            aux_key: crate::balances::aux_key_frag(&o.party_code, aux_kind),
            settle_no: String::new(),
            debit,
            credit,
            settled: s,
            posted: true,
        });
    }
    Ok(out)
}

/// 核销一对「未清行」：真分录走 `settle`，期初单（负 entry_id）走 `opening_settle`。
///
/// `from` 是借方那条、`to` 是贷方那条（与 `settle` 的约定一致）。期初单可能是
/// 借方（应收欠款）也可能是贷方（应付欠款）：
/// - 应收期初在 `from` 位 → `opening_settle(-from, to)`
/// - 应付期初在 `to` 位   → `opening_settle(-to, from)`
///
/// 两种情况里期初单都是「被核销的欠款单」（`opening_settle.opening_id`），
/// 另一条才是核销方（`opening_settle.to_entry`）——参数顺序不能照搬，得翻。
pub fn settle_pair(db: &Db, from: i64, to: i64, amount: Money, who: &str) -> DbResult<i64> {
    match (from < 0, to < 0) {
        (true, false) => opening_settle(db, -from, to, amount, who),
        (false, true) => opening_settle(db, -to, from, amount, who),
        (false, false) => super::settle(db, from, to, amount, who),
        (true, true) => Err(fincore::FinError::msg("两条期初明细之间不能互相核销").into()),
    }
}
