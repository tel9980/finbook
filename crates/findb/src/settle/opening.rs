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
