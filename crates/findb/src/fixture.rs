//! 挂具台账（氧化厂的挂具 / 挂篮 / 夹具）
//!
//! 挂具是「工件上槽时要挂的那套东西」：自己有寿命、会坏、常常是某个客户专用的。
//! 这类东西不该混进存货（不是按金额流转的商品，也没有逐件计价的意义），
//! 但生产上必须能回答三个问题：
//!
//! 1. 现在有几套在库里、几套在车间（[`balances`]）；
//! 2. 这批活用的是哪几套、给哪个客户的（[`moves`] 按客户过滤）；
//! 3. 哪些挂具次数快用完了，该换新（[`ToolBalance::needs_replace`]）。
//!
//! 单据四类（[`KINDS`]）：
//! - `buy` 购置：在库 +，来源是新买的；
//! - `issue` 领用：在库 −，在用 +（挂具进了车间）；
//! - `back` 归还：在用 −，在库 +，同时累计使用次数（寿命只看归还，不看领用——
//!   领了没用就还回来不该记一次）；
//! - `scrap` 报废：**只从在库扣**。口径上坏挂具要先归还入库再报废，
//!   这样「在用」永远是「在车间还没回来的」，不会出现账上挂着已报废的在用数。
//!
//! 口径约定（和客供料台账一致）：只记数量与次数，不进存货、不出凭证；
//! 数量一律 `Money`（Decimal）在 **Rust 侧**加减，绝不写 `SUM(qty)` ——
//! SQLite 的 `SUM` 会退化成 REAL 浮点。

use chrono::NaiveDate;
use fincore::{FinError, Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 校验类错误（跟着 `DbError` 走，调用方照常 `.to_string()` 展示）
fn invalid(msg: impl Into<String>) -> crate::DbError {
    crate::DbError::Fin(FinError::validate(msg))
}

/// 状态类错误（余额不够、档案已被引用等）
fn blocked(msg: impl Into<String>) -> crate::DbError {
    crate::DbError::Fin(FinError::state(msg))
}

const I_COLS: &str = "id, code, name, tool_kind, spec, unit, life_times, disabled, memo";
const M_COLS: &str =
    "id, no, period, date, tool_code, customer, kind, qty, times, memo, created_by, created_at";

/// 单据类型（数据值, 中文标签）
pub const KINDS: &[(&str, &str)] = &[
    ("buy", "购置"),
    ("issue", "领用"),
    ("back", "归还"),
    ("scrap", "报废"),
];

/// 单据类型的中文标签（不认识的原样返回，便于前端兜底显示）
pub fn kind_label(kind: &str) -> &str {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, l)| *l)
        .unwrap_or(kind)
}

/// 单号前缀：GJ-{G/L/H/B}-期间-序号
fn no_prefix(kind: &str) -> &'static str {
    match kind {
        "buy" => "GJ-G",
        "issue" => "GJ-L",
        "back" => "GJ-H",
        _ => "GJ-B",
    }
}

// ============================ 档案 ============================

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ToolItem {
    pub id: i64,
    /// 挂具编号（档案主键，业务上用记号笔写在挂具上）
    pub code: String,
    pub name: String,
    /// 挂具 / 挂篮 / 夹具（自由文本，各厂说法不一样）
    pub tool_kind: String,
    /// 规格（适配什么尺寸的工件）
    pub spec: String,
    pub unit: String,
    /// 额定使用次数，0 = 不限
    pub life_times: i64,
    pub disabled: bool,
    pub memo: String,
}

fn map_item(row: &rusqlite::Row) -> rusqlite::Result<ToolItem> {
    Ok(ToolItem {
        id: row.get(0)?,
        code: row.get(1)?,
        name: row.get(2)?,
        tool_kind: row.get(3)?,
        spec: row.get(4)?,
        unit: row.get(5)?,
        life_times: row.get(6)?,
        disabled: row.get::<_, i64>(7)? != 0,
        memo: row.get(8)?,
    })
}

/// 档案列表（编号升序）
pub fn item_list(db: &Db) -> DbResult<Vec<ToolItem>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(&format!("SELECT {I_COLS} FROM tool_item ORDER BY code"))?;
    let rows = stmt
        .query_map([], map_item)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 按编号取档案
pub fn item_get(db: &Db, code: &str) -> DbResult<Option<ToolItem>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(&format!("SELECT {I_COLS} FROM tool_item WHERE code=?1"))?;
    Ok(stmt.query_row([code], map_item).optional()?)
}

