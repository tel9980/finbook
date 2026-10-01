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

const RT_COLS: &str = "id,item_code,version,seq,op_code,op_name,work_center,std_hours,rate,qc_required";

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
pub fn routing_save_version(db: &Db, item_code: &str, version: &str, ops: &[RoutingOp]) -> DbResult<()> {
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
    let mut st = db
        .conn()
        .prepare(&format!("SELECT {PO_COLS} FROM prod_op WHERE po_id=?1 ORDER BY id"))?;
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
    let status = if new_qty.is_positive() {
        "in_progress"
    } else {
        "pending"
    };
    tx.execute(
        "UPDATE prod_op SET qty_done=?2, hours=?3, status=?4 WHERE id=?1",
        rusqlite::params![op_id, crate::exact_param(new_qty), crate::exact_param(new_hours), status],
    )?;
    tx.commit()?;
    Ok(())
}

/// 手工标记某工序完成
pub fn prod_op_finish(db: &Db, op_id: i64) -> DbResult<()> {
    db.conn().execute(
        "UPDATE prod_op SET status='done' WHERE id=?1",
        rusqlite::params![op_id],
    )?;
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

const MRP_COLS: &str = "id,run_at,item_code,item_name,level,gross_req,on_hand,net_req,planned_qty,action,source";

/// 查询最近一次 MRP 运算结果
pub fn mrp_latest(db: &Db) -> DbResult<Vec<MrpRow>> {
    // 空表时 MAX(run_at) 仍返回一行 NULL：必须按 Option<String> 取值，
    // 否则 rusqlite 会以 "Invalid column type Null" 报错（页面 500）。
    let latest: Option<String> = db
        .conn()
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
        return Err(FinError::msg("仅采购类建议可下推请购（生产类请走「下达」生成生产订单）").into());
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
    let run_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string();
    // 净需求累加表：item -> (gross, level, source)
    struct Acc {
        gross: Money,
        level: i32,
        sources: Vec<String>,
    }
    let mut acc: std::collections::BTreeMap<String, Acc> = std::collections::BTreeMap::new();

    // 用队列做逐层展开（同层可合并），guard 防 BOM 循环引用
    let mut queue: std::collections::VecDeque<(String, Money, i32, String)> =
        demands
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
        let mut st = db
            .conn()
            .prepare("SELECT item_code, planned_qty, completed_qty, status FROM production_order")?;
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
    let run_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.6f").to_string();
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
        let src = source
            .get(&item)
            .map(|v| v.join(","))
            .unwrap_or_default();
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
    let latest: Option<String> = db
        .conn()
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
    let row = match db
        .conn()
        .query_row(
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
            !matches!(o.status, crate::scm::ProdStatus::Completed | crate::scm::ProdStatus::Cancelled)
                && o.order_kind == "inhouse"
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
            *load.entry(day.format("%Y-%m-%d").to_string()).or_insert(Money::ZERO) += put;
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

const AP_COLS: &str = "id,biz_kind,biz_id,title,applicant,current_node,status,created_at,finished_at";

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
pub fn approval_act(
    db: &Db,
    id: i64,
    who: &str,
    approve: bool,
    comment: &str,
) -> DbResult<Approval> {
    let ap = approval_get(db, id)?.ok_or_else(|| FinError::msg("审批流不存在"))?;
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
    let action = if approve { "approve" } else { "reject" };
    let tx = db.write_tx()?;
    tx.execute(
        "UPDATE approval_step SET action=?1, comment=?2, acted_at=?3 WHERE id=?4",
        rusqlite::params![action, comment, now(), cur.id],
    )?;
    if approve {
        let next = ap.current_node + 1;
        let has_next = ap.steps.iter().any(|s| s.seq == next);
        if has_next {
            tx.execute(
                "UPDATE approval SET current_node=?2 WHERE id=?1",
                rusqlite::params![id, next],
            )?;
        } else {
            tx.execute(
                "UPDATE approval SET status='approved', finished_at=?2 WHERE id=?1",
                rusqlite::params![id, now()],
            )?;
        }
    } else {
        tx.execute(
            "UPDATE approval SET status='rejected', finished_at=?2 WHERE id=?1",
            rusqlite::params![id, now()],
        )?;
    }
    tx.commit()?;
    approval_get(db, id)?.ok_or_else(|| FinError::msg("审批流读取失败").into())
}

/// 撤销审批流（仅申请人、且还在第一节点）
pub fn approval_cancel(db: &Db, id: i64, who: &str) -> DbResult<()> {
    let ap = approval_get(db, id)?.ok_or_else(|| FinError::msg("审批流不存在"))?;
    if ap.applicant != who {
        return Err(FinError::msg("只有申请人可以撤销审批流").into());
    }
    if ap.status != "pending" || ap.current_node > 1 {
        return Err(FinError::msg("审批已流转，不能撤销").into());
    }
    db.conn().execute(
        "UPDATE approval SET status='cancelled', finished_at=?2 WHERE id=?1",
        rusqlite::params![id, now()],
    )?;
    Ok(())
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
        Some(_) => format!("SELECT {ARC_COLS} FROM e_archive WHERE period=?1 AND kind=?2 ORDER BY file_no"),
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
pub fn archive_next_no(db: &Db, period: Period, kind: &str) -> DbResult<String> {
    let cnt: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM e_archive WHERE period=?1 AND kind=?2",
        rusqlite::params![period.ymm(), kind],
        |r| r.get(0),
    )?;
    Ok(format!("{}-{}-{:03}", period.ymm(), kind, cnt + 1))
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
    let mut map: std::collections::BTreeMap<String, (Money, Money, std::collections::BTreeSet<i64>)> =
        std::collections::BTreeMap::new();
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
    let params: Vec<&dyn rusqlite::types::ToSql> =
        vids.iter().map(|i| i as &dyn rusqlite::types::ToSql).collect();
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
    use crate::balances::{BalanceQuery, BalanceSnapshot};
    let mut bq = BalanceQuery::range(from, period);
    if let Some(u) = scope {
        bq = bq.with_user_scope(u);
    }
    let snap = BalanceSnapshot::load(db, &bq)?;
    let b = |code: &str| snap.for_account(code, None).end();
    let occ = |code: &str| {
        let r = snap.for_account(code, None);
        r.credit - r.debit // 收入类贷方正
    };
    let exp = |code: &str| {
        let r = snap.for_account(code, None);
        r.debit - r.credit // 费用类借方正
    };
    // 流动资产 ≈ 货币资金 + 交易性金融资产 + 应收 + 预付 + 其他应收 + 存货
    let cur_asset = b("1001") + b("1002") + b("1012") + b("1101") + b("1121") + b("1122")
        + b("1123") + b("1221") + b("1403") + b("1405") + b("1406") + b("1411");
    // 流动负债 ≈ 短期借款 + 应付 + 预收 + 薪酬 + 税费 + 其他应付
    let cur_liab = (b("2001") + b("2201") + b("2202") + b("2203") + b("2211") + b("2221")
        + b("2241"))
    .negated(); // 负债贷方余额为负，取负得正数
    let total_asset = {
        // 全部 1 开头资产类（借方正）合计
        let mut s = Money::ZERO;
        for a in crate::accounts::list(db)?.iter().filter(|a| {
            matches!(
                a.category,
                fincore::account::AcctCategory::Asset
            ) && a.code.len() == 4
        }) {
            s += b(&a.code);
        }
        s
    };
    let total_liab = {
        let mut s = Money::ZERO;
        for a in crate::accounts::list(db)?.iter().filter(|a| {
            matches!(
                a.category,
                fincore::account::AcctCategory::Liability
            ) && a.code.len() == 4
        }) {
            s += b(&a.code).negated();
        }
        s
    };
    let equity = total_asset - total_liab;
    let revenue = occ("6001") + occ("6051");
    let cost = exp("6401") + exp("6402");
    let net_profit = revenue - cost - exp("6403") - exp("6601") - exp("6602") - exp("6603")
        - exp("6701") + occ("6301") - exp("6711") - exp("6801");
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
            cum_cost: cum_cost,
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
    let mut bq_cur = BalanceQuery::period(period);
    let mut bq_prev = BalanceQuery::period(prev);
    if let Some(u) = scope {
        bq_cur = bq_cur.with_user_scope(u);
        bq_prev = bq_prev.with_user_scope(u);
    }
    let snap_cur = BalanceSnapshot::load(db, &bq_cur)?;
    let snap_prev = BalanceSnapshot::load(db, &bq_prev)?;
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
        let v = if dir > 0 { occ(snap, code) } else { exp(snap, code) };
        if dir > 0 { v } else { v.negated() }
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

    let cost_drivers = [("6401", "主营业务成本", -1i8), ("6402", "其他业务成本", -1)]
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

    // 净利润构成（利润表口径）：营业收入、营业成本、税金、三项费用、资产减值、营业外、所得税
    let profit_drivers = [
        ("_rev", "营业收入", 1i8),
        ("_cost", "营业成本", -1i8),
        ("6403", "税金及附加", -1i8),
        ("6601", "销售费用", -1i8),
        ("6602", "管理费用", -1i8),
        ("6603", "财务费用", -1i8),
        ("6701", "资产减值损失", -1i8),
        ("6301", "营业外收入", 1i8),
        ("6711", "营业外支出", -1i8),
        ("6801", "所得税费用", -1i8),
    ]
    .iter()
    .map(|(c, n, d)| {
        let (cur, prv) = if *c == "_rev" {
            (occ(&snap_cur, "6001") + occ(&snap_cur, "6051"), occ(&snap_prev, "6001") + occ(&snap_prev, "6051"))
        } else if *c == "_cost" {
            let a = exp(&snap_cur, "6401") + exp(&snap_cur, "6402");
            let b = exp(&snap_prev, "6401") + exp(&snap_prev, "6402");
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

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::account::AuxKind;
    use fincore::auxiliary::AuxEntity;
    use fincore::voucher::{Entry, Voucher};

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_adv_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
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
                id: 0, item_code: "FG01".into(), version: "v1".into(), seq: 1,
                op_code: "OP1".into(), op_name: "下料".into(), work_center: "WC1".into(),
                std_hours: m("2"), rate: m("50"), qc_required: false,
            }],
        )
        .unwrap();
        routing_save_version(
            &db,
            "FG01",
            "v2",
            &[RoutingOp {
                id: 0, item_code: "FG01".into(), version: "v2".into(), seq: 1,
                op_code: "OP1".into(), op_name: "下料".into(), work_center: "WC1".into(),
                std_hours: m("4"), rate: m("50"), qc_required: false,
            }],
        )
        .unwrap();
        assert_eq!(routing_versions(&db, "FG01").unwrap().len(), 2);
        assert_eq!(routing_list_version(&db, "FG01", "v1").unwrap()[0].std_hours, m("2"));
        // 统计（默认版本）
        let st = routing_stats(&db, "FG01").unwrap();
        assert_eq!(st.op_count, 0); // 默认版本为空
        // 导入导出
        let json = routing_export_json(&db, "FG01", "v1").unwrap();
        assert!(json.contains("OP1"));
        let n = routing_import_json(&db, "FG01", "v3", &json).unwrap();
        assert_eq!(n, 1);
        assert_eq!(routing_list_version(&db, "FG01", "v3").unwrap()[0].op_code, "OP1");
    }

    #[test]
    fn dispatch_and_progress() {
        let db = tmpdb("dispatch");
        // 建生产订单 + 工艺路线 + 生成工序
        let p = Period::new(2026, 1).unwrap();
        let mut order = crate::scm::ProductionOrder {
            id: 0, no: "SC2026010001".into(), period: p,
            date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            item_code: "FG01".into(), item_name: "成品".into(),
            planned_qty: m("10"), completed_qty: m("0"),
            status: crate::scm::ProdStatus::Released,
            work_center: "WC1".into(), prepared_by: "u".into(), memo: String::new(),
            order_kind: "inhouse".into(),
            supplier_code: String::new(),
            supplier_name: String::new(),
            plan_start: String::new(),
            plan_end: String::new(),
        };
        order.no = crate::scm::prod_next_no(&db, p).unwrap();
        let po_id = crate::scm::prod_save(&db, &mut order).unwrap();
        routing_save(&db, "FG01", &[
            RoutingOp { id: 0, item_code: "FG01".into(), version: String::new(), seq: 1,
                op_code: "OP1".into(), op_name: "下料".into(), work_center: "WC1".into(),
                std_hours: m("2"), rate: m("50"), qc_required: false },
            RoutingOp { id: 0, item_code: "FG01".into(), version: String::new(), seq: 2,
                op_code: "OP2".into(), op_name: "装配".into(), work_center: "WC2".into(),
                std_hours: m("3"), rate: m("60"), qc_required: false },
        ]).unwrap();
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
        let id = approval_start(&db, "claim", 1, "报销单", "alice", &["bob".into(), "carol".into()])
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

    #[test]
    fn summary_and_multicol() {
        let db = tmpdb("sumtab");
        let p = Period::new(2026, 1).unwrap();
        post(&db, p, 1, &[("100201", "1000", "0"), ("600101", "0", "1000")]);
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
        post(&db, p, 1, &[("100201", "100000", "0"), ("600101", "0", "100000")]);
        post(&db, p, 2, &[("6401", "60000", "0"), ("100201", "0", "60000")]);
        let ratios = fin_ratios(&db, p, p, None).unwrap();
        assert!(ratios.len() >= 6);
        let gm = ratios.iter().find(|r| r.key == "gross_margin").unwrap();
        // 毛利率 = (100000-60000)/100000 = 40%
        assert_eq!(gm.value, m("0.4"));
    }

    #[test]
    fn financial_analysis_trend_and_drivers() {
        let db = tmpdb("finanalysis");
        let p = Period::new(2026, 2).unwrap();
        crate::auxs::insert(&db, &AuxEntity::new(AuxKind::Bank, "B01", "工行")).unwrap();
        // 1 月：收入 100000，成本 40000
        let jan = Period::new(2026, 1).unwrap();
        post(&db, jan, 1, &[("100201", "100000", "0"), ("600101", "0", "100000")]);
        post(&db, jan, 2, &[("6401", "40000", "0"), ("100201", "0", "40000")]);
        // 2 月：收入 30000，成本 10000
        post(&db, p, 1, &[("100201", "30000", "0"), ("600101", "0", "30000")]);
        post(&db, p, 2, &[("6401", "10000", "0"), ("100201", "0", "10000")]);

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
