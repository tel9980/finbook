//! 高级功能：工艺路线 / 工序报工 / MRP / 预算多版本 / 审批流 / 报表附注 /
//! 会计电子档案 / 摘要汇总表 / 多栏账增强
//!
//! 对标金蝶云星空 / 用友 U8+ 的深度功能。设计原则与核心层一致：
//! - 金额一律 TEXT 存储、Rust 侧 Decimal 运算，不走 SQL SUM；
//! - 所有写操作幂等（UNIQUE 约束 + upsert）；
//! - 单据级双向追溯（业务单 ↔ 凭证）。

use fincore::{FinError, Money, Period};
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

use crate::{Db, DbResult};

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn read_m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

// ===========================================================================
// 工艺路线（Routing）
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RoutingOp {
    pub id: i64,
    pub item_code: String,
    pub version: String,
    pub seq: i32,
    pub op_code: String,
    pub op_name: String,
    pub work_center: String,
    pub std_hours: Money,
    pub rate: Money,
    /// v31：工序检验点（完工前需录工序检验单）
    #[serde(default)]
    pub qc_required: bool,
}

fn map_routing(r: &rusqlite::Row) -> rusqlite::Result<RoutingOp> {
    Ok(RoutingOp {
        id: r.get(0)?,
        item_code: r.get(1)?,
        version: r.get(2)?,
        seq: r.get(3)?,
        op_code: r.get(4)?,
        op_name: r.get(5)?,
        work_center: r.get(6)?,
        std_hours: read_m(&r.get::<_, String>(7)?),
        rate: read_m(&r.get::<_, String>(8)?),
        qc_required: r.get::<_, i64>(9)? != 0,
    })
}

const RT_COLS: &str =
    "id,item_code,version,seq,op_code,op_name,work_center,std_hours,rate,qc_required";

pub fn routing_list(db: &Db, item_code: &str) -> DbResult<Vec<RoutingOp>> {
    routing_list_version(db, item_code, "")
}

/// 按版本列工艺路线（version 为空 = 默认版本）
pub fn routing_list_version(db: &Db, item_code: &str, version: &str) -> DbResult<Vec<RoutingOp>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {RT_COLS} FROM routing WHERE item_code=?1 AND version=?2 ORDER BY seq"
    ))?;
    let rows = st
        .query_map(rusqlite::params![item_code, version], map_routing)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 某产品的全部工艺版本
