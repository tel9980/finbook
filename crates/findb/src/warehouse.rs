//! 仓库主数据（对标金蝶仓库档案）
//!
//! - 建账/升级自动种一个默认仓「01 主仓」（`is_default=1`）；
//! - 出入库等写流水的入口统一走 [`resolve`]：空 = 默认仓、非空必须存在且未停用
//!   （历史 `warehouse=''` 的流水保留为「未指定」，不做数据改写）；
//! - 默认仓不可删除；被流水/盘点行引用过的仓不可删除。

use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

/// 仓库档案
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Warehouse {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub memo: String,
}

const COLS: &str = "code,name,is_default,disabled,memo";

fn map(r: &rusqlite::Row) -> rusqlite::Result<Warehouse> {
    Ok(Warehouse {
        code: r.get(0)?,
        name: r.get(1)?,
        is_default: r.get::<_, i64>(2)? != 0,
        disabled: r.get::<_, i64>(3)? != 0,
        memo: r.get(4)?,
    })
}

/// 仓库列表（默认仓在前，启用在前）
pub fn list(db: &Db) -> DbResult<Vec<Warehouse>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {COLS} FROM warehouse ORDER BY is_default DESC, disabled ASC, code"
    ))?;
    let rows = st.query_map([], map)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get(db: &Db, code: &str) -> DbResult<Option<Warehouse>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM warehouse WHERE code=?1"),
            rusqlite::params![code],
            map,
        )
        .optional()
        .map_err(Into::into)
}

/// 新增/修改仓库（upsert）。设为默认仓时自动清掉其它默认标记（单默认）。
pub fn save(db: &Db, w: &Warehouse) -> DbResult<()> {
    let code = w.code.trim();
    let name = w.name.trim();
    if code.is_empty() {
        return Err(fincore::FinError::msg("仓库编码不能为空").into());
    }
    if name.is_empty() {
        return Err(fincore::FinError::msg("仓库名称不能为空").into());
    }
    let tx = db.write_tx()?;
    if w.is_default {
        tx.execute("UPDATE warehouse SET is_default=0", [])?;
    }
    tx.execute(
        "INSERT INTO warehouse(code,name,is_default,disabled,memo)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(code) DO UPDATE SET name=excluded.name,
             is_default=excluded.is_default, disabled=excluded.disabled, memo=excluded.memo",
        rusqlite::params![
            code,
            name,
            w.is_default as i64,
            w.disabled as i64,
            w.memo.trim()
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// 删除仓库：默认仓不可删；被库存流水或盘点行引用过不可删（保账实可追溯）
pub fn delete(db: &Db, code: &str) -> DbResult<()> {
    let w = get(db, code)?.ok_or_else(|| fincore::FinError::not_found("仓库"))?;
    if w.is_default {
        return Err(fincore::FinError::msg("默认仓库不能删除，请先把其它仓库设为默认").into());
    }
    let used: i64 = db.conn().query_row(
        "SELECT (SELECT COUNT(*) FROM stock_move WHERE warehouse=?1)
              + (SELECT COUNT(*) FROM inv_count WHERE warehouse=?1)",
        rusqlite::params![code],
        |r| r.get(0),
    )?;
    if used > 0 {
        return Err(fincore::FinError::msg(format!(
            "仓库已被 {used} 条流水/盘点行引用，不能删除（可停用）"
        ))
        .into());
    }
    db.conn()
        .execute("DELETE FROM warehouse WHERE code=?1", rusqlite::params![code])?;
    Ok(())
}

/// 默认仓编码（连接版，事务内可用）：默认启用仓 → 首个启用仓 → "01"
pub fn default_code_conn(conn: &rusqlite::Connection) -> DbResult<String> {
    let code: Option<String> = conn
        .query_row(
            "SELECT code FROM warehouse WHERE is_default=1 AND disabled=0 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(c) = code {
        return Ok(c);
    }
    let code: Option<String> = conn
        .query_row(
            "SELECT code FROM warehouse WHERE disabled=0 ORDER BY code LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(code.unwrap_or_else(|| "01".to_string()))
}

pub fn default_code(db: &Db) -> DbResult<String> {
    default_code_conn(db.conn())
}

/// 解析写入用的仓库编码（连接版）：
/// 空 → 默认仓；非空 → 必须存在且未停用（防写入不存在仓库造成报表口径分裂）。
pub fn resolve_conn(conn: &rusqlite::Connection, input: &str) -> DbResult<String> {
    let code = input.trim();
    if code.is_empty() {
        return default_code_conn(conn);
    }
    let w: Option<(i64,)> = conn
        .query_row(
            "SELECT disabled FROM warehouse WHERE code=?1",
            rusqlite::params![code],
            |r| Ok((r.get(0)?,)),
        )
        .optional()?;
    match w {
        Some((0,)) => Ok(code.to_string()),
        Some(_) => Err(fincore::FinError::msg(format!("仓库 {code} 已停用，不能写入")).into()),
        None => Err(fincore::FinError::msg(format!(
            "仓库 {code} 不存在，请先在「仓库」档案里维护"
        ))
        .into()),
    }
}

pub fn resolve(db: &Db, input: &str) -> DbResult<String> {
    resolve_conn(db.conn(), input)
}
