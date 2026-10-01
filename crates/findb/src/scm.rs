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
pub enum PoStatus { Draft, Confirmed, PartialIn, Completed, Cancelled }

impl PoStatus {
    pub fn label(self) -> &'static str {
        match self {
            PoStatus::Draft => "草稿", PoStatus::Confirmed => "已确认",
            PoStatus::PartialIn => "部分入库", PoStatus::Completed => "已完成",
            PoStatus::Cancelled => "已作废",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PoLine {
    pub id: i64, pub po_id: i64,
    pub item_code: String, pub item_name: String,
    pub qty_ordered: Money, pub qty_received: Money,
    pub unit_price: Money, pub tax_rate: Money,
    pub amount: Money, pub tax_amount: Money, pub memo: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PurchaseOrder {
    pub id: i64, pub period: Period, pub no: String, pub date: NaiveDate,
    pub supplier_code: String, pub supplier_name: String,
    pub status: PoStatus,
    pub total_amount: Money, pub total_tax: Money, pub received_amount: Money,
    pub prepared_by: String, pub memo: String, pub lines: Vec<PoLine>,
}

impl PurchaseOrder {
    pub fn new(period: Period, date: NaiveDate, supplier_code: &str, supplier_name: &str, prepared_by: &str) -> Self {
        Self { id: 0, period, no: String::new(), date,
            supplier_code: supplier_code.to_string(), supplier_name: supplier_name.to_string(),
            status: PoStatus::Draft, total_amount: Money::ZERO, total_tax: Money::ZERO,
            received_amount: Money::ZERO, prepared_by: prepared_by.to_string(),
            memo: String::new(), lines: Vec::new(),
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
    let mut po = stmt
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

/// 采购订单状态流转（草稿 → 已确认 / 作废）
pub fn po_set_status(db: &Db, id: i64, to: PoStatus) -> DbResult<()> {
    let Some(mut po) = po_get(db, id)? else {
        return Err(FinError::not_found("采购订单").into());
    };
    po.status = to;
    po_save(db, &mut po)?;
    Ok(())
}

// ===========================================================================
// 销售订单
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum SoStatus { Draft, Confirmed, PartialShip, Completed, Cancelled }

impl SoStatus {
    pub fn label(self) -> &'static str {
        match self {
            SoStatus::Draft => "草稿", SoStatus::Confirmed => "已确认",
            SoStatus::PartialShip => "部分发货", SoStatus::Completed => "已完成",
            SoStatus::Cancelled => "已作废",
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SoLine {
    pub id: i64, pub so_id: i64,
    pub item_code: String, pub item_name: String,
    pub qty_ordered: Money, pub qty_shipped: Money,
    pub unit_price: Money, pub tax_rate: Money,
    pub amount: Money, pub tax_amount: Money, pub memo: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SalesOrder {
    pub id: i64, pub period: Period, pub no: String, pub date: NaiveDate,
    pub customer_code: String, pub customer_name: String,
    pub status: SoStatus,
    pub total_amount: Money, pub total_tax: Money, pub shipped_amount: Money,
    pub prepared_by: String, pub memo: String, pub lines: Vec<SoLine>,
}

impl SalesOrder {
    pub fn new(period: Period, date: NaiveDate, customer_code: &str, customer_name: &str, prepared_by: &str) -> Self {
        Self { id: 0, period, no: String::new(), date,
            customer_code: customer_code.to_string(), customer_name: customer_name.to_string(),
            status: SoStatus::Draft, total_amount: Money::ZERO, total_tax: Money::ZERO,
            shipped_amount: Money::ZERO, prepared_by: prepared_by.to_string(),
            memo: String::new(), lines: Vec::new(),
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
pub enum ProdStatus { Draft, Released, InProgress, Completed, Cancelled }

impl ProdStatus {
    pub fn label(self) -> &'static str {
        match self {
            ProdStatus::Draft => "草稿", ProdStatus::Released => "已下达",
            ProdStatus::InProgress => "生产中", ProdStatus::Completed => "已完工",
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
    let prefix = format!("{}{:04}{:02}", crate::doc_prefix(db, "po", "CG"), year, month);
    let sql = format!(
        "SELECT COALESCE(MAX(CAST(SUBSTR(no, {}) AS INTEGER)), 0) + 1 FROM purchase_order WHERE no LIKE ?",
        prefix.len() + 1
    );
    let n: i64 = db.conn()
        .query_row(&sql, [format!("{}%", prefix)], |r| r.get(0))
        .unwrap_or(0);
    Ok(format!("{}{:04}", prefix, n))
}

pub fn so_next_no(db: &Db, period: Period) -> DbResult<String> {
    let year = period.year();
    let month = period.month();
    let prefix = format!("{}{:04}{:02}", crate::doc_prefix(db, "so", "XS"), year, month);
    let sql = format!(
        "SELECT COALESCE(MAX(CAST(SUBSTR(no, {}) AS INTEGER)), 0) + 1 FROM sales_order WHERE no LIKE ?",
        prefix.len() + 1
    );
    let n: i64 = db.conn()
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
                serde_json::to_value(&po.status)?.as_str().unwrap(),
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
                serde_json::to_value(&po.status)?.as_str().unwrap(),
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
                id, line.item_code, line.item_name, crate::exact_param(line.qty_ordered),
                crate::exact_param(line.qty_received), crate::exact_param(line.unit_price),
                crate::exact_param(line.tax_rate), crate::money_param(line.amount),
                crate::money_param(line.tax_amount), line.memo
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
    let sql = if let Some(s) = status {
        format!(
            "SELECT id, period, no, date, supplier_code, supplier_name, status,
             total_amount, total_tax, received_amount, prepared_by, memo
             FROM purchase_order WHERE period=? AND status=? ORDER BY date DESC, id DESC"
        )
    } else {
        format!(
            "SELECT id, period, no, date, supplier_code, supplier_name, status,
             total_amount, total_tax, received_amount, prepared_by, memo
             FROM purchase_order WHERE period=? ORDER BY date DESC, id DESC"
        )
    };
    
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = if let Some(s) = status {
        stmt.query_map(rusqlite::params![period.ymm(), serde_json::to_value(&s)?.as_str().unwrap()], |r| {
            Ok(PurchaseOrder {
                id: r.get(0)?, period: Period::from_ymm(r.get(1)?),
                no: r.get(2)?, date: r.get(3)?,
                supplier_code: r.get(4)?, supplier_name: r.get(5)?,
                status: status_from(&r.get::<_, String>(6)?).unwrap_or(PoStatus::Draft),
                total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                received_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                prepared_by: r.get(10)?, memo: r.get(11)?,
                lines: Vec::new(),
            })
        })?.collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([period.ymm()], |r| {
            Ok(PurchaseOrder {
                id: r.get(0)?, period: Period::from_ymm(r.get(1)?),
                no: r.get(2)?, date: r.get(3)?,
                supplier_code: r.get(4)?, supplier_name: r.get(5)?,
                status: status_from(&r.get::<_, String>(6)?).unwrap_or(PoStatus::Draft),
                total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                received_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                prepared_by: r.get(10)?, memo: r.get(11)?,
                lines: Vec::new(),
            })
        })?.collect::<Result<Vec<_>, _>>()?
    };
    
    let mut orders = Vec::new();
    for mut po in rows {
        let mut stmt = db.conn().prepare(
            "SELECT id, item_code, item_name, qty_ordered, qty_received,
             unit_price, tax_rate, amount, tax_amount, memo
             FROM po_line WHERE po_id=? ORDER BY id"
        )?;
        let lines = stmt.query_map([po.id], |r| Ok(PoLine {
            id: r.get(0)?, po_id: po.id,
            item_code: r.get(1)?, item_name: r.get(2)?,
            qty_ordered: Money::parse_or_zero(&r.get::<_, String>(3)?),
            qty_received: Money::parse_or_zero(&r.get::<_, String>(4)?),
            unit_price: Money::parse_or_zero(&r.get::<_, String>(5)?),
            tax_rate: Money::parse_or_zero(&r.get::<_, String>(6)?),
            amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
            tax_amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
            memo: r.get(9)?,
        }))?.collect::<Result<Vec<_>, _>>()?;
        po.lines = lines;
        orders.push(po);
    }
    Ok(orders)
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
                serde_json::to_value(&so.status)?.as_str().unwrap(),
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
                serde_json::to_value(&so.status)?.as_str().unwrap(),
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
                id, line.item_code, line.item_name, crate::exact_param(line.qty_ordered),
                crate::exact_param(line.qty_shipped), crate::exact_param(line.unit_price),
                crate::exact_param(line.tax_rate), crate::money_param(line.amount),
                crate::money_param(line.tax_amount), line.memo
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
    let sql = if let Some(s) = status {
        format!(
            "SELECT id, period, no, date, customer_code, customer_name, status,
             total_amount, total_tax, shipped_amount, prepared_by, memo
             FROM sales_order WHERE period=? AND status=? ORDER BY date DESC, id DESC"
        )
    } else {
        format!(
            "SELECT id, period, no, date, customer_code, customer_name, status,
             total_amount, total_tax, shipped_amount, prepared_by, memo
             FROM sales_order WHERE period=? ORDER BY date DESC, id DESC"
        )
    };
    
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = if let Some(s) = status {
        stmt.query_map(rusqlite::params![period.ymm(), serde_json::to_value(&s)?.as_str().unwrap()], |r| {
            Ok(SalesOrder {
                id: r.get(0)?, period: Period::from_ymm(r.get(1)?),
                no: r.get(2)?, date: r.get(3)?,
                customer_code: r.get(4)?, customer_name: r.get(5)?,
                status: status_from(&r.get::<_, String>(6)?).unwrap_or(SoStatus::Draft),
                total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                shipped_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                prepared_by: r.get(10)?, memo: r.get(11)?,
                lines: Vec::new(),
            })
        })?.collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([period.ymm()], |r| {
            Ok(SalesOrder {
                id: r.get(0)?, period: Period::from_ymm(r.get(1)?),
                no: r.get(2)?, date: r.get(3)?,
                customer_code: r.get(4)?, customer_name: r.get(5)?,
                status: status_from(&r.get::<_, String>(6)?).unwrap_or(SoStatus::Draft),
                total_amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
                total_tax: Money::parse_or_zero(&r.get::<_, String>(8)?),
                shipped_amount: Money::parse_or_zero(&r.get::<_, String>(9)?),
                prepared_by: r.get(10)?, memo: r.get(11)?,
                lines: Vec::new(),
            })
        })?.collect::<Result<Vec<_>, _>>()?
    };
    
    let mut orders = Vec::new();
    for mut so in rows {
        let mut stmt = db.conn().prepare(
            "SELECT id, item_code, item_name, qty_ordered, qty_shipped,
             unit_price, tax_rate, amount, tax_amount, memo
             FROM so_line WHERE so_id=? ORDER BY id"
        )?;
        let lines = stmt.query_map([so.id], |r| Ok(SoLine {
            id: r.get(0)?, so_id: so.id,
            item_code: r.get(1)?, item_name: r.get(2)?,
            qty_ordered: Money::parse_or_zero(&r.get::<_, String>(3)?),
            qty_shipped: Money::parse_or_zero(&r.get::<_, String>(4)?),
            unit_price: Money::parse_or_zero(&r.get::<_, String>(5)?),
            tax_rate: Money::parse_or_zero(&r.get::<_, String>(6)?),
            amount: Money::parse_or_zero(&r.get::<_, String>(7)?),
            tax_amount: Money::parse_or_zero(&r.get::<_, String>(8)?),
            memo: r.get(9)?,
        }))?.collect::<Result<Vec<_>, _>>()?;
        so.lines = lines;
        orders.push(so);
    }
    Ok(orders)
}

/// 按 id 取销售订单（含明细）
pub fn so_get(db: &Db, id: i64) -> DbResult<Option<SalesOrder>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, period, no, date, customer_code, customer_name, status,
                total_amount, total_tax, shipped_amount, prepared_by, memo
         FROM sales_order WHERE id=?1",
    )?;
    let mut so = stmt
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

/// 订单状态流转（草稿 → 已确认 / 作废）：信用检查在 so_save 内把关
pub fn so_set_status(db: &Db, id: i64, to: SoStatus) -> DbResult<()> {
    let Some(mut so) = so_get(db, id)? else {
        return Err(FinError::not_found("销售订单").into());
    };
    so.status = to;
    so_save(db, &mut so)?;
    Ok(())
}

/// 发货流水变化后同步订单状态：已确认 → 部分发货 / 已完成（不动草稿与作废）
pub fn so_progress_update(db: &Db, id: i64) -> DbResult<()> {
    let Some(so) = so_get(db, id)? else {
        return Ok(());
    };
    if !matches!(so.status, SoStatus::Confirmed | SoStatus::PartialShip | SoStatus::Completed) {
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
        db.conn().execute(
            "UPDATE sales_order SET status=?2 WHERE id=?1",
            rusqlite::params![id, serde_json::to_value(to)?.as_str().unwrap()],
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
         FROM bom WHERE parent_code=?1 AND version=?2 ORDER BY seq"
    )?;
    let rows = stmt.query_map(rusqlite::params![parent_code, version], |r| Ok(BomItem {
        id: r.get(0)?,
        parent_code: r.get(1)?,
        child_code: r.get(2)?,
        qty: Money::parse_or_zero(&r.get::<_, String>(3)?),
        loss_rate: Money::parse_or_zero(&r.get::<_, String>(4)?),
        seq: r.get(5)?,
    }))?;
    let mut items = Vec::new();
    for item in rows { items.push(item?); }
    Ok(items)
}

pub fn bom_save(db: &Db, parent_code: &str, children: &[(String, Money, Money)]) -> DbResult<()> {
    bom_save_version(db, parent_code, "", children, "")
}

/// 带版本保存 BOM，并记录变更历史
pub fn bom_save_version(db: &Db, parent_code: &str, version: &str, children: &[(String, Money, Money)], who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM bom WHERE parent_code=?1 AND version=?2", rusqlite::params![parent_code, version])?;
    for (i, (child_code, qty, loss_rate)) in children.iter().enumerate() {
        tx.execute(
            "INSERT INTO bom(parent_code, child_code, version, qty, loss_rate, seq) VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![parent_code, child_code, version, crate::exact_param(*qty), crate::exact_param(*loss_rate), i as i32]
        )?;
    }
    bom_log_tx(&tx, parent_code, "save", &format!("版本 {}，{} 个子件", version, children.len()), who)?;
    tx.commit()?;
    Ok(())
}

pub fn bom_delete(db: &Db, parent_code: &str, version: &str, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    tx.execute("DELETE FROM bom WHERE parent_code=?1 AND version=?2", rusqlite::params![parent_code, version])?;
    bom_log_tx(&tx, parent_code, "delete", &format!("版本 {}", version), who)?;
    tx.commit()?;
    Ok(())
}

fn bom_log_tx(tx: &rusqlite::Transaction, parent_code: &str, action: &str, detail: &str, who: &str) -> DbResult<()> {
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
pub fn bom_change_log(db: &Db, parent_code: &str) -> DbResult<Vec<(String, String, String, String)>> {
    let mut stmt = db.conn().prepare(
        "SELECT action, detail, changed_by, changed_at FROM bom_change_log
         WHERE parent_code=?1 ORDER BY id DESC"
    )?;
    let rows = stmt.query_map([parent_code], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    let mut out = Vec::new();
    for r in rows { out.push(r?); }
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
         FROM bom_substitute WHERE parent_code=?1 AND child_code=?2 ORDER BY priority"
    )?;
    let rows = stmt.query_map(rusqlite::params![parent_code, child_code], |r| Ok(Substitute {
        id: r.get(0)?,
        parent_code: r.get(1)?,
        child_code: r.get(2)?,
        substitute: r.get(3)?,
        ratio: Money::parse_or_zero(&r.get::<_, String>(4)?),
        priority: r.get(5)?,
    }))?;
    let mut out = Vec::new();
    for r in rows { out.push(r?); }
    Ok(out)
}

pub fn bom_substitute_save(db: &Db, s: &Substitute, who: &str) -> DbResult<i64> {
    let tx = db.write_tx()?;
    tx.execute(
        "INSERT INTO bom_substitute(parent_code, child_code, substitute, ratio, priority)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(parent_code, child_code, substitute) DO UPDATE SET
             ratio=excluded.ratio, priority=excluded.priority",
        rusqlite::params![s.parent_code, s.child_code, s.substitute, crate::exact_param(s.ratio), s.priority],
    )?;
    let id: i64 = tx.query_row(
        "SELECT id FROM bom_substitute WHERE parent_code=?1 AND child_code=?2 AND substitute=?3",
        rusqlite::params![s.parent_code, s.child_code, s.substitute],
        |r| r.get(0),
    )?;
    bom_log_tx(&tx, &s.parent_code, "add_sub", &format!("{}/{} → {}", s.child_code, s.substitute, s.ratio.fmt_qty()), who)?;
    tx.commit()?;
    Ok(id)
}

pub fn bom_substitute_delete(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let parent: Option<String> = db.conn().query_row(
        "SELECT parent_code FROM bom_substitute WHERE id=?1", [id], |r| r.get(0)).optional()?;
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
        let e = out.entry(code.clone()).or_insert(BomNode { code: code.clone(), level, qty: Money::ZERO });
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
    let props: Option<String> = db.conn().query_row(
        "SELECT props_json FROM aux_entity WHERE kind='item' AND code=?1",
        [item_code],
        |r| r.get(0),
    ).optional()?;
    let Some(props) = props else {
        return Ok(Money::ZERO);
    };
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&props).unwrap_or_default();
    Ok(map.get("ref_cost").map(|s| Money::parse_or_zero(s)).unwrap_or(Money::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    
    #[test]
    fn po_crud() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut po = PurchaseOrder::new(p, NaiveDate::from_ymd(2026, 1, 5), "S001", "供应商A", "u1");
        po.no = po_next_no(&db, p).unwrap();
        po.lines.push(PoLine {
            id: 0, po_id: 0,
            item_code: "140301".to_string(), item_name: "原材料A".to_string(),
            qty_ordered: Money::parse("100").unwrap(), qty_received: Money::ZERO,
            unit_price: Money::parse("10").unwrap(), tax_rate: Money::parse("0.13").unwrap(),
            amount: Money::parse("1000").unwrap(), tax_amount: Money::parse("130").unwrap(),
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
        let mut so = SalesOrder::new(p, NaiveDate::from_ymd(2026, 1, 10), "C001", "客户B", "u1");
        so.no = so_next_no(&db, p).unwrap();
        so.lines.push(SoLine {
            id: 0, so_id: 0,
            item_code: "140301".to_string(), item_name: "原材料A".to_string(),
            qty_ordered: Money::parse("50").unwrap(), qty_shipped: Money::ZERO,
            unit_price: Money::parse("12").unwrap(), tax_rate: Money::parse("0.13").unwrap(),
            amount: Money::parse("600").unwrap(), tax_amount: Money::parse("78").unwrap(),
            memo: String::new(),
        });
        let id = so_save(&db, &mut so).unwrap();
        assert!(id > 0);
        so_delete(&db, id).unwrap();
    }
    
    #[test]
    fn bom_crud() {
        let db = mem();
        bom_save(&db, "1001", &[
            ("140301".to_string(), Money::parse("2").unwrap(), Money::parse("0.02").unwrap()),
            ("140302".to_string(), Money::parse("1").unwrap(), Money::ZERO),
        ]).unwrap();
        let items = bom_list(&db, "1001").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].child_code, "140301");
    }

    #[test]
    fn bom_version_and_log() {
        let db = mem();
        bom_save_version(&db, "1001", "v1", &[
            ("140301".to_string(), m("2"), m("0")),
        ], "u").unwrap();
        bom_save_version(&db, "1001", "v2", &[
            ("140301".to_string(), m("3"), m("0")),
        ], "u").unwrap();
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
        bom_substitute_save(&db, &Substitute {
            id: 0, parent_code: "FG".into(), child_code: "RM".into(),
            substitute: "ALT".into(), ratio: m("1.2"), priority: 0,
        }, "u").unwrap();
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
    let prefix = format!("{}{:04}{:02}", crate::doc_prefix(db, "prod", "SC"), year, month);
    let sql = format!(
        "SELECT COALESCE(MAX(CAST(SUBSTR(no, {}) AS INTEGER)), 0) + 1 FROM production_order WHERE no LIKE ?",
        prefix.len() + 1
    );
    let n: i64 = db.conn()
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
             planned_qty=?, completed_qty=?, status=?, work_center=?, prepared_by=?, memo=?,
             order_kind=?, supplier_code=?, supplier_name=?, plan_start=?, plan_end=?, updated_at=?
             WHERE id=?",
            rusqlite::params![
                order.period.ymm(), order.date, order.item_code, order.item_name,
                crate::exact_param(order.planned_qty), crate::exact_param(order.completed_qty),
                order.status.code(),
                order.work_center, order.prepared_by, order.memo,
                order.order_kind, order.supplier_code, order.supplier_name,
                order.plan_start, order.plan_end,
                now, order.id
            ],
        )?;
        order.id
    } else {
        tx.execute(
            "INSERT INTO production_order(period, no, date, item_code, item_name,
             planned_qty, completed_qty, status, work_center, prepared_by, memo,
             order_kind, supplier_code, supplier_name, plan_start, plan_end, created_at, updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?17)",
            rusqlite::params![
                order.period.ymm(), order.no, order.date, order.item_code, order.item_name,
                crate::exact_param(order.planned_qty), crate::exact_param(order.completed_qty),
                order.status.code(),
                order.work_center, order.prepared_by, order.memo,
                order.order_kind, order.supplier_code, order.supplier_name,
                order.plan_start, order.plan_end,
                now
            ],
        )?;
        tx.last_insert_rowid()
    };
    order.id = id;
    tx.commit()?;
    Ok(id)
}

pub fn prod_list(db: &Db, period: Period, status: Option<ProdStatus>) -> DbResult<Vec<ProductionOrder>> {
    let sql = if let Some(_s) = status {
        format!(
            "SELECT id, no, period, date, item_code, item_name, planned_qty, completed_qty,
             status, work_center, prepared_by, memo, order_kind, supplier_code, supplier_name,
             plan_start, plan_end
             FROM production_order WHERE period=? AND status=? ORDER BY date DESC, id DESC"
        )
    } else {
        format!(
            "SELECT id, no, period, date, item_code, item_name, planned_qty, completed_qty,
             status, work_center, prepared_by, memo, order_kind, supplier_code, supplier_name,
             plan_start, plan_end
             FROM production_order WHERE period=? ORDER BY date DESC, id DESC"
        )
    };
    
    let mut stmt = db.conn().prepare(&sql)?;
    let rows = if let Some(s) = status {
        stmt.query_map(rusqlite::params![period.ymm(), s.code()], |r| {
            Ok(ProductionOrder {
                id: r.get(0)?, no: r.get(1)?, period: Period::from_ymm(r.get(2)?),
                date: r.get(3)?, item_code: r.get(4)?, item_name: r.get(5)?,
                planned_qty: Money::parse_or_zero(&r.get::<_, String>(6)?),
                completed_qty: Money::parse_or_zero(&r.get::<_, String>(7)?),
                status: prod_status_from(&r.get::<_, String>(8)?),
                work_center: r.get(9)?, prepared_by: r.get(10)?, memo: r.get(11)?,
                order_kind: r.get(12)?, supplier_code: r.get(13)?, supplier_name: r.get(14)?,
                plan_start: r.get(15)?, plan_end: r.get(16)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?
    } else {
        stmt.query_map([period.ymm()], |r| {
            Ok(ProductionOrder {
                id: r.get(0)?, no: r.get(1)?, period: Period::from_ymm(r.get(2)?),
                date: r.get(3)?, item_code: r.get(4)?, item_name: r.get(5)?,
                planned_qty: Money::parse_or_zero(&r.get::<_, String>(6)?),
                completed_qty: Money::parse_or_zero(&r.get::<_, String>(7)?),
                status: prod_status_from(&r.get::<_, String>(8)?),
                work_center: r.get(9)?, prepared_by: r.get(10)?, memo: r.get(11)?,
                order_kind: r.get(12)?, supplier_code: r.get(13)?, supplier_name: r.get(14)?,
                plan_start: r.get(15)?, plan_end: r.get(16)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows)
}

/// 细排：批量写回计划开工/完工日（仅未完工订单可排；条件更新防误写终态单）
pub fn prod_schedule(db: &Db, items: &[(i64, String, String)]) -> DbResult<usize> {
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
            id: 0, no: String::new(), period: p,
            date: NaiveDate::from_ymd(2026, 1, 5),
            item_code: "1001".to_string(), item_name: "成品A".to_string(),
            planned_qty: Money::parse("100").unwrap(), completed_qty: Money::ZERO,
            status: ProdStatus::Draft, work_center: "WC01".to_string(),
            prepared_by: "u1".to_string(), memo: String::new(),
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
