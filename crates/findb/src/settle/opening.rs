//! 往来期初明细（按单据）：平台迁移导入 v2 用，期初欠款精确到单。
//!
//! 分册说明：本文件原属 `settle.rs` 单文件（3,437 行），按小节标题切开，
//! 函数体与注释一字未改；跨文件共用的内部函数标 `pub(super)`，对外 API 由
//! `settle/mod.rs` 统一 `pub use` 出去，外部调用路径 `findb::settle::xxx` 不变。

use super::*;

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