/// 新增/修改档案（按 code 判重）。返回行 id。
pub fn item_save(db: &Db, it: &mut ToolItem) -> DbResult<i64> {
    let code = it.code.trim().to_string();
    if code.is_empty() {
        return Err(invalid("挂具编号必填"));
    }
    if it.life_times < 0 {
        return Err(invalid("额定使用次数不能为负（0 表示不限）"));
    }
    let tx = db.write_tx()?;
    let existing: Option<i64> = tx
        .query_row("SELECT id FROM tool_item WHERE code=?1", [&code], |r| {
            r.get(0)
        })
        .optional()?;
    let id = match existing {
        Some(id) => {
            tx.execute(
                "UPDATE tool_item SET name=?2, tool_kind=?3, spec=?4, unit=?5, life_times=?6, \
                 disabled=?7, memo=?8 WHERE id=?1",
                rusqlite::params![
                    id,
                    it.name.trim(),
                    it.tool_kind.trim(),
                    it.spec.trim(),
                    if it.unit.trim().is_empty() {
                        "套"
                    } else {
                        it.unit.trim()
                    },
                    it.life_times,
                    it.disabled as i64,
                    it.memo.trim(),
                ],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO tool_item(code, name, tool_kind, spec, unit, life_times, disabled, memo) \
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    code,
                    it.name.trim(),
                    if it.tool_kind.trim().is_empty() { "挂具" } else { it.tool_kind.trim() },
                    it.spec.trim(),
                    if it.unit.trim().is_empty() { "套" } else { it.unit.trim() },
                    it.life_times,
                    it.disabled as i64,
                    it.memo.trim(),
                ],
            )?;
            tx.last_insert_rowid()
        }
    };
    tx.commit()?;
    it.id = id;
    it.code = code;
    Ok(id)
}

/// 删除档案。已有流水的不能删 —— 删了结存就无从重算。
pub fn item_delete(db: &Db, code: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM tool_move WHERE tool_code=?1",
        [code],
        |r| r.get(0),
    )?;
    if n > 0 {
        return Err(blocked(format!(
            "挂具 {code} 已有 {n} 条收发记录，不能删档案（停用即可）"
        )));
    }
    tx.execute("DELETE FROM tool_item WHERE code=?1", [code])?;
    tx.commit()?;
    Ok(())
}

// ============================ 收发流水 ============================

#[derive(Clone, Debug, serde::Serialize)]
pub struct ToolMove {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    pub tool_code: String,
    /// 客户专用挂具；留空 = 通用
    pub customer: String,
    pub kind: String,
    pub qty: Money,
    /// 本次归还折合使用次数（寿命累计用）
    pub times: i64,
    pub memo: String,
    pub created_by: String,
    pub created_at: String,
}

impl ToolMove {
    pub fn new(tool_code: &str, kind: &str, date: NaiveDate) -> Self {
        ToolMove {
            id: 0,
            no: String::new(),
            period: Period::from_date(date),
            date,
            tool_code: tool_code.to_string(),
            customer: String::new(),
            kind: kind.to_string(),
            qty: Money::ZERO,
            times: 0,
            memo: String::new(),
            created_by: String::new(),
            created_at: String::new(),
        }
    }

    pub fn kind_label(&self) -> &str {
        kind_label(&self.kind)
    }
}

fn map_move(row: &rusqlite::Row) -> rusqlite::Result<ToolMove> {
    let period: i32 = row.get(2)?;
    let date: String = row.get(3)?;
    Ok(ToolMove {
        id: row.get(0)?,
        no: row.get(1)?,
        period: Period::from_ymm(period),
        date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").unwrap_or_default(),
        tool_code: row.get(4)?,
        customer: row.get(5)?,
        kind: row.get(6)?,
        qty: crate::read_money(row, 7)?,
        times: row.get(8)?,
        memo: row.get(9)?,
        created_by: row.get(10)?,
        created_at: row.get(11)?,
    })
}

