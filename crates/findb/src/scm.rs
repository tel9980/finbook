//! 供应链管理：采购订单 / 销售订单 / BOM / 生产订单
//!
//! 对标金蝶云星空 / 用友 T+ Cloud 的供应链基础模块。

use chrono::NaiveDate;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult, FinError};

/// 状态列以**无引号文本**落库（历史行为：`to_value(...).as_str()`），而 serde_json
/// 解析枚举要求合法 JSON——裸 `Draft` 会报 "expected value"。读回时统一补引号，
/// 并兼容历史数据中偶发的带引号值；否则所有状态会被 `unwrap_or(Draft)` 静默吞掉。
pub fn status_from<T: serde::de::DeserializeOwned>(stored: &str) -> Option<T> {
    let s = stored.trim();
    let quoted = if s.starts_with('"') {
        s.to_string()
    } else {
        format!("\"{s}\"")
    };
    serde_json::from_str(&quoted).ok()
}

// ===========================================================================
// 采购订单
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum PoStatus {
    Draft,
    Confirmed,
    PartialIn,
    Completed,
    Cancelled,
}

impl PoStatus {
    pub fn label(self) -> &'static str {
        match self {
            PoStatus::Draft => "草稿",
            PoStatus::Confirmed => "已确认",
            PoStatus::PartialIn => "部分入库",
            PoStatus::Completed => "已完成",
            PoStatus::Cancelled => "已作废",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PoLine {
    pub id: i64,
    pub po_id: i64,
    pub item_code: String,
    pub item_name: String,
    pub qty_ordered: Money,
    pub qty_received: Money,
    pub unit_price: Money,
    pub tax_rate: Money,
    pub amount: Money,
    pub tax_amount: Money,
    pub memo: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PurchaseOrder {
    pub id: i64,
    pub period: Period,
    pub no: String,
    pub date: NaiveDate,
    pub supplier_code: String,
    pub supplier_name: String,
    pub status: PoStatus,
    pub total_amount: Money,
    pub total_tax: Money,
    pub received_amount: Money,
    pub prepared_by: String,
    pub memo: String,
    pub lines: Vec<PoLine>,
}

impl PurchaseOrder {
    pub fn new(
        period: Period,
        date: NaiveDate,
        supplier_code: &str,
        supplier_name: &str,
        prepared_by: &str,
    ) -> Self {
        Self {
            id: 0,
            period,
            no: String::new(),
            date,
            supplier_code: supplier_code.to_string(),
            supplier_name: supplier_name.to_string(),
            status: PoStatus::Draft,
            total_amount: Money::ZERO,
            total_tax: Money::ZERO,
            received_amount: Money::ZERO,
            prepared_by: prepared_by.to_string(),
            memo: String::new(),
            lines: Vec::new(),
        }
    }
}

/// 按 id 取采购订单（含明细）
pub fn po_get(db: &Db, id: i64) -> DbResult<Option<PurchaseOrder>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, period, no, date, supplier_code, supplier_name, status,
                total_amount, total_tax, received_amount, prepared_by, memo
         FROM purchase_order WHERE id=?1",
    )?;
    let po = stmt
        .query_row([id], |r| {
            Ok(PurchaseOrder {
                id: r.get(0)?,
                period: Period::from_ymm(r.get(1)?),
                no: r.get(2)?,
                date: r.get(3)?,
                supplier_code: r.get(4)?,
                supplier_name: r.get(5)?,
                status: status_from(&r.get::<_, String>(6)?).unwrap_or(PoStatus::Draft),
                total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                received_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                prepared_by: r.get(10)?,
                memo: r.get(11)?,
                lines: Vec::new(),
            })
        })
        .optional()?;
    let Some(po) = po else {
        return Ok(None);
    };
    let mut po = po;
    let mut lstmt = db.conn().prepare(
        "SELECT id, item_code, item_name, qty_ordered, qty_received,
                unit_price, tax_rate, amount, tax_amount, memo
         FROM po_line WHERE po_id=? ORDER BY id",
    )?;
    po.lines = lstmt
        .query_map([id], |r| {
            Ok(PoLine {
                id: r.get(0)?,
                po_id: id,
                item_code: r.get(1)?,
                item_name: r.get(2)?,
                qty_ordered: Money::parse_or_zero(&r.get::<_, String>(3)?),
                qty_received: Money::parse_or_zero(&r.get::<_, String>(4)?),
                unit_price: Money::parse_or_zero(&r.get::<_, String>(5)?),
                tax_rate: Money::parse_or_zero(&r.get::<_, String>(6)?),
                amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                tax_amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
                memo: r.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(po))
}

/// 采购订单合法状态流转
///
/// 已完成/已作废是终态，不能回退；进行中的单子也不能凭空跳回草稿。
/// 收货/入库进度由 `po_progress` 推进，不接受手工往回拨。
fn po_transition_ok(from: PoStatus, to: PoStatus) -> bool {
    use PoStatus::*;
    match (from, to) {
        (a, b) if a == b => true, // 幂等
        (Draft, Confirmed) | (Draft, Cancelled) => true,
        (Confirmed, PartialIn) | (Confirmed, Cancelled) => true,
        (PartialIn, Completed) | (PartialIn, Cancelled) => true,
        _ => false,
    }
}

/// 采购订单状态流转（草稿 → 已确认 / 作废）
///
/// 只改状态列，绝不把明细整表读出再 `po_save` 写回——`po_save` 会 `DELETE` + 重插
/// 全部 `po_line`，这期间并发入库记上的 `qty_received` 会被这轮回滚覆盖。
/// 写入用「原状态」做条件（比较并交换），命中 0 行即状态已被他人改动。
pub fn po_set_status(db: &Db, id: i64, to: PoStatus) -> DbResult<()> {
    let from: String = db
        .conn()
        .query_row("SELECT status FROM purchase_order WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| FinError::not_found("采购订单"))?;
    let cur = status_from::<PoStatus>(&from).unwrap_or(PoStatus::Draft);
    if !po_transition_ok(cur, to) {
        return Err(FinError::state(format!(
            "采购订单不能从「{}」流转到「{}」",
            cur.label(),
            to.label()
        ))
        .into());
    }
    let n = db.conn().execute(
        "UPDATE purchase_order SET status=?2 WHERE id=?1 AND status=?3",
        rusqlite::params![id, serde_json::to_value(to)?.as_str().unwrap(), from],
    )?;
    if n == 0 {
        return Err(FinError::state("采购订单状态已被他人变更，请刷新后重试").into());
    }
    Ok(())
}

// ===========================================================================
// 销售订单
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum SoStatus {
    Draft,
    Confirmed,
    PartialShip,
    Completed,
    Cancelled,
}

impl SoStatus {
    pub fn label(self) -> &'static str {
        match self {
            SoStatus::Draft => "草稿",
            SoStatus::Confirmed => "已确认",
            SoStatus::PartialShip => "部分发货",
            SoStatus::Completed => "已完成",
            SoStatus::Cancelled => "已作废",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SoLine {
    pub id: i64,
    pub so_id: i64,
    pub item_code: String,
    pub item_name: String,
    pub qty_ordered: Money,
    pub qty_shipped: Money,
    pub unit_price: Money,
    pub tax_rate: Money,
    pub amount: Money,
    pub tax_amount: Money,
    pub memo: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SalesOrder {
    pub id: i64,
    pub period: Period,
    pub no: String,
    pub date: NaiveDate,
    pub customer_code: String,
    pub customer_name: String,
    pub status: SoStatus,
    pub total_amount: Money,
    pub total_tax: Money,
    pub shipped_amount: Money,
    pub prepared_by: String,
    pub memo: String,
    pub lines: Vec<SoLine>,
}

impl SalesOrder {
    pub fn new(
        period: Period,
        date: NaiveDate,
        customer_code: &str,
        customer_name: &str,
        prepared_by: &str,
    ) -> Self {
        Self {
            id: 0,
            period,
            no: String::new(),
            date,
            customer_code: customer_code.to_string(),
            customer_name: customer_name.to_string(),
            status: SoStatus::Draft,
            total_amount: Money::ZERO,
            total_tax: Money::ZERO,
            shipped_amount: Money::ZERO,
            prepared_by: prepared_by.to_string(),
            memo: String::new(),
            lines: Vec::new(),
        }
    }
}

// ===========================================================================
// BOM & 生产
// ===========================================================================

#[derive(Clone, Debug)]
pub struct BomItem {
    pub id: i64,
    pub parent_code: String,
    pub child_code: String,
    pub qty: Money,
    pub loss_rate: Money,
    pub seq: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ProdStatus {
    Draft,
    Released,
    InProgress,
    Completed,
    Cancelled,
}

impl ProdStatus {
    pub fn label(self) -> &'static str {
        match self {
            ProdStatus::Draft => "草稿",
            ProdStatus::Released => "已下达",
            ProdStatus::InProgress => "生产中",
            ProdStatus::Completed => "已完工",
            ProdStatus::Cancelled => "已作废",
        }
    }
    /// 落库码（小写，与 prod_complete/prod_start 的裸 SQL 口径一致）
    pub fn code(self) -> &'static str {
        match self {
            ProdStatus::Draft => "draft",
            ProdStatus::Released => "released",
            ProdStatus::InProgress => "in_progress",
            ProdStatus::Completed => "completed",
            ProdStatus::Cancelled => "cancelled",
        }
    }
}

/// 读生产订单状态：容忍历史三种存量（裸小写 / 裸驼峰 serde 值 / 带引号 JSON），未知回退草稿。
/// 此前直接 `serde_json::from_str`（裸值不是合法 JSON）导致所有状态被读成 Draft。
pub fn prod_status_from(s: &str) -> ProdStatus {
    match s.trim().trim_matches('"').to_ascii_lowercase().as_str() {
        "released" => ProdStatus::Released,
        "inprogress" | "in_progress" => ProdStatus::InProgress,
        "completed" => ProdStatus::Completed,
        "cancelled" | "canceled" => ProdStatus::Cancelled,
        _ => ProdStatus::Draft,
    }
}

#[derive(Clone, Debug)]
pub struct ProductionOrder {
    pub id: i64,
    pub no: String,
    pub period: Period,
    pub date: NaiveDate,
    pub item_code: String,
    pub item_name: String,
    pub planned_qty: Money,
    pub completed_qty: Money,
    pub status: ProdStatus,
    pub work_center: String,
    /// 来源销售订单（0 = 独立建单：备货或 MRP 建议）
    ///
    /// 产销之间在数据上必须是连着的。缺这一列时，「这批货是哪张订单要的」
    /// 只能靠人肉记忆，而缺货时更无法反查「哪些订单正等着这批料」。
    pub so_id: i64,
    pub prepared_by: String,
    pub memo: String,
    /// inhouse 自制 / outsourcing 委外
    pub order_kind: String,
    pub supplier_code: String,
    pub supplier_name: String,
    /// 细排计划开工日（空 = 未排，链6）
    pub plan_start: String,
    /// 细排计划完工日
    pub plan_end: String,
}

// ===========================================================================
// 数据库操作
// ===========================================================================

pub fn po_next_no(db: &Db, period: Period) -> DbResult<String> {
    let year = period.year();
    let month = period.month();
    let prefix = format!(
        "{}{:04}{:02}",
        crate::doc_prefix(db, "po", "CG"),
        year,
        month
    );
    let sql = format!(
        "SELECT COALESCE(MAX(CAST(SUBSTR(no, {}) AS INTEGER)), 0) + 1 FROM purchase_order WHERE no LIKE ?",
        prefix.len() + 1
    );
    let n: i64 = db
        .conn()
        .query_row(&sql, [format!("{}%", prefix)], |r| r.get(0))
        .unwrap_or(0);
    Ok(format!("{}{:04}", prefix, n))
}

pub fn so_next_no(db: &Db, period: Period) -> DbResult<String> {
    let year = period.year();
    let month = period.month();
    let prefix = format!(
        "{}{:04}{:02}",
        crate::doc_prefix(db, "so", "XS"),
        year,
        month
    );
    let sql = format!(
        "SELECT COALESCE(MAX(CAST(SUBSTR(no, {}) AS INTEGER)), 0) + 1 FROM sales_order WHERE no LIKE ?",
        prefix.len() + 1
    );
    let n: i64 = db
        .conn()
        .query_row(&sql, [format!("{}%", prefix)], |r| r.get(0))
        .unwrap_or(0);
    Ok(format!("{}{:04}", prefix, n))
}

pub fn po_save(db: &Db, po: &mut PurchaseOrder) -> DbResult<i64> {
    let tx = db.write_tx()?;
    po.total_amount = po.lines.iter().map(|l| l.amount).sum();
    po.total_tax = po.lines.iter().map(|l| l.tax_amount).sum();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let id = if po.id > 0 {
        tx.execute(
            "UPDATE purchase_order SET period=?, date=?, supplier_code=?, supplier_name=?,
             status=?, total_amount=?, total_tax=?, received_amount=?, prepared_by=?, memo=?, updated_at=?
             WHERE id=?",
            rusqlite::params![
                po.period.ymm(), po.date, po.supplier_code, po.supplier_name,
                serde_json::to_value(po.status)?.as_str().unwrap(),
                crate::money_param(po.total_amount), crate::money_param(po.total_tax),
                crate::money_param(po.received_amount), po.prepared_by, po.memo, now, po.id
            ],
        )?;
        po.id
    } else {
        tx.execute(
            "INSERT INTO purchase_order(period, no, date, supplier_code, supplier_name,
             status, total_amount, total_tax, received_amount, prepared_by, memo, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)",
            rusqlite::params![
                po.period.ymm(), po.no, po.date, po.supplier_code, po.supplier_name,
                serde_json::to_value(po.status)?.as_str().unwrap(),
                crate::money_param(po.total_amount), crate::money_param(po.total_tax),
                crate::money_param(po.received_amount), po.prepared_by, po.memo, now
            ],
        )?;
        tx.last_insert_rowid()
    };
    po.id = id;

    tx.execute("DELETE FROM po_line WHERE po_id=?", [id])?;
    for line in &po.lines {
        tx.execute(
            "INSERT INTO po_line(po_id, item_code, item_name, qty_ordered, qty_received,
             unit_price, tax_rate, amount, tax_amount, memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                id,
                line.item_code,
                line.item_name,
                crate::exact_param(line.qty_ordered),
                crate::exact_param(line.qty_received),
                crate::exact_param(line.unit_price),
                crate::exact_param(line.tax_rate),
                crate::money_param(line.amount),
                crate::money_param(line.tax_amount),
                line.memo
            ],
        )?;
    }
    tx.commit()?;
    Ok(id)
}

pub fn po_delete(db: &Db, id: i64) -> DbResult<()> {
    // 两步删除必须同事务：否则第二步失败会留下没有明细的空壳单据
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM po_line WHERE po_id=?", [id])?;
    tx.execute("DELETE FROM purchase_order WHERE id=?", [id])?;
    tx.commit()?;
    Ok(())
}

pub fn po_list(db: &Db, period: Period, status: Option<PoStatus>) -> DbResult<Vec<PurchaseOrder>> {
    let sql = if status.is_some() {
        "SELECT id, period, no, date, supplier_code, supplier_name, status,
             total_amount, total_tax, received_amount, prepared_by, memo
             FROM purchase_order WHERE period=? AND status=? ORDER BY date DESC, id DESC"
            .to_string()
    } else {
        "SELECT id, period, no, date, supplier_code, supplier_name, status,
             total_amount, total_tax, received_amount, prepared_by, memo
             FROM purchase_order WHERE period=? ORDER BY date DESC, id DESC"
            .to_string()
    };

    let mut stmt = db.conn().prepare(&sql)?;
    let rows = if let Some(s) = status {
        stmt.query_map(
            rusqlite::params![period.ymm(), serde_json::to_value(s)?.as_str().unwrap()],
            |r| {
                Ok(PurchaseOrder {
                    id: r.get(0)?,
                    period: Period::from_ymm(r.get(1)?),
                    no: r.get(2)?,
                    date: r.get(3)?,
                    supplier_code: r.get(4)?,
                    supplier_name: r.get(5)?,
                    status: status_from(&r.get::<_, String>(6)?).unwrap_or(PoStatus::Draft),
                    total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                    total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                    received_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                    prepared_by: r.get(10)?,
                    memo: r.get(11)?,
                    lines: Vec::new(),
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([period.ymm()], |r| {
            Ok(PurchaseOrder {
                id: r.get(0)?,
                period: Period::from_ymm(r.get(1)?),
                no: r.get(2)?,
                date: r.get(3)?,
                supplier_code: r.get(4)?,
                supplier_name: r.get(5)?,
                status: status_from(&r.get::<_, String>(6)?).unwrap_or(PoStatus::Draft),
                total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                received_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                prepared_by: r.get(10)?,
                memo: r.get(11)?,
                lines: Vec::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };

    // 一次取回本期间所有到货流水，Rust 侧按订单分组累加（不在 SQL 里 SUM——
    // 金额/数量列全库按 TEXT 存，SQLite 对 TEXT 的 SUM 返回 Integer/Real）
    let recv_by_po = po_receipt_qty_map(db, period)?;

    let mut orders = Vec::new();
    for mut po in rows {
        let mut stmt = db.conn().prepare(
            "SELECT id, item_code, item_name, qty_ordered, qty_received,
             unit_price, tax_rate, amount, tax_amount, memo
             FROM po_line WHERE po_id=? ORDER BY id",
        )?;
        let lines = stmt
            .query_map([po.id], |r| {
                Ok(PoLine {
                    id: r.get(0)?,
                    po_id: po.id,
                    item_code: r.get(1)?,
                    item_name: r.get(2)?,
                    qty_ordered: Money::parse_or_zero(&r.get::<_, String>(3)?),
                    qty_received: Money::parse_or_zero(&r.get::<_, String>(4)?),
                    unit_price: Money::parse_or_zero(&r.get::<_, String>(5)?),
                    tax_rate: Money::parse_or_zero(&r.get::<_, String>(6)?),
                    amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                    tax_amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
                    memo: r.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        po.lines = lines;
        // 执行进度（已收数量/金额）从 po_receipt 实时汇总，不读冗余列。
        //
        // 与销售侧同一个毛病：`purchase_order.received_amount` / `po_line.qty_received`
        // 建表后**从未被任何代码回写**（只被读和初始化），到货只往 po_receipt 插行。
        // 于是采购订单列表显示「已收 0」却同时显示状态「已入库完成」，自相矛盾。
        let recv_qty = *recv_by_po.get(&po.id).unwrap_or(&Money::ZERO);
        po.received_amount = po_received_gross(&po, recv_qty);
        for l in po.lines.iter_mut() {
            l.qty_received = recv_qty;
        }
        orders.push(po);
    }
    Ok(orders)
}

/// 本期间各采购订单的累计已收数量（Rust 侧累加，不在 SQL 里 SUM）
fn po_receipt_qty_map(db: &Db, period: Period) -> DbResult<std::collections::HashMap<i64, Money>> {
    let mut st = db
        .conn()
        .prepare("SELECT po_id, qty FROM po_receipt WHERE period=?1")?;
    let rows = st.query_map([period.ymm()], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut map: std::collections::HashMap<i64, Money> = std::collections::HashMap::new();
    for r in rows {
        let (po_id, qty) = r?;
        *map.entry(po_id).or_insert(Money::ZERO) += Money::parse_or_zero(&qty);
    }
    Ok(map)
}

/// 某存货的可承诺量（ATP, Available to Promise）
///
/// ```text
/// ATP = 现有可用库存 + 在途（已下达未完工的生产订单剩余量） − 已占用（已确认销售订单未发货量）
/// ```
///
/// **为什么销售订单确认时必须看 ATP**：ERP 里最贵的两类错，一类是接了不该接的
/// 单（超卖），另一类是拒了本该接的单（丢生意）。而「拒单」几乎总是因为 ATP
/// 算错——只减已占用、不加在途，会把「三天后就能做完的量」误判成不可承诺；
/// 只加库存不减占用，则会在多张订单抢同一批料时超卖。
///
/// 三个数都分别返回而不只给结果：只看一个 ATP 数字，被算错了也不知道错在哪。
/// 数量列同样是 TEXT 存储，所以三处都在 Rust 侧累加（沿用全库约定）。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Atp {
    pub item: String,
    pub on_hand: Money,
    pub incoming: Money,
    pub committed: Money,
    pub atp: Money,
    /// 在途里**已排期**的部分（有 `plan_end`，能给客户一个具体日期）
    pub incoming_dated: Money,
    /// 在途里**没排期**的部分 —— 有量但答不出交期
    ///
    /// 刻意与 `incoming_dated` 分开而不是只给个总数。销售问「这批什么时候能到」，
    /// 一个合并后的「在途 500」回答不了：其中 300 有排期、200 没排期，
    /// 而没排期那 200 **根本不能用来承诺交期**。合成一个数，承诺日期就成了编的。
    pub incoming_undated: Money,
    /// 在途里最早的计划完工日（`incoming_dated > 0` 时才有值）
    pub earliest_ready: String,
}

pub fn atp(db: &Db, item: &str) -> DbResult<Atp> {
    let code = item.trim();
    if code.is_empty() {
        return Ok(Atp::default());
    }

    // 现有可用库存：排除待检（来料检验未转正的货不可领用，算进 ATP 就是虚承诺）
    let mut on_hand = Money::ZERO;
    {
        let mut st = db
            .conn()
            .prepare("SELECT qty, qc_status FROM stock_move WHERE item=?1")?;
        let rows = st.query_map([code], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?;
        for r in rows {
            let (qty, qc) = r?;
            if qc == "pending" {
                continue;
            }
            on_hand += Money::parse_or_zero(&qty);
        }
    }

    // 在途：已下推但未完工的生产订单剩余量（草稿也算——草稿是已认领的产能）
    let mut incoming = Money::ZERO;
    let incoming_dated;
    let incoming_undated;
    let earliest_ready;
    {
        let mut st = db.conn().prepare(
            "SELECT planned_qty, completed_qty, plan_end FROM production_order
             WHERE item_code=?1 AND status NOT IN ('completed','cancelled')",
        )?;
        let rows = st.query_map([code], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut dated = Money::ZERO;
        let mut undated = Money::ZERO;
        let mut earliest: Option<NaiveDate> = None;
        for r in rows {
            let (p, c, plan_end) = r?;
            let left = Money::parse_or_zero(&p) - Money::parse_or_zero(&c);
            if !left.is_positive() {
                continue;
            }
            incoming += left;
            // `plan_end` 是**计划员排的**，不是系统算的（仓里没有工作中心产能数据，
            // 编不出真实完工日）。所以有排期的才给日期，没排期的就明说答不出来。
            match NaiveDate::parse_from_str(plan_end.trim(), "%Y-%m-%d") {
                Ok(d) => {
                    dated += left;
                    earliest = Some(match earliest {
                        Some(e) if e <= d => e,
                        _ => d,
                    });
                }
                Err(_) => undated += left,
            }
        }
        incoming_dated = dated;
        incoming_undated = undated;
        earliest_ready = earliest.map(|d| d.to_string()).unwrap_or_default();
    }

    // 已占用：已确认销售订单未发货量（草稿/作废不占用——草稿还不是承诺）
    //
    // 两个坑都在这里踩过：
    //
    // 1. 刻意**不在 SQL 里按状态字符串过滤**。`SoStatus` 没有 `code()`、也没有
    //    serde rename，库里存的是首字母大写的 `"Draft"`/`"Confirmed"`，而
    //    `PoStatus`/`ProdStatus` 用的是小写 `code()`。照小写写
    //    `NOT IN ('draft',...)` 的结果是**一个都没排除掉**——草稿单被算成占用，
    //    ATP 凭空少 5。状态改在 Rust 侧用 `status_from` 解析，大小写写错也不会
    //    静默失效。
    // 2. 已发货量**必须从 `so_shipment` 汇总，不能读 `so_line.qty_shipped`**——
    //    后者是建表后从未被任何代码回写的冗余列（只有 `so_list` 的读路径做了
    //    实时汇总）。用它算 ATP 会把已发完的订单继续算成占用，ATP 变成负数，
    //    于是「明明发完了却说承诺不了」。
    let mut shipped_by_so: std::collections::HashMap<i64, Money> = std::collections::HashMap::new();
    {
        let mut st = db.conn().prepare("SELECT so_id, qty FROM so_shipment")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for r in rows {
            let (so_id, qty) = r?;
            *shipped_by_so.entry(so_id).or_insert(Money::ZERO) += Money::parse_or_zero(&qty);
        }
    }
    let mut committed = Money::ZERO;
    {
        let mut st = db.conn().prepare(
            "SELECT l.so_id, l.qty_ordered, s.status
             FROM so_line l JOIN sales_order s ON s.id = l.so_id
             WHERE l.item_code=?1",
        )?;
        let rows = st.query_map([code], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for r in rows {
            let (so_id, ordered, status) = r?;
            if matches!(
                status_from::<SoStatus>(&status),
                None | Some(SoStatus::Draft) | Some(SoStatus::Cancelled)
            ) {
                continue;
            }
            let shipped = *shipped_by_so.get(&so_id).unwrap_or(&Money::ZERO);
            let left = Money::parse_or_zero(&ordered) - shipped;
            if left.is_positive() {
                committed += left;
            }
        }
    }

    Ok(Atp {
        item: code.to_string(),
        atp: on_hand + incoming - committed,
        on_hand,
        incoming,
        committed,
        incoming_dated,
        incoming_undated,
        earliest_ready,
    })
}

/// 「到某日为止可承诺量」= 现货 + **计划完工日 ≤ 该日**的在途 − 已占用
///
/// 刻意**不把没排期的在途算进来**：那部分有量但没有日期，答不出「什么时候到」，
/// 算进来等于给一个自己都给不出的交期承诺。未排期的量在 `Atp.incoming_undated`
/// 里单列，由界面明确提示「这批没排期，不能用来承诺交期」。
pub fn atp_until(db: &Db, item: &str, on: NaiveDate) -> DbResult<Atp> {
    let a = atp(db, item)?;
    if a.incoming_undated.is_zero() && a.incoming_dated.is_zero() {
        return Ok(a);
    }
    // 已排期在途按 plan_end 逐单累计到该日为止的部分
    let mut ready = Money::ZERO;
    let code = item.trim();
    if !code.is_empty() {
        let mut st = db.conn().prepare(
            "SELECT planned_qty, completed_qty, plan_end FROM production_order
             WHERE item_code=?1 AND status NOT IN ('completed','cancelled')",
        )?;
        let rows = st.query_map([code], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for r in rows {
            let (p, c, plan_end) = r?;
            let left = Money::parse_or_zero(&p) - Money::parse_or_zero(&c);
            if !left.is_positive() {
                continue;
            }
            if let Ok(d) = NaiveDate::parse_from_str(plan_end.trim(), "%Y-%m-%d") {
                if d <= on {
                    ready += left;
                }
            }
        }
    }
    Ok(Atp {
        // 这个口径下的「在途」只含有日期保证的那部分
        incoming: ready,
        incoming_dated: ready,
        atp: a.on_hand + ready - a.committed,
        ..a
    })
}

/// 销售订单下推生产订单
///
/// 产销之间在数据上必须是连着的：以前 `production_order` 没有 `so_id`，生产
/// 出来的货与谁要它没有任何记录对应关系，于是「这批货是哪张订单要的」只能靠
/// 人肉记忆，缺货时更无法反查「哪些订单正等着这批料」。
///
/// 下推数量**封顶到该订单还没下推的余量**（订购 − 已发货 − 已下推生产），
/// 理由与销售发货封顶完全一样：金额/数量封顶与实物封顶必须同一个口径，
/// 否则会出现「订单说下推了 100、实际只认领了 60」这种对不上的状态。
pub fn prod_from_so(
    db: &Db,
    so_id: i64,
    period: Period,
    date: NaiveDate,
    want: Option<Money>,
    who: &str,
) -> DbResult<i64> {
    let so = so_get(db, so_id)?.ok_or_else(|| fincore::FinError::not_found("销售订单不存在"))?;
    if matches!(so.status, SoStatus::Draft | SoStatus::Cancelled) {
        return Err(fincore::FinError::state(format!(
            "订单 {} 是「{}」，先确认才能下推生产",
            so.no,
            so.status.label()
        ))
        .into());
    }
    let line = so
        .lines
        .first()
        .ok_or_else(|| fincore::FinError::state("销售订单没有明细行"))?;
    let item = line.item_code.clone();

    // 已下推生产量（所有非作废的生产订单都算——草稿也是已认领的产能，
    // 只排除 cancelled，否则同一张订单可以无限重复下推）
    let mut pushed = Money::ZERO;
    {
        let mut st = db.conn().prepare(
            "SELECT planned_qty FROM production_order
             WHERE so_id=?1 AND status <> 'cancelled'",
        )?;
        let rows = st.query_map([so_id], |r| r.get::<_, String>(0))?;
        for r in rows {
            pushed += Money::parse_or_zero(&r?);
        }
    }
    let shipped: Money = crate::sales::so_shipment_sum(db, so_id)?;
    let remain = line.qty_ordered - shipped - pushed;
    if !remain.is_positive() {
        return Err(fincore::FinError::state(format!(
            "订单 {} 没有可再下推的量（订购 {}，已发 {}，已下推生产 {}）",
            so.no, line.qty_ordered, shipped, pushed
        ))
        .into());
    }
    let qty = match want {
        Some(w) if w.is_positive() => w.min(remain),
        _ => remain,
    };

    let mut order = ProductionOrder {
        id: 0,
        no: prod_next_no(db, period)?,
        period,
        date,
        item_code: item,
        item_name: line.item_name.clone(),
        planned_qty: qty,
        completed_qty: Money::ZERO,
        status: ProdStatus::Released,
        work_center: String::new(),
        so_id,
        prepared_by: who.to_string(),
        memo: format!("下推自销售订单 {}", so.no),
        order_kind: "inhouse".to_string(),
        supplier_code: String::new(),
        supplier_name: String::new(),
        plan_start: String::new(),
        plan_end: String::new(),
    };
    let id = prod_save(db, &mut order)?;
    crate::docflow::link_add(db, "so", so_id, "prod", id, "销售订单下推生产订单")?;
    if qty < remain {
        db.log(
            who,
            "生产",
            "下推截断",
            &format!("{} 申请 {want:?}，实际下推 {qty}（剩余 {remain}）", so.no),
        )?;
    }
    Ok(id)
}

/// 按已收数量推导已收货金额（价税合计口径）
///
/// `po_receipt` 只记数量、没有金额列，金额按订购价税合计的**同一比例**推导：
/// `价税合计 × (已收数量 ÷ 订购总数量)`。与销售侧 `so_shipped_gross` 口径对称。
///
/// 注意这里是**允许超收**的：`po_receipt_with_stock` 不封顶（超收在实务里常见），
/// 所以已收数量可能大于订购数量，推导出的金额会大于订单金额 —— 这如实反映了
/// 「到货金额确实超过订单金额」，不是 bug。是否要提示/拦截是另一个产品决策。
pub fn po_received_gross(po: &PurchaseOrder, received_qty: Money) -> Money {
    let ordered: Money = po.lines.iter().map(|l| l.qty_ordered).sum();
    if ordered.is_zero() || !received_qty.is_positive() {
        return Money::ZERO;
    }
    let gross = po.total_amount + po.total_tax;
    match gross.checked_div(ordered) {
        Some(unit) => (unit * received_qty).round2(),
        None => Money::ZERO,
    }
}

pub fn so_save(db: &Db, so: &mut SalesOrder) -> DbResult<i64> {
    so.total_amount = so.lines.iter().map(|l| l.amount).sum();
    so.total_tax = so.lines.iter().map(|l| l.tax_amount).sum();
    // 信用控制（对标金蝶）：非草稿/非作废订单校验客户信用额度（辅助档案 props.credit_limit，0=不限）。
    // 占用 = 已确认订单（总额 − 已收款）：credit_check 不计草稿，因此旧单若是已确认要先剔除再加新额，
    // 草稿单首次确认则只加不减。
    if !matches!(so.status, SoStatus::Draft | SoStatus::Cancelled) && !so.customer_code.is_empty() {
        let (mut used, limit, _) = crate::sales::credit_check(db, &so.customer_code, so.period)?;
        if so.id > 0 {
            let (old_total, old_status): (String, String) = db
                .conn()
                .query_row(
                    "SELECT total_amount, status FROM sales_order WHERE id=?1",
                    [so.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?
                .unwrap_or_default();
            let old_counted = !matches!(old_status.as_str(), "Draft" | "Cancelled");
            if old_counted {
                used -= Money::parse_or_zero(&old_total);
            }
        }
        used += so.total_amount;
        if !limit.is_zero() && used > limit {
            return Err(FinError::state(format!(
                "客户 {} 信用额度不足：占用 {}，额度 {}（信用额度在辅助档案·客户中设置）",
                so.customer_code, used, limit
            ))
            .into());
        }
    }
    let tx = db.write_tx()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let id = if so.id > 0 {
        tx.execute(
            "UPDATE sales_order SET period=?, date=?, customer_code=?, customer_name=?,
             status=?, total_amount=?, total_tax=?, shipped_amount=?, prepared_by=?, memo=?, updated_at=?
             WHERE id=?",
            rusqlite::params![
                so.period.ymm(), so.date, so.customer_code, so.customer_name,
                serde_json::to_value(so.status)?.as_str().unwrap(),
                crate::money_param(so.total_amount), crate::money_param(so.total_tax),
                crate::money_param(so.shipped_amount), so.prepared_by, so.memo, now, so.id
            ],
        )?;
        so.id
    } else {
        tx.execute(
            "INSERT INTO sales_order(period, no, date, customer_code, customer_name,
             status, total_amount, total_tax, shipped_amount, prepared_by, memo, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)",
            rusqlite::params![
                so.period.ymm(), so.no, so.date, so.customer_code, so.customer_name,
                serde_json::to_value(so.status)?.as_str().unwrap(),
                crate::money_param(so.total_amount), crate::money_param(so.total_tax),
                crate::money_param(so.shipped_amount), so.prepared_by, so.memo, now
            ],
        )?;
        tx.last_insert_rowid()
    };
    so.id = id;

    tx.execute("DELETE FROM so_line WHERE so_id=?", [id])?;
    for line in &so.lines {
        tx.execute(
            "INSERT INTO so_line(so_id, item_code, item_name, qty_ordered, qty_shipped,
             unit_price, tax_rate, amount, tax_amount, memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                id,
                line.item_code,
                line.item_name,
                crate::exact_param(line.qty_ordered),
                crate::exact_param(line.qty_shipped),
                crate::exact_param(line.unit_price),
                crate::exact_param(line.tax_rate),
                crate::money_param(line.amount),
                crate::money_param(line.tax_amount),
                line.memo
            ],
        )?;
    }
    tx.commit()?;
    Ok(id)
}

pub fn so_delete(db: &Db, id: i64) -> DbResult<()> {
    // 两步删除必须同事务：否则第二步失败会留下没有明细的空壳单据
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM so_line WHERE so_id=?", [id])?;
    tx.execute("DELETE FROM sales_order WHERE id=?", [id])?;
    tx.commit()?;
    Ok(())
}

pub fn so_list(db: &Db, period: Period, status: Option<SoStatus>) -> DbResult<Vec<SalesOrder>> {
    let sql = if status.is_some() {
        "SELECT id, period, no, date, customer_code, customer_name, status,
             total_amount, total_tax, shipped_amount, prepared_by, memo
             FROM sales_order WHERE period=? AND status=? ORDER BY date DESC, id DESC"
            .to_string()
    } else {
        "SELECT id, period, no, date, customer_code, customer_name, status,
             total_amount, total_tax, shipped_amount, prepared_by, memo
             FROM sales_order WHERE period=? ORDER BY date DESC, id DESC"
            .to_string()
    };

    let mut stmt = db.conn().prepare(&sql)?;
    let rows = if let Some(s) = status {
        stmt.query_map(
            rusqlite::params![period.ymm(), serde_json::to_value(s)?.as_str().unwrap()],
            |r| {
                Ok(SalesOrder {
                    id: r.get(0)?,
                    period: Period::from_ymm(r.get(1)?),
                    no: r.get(2)?,
                    date: r.get(3)?,
                    customer_code: r.get(4)?,
                    customer_name: r.get(5)?,
                    status: status_from(&r.get::<_, String>(6)?).unwrap_or(SoStatus::Draft),
                    total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                    total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                    shipped_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                    prepared_by: r.get(10)?,
                    memo: r.get(11)?,
                    lines: Vec::new(),
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([period.ymm()], |r| {
            Ok(SalesOrder {
                id: r.get(0)?,
                period: Period::from_ymm(r.get(1)?),
                no: r.get(2)?,
                date: r.get(3)?,
                customer_code: r.get(4)?,
                customer_name: r.get(5)?,
                status: status_from(&r.get::<_, String>(6)?).unwrap_or(SoStatus::Draft),
                total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                shipped_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                prepared_by: r.get(10)?,
                memo: r.get(11)?,
                lines: Vec::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };

    // 一次取回本期间所有发货流水，Rust 侧按订单分组累加。
    //
    // 刻意**不在 SQL 里 SUM 金额/数量列**：全库约定是「金额存 TEXT、不在 SQL 里
    // SUM」（见 invoices::summary 的注释）。SQLite 对 TEXT 列的 SUM 返回
    // Integer/Real，get::<String> 直接报 InvalidColumnType——而且这个错只在
    // 「全整数数据」时出现，有小数时碰巧能过，属于最难自查的那类。
    let shipped_by_so = so_shipment_qty_map(db, period)?;

    let mut orders = Vec::new();
    for mut so in rows {
        let mut stmt = db.conn().prepare(
            "SELECT id, item_code, item_name, qty_ordered, qty_shipped,
             unit_price, tax_rate, amount, tax_amount, memo
             FROM so_line WHERE so_id=? ORDER BY id",
        )?;
        let lines = stmt
            .query_map([so.id], |r| {
                Ok(SoLine {
                    id: r.get(0)?,
                    so_id: so.id,
                    item_code: r.get(1)?,
                    item_name: r.get(2)?,
                    qty_ordered: Money::parse_or_zero(&r.get::<_, String>(3)?),
                    qty_shipped: Money::parse_or_zero(&r.get::<_, String>(4)?),
                    unit_price: Money::parse_or_zero(&r.get::<_, String>(5)?),
                    tax_rate: Money::parse_or_zero(&r.get::<_, String>(6)?),
                    amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                    tax_amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
                    memo: r.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        so.lines = lines;
        // 执行进度（已发数量/金额）从 so_shipment 实时汇总，不读冗余列。
        //
        // 冗余列 `sales_order.shipped_amount` / `so_line.qty_shipped` 建表后
        // **从未被任何代码回写**——发货只往 so_shipment 插行。于是订单列表显示
        // 「已发货 0.00」却同时显示状态「部分发货」，自相矛盾；而这恰恰是订单
        // 驱动最核心的执行进度字段。读时汇总也顺带免掉了「多处回写、各处可能
        // 漏一处」的漂移来源。
        let shipped_qty = *shipped_by_so.get(&so.id).unwrap_or(&Money::ZERO);
        so.shipped_amount = so_shipped_gross(&so, shipped_qty);
        for l in so.lines.iter_mut() {
            l.qty_shipped = shipped_qty;
        }
        orders.push(so);
    }
    Ok(orders)
}

/// 某期间各销售订单的累计已发数量（Rust 侧累加，不在 SQL 里 SUM）
fn so_shipment_qty_map(db: &Db, period: Period) -> DbResult<std::collections::HashMap<i64, Money>> {
    let mut st = db
        .conn()
        .prepare("SELECT so_id, qty FROM so_shipment WHERE period=?1")?;
    let rows = st.query_map([period.ymm()], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut map: std::collections::HashMap<i64, Money> = std::collections::HashMap::new();
    for r in rows {
        let (so_id, qty) = r?;
        *map.entry(so_id).or_insert(Money::ZERO) += Money::parse_or_zero(&qty);
    }
    Ok(map)
}

/// 按已发数量推导已发货金额（价税合计口径）
///
/// `so_shipment` 只记数量、没有金额列，所以金额按与收入确认**相同的比例口径**
/// 推导：`价税合计 × (已发数量 ÷ 订购总数量)`。这样「已发货金额」与「已确认
/// 收入 + 应收」天然一致，不会出现两个百分比对不上。
pub fn so_shipped_gross(so: &SalesOrder, shipped_qty: Money) -> Money {
    let ordered: Money = so.lines.iter().map(|l| l.qty_ordered).sum();
    if ordered.is_zero() || !shipped_qty.is_positive() {
        return Money::ZERO;
    }
    let gross = so.total_amount + so.total_tax;
    match gross.checked_div(ordered) {
        Some(unit) => (unit * shipped_qty).round2(),
        None => Money::ZERO,
    }
}

/// 某销售订单的累计已发（数量, 价税合计金额）
///
/// `so_shipment` **只记数量**（qty），没有金额列——发货金额按与收入确认**相同的
/// 比例口径**推导：`价税合计 × (已发数量 ÷ 订购总数量)`。这样「已发货金额」与
/// 「已确认收入+应收」天然一致，不会出现两个百分比对不上的情况。
pub fn so_shipment_progress(db: &Db, so: &SalesOrder) -> DbResult<(Money, Money)> {
    let ordered: Money = so.lines.iter().map(|l| l.qty_ordered).sum();
    // COALESCE 的默认值必须写 **TEXT 零** `'0'` 而不是 `0`：金额列全库按 TEXT 存，
    // 写成整数会让整个表达式变成 Integer 类型，`get::<_, String>` 直接报
    // InvalidColumnType —— 空表（还没发过货）时才触发，有数据时反而正常。
    let mut st = db
        .conn()
        .prepare("SELECT COALESCE(SUM(qty),'0') FROM so_shipment WHERE so_id=?1")?;
    let shipped_qty: Money =
        Money::parse_or_zero(&st.query_row([so.id], |r| r.get::<_, String>(0))?);
    if ordered.is_zero() || !shipped_qty.is_positive() {
        return Ok((shipped_qty, Money::ZERO));
    }
    let gross = so.total_amount + so.total_tax;
    // 不用除法直接乘：先算比例再乘，避免整数除法把金额抹成 0
    let amount = (gross * shipped_qty)
        .checked_div(ordered)
        .unwrap_or(Money::ZERO);
    Ok((shipped_qty, amount.round2()))
}

/// 按 id 取销售订单（含明细）
pub fn so_get(db: &Db, id: i64) -> DbResult<Option<SalesOrder>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, period, no, date, customer_code, customer_name, status,
                total_amount, total_tax, shipped_amount, prepared_by, memo
         FROM sales_order WHERE id=?1",
    )?;
    let so = stmt
        .query_row([id], |r| {
            Ok(SalesOrder {
                id: r.get(0)?,
                period: Period::from_ymm(r.get(1)?),
                no: r.get(2)?,
                date: r.get(3)?,
                customer_code: r.get(4)?,
                customer_name: r.get(5)?,
                status: status_from(&r.get::<_, String>(6)?).unwrap_or(SoStatus::Draft),
                total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                shipped_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                prepared_by: r.get(10)?,
                memo: r.get(11)?,
                lines: Vec::new(),
            })
        })
        .optional()?;
    let Some(so) = so else {
        return Ok(None);
    };
    let mut so = so;
    let mut lstmt = db.conn().prepare(
        "SELECT id, item_code, item_name, qty_ordered, qty_shipped,
                unit_price, tax_rate, amount, tax_amount, memo
         FROM so_line WHERE so_id=? ORDER BY id",
    )?;
    so.lines = lstmt
        .query_map([id], |r| {
            Ok(SoLine {
                id: r.get(0)?,
                so_id: id,
                item_code: r.get(1)?,
                item_name: r.get(2)?,
                qty_ordered: Money::parse_or_zero(&r.get::<_, String>(3)?),
                qty_shipped: Money::parse_or_zero(&r.get::<_, String>(4)?),
                unit_price: Money::parse_or_zero(&r.get::<_, String>(5)?),
                tax_rate: Money::parse_or_zero(&r.get::<_, String>(6)?),
                amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                tax_amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
                memo: r.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(so))
}

/// 销售订单合法状态流转
///
/// 已完成/已作废是终态；进行中的单子不能凭空跳回草稿。
/// 发货进度由 `so_progress_update` 自动推进，手工流转只负责确认与作废。
fn so_transition_ok(from: SoStatus, to: SoStatus) -> bool {
    use SoStatus::*;
    match (from, to) {
        (a, b) if a == b => true, // 幂等
        (Draft, Confirmed) | (Draft, Cancelled) => true,
        (Confirmed, PartialShip) | (Confirmed, Cancelled) => true,
        (PartialShip, Completed) | (PartialShip, Cancelled) => true,
        _ => false,
    }
}

/// 订单状态流转（草稿 → 已确认 / 作废）
///
/// 只改状态列，绝不把明细整表读出再 `so_save` 写回——`so_save` 会 `DELETE` + 重插
/// 全部 `so_line`，这期间并发发货记上的 `qty_shipped` 会被这轮回滚覆盖。
/// 写入用「原状态」做条件（比较并交换），命中 0 行即状态已被他人改动。
pub fn so_set_status(db: &Db, id: i64, to: SoStatus) -> DbResult<()> {
    // 只读表头：信用检查需要总额、客户与期间，但不需要任何一行明细
    let (from, total, customer, period) = db
        .conn()
        .query_row(
            "SELECT status, total_amount, customer_code, period FROM sales_order WHERE id=?1",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Money::parse_or_zero(&r.get::<_, String>(1)?),
                    r.get::<_, String>(2)?,
                    Period::from_ymm(r.get(3)?),
                ))
            },
        )
        .optional()?
        .ok_or_else(|| FinError::not_found("销售订单"))?;
    let cur = status_from::<SoStatus>(&from).unwrap_or(SoStatus::Draft);
    if !so_transition_ok(cur, to) {
        return Err(FinError::state(format!(
            "销售订单不能从「{}」流转到「{}」",
            cur.label(),
            to.label()
        ))
        .into());
    }
    // 信用控制（对标金蝶，口径与 so_save 完全一致）：credit_check 只计非草稿非作废订单，
    // 旧状态已计入的先剔除，状态流转不改金额所以再加回同一笔。
    if !matches!(to, SoStatus::Draft | SoStatus::Cancelled) && !customer.is_empty() {
        let (mut used, limit, _) = crate::sales::credit_check(db, &customer, period)?;
        if !matches!(cur, SoStatus::Draft | SoStatus::Cancelled) {
            used -= total;
        }
        used += total;
        if !limit.is_zero() && used > limit {
            return Err(FinError::state(format!(
                "客户 {} 信用额度不足：占用 {}，额度 {}（信用额度在辅助档案·客户中设置）",
                customer,
                used.fmt_money(),
                limit.fmt_money()
            ))
            .into());
        }
    }
    let n = db.conn().execute(
        "UPDATE sales_order SET status=?2 WHERE id=?1 AND status=?3",
        rusqlite::params![id, serde_json::to_value(to)?.as_str().unwrap(), from],
    )?;
    if n == 0 {
        return Err(FinError::state("销售订单状态已被他人变更，请刷新后重试").into());
    }
    Ok(())
}

/// 发货流水变化后同步订单状态：已确认 → 部分发货 / 已完成（不动草稿与作废）
pub fn so_progress_update(db: &Db, id: i64) -> DbResult<()> {
    let Some(so) = so_get(db, id)? else {
        return Ok(());
    };
    if !matches!(
        so.status,
        SoStatus::Confirmed | SoStatus::PartialShip | SoStatus::Completed
    ) {
        return Ok(());
    }
    let total_qty: Money = so.lines.iter().map(|l| l.qty_ordered).sum();
    if total_qty.is_zero() {
        return Ok(());
    }
    let shipped = crate::sales::so_shipment_sum(db, id)?;
    let to = if shipped.is_zero() {
        SoStatus::Confirmed
    } else if shipped >= total_qty {
        SoStatus::Completed
    } else {
        SoStatus::PartialShip
    };
    if to != so.status {
        // 同样用原状态做条件：并发发货时不会把别人刚推进的状态改回去。
        // 命中 0 行说明状态在这期间已被并发改写（另一笔发货已推进过），此时本函数
        // 算出的 `to` 已经过时，直接放弃即可——这是"由发货流水派生的状态"，
        // 报错只会把一次正常操作变成 500。
        db.conn().execute(
            "UPDATE sales_order SET status=?2 WHERE id=?1 AND status=?3",
            rusqlite::params![
                id,
                serde_json::to_value(to)?.as_str().unwrap(),
                serde_json::to_value(so.status)?.as_str().unwrap()
            ],
        )?;
    }
    Ok(())
}

// BOM操作
pub fn bom_list(db: &Db, parent_code: &str) -> DbResult<Vec<BomItem>> {
    bom_list_version(db, parent_code, "")
}

/// 按版本列 BOM（version 为空 = 默认版本）
pub fn bom_list_version(db: &Db, parent_code: &str, version: &str) -> DbResult<Vec<BomItem>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, parent_code, child_code, qty, loss_rate, seq
         FROM bom WHERE parent_code=?1 AND version=?2 ORDER BY seq",
    )?;
    let rows = stmt.query_map(rusqlite::params![parent_code, version], |r| {
        Ok(BomItem {
            id: r.get(0)?,
            parent_code: r.get(1)?,
            child_code: r.get(2)?,
            qty: Money::parse_or_zero(&r.get::<_, String>(3)?),
            loss_rate: Money::parse_or_zero(&r.get::<_, String>(4)?),
            seq: r.get(5)?,
        })
    })?;
    let mut items = Vec::new();
    for item in rows {
        items.push(item?);
    }
    Ok(items)
}

pub fn bom_save(db: &Db, parent_code: &str, children: &[(String, Money, Money)]) -> DbResult<()> {
    bom_save_version(db, parent_code, "", children, "")
}

/// 带版本保存 BOM，并记录变更历史
pub fn bom_save_version(
    db: &Db,
    parent_code: &str,
    version: &str,
    children: &[(String, Money, Money)],
    who: &str,
) -> DbResult<()> {
    let tx = db.write_tx()?;
    tx.execute(
        "DELETE FROM bom WHERE parent_code=?1 AND version=?2",
        rusqlite::params![parent_code, version],
    )?;
    for (i, (child_code, qty, loss_rate)) in children.iter().enumerate() {
        tx.execute(
            "INSERT INTO bom(parent_code, child_code, version, qty, loss_rate, seq) VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![parent_code, child_code, version, crate::exact_param(*qty), crate::exact_param(*loss_rate), i as i32]
        )?;
    }
    bom_log_tx(
        &tx,
        parent_code,
        "save",
        &format!("版本 {}，{} 个子件", version, children.len()),
        who,
    )?;
    tx.commit()?;
    Ok(())
}

pub fn bom_delete(db: &Db, parent_code: &str, version: &str, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    tx.execute(
        "DELETE FROM bom WHERE parent_code=?1 AND version=?2",
        rusqlite::params![parent_code, version],
    )?;
    bom_log_tx(
        &tx,
        parent_code,
        "delete",
        &format!("版本 {}", version),
        who,
    )?;
    tx.commit()?;
    Ok(())
}

fn bom_log_tx(
    tx: &rusqlite::Transaction,
    parent_code: &str,
    action: &str,
    detail: &str,
    who: &str,
) -> DbResult<()> {
    tx.execute(
        "INSERT INTO bom_change_log(parent_code, action, detail, changed_by, changed_at)
         VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            parent_code,
            action,
            detail,
            who,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(())
}

/// BOM 变更历史
pub fn bom_change_log(
    db: &Db,
    parent_code: &str,
) -> DbResult<Vec<(String, String, String, String)>> {
    let mut stmt = db.conn().prepare(
        "SELECT action, detail, changed_by, changed_at FROM bom_change_log
         WHERE parent_code=?1 ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([parent_code], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

// ===========================================================================
// 替代料
// ===========================================================================

#[derive(Clone, Debug)]
pub struct Substitute {
    pub id: i64,
    pub parent_code: String,
    pub child_code: String,
    pub substitute: String,
    pub ratio: Money,
    pub priority: i32,
}

pub fn bom_substitutes(db: &Db, parent_code: &str, child_code: &str) -> DbResult<Vec<Substitute>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, parent_code, child_code, substitute, ratio, priority
         FROM bom_substitute WHERE parent_code=?1 AND child_code=?2 ORDER BY priority",
    )?;
    let rows = stmt.query_map(rusqlite::params![parent_code, child_code], |r| {
        Ok(Substitute {
            id: r.get(0)?,
            parent_code: r.get(1)?,
            child_code: r.get(2)?,
            substitute: r.get(3)?,
            ratio: Money::parse_or_zero(&r.get::<_, String>(4)?),
            priority: r.get(5)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn bom_substitute_save(db: &Db, s: &Substitute, who: &str) -> DbResult<i64> {
    let tx = db.write_tx()?;
    tx.execute(
        "INSERT INTO bom_substitute(parent_code, child_code, substitute, ratio, priority)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(parent_code, child_code, substitute) DO UPDATE SET
             ratio=excluded.ratio, priority=excluded.priority",
        rusqlite::params![
            s.parent_code,
            s.child_code,
            s.substitute,
            crate::exact_param(s.ratio),
            s.priority
        ],
    )?;
    let id: i64 = tx.query_row(
        "SELECT id FROM bom_substitute WHERE parent_code=?1 AND child_code=?2 AND substitute=?3",
        rusqlite::params![s.parent_code, s.child_code, s.substitute],
        |r| r.get(0),
    )?;
    bom_log_tx(
        &tx,
        &s.parent_code,
        "add_sub",
        &format!("{}/{} → {}", s.child_code, s.substitute, s.ratio.fmt_qty()),
        who,
    )?;
    tx.commit()?;
    Ok(id)
}

pub fn bom_substitute_delete(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let parent: Option<String> = db
        .conn()
        .query_row(
            "SELECT parent_code FROM bom_substitute WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM bom_substitute WHERE id=?1", [id])?;
    if let Some(p) = parent {
        bom_log_tx(&tx, &p, "del_sub", &format!("替代料 id={id}"), who)?;
    }
    tx.commit()?;
    Ok(())
}

// ===========================================================================
// 多层 BOM 展开 & 成本汇总
// ===========================================================================

/// 展开节点
#[derive(Clone, Debug)]
pub struct BomNode {
    pub code: String,
    pub level: i32,
    /// 累计用量（1 单位顶层成品所需的该物料数量，含损耗）
    pub qty: Money,
}

/// 多层 BOM 展开：给定顶层成品与目标产量，逐层展开成 (物料, 层级, 累计用量)。
/// 有 BOM 的物料继续向下展开，无 BOM 的视为采购件。
pub fn bom_explode(db: &Db, top_code: &str, top_qty: Money) -> DbResult<Vec<BomNode>> {
    let mut out: std::collections::BTreeMap<String, BomNode> = std::collections::BTreeMap::new();
    let mut queue: std::collections::VecDeque<(String, Money, i32)> =
        std::collections::VecDeque::from([(top_code.to_string(), top_qty, 0)]);
    let mut guard = 0usize;
    while let Some((code, qty, level)) = queue.pop_front() {
        guard += 1;
        if guard > 10_000 {
            return Err(fincore::FinError::msg("BOM 展开超过 10000 节点，疑似循环引用").into());
        }
        let e = out.entry(code.clone()).or_insert(BomNode {
            code: code.clone(),
            level,
            qty: Money::ZERO,
        });
        e.qty += qty;
        let children = bom_list(db, &code)?;
        if children.is_empty() {
            continue;
        }
        for ch in children {
            let eff = ch.qty * (Money::ONE + ch.loss_rate);
            let need = (qty * eff).round_dp(fincore::money::QTY_DP);
            queue.push_back((ch.child_code, need, level + 1));
        }
    }
    let mut v: Vec<BomNode> = out.into_values().collect();
    v.sort_by(|a, b| a.level.cmp(&b.level).then(a.code.cmp(&b.code)));
    Ok(v)
}

/// BOM 成本汇总：按参考成本（存货档案 props.ref_cost）逐层累加物料成本。
pub fn bom_cost_rollup(db: &Db, top_code: &str, top_qty: Money) -> DbResult<Money> {
    let nodes = bom_explode(db, top_code, top_qty)?;
    let mut total = Money::ZERO;
    for n in nodes {
        if n.code == top_code {
            continue; // 顶层成本 = 各子件成本之和
        }
        let ref_cost = item_ref_cost(db, &n.code)?;
        total += (n.qty * ref_cost).round2();
    }
    Ok(total)
}

fn item_ref_cost(db: &Db, item_code: &str) -> DbResult<Money> {
    let props: Option<String> = db
        .conn()
        .query_row(
            "SELECT props_json FROM aux_entity WHERE kind='item' AND code=?1",
            [item_code],
            |r| r.get(0),
        )
        .optional()?;
    let Some(props) = props else {
        return Ok(Money::ZERO);
    };
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&props).unwrap_or_default();
    Ok(map
        .get("ref_cost")
        .map(|s| Money::parse_or_zero(s))
        .unwrap_or(Money::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    /// 采购订单的执行进度（已收数量/金额）必须从 po_receipt 汇总，不能读冗余列
    ///
    /// 回归背景：`purchase_order.received_amount` / `po_line.qty_received` 建表后
    /// **从未被任何代码回写**（只被读和初始化），到货只往 po_receipt 插行。于是
    /// 采购订单列表显示「已收 0」却同时显示状态「已入库完成」——**自相矛盾**，
    /// 而这正是采购订单最核心的执行进度字段。
    ///
    /// 与销售侧 `so_list` 是同一个毛病，两边都要修。
    #[test]
    fn po_list_progress_comes_from_receipts() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = PurchaseOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "S001",
            "供应商A",
            "u1",
        );
        po.no = po_next_no(&db, p).unwrap();
        po.lines.push(PoLine {
            id: 0,
            po_id: 0,
            item_code: "140301".to_string(),
            item_name: "原材料A".to_string(),
            qty_ordered: m("100"),
            qty_received: Money::ZERO,
            unit_price: m("10"),
            tax_rate: m("0.13"),
            amount: m("1000"),
            tax_amount: m("130"),
            memo: String::new(),
        });
        let id = po_save(&db, &mut po).unwrap();
        po_set_status(&db, id, PoStatus::Confirmed).unwrap();

        // 未到货：进度必须是 0（不是「未初始化」）
        let got = po_list(&db, p, None).unwrap();
        assert_eq!(
            got[0].received_amount,
            Money::ZERO,
            "没到货时不该有已收金额"
        );
        assert_eq!(got[0].lines[0].qty_received, Money::ZERO);

        // 到货 40 件 → 已收 40，已收金额 = 1130 × 40% = 452
        db.conn()
            .execute(
                "INSERT INTO po_receipt(po_id, period, date, qty, memo) VALUES(?1,?2,'2026-01-08',40,'')",
                rusqlite::params![id, p.ymm()],
            )
            .unwrap();
        crate::procurement::refresh_po_status(&db, id).unwrap();
        let got = po_list(&db, p, None).unwrap();
        assert_eq!(
            got[0].lines[0].qty_received,
            m("40"),
            "行级已收数量应汇总到 40"
        );
        assert_eq!(
            got[0].received_amount,
            m("452"),
            "已收金额 = 价税合计 1130 × 40% = 452"
        );
        assert_eq!(
            got[0].status,
            PoStatus::PartialIn,
            "部分到货应是「部分入库」"
        );

        // 再到 60 → 累计 100 = 全量
        db.conn()
            .execute(
                "INSERT INTO po_receipt(po_id, period, date, qty, memo) VALUES(?1,?2,'2026-01-12',60,'')",
                rusqlite::params![id, p.ymm()],
            )
            .unwrap();
        crate::procurement::refresh_po_status(&db, id).unwrap();
        let got = po_list(&db, p, None).unwrap();
        assert_eq!(got[0].lines[0].qty_received, m("100"));
        assert_eq!(
            got[0].received_amount,
            m("1130"),
            "全量到货金额 = 订单价税合计"
        );
        assert_eq!(got[0].status, PoStatus::Completed);
    }

    /// 超收如实反映：到货超过订购量时，已收金额会大于订单金额
    ///
    /// 这**不是 bug**：`po_receipt_with_stock` 刻意不封顶（超收在实务里常见），
    /// 库存与到货金额都按实际到货入账。锁住这个行为是为了防止有人"顺手加个封顶"
    /// 把真实的超收挡掉——那样账实会不符。
    #[test]
    fn po_over_receipt_is_reflected_not_truncated() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = PurchaseOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "S001",
            "供应商A",
            "u1",
        );
        po.no = po_next_no(&db, p).unwrap();
        po.lines.push(PoLine {
            id: 0,
            po_id: 0,
            item_code: "140301".to_string(),
            item_name: "原材料A".to_string(),
            qty_ordered: m("100"),
            qty_received: Money::ZERO,
            unit_price: m("10"),
            tax_rate: m("0.13"),
            amount: m("1000"),
            tax_amount: m("130"),
            memo: String::new(),
        });
        let id = po_save(&db, &mut po).unwrap();
        db.conn()
            .execute(
                "INSERT INTO po_receipt(po_id, period, date, qty, memo) VALUES(?1,?2,'2026-01-08',120,'')",
                rusqlite::params![id, p.ymm()],
            )
            .unwrap();
        crate::procurement::refresh_po_status(&db, id).unwrap();
        let got = po_list(&db, p, None).unwrap();
        assert_eq!(got[0].lines[0].qty_received, m("120"), "超收不该被截到 100");
        assert_eq!(
            got[0].received_amount,
            m("1356"),
            "超收金额 = 1130 × 120% = 1356，应如实大于订单金额"
        );
    }

    /// ATP = 现有可用 + 在途 − 已占用
    ///
    /// 三个分量分别断言，而不是只断言结果：只看一个 ATP 数字，
    /// 被算错了也不知道错在哪一项。
    #[test]
    fn atp_is_onhand_plus_incoming_minus_committed() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();

        // 现有库存 20
        for _ in 0..2 {
            let q = m("10");
            crate::business::stock_insert(
                &db,
                &crate::business::StockMove {
                    id: 0,
                    period: p,
                    biz_date: d,
                    kind: crate::business::StockKind::Purchase,
                    item: "A".into(),
                    warehouse: String::new(),
                    batch_no: String::new(),
                    qty: q,
                    price: m("1"),
                    amount: m("10"),
                    voucher_id: None,
                    memo: String::new(),
                },
            )
            .unwrap();
        }

        // 一张已确认的销售订单要 30 件（占用 30），一张草稿要 5 件（不占用）
        mk_so(&db, p, d, "Confirmed", "30", "A");
        mk_so(&db, p, d, "Draft", "5", "A");

        // 一张生产订单在产 15 件（在途 +15）
        mk_prod(&db, p, d, "15", ProdStatus::InProgress);

        let a = atp(&db, "A").unwrap();
        assert_eq!(a.on_hand, m("20"), "现有可用库存");
        assert_eq!(a.committed, m("30"), "只算已确认订单，草稿不占用");
        assert_eq!(a.incoming, m("15"), "未完工生产计划算在途");
        assert_eq!(a.atp, m("5"), "20 + 15 − 30 = 5");
    }

    /// 待检库存不计入 ATP
    ///
    /// 来料检验未转正的货**不可领用**，算进 ATP 就是对客户虚承诺交期。
    #[test]
    fn atp_excludes_qc_pending_stock() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let id = crate::business::stock_insert(
            &db,
            &crate::business::StockMove {
                id: 0,
                period: p,
                biz_date: d,
                kind: crate::business::StockKind::Purchase,
                item: "A".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: m("50"),
                price: m("1"),
                amount: m("50"),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        assert_eq!(atp(&db, "A").unwrap().on_hand, m("50"));
        db.conn()
            .execute(
                "UPDATE stock_move SET qc_status='pending' WHERE id=?1",
                [id],
            )
            .unwrap();
        let a = atp(&db, "A").unwrap();
        assert_eq!(a.on_hand, Money::ZERO, "待检不可领用，不能算进可承诺量");
        assert_eq!(a.atp, Money::ZERO);
    }

    /// 已完工 / 已作废的生产订单不再算在途；已发货/作废的销售订单不再占用
    #[test]
    fn atp_excludes_terminal_states() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        mk_prod(&db, p, d, "20", ProdStatus::Completed);
        mk_prod(&db, p, d, "30", ProdStatus::Cancelled);
        // Completed 的订单必须已发完（否则夹具本身不真实：状态说发完了、
        // 发货记录却是 0，ATP 会把这 40 件算成还占着）
        mk_so_ship(&db, p, d, "Completed", "40", "40", "A");
        mk_so(&db, p, d, "Cancelled", "50", "A");
        let a = atp(&db, "A").unwrap();
        assert_eq!(a.incoming, Money::ZERO, "完工/作废的产单不是在途");
        assert_eq!(a.committed, Money::ZERO, "已发货完/作废的订单不占用");
    }

    /// 销售订单下推生产订单：数量封顶到未下推余量，且产销两单在数据上连着
    ///
    /// 回归背景：`production_order` 以前**没有 so_id**，产出的货与谁要它没有任何
    /// 记录对应，「这批货是哪张订单要的」只能靠人肉记忆。
    #[test]
    fn prod_push_from_sales_order_is_capped_and_linked() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let so_id = mk_so(&db, p, d, "Confirmed", "100", "A");

        // 第一次下推 → 全量 100
        let p1 = prod_from_so(&db, so_id, p, d, None, "u1").unwrap();
        let o1 = prod_get(&db, p1).unwrap().unwrap();
        assert_eq!(o1.so_id, so_id, "生产订单必须记住来源销售订单");
        assert_eq!(o1.planned_qty, m("100"));
        assert!(o1.memo.contains("下推自销售订单"), "{}", o1.memo);

        // 再下推 → 已全部认领，拒绝
        assert!(
            prod_from_so(&db, so_id, p, d, None, "u1").is_err(),
            "全部下推后不该还能再推（否则产销对不上）"
        );

        // 部分发货后再新建一张订单，验证「订购 − 已发 − 已下推」的封顶口径
        let so2 = mk_so(&db, p, d, "Confirmed", "100", "A");
        let p2 = prod_from_so(&db, so2, p, d, Some(m("30")), "u1").unwrap();
        assert_eq!(prod_get(&db, p2).unwrap().unwrap().planned_qty, m("30"));
        // 再申请 999 → 封顶到 70
        let p3 = prod_from_so(&db, so2, p, d, Some(m("999")), "u1").unwrap();
        assert_eq!(
            prod_get(&db, p3).unwrap().unwrap().planned_qty,
            m("70"),
            "申请 999 只能下推未认领的 70"
        );
        // 作废一张后，那部分额度回来
        db.conn()
            .execute(
                "UPDATE production_order SET status='cancelled' WHERE id=?1",
                [p3],
            )
            .unwrap();
        let p4 = prod_from_so(&db, so2, p, d, None, "u1").unwrap();
        assert_eq!(prod_get(&db, p4).unwrap().unwrap().planned_qty, m("70"));
    }

    /// 草稿 / 作废的销售订单不能下推生产
    #[test]
    fn prod_push_rejects_draft_and_cancelled_sales_order() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        for st in ["Draft", "Cancelled"] {
            let id = mk_so(&db, p, d, st, "10", "A");
            assert!(
                prod_from_so(&db, id, p, d, None, "u1").is_err(),
                "{st} 状态的销售订单不该能下推生产"
            );
        }
    }

    /// 在途必须分成「已排期 / 未排期」，未排期的不能用来承诺交期
    ///
    /// 仓里**没有工作中心产能数据**（`work_center` 是自由文本），所以系统编不出
    /// 真实完工日 —— 只有**计划员自己排的** `plan_end` 才是有依据的日期。
    ///
    /// 所以合成一个「在途 500」是危险的：其中 300 有排期、200 没有，
    /// 而那 200 根本给不出交期。合成一个数，销售拿去承诺就变成了编的。
    #[test]
    fn atp_separates_dated_and_undated_incoming() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        // 300 已排期（1 月 20 日完工），200 没排期
        let a1 = mk_prod(&db, p, d, "300", ProdStatus::InProgress);
        let _a2 = mk_prod(&db, p, d, "200", ProdStatus::InProgress);
        db.conn()
            .execute(
                "UPDATE production_order SET plan_start='2026-01-10', plan_end='2026-01-20' WHERE id=?1",
                [a1],
            )
            .unwrap();

        let r = atp(&db, "A").unwrap();
        assert_eq!(r.incoming, m("500"), "总数不变");
        assert_eq!(r.incoming_dated, m("300"), "只有排了期的才算有交期");
        assert_eq!(r.incoming_undated, m("200"), "没排期的要单列出来");
        assert_eq!(r.earliest_ready, "2026-01-20", "最早完工日");
    }

    /// 「到某日为止可承诺量」只认有日期保证的那部分
    #[test]
    fn atp_until_only_counts_incoming_scheduled_by_that_date() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        // 现货 100
        crate::business::stock_insert(
            &db,
            &crate::business::StockMove {
                id: 0,
                period: p,
                biz_date: d,
                kind: crate::business::StockKind::Purchase,
                item: "A".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: m("100"),
                price: m("1"),
                amount: m("100"),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        // 300 在 1/20 完工，200 没排期
        let a1 = mk_prod(&db, p, d, "300", ProdStatus::InProgress);
        let _a2 = mk_prod(&db, p, d, "200", ProdStatus::InProgress);
        db.conn()
            .execute(
                "UPDATE production_order SET plan_start='2026-01-10', plan_end='2026-01-20' WHERE id=?1",
                [a1],
            )
            .unwrap();

        // 1/15 之前：只有现货（1/20 完工的那 300 还不算数）
        let early = atp_until(&db, "A", NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()).unwrap();
        assert_eq!(early.incoming, Money::ZERO, "1/20 完工的量在 1/15 不可承诺");
        assert_eq!(early.atp, m("100"), "1/15 只能承诺现货 100");

        // 1/20 当天：300 算进来；没排期的 200 仍然不算
        let later = atp_until(&db, "A", NaiveDate::from_ymd_opt(2026, 1, 20).unwrap()).unwrap();
        assert_eq!(later.incoming, m("300"), "1/20 完工的量在 1/20 可承诺");
        assert_eq!(
            later.atp,
            m("400"),
            "现货 100 + 有日期的在途 300；未排期的 200 不给日期就不承诺"
        );
        // 原始口径仍然保留在字段里，界面要能同时说「总量」和「有保证的量」
        assert_eq!(
            later.incoming_undated,
            m("200"),
            "未排期部分不能被这个口径吞掉"
        );
    }

    /// 排期校验：格式不对、完工早于开工都必须被拒
    ///
    /// 回归背景：`prod_schedule` 原来是**裸写** —— 连日期格式都不查，于是
    /// 「完工日早于开工日」这种自相矛盾的排期能直接进库。而 ATP 的日期承诺
    /// 要拿 `plan_end` 当依据：排期错了，承诺日期就是错的，且没有任何提示。
    #[test]
    fn prod_schedule_rejects_bad_dates() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let id = mk_prod(&db, p, d, "10", ProdStatus::Released);

        // 完工早于开工
        let e = prod_schedule(
            &db,
            &[(id, "2026-01-20".to_string(), "2026-01-10".to_string())],
        )
        .unwrap_err();
        assert!(format!("{e:?}").contains("早于开工日"), "实际 {:?}", e);

        // 格式不对
        let e = prod_schedule(
            &db,
            &[(id, "2026/01/10".to_string(), "2026-01-20".to_string())],
        )
        .unwrap_err();
        assert!(format!("{e:?}").contains("格式"), "实际 {:?}", e);

        // 整批校验：后面有一行不合法，前面那行也不能落库（静默跳过一半更难排查）
        let ok_id = mk_prod(&db, p, d, "20", ProdStatus::Released);
        let read = |id: i64| -> String {
            db.conn()
                .query_row(
                    "SELECT COALESCE(plan_start,'') FROM production_order WHERE id=?1",
                    [id],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let before = read(ok_id);
        assert!(prod_schedule(
            &db,
            &[
                (ok_id, "2026-01-10".to_string(), "2026-01-20".to_string()),
                (id, "2026-01-25".to_string(), "2026-01-20".to_string()),
            ]
        )
        .is_err());
        assert_eq!(before, read(ok_id), "整批失败时不该有半批落库");

        // 合法排期能写进去
        assert_eq!(
            prod_schedule(
                &db,
                &[(ok_id, "2026-01-10".to_string(), "2026-01-20".to_string())]
            )
            .unwrap(),
            1
        );
    }

    /// 下推生产订单把 ATP 从「欠」拉到「够」——这正是 ATP 存在的意义
    ///
    /// 断言的是**变化**而不是某一时刻的绝对值：承诺之前就能看到
    /// 「现货不够，但下推生产之后够了」，而不是只能靠事后看库存。
    #[test]
    fn atp_reflects_newly_pushed_production() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let so_id = mk_so(&db, p, d, "Confirmed", "60", "A");

        // 下推之前：无现货、无在途，而订单已占用 60 → ATP 为负（承诺不了）
        let before = atp(&db, "A").unwrap();
        assert_eq!(before.on_hand, Money::ZERO);
        assert_eq!(before.incoming, Money::ZERO);
        assert_eq!(before.committed, m("60"));
        assert_eq!(
            before.atp,
            m("-60"),
            "没有现货也没有在途时不该承诺得了 60 件"
        );

        // 下推 60 件生产
        prod_from_so(&db, so_id, p, d, None, "u1").unwrap();
        let after = atp(&db, "A").unwrap();
        assert_eq!(after.incoming, m("60"), "下推后在途 +60");
        assert_eq!(after.committed, m("60"), "占用不变（订单还是已确认）");
        assert_eq!(
            after.atp,
            Money::ZERO,
            "在途抵掉占用后才够承诺——这才是「可承诺量」"
        );
    }

    fn mk_so(db: &Db, p: Period, d: NaiveDate, status: &str, qty: &str, item: &str) -> i64 {
        mk_so_ship(db, p, d, status, qty, "0", item)
    }

    /// 造销售订单；`ship` = 已发货数量（Completed 状态必须已发完，否则夹具本身不真实）
    fn mk_so_ship(
        db: &Db,
        p: Period,
        d: NaiveDate,
        status: &str,
        qty: &str,
        ship: &str,
        item: &str,
    ) -> i64 {
        let mut so = SalesOrder::new(p, d, "C001", "客户B", "u1");
        so.no = so_next_no(db, p).unwrap();
        so.lines.push(SoLine {
            id: 0,
            so_id: 0,
            item_code: item.to_string(),
            item_name: "成品".to_string(),
            qty_ordered: Money::parse(qty).unwrap(),
            qty_shipped: Money::ZERO,
            unit_price: m("10"),
            tax_rate: m("0.13"),
            amount: Money::parse(qty).unwrap() * m("10"),
            tax_amount: Money::ZERO,
            memo: String::new(),
        });
        so.status = status_from::<SoStatus>(status).unwrap_or(SoStatus::Draft);
        let id = so_save(db, &mut so).unwrap();
        if so.status != SoStatus::Draft {
            so_set_status(db, id, so.status).unwrap();
        }
        if !Money::parse(ship).unwrap().is_zero() {
            db.conn()
                .execute(
                    "INSERT INTO so_shipment(so_id, period, date, qty, memo) VALUES(?1,?2,?3,?4,'')",
                    rusqlite::params![id, p.ymm(), d.to_string(), ship],
                )
                .unwrap();
        }
        id
    }

    fn mk_prod(db: &Db, p: Period, d: NaiveDate, qty: &str, status: ProdStatus) -> i64 {
        let mut o = ProductionOrder {
            id: 0,
            no: prod_next_no(db, p).unwrap(),
            period: p,
            date: d,
            item_code: "A".into(),
            item_name: "成品".into(),
            planned_qty: Money::parse(qty).unwrap(),
            completed_qty: Money::ZERO,
            status: ProdStatus::Released,
            work_center: String::new(),
            so_id: 0,
            prepared_by: "u1".into(),
            memo: String::new(),
            order_kind: "inhouse".into(),
            supplier_code: String::new(),
            supplier_name: String::new(),
            plan_start: String::new(),
            plan_end: String::new(),
        };
        let id = prod_save(db, &mut o).unwrap();
        if status != ProdStatus::Released {
            db.conn()
                .execute(
                    "UPDATE production_order SET status=?2 WHERE id=?1",
                    rusqlite::params![id, status.code()],
                )
                .unwrap();
        }
        id
    }

    #[test]
    fn po_crud() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = PurchaseOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "S001",
            "供应商A",
            "u1",
        );
        po.no = po_next_no(&db, p).unwrap();
        po.lines.push(PoLine {
            id: 0,
            po_id: 0,
            item_code: "140301".to_string(),
            item_name: "原材料A".to_string(),
            qty_ordered: Money::parse("100").unwrap(),
            qty_received: Money::ZERO,
            unit_price: Money::parse("10").unwrap(),
            tax_rate: Money::parse("0.13").unwrap(),
            amount: Money::parse("1000").unwrap(),
            tax_amount: Money::parse("130").unwrap(),
            memo: String::new(),
        });
        let id = po_save(&db, &mut po).unwrap();
        assert!(id > 0);
        let list = po_list(&db, p, None).unwrap();
        assert_eq!(list.len(), 1);
        po_delete(&db, id).unwrap();
        assert_eq!(po_list(&db, p, None).unwrap().len(), 0);
    }

    #[test]
    fn so_crud() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut so = SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
            "C001",
            "客户B",
            "u1",
        );
        so.no = so_next_no(&db, p).unwrap();
        so.lines.push(SoLine {
            id: 0,
            so_id: 0,
            item_code: "140301".to_string(),
            item_name: "原材料A".to_string(),
            qty_ordered: Money::parse("50").unwrap(),
            qty_shipped: Money::ZERO,
            unit_price: Money::parse("12").unwrap(),
            tax_rate: Money::parse("0.13").unwrap(),
            amount: Money::parse("600").unwrap(),
            tax_amount: Money::parse("78").unwrap(),
            memo: String::new(),
        });
        let id = so_save(&db, &mut so).unwrap();
        assert!(id > 0);
        so_delete(&db, id).unwrap();
    }

    /// 回归：状态流转不得把明细整表读出再写回。
    /// 旧实现 `so_get → 改状态 → so_save`，`so_save` 会 DELETE+重插全部 so_line，
    /// 期间并发记上的 qty_shipped 会被这轮回滚覆盖。
    #[test]
    fn so_set_status_keeps_concurrent_line_progress() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut so = SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
            "C001",
            "客户B",
            "u1",
        );
        so.no = so_next_no(&db, p).unwrap();
        so.lines.push(SoLine {
            id: 0,
            so_id: 0,
            item_code: "140301".to_string(),
            item_name: "原材料A".to_string(),
            qty_ordered: m("50"),
            qty_shipped: Money::ZERO,
            unit_price: m("12"),
            tax_rate: m("0.13"),
            amount: m("600"),
            tax_amount: m("78"),
            memo: String::new(),
        });
        let id = so_save(&db, &mut so).unwrap();
        so_set_status(&db, id, SoStatus::Confirmed).unwrap();
        // 模拟并发出货：直接写 qty_shipped（发货回写走 sales.rs，不在本文件）
        db.conn()
            .execute(
                "UPDATE so_line SET qty_shipped=?2 WHERE so_id=?1",
                rusqlite::params![id, "20"],
            )
            .unwrap();
        so_set_status(&db, id, SoStatus::Cancelled).unwrap();
        let got = so_get(&db, id).unwrap().unwrap();
        assert_eq!(got.status, SoStatus::Cancelled);
        assert_eq!(
            got.lines[0].qty_shipped,
            m("20"),
            "状态流转不得回滚明细上的并发进度（回归前会被 so_save 清成 0）"
        );
    }

    /// 回归：非法状态流转必须被状态机拦下。
    /// 旧实现什么状态都能改（已完成可回草稿、已作废可复活）。
    #[test]
    fn so_set_status_rejects_illegal_transitions() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut so = SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
            "C001",
            "客户B",
            "u1",
        );
        so.no = so_next_no(&db, p).unwrap();
        so.lines.push(SoLine {
            id: 0,
            so_id: 0,
            item_code: "140301".to_string(),
            item_name: "原材料A".to_string(),
            qty_ordered: m("50"),
            qty_shipped: Money::ZERO,
            unit_price: m("12"),
            tax_rate: m("0.13"),
            amount: m("600"),
            tax_amount: m("78"),
            memo: String::new(),
        });
        let id = so_save(&db, &mut so).unwrap();
        // 草稿不能直接跳到已完成
        assert!(so_set_status(&db, id, SoStatus::Completed).is_err());
        // 作废是终态，不能复活
        so_set_status(&db, id, SoStatus::Cancelled).unwrap();
        assert!(so_set_status(&db, id, SoStatus::Confirmed).is_err());
        assert!(so_set_status(&db, id, SoStatus::Draft).is_err());
        assert_eq!(
            so_get(&db, id).unwrap().unwrap().status,
            SoStatus::Cancelled
        );
        // 订单不存在要报错
        assert!(so_set_status(&db, 999_999, SoStatus::Confirmed).is_err());
    }

    /// 回归：`so_progress_update` 是「由发货流水派生的状态」，只能推进、不能复活。
    /// 作废/草稿单据上挂着发货流水时，进度同步不得把它们改成"部分发货/已完成"。
    #[test]
    fn so_progress_update_never_resurrects_draft_or_cancelled() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut so = SalesOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
            "C001",
            "客户B",
            "u1",
        );
        so.no = so_next_no(&db, p).unwrap();
        so.lines.push(SoLine {
            id: 0,
            so_id: 0,
            item_code: "140301".to_string(),
            item_name: "原材料A".to_string(),
            qty_ordered: m("50"),
            qty_shipped: Money::ZERO,
            unit_price: m("12"),
            tax_rate: m("0.13"),
            amount: m("600"),
            tax_amount: m("78"),
            memo: String::new(),
        });
        let id = so_save(&db, &mut so).unwrap();
        // 造一条已发货流水：数量 20 < 订购 50 → 派生状态应是"部分发货"
        db.conn()
            .execute(
                "INSERT INTO so_shipment(so_id, period, date, qty, memo)
                 VALUES(?1, ?2, '2026-01-12', 20, '')",
                rusqlite::params![id, p.ymm()],
            )
            .unwrap();
        // 草稿：进度同步不动它（发货通知要求先确认订单）
        so_progress_update(&db, id).unwrap();
        assert_eq!(so_get(&db, id).unwrap().unwrap().status, SoStatus::Draft);
        // 确认 → 部分发货
        so_set_status(&db, id, SoStatus::Confirmed).unwrap();
        so_progress_update(&db, id).unwrap();
        assert_eq!(
            so_get(&db, id).unwrap().unwrap().status,
            SoStatus::PartialShip
        );
        // 作废 → 进度同步不得复活
        so_set_status(&db, id, SoStatus::Cancelled).unwrap();
        so_progress_update(&db, id).unwrap();
        assert_eq!(
            so_get(&db, id).unwrap().unwrap().status,
            SoStatus::Cancelled,
            "已作废订单不得被进度同步改回部分发货"
        );
    }

    /// 回归：采购订单状态流转同样不得整表回写明细、且要过状态机。
    #[test]
    fn po_set_status_is_guarded_and_keeps_lines() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = PurchaseOrder::new(
            p,
            NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            "S001",
            "供应商A",
            "u1",
        );
        po.no = po_next_no(&db, p).unwrap();
        po.lines.push(PoLine {
            id: 0,
            po_id: 0,
            item_code: "140301".to_string(),
            item_name: "原材料A".to_string(),
            qty_ordered: m("100"),
            qty_received: Money::ZERO,
            unit_price: m("10"),
            tax_rate: m("0.13"),
            amount: m("1000"),
            tax_amount: m("130"),
            memo: String::new(),
        });
        let id = po_save(&db, &mut po).unwrap();
        po_set_status(&db, id, PoStatus::Confirmed).unwrap();
        db.conn()
            .execute(
                "UPDATE po_line SET qty_received=?2 WHERE po_id=?1",
                rusqlite::params![id, "40"],
            )
            .unwrap();
        po_set_status(&db, id, PoStatus::Cancelled).unwrap();
        let got = po_get(&db, id).unwrap().unwrap();
        assert_eq!(got.status, PoStatus::Cancelled);
        assert_eq!(
            got.lines[0].qty_received,
            m("40"),
            "状态流转不得回滚并发入库量"
        );
        // 终态不可复活；不存在的单据要报错
        assert!(po_set_status(&db, id, PoStatus::Draft).is_err());
        assert!(po_set_status(&db, 999_999, PoStatus::Confirmed).is_err());
    }

    #[test]
    fn bom_crud() {
        let db = mem();
        bom_save(
            &db,
            "1001",
            &[
                (
                    "140301".to_string(),
                    Money::parse("2").unwrap(),
                    Money::parse("0.02").unwrap(),
                ),
                (
                    "140302".to_string(),
                    Money::parse("1").unwrap(),
                    Money::ZERO,
                ),
            ],
        )
        .unwrap();
        let items = bom_list(&db, "1001").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].child_code, "140301");
    }

    #[test]
    fn bom_version_and_log() {
        let db = mem();
        bom_save_version(
            &db,
            "1001",
            "v1",
            &[("140301".to_string(), m("2"), m("0"))],
            "u",
        )
        .unwrap();
        bom_save_version(
            &db,
            "1001",
            "v2",
            &[("140301".to_string(), m("3"), m("0"))],
            "u",
        )
        .unwrap();
        // 版本隔离
        assert_eq!(bom_list_version(&db, "1001", "v1").unwrap()[0].qty, m("2"));
        assert_eq!(bom_list_version(&db, "1001", "v2").unwrap()[0].qty, m("3"));
        // 变更历史
        let log = bom_change_log(&db, "1001").unwrap();
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn bom_explode_multilevel() {
        let db = mem();
        // FG = 2 × SA；SA = 3 × RM
        bom_save(&db, "FG", &[("SA".into(), m("2"), m("0"))]).unwrap();
        bom_save(&db, "SA", &[("RM".into(), m("3"), m("0"))]).unwrap();
        let nodes = bom_explode(&db, "FG", m("10")).unwrap();
        // FG 10 + SA 20 + RM 60
        let sa = nodes.iter().find(|n| n.code == "SA").unwrap();
        assert_eq!(sa.qty, m("20"));
        assert_eq!(sa.level, 1);
        let rm = nodes.iter().find(|n| n.code == "RM").unwrap();
        assert_eq!(rm.qty, m("60"));
        assert_eq!(rm.level, 2);
    }

    #[test]
    fn bom_substitute_crud() {
        let db = mem();
        bom_substitute_save(
            &db,
            &Substitute {
                id: 0,
                parent_code: "FG".into(),
                child_code: "RM".into(),
                substitute: "ALT".into(),
                ratio: m("1.2"),
                priority: 0,
            },
            "u",
        )
        .unwrap();
        let subs = bom_substitutes(&db, "FG", "RM").unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].ratio, m("1.2"));
        let id = subs[0].id;
        bom_substitute_delete(&db, id, "u").unwrap();
        assert!(bom_substitutes(&db, "FG", "RM").unwrap().is_empty());
    }
}

// ===========================================================================
// 生产订单
// ===========================================================================

pub fn prod_next_no(db: &Db, period: Period) -> DbResult<String> {
    let year = period.year();
    let month = period.month();
    let prefix = format!(
        "{}{:04}{:02}",
        crate::doc_prefix(db, "prod", "SC"),
        year,
        month
    );
    let sql = format!(
        "SELECT COALESCE(MAX(CAST(SUBSTR(no, {}) AS INTEGER)), 0) + 1 FROM production_order WHERE no LIKE ?",
        prefix.len() + 1
    );
    let n: i64 = db
        .conn()
        .query_row(&sql, [format!("{}%", prefix)], |r| r.get(0))
        .unwrap_or(0);
    Ok(format!("{}{:04}", prefix, n))
}

pub fn prod_save(db: &Db, order: &mut ProductionOrder) -> DbResult<i64> {
    let tx = db.write_tx()?;
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let id = if order.id > 0 {
        tx.execute(
            "UPDATE production_order SET period=?, date=?, item_code=?, item_name=?,
             planned_qty=?, completed_qty=?, status=?, work_center=?, so_id=?, prepared_by=?, memo=?,
             order_kind=?, supplier_code=?, supplier_name=?, plan_start=?, plan_end=?, updated_at=?
             WHERE id=?",
            rusqlite::params![
                order.period.ymm(), order.date, order.item_code, order.item_name,
                crate::exact_param(order.planned_qty), crate::exact_param(order.completed_qty),
                order.status.code(),
                order.work_center, order.so_id, order.prepared_by, order.memo,
                order.order_kind, order.supplier_code, order.supplier_name,
                order.plan_start, order.plan_end,
                now, order.id
            ],
        )?;
        order.id
    } else {
        tx.execute(
            "INSERT INTO production_order(period, no, date, item_code, item_name,
             planned_qty, completed_qty, status, work_center, so_id, prepared_by, memo,
             order_kind, supplier_code, supplier_name, plan_start, plan_end, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?18)",
            rusqlite::params![
                order.period.ymm(),
                order.no,
                order.date,
                order.item_code,
                order.item_name,
                crate::exact_param(order.planned_qty),
                crate::exact_param(order.completed_qty),
                order.status.code(),
                order.work_center,
                order.so_id,
                order.prepared_by,
                order.memo,
                order.order_kind,
                order.supplier_code,
                order.supplier_name,
                order.plan_start,
                order.plan_end,
                now
            ],
        )?;
        tx.last_insert_rowid()
    };
    order.id = id;
    tx.commit()?;
    Ok(id)
}

/// 草稿 → 已下达。条件更新（`AND status='draft'`）防并发：并发下达时，只有一个能成功，另一个拿到「不是草稿」而不是把状态覆盖掉。
pub fn prod_release(db: &Db, id: i64) -> DbResult<()> {
    let n = db.conn().execute(
        "UPDATE production_order SET status='released', updated_at=?2 WHERE id=?1 AND status='draft'",
        rusqlite::params![id, chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()],
    )?;
    if n == 0 {
        // 区分「不存在」与「不是草稿」：前者 404 更好定位，后者是状态机拒绝
        return match prod_get(db, id)? {
            None => Err(fincore::FinError::not_found("生产订单不存在").into()),
            Some(o) => Err(fincore::FinError::state(format!(
                "只有草稿状态的生产订单能下达，当前是「{}」",
                o.status.label()
            ))
            .into()),
        };
    }
    Ok(())
}

/// 按 id 取生产订单（工作流条件字段取数用；`prod_list` 只能按期间+状态筛，
/// 审批拦截器手上只有 biz_id，筛不出单张）
pub fn prod_get(db: &Db, id: i64) -> DbResult<Option<ProductionOrder>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, no, period, date, item_code, item_name, planned_qty, completed_qty,
         status, work_center, so_id, prepared_by, memo, order_kind, supplier_code, supplier_name,
         plan_start, plan_end
         FROM production_order WHERE id=?1",
    )?;
    let mut rows = stmt.query([id])?;
    let Some(r) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(ProductionOrder {
        id: r.get(0)?,
        no: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        date: r.get(3)?,
        item_code: r.get(4)?,
        item_name: r.get(5)?,
        planned_qty: Money::parse_or_zero(&r.get::<_, String>(6)?),
        completed_qty: Money::parse_or_zero(&r.get::<_, String>(7)?),
        status: prod_status_from(&r.get::<_, String>(8)?),
        work_center: r.get(9)?,
        so_id: r.get(10)?,
        prepared_by: r.get(11)?,
        memo: r.get(12)?,
        order_kind: r.get(13)?,
        supplier_code: r.get(14)?,
        supplier_name: r.get(15)?,
        plan_start: r.get(16)?,
        plan_end: r.get(17)?,
    }))
}

pub fn prod_list(
    db: &Db,
    period: Period,
    status: Option<ProdStatus>,
) -> DbResult<Vec<ProductionOrder>> {
    let sql = if let Some(_s) = status {
        "SELECT id, no, period, date, item_code, item_name, planned_qty, completed_qty,
             status, work_center, so_id, prepared_by, memo, order_kind, supplier_code, supplier_name,
             plan_start, plan_end
             FROM production_order WHERE period=? AND status=? ORDER BY date DESC, id DESC".to_string()
    } else {
        "SELECT id, no, period, date, item_code, item_name, planned_qty, completed_qty,
             status, work_center, so_id, prepared_by, memo, order_kind, supplier_code, supplier_name,
             plan_start, plan_end
             FROM production_order WHERE period=? ORDER BY date DESC, id DESC".to_string()
    };

    let mut stmt = db.conn().prepare(&sql)?;
    let rows = if let Some(s) = status {
        stmt.query_map(rusqlite::params![period.ymm(), s.code()], |r| {
            Ok(ProductionOrder {
                id: r.get(0)?,
                no: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: r.get(3)?,
                item_code: r.get(4)?,
                item_name: r.get(5)?,
                planned_qty: Money::parse_or_zero(&r.get::<_, String>(6)?),
                completed_qty: Money::parse_or_zero(&r.get::<_, String>(7)?),
                status: prod_status_from(&r.get::<_, String>(8)?),
                work_center: r.get(9)?,
                so_id: r.get(10)?,
                prepared_by: r.get(11)?,
                memo: r.get(12)?,
                order_kind: r.get(13)?,
                supplier_code: r.get(14)?,
                supplier_name: r.get(15)?,
                plan_start: r.get(16)?,
                plan_end: r.get(17)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([period.ymm()], |r| {
            Ok(ProductionOrder {
                id: r.get(0)?,
                no: r.get(1)?,
                period: Period::from_ymm(r.get(2)?),
                date: r.get(3)?,
                item_code: r.get(4)?,
                item_name: r.get(5)?,
                planned_qty: Money::parse_or_zero(&r.get::<_, String>(6)?),
                completed_qty: Money::parse_or_zero(&r.get::<_, String>(7)?),
                status: prod_status_from(&r.get::<_, String>(8)?),
                work_center: r.get(9)?,
                so_id: r.get(10)?,
                prepared_by: r.get(11)?,
                memo: r.get(12)?,
                order_kind: r.get(13)?,
                supplier_code: r.get(14)?,
                supplier_name: r.get(15)?,
                plan_start: r.get(16)?,
                plan_end: r.get(17)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

/// 细排：批量写回计划开工/完工日（仅未完工订单可排；条件更新防误写终态单）
pub fn prod_schedule(db: &Db, items: &[(i64, String, String)]) -> DbResult<usize> {
    // 先整批校验再落库：原来这里是裸写，连日期格式与 `end >= start` 都不查，
    // 于是「完工日早于开工日」这种自相矛盾的排期能直接存进库，而 ATP/交期
    // 承诺会拿它当依据 —— 排期错了，承诺日期就是错的，且没有任何提示。
    //
    // 整批校验（而不是逐行跳过）：批量排期本来就是一次操作，
    // 静默跳过一半比整体失败更难排查。
    for (id, start, end) in items {
        let s = NaiveDate::parse_from_str(start.trim(), "%Y-%m-%d").map_err(|_| {
            fincore::FinError::state(format!(
                "生产订单 {id}：开工日格式应为 YYYY-MM-DD，收到「{start}」"
            ))
        })?;
        let e = NaiveDate::parse_from_str(end.trim(), "%Y-%m-%d").map_err(|_| {
            fincore::FinError::state(format!(
                "生产订单 {id}：完工日格式应为 YYYY-MM-DD，收到「{end}」"
            ))
        })?;
        if e < s {
            return Err(fincore::FinError::state(format!(
                "生产订单 {id}：完工日 {end} 早于开工日 {s}"
            ))
            .into());
        }
    }
    let tx = db.write_tx()?;
    let now_s = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut n = 0usize;
    for (id, start, end) in items {
        n += tx.execute(
            "UPDATE production_order SET plan_start=?, plan_end=?, updated_at=?
             WHERE id=? AND status NOT IN ('completed','cancelled')",
            rusqlite::params![start, end, now_s, id],
        )?;
    }
    tx.commit()?;
    Ok(n)
}

#[cfg(test)]
mod prod_tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn prod_crud() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut order = ProductionOrder {
            id: 0,
            no: String::new(),
            period: p,
            date: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            item_code: "1001".to_string(),
            item_name: "成品A".to_string(),
            planned_qty: Money::parse("100").unwrap(),
            completed_qty: Money::ZERO,
            status: ProdStatus::Draft,
            work_center: "WC01".to_string(),
            so_id: 0,
            prepared_by: "u1".to_string(),
            memo: String::new(),
            order_kind: "inhouse".to_string(),
            supplier_code: String::new(),
            supplier_name: String::new(),
            plan_start: String::new(),
            plan_end: String::new(),
        };
        order.no = prod_next_no(&db, p).unwrap();
        let id = prod_save(&db, &mut order).unwrap();
        assert!(id > 0);

        let list = prod_list(&db, p, None).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].no, order.no);
    }
}