pub fn routing_versions(db: &Db, item_code: &str) -> DbResult<Vec<String>> {
    let mut st = db
        .conn()
        .prepare("SELECT DISTINCT version FROM routing WHERE item_code=?1 ORDER BY version")?;
    let rows = st
        .query_map(rusqlite::params![item_code], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 整单保存某产品的工艺路线（先删后插，事务保证原子性）
pub fn routing_save(db: &Db, item_code: &str, ops: &[RoutingOp]) -> DbResult<()> {
    routing_save_version(db, item_code, "", ops)
}

/// 带版本保存工艺路线
pub fn routing_save_version(
    db: &Db,
    item_code: &str,
    version: &str,
    ops: &[RoutingOp],
) -> DbResult<()> {
    let tx = db.write_tx()?;
    tx.execute(
        "DELETE FROM routing WHERE item_code=?1 AND version=?2",
        rusqlite::params![item_code, version],
    )?;
    for op in ops {
        tx.execute(
            "INSERT INTO routing(item_code,version,seq,op_code,op_name,work_center,std_hours,rate,qc_required)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            rusqlite::params![
                item_code,
                version,
                op.seq,
                op.op_code,
                op.op_name,
                op.work_center,
                crate::exact_param(op.std_hours),
                crate::exact_param(op.rate),
                op.qc_required as i64
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn routing_delete(db: &Db, item_code: &str) -> DbResult<()> {
    routing_delete_version(db, item_code, "")
}

/// 删除指定版本的工艺路线
pub fn routing_delete_version(db: &Db, item_code: &str, version: &str) -> DbResult<()> {
    db.conn().execute(
        "DELETE FROM routing WHERE item_code=?1 AND version=?2",
        rusqlite::params![item_code, version],
    )?;
    Ok(())
}

/// 工艺统计：某产品的工序数、标准工时合计、标准成本合计
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RoutingStats {
    pub item_code: String,
    pub op_count: usize,
    pub total_hours: Money,
    pub total_cost: Money,
}

/// 工艺统计（默认版本）
pub fn routing_stats(db: &Db, item_code: &str) -> DbResult<RoutingStats> {
    let ops = routing_list(db, item_code)?;
    let total_hours: Money = ops.iter().map(|o| o.std_hours).sum();
    let total_cost: Money = ops.iter().map(|o| o.std_hours * o.rate.inner()).sum();
    Ok(RoutingStats {
        item_code: item_code.to_string(),
        op_count: ops.len(),
        total_hours,
        total_cost,
    })
}

/// 工艺导入：JSON 数组 [{seq,op_code,op_name,work_center,std_hours,rate},...]
pub fn routing_import_json(db: &Db, item_code: &str, version: &str, json: &str) -> DbResult<usize> {
    #[derive(serde::Deserialize)]
    struct RawOp {
        #[serde(default)]
        seq: i32,
        #[serde(default)]
        op_code: String,
        #[serde(default)]
        op_name: String,
        #[serde(default)]
        work_center: String,
        #[serde(default)]
        std_hours: String,
        #[serde(default)]
        rate: String,
        #[serde(default)]
        qc_required: bool,
    }
    let raw: Vec<RawOp> = serde_json::from_str(json)
        .map_err(|e| FinError::msg(format!("工艺 JSON 解析失败：{e}")))?;
    let ops: Vec<RoutingOp> = raw
        .into_iter()
        .map(|r| RoutingOp {
            id: 0,
            item_code: item_code.to_string(),
            version: version.to_string(),
            seq: r.seq,
            op_code: r.op_code,
            op_name: r.op_name,
            work_center: r.work_center,
            std_hours: Money::parse_or_zero(&r.std_hours),
            rate: Money::parse_or_zero(&r.rate),
            qc_required: r.qc_required,
        })
        .collect();
    routing_save_version(db, item_code, version, &ops)?;
    Ok(ops.len())
}

/// 工艺导出：整条路线序列化为 JSON
pub fn routing_export_json(db: &Db, item_code: &str, version: &str) -> DbResult<String> {
    let ops = routing_list_version(db, item_code, version)?;
    serde_json::to_string(&ops).map_err(|e| FinError::msg(format!("序列化失败：{e}")).into())
}

// ===========================================================================
// 工序报工（生产订单工序进度）
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProdOp {
    pub id: i64,
    pub po_id: i64,
    pub routing_id: i64,
    pub op_name: String,
    pub work_center: String,
    pub worker: String,
    pub qty_done: Money,
    pub hours: Money,
    pub status: String, // pending / in_progress / done
    pub memo: String,
}

fn map_prod_op(r: &rusqlite::Row) -> rusqlite::Result<ProdOp> {
    Ok(ProdOp {
        id: r.get(0)?,
        po_id: r.get(1)?,
        routing_id: r.get(2)?,
        op_name: r.get(3)?,
        work_center: r.get(4)?,
        worker: r.get(5)?,
        qty_done: read_m(&r.get::<_, String>(6)?),
        hours: read_m(&r.get::<_, String>(7)?),
        status: r.get(8)?,
        memo: r.get(9)?,
    })
}

const PO_COLS: &str = "id,po_id,routing_id,op_name,work_center,worker,qty_done,hours,status,memo";

/// 生产订单开工时按工艺路线生成工序清单
pub fn prod_op_init_from_routing(db: &Db, po_id: i64, item_code: &str) -> DbResult<usize> {
    let ops = routing_list(db, item_code)?;
    if ops.is_empty() {
        return Ok(0);
    }
    // 已有工序则不重复生成
    let cnt: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM prod_op WHERE po_id=?1",
        [po_id],
        |r| r.get(0),
    )?;
    if cnt > 0 {
        return Ok(0);
    }
    let tx = db.write_tx()?;
    for op in &ops {
        tx.execute(
            "INSERT INTO prod_op(po_id,routing_id,op_name,work_center,worker,qty_done,hours,status,memo)
             VALUES(?1,?2,?3,?4,'','0','0','pending','')",
            rusqlite::params![po_id, op.id, op.op_name, op.work_center],
        )?;
    }
    tx.commit()?;
    Ok(ops.len())
}

pub fn prod_op_list(db: &Db, po_id: i64) -> DbResult<Vec<ProdOp>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {PO_COLS} FROM prod_op WHERE po_id=?1 ORDER BY id"
    ))?;
    let rows = st
        .query_map(rusqlite::params![po_id], map_prod_op)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 报工：累加完工数量与工时，自动推进状态（读改写同事务，避免并发报工丢更新）
pub fn prod_op_report(db: &Db, op_id: i64, qty: Money, hours: Money) -> DbResult<()> {
    if qty.is_negative() || hours.is_negative() {
        return Err(FinError::msg("报工数量与工时不能为负").into());
    }
    let tx = db.write_tx()?;
    // 定点累加：金额/数量一律在 Rust 侧用 Decimal 运算，绝不走 SQL 的 REAL 浮点
    let cur = tx
        .query_row(
            "SELECT qty_done, hours FROM prod_op WHERE id=?1",
            rusqlite::params![op_id],
            |r| {
                Ok((
                    read_m(&r.get::<_, String>(0)?),
                    read_m(&r.get::<_, String>(1)?),
                ))
            },
        )
        .optional()?
        .ok_or_else(|| FinError::msg(format!("工序 {op_id} 不存在")))?;
    let new_qty = cur.0 + qty;
    let new_hours = cur.1 + hours;
    // 状态只做「推进」不做「回退」：已完工（done）的工序不能被一次报工打回
    // in_progress/pending，否则生产进度（prod_progress）会凭空倒退
    let status = match tx.query_row(
        "SELECT status FROM prod_op WHERE id=?1",
        rusqlite::params![op_id],
        |r| r.get::<_, String>(0),
    )? {
        s if s == "done" => "done",
        _ if new_qty.is_positive() => "in_progress",
        _ => "pending",
    };
    tx.execute(
        "UPDATE prod_op SET qty_done=?2, hours=?3, status=?4 WHERE id=?1",
        rusqlite::params![
            op_id,
            crate::exact_param(new_qty),
            crate::exact_param(new_hours),
            status
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// 手工标记某工序完成
pub fn prod_op_finish(db: &Db, op_id: i64) -> DbResult<()> {
    let n = db.conn().execute(
        "UPDATE prod_op SET status='done' WHERE id=?1",
        rusqlite::params![op_id],
    )?;
    if n == 0 {
        return Err(FinError::not_found(format!("工序 {op_id} 不存在")).into());
    }
    Ok(())
}

/// 工序派工：把工人分配到某道工序
pub fn prod_op_dispatch(db: &Db, op_id: i64, worker: &str) -> DbResult<()> {
    if worker.trim().is_empty() {
        return Err(FinError::msg("派工工人不能为空").into());
    }
    db.conn().execute(
        "UPDATE prod_op SET worker=?2 WHERE id=?1",
        rusqlite::params![op_id, worker],
    )?;
    Ok(())
}

/// 生产进度：完工工序数 / 总工序数
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProdProgress {
    pub po_id: i64,
    pub total_ops: usize,
    pub done_ops: usize,
    pub in_progress_ops: usize,
    /// 进度百分比（0~100）
    pub percent: Money,
}

pub fn prod_progress(db: &Db, po_id: i64) -> DbResult<ProdProgress> {
    let ops = prod_op_list(db, po_id)?;
    let total = ops.len();
    let done = ops.iter().filter(|o| o.status == "done").count();
    let in_prog = ops.iter().filter(|o| o.status == "in_progress").count();
    let percent = if total == 0 {
        Money::ZERO
    } else {
        (Money::from_i64(done as i64) * Money::from_i64(100))
            .checked_div(Money::from_i64(total as i64))
            .expect("total 已判非零")
            .round2()
    };
    Ok(ProdProgress {
        po_id,
        total_ops: total,
        done_ops: done,
        in_progress_ops: in_prog,
        percent,
    })
}

/// 按工时 × 费率归集工序成本到生产订单（写入 prod_cost 的 labor/overhead）
pub fn prod_op_collect_cost(db: &Db, po_id: i64) -> DbResult<Money> {
    // 取该生产订单的产成品，找工艺路线费率
    let po = crate::manufacturing::get_prod_order(db, po_id)?
        .ok_or_else(|| FinError::msg(format!("生产订单 {po_id} 不存在")))?;
    let routes = routing_list(db, &po.item_code)?;
    let ops = prod_op_list(db, po_id)?;
    let mut total = Money::ZERO;
    for op in &ops {
        if let Some(rt) = routes.iter().find(|r| r.id == op.routing_id) {
            let cost = (op.hours * rt.rate.inner()).round2();
            total += cost;
        }
    }
    if !total.is_zero() {
        crate::manufacturing::add_cost(
            db,
            po_id,
            crate::manufacturing::CostType::Labor,
            total,
            "工序工时成本归集",
        )?;
    }
    Ok(total)
}

// ===========================================================================
// 存货计划参数 + MRP 运算
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ItemPlan {
    pub item_code: String,
    pub safety_stock: Money,
    pub lead_days: i32,
    pub lot_size: Money,
}

pub fn item_plan_get(db: &Db, item_code: &str) -> DbResult<Option<ItemPlan>> {
    db.conn()
        .query_row(
            "SELECT item_code,safety_stock,lead_days,lot_size FROM item_plan WHERE item_code=?1",
            rusqlite::params![item_code],
            |r| {
                Ok(ItemPlan {
                    item_code: r.get(0)?,
                    safety_stock: read_m(&r.get::<_, String>(1)?),
                    lead_days: r.get(2)?,
                    lot_size: read_m(&r.get::<_, String>(3)?),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

/// 一次取全表（存货档案导出要用：逐条 `item_plan_get` 是 N 次查询，
/// 上千个存货时导出会明显卡）。
pub fn item_plan_all(db: &Db) -> DbResult<Vec<ItemPlan>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT item_code,safety_stock,lead_days,lot_size FROM item_plan")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ItemPlan {
                item_code: r.get(0)?,
                safety_stock: read_m(&r.get::<_, String>(1)?),
                lead_days: r.get(2)?,
                lot_size: read_m(&r.get::<_, String>(3)?),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn item_plan_upsert(db: &Db, p: &ItemPlan) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO item_plan(item_code,safety_stock,lead_days,lot_size) VALUES(?1,?2,?3,?4)
         ON CONFLICT(item_code) DO UPDATE SET safety_stock=excluded.safety_stock,
            lead_days=excluded.lead_days, lot_size=excluded.lot_size",
        rusqlite::params![
            p.item_code,
            crate::exact_param(p.safety_stock),
            p.lead_days,
            crate::exact_param(p.lot_size)
        ],
    )?;
    Ok(())
}

/// MRP 运算结果行
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MrpRow {
    pub id: i64,
    pub run_at: String,
    pub item_code: String,
    pub item_name: String,
    pub level: i32,
    pub gross_req: Money,
    pub on_hand: Money,
    pub net_req: Money,
    pub planned_qty: Money,
    pub action: String, // produce / purchase / none
    pub source: String,
}

fn map_mrp(r: &rusqlite::Row) -> rusqlite::Result<MrpRow> {
    Ok(MrpRow {
        id: r.get(0)?,
        run_at: r.get(1)?,
        item_code: r.get(2)?,
        item_name: r.get(3)?,
        level: r.get(4)?,
        gross_req: read_m(&r.get::<_, String>(5)?),
        on_hand: read_m(&r.get::<_, String>(6)?),
        net_req: read_m(&r.get::<_, String>(7)?),
        planned_qty: read_m(&r.get::<_, String>(8)?),
        action: r.get(9)?,
        source: r.get(10)?,
    })
}

const MRP_COLS: &str =
    "id,run_at,item_code,item_name,level,gross_req,on_hand,net_req,planned_qty,action,source";

/// 查询最近一次 MRP 运算结果
pub fn mrp_latest(db: &Db) -> DbResult<Vec<MrpRow>> {
    // 空表时 MAX(run_at) 仍返回一行 NULL：必须按 Option<String> 取值，
    // 否则 rusqlite 会以 "Invalid column type Null" 报错（页面 500）。
    let latest: Option<String> =
        db.conn()
            .query_row("SELECT MAX(run_at) FROM mrp_result", [], |r| {
                r.get::<_, Option<String>>(0)
            })?;
    let Some(ts) = latest else {
        return Ok(Vec::new());
    };
    mrp_by_run(db, &ts)
}

pub fn mrp_by_run(db: &Db, run_at: &str) -> DbResult<Vec<MrpRow>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {MRP_COLS} FROM mrp_result WHERE run_at=?1 ORDER BY level, item_code"
    ))?;
    let rows = st
        .query_map(rusqlite::params![run_at], map_mrp)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 按 id 取一条 MRP 结果行
pub fn mrp_get(db: &Db, id: i64) -> DbResult<Option<MrpRow>> {
    db.conn()
        .query_row(
            &format!("SELECT {MRP_COLS} FROM mrp_result WHERE id=?1"),
            rusqlite::params![id],
            map_mrp,
        )
        .optional()
        .map_err(Into::into)
}

/// MRP **采购建议**下推请购单（草稿，对标金蝶 MRP 投放 → 请购）：
/// - 仅 `action=purchase` 且计划量 > 0 可下推；
/// - 单号按期间自动取号；同一 MRP 行只能下推一次（doc_link 幂等，重复 400）；
/// - 返回 (请购单 id, 单号)。
pub fn mrp_to_req(db: &Db, id: i64, requester: &str) -> DbResult<(i64, String)> {
    let row = mrp_get(db, id)?.ok_or_else(|| FinError::not_found("MRP 结果行"))?;
    if row.action != "purchase" {
        return Err(
            FinError::msg("仅采购类建议可下推请购（生产类请走「下达」生成生产订单）").into(),
        );
    }
    if !row.planned_qty.is_positive() {
        return Err(FinError::msg("计划量为 0，无可下推数量").into());
    }
    if crate::docflow::has_link(db, "mrp", id, "req")? {
        return Err(FinError::msg("该 MRP 行已下推过请购单，请勿重复下推").into());
    }
    let date = chrono::Local::now().date_naive();
    let period = Period::from_date(date);
    let mut r = crate::procurement::PurchaseReq {
        id: 0,
        no: String::new(),
        period,
        date,
        item_code: row.item_code.clone(),
        item_name: if row.item_name.trim().is_empty() {
            row.item_code.clone()
        } else {
            row.item_name.clone()
        },
        qty: row.planned_qty,
        status: "draft".into(),
        requester: requester.to_string(),
        memo: format!("MRP 下推（{}）", row.run_at),
    };
    if r.no.trim().is_empty() {
        r.no = crate::procurement::pr_next_no(db, period)?;
    }
    let req_id = crate::procurement::pr_save(db, &mut r)?;
    crate::docflow::link_add(db, "mrp", id, "req", req_id, "MRP 采购建议下推")?;
    Ok((req_id, r.no))
}

/// 存货现有库存（stock_move 数量代数和）
fn on_hand_qty(db: &Db, item_code: &str) -> DbResult<Money> {
    let mut st = db
        .conn()
        .prepare("SELECT qty FROM stock_move WHERE item=?1")?;
    let rows = st
        .query_map(rusqlite::params![item_code], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut sum = Money::ZERO;
    for s in rows {
        sum += read_m(&s);
    }
    Ok(sum)
}

/// BOM 单层展开
struct BomNode {
    child: String,
    qty: Money,
    loss_rate: Money,
}

fn bom_children(db: &Db, parent: &str) -> DbResult<Vec<BomNode>> {
    let items = crate::scm::bom_list(db, parent)?;
    Ok(items
        .into_iter()
        .map(|b| BomNode {
            child: b.child_code,
            qty: b.qty,
            loss_rate: b.loss_rate,
        })
        .collect())
}

/// MRP 主运算：输入产成品需求清单（item_code, qty, source），
/// 按 BOM 逐层展开，算毛需求 → 净需求（扣现有库存与安全库存）→ 计划量（套批量）。
///
/// 有 BOM 的物料 action=produce，否则 purchase。返回本次 run_at 时间戳。
pub fn mrp_run(db: &Db, demands: &[(String, Money, String)]) -> DbResult<String> {
    // run_at 微秒精度：秒级会令同秒两次运行在 mrp_latest（MAX(run_at)）中混合
    let run_at = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.6f")
        .to_string();
    // 净需求累加表：item -> (gross, level, source)
    struct Acc {
        gross: Money,
        level: i32,
        sources: Vec<String>,
    }
    let mut acc: std::collections::BTreeMap<String, Acc> = std::collections::BTreeMap::new();

    // 用队列做逐层展开（同层可合并），guard 防 BOM 循环引用
    let mut queue: std::collections::VecDeque<(String, Money, i32, String)> = demands
        .iter()
        .map(|(c, q, s)| (c.clone(), *q, 0, s.clone()))
        .collect();
    let mut guard = 0usize;

    while let Some((code, qty, level, source)) = queue.pop_front() {
        guard += 1;
        if guard > 10_000 {
            return Err(FinError::msg("BOM 展开超过 10000 节点，疑似循环引用").into());
        }
        let e = acc.entry(code.clone()).or_insert_with(|| Acc {
            gross: Money::ZERO,
            level,
            sources: Vec::new(),
        });
        e.gross += qty;
        if level > e.level {
            e.level = level;
        }
        if !source.is_empty() && !e.sources.contains(&source) {
            e.sources.push(source.clone());
        }
        // 有 BOM 说明是自制件：净需求先按下层继续展开（这里先展开毛需求，
        // 库存抵扣在落库时统一算，保证同层合并）
        let children = bom_children(db, &code)?;
        if !children.is_empty() {
            for ch in children {
                let eff = ch.qty * (Money::ONE + ch.loss_rate);
                let need = (qty * eff.inner()).round_dp(fincore::money::QTY_DP);
                queue.push_back((ch.child.clone(), need, level + 1, format!("BOM:{code}")));
            }
        }
    }

    // 落库：先清掉同一 run_at（理论上不会冲突），再逐条插入
    let tx = db.write_tx()?;
    for (code, a) in &acc {
        let on_hand = on_hand_qty(db, code)?;
        let plan = item_plan_get(db, code)?.unwrap_or(ItemPlan {
            item_code: code.clone(),
            safety_stock: Money::ZERO,
            lead_days: 0,
            lot_size: Money::ZERO,
        });
        // 净需求 = 毛需求 + 安全库存 - 现有库存
        let mut net = a.gross + plan.safety_stock - on_hand;
        if net.is_negative() {
            net = Money::ZERO;
        }
        // 计划量：套最小批量（向上取整到 lot_size 的整数倍）
        let planned = if plan.lot_size.is_positive() && net.is_positive() {
            let lots = (net.inner() / plan.lot_size.inner())
                .ceil()
                .to_string()
                .parse::<i64>()
                .unwrap_or(1);
            (plan.lot_size * Money::from_i64(lots)).round_dp(fincore::money::QTY_DP)
        } else {
            net
        };
        let has_bom = !bom_children(db, code)?.is_empty();
        let action = if net.is_zero() {
            "none"
        } else if has_bom {
            "produce"
        } else {
            "purchase"
        };
        // 尝试从存货档案取名称
        let name: String = tx
            .query_row(
                "SELECT name FROM aux_entity WHERE kind='item' AND code=?1",
                rusqlite::params![code],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| code.clone());
        tx.execute(
            "INSERT INTO mrp_result(run_at,item_code,item_name,level,gross_req,on_hand,net_req,planned_qty,action,source)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                run_at,
                code,
                name,
                a.level,
                crate::exact_param(a.gross),
                crate::exact_param(on_hand),
                crate::exact_param(net),
                crate::exact_param(planned),
                action,
                a.sources.join(",")
            ],
        )?;
    }
    tx.commit()?;
    Ok(run_at)
}

/// 从已确认销售订单收集 MRP 需求（未发完的数量）
pub fn mrp_demands_from_sales(db: &Db, period: Period) -> DbResult<Vec<(String, Money, String)>> {
    let orders = crate::scm::so_list(db, period, None)?;
    let mut out = Vec::new();
    for so in orders {
        if !matches!(
            so.status,
            crate::scm::SoStatus::Confirmed | crate::scm::SoStatus::PartialShip
        ) {
            continue;
        }
        for l in &so.lines {
            let open = l.qty_ordered - l.qty_shipped;
            if open.is_positive() {
                out.push((l.item_code.clone(), open, format!("SO-{}", so.no)));
            }
        }
    }
    Ok(out)
}

// ===========================================================================
// MPS 主生产计划 / 粗排（链6）
// ===========================================================================

/// MPS 计划行（成品维度：需求 − 现有 − 在制 = 计划）
#[derive(Clone, Debug, serde::Serialize)]
pub struct MpsPlan {
    pub id: i64,
    pub run_at: String,
    pub item_code: String,
    pub item_name: String,
    pub demand: Money,
    pub on_hand: Money,
    pub wip: Money,
    pub planned: Money,
    pub source: String,
    pub due_date: String,
    /// open / converted
    pub status: String,
}

const MPS_COLS: &str =
    "id,run_at,item_code,item_name,demand,on_hand,wip,planned,source,due_date,status";

fn map_mps(r: &rusqlite::Row) -> rusqlite::Result<MpsPlan> {
    Ok(MpsPlan {
        id: r.get(0)?,
        run_at: r.get(1)?,
        item_code: r.get(2)?,
        item_name: r.get(3)?,
        demand: read_m(&r.get::<_, String>(4)?),
        on_hand: read_m(&r.get::<_, String>(5)?),
        wip: read_m(&r.get::<_, String>(6)?),
        planned: read_m(&r.get::<_, String>(7)?),
        source: r.get(8)?,
        due_date: r.get(9)?,
        status: r.get(10)?,
    })
}

/// MPS 运行：需求聚合（已确认销售订单未发量 + 手工行）→ 净算计划量
/// （扣现有库存与在制未完工，防止重复下达）。sales_period=0 不取销售需求。
/// 返回本次 run_at。
pub fn mps_run(
    db: &Db,
    extra: &[(String, Money, String)],
    sales_period: Period,
    due_date: &str,
) -> DbResult<String> {
    use std::collections::HashMap;
    let mut demand: HashMap<String, Money> = HashMap::new();
    let mut source: HashMap<String, Vec<String>> = HashMap::new();
    if sales_period.ymm() > 0 {
        for (item, qty, src) in mrp_demands_from_sales(db, sales_period)? {
            *demand.entry(item.clone()).or_insert(Money::ZERO) += qty;
            source.entry(item).or_default().push(src);
        }
    }
    for (item, qty, src) in extra {
        let it = item.trim();
        if it.is_empty() || !qty.is_positive() {
            continue;
        }
        *demand.entry(it.to_string()).or_insert(Money::ZERO) += *qty;
        source.entry(it.to_string()).or_default().push(src.clone());
    }
    // 在制 = 全部未完工生产订单的 open qty（Draft/Released/InProgress）
    let mut wip: HashMap<String, Money> = HashMap::new();
    {
        let mut st = db.conn().prepare(
            "SELECT item_code, planned_qty, completed_qty, status FROM production_order",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (item, pq, cq, stt) in rows {
            if stt == "completed" || stt == "cancelled" {
                continue;
            }
            let open = read_m(&pq) - read_m(&cq);
            if open.is_positive() {
                *wip.entry(item).or_insert(Money::ZERO) += open;
            }
        }
    }
    // run_at 精确到微秒：秒级精度下同秒两次运行会在 mps_latest（MAX(run_at)）中混合
    let run_at = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S%.6f")
        .to_string();
    let tx = db.write_tx()?;
    let now_s = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let mut items: Vec<String> = demand.keys().cloned().collect();
    items.sort();
    for item in items {
        let d = demand[&item];
        let on_hand: Money = crate::inventory2::warehouse_stock(db, &item)?
            .iter()
            .map(|w| w.qty)
            .sum();
        let w = wip.get(&item).copied().unwrap_or(Money::ZERO);
        let net = d - on_hand - w;
        let planned = if net.is_positive() { net } else { Money::ZERO };
        let src = source.get(&item).map(|v| v.join(",")).unwrap_or_default();
        tx.execute(
            "INSERT INTO mps_plan(run_at,item_code,item_name,demand,on_hand,wip,planned,source,due_date,status,created_at)
             VALUES(?1,?2,'',?3,?4,?5,?6,?7,?8,'open',?9)",
            rusqlite::params![run_at, item, crate::exact_param(d), crate::exact_param(on_hand), crate::exact_param(w), crate::exact_param(planned), src, due_date, now_s],
        )?;
    }
    tx.commit()?;
    Ok(run_at)
}

/// 最近一次 MPS 结果
pub fn mps_latest(db: &Db) -> DbResult<Vec<MpsPlan>> {
    let latest: Option<String> =
        db.conn()
            .query_row("SELECT MAX(run_at) FROM mps_plan", [], |r| {
                r.get::<_, Option<String>>(0)
            })?;
    let Some(ts) = latest else {
        return Ok(Vec::new());
    };
    let mut st = db.conn().prepare(&format!(
        "SELECT {MPS_COLS} FROM mps_plan WHERE run_at=?1 ORDER BY id"
    ))?;
    let rows = st
        .query_map([ts], map_mps)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// MPS 行下达：open → converted（条件更新防并发）并生成已下达生产订单。
/// qty=None 用行计划量；**订单账期=当前计划期**（对齐 create_prod 惯例——period 跟计划、
/// date 跟交期，允许跨期），单据日期=行建议交期（无交期=今天）。
/// 两步非原子（防重闸在先，prod_save 独立事务）——prod_save 失败时行已转，
/// 可由审计日志定位补单；v1 声明。返回 (生产订单 id, 单号)。
pub fn mps_convert_to_order(
    db: &Db,
    id: i64,
    qty: Option<Money>,
    period: Period,
    who: &str,
) -> DbResult<(i64, String)> {
    let row = match db.conn().query_row(
        &format!("SELECT {MPS_COLS} FROM mps_plan WHERE id=?1"),
        [id],
        map_mps,
    ) {
        Ok(r) => r,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(fincore::FinError::not_found("MPS 行不存在").into())
        }
        Err(e) => return Err(e.into()),
    };
    if row.status != "open" {
        return Err(fincore::FinError::state("该 MPS 行已下达").into());
    }
    let q = qty.unwrap_or(row.planned);
    if !q.is_positive() {
        return Err(fincore::FinError::msg("计划量为 0，无需下达").into());
    }
    // 防并发：条件更新（open → converted）
    let n = db.conn().execute(
        "UPDATE mps_plan SET status='converted' WHERE id=?1 AND status='open'",
        [id],
    )?;
    if n == 0 {
        return Err(fincore::FinError::state("MPS 行状态已变化").into());
    }
    let date = chrono::NaiveDate::parse_from_str(row.due_date.trim(), "%Y-%m-%d")
        .unwrap_or_else(|_| chrono::Local::now().date_naive());
    let mut order = crate::scm::ProductionOrder {
        id: 0,
        no: String::new(),
        period,
        date,
        item_code: row.item_code.clone(),
        item_name: row.item_name.clone(),
        planned_qty: q,
        completed_qty: Money::ZERO,
        status: crate::scm::ProdStatus::Released,
        work_center: String::new(),
        so_id: 0, // MPS 按预测/安全库存建议下达，与具体销售订单无关
        prepared_by: who.to_string(),
        memo: "MPS 下达".to_string(),
        order_kind: "inhouse".to_string(),
        supplier_code: String::new(),
        supplier_name: String::new(),
        plan_start: String::new(),
        plan_end: String::new(),
    };
    if order.item_name.is_empty() {
        order.item_name = order.item_code.clone();
    }
    order.no = crate::scm::prod_next_no(db, order.period)?;
    let oid = crate::scm::prod_save(db, &mut order)?;
    Ok((oid, order.no))
}

/// 粗排建议行
#[derive(Clone, Debug, serde::Serialize)]
pub struct RoughRow {
    pub id: i64,
    pub no: String,
    pub item_code: String,
    pub item_name: String,
    pub open_qty: Money,
    pub work_center: String,
    pub need_days: i64,
    pub sug_start: String,
    pub sug_end: String,
}

/// 按日负荷
#[derive(Clone, Debug, serde::Serialize)]
pub struct LoadRow {
    pub date: String,
    pub qty: Money,
    pub capacity: Money,
    pub over: bool,
}

/// 粗排（v1 **件/日产能**口径——工艺路线暂无标准工时字段，工时口径留待迭代）：
/// 本期间未完工自制订单按单据日期顺排，need_days 以 ceil(open/日产能) 计；
/// 起点=今天，逐单装载产生按日负荷（超载标红）。返回 (排期建议, 按日负荷)。
pub fn rough_schedule(
    db: &Db,
    period: Period,
    daily_qty: Money,
) -> DbResult<(Vec<RoughRow>, Vec<LoadRow>)> {
    if !daily_qty.is_positive() {
        return Err(fincore::FinError::msg("日产能必须大于 0").into());
    }
    let orders = crate::scm::prod_list(db, period, None)?;
    let mut rows: Vec<RoughRow> = orders
        .into_iter()
        .filter(|o| {
            !matches!(
                o.status,
                crate::scm::ProdStatus::Completed | crate::scm::ProdStatus::Cancelled
            ) && o.order_kind == "inhouse"
                && (o.planned_qty - o.completed_qty).is_positive()
        })
        .map(|o| RoughRow {
            open_qty: o.planned_qty - o.completed_qty,
            need_days: 0,
            sug_start: String::new(),
            sug_end: String::new(),
            id: o.id,
            no: o.no,
            item_code: o.item_code,
            item_name: o.item_name,
            work_center: o.work_center,
        })
        .collect();
    // 排序：单据日期 → id
    let date_of = orders_date_map(db, period)?;
    rows.sort_by(|a, b| {
        let da = date_of.get(&a.id).cloned().unwrap_or_default();
        let dbb = date_of.get(&b.id).cloned().unwrap_or_default();
        da.cmp(&dbb).then_with(|| a.id.cmp(&b.id))
    });
    let mut cursor = chrono::Local::now().date_naive();
    let mut load: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    for r in &mut rows {
        // need_days = ceil(open / daily)（Money 加法累加，避免浮点）
        let mut days: i64 = 1;
        let mut acc = daily_qty;
        while acc < r.open_qty {
            acc += daily_qty;
            days += 1;
        }
        r.need_days = days;
        r.sug_start = cursor.format("%Y-%m-%d").to_string();
        let end = cursor + chrono::Duration::days(days - 1);
        r.sug_end = end.format("%Y-%m-%d").to_string();
        // 逐日装载
        let mut rest = r.open_qty;
        for i in 0..days {
            let day = cursor + chrono::Duration::days(i);
            let put = rest.min(daily_qty);
            *load
                .entry(day.format("%Y-%m-%d").to_string())
                .or_insert(Money::ZERO) += put;
            rest -= put;
        }
        cursor = end + chrono::Duration::days(1);
    }
    let load_rows: Vec<LoadRow> = load
        .into_iter()
        .map(|(date, qty)| LoadRow {
            over: qty > daily_qty,
            date,
            qty,
            capacity: daily_qty,
        })
        .collect();
    Ok((rows, load_rows))
}

/// 粗排排序辅助：订单 id → 单据日期（"YYYY-MM-DD" 字典序即日期序）
fn orders_date_map(db: &Db, period: Period) -> DbResult<std::collections::HashMap<i64, String>> {
    let mut st = db
        .conn()
        .prepare("SELECT id, date FROM production_order WHERE period=?1")?;
    let rows = st
        .query_map([period.ymm()], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().collect())
}

// ===========================================================================
// 预算多版本
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BudgetVersion {
    pub key: String,
    pub name: String,
    pub is_current: bool,
    pub created_at: String,
    pub memo: String,
}

pub fn bversion_list(db: &Db) -> DbResult<Vec<BudgetVersion>> {
    let mut st = db.conn().prepare(
        "SELECT key,name,is_current,created_at,memo FROM budget_version ORDER BY created_at",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(BudgetVersion {
                key: r.get(0)?,
                name: r.get(1)?,
                is_current: r.get::<_, i64>(2)? != 0,
                created_at: r.get(3)?,
                memo: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn bversion_save(db: &Db, v: &BudgetVersion) -> DbResult<()> {
    let tx = db.write_tx()?;
    if v.is_current {
        // 单一生效版本：先把其他版本全部置为不生效
        tx.execute("UPDATE budget_version SET is_current=0", [])?;
    }
    tx.execute(
        "INSERT INTO budget_version(key,name,is_current,created_at,memo) VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(key) DO UPDATE SET name=excluded.name, is_current=excluded.is_current, memo=excluded.memo",
        rusqlite::params![v.key, v.name, if v.is_current { 1 } else { 0 }, v.created_at, v.memo],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn bversion_delete(db: &Db, key: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    tx.execute(
        "DELETE FROM budget WHERE version=?1",
        rusqlite::params![key],
    )?;
    tx.execute(
        "DELETE FROM budget_version WHERE key=?1",
        rusqlite::params![key],
    )?;
    tx.commit()?;
    Ok(())
}

/// 当前生效版本 key（无则 ''，兼容旧数据）
pub fn bversion_current(db: &Db) -> DbResult<String> {
    let k: Option<String> = db
        .conn()
        .query_row(
            "SELECT key FROM budget_version WHERE is_current=1 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    Ok(k.unwrap_or_default())
}

/// 复制整个版本的预算行到另一个版本
pub fn bversion_copy(db: &Db, from: &str, to: &str) -> DbResult<usize> {
    let rows = crate::mgmt::budget_list_version(db, None, from)?;
    let mut n = 0;
    for b in rows {
        crate::mgmt::budget_upsert_version(
            db,
            &crate::mgmt::Budget {
                id: 0,
                version: to.to_string(),
                ..b
            },
        )?;
        n += 1;
    }
    Ok(n)
}

// ===========================================================================
// 审批流引擎
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ApprovalStep {
    pub id: i64,
    pub approval_id: i64,
    pub seq: i32,
    pub approver: String,
    pub action: String, // '' / approve / reject
    pub comment: String,
    pub acted_at: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Approval {
    pub id: i64,
    pub biz_kind: String,
    pub biz_id: i64,
    pub title: String,
    pub applicant: String,
    pub current_node: i32,
    pub status: String, // pending / approved / rejected / cancelled
    pub created_at: String,
    pub finished_at: Option<String>,
    pub steps: Vec<ApprovalStep>,
}

fn load_steps(db: &Db, approval_id: i64) -> DbResult<Vec<ApprovalStep>> {
    let mut st = db.conn().prepare(
        "SELECT id,approval_id,seq,approver,action,comment,acted_at FROM approval_step
         WHERE approval_id=?1 ORDER BY seq",
    )?;
    let rows = st
        .query_map(rusqlite::params![approval_id], |r| {
            Ok(ApprovalStep {
                id: r.get(0)?,
                approval_id: r.get(1)?,
                seq: r.get(2)?,
                approver: r.get(3)?,
                action: r.get(4)?,
                comment: r.get(5)?,
                acted_at: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn map_approval(db: &Db, r: &rusqlite::Row) -> rusqlite::Result<Approval> {
    let id: i64 = r.get(0)?;
    Ok(Approval {
        id,
        biz_kind: r.get(1)?,
        biz_id: r.get(2)?,
        title: r.get(3)?,
        applicant: r.get(4)?,
        current_node: r.get(5)?,
        status: r.get(6)?,
        created_at: r.get(7)?,
        finished_at: r.get(8)?,
        steps: load_steps(db, id).unwrap_or_default(),
    })
}

const AP_COLS: &str =
    "id,biz_kind,biz_id,title,applicant,current_node,status,created_at,finished_at";

/// 发起审批流：approvers 按顺序为各级审批人
pub fn approval_start(
    db: &Db,
    biz_kind: &str,
    biz_id: i64,
    title: &str,
    applicant: &str,
    approvers: &[String],
) -> DbResult<i64> {
    if approvers.is_empty() {
        return Err(FinError::msg("审批流至少需要一个审批人").into());
    }
    // 同一单据只能有一个审批流
    let exists: Option<i64> = db
        .conn()
        .query_row(
            "SELECT id FROM approval WHERE biz_kind=?1 AND biz_id=?2",
            rusqlite::params![biz_kind, biz_id],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_some() {
        return Err(FinError::msg("该单据已存在审批流，不能重复发起").into());
    }
    let tx = db.write_tx()?;
    tx.execute(
        "INSERT INTO approval(biz_kind,biz_id,title,applicant,current_node,status,created_at)
         VALUES(?1,?2,?3,?4,1,'pending',?5)",
        rusqlite::params![biz_kind, biz_id, title, applicant, now()],
    )?;
    let id = tx.last_insert_rowid();
    for (i, a) in approvers.iter().enumerate() {
        tx.execute(
            "INSERT INTO approval_step(approval_id,seq,approver) VALUES(?1,?2,?3)",
            rusqlite::params![id, i as i32 + 1, a],
        )?;
    }
    tx.commit()?;
    Ok(id)
}

pub fn approval_get(db: &Db, id: i64) -> DbResult<Option<Approval>> {
    db.conn()
        .query_row(
            &format!("SELECT {AP_COLS} FROM approval WHERE id=?1"),
            rusqlite::params![id],
            |r| map_approval(db, r),
        )
        .optional()
        .map_err(Into::into)
}

pub fn approval_get_for_biz(db: &Db, biz_kind: &str, biz_id: i64) -> DbResult<Option<Approval>> {
    db.conn()
        .query_row(
            &format!("SELECT {AP_COLS} FROM approval WHERE biz_kind=?1 AND biz_id=?2"),
            rusqlite::params![biz_kind, biz_id],
            |r| map_approval(db, r),
        )
        .optional()
        .map_err(Into::into)
}

/// 我待审的列表（当前节点审批人 = who，且流程待审）
pub fn approval_todo(db: &Db, who: &str) -> DbResult<Vec<Approval>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {AP_COLS} FROM approval a WHERE a.status='pending'
           AND EXISTS (SELECT 1 FROM approval_step s
                       WHERE s.approval_id=a.id AND s.seq=a.current_node AND s.approver=?1)
         ORDER BY a.created_at DESC"
    ))?;
    let rows = st
        .query_map(rusqlite::params![who], |r| map_approval(db, r))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 全部审批流（按创建时间倒序，可选 limit）
pub fn approval_list(db: &Db, limit: i64) -> DbResult<Vec<Approval>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {AP_COLS} FROM approval ORDER BY created_at DESC LIMIT ?1"
    ))?;
    let rows = st
        .query_map(rusqlite::params![limit.max(1)], |r| map_approval(db, r))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 审批（通过/驳回）
///
/// 通过：当前节点标记 approve，若有下一节点则推进，否则整单 approved。
/// 驳回：整单 rejected，不再流转。
///
/// 全流程在同一事务里「读 → 条件写」：节点动作与整单状态都用带前置条件的
/// `UPDATE ... WHERE <原状态>`（比较并交换），命中 0 行即被别人抢先或状态已变。
/// 原来的写法把状态判断放在事务外、写入又不带条件，两个人同时点「通过」会各写一次，
/// 后一个还会把前一个审批人的意见整条覆盖掉。
pub fn approval_act(
    db: &Db,
    id: i64,
    who: &str,
    approve: bool,
    comment: &str,
) -> DbResult<Approval> {
    let action = if approve { "approve" } else { "reject" };
    let tx = db.write_tx()?;
    // 事务内读当前状态：审批流本身的可见性也必须在同一事务里判
    let ap = approval_get_of(&tx, id)?.ok_or_else(|| FinError::msg("审批流不存在"))?;
    if ap.status != "pending" {
        return Err(FinError::msg(format!("审批流已{}，不能再操作", ap.status)).into());
    }
    let cur = ap
        .steps
        .iter()
        .find(|s| s.seq == ap.current_node)
        .ok_or_else(|| FinError::msg("当前审批节点异常"))?;
    if cur.approver != who {
        return Err(FinError::msg(format!(
            "当前节点审批人是 {}，您（{}）无权审批",
            cur.approver, who
        ))
        .into());
    }
    // 节点动作：只允许写「还没人动过」的节点，防止重复审批 / 覆盖他人意见
    let n = tx.execute(
        "UPDATE approval_step SET action=?1, comment=?2, acted_at=?3
         WHERE id=?4 AND (action IS NULL OR action='')",
        rusqlite::params![action, comment, now(), cur.id],
    )?;
    if n == 0 {
        return Err(FinError::state("该节点已被审批，请刷新后重试").into());
    }
    if approve {
        let next = ap.current_node + 1;
        let has_next = ap.steps.iter().any(|s| s.seq == next);
        let n = if has_next {
            tx.execute(
                "UPDATE approval SET current_node=?2 WHERE id=?1 AND status='pending' AND current_node=?3",
                rusqlite::params![id, next, ap.current_node],
            )?
        } else {
            tx.execute(
                "UPDATE approval SET status='approved', finished_at=?2
                 WHERE id=?1 AND status='pending' AND current_node=?3",
                rusqlite::params![id, now(), ap.current_node],
            )?
        };
        if n == 0 {
            return Err(FinError::state("审批流状态已被他人变更，请刷新后重试").into());
        }
    } else {
        let n = tx.execute(
            "UPDATE approval SET status='rejected', finished_at=?2
             WHERE id=?1 AND status='pending' AND current_node=?3",
            rusqlite::params![id, now(), ap.current_node],
        )?;
        if n == 0 {
            return Err(FinError::state("审批流状态已被他人变更，请刷新后重试").into());
        }
    }
    tx.commit()?;
    approval_get(db, id)?.ok_or_else(|| FinError::msg("审批流读取失败").into())
}

/// 撤销审批流（仅申请人、且还在第一节点）
///
/// 同样用条件更新（pending + 仍在第一节点），避免与并发的审批动作互相覆盖。
pub fn approval_cancel(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    let ap = approval_get_of(&tx, id)?.ok_or_else(|| FinError::msg("审批流不存在"))?;
    if ap.applicant != who {
        return Err(FinError::msg("只有申请人可以撤销审批流").into());
    }
    if ap.status != "pending" || ap.current_node > 1 {
        return Err(FinError::msg("审批已流转，不能撤销").into());
    }
    let n = tx.execute(
        "UPDATE approval SET status='cancelled', finished_at=?2
         WHERE id=?1 AND status='pending' AND current_node=1",
        rusqlite::params![id, now()],
    )?;
    if n == 0 {
        return Err(FinError::state("审批流状态已被他人变更，请刷新后重试").into());
    }
    tx.commit()?;
    Ok(())
}

/// 同 [`approval_get`]，但只依赖连接：审批的「读状态 → 条件写」必须在同一事务内。
fn approval_get_of(conn: &rusqlite::Connection, id: i64) -> DbResult<Option<Approval>> {
    let ap = conn
        .query_row(
            &format!("SELECT {AP_COLS} FROM approval WHERE id=?1"),
            rusqlite::params![id],
            map_approval_head,
        )
        .optional()?;
    let Some(mut ap) = ap else {
        return Ok(None);
    };
    let mut st = conn.prepare(
        "SELECT id,approval_id,seq,approver,action,comment,acted_at FROM approval_step
         WHERE approval_id=?1 ORDER BY seq",
    )?;
    ap.steps = st
        .query_map(rusqlite::params![id], |r| {
            Ok(ApprovalStep {
                id: r.get(0)?,
                approval_id: r.get(1)?,
                seq: r.get(2)?,
                approver: r.get(3)?,
                action: r.get(4)?,
                comment: r.get(5)?,
                acted_at: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(ap))
}

/// 只映射表头（分录由调用方另行加载）
fn map_approval_head(r: &rusqlite::Row) -> rusqlite::Result<Approval> {
    let id: i64 = r.get(0)?;
    Ok(Approval {
        id,
        biz_kind: r.get(1)?,
        biz_id: r.get(2)?,
        title: r.get(3)?,
        applicant: r.get(4)?,
        current_node: r.get(5)?,
        status: r.get(6)?,
        created_at: r.get(7)?,
        finished_at: r.get(8)?,
        steps: Vec::new(),
    })
}

// ===========================================================================
// 报表附注
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReportNote {
    pub id: i64,
    pub report_key: String,
    pub period: Period,
    pub seq: i32,
    pub title: String,
    pub content: String,
    pub updated_by: String,
    pub updated_at: String,
}

fn map_note(r: &rusqlite::Row) -> rusqlite::Result<ReportNote> {
    Ok(ReportNote {
        id: r.get(0)?,
        report_key: r.get(1)?,
        period: Period::from_ymm(r.get(2)?),
        seq: r.get(3)?,
        title: r.get(4)?,
        content: r.get(5)?,
        updated_by: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

const NOTE_COLS: &str = "id,report_key,period,seq,title,content,updated_by,updated_at";

pub fn note_list(db: &Db, report_key: &str, period: Period) -> DbResult<Vec<ReportNote>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {NOTE_COLS} FROM report_note WHERE report_key=?1 AND period=?2 ORDER BY seq"
    ))?;
    let rows = st
        .query_map(rusqlite::params![report_key, period.ymm()], map_note)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn note_save(db: &Db, n: &mut ReportNote) -> DbResult<i64> {
    n.updated_at = now();
    if n.id > 0 {
        db.conn().execute(
            "UPDATE report_note SET report_key=?2,period=?3,seq=?4,title=?5,content=?6,updated_by=?7,updated_at=?8
             WHERE id=?1",
            rusqlite::params![
                n.id,
                n.report_key,
                n.period.ymm(),
                n.seq,
                n.title,
                n.content,
                n.updated_by,
                n.updated_at
            ],
        )?;
        Ok(n.id)
    } else {
        db.conn().execute(
            "INSERT INTO report_note(report_key,period,seq,title,content,updated_by,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                n.report_key,
                n.period.ymm(),
                n.seq,
                n.title,
                n.content,
                n.updated_by,
                n.updated_at
            ],
        )?;
        Ok(db.conn().last_insert_rowid())
    }
}

pub fn note_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM report_note WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

// ===========================================================================
// 会计电子档案
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct EArchive {
    pub id: i64,
    pub period: Period,
    pub kind: String, // voucher / ledger / report / balance
    pub title: String,
    pub file_no: String,
    pub content_hash: String,
    pub payload: String,
    pub archived_by: String,
    pub archived_at: String,
    pub sealed: bool,
}

fn map_archive(r: &rusqlite::Row) -> rusqlite::Result<EArchive> {
    Ok(EArchive {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        kind: r.get(2)?,
        title: r.get(3)?,
        file_no: r.get(4)?,
        content_hash: r.get(5)?,
        payload: r.get(6)?,
        archived_by: r.get(7)?,
        archived_at: r.get(8)?,
        sealed: r.get::<_, i64>(9)? != 0,
    })
}

const ARC_COLS: &str =
    "id,period,kind,title,file_no,content_hash,payload,archived_by,archived_at,sealed";

fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// 归档：内容哈希 + 封存。相同 (period, kind, file_no) 重复归档会报错，保证档案唯一。
pub fn archive_create(
    db: &Db,
    period: Period,
    kind: &str,
    title: &str,
    file_no: &str,
    payload: &str,
    user: &str,
) -> DbResult<i64> {
    let hash = sha256_hex(payload);
    db.conn().execute(
        "INSERT INTO e_archive(period,kind,title,file_no,content_hash,payload,archived_by,archived_at,sealed)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1)",
        rusqlite::params![period.ymm(), kind, title, file_no, hash, payload, user, now()],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn archive_list(db: &Db, period: Period, kind: Option<&str>) -> DbResult<Vec<EArchive>> {
    let sql = match kind {
        Some(_) => {
            format!("SELECT {ARC_COLS} FROM e_archive WHERE period=?1 AND kind=?2 ORDER BY file_no")
        }
        None => format!("SELECT {ARC_COLS} FROM e_archive WHERE period=?1 ORDER BY kind, file_no"),
    };
    let mut st = db.conn().prepare(&sql)?;
    let rows = match kind {
        Some(k) => st.query_map(rusqlite::params![period.ymm(), k], map_archive)?,
        None => st.query_map(rusqlite::params![period.ymm()], map_archive)?,
    };
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn archive_get(db: &Db, id: i64) -> DbResult<Option<EArchive>> {
    db.conn()
        .query_row(
            &format!("SELECT {ARC_COLS} FROM e_archive WHERE id=?1"),
            rusqlite::params![id],
            map_archive,
        )
        .optional()
        .map_err(Into::into)
}

/// 校验档案完整性（内容是否被篡改）
pub fn archive_verify(a: &EArchive) -> bool {
    sha256_hex(&a.payload) == a.content_hash
}

/// 自动生成档案号：YYYYMM-kind-seq
///
/// 序号取已有档案号的**最大数字后缀 + 1**，不是 `COUNT(*) + 1`：
/// 删掉一张档案后 COUNT 会回落，下一张就复用了已存在的号，撞上
/// `e_archive` 的 UNIQUE(period, kind, file_no)——而档案号的唯一性正是
/// 电子会计档案「同一期间同一类目不得重号」合规项的硬要求。
pub fn archive_next_no(db: &Db, period: Period, kind: &str) -> DbResult<String> {
    let mut st = db
        .conn()
        .prepare("SELECT file_no FROM e_archive WHERE period=?1 AND kind=?2")?;
    let rows = st
        .query_map(rusqlite::params![period.ymm(), kind], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(st);
    let max = rows
        .iter()
        .filter_map(|no| {
            // 号的形状是 `YYYYMM-kind-NNN`，取最后一段数字；非本函数生成的号忽略
            no.rsplit('-')
                .next()
                .and_then(|tail| tail.trim().parse::<i64>().ok())
        })
        .max()
        .unwrap_or(0);
    Ok(format!("{}-{}-{:03}", period.ymm(), kind, max + 1))
}

// ===========================================================================
// 摘要汇总表
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SummaryRow {
    pub summary: String,
    pub voucher_count: i64,
    pub debit: Money,
    pub credit: Money,
}

/// 摘要汇总表：期间范围内按摘要分组，统计凭证张数与借贷发生额。
///
/// 只统计已过账凭证（status='posted'），金额 Rust 侧 Decimal 累加。
pub fn summary_table(
    db: &Db,
    from: Period,
    to: Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<Vec<SummaryRow>> {
    let mut sql = String::from(
        "SELECT e.summary, e.debit, e.credit, v.id
         FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
         WHERE v.status='posted' AND e.period BETWEEN ?1 AND ?2 AND e.summary <> ''",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(from.ymm()), Box::new(to.ymm())];
    if let Some(u) = scope {
        crate::push_report_scope(&mut sql, &mut params, &u.data_scope, &u.username);
    }
    sql.push_str(" ORDER BY e.summary");
    let mut st = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut rows = st.query(refs.as_slice())?;
    let mut map: std::collections::BTreeMap<
        String,
        (Money, Money, std::collections::BTreeSet<i64>),
    > = std::collections::BTreeMap::new();
    while let Some(r) = rows.next()? {
        let summary: String = r.get(0)?;
        let d = read_m(&r.get::<_, String>(1)?);
        let c = read_m(&r.get::<_, String>(2)?);
        let vid: i64 = r.get(3)?;
        let e = map
            .entry(summary)
            .or_insert_with(|| (Money::ZERO, Money::ZERO, std::collections::BTreeSet::new()));
        e.0 += d;
        e.1 += c;
        e.2.insert(vid);
    }
    Ok(map
        .into_iter()
        .map(|(summary, (debit, credit, vids))| SummaryRow {
            summary,
            voucher_count: vids.len() as i64,
            debit,
            credit,
        })
        .collect())
}

// ===========================================================================
// 多栏账（增强版：按对方科目拆栏）
// ===========================================================================

/// 多栏账行：一行一笔发生额，各栏科目拆分到列
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct MultiColRow {
    pub date: String,
    pub voucher_no: String,
    pub summary: String,
    /// 主科目本行发生额（带符号：借正贷负）
    pub amount: Money,
    /// 各栏对方科目的金额（与 columns 顺序对应）
    pub cols: Vec<Money>,
    /// 余额（主科目累计）
    pub balance: Money,
}

/// 多栏账：以 main_code 为主科目，columns 为栏目科目（通常是对方的费用/成本明细），
/// 按凭证逐行把主科目发生额拆到各栏目。
///
/// 典型用法：管理费用多栏账 main=6602，columns=660201..660212，
/// 看每张凭证里 6602 的钱分别进了哪些费用子目。
/// 这里更通用的实现是：主科目可以是任一上级（如 6602），
/// 栏目取凭证中属于 columns 前缀的分录金额。
pub fn multi_column_table(
    db: &Db,
    main_code: &str,
    columns: &[String],
    from: Period,
    to: Period,
    user: Option<&fincore::user::User>,
) -> DbResult<Vec<MultiColRow>> {
    // 主科目的全部已过账分录（含数据范围过滤：仅本人凭证 / 科目区间）
    let mut sql = String::from(
        "SELECT v.date, v.word, v.no, e.summary, e.debit, e.credit, e.voucher_id
         FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
         WHERE v.status='posted' AND e.period BETWEEN ?1 AND ?2
           AND e.account_code LIKE ?3 ESCAPE '\\'",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
        Box::new(from.ymm()),
        Box::new(to.ymm()),
        Box::new(format!("{}%", crate::escape_like(main_code))),
    ];
    if let Some(u) = user {
        if u.data_scope.own_voucher_only {
            params.push(Box::new(u.username.clone()));
            sql.push_str(&format!(" AND v.prepared_by = ?{}", params.len()));
        }
        let lo = u.data_scope.account_from.trim();
        let hi = u.data_scope.account_to.trim();
        if !lo.is_empty() {
            params.push(Box::new(lo.to_string()));
            sql.push_str(&format!(" AND e.account_code >= ?{}", params.len()));
        }
        if !hi.is_empty() {
            params.push(Box::new(hi.to_string()));
            sql.push_str(&format!(" AND e.account_code <= ?{}", params.len()));
        }
    }
    sql.push_str(" ORDER BY v.date, v.id, e.line");
    let mut st = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut rows = st.query(refs.as_slice())?;

    // 主科目分录：voucher_id -> (date, no, summary, amount)
    struct MainLine {
        date: String,
        vno: String,
        summary: String,
        amount: Money,
    }
    let mut mains: Vec<(i64, MainLine)> = Vec::new();
    let mut vids: Vec<i64> = Vec::new();
    while let Some(r) = rows.next()? {
        let vid: i64 = r.get(6)?;
        let d = read_m(&r.get::<_, String>(4)?);
        let c = read_m(&r.get::<_, String>(5)?);
        let amount = d - c;
        let word: String = r.get(1)?;
        let no: i32 = r.get(2)?;
        mains.push((
            vid,
            MainLine {
                date: r.get(0)?,
                vno: format!("{}-{}", word, no),
                summary: r.get(3)?,
                amount,
            },
        ));
        vids.push(vid);
    }
    if mains.is_empty() {
        return Ok(Vec::new());
    }

    // 拉取这些凭证中所有属于栏目科目的分录
    vids.sort_unstable();
    vids.dedup();
    let placeholders = vids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT voucher_id, account_code, debit, credit FROM voucher_entry
         WHERE voucher_id IN ({placeholders})"
    );
    let mut st2 = db.conn().prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = vids
        .iter()
        .map(|i| i as &dyn rusqlite::types::ToSql)
        .collect();
    let mut rr = st2.query(params.as_slice())?;
    // vid -> Vec<(col_idx, amount)>
    let mut colmap: std::collections::BTreeMap<i64, Vec<(usize, Money)>> =
        std::collections::BTreeMap::new();
    while let Some(r) = rr.next()? {
        let vid: i64 = r.get(0)?;
        let code: String = r.get(1)?;
        let d = read_m(&r.get::<_, String>(2)?);
        let c = read_m(&r.get::<_, String>(3)?);
        // 对方科目方向与主科目相反：主科目借，对方取贷方；这里直接取符号差
        let amount = d - c;
        for (idx, col) in columns.iter().enumerate() {
            if code.starts_with(col.as_str()) {
                colmap.entry(vid).or_default().push((idx, amount));
                break;
            }
        }
    }

    let mut out = Vec::new();
    let mut balance = Money::ZERO;
    for (vid, m) in mains {
        let mut cols = vec![Money::ZERO; columns.len()];
        if let Some(v) = colmap.get(&vid) {
            for (idx, a) in v {
                if *idx < cols.len() {
                    cols[*idx] += *a;
                }
            }
        }
        balance += m.amount;
        out.push(MultiColRow {
            date: m.date,
            voucher_no: m.vno,
            summary: m.summary,
            amount: m.amount,
            cols,
            balance,
        });
    }
    Ok(out)
}

// ===========================================================================
// 财务指标分析
// ===========================================================================

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FinRatio {
    pub key: String,
    pub name: String,
    pub value: Money,
    /// 展示文本（比率带 %，倍数带"倍"，天数带"天"）
    pub display: String,
    pub formula: String,
}

/// 财务概况总量（管理员「账目总览」与财务指标共用同一口径）
#[derive(Clone, Debug)]
pub struct FinTotals {
    /// 流动资产
    pub cur_asset: Money,
    /// 速动资产（流动资产 − 存货）
    pub quick_asset: Money,
    /// 流动负债
    pub cur_liab: Money,
    /// 资产总额
    pub total_asset: Money,
    /// 负债总额
    pub total_liab: Money,
    /// 所有者权益
    pub equity: Money,
    /// 营业收入
    pub revenue: Money,
    /// 营业成本
    pub cost: Money,
    /// 净利润
    pub net_profit: Money,
}

/// 计算 [from, period] 区间的财务总量，口径与资产负债表 / 利润表一致
pub fn financial_totals(
    db: &Db,
    period: Period,
    from: Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<FinTotals> {
    fin_totals_impl(db, period, from, scope, Flow::Range)
}

/// 与 [`financial_totals`] 同口径，但损益取**本年累计**（1 月至 `period`）。
///
/// 为什么单独有这个函数：首页要同时显示「这个月赚多少」和「今年累计赚多少」。
/// 原来后者是再调一次 `financial_totals(db, cur, 1 月, ..)` —— 那是一组**不同的
/// 查询键**，等于把整本账又扫了一遍（6000 张凭证规模下约 8ms）。
///
/// 其实不用扫：余额快照里 `ytd_debit/ytd_credit` 本来就是「1 月至本期」的累计
/// 发生额（`BalanceSnapshot::load_raw` 已经算好了），资产/负债又都是**时点数**，
/// 与区间无关。所以只要把 `from` 传成 `period`（查询键与 `financial_totals` 完全
/// 相同，命中同一份缓存），再把损益读成 `ytd_*`，累计数就白拿了。
///
/// **调用时 `from` 必须等于 `period`**，否则这笔账会退化成再扫一遍全表。
pub fn financial_ytd(
    db: &Db,
    period: Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<FinTotals> {
    fin_totals_impl(db, period, period, scope, Flow::Ytd)
}

/// 损益取数用哪一列发生额
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// 区间发生额（`debit/credit`）：`from..=period`
    Range,
    /// 本年累计发生额（`ytd_debit/ytd_credit`）：1 月至 `period`
    Ytd,
}

fn fin_totals_impl(
    db: &Db,
    period: Period,
    from: Period,
    scope: Option<&fincore::user::User>,
    flow: Flow,
) -> DbResult<FinTotals> {
    use crate::balances::{BalanceQuery, BalanceSnapshot};
    let mut bq = BalanceQuery::range(from, period);
    if let Some(u) = scope {
        bq = bq.with_user_scope(u);
    }
    let snap = BalanceSnapshot::load(db, &bq)?;
    let b = |code: &str| snap.for_account(code, None).end();

    // 损益取数必须排除结转损益凭证（source='carry_forward'），否则结完账后
    // 收入/成本/净利润会被结转分录冲成 0。资产负债表侧仍用含结转的快照：
    // 结转只影响损益与权益科目，不影响资产/负债合计。
    let mut pl_bq = BalanceQuery::range(from, period).with_exclude_carry(true);
    if let Some(u) = scope {
        pl_bq = pl_bq.with_user_scope(u);
    }
    let pl_snap = BalanceSnapshot::load(db, &pl_bq)?;
    // 损益取数：`Flow::Ytd` 读本年累计列，其余读区间列。两者都来自**同一份**
    // 快照，所以「本期」和「本年累计」不需要各扫一遍全表。
    let occ = |code: &str| {
        let r = pl_snap.for_account(code, None);
        match flow {
            Flow::Range => r.credit - r.debit, // 收入类贷方正
            Flow::Ytd => r.ytd_credit - r.ytd_debit,
        }
    };
    let exp = |code: &str| {
        let r = pl_snap.for_account(code, None);
        match flow {
            Flow::Range => r.debit - r.credit, // 费用类借方正
            Flow::Ytd => r.ytd_debit - r.ytd_credit,
        }
    };
    // 流动资产 ≈ 货币资金 + 交易性金融资产 + 应收 + 预付 + 其他应收 + 存货
    let cur_asset = b("1001")
        + b("1002")
        + b("1012")
        + b("1101")
        + b("1121")
        + b("1122")
        + b("1123")
        + b("1221")
        + b("1403")
        + b("1405")
        + b("1406")
        + b("1411");
    // 流动负债 ≈ 短期借款 + 应付 + 预收 + 薪酬 + 税费 + 其他应付
    let cur_liab =
        (b("2001") + b("2201") + b("2202") + b("2203") + b("2211") + b("2221") + b("2241"))
            .negated(); // 负债贷方余额为负，取负得正数
                        // 科目表只读一次。原来这两个合计各读一遍科目表，而 financial_analysis
                        // 要逐月调这个函数，10 个月就是 20 次全表读——纯浪费。
    let accounts = crate::accounts::list(db)?;
    let total_asset = {
        // 全部 1 开头资产类（借方正）合计
        let mut s = Money::ZERO;
        for a in accounts.iter().filter(|a| {
            matches!(a.category, fincore::account::AcctCategory::Asset) && a.code.len() == 4
        }) {
            s += b(&a.code);
        }
        s
    };
    let total_liab = {
        let mut s = Money::ZERO;
        for a in accounts.iter().filter(|a| {
            matches!(a.category, fincore::account::AcctCategory::Liability) && a.code.len() == 4
        }) {
            s += b(&a.code).negated();
        }
        s
    };
    let equity = total_asset - total_liab;
    let revenue = occ("6001") + occ("6051");
    // 「未结转的成本类科目」也要计入当期成本。
    //
    // 背景：成本类科目（`5001 生产成本`、`5101 制造费用`）不是损益类，
    // `AcctCategory::is_profit_loss()` 不含 Cost —— 这是对的，它们要先归集
    // 再结转进 6401。但小厂往往**不做成本核算**（不核算单位成本、不算产成品），
    // 房租、水电、煤气、外发加工费就直接挂在 5101 各明细上，指望期末结转。
    //
    // 如果利润表只认 6401/6402，这些真实发生的支出会从利润里**整体消失**，
    // 净利润凭空虚高 —— 账看着是平的，赚没赚钱却是假的。这种错最危险，
    // 因为它不报错、不报警，只有老板觉得"怎么利润这么高"才发现。
    //
    // 所以：把成本类科目的**当前余额**计入当期成本。它们的余额就是"已发生
    // 但还没转到主营业务成本"的部分。真做了结转的账套，这些余额会被结转
    // 冲成 0，不会重复计算。
    let unrecovered_cost = {
        let accounts = crate::accounts::list(db)?;
        let mut s = Money::ZERO;
        for a in accounts.iter() {
            if !matches!(a.category, fincore::account::AcctCategory::Cost) {
                continue;
            }
            // 只取末级：父科目余额是子科目之和，重复相加会翻倍
            if accounts
                .iter()
                .any(|x| x.code.starts_with(&a.code) && x.code != a.code)
            {
                continue;
            }
            s += exp(&a.code);
        }
        s
    };
    let cost = exp("6401") + exp("6402") + unrecovered_cost;
    let net_profit = revenue - cost - exp("6403") - exp("6601") - exp("6602") - exp("6603")
        - exp("6701")
        + occ("6101") // 公允价值变动收益
        + occ("6111") // 投资收益
        + occ("6301")
        - exp("6711")
        - exp("6801");
    // 速动资产 = 流动资产 − 存货（1403 原材料 / 1405 库存商品 / 1406 发出商品 / 1411 周转材料）
    let quick_asset = cur_asset - b("1403") - b("1405") - b("1406") - b("1411");
    Ok(FinTotals {
        cur_asset,
        quick_asset,
        cur_liab,
        total_asset,
        total_liab,
        equity,
        revenue,
        cost,
        net_profit,
    })
}

/// 常用财务指标：偿债能力 / 营运能力 / 盈利能力
///
/// 全部基于期末余额快照计算，数据源是 BalanceSnapshot，口径与资产负债表一致。
pub fn fin_ratios(
    db: &Db,
    period: Period,
    from: Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<Vec<FinRatio>> {
    let t = financial_totals(db, period, from, scope)?;
    let (cur_asset, cur_liab) = (t.cur_asset, t.cur_liab);
    let (total_asset, total_liab, equity) = (t.total_asset, t.total_liab, t.equity);
    let (revenue, cost, net_profit) = (t.revenue, t.cost, t.net_profit);

    let pct = |v: Option<Money>| match v {
        Some(x) => format!("{}%", (x * Money::from_i64(100)).round2()),
        None => "—".to_string(),
    };
    let times = |v: Option<Money>| match v {
        Some(x) => format!("{}倍", x),
        None => "—".to_string(),
    };
    let val = |v: Option<Money>| v.unwrap_or(Money::ZERO);

    let mut out = Vec::new();
    let mut push = |key: &str, name: &str, value: Money, display: String, formula: &str| {
        out.push(FinRatio {
            key: key.into(),
            name: name.into(),
            value,
            display,
            formula: formula.into(),
        });
    };

    // 分母阈值：绝对值小于 1 分钱视为数据不足，比率无意义（避免分母接近 0 时算出 100% 之类的假值）
    let div = |a: Money, b: Money| -> Option<Money> {
        if b.abs() < Money::from_cents(1) {
            None
        } else {
            Some(Money::new(a.inner() / b.inner()).round2())
        }
    };

    let current_ratio = div(cur_asset, cur_liab);
    push(
        "current_ratio",
        "流动比率",
        val(current_ratio),
        times(current_ratio),
        "流动资产 ÷ 流动负债",
    );
    let quick_ratio = div(t.quick_asset, cur_liab);
    push(
        "quick_ratio",
        "速动比率",
        val(quick_ratio),
        times(quick_ratio),
        "(流动资产 − 存货) ÷ 流动负债",
    );
    let debt_ratio = div(total_liab, total_asset);
    push(
        "debt_ratio",
        "资产负债率",
        val(debt_ratio),
        pct(debt_ratio),
        "负债总额 ÷ 资产总额",
    );
    let gross_margin = div(revenue - cost, revenue);
    push(
        "gross_margin",
        "毛利率",
        val(gross_margin),
        pct(gross_margin),
        "(营业收入 − 营业成本) ÷ 营业收入",
    );
    let net_margin = div(net_profit, revenue);
    push(
        "net_margin",
        "净利率",
        val(net_margin),
        pct(net_margin),
        "净利润 ÷ 营业收入",
    );
    let roe = div(net_profit, equity);
    push(
        "roe",
        "净资产收益率(ROE)",
        val(roe),
        pct(roe),
        "净利润 ÷ 所有者权益",
    );
    let roa = div(net_profit, total_asset);
    push(
        "roa",
        "总资产报酬率(ROA)",
        val(roa),
        pct(roa),
        "净利润 ÷ 资产总额",
    );
    Ok(out)
}

// ---------------------------------------------------------------------------
// 财务分析：逐月趋势 / 指标内因构成 / 异常检测（供管理员只读总览）
// ---------------------------------------------------------------------------

/// 逐月趋势点
#[derive(Clone, Debug)]
pub struct TrendPoint {
    pub period: Period,
    /// 当月营业收入
    pub revenue: Money,
    /// 当月营业成本
    pub cost: Money,
    /// 当月净利润
    pub net_profit: Money,
    /// 年初至今累计营业收入
    pub cum_revenue: Money,
    /// 年初至今累计营业成本
    pub cum_cost: Money,
    /// 年初至今累计净利润
    pub cum_net_profit: Money,
    pub anomaly_revenue: bool,
    pub anomaly_cost: bool,
    pub anomaly_net_profit: bool,
}

/// 指标内因构成项（营业收入/营业成本/净利润的驱动科目或利润表行）
#[derive(Clone, Debug)]
pub struct DriverItem {
    pub code: String,
    pub name: String,
    /// 本月金额（有符号：收入类为正，成本费用类为负）
    pub amount: Money,
    /// 上月金额（环比基准）
    pub prev_amount: Money,
}

/// 首页「钱在哪」一行的往来单位明细
#[derive(Clone, Debug)]
pub struct PartyMoney {
    /// 往来单位编码（裸编码，如 `C001` / `S001`）
    pub code: String,
    /// 往来单位名称
    pub name: String,
    /// 未结清金额（正数）
    pub amount: Money,
    /// 最老一笔的天数（0 表示刚发生）
    pub max_days: i64,
    /// 该单位同时也是另一种身份时，另一方向的金额（轧差用）
    ///
    /// 例：宏源既是外发加工商（我欠他 28,000）又拿货来加工（他欠我 15,000）。
    /// 显示为「我欠宏源 28,000」，`also: 15,000`，`also_is_receivable: true`，
    /// 老板一眼看到净欠 13,000 —— 而不是自己去两张表里心算。
    pub also: Money,
    /// `also` 的方向：true 表示"他欠我"，false 表示"我欠他"
    pub also_is_receivable: bool,
}

impl PartyMoney {
    /// 与该单位的净额，**永远是正数**；方向看 [`PartyMoney::net_is_receivable`]。
    ///
    /// `amount` 是"本行方向上我方的未结金额"，`also` 是"另一个方向上对方的金额"，
    /// 两者方向相反，所以相减取绝对差。
    ///
    /// 为什么不带符号：界面上一行写成「净我欠他 -13,000」是双重否定，
    /// 老板会读错。金额只表态"多少钱"，方向单独用一句话说清楚。
    pub fn net_with(&self) -> Money {
        if self.also.is_zero() {
            return self.amount;
        }
        (self.amount - self.also).round2().abs()
    }

    /// 净额的方向是否为"他欠我"。
    ///
    /// 方向要看**本行自己**是哪个方向，再由 `amount` 与 `also` 谁大决定是否翻转：
    /// - 客户行：`amount` 是"他欠我"。`amount >= also` → 他仍净欠我；
    ///   否则我净欠他（他预付得更多）。
    /// - 供应商行：`amount` 是"我欠他"。`amount >= also` → 我仍净欠他；
    ///   否则他净欠我（我预付得更多）。
    ///
    /// `also_is_receivable` 记录的是"另一方向是不是他欠我"，等价于
    /// "本行是供应商行"（来自供应商侧匹配时 also 是客户金额）。
    pub fn net_is_receivable(&self) -> bool {
        // 本行是客户行 ⇔ 另方向（also）不是我欠他，即 also_is_receivable == false
        let this_row_is_customer = !self.also_is_receivable;
        if this_row_is_customer {
            // 他欠我 amount，我欠他 also
            self.amount >= self.also
        } else {
            // 我欠他 amount，他欠我 also
            self.amount < self.also
        }
    }
}

/// 首页「钱在哪」：应收应付合到一起看
///
/// 为什么单列一个结构而不复用账龄报表：账龄报表是按科目拆的（112201 一张、
/// 220201 另一张），老板要的是**一句话**——"别人欠我多少、我欠别人多少、
/// 我自己手里有多少"。三个数并排才有判断力，分开看永远做不出决定。
#[derive(Clone, Debug)]
pub struct MoneyPosition {
    /// 客户欠我的（应收未收）
    pub receivable: Money,
    /// 我欠供应商的（应付未付）
    pub payable: Money,
    /// 客户预收我的（我欠客户的钱/货）
    pub advance_in: Money,
    /// 我预付供应商的（供应商欠我的货）
    pub advance_out: Money,
    /// 自己手里能动用的钱（现金 + 银行）
    pub cash: Money,
    /// 应收毛额（各户借方未轧差之和，用于看清"总共赊出去多少"）
    pub receivable_gross: Money,
    /// 应付毛额（各户贷方未轧差之和）
    pub payable_gross: Money,
    /// 欠我最多的客户（按金额降序，最多 5 个）
    pub top_customers: Vec<PartyMoney>,
    /// 我欠最多的供应商（按金额降序，最多 5 个）
    pub top_suppliers: Vec<PartyMoney>,
    /// 超过 90 天还没收回的应收总额（催收重点）
    pub overdue_90: Money,
    /// 超过 90 天还没收回的客户家数
    pub overdue_90_count: usize,
}

impl MoneyPosition {
    /// 净头寸 = 应收 + 预付 − 应付 − 预收
    ///
    /// 正数表示别人欠我的多，负数表示我欠别人的多。这个数加上手头现金
    /// 就是"这家厂实际的家底"——利润表看不出来这个。
    pub fn net(&self) -> Money {
        self.receivable - self.payable + self.advance_out - self.advance_in
    }

    /// 欠款总天数加权：把最老那笔的天数摊出来，提醒老板"这钱压多久了"
    pub fn worst_days(&self) -> i64 {
        self.top_customers
            .iter()
            .chain(self.top_suppliers.iter())
            .map(|p| p.max_days)
            .max()
            .unwrap_or(0)
    }
}

/// 财务分析结果
#[derive(Clone, Debug)]
pub struct FinancialAnalysis {
    pub trend: Vec<TrendPoint>,
    pub revenue_drivers: Vec<DriverItem>,
    pub cost_drivers: Vec<DriverItem>,
    /// 净利润构成（利润表口径：收入、成本、各项费用，正=增利、负=减利）
    pub profit_drivers: Vec<DriverItem>,
    /// 异常说明（人类可读）
    pub anomaly_notes: Vec<String>,
    /// 偿债/营运/盈利指标（复用 fin_ratios）
    pub ratios: Vec<FinRatio>,
}

/// 异常检测：均值 ± 2σ 之外视为异常（样本过少或方差为 0 时不标记）
fn flag_series(vals: &[Money]) -> Vec<bool> {
    if vals.len() < 3 {
        return vec![false; vals.len()];
    }
    let f: Vec<f64> = vals.iter().map(|v| v.to_f64()).collect();
    let n = f.len() as f64;
    let mean = f.iter().sum::<f64>() / n;
    let var = f.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    let std = var.sqrt();
    if std.abs() < 1e-6 {
        return vec![false; vals.len()];
    }
    f.iter().map(|x| (x - mean).abs() > 2.0 * std).collect()
}

/// 营业成本构成的科目清单：6401 主营业务成本、6402 其他业务成本，
/// 后面再跟上全部**成本类末级科目**（5101 制造费用、510101 房租…）。
///
/// 为什么要有这个共享清单：`financial_totals` 的净利润口径已经把成本类
/// 科目算进"营业成本"，成本构成就必须列在同一份清单上。两处各写一遍的
/// 后果是"明细加起来对不上合计"，这是最容易被老板当场质疑的一类不一致。
pub fn cost_driver_accounts(db: &Db) -> DbResult<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = vec![
        ("6401".to_string(), "主营业务成本".to_string()),
        ("6402".to_string(), "其他业务成本".to_string()),
    ];
    let accounts = crate::accounts::list(db)?;
    let is_leaf = |code: &str| {
        !accounts
            .iter()
            .any(|x| x.code.starts_with(code) && x.code != code)
    };
    for a in accounts.iter() {
        if !matches!(a.category, fincore::account::AcctCategory::Cost) || !is_leaf(&a.code) {
            continue;
        }
        out.push((a.code.clone(), a.name.clone()));
    }
    Ok(out)
}

/// 某区间的营业成本构成（首页「钱花哪了 Top3」专用）。
///
/// 与 `financial_analysis(..).cost_drivers` 的差别只在**取数范围**：
/// 这里只装一个剔除结转的快照、只算成本这一类；那边为了画整年趋势要
/// 逐月调 `financial_totals`（10 个月 = 10 次全表扫，实测 168ms）。
/// 首页只要前三条，为它算一整年的时间序列纯属浪费——优化前首页
/// 233ms 里有 160ms 花在这上面。
pub fn cost_drivers_of(
    db: &Db,
    period: Period,
    from: Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<Vec<DriverItem>> {
    use crate::balances::{BalanceQuery, BalanceSnapshot};
    let mut bq = BalanceQuery::range(from, period).with_exclude_carry(true);
    if let Some(u) = scope {
        bq = bq.with_user_scope(u);
    }
    let snap = BalanceSnapshot::load(db, &bq)?;
    let mut out: Vec<DriverItem> = Vec::new();
    for (code, name) in cost_driver_accounts(db)? {
        let r = snap.for_account(&code, None);
        // 成本类都是借方科目，支出为正
        let amount = r.debit - r.credit;
        if amount.is_zero() {
            continue;
        }
        out.push(DriverItem {
            code,
            name,
            amount,
            prev_amount: fincore::Money::ZERO,
        });
    }
    // 金额大的排前面：老板要的是"钱主要花哪了"，不是科目表顺序
    out.sort_by(|x, y| {
        y.amount
            .to_f64()
            .partial_cmp(&x.amount.to_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// 计算 [1月, period] 区间的逐月趋势与异常，并给出指标内因构成
pub fn financial_analysis(
    db: &Db,
    period: Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<FinancialAnalysis> {
    use crate::balances::{BalanceQuery, BalanceSnapshot};

    let jan = Period::new(period.year(), 1).unwrap_or(period);

    // ---- 逐月趋势 ----
    let mut months: Vec<Period> = Vec::new();
    let mut m = jan;
    loop {
        if m > period {
            break;
        }
        months.push(m);
        m = m.next();
    }

    let mut trend: Vec<TrendPoint> = Vec::with_capacity(months.len());
    let mut cum_rev = Money::ZERO;
    let mut cum_cost = Money::ZERO;
    let mut cum_np = Money::ZERO;
    for mo in &months {
        let t = financial_totals(db, *mo, *mo, scope)?;
        cum_rev += t.revenue;
        cum_cost += t.cost;
        cum_np += t.net_profit;
        trend.push(TrendPoint {
            period: *mo,
            revenue: t.revenue,
            cost: t.cost,
            net_profit: t.net_profit,
            cum_revenue: cum_rev,
            cum_cost,
            cum_net_profit: cum_np,
            anomaly_revenue: false,
            anomaly_cost: false,
            anomaly_net_profit: false,
        });
    }

    // 异常标记（均值 ± 2σ）
    let rev_flags = flag_series(&trend.iter().map(|t| t.revenue).collect::<Vec<_>>());
    let cost_flags = flag_series(&trend.iter().map(|t| t.cost).collect::<Vec<_>>());
    let np_flags = flag_series(&trend.iter().map(|t| t.net_profit).collect::<Vec<_>>());
    let mut anomaly_notes: Vec<String> = Vec::new();
    for (i, tp) in trend.iter_mut().enumerate() {
        tp.anomaly_revenue = rev_flags[i];
        tp.anomaly_cost = cost_flags[i];
        // 净利润异常：统计离群 或 当月亏损
        tp.anomaly_net_profit = np_flags[i] || tp.net_profit.is_negative();
        if tp.anomaly_revenue {
            anomaly_notes.push(format!(
                "{} 营业收入 {} 显著偏离年内均值，请核查收入确认时点",
                tp.period.label(),
                tp.revenue.fmt_money()
            ));
        }
        if tp.anomaly_cost {
            anomaly_notes.push(format!(
                "{} 营业成本 {} 显著偏离年内均值，请核查成本结转口径",
                tp.period.label(),
                tp.cost.fmt_money()
            ));
        }
        if np_flags[i] {
            anomaly_notes.push(format!(
                "{} 净利润 {} 异常波动，请核查收入成本配比",
                tp.period.label(),
                tp.net_profit.fmt_money()
            ));
        } else if tp.net_profit.is_negative() {
            anomaly_notes.push(format!(
                "{} 出现亏损（净利润 {}），建议关注费用与毛利",
                tp.period.label(),
                tp.net_profit.fmt_money()
            ));
        }
    }

    // ---- 内因构成（本月 vs 上月，环比展示变化） ----
    let prev = period.prev();
    // 内因构成也是利润表口径：结转损益凭证不能把本月收入/成本冲成 0。
    let mut bq_cur = BalanceQuery::period(period).with_exclude_carry(true);
    let mut bq_prev = BalanceQuery::period(prev).with_exclude_carry(true);
    if let Some(u) = scope {
        bq_cur = bq_cur.with_user_scope(u);
        bq_prev = bq_prev.with_user_scope(u);
    }
    let snap_cur = BalanceSnapshot::load(db, &bq_cur)?;
    let snap_prev = BalanceSnapshot::load(db, &bq_prev)?;
    // 成本合计：主营业务成本 + 其他业务成本 + 未结转的成本类科目余额。
    //
    // 与 financial_totals 的成本口径**必须一致**，否则首页净利润和成本构成
    // 明细会互相打架。成本类科目只取末级（父科目是子科目之和）。
    let cost_total = |snap: &BalanceSnapshot| -> Money {
        let exp_of = |code: &str| {
            let r = snap.for_account(code, None);
            r.debit - r.credit
        };
        let mut v = exp_of("6401") + exp_of("6402");
        if let Ok(accounts) = crate::accounts::list(db) {
            for a in accounts.iter() {
                if !matches!(a.category, fincore::account::AcctCategory::Cost) {
                    continue;
                }
                if accounts
                    .iter()
                    .any(|x| x.code.starts_with(&a.code) && x.code != a.code)
                {
                    continue;
                }
                v += exp_of(&a.code);
            }
        }
        v
    };
    let occ = |snap: &BalanceSnapshot, code: &str| {
        let r = snap.for_account(code, None);
        r.credit - r.debit // 收入类贷方正
    };
    let exp = |snap: &BalanceSnapshot, code: &str| {
        let r = snap.for_account(code, None);
        r.debit - r.credit // 费用类借方正
    };
    // 有符号项：正=增利，负=减利
    let signed = |snap: &BalanceSnapshot, code: &str, dir: i8| {
        let v = if dir > 0 {
            occ(snap, code)
        } else {
            exp(snap, code)
        };
        if dir > 0 {
            v
        } else {
            v.negated()
        }
    };

    let revenue_drivers = [("6001", "主营业务收入", 1i8), ("6051", "其他业务收入", 1)]
        .iter()
        .map(|(c, n, d)| DriverItem {
            code: c.to_string(),
            name: n.to_string(),
            amount: signed(&snap_cur, c, *d),
            prev_amount: signed(&snap_prev, c, *d),
        })
        .collect();

    // 营业成本构成：除了 6401/6402，还要把**成本类科目**（5101 制造费用等）
    // 的余额算进来 —— 与 financial_totals 的成本口径保持一致。
    //
    // 不一致的后果：首页净利润把 5101 算进成本，而这里不列出来，老板点开
    // "成本构成"会发现明细加起来对不上合计，这是最容易被质疑的一类不一致。
    let mut cost_drivers: Vec<DriverItem> =
        [("6401", "主营业务成本", -1i8), ("6402", "其他业务成本", -1)]
            .iter()
            .map(|(c, n, _d)| {
                // 营业成本构成显示为正数（成本规模）
                let cur = exp(&snap_cur, c);
                let prv = exp(&snap_prev, c);
                DriverItem {
                    code: c.to_string(),
                    name: n.to_string(),
                    amount: cur,
                    prev_amount: prv,
                }
            })
            .collect();
    for (code, name) in cost_driver_accounts(db)?.into_iter().skip(2) {
        // skip(2)：前两条 6401/6402 上面已经加过了，这里只补成本类科目
        let cur = exp(&snap_cur, &code);
        let prv = exp(&snap_prev, &code);
        // 两个月都没发生额的不占位，否则成本构成里全是一排 0
        if cur.is_zero() && prv.is_zero() {
            continue;
        }
        cost_drivers.push(DriverItem {
            code,
            name,
            amount: cur,
            prev_amount: prv,
        });
    }
    {
        // 金额大的排前面：老板要的是"钱主要花哪了"，不是科目表顺序
        cost_drivers.sort_by(|x, y| {
            y.amount
                .to_f64()
                .partial_cmp(&x.amount.to_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // 净利润构成（利润表口径）：营业收入、营业成本、税金、三项费用、资产减值、营业外、所得税
    let profit_drivers = [
        ("_rev", "营业收入", 1i8),
        ("_cost", "营业成本", -1i8),
        ("6403", "税金及附加", -1i8),
        ("6601", "销售费用", -1i8),
        ("6602", "管理费用", -1i8),
        ("6603", "财务费用", -1i8),
        ("6701", "资产减值损失", -1i8),
        ("6101", "公允价值变动收益", 1i8),
        ("6111", "投资收益", 1i8),
        ("6301", "营业外收入", 1i8),
        ("6711", "营业外支出", -1i8),
        ("6801", "所得税费用", -1i8),
    ]
    .iter()
    .map(|(c, n, d)| {
        let (cur, prv) = if *c == "_rev" {
            (
                occ(&snap_cur, "6001") + occ(&snap_cur, "6051"),
                occ(&snap_prev, "6001") + occ(&snap_prev, "6051"),
            )
        } else if *c == "_cost" {
            // 与 financial_totals 同口径：6401+6402+未结转成本类科目余额
            let a = cost_total(&snap_cur);
            let b = cost_total(&snap_prev);
            (a.negated(), b.negated())
        } else {
            (signed(&snap_cur, c, *d), signed(&snap_prev, c, *d))
        };
        DriverItem {
            code: c.to_string(),
            name: n.to_string(),
            amount: cur,
            prev_amount: prv,
        }
    })
    .collect();

    let ratios = fin_ratios(db, period, jan, scope)?;

    Ok(FinancialAnalysis {
        trend,
        revenue_drivers,
        cost_drivers,
        profit_drivers,
        anomaly_notes,
        ratios,
    })
}

/// 首页「钱在哪」：应收、应付、预收、预付、手头现金一次算清
///
/// 口径说明：
/// - 应收 = `1122` 下各末级科目的**净借方余额**（未核销部分），不含预收
/// - 应付 = `2202` 下各末级科目的**净贷方余额**，不含预付
/// - 预收 / 预付：同一批科目里方向相反的部分（贷方余额的应收、借方余额的应付）
/// - 现金 = `1001` 库存现金 + `1002` 银行存款 的借方余额
///
/// 走科目余额而不是走核销台账，是因为**没做核销的账也要能看到欠款**。
/// 小厂大量凭证录完从不核销，只认核销的话首页会显示"没人欠我钱"——
/// 那比不显示更糟。
pub fn money_position(
    db: &Db,
    period: Period,
    as_of: chrono::NaiveDate,
    scope: Option<&fincore::user::User>,
) -> DbResult<MoneyPosition> {
    use crate::balances::{BalanceQuery, BalanceSnapshot};

    let mut bq = BalanceQuery::period(period);
    if let Some(u) = scope {
        bq = bq.with_user_scope(u);
    }
    let snap = BalanceSnapshot::load(db, &bq)?;
    let accounts = crate::accounts::list(db)?;

    // 取一批科目前缀下的末级科目余额，按**往来单位**拆借方/贷方两堆。
    //
    // 必须按往来单位逐户拆，不能先按科目汇总再拆极性 —— 先汇总的话
    // C001 欠 50000、C002 预交 8000 会被轧成"应收 42000"，预收变 0。
    // 老板看到"客户欠我 4.2 万"是错的：实际是 A 欠我 5 万、我欠 B 8 千的货，
    // 这两件事必须分开看，否则该催 A 的钱不知道催多少、该给 B 发货也不知道发多少。
    let collect = |prefixes: &[&str]| -> (Money, Money) {
        let mut debit = Money::ZERO;
        let mut credit = Money::ZERO;
        for a in accounts.iter() {
            if !prefixes.iter().any(|p| a.code.starts_with(p)) {
                continue;
            }
            // 只取末级：父科目余额是子科目之和，重复相加翻倍
            if accounts
                .iter()
                .any(|x| x.code.starts_with(&a.code) && x.code != a.code)
            {
                continue;
            }
            // 逐户：把该科目下每个往来单位的**期末余额**单独判断极性后再汇总。
            // 必须用 end() 而不是 debit-credit：后者只是本期发生额，
            // 会把 9 月挂的应收在 11 月的快照里漏掉（实测华达 80000 凭空消失）。
            let rows = snap.aux_breakdown(&a.code);
            if rows.is_empty() {
                // 没有 aux 维度的科目（极少），退回整户汇总
                let net = snap.for_account(&a.code, None).end().round2();
                if net.is_positive() {
                    debit += net;
                } else if net.is_negative() {
                    credit += net.abs();
                }
                continue;
            }
            for r in rows.iter() {
                let net = r.end().round2();
                if net.is_positive() {
                    debit += net;
                } else if net.is_negative() {
                    credit += net.abs();
                }
            }
        }
        (debit, credit)
    };
    // 上面 collect 的语义：**按户轧差后**再按极性归类。
    // 同一供应商既是我欠他 30000、又有预付 5000 时，轧成净欠 25000 —— 这是对的，
    // 因为预付本来就能抵这笔欠款。下面再单独算"毛额"给老板看明细。

    let (ar_debit, ar_credit) = collect(&["1122"]);
    let (ap_debit, ap_credit) = collect(&["2202"]);
    let (cash_debit, _) = collect(&["1001", "1002"]);

    // 毛额：把每个往来单位的借方、贷方分开累加，不做轧差。
    // 为什么两个数都要给：净额回答"实际能收回/要付出多少"，
    // 毛额回答"总共赊了多少"。只看净额会漏掉"某客户已预付、另一客户欠更多"的情况。
    let gross_of = |prefixes: &[&str]| -> (Money, Money) {
        let mut d = Money::ZERO;
        let mut c = Money::ZERO;
        for a in accounts.iter() {
            if !prefixes.iter().any(|p| a.code.starts_with(p)) {
                continue;
            }
            if accounts
                .iter()
                .any(|x| x.code.starts_with(&a.code) && x.code != a.code)
            {
                continue;
            }
            let rows = snap.aux_breakdown(&a.code);
            if rows.is_empty() {
                let e = snap.for_account(&a.code, None).end().round2();
                if e.is_positive() {
                    d += e;
                } else {
                    c += e.abs();
                }
                continue;
            }
            for r in rows.iter() {
                let e = r.end().round2();
                if e.is_positive() {
                    d += e;
                } else {
                    c += e.abs();
                }
            }
        }
        (d, c)
    };
    let (ar_gross, _) = gross_of(&["1122"]);
    let (_, ap_gross) = gross_of(&["2202"]);

    // ---- 往来单位明细（谁欠我、我欠谁） ----
    //
    // **不用核销台账，按往来单位的借贷净额算。**
    //
    // 一开始这里复用了 `settle::aging()`，结果踩了个坑：aging 只统计"未核销"
    // 的分录，而小厂大量收款凭证录完**从不做核销**。于是华达欠 80000、已还
    // 30000，首页却仍显示欠 80000 —— 明细与合计（按科目余额算的 22000）对不上，
    // 两个数都在首页上，老板必然质疑。
    //
    // 这里的取舍：**老板关心的是"这个客户还欠我多少"，不是"核销了几笔"**。
    // 录了收款就该冲减欠款，不因为这笔收款没做核销就装看不见。
    // 核销台账的价值在"按单据对账"（对账单模块负责），不在首页这个总数。
    // 真正需要账龄分档的地方（催收明细）仍走 aging()，那里必须按单据算。
    let buckets = fincore::engine::aging::buckets_by_days();
    let _ = &buckets;

    let mut top_customers: Vec<PartyMoney> = Vec::new();
    let mut overdue_90 = Money::ZERO;
    let mut overdue_90_count = 0usize;
    // 按往来单位聚合：每户的借方合计 − 贷方合计 = 该户净欠款。
    // 同时记录该户"最老一笔未结清日期"用于算账龄。
    let mut cust_map: std::collections::BTreeMap<String, (Money, Option<chrono::NaiveDate>)> =
        std::collections::BTreeMap::new();
    for a in accounts.iter() {
        if !a.code.starts_with("1122") {
            continue;
        }
        if accounts
            .iter()
            .any(|x| x.code.starts_with(&a.code) && x.code != a.code)
        {
            continue;
        }
        for r in snap.aux_breakdown(&a.code) {
            let key = crate::balances::aux_key_value_of(&r.aux.key(), fincore::AuxKind::Customer);
            if key.is_empty() {
                continue;
            }
            let net = r.end().round2();
            if net.is_zero() {
                continue;
            }
            let e = cust_map.entry(key).or_insert((Money::ZERO, None));
            e.0 += net;
            // 账龄取该户最早那笔借方发生日；这里没有逐笔日期，退回用期间末尾估算
            if e.1.is_none() {
                e.1 = Some(as_of);
            }
        }
    }
    // 账龄：金额用净额（可靠），但"压了多久"必须看真实单据日期。
    //
    // 净额口径天然没有日期（它是若干笔分录轧出来的），所以这里再单独查一次
    // 未核销分录，取每户**最老一笔**的日期。两套数据各管一件事：
    // 金额以净额为准（不漏收款），天数以最老未核销单据为准（能催得下去）。
    let oldest: std::collections::BTreeMap<String, chrono::NaiveDate> = {
        let mut m = std::collections::BTreeMap::new();
        let mut collect_oldest = |account: &str, kind: fincore::AuxKind| {
            if let Ok(entries) = crate::settle::open_entries(db, account, period, false) {
                for e in entries {
                    let key = crate::balances::aux_key_value_of(&e.aux_key, kind);
                    if key.is_empty() {
                        continue;
                    }
                    m.entry(key)
                        .and_modify(|d: &mut chrono::NaiveDate| {
                            if e.date < *d {
                                *d = e.date;
                            }
                        })
                        .or_insert(e.date);
                }
            }
        };
        for a in accounts.iter() {
            let leaf = !accounts
                .iter()
                .any(|x| x.code.starts_with(&a.code) && x.code != a.code);
            if !leaf {
                continue;
            }
            if a.code.starts_with("1122") {
                collect_oldest(&a.code, fincore::AuxKind::Customer);
            } else if a.code.starts_with("2202") {
                collect_oldest(&a.code, fincore::AuxKind::Supplier);
            }
        }
        m
    };

    for (code, (amt, _)) in cust_map.iter() {
        // 只列"他欠我"的（正数）。贷方余额的客户是**预收**，跟供应商那侧的
        // 欠款其实是同一件事的两面，由下面的双向身份匹配去对上。
        // 这里仍要跳过，否则"欠我最多的客户"会混进一堆预付款的负数列。
        if !amt.is_positive() {
            continue;
        }
        let name = crate::auxs::get(db, fincore::AuxKind::Customer, code)
            .ok()
            .flatten()
            .map(|e| e.name)
            .unwrap_or_else(|| code.clone());
        let days = oldest
            .get(code)
            .map(|d| (as_of - *d).num_days().max(0))
            .unwrap_or(0);
        if days >= 90 {
            overdue_90 += *amt;
            overdue_90_count += 1;
        }
        top_customers.push(PartyMoney {
            code: code.clone(),
            name,
            amount: *amt,
            max_days: days,
            also: Money::ZERO,
            also_is_receivable: false,
        });
    }
    // 同一客户可能挂在多个应收科目下，合并后再排序
    top_customers.sort_by(|a, b| {
        b.amount
            .to_f64()
            .partial_cmp(&a.amount.to_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top_customers.dedup_by(|b, a| {
        if a.code == b.code {
            a.amount += b.amount;
            true
        } else {
            false
        }
    });
    top_customers.truncate(5);

    let mut top_suppliers: Vec<PartyMoney> = Vec::new();
    let mut sup_map: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    for a in accounts.iter() {
        if !a.code.starts_with("2202") {
            continue;
        }
        if accounts
            .iter()
            .any(|x| x.code.starts_with(&a.code) && x.code != a.code)
        {
            continue;
        }
        for r in snap.aux_breakdown(&a.code) {
            let key = crate::balances::aux_key_value_of(&r.aux.key(), fincore::AuxKind::Supplier);
            if key.is_empty() {
                continue;
            }
            let net = r.end().round2().negated();
            if net.is_zero() {
                continue;
            }
            *sup_map.entry(key).or_insert(Money::ZERO) += net;
        }
    }
    for (code, amt) in sup_map.iter() {
        if !amt.is_positive() {
            continue;
        }
        let name = crate::auxs::get(db, fincore::AuxKind::Supplier, code)
            .ok()
            .flatten()
            .map(|e| e.name)
            .unwrap_or_else(|| code.clone());
        let days = oldest
            .get(code)
            .map(|d| (as_of - *d).num_days().max(0))
            .unwrap_or(0);
        top_suppliers.push(PartyMoney {
            code: code.clone(),
            name,
            amount: *amt,
            max_days: days,
            also: Money::ZERO,
            also_is_receivable: false,
        });
    }
    top_suppliers.sort_by(|a, b| {
        b.amount
            .to_f64()
            .partial_cmp(&a.amount.to_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    top_suppliers.dedup_by(|b, a| {
        if a.code == b.code {
            a.amount += b.amount;
            true
        } else {
            false
        }
    });
    top_suppliers.truncate(5);

    // ---- 双向身份合并：同一单位既欠我又被我欠 ----
    //
    // 小厂常见：宏源既是外发加工商（我欠他外发费），又拿料来加工（他欠我加工费）。
    // 系统里这是两个档案（客户 S001 / 供应商 S001），首页会分两行显示，
    // 老板得自己算 15000 − 28000 = 我实际还欠 13000。
    //
    // 这里把两边对上：客户行标出"同时我欠他 X"，供应商行标出"同时他欠我 X"。
    // 不合并成一行，是因为两边金额和账龄都要能查；但要**明示净额**，
    // 否则老板可能把两笔都付了，或者两笔都催了 —— 那才是真出错。
    //
    // 匹配口径：**单位名称完全相同**。编码不一定相同（一个是 C 开头一个是 S 开头），
    // 但现实中同一个厂只有一个名字。名称相同就认为是同一家。
    // 匹配用的映射必须**包含预收/预付那一侧**，不能只从 top_customers 建。
    //
    // 踩过的坑：宏源预付我 50000、我欠他 28000 时，客户侧是贷方余额（预收），
    // 被"只列正数"过滤掉了，于是 top_customers 里没有宏源 ——
    // 双向匹配就失效，供应商行看不到"他预付我 5 万"，净额算不出来。
    let cust_by_name: std::collections::BTreeMap<String, (String, Money)> = {
        let mut m = std::collections::BTreeMap::new();
        for (code, (amt, _)) in cust_map.iter() {
            if let Ok(Some(e)) = crate::auxs::get(db, fincore::AuxKind::Customer, code) {
                // 预收（负数）取绝对值参与匹配：他预付我的钱，抵我欠他的
                m.insert(e.name.clone(), (code.clone(), amt.abs()));
            }
        }
        m
    };
    let mut sup_by_name: std::collections::BTreeMap<String, (String, Money)> = {
        let mut m = std::collections::BTreeMap::new();
        for (code, amt) in sup_map.iter() {
            if let Ok(Some(e)) = crate::auxs::get(db, fincore::AuxKind::Supplier, code) {
                m.insert(e.name.clone(), (code.clone(), amt.abs()));
            }
        }
        m
    };
    // 只保留出现在供应商表里的（老板只看得到这几家），否则会匹配到未显示的单位
    sup_by_name.retain(|name, _| top_suppliers.iter().any(|s| &s.name == name));
    for c in top_customers.iter_mut() {
        if let Some((_, amt)) = sup_by_name.get(&c.name) {
            c.also = *amt;
            c.also_is_receivable = false; // 供应商那边是"我欠他"
        }
    }
    // 供应商行：他若同时是客户，把客户侧的净额对上
    // （可能是我欠他、也可能是他预付我更多，方向交给 net_is_receivable 判）
    for sp in top_suppliers.iter_mut() {
        if let Some((_, amt)) = cust_by_name.get(&sp.name) {
            sp.also = *amt;
            sp.also_is_receivable = true; // 客户那边是"他欠我"
        }
    }

    Ok(MoneyPosition {
        receivable: ar_debit,
        payable: ap_credit,
        advance_in: ar_credit,
        advance_out: ap_debit,
        cash: cash_debit,
        receivable_gross: ar_gross,
        payable_gross: ap_gross,
        top_customers,
        top_suppliers,
        overdue_90,
        overdue_90_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::account::AuxKind;
    use fincore::auxiliary::AuxEntity;
    use fincore::voucher::{Entry, Voucher};

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_adv_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &crate::tests::test_opts()).unwrap()
    }
    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    fn post(db: &Db, period: Period, no: i32, lines: &[(&str, &str, &str)]) {
        let date = period.first_day();
        let mut v = Voucher::new(period, date, "记", no);
        for (i, (acc, d, c)) in lines.iter().enumerate() {
            let mut aux = fincore::voucher::AuxRef::default();
            if acc.starts_with("1002") {
                aux.bank = Some("B01".into());
            }
            // 应收/应付科目强制要客户/供应商维度，测试里补一个占位档案编码；
            // 不补的话 save 会被校验拦下（「核算客户，必须填写客户」）。
            if acc.starts_with("1122") {
                aux.customer = Some("C01".into());
            }
            if acc.starts_with("2202") {
                aux.supplier = Some("S01".into());
            }
            v.push_entry(Entry {
                debit: m(d),
                credit: m(c),
                aux,
                ..Entry::new(i as i32 + 1, *acc, "测试摘要")
            });
        }
        let id = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, id, "p").unwrap();
    }

    /// 首页「钱花哪了 Top3」用的 `cost_drivers_of` 必须与
    /// `financial_analysis` 的成本构成**同口径**。
    ///
    /// 这两个函数分开之后最容易出的错就是：一个把 5101 制造费用算进去、
    /// 另一个没算，于是首页净利润减掉 15000 的成本，点开明细却只有 0。
    #[test]
    fn cost_drivers_of_matches_financial_analysis() {
        let db = tmpdb("cost_drivers_eq");
        let p = Period::new(2026, 3).unwrap();
        // 一笔收入 + 两笔成本：房租记在 5101 制造费用下（成本类，最容易被漏），
        // 另一笔走 6401 主营业务成本
        post(
            &db,
            p,
            1,
            &[("112201", "30000", "0"), ("600101", "0", "30000")],
        );
        post(
            &db,
            p,
            2,
            &[("510105", "15000", "0"), ("220201", "0", "15000")],
        );
        // 贷方用现金：只要借贷平衡，这条测试关心的是成本构成口径，不是科目语义
        post(&db, p, 3, &[("6401", "5000", "0"), ("1001", "0", "5000")]);

        let a = financial_analysis(&db, p, None).unwrap();
        let mine = cost_drivers_of(&db, p, p, None).unwrap();

        let map = |v: &[DriverItem]| {
            let mut m = std::collections::BTreeMap::new();
            for d in v.iter().filter(|d| !d.amount.is_zero()) {
                m.insert(d.code.clone(), d.amount);
            }
            m
        };
        assert_eq!(
            map(&a.cost_drivers),
            map(&mine),
            "两处成本构成口径必须一致，否则「明细加起来对不上合计」"
        );
        // 合计也得对得上 financial_totals 的营业成本
        let t = financial_totals(&db, p, p, None).unwrap();
        let sum: Money = mine
            .iter()
            .map(|d| d.amount)
            .fold(Money::ZERO, |x, y| x + y);
        assert_eq!(sum, t.cost, "成本构成合计应等于营业成本");
        assert_eq!(t.cost, m("20000"));
    }

    /// 本年累计净利润：读快照的 `ytd_*` 列，必须与「从年初再扫一遍」完全一致。
    ///
    /// 这条测试守的是一个很容易被"优化"掉的等价关系：`financial_ytd` 为了
    /// 省一次全表扫，不再用 `financial_totals(db, cur, 年初, ..)`，而是命中同一份
    /// 快照读本年累计列。哪天有人动了 `load_raw` 里 `ytd_from` 的算法（比如跨年
    /// 时该不该含建账前的 `debit_accum`），两边就会分叉——首页显示的年累计与
    /// 利润表的年累计对不上，而两个数各自都"看着对"。
    #[test]
    fn financial_ytd_matches_scanning_from_january() {
        let db = tmpdb("fin_ytd_eq");
        let start = Period::new(2026, 1).unwrap();
        // 1 月：一笔收入 + 一笔费用（累计从 1 月起算）
        post(
            &db,
            start,
            1,
            &[("112201", "50000", "0"), ("600101", "0", "50000")],
        );
        post(
            &db,
            start,
            2,
            &[("660201", "8000", "0"), ("1001", "0", "8000")],
        );
        // 3 月：成本类支出（5101 制造费用，最容易被漏的那一类）
        let p = Period::new(2026, 3).unwrap();
        post(
            &db,
            p,
            1,
            &[("112201", "20000", "0"), ("600101", "0", "20000")],
        );
        post(
            &db,
            p,
            2,
            &[("510105", "6000", "0"), ("220201", "0", "6000")],
        );

        let by_scan = financial_totals(&db, p, start, None).unwrap();
        let by_ytd = financial_ytd(&db, p, None).unwrap();

        assert_eq!(
            by_ytd.revenue, by_scan.revenue,
            "本年累计收入必须与从 1 月扫到本期一致"
        );
        assert_eq!(
            by_ytd.cost, by_scan.cost,
            "本年累计成本必须一致（含制造费用）"
        );
        assert_eq!(
            by_ytd.net_profit, by_scan.net_profit,
            "本年累计净利润必须一致——首页就是显示这个数"
        );
        // 数也得对：收入 70000，成本 6000 + 费用 8000 → 净利 56000
        assert_eq!(by_ytd.revenue, m("70000"));
        assert_eq!(by_ytd.net_profit, m("56000"));

        // 时点数与区间无关：资产负债合计与「本期」那份必须相同
        let cur = financial_totals(&db, p, p, None).unwrap();
        assert_eq!(by_ytd.total_asset, cur.total_asset);
        assert_eq!(by_ytd.total_liab, cur.total_liab);
        assert_eq!(by_ytd.equity, cur.equity);
        // 而本期净利只有 3 月那两笔：20000 - 6000
        assert_eq!(cur.net_profit, m("14000"));
    }

    /// 余额快照缓存：写完立刻读，必须看到新数。
    ///
    /// 缓存的键里带了 `total_changes()`，这条测试守的就是那个「一漏一个字段
    /// 就把旧结果当新结果发出去」的风险。
    #[test]
    fn snapshot_cache_sees_new_voucher() {
        use crate::balances::{BalanceQuery, BalanceSnapshot};
        let db = tmpdb("snap_cache");
        let p = Period::new(2026, 4).unwrap();
        post(
            &db,
            p,
            1,
            &[("112201", "1000", "0"), ("600101", "0", "1000")],
        );

        let q = BalanceQuery::period(p).with_exclude_carry(true);
        let first = BalanceSnapshot::load(&db, &q).unwrap();
        assert_eq!(first.for_account("600101", None).credit, m("1000"));

        post(&db, p, 2, &[("112201", "500", "0"), ("600101", "0", "500")]);

        let second = BalanceSnapshot::load(&db, &q).unwrap();
        assert_eq!(
            second.for_account("600101", None).credit,
            m("1500"),
            "缓存没失效：写入之后仍然读到旧快照"
        );
    }

    /// 科目表缓存：新增科目之后必须立刻能取到。
    #[test]
    fn chart_cache_sees_new_account() {
        let db = tmpdb("chart_cache");
        assert!(crate::accounts::chart(&db).unwrap().get("660901").is_none());
        let a = fincore::account::Account::new(
            "660901",
            "办公费",
            fincore::account::AcctCategory::Expense,
        );
        crate::accounts::insert(&db, &a).unwrap();
        assert!(
            crate::accounts::chart(&db).unwrap().get("660901").is_some(),
            "缓存没失效：新建的科目取不到"
        );
    }

    #[test]
    fn routing_and_ops() {
        let db = tmpdb("routing");
        routing_save(
            &db,
            "FG01",
            &[
                RoutingOp {
                    id: 0,
                    item_code: "FG01".into(),
                    version: String::new(),
                    seq: 1,
                    op_code: "OP1".into(),
                    op_name: "下料".into(),
                    work_center: "WC1".into(),
                    std_hours: m("2"),
                    rate: m("50"),
                    qc_required: false,
                },
                RoutingOp {
                    id: 0,
                    item_code: "FG01".into(),
                    version: String::new(),
                    seq: 2,
                    op_code: "OP2".into(),
                    op_name: "装配".into(),
                    work_center: "WC2".into(),
                    std_hours: m("3"),
                    rate: m("60"),
                    qc_required: false,
                },
            ],
        )
        .unwrap();
        let ops = routing_list(&db, "FG01").unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[1].op_name, "装配");
    }

    #[test]
    fn routing_version_and_stats() {
        let db = tmpdb("rtver");
        routing_save_version(
            &db,
            "FG01",
            "v1",
            &[RoutingOp {
                id: 0,
                item_code: "FG01".into(),
                version: "v1".into(),
                seq: 1,
                op_code: "OP1".into(),
                op_name: "下料".into(),
                work_center: "WC1".into(),
                std_hours: m("2"),
                rate: m("50"),
                qc_required: false,
            }],
        )
        .unwrap();
        routing_save_version(
            &db,
            "FG01",
            "v2",
            &[RoutingOp {
                id: 0,
                item_code: "FG01".into(),
                version: "v2".into(),
                seq: 1,
                op_code: "OP1".into(),
                op_name: "下料".into(),
                work_center: "WC1".into(),
                std_hours: m("4"),
                rate: m("50"),
                qc_required: false,
            }],
        )
        .unwrap();
        assert_eq!(routing_versions(&db, "FG01").unwrap().len(), 2);
        assert_eq!(
            routing_list_version(&db, "FG01", "v1").unwrap()[0].std_hours,
            m("2")
        );
        // 统计（默认版本）
        let st = routing_stats(&db, "FG01").unwrap();
        assert_eq!(st.op_count, 0); // 默认版本为空
                                    // 导入导出
        let json = routing_export_json(&db, "FG01", "v1").unwrap();
        assert!(json.contains("OP1"));
        let n = routing_import_json(&db, "FG01", "v3", &json).unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            routing_list_version(&db, "FG01", "v3").unwrap()[0].op_code,
            "OP1"
        );
    }

    #[test]
    fn dispatch_and_progress() {
        let db = tmpdb("dispatch");
        // 建生产订单 + 工艺路线 + 生成工序
        let p = Period::new(2026, 1).unwrap();
        let mut order = crate::scm::ProductionOrder {
            id: 0,
            no: "SC2026010001".into(),
            period: p,
            date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            item_code: "FG01".into(),
            item_name: "成品".into(),
            planned_qty: m("10"),
            completed_qty: m("0"),
            status: crate::scm::ProdStatus::Released,
            work_center: "WC1".into(),
            prepared_by: "u".into(),
            memo: String::new(),
            so_id: 0,
            order_kind: "inhouse".into(),
            supplier_code: String::new(),
            supplier_name: String::new(),
            plan_start: String::new(),
            plan_end: String::new(),
        };
        order.no = crate::scm::prod_next_no(&db, p).unwrap();
        let po_id = crate::scm::prod_save(&db, &mut order).unwrap();
        routing_save(
            &db,
            "FG01",
            &[
                RoutingOp {
                    id: 0,
                    item_code: "FG01".into(),
                    version: String::new(),
                    seq: 1,
                    op_code: "OP1".into(),
                    op_name: "下料".into(),
                    work_center: "WC1".into(),
                    std_hours: m("2"),
                    rate: m("50"),
                    qc_required: false,
                },
                RoutingOp {
                    id: 0,
                    item_code: "FG01".into(),
                    version: String::new(),
                    seq: 2,
                    op_code: "OP2".into(),
                    op_name: "装配".into(),
                    work_center: "WC2".into(),
                    std_hours: m("3"),
                    rate: m("60"),
                    qc_required: false,
                },
            ],
        )
        .unwrap();
        prod_op_init_from_routing(&db, po_id, "FG01").unwrap();
        let ops = prod_op_list(&db, po_id).unwrap();
        assert_eq!(ops.len(), 2);
        // 派工
        prod_op_dispatch(&db, ops[0].id, "张三").unwrap();
        assert_eq!(prod_op_list(&db, po_id).unwrap()[0].worker, "张三");
        // 完工一道 → 进度 50%
        prod_op_finish(&db, ops[0].id).unwrap();
        let prog = prod_progress(&db, po_id).unwrap();
        assert_eq!(prog.done_ops, 1);
        assert_eq!(prog.percent, m("50"));
    }

    #[test]
    fn mrp_basic() {
        let db = tmpdb("mrp");
        // BOM: FG01 = 2 × RM01
        crate::scm::bom_save(&db, "FG01", &[("RM01".into(), m("2"), m("0"))]).unwrap();
        let run = mrp_run(&db, &[("FG01".into(), m("10"), "SO-001".into())]).unwrap();
        let rows = mrp_by_run(&db, &run).unwrap();
        let fg = rows.iter().find(|r| r.item_code == "FG01").unwrap();
        assert_eq!(fg.gross_req, m("10"));
        assert_eq!(fg.action, "produce");
        let rm = rows.iter().find(|r| r.item_code == "RM01").unwrap();
        assert_eq!(rm.gross_req, m("20"));
        assert_eq!(rm.action, "purchase");
    }

    #[test]
    fn mrp_lot_size_and_safety_stock() {
        let db = tmpdb("mrp_lot");
        item_plan_upsert(
            &db,
            &ItemPlan {
                item_code: "RM01".into(),
                safety_stock: m("5"),
                lead_days: 3,
                lot_size: m("10"),
            },
        )
        .unwrap();
        let run = mrp_run(&db, &[("RM01".into(), m("7"), "X".into())]).unwrap();
        let rows = mrp_by_run(&db, &run).unwrap();
        let rm = &rows[0];
        // 毛需求 7 + 安全库存 5 = 12 → 套批量 10 → 计划 20
        assert_eq!(rm.net_req, m("12"));
        assert_eq!(rm.planned_qty, m("20"));
    }

    #[test]
    fn approval_flow() {
        let db = tmpdb("appr");
        let id = approval_start(
            &db,
            "claim",
            1,
            "报销单",
            "alice",
            &["bob".into(), "carol".into()],
        )
        .unwrap();
        // 非当前节点审批人拒绝
        assert!(approval_act(&db, id, "carol", true, "").is_err());
        // bob 通过 → 推进到 carol
        let ap = approval_act(&db, id, "bob", true, "同意").unwrap();
        assert_eq!(ap.current_node, 2);
        assert_eq!(ap.status, "pending");
        // carol 通过 → 整单 approved
        let ap = approval_act(&db, id, "carol", true, "同意").unwrap();
        assert_eq!(ap.status, "approved");
        // 重复发起报错
        assert!(approval_start(&db, "claim", 1, "x", "alice", &["bob".into()]).is_err());
    }

    #[test]
    fn approval_reject_stops_flow() {
        let db = tmpdb("appr_rj");
        let id = approval_start(&db, "po", 5, "采购单", "alice", &["bob".into()]).unwrap();
        let ap = approval_act(&db, id, "bob", false, "价格太高").unwrap();
        assert_eq!(ap.status, "rejected");
        assert!(approval_act(&db, id, "bob", true, "").is_err());
    }

    /// 回归：同一节点重复审批必须失败，且不能覆盖第一位审批人的意见。
    /// 旧实现的状态判断在事务外、写入又不带条件，第二次「通过」会成功并覆盖 comment。
    #[test]
    fn approval_act_twice_fails_and_keeps_first_comment() {
        let db = tmpdb("appr_twice");
        let id = approval_start(&db, "claim", 9, "报销单", "alice", &["bob".into()]).unwrap();
        let ap = approval_act(&db, id, "bob", true, "第一次：同意").unwrap();
        assert_eq!(ap.status, "approved");
        assert!(
            approval_act(&db, id, "bob", true, "第二次：也同意").is_err(),
            "重复审批必须报错"
        );
        assert!(
            approval_act(&db, id, "bob", false, "反悔").is_err(),
            "已完成的单据不能再驳回"
        );
        let ap2 = approval_get(&db, id).unwrap().unwrap();
        assert_eq!(ap2.status, "approved", "整单状态不应被第二次操作改写");
        let s = ap2.steps.iter().find(|s| s.seq == 1).unwrap();
        assert_eq!(s.action, "approve");
        assert_eq!(
            s.comment, "第一次：同意",
            "第二位审批人的意见不应覆盖第一位"
        );

        // 单线程复现「读状态 → 写」之间被别人抢先的落点：节点已写过 action、
        // 而整单还停在 pending。旧实现的 `UPDATE approval_step ... WHERE id=?`
        // 没有 `action` 前置条件，会直接覆盖掉先到者的意见并"成功"返回。
        let id2 = approval_start(&db, "claim", 10, "报销单", "alice", &["bob".into()]).unwrap();
        let step_id: i64 = db
            .conn()
            .query_row(
                "SELECT id FROM approval_step WHERE approval_id=?1",
                [id2],
                |r| r.get(0),
            )
            .unwrap();
        db.conn()
            .execute(
                "UPDATE approval_step SET action='approve', comment='先到的审批人：同意', acted_at='x'
                 WHERE id=?1",
                [step_id],
            )
            .unwrap();
        assert!(
            approval_act(&db, id2, "bob", true, "后到的审批人：也同意").is_err(),
            "已写过的节点不得被再次审批（回归前会成功返回并覆盖意见）"
        );
        let s2 = approval_get(&db, id2).unwrap().unwrap().steps.remove(0);
        assert_eq!(s2.action, "approve");
        assert_eq!(
            s2.comment, "先到的审批人：同意",
            "先到者的审批意见不得被覆盖"
        );
    }

    /// 回归：撤销审批必须带状态条件，否则与并发的审批动作互相覆盖。
    #[test]
    fn approval_cancel_is_guarded() {
        let db = tmpdb("appr_cancel");
        let id = approval_start(&db, "claim", 11, "报销单", "alice", &["bob".into()]).unwrap();
        // 非申请人不能撤
        assert!(approval_cancel(&db, id, "carol").is_err());
        // 申请人可撤；撤完再撤必须报错
        approval_cancel(&db, id, "alice").unwrap();
        assert_eq!(approval_get(&db, id).unwrap().unwrap().status, "cancelled");
        assert!(
            approval_cancel(&db, id, "alice").is_err(),
            "重复撤销必须报错"
        );
        // 已流转的单据不能撤
        let id2 = approval_start(&db, "claim", 12, "报销单", "alice", &["bob".into()]).unwrap();
        approval_act(&db, id2, "bob", true, "").unwrap();
        assert!(approval_cancel(&db, id2, "alice").is_err());
    }

    /// 回归：已完工的工序再报一次工不能被退回 in_progress（生产进度会凭空倒退）。
    #[test]
    fn prod_op_report_keeps_done_status() {
        let db = tmpdb("opkeep");
        let p = Period::new(2026, 1).unwrap();
        let mut order = crate::scm::ProductionOrder {
            id: 0,
            no: String::new(),
            period: p,
            date: p.first_day(),
            item_code: "FG01".into(),
            item_name: "成品".into(),
            planned_qty: m("10"),
            completed_qty: Money::ZERO,
            status: crate::scm::ProdStatus::Released,
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
        order.no = crate::scm::prod_next_no(&db, p).unwrap();
        let po_id = crate::scm::prod_save(&db, &mut order).unwrap();
        routing_save(
            &db,
            "FG01",
            &[RoutingOp {
                id: 0,
                item_code: "FG01".into(),
                version: String::new(),
                seq: 1,
                op_code: "OP1".into(),
                op_name: "下料".into(),
                work_center: "WC1".into(),
                std_hours: m("2"),
                rate: m("50"),
                qc_required: false,
            }],
        )
        .unwrap();
        prod_op_init_from_routing(&db, po_id, "FG01").unwrap();
        let ops = prod_op_list(&db, po_id).unwrap();
        prod_op_finish(&db, ops[0].id).unwrap();
        assert_eq!(prod_op_list(&db, po_id).unwrap()[0].status, "done");
        // 再报工：数量/工时累加，但状态必须仍是 done
        prod_op_report(&db, ops[0].id, m("3"), m("1")).unwrap();
        let op = prod_op_list(&db, po_id).unwrap().remove(0);
        assert_eq!(op.status, "done", "已完工工序不能被报工打回");
        assert_eq!(op.qty_done, m("3"));
        // 进度仍是 100%
        assert_eq!(prod_progress(&db, po_id).unwrap().done_ops, 1);
    }

    /// 回归：`prod_op_finish` 必须校验工序存在，不能对不存在的 id 静默成功。
    #[test]
    fn prod_op_finish_rejects_unknown_op() {
        let db = tmpdb("op404");
        assert!(
            prod_op_finish(&db, 999_999).is_err(),
            "不存在的工序应报错（否则界面提示成功、实际没改任何数据）"
        );
    }

    #[test]
    fn archive_and_verify() {
        let db = tmpdb("arch");
        let p = Period::new(2026, 1).unwrap();
        let no = archive_next_no(&db, p, "voucher").unwrap();
        assert!(no.starts_with("202601-voucher-"));
        let id = archive_create(&db, p, "voucher", "1月凭证", &no, "{\"a\":1}", "admin").unwrap();
        let a = archive_get(&db, id).unwrap().unwrap();
        assert!(archive_verify(&a));
        // 篡改内容后校验失败
        let mut tampered = a.clone();
        tampered.payload = "{\"a\":2}".into();
        assert!(!archive_verify(&tampered));
        // 重复 file_no 归档报错
        assert!(archive_create(&db, p, "voucher", "x", &no, "{}", "admin").is_err());
    }

    /// 回归：档案号必须唯一。旧的 `COUNT(*)+1` 在删掉一张档案后会复用一个已存在的号，
    /// 直接撞上 `e_archive` 的 UNIQUE(period, kind, file_no)。
    #[test]
    fn archive_next_no_survives_deletion() {
        let db = tmpdb("arch2");
        let p = Period::new(2026, 1).unwrap();
        for i in 1..=3 {
            let no = archive_next_no(&db, p, "voucher").unwrap();
            assert_eq!(no, format!("202601-voucher-{i:03}"));
            archive_create(&db, p, "voucher", &format!("第{i}张"), &no, "{}", "admin").unwrap();
        }
        // 删掉中间一张（-002）
        let victim = archive_list(&db, p, Some("voucher"))
            .unwrap()
            .into_iter()
            .find(|a| a.file_no == "202601-voucher-002")
            .unwrap();
        db.conn()
            .execute("DELETE FROM e_archive WHERE id=?1", [victim.id])
            .unwrap();
        // 下一个号必须接着 003 往后走，不能回落成 002
        let no = archive_next_no(&db, p, "voucher").unwrap();
        assert_eq!(
            no, "202601-voucher-004",
            "删档后档案号不可复用（回归前会退回 002 并撞唯一索引）"
        );
        assert!(archive_create(&db, p, "voucher", "第4张", &no, "{}", "admin").is_ok());
    }

    #[test]
    fn summary_and_multicol() {
        let db = tmpdb("sumtab");
        let p = Period::new(2026, 1).unwrap();
        post(
            &db,
            p,
            1,
            &[("100201", "1000", "0"), ("600101", "0", "1000")],
        );
        post(&db, p, 2, &[("660201", "300", "0"), ("100201", "0", "300")]);
        let rows = summary_table(&db, p, p, None).unwrap();
        assert!(!rows.is_empty());
        let r = rows.iter().find(|r| r.summary == "测试摘要").unwrap();
        assert_eq!(r.voucher_count, 2);
        assert_eq!(r.debit, m("1300"));
        assert_eq!(r.credit, m("1300"));

        // 多栏账：主科目 6602，栏目 660201
        let mc = multi_column_table(&db, "6602", &["1002".to_string()], p, p, None).unwrap();
        assert_eq!(mc.len(), 1);
        assert_eq!(mc[0].amount, m("300"));
        assert_eq!(mc[0].balance, m("300"));
        // 对方科目 100201 命中栏目 1002 前缀
        assert_eq!(mc[0].cols[0], m("-300"));
    }

    #[test]
    fn budget_versions() {
        let db = tmpdb("bver");
        bversion_save(
            &db,
            &BudgetVersion {
                key: "v1".into(),
                name: "初稿".into(),
                is_current: true,
                created_at: "2026-01-01".into(),
                memo: String::new(),
            },
        )
        .unwrap();
        bversion_save(
            &db,
            &BudgetVersion {
                key: "v2".into(),
                name: "调整版".into(),
                is_current: true,
                created_at: "2026-02-01".into(),
                memo: String::new(),
            },
        )
        .unwrap();
        // 单一生效：v1 被顶掉
        assert_eq!(bversion_current(&db).unwrap(), "v2");
        let list = bversion_list(&db).unwrap();
        assert_eq!(list.len(), 2);
        assert!(!list[0].is_current);
    }

    #[test]
    fn ratios_smoke() {
        let db = tmpdb("ratio");
        let p = Period::new(2026, 1).unwrap();
        crate::auxs::insert(&db, &AuxEntity::new(AuxKind::Bank, "B01", "工行")).unwrap();
        post(
            &db,
            p,
            1,
            &[("100201", "100000", "0"), ("600101", "0", "100000")],
        );
        post(
            &db,
            p,
            2,
            &[("6401", "60000", "0"), ("100201", "0", "60000")],
        );
        let ratios = fin_ratios(&db, p, p, None).unwrap();
        assert!(ratios.len() >= 6);
        let gm = ratios.iter().find(|r| r.key == "gross_margin").unwrap();
        // 毛利率 = (100000-60000)/100000 = 40%
        assert_eq!(gm.value, m("0.4"));
    }

    /// H2/H3：损益取数要剔除结转凭证，且净利润要包含 6101/6111。
    #[test]
    fn financial_totals_excludes_carry_and_counts_investment_income() {
        let db = tmpdb("fin_totals_h2h3");
        let p = Period::new(2026, 1).unwrap();
        post(&db, p, 1, &[("1001", "1000", "0"), ("600101", "0", "1000")]);
        post(&db, p, 2, &[("6401", "400", "0"), ("1001", "0", "400")]);
        post(&db, p, 3, &[("1001", "50", "0"), ("6101", "0", "50")]);
        post(&db, p, 4, &[("1001", "100", "0"), ("6111", "0", "100")]);
        post(&db, p, 5, &[("6403", "20", "0"), ("1001", "0", "20")]);

        let before = financial_totals(&db, p, p, None).unwrap();
        assert_eq!(before.revenue, m("1000"));
        assert_eq!(before.cost, m("400"));
        // 1000 - 400 - 20 + 50（公允价值变动）+ 100（投资收益）= 730
        assert_eq!(before.net_profit, m("730"));

        // 结转损益：把收入/成本/费用/公允价值变动/投资收益全部转入本年利润
        let date = p.last_day();
        let mut v = Voucher::new(p, date, "记", 99);
        v.source = fincore::VoucherSource::CarryForward;
        for (i, (acc, d, c)) in [
            ("600101", "1000", "0"),
            ("6101", "50", "0"),
            ("6111", "100", "0"),
            ("6401", "0", "400"),
            ("6403", "0", "20"),
            ("4103", "0", "730"),
        ]
        .iter()
        .enumerate()
        {
            v.push_entry(Entry {
                debit: m(d),
                credit: m(c),
                ..Entry::new(i as i32 + 1, *acc, "结转损益")
            });
        }
        let id = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, id, "p").unwrap();

        let after = financial_totals(&db, p, p, None).unwrap();
        assert_eq!(after.revenue, m("1000"), "结转后收入不得被冲成 0");
        assert_eq!(after.cost, m("400"), "结转后成本不得被冲成 0");
        assert_eq!(after.net_profit, m("730"), "结转后净利润不得被冲成 0");

        // 利润构成同样要剔除结转，且包含 6101/6111
        let a = financial_analysis(&db, p, None).unwrap();
        let inv = a
            .profit_drivers
            .iter()
            .find(|d| d.code == "6111")
            .expect("利润构成应包含投资收益");
        assert_eq!(inv.amount, m("100"));
    }

    #[test]
    fn financial_analysis_trend_and_drivers() {
        let db = tmpdb("finanalysis");
        let p = Period::new(2026, 2).unwrap();
        crate::auxs::insert(&db, &AuxEntity::new(AuxKind::Bank, "B01", "工行")).unwrap();
        // 1 月：收入 100000，成本 40000
        let jan = Period::new(2026, 1).unwrap();
        post(
            &db,
            jan,
            1,
            &[("100201", "100000", "0"), ("600101", "0", "100000")],
        );
        post(
            &db,
            jan,
            2,
            &[("6401", "40000", "0"), ("100201", "0", "40000")],
        );
        // 2 月：收入 30000，成本 10000
        post(
            &db,
            p,
            1,
            &[("100201", "30000", "0"), ("600101", "0", "30000")],
        );
        post(
            &db,
            p,
            2,
            &[("6401", "10000", "0"), ("100201", "0", "10000")],
        );

        let a = financial_analysis(&db, p, None).unwrap();
        assert_eq!(a.trend.len(), 2, "应含 1、2 两月趋势点");
        assert_eq!(a.trend[0].revenue, m("100000"));
        assert_eq!(a.trend[0].net_profit, m("60000"));
        // 年初至今累计收入 = 100000 + 30000
        assert_eq!(a.trend[1].cum_revenue, m("130000"));
        assert_eq!(a.trend[1].cum_net_profit, m("80000"));

        // 营业收入构成：主营业务收入 6001 本月 30000，上月 100000
        let rev = &a.revenue_drivers[0];
        assert_eq!(rev.code, "6001");
        assert_eq!(rev.amount, m("30000"));
        assert_eq!(rev.prev_amount, m("100000"));
        // 净利润构成首项为营业收入（本月 30000）
        assert_eq!(a.profit_drivers[0].name, "营业收入");
        assert_eq!(a.profit_drivers[0].amount, m("30000"));
        // 指标至少含 ROE/ROA 等
        assert!(a.ratios.iter().any(|r| r.key == "roe"));
    }
}