/// 流水列表（日期倒序，可按挂具编号 / 客户过滤）
pub fn moves(db: &Db, tool: Option<&str>, customer: Option<&str>) -> DbResult<Vec<ToolMove>> {
    let conn = db.conn();
    let mut sql = format!("SELECT {M_COLS} FROM tool_move WHERE 1=1");
    let mut params: Vec<String> = Vec::new();
    if let Some(t) = tool.map(str::trim).filter(|s| !s.is_empty()) {
        params.push(t.to_string());
        sql.push_str(&format!(" AND tool_code=?{}", params.len()));
    }
    if let Some(c) = customer.map(str::trim).filter(|s| !s.is_empty()) {
        params.push(c.to_string());
        sql.push_str(&format!(" AND customer=?{}", params.len()));
    }
    sql.push_str(" ORDER BY date DESC, id DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), map_move)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 单据取一条
pub fn move_get(db: &Db, id: i64) -> DbResult<Option<ToolMove>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(&format!("SELECT {M_COLS} FROM tool_move WHERE id=?1"))?;
    Ok(stmt.query_row([id], map_move).optional()?)
}

/// 保存一条收发记录。返回 id。
pub fn move_save(db: &Db, m: &mut ToolMove, who: &str) -> DbResult<i64> {
    let kind = m.kind.trim().to_string();
    if !KINDS.iter().any(|(k, _)| *k == kind) {
        return Err(invalid(format!("单据类型不认识：{kind}")));
    }
    let code = m.tool_code.trim().to_string();
    if code.is_empty() {
        return Err(invalid("挂具编号必填"));
    }
    if !m.qty.is_positive() {
        return Err(invalid("数量必须大于 0"));
    }
    if item_get(db, &code)?.is_none() {
        return Err(invalid(format!(
            "挂具 {code} 不在档案里，先在「挂具档案」里建一条"
        )));
    }
    if m.period.ymm() != Period::from_date(m.date).ymm() {
        m.period = Period::from_date(m.date);
    }
    // 归还没填次数时按套数折算一次（1 套归还 = 1 次使用）
    if kind == "back" && m.times <= 0 {
        let whole = m.qty.cents() / 100;
        if whole > 0 {
            m.times = whole;
        }
    }

    let tx = db.write_tx()?;
    let bal = balance_in(&tx, &code)?;
    match kind.as_str() {
        "issue" if m.qty.cents() > bal.in_stock.cents() => {
            return Err(invalid(format!(
                "挂具 {code} 在库 {}，本次领用 {} 不够（不够就先登记购置）",
                bal.in_stock.fmt_qty(),
                m.qty.fmt_qty()
            )));
        }
        "back" if m.qty.cents() > bal.in_use.cents() => {
            return Err(invalid(format!(
                "挂具 {code} 在用 {}，本次归还 {} 超出（先用「领用」登记出去）",
                bal.in_use.fmt_qty(),
                m.qty.fmt_qty()
            )));
        }
        "scrap" if m.qty.cents() > bal.in_stock.cents() => {
            return Err(invalid(format!(
                "挂具 {code} 在库 {}，本次报废 {} 超出（坏挂具先「归还」入库，再报废）",
                bal.in_stock.fmt_qty(),
                m.qty.fmt_qty()
            )));
        }
        _ => {}
    }

    if m.no.trim().is_empty() {
        m.no = next_no(&tx, &kind, m.period)?;
    }
    tx.execute(
        "INSERT INTO tool_move(no, period, date, tool_code, customer, kind, qty, times, memo, \
         created_by, created_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            m.no.trim(),
            m.period.ymm(),
            m.date.format("%Y-%m-%d").to_string(),
            code,
            m.customer.trim(),
            kind,
            crate::exact_param(m.qty),
            m.times,
            m.memo.trim(),
            who,
            now(),
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    m.id = id;
    Ok(id)
}

/// 删除一条流水（录错了才用；结存随之重算）
pub fn move_delete(db: &Db, id: i64) -> DbResult<()> {
    let tx = db.write_tx()?;
    let n = tx.execute("DELETE FROM tool_move WHERE id=?1", [id])?;
    if n == 0 {
        return Err(blocked("记录不存在，可能已被删除"));
    }
    tx.commit()?;
    Ok(())
}

/// 单号自增：GJ-G-202601-001
fn next_no(tx: &rusqlite::Transaction, kind: &str, period: Period) -> DbResult<String> {
    let prefix = format!("{}-{}", no_prefix(kind), period.ymm());
    let n: i64 = tx.query_row(
        "SELECT COUNT(*) FROM tool_move WHERE no LIKE ?1 || '-%'",
        [&prefix],
        |r| r.get(0),
    )?;
    let mut seq = n + 1;
    loop {
        let no = format!("{prefix}-{seq:03}");
        let taken: Option<i64> = tx
            .query_row("SELECT id FROM tool_move WHERE no=?1", [&no], |r| r.get(0))
            .optional()?;
        if taken.is_none() {
            return Ok(no);
        }
        seq += 1;
    }
}

// ============================ 结存 ============================

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ToolBalance {
    pub tool_code: String,
    pub name: String,
    pub tool_kind: String,
    pub spec: String,
    pub unit: String,
    /// 在库
    pub in_stock: Money,
    /// 在车间（领出去还没回来）
    pub in_use: Money,
    /// 累计报废
    pub scraped: Money,
    /// 累计使用次数
    pub used_times: i64,
    /// 额定使用次数（0 = 不限）
    pub life_times: i64,
    pub disabled: bool,
}

impl ToolBalance {
    /// 该换新了：额定次数用完（0 = 不限，永不提示）
    pub fn needs_replace(&self) -> bool {
        self.life_times > 0 && self.used_times >= self.life_times
    }

    /// 剩余可用次数（不限使用次数时给 None）
    pub fn times_left(&self) -> Option<i64> {
        if self.life_times > 0 {
            Some((self.life_times - self.used_times).max(0))
        } else {
            None
        }
    }
}

/// 单个挂具的结存（事务内版本，供写入前的余额校验用）
fn balance_in(tx: &rusqlite::Transaction, code: &str) -> DbResult<ToolBalance> {
    let mut stmt =
        tx.prepare("SELECT kind, qty, times FROM tool_move WHERE tool_code=?1 ORDER BY id")?;
    let rows = stmt
        .query_map([code], |r| {
            let kind: String = r.get(0)?;
            let qty = crate::read_money(r, 1)?;
            let times: i64 = r.get(2)?;
            Ok((kind, qty, times))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut bal = ToolBalance {
        tool_code: code.to_string(),
        ..Default::default()
    };
    let item = tx
        .query_row(
            "SELECT name, tool_kind, spec, unit, life_times, disabled FROM tool_item WHERE code=?1",
            [code],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?;
    if let Some((name, kind, spec, unit, life, dis)) = item {
        bal.name = name;
        bal.tool_kind = kind;
        bal.spec = spec;
        bal.unit = unit;
        bal.life_times = life;
        bal.disabled = dis != 0;
    }
    for (kind, qty, times) in rows {
        match kind.as_str() {
            "buy" => bal.in_stock += qty,
            "issue" => {
                bal.in_stock -= qty;
                bal.in_use += qty;
            }
            "back" => {
                bal.in_use -= qty;
                bal.in_stock += qty;
                bal.used_times += times;
            }
            "scrap" => {
                bal.in_stock -= qty;
                bal.scraped += qty;
            }
            _ => {}
        }
    }
    Ok(bal)
}

/// 全部挂具的结存（档案里没有流水的那几套也列出来，数量全 0）
pub fn balances(db: &Db) -> DbResult<Vec<ToolBalance>> {
    let items = item_list(db)?;
    let conn = db.conn();
    // 一次把流水取平，再在 Rust 侧聚合（不写 SUM，避免退化成浮点）
    let mut stmt = conn.prepare("SELECT tool_code, kind, qty, times FROM tool_move ORDER BY id")?;
    let rows = stmt
        .query_map([], |r| {
            let code: String = r.get(0)?;
            let kind: String = r.get(1)?;
            let qty = crate::read_money(r, 2)?;
            let times: i64 = r.get(3)?;
            Ok((code, kind, qty, times))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out: Vec<ToolBalance> = items
        .into_iter()
        .map(|it| ToolBalance {
            tool_code: it.code,
            name: it.name,
            tool_kind: it.tool_kind,
            spec: it.spec,
            unit: it.unit,
            in_stock: Money::ZERO,
            in_use: Money::ZERO,
            scraped: Money::ZERO,
            used_times: 0,
            life_times: it.life_times,
            disabled: it.disabled,
        })
        .collect();
    for (code, kind, qty, times) in rows {
        let Some(b) = out.iter_mut().find(|b| b.tool_code == code) else {
            continue;
        };
        match kind.as_str() {
            "buy" => b.in_stock += qty,
            "issue" => {
                b.in_stock -= qty;
                b.in_use += qty;
            }
            "back" => {
                b.in_use -= qty;
                b.in_stock += qty;
                b.used_times += times;
            }
            "scrap" => {
                b.in_stock -= qty;
                b.scraped += qty;
            }
            _ => {}
        }
    }
    Ok(out)
}

/// 某个挂具的结存（界面显示与外部检查用）
pub fn balance_of(db: &Db, code: &str) -> DbResult<ToolBalance> {
    balances(db)?
        .into_iter()
        .find(|b| b.tool_code == code)
        .ok_or_else(|| blocked(format!("挂具 {code} 不在档案里")))
}

/// 需要换新的挂具（寿命用完的）
pub fn need_replace(db: &Db) -> DbResult<Vec<ToolBalance>> {
    Ok(balances(db)?
        .into_iter()
        .filter(|b| b.needs_replace())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn item(db: &Db, code: &str, life: i64) {
        let mut it = ToolItem {
            code: code.to_string(),
            name: format!("{code} 号挂具"),
            tool_kind: "挂具".to_string(),
            spec: "600×400".to_string(),
            unit: "套".to_string(),
            life_times: life,
            ..Default::default()
        };
        item_save(db, &mut it).unwrap();
    }

    fn mv(db: &Db, code: &str, kind: &str, qty: &str, day: u32) -> ToolMove {
        let mut m = ToolMove::new(code, kind, NaiveDate::from_ymd_opt(2026, 1, day).unwrap());
        m.qty = Money::parse(qty).unwrap();
        move_save(db, &mut m, "张三").unwrap();
        m
    }

    #[test]
    fn 购置领用归还报废的结存() {
        let db = mem();
        item(&db, "GJ01", 50);
        mv(&db, "GJ01", "buy", "10", 1);
        let b = balance_of(&db, "GJ01").unwrap();
        assert_eq!(b.in_stock.fmt_qty(), "10", "购置 10 套应在库 10");
        assert_eq!(b.in_use.fmt_qty(), "0");

        mv(&db, "GJ01", "issue", "4", 2);
        let b = balance_of(&db, "GJ01").unwrap();
        assert_eq!(b.in_stock.fmt_qty(), "6", "领用 4 套后库里剩 6");
        assert_eq!(b.in_use.fmt_qty(), "4", "领出去 4 套在车间");

        let mut back = ToolMove::new("GJ01", "back", NaiveDate::from_ymd_opt(2026, 1, 3).unwrap());
        back.qty = Money::parse("3").unwrap();
        move_save(&db, &mut back, "张三").unwrap();
        let b = balance_of(&db, "GJ01").unwrap();
        assert_eq!(b.in_stock.fmt_qty(), "9");
        assert_eq!(b.in_use.fmt_qty(), "1");
        assert_eq!(b.used_times, 3, "归还 3 套折合 3 次使用");

        mv(&db, "GJ01", "scrap", "2", 4);
        let b = balance_of(&db, "GJ01").unwrap();
        assert_eq!(b.in_stock.fmt_qty(), "7", "报废从在库扣");
        assert_eq!(b.scraped.fmt_qty(), "2");
        assert_eq!(b.in_use.fmt_qty(), "1", "报废不动在用");
    }

    #[test]
    fn 领用超过在库要报错() {
        let db = mem();
        item(&db, "GJ02", 0);
        mv(&db, "GJ02", "buy", "2", 1);
        let mut m = ToolMove::new(
            "GJ02",
            "issue",
            NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(),
        );
        m.qty = Money::parse("3").unwrap();
        let e = move_save(&db, &mut m, "张三").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("在库 2"), "报错要说清在库多少：{msg}");
        assert!(msg.contains("先登记购置"), "报错要给出下一步：{msg}");
    }

    #[test]
    fn 归还未领用的挂具要报错() {
        let db = mem();
        item(&db, "GJ03", 0);
        let mut m = ToolMove::new("GJ03", "back", NaiveDate::from_ymd_opt(2026, 1, 2).unwrap());
        m.qty = Money::parse("1").unwrap();
        let e = move_save(&db, &mut m, "张三").unwrap_err();
        assert!(e.to_string().contains("在用 0"), "没说清在用多少：{}", e);
    }

    #[test]
    fn 报废数量超过在库要提示先归还() {
        let db = mem();
        item(&db, "GJ04", 0);
        mv(&db, "GJ04", "buy", "5", 1);
        mv(&db, "GJ04", "issue", "4", 2);
        // 在库只剩 1，报废 2 套应被拦，并提示先归还
        let mut m = ToolMove::new(
            "GJ04",
            "scrap",
            NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(),
        );
        m.qty = Money::parse("2").unwrap();
        let e = move_save(&db, &mut m, "张三").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("在库 1"), "{msg}");
        assert!(msg.contains("先「归还」入库"), "要引导先归还：{msg}");
    }

    #[test]
    fn 使用次数用完要提示换新() {
        let db = mem();
        item(&db, "GJ05", 2);
        mv(&db, "GJ05", "buy", "2", 1);
        mv(&db, "GJ05", "issue", "2", 2);
        let mut back = ToolMove::new("GJ05", "back", NaiveDate::from_ymd_opt(2026, 1, 3).unwrap());
        back.qty = Money::parse("1").unwrap();
        back.times = 1;
        move_save(&db, &mut back, "张三").unwrap();
        let b = balance_of(&db, "GJ05").unwrap();
        assert!(!b.needs_replace(), "只用了 1 次，还没到寿命");
        assert_eq!(b.times_left(), Some(1));

        let mut back2 = ToolMove::new("GJ05", "back", NaiveDate::from_ymd_opt(2026, 1, 4).unwrap());
        back2.qty = Money::parse("1").unwrap();
        back2.times = 1;
        move_save(&db, &mut back2, "张三").unwrap();
        let b = balance_of(&db, "GJ05").unwrap();
        assert_eq!(b.used_times, 2);
        assert!(b.needs_replace(), "用满 2 次应提示换新");
        assert_eq!(b.times_left(), Some(0));
        assert_eq!(need_replace(&db).unwrap().len(), 1);

        // 不限次数的挂具永远不提示
        item(&db, "GJ06", 0);
        mv(&db, "GJ06", "buy", "1", 1);
        mv(&db, "GJ06", "issue", "1", 2);
        let mut back3 = ToolMove::new("GJ06", "back", NaiveDate::from_ymd_opt(2026, 1, 3).unwrap());
        back3.qty = Money::parse("1").unwrap();
        back3.times = 999;
        move_save(&db, &mut back3, "张三").unwrap();
        let b6 = balance_of(&db, "GJ06").unwrap();
        assert!(!b6.needs_replace(), "life_times=0 表示不限，不该提示换新");
        assert_eq!(b6.times_left(), None);
    }

    #[test]
    fn 单号按类型与期间生成且不重复() {
        let db = mem();
        item(&db, "GJ07", 0);
        let a = mv(&db, "GJ07", "buy", "10", 1);
        let b = mv(&db, "GJ07", "issue", "3", 2);
        let c = mv(&db, "GJ07", "back", "3", 3);
        let d = mv(&db, "GJ07", "scrap", "1", 4);
        assert_eq!(a.no, "GJ-G-202601-001");
        assert_eq!(b.no, "GJ-L-202601-001");
        assert_eq!(c.no, "GJ-H-202601-001");
        assert_eq!(d.no, "GJ-B-202601-001");
        // 同类型第二张接着排
        let e = mv(&db, "GJ07", "buy", "1", 5);
        assert_eq!(e.no, "GJ-G-202601-002");
    }

    #[test]
    fn 档案编号必填且有条流水不能删() {
        let db = mem();
        let mut it = ToolItem::default();
        let e = item_save(&db, &mut it).unwrap_err();
        assert!(e.to_string().contains("编号必填"));

        item(&db, "GJ08", 0);
        mv(&db, "GJ08", "buy", "1", 1);
        let e = item_delete(&db, "GJ08").unwrap_err();
        assert!(e.to_string().contains("已有 1 条收发记录"), "{e}");

        item(&db, "GJ09", 0);
        item_delete(&db, "GJ09").unwrap();
        assert!(item_get(&db, "GJ09").unwrap().is_none());
    }

    #[test]
    fn 客户专用挂具能按客户查流水() {
        let db = mem();
        item(&db, "GJ10", 0);
        let mut a = ToolMove::new("GJ10", "buy", NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        a.qty = Money::parse("10").unwrap();
        a.customer = "C001".to_string();
        move_save(&db, &mut a, "张三").unwrap();

        let mut b = ToolMove::new("GJ10", "buy", NaiveDate::from_ymd_opt(2026, 1, 2).unwrap());
        b.qty = Money::parse("2").unwrap();
        move_save(&db, &mut b, "张三").unwrap();

        let all = moves(&db, Some("GJ10"), None).unwrap();
        assert_eq!(all.len(), 2);
        let c1 = moves(&db, Some("GJ10"), Some("C001")).unwrap();
        assert_eq!(c1.len(), 1, "按客户过滤只出这个客户专用的那几条");
        assert_eq!(c1[0].customer, "C001");
    }
}
