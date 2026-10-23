//! 可视化工作流（对标金蝶审批流设计器）
//!
//! 节点-动作模型：start / approve（审批）/ condition / message 节点 + normal / reject 连线，
//! 节点携带画布坐标 (x, y) 供前端 SVG 画布渲染。**无已发布流程 = 走原固定审批链**
//! （默认流，向后兼容：流程不配置时所有既有审批行为不变）。
//!
//! 运行时：单据的第一次审批动作自动创建实例并推进到下一节点；走到终点（无 normal
//! 出边的 approve 节点即流程终点）时返回 `Gate::Final`，由调用方执行其原有业务审批
//! （报价转已审批 / 请购批准 / 报销通过 / 收付款单出凭证）。驳回沿 reject 连线或节点
//! reject_to 移动，无路径则实例终态 rejected（业务单据状态由调用方自行处理）。

use fincore::{Money, Perm, User};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::{Db, DbResult};

pub const BIZ_QUOTATION: &str = "quotation";
pub const BIZ_PURCHASE_REQ: &str = "purchase_req";
pub const BIZ_CLAIM: &str = "claim";
pub const BIZ_RECEIPT: &str = "receipt";
pub const BIZ_PURCHASE_ORDER: &str = "purchase_order";
pub const BIZ_SALES_ORDER: &str = "sales_order";
pub const BIZ_PRODUCTION_ORDER: &str = "production_order";

/// 业务类型 → 中文（前端下拉与实例列表展示）
///
/// 顺序按「单据生命周期」排：报价 → 请购 → 采购订单 → 生产订单 → 销售订单 → 报销 → 收付款。
/// 这样前端下拉里相邻的就是上下游单据，配置流程时不容易把顺序看反。
pub const ALL_BIZ: &[(&str, &str)] = &[
    (BIZ_QUOTATION, "报价单"),
    (BIZ_PURCHASE_REQ, "请购单"),
    (BIZ_PURCHASE_ORDER, "采购订单"),
    (BIZ_PRODUCTION_ORDER, "生产订单"),
    (BIZ_SALES_ORDER, "销售订单"),
    (BIZ_CLAIM, "报销单"),
    (BIZ_RECEIPT, "收付款单"),
];

pub fn biz_label(t: &str) -> String {
    ALL_BIZ
        .iter()
        .find(|(k, _)| *k == t)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| t.to_string())
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn role_code(r: &Role) -> String {
    serde_json::to_value(r)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

use fincore::Role;

#[cfg(test)]
use fincore::Period;

/// 画布节点
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WfNode {
    pub id: String,
    /// start / approve / condition / message
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub name: String,
    /// 允许审批的角色 code（空 = 任何具备审批权的人）
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default = "default_strategy")]
    pub strategy: String,
    #[serde(default)]
    pub reject_to: String,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
}

fn default_strategy() -> String {
    "all".to_string()
}

/// 画布连线
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WfEdge {
    pub id: String,
    #[serde(rename = "from")]
    pub from_node: String,
    #[serde(rename = "to")]
    pub to_node: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub condition: String,
}

fn default_kind() -> String {
    "normal".to_string()
}

/// 流程定义（含节点与连线）
#[derive(Clone, Debug, Serialize)]
pub struct WfFlow {
    pub id: i64,
    pub name: String,
    pub biz_type: String,
    /// draft / published
    pub status: String,
    pub nodes: Vec<WfNode>,
    pub edges: Vec<WfEdge>,
    pub created_by: String,
    pub updated_at: String,
}

/// 客户端保存入参（节点/连线来自 JSON）
#[derive(Clone, Debug, Deserialize)]
pub struct WfFlowInput {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    pub biz_type: String,
    pub nodes: Vec<WfNode>,
    pub edges: Vec<WfEdge>,
}

/// 运行轨迹条目
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WfLogEntry {
    pub node: String,
    pub action: String,
    pub who: String,
    pub at: String,
}

/// 运行实例（列表用）
#[derive(Clone, Debug, Serialize)]
pub struct WfInstance {
    pub id: i64,
    pub flow_id: i64,
    pub flow_name: String,
    pub biz_type: String,
    pub biz_label: String,
    pub biz_id: i64,
    pub current_node: String,
    pub current_label: String,
    /// running / approved / rejected
    pub status: String,
    pub log: Vec<WfLogEntry>,
    pub created_at: String,
}

/// 审批拦截结果
#[derive(Clone, Debug)]
pub enum Gate {
    /// 该类型没有已发布流程 → 调用方执行原有直接审批（默认流）
    NoFlow,
    /// 已推进到下一节点（未到终态）：调用方只回 pending 提示，不执行业务动作
    Pending { next: String },
    /// 实例到终态：调用方执行其原有业务审批/驳回
    Final { approved: bool },
}

fn load_nodes(tx: &rusqlite::Connection, flow_id: i64) -> DbResult<Vec<WfNode>> {
    let mut st = tx.prepare(
        "SELECT id,type,name,participants,strategy,reject_to,x,y
         FROM workflow_node WHERE flow_id=?1 ORDER BY seq, rowid",
    )?;
    let rows = st
        .query_map([flow_id], |r| {
            let p: String = r.get(3)?;
            Ok(WfNode {
                id: r.get(0)?,
                node_type: r.get(1)?,
                name: r.get(2)?,
                participants: serde_json::from_str(&p).unwrap_or_default(),
                strategy: r.get(4)?,
                reject_to: r.get(5)?,
                x: r.get(6)?,
                y: r.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn load_edges(tx: &rusqlite::Connection, flow_id: i64) -> DbResult<Vec<WfEdge>> {
    let mut st = tx.prepare(
        "SELECT id,from_node,to_node,kind,condition
         FROM workflow_edge WHERE flow_id=?1 ORDER BY seq, rowid",
    )?;
    let rows = st
        .query_map([flow_id], |r| {
            Ok(WfEdge {
                id: r.get(0)?,
                from_node: r.get(1)?,
                to_node: r.get(2)?,
                kind: r.get(3)?,
                condition: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn flow_of(tx: &rusqlite::Connection, id: i64) -> DbResult<Option<WfFlow>> {
    let row = tx
        .query_row(
            "SELECT id,name,biz_type,status,created_by,updated_at FROM workflow_flow WHERE id=?1",
            rusqlite::params![id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((id, name, biz_type, status, created_by, updated_at)) = row else {
        return Ok(None);
    };
    Ok(Some(WfFlow {
        id,
        name,
        biz_type,
        status,
        nodes: load_nodes(tx, id)?,
        edges: load_edges(tx, id)?,
        created_by,
        updated_at,
    }))
}

pub fn flow_list(db: &Db) -> DbResult<Vec<WfFlow>> {
    let mut st = db.conn().prepare(
        "SELECT id FROM workflow_flow ORDER BY CASE status WHEN 'published' THEN 0 ELSE 1 END, id DESC",
    )?;
    let ids: Vec<i64> = st
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(f) = flow_of(db.conn(), id)? {
            out.push(f);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 预置流程模板
// ---------------------------------------------------------------------------

/// 预置审批流程模板（对标金蝶云·星空的默认审批链）
///
/// 存在理由：空白画布要求用户自己拉节点、连线、配角色，等于让每个账套都从零
/// 设计一遍审批链——实际情况是绝大多数企业直接用行业惯例。模板把这些惯例固化
/// 下来一键落地，之后仍可在画布上改。
///
/// 三条设计约束（都是从引擎实际行为里读出来的，不是想当然；其中两条最初
/// 我都写错了，是被 `amount_tiered` 的测试打出来的）：
/// ① **「有条件边的节点」必须有兜底出边**。`branch_next` 在条件全不匹配且没有
///    空条件出边时直接 `Err("所有条件分支均不满足…")`，单据直接卡死。终止要靠
///    指向一个**无出边的 `message` 节点**——消息节点被自动跳过且 `branch_next`
///    返回 `None`，实例随即置 approved（见 `intercept` 的消息节点循环）。
///    单纯「让审批节点没有出边」是不行的：实例会停在那儿等人批一个终点节点。
/// ② **实例起点是 nodes 数组里第一个 `approve` 节点**（`first_approve`），
///    不是从 start 沿边走。所以条件分支的第一个审批节点必须排在数组前面。
/// ③ **条件字段由 `cond_context` 提供**，只有四类单据有：quotation/claim/receipt/
///    purchase_req，其中只有 claim 与 receipt 有 `amount`。给没有 `amount` 的单据
///    套金额分级模板会在审批时直接报「条件配置错误」——所以模板显式声明适用
///    单据，`apply_template` 强制校验。
///
/// 另外两条**引擎现状边界**，模板无法弥补、也不该假装能弥补：
/// - 持 VoucherAudit 的主管/审核人/管理员可批**任意**节点（`audit_ok` 短路绕过
///   参与人判定），所以模板里的「出纳复核」只是流程留痕；真正拦住「会计自签
///   现金凭证」的是账套参数 `require_cashier`（凭证级硬门）。两者不能互替。
/// - 条件分支是**按边顺序取第一条命中**，没有「多路并行」语义。
#[derive(Clone, Debug, Serialize)]
pub struct WfTemplate {
    pub key: String,
    pub name: String,
    pub desc: String,
    /// 适用业务类型（`ALL_BIZ` 的 key）
    pub biz_types: Vec<String>,
    /// 依赖的条件字段（如 ["amount"]），无则空
    pub requires_fields: Vec<String>,
    pub nodes: Vec<WfNode>,
    pub edges: Vec<WfEdge>,
}

fn tnode(id: &str, ty: &str, name: &str, roles: &[&str], x: f64, y: f64) -> WfNode {
    WfNode {
        id: id.to_string(),
        node_type: ty.to_string(),
        name: name.to_string(),
        participants: roles.iter().map(|s| s.to_string()).collect(),
        strategy: default_strategy(),
        reject_to: String::new(),
        x,
        y,
    }
}

fn tedge(from: &str, to: &str) -> WfEdge {
    WfEdge {
        id: format!("{from}->{to}"),
        from_node: from.to_string(),
        to_node: to.to_string(),
        kind: default_kind(),
        condition: String::new(),
    }
}

fn tedge_if(from: &str, to: &str, cond: &str) -> WfEdge {
    WfEdge {
        condition: cond.to_string(),
        ..tedge(from, to)
    }
}

/// 预置模板清单。
///
/// 金额阈值取 5000 / 50000 是财务惯例里的常见档位（小额主管批、中额加财务主管、
/// 大额再上总经理），不是某一行业的强制标准——企业应按自身授权制度改。
///
/// ## 为什么有「小微」系列模板（`micro_*`）
///
/// 其余模板的审批节点几乎都写 `supervisor`（财务主管）。那是**有主管的
/// 中大型企业**的形态。可现实中大量小企业根本没有财务主管这个岗位 ——
/// 只有会计与出纳两人。此时照搬那些模板会有两个问题：
///
/// 1. **审批人名不副实**。`intercept` 的门禁是「审核权限恒可批，否则须命中
///    节点参与人角色」；会计角色**没有** `VoucherAudit`（见 `Role::Accountant`
///    的注释：录入与记账同账户完成），所以会计批不了 `supervisor` 节点。
///    实际批的人是**平台管理员**——审批链上挂着一个财务系统管理员，
///    名字却写着「财务主管审批」，追溯时只会误导。
/// 2. **多一跳且无内控增益**。2 人公司本来就没有能互相牵制的第三方，
///    硬套两级审批只是把单据卡在系统里等人点。
///
/// 所以这里给出**与「会计 + 出纳」两岗结构相符**的模板，让小企业不必去改
/// 别人模板里的角色名。角色写 `accountant` / `cashier` 是刻意的：会签兜底
/// 让有审核权限的人也能代批（见 `intercept` 里 `audit_ok` 计票那段），
/// 所以小公司即便只有一个人兼两岗，流程照样走得完。
pub fn templates() -> Vec<WfTemplate> {
    vec![
        WfTemplate {
            key: "micro_single".into(),
            name: "小微单级（会计审）".into(),
            desc: "制单 → 会计审批。**给只有会计、没有财务主管的小企业**：\
                   订单/请购/报价等由会计一岗签出即可，省掉一个不存在的岗位。\
                   注意：制单与审批通常是同一个人，属于**弱内控**——如果还想留一道关，\
                   建议改用「小微两岗」或开启账套参数「审核环节」由出纳/管理员审凭证。".into(),
            biz_types: vec![
                BIZ_QUOTATION.into(),
                BIZ_PURCHASE_REQ.into(),
                BIZ_PURCHASE_ORDER.into(),
                BIZ_SALES_ORDER.into(),
                BIZ_PRODUCTION_ORDER.into(),
            ],
            requires_fields: vec![],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 140.0),
                tnode("acct", "approve", "会计审批", &["accountant"], 250.0, 140.0),
            ],
            edges: vec![tedge("start", "acct")],
        },
        WfTemplate {
            key: "micro_two_role".into(),
            name: "小微两岗（会计 → 出纳）".into(),
            desc: "制单 → 会计复核 → 出纳付款。**收付款/报销专用**，对应「会计管账、\
                   出纳管钱」的两岗分工：出纳节点对应实际付款动作，也是资金留痕的一环。\
                   账套参数「要求出纳签字」开启时，付款凭证记账前还会再要一次出纳签字。".into(),
            biz_types: vec![BIZ_RECEIPT.into(), BIZ_CLAIM.into()],
            requires_fields: vec![],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 140.0),
                tnode("acct", "approve", "会计复核", &["accountant"], 240.0, 140.0),
                tnode("cash", "approve", "出纳付款", &["cashier"], 430.0, 140.0),
            ],
            edges: vec![tedge("start", "acct"), tedge("acct", "cash")],
        },
        WfTemplate {
            key: "simple".into(),
            name: "单级审批（主管）".into(),
            desc: "制单 → 主管审批。小额、高频单据用（如报价单、请购单）。\
                   ⚠ 需要企业里真有「财务主管」岗位；没有的话请用「小微单级」。".into(),
            biz_types: vec![BIZ_QUOTATION.into(), BIZ_PURCHASE_REQ.into()],
            requires_fields: vec![],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 140.0),
                tnode("lead", "approve", "主管审批", &["supervisor"], 240.0, 140.0),
            ],
            edges: vec![tedge("start", "lead")],
        },
        WfTemplate {
            key: "standard".into(),
            name: "标准两级（业务 → 财务）".into(),
            desc: "⚠ 本模板需要企业里真有「财务主管」岗位；没有的话请改用「小微单级」/「小微两岗」。制单 → 业务主管 → 财务主管。业务与财务两道分离，通用性最好的一档。".into(),
            biz_types: vec![
                BIZ_QUOTATION.into(),
                BIZ_PURCHASE_REQ.into(),
                BIZ_CLAIM.into(),
            ],
            requires_fields: vec![],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 140.0),
                tnode("biz", "approve", "业务主管审批", &["supervisor"], 240.0, 140.0),
                tnode("fin", "approve", "财务主管审批", &["supervisor"], 420.0, 140.0),
            ],
            edges: vec![tedge("start", "biz"), tedge("biz", "fin")],
        },
        WfTemplate {
            key: "claim_full".into(),
            name: "报销三级（部门 → 财务 → 出纳）".into(),
            desc: "⚠ 本模板需要企业里真有「财务主管」岗位；没有的话请改用「小微单级」/「小微两岗」。制单 → 部门负责人 → 财务主管 → 出纳付款。报销单专用，出纳节点对应实际付款动作。".into(),
            biz_types: vec![BIZ_CLAIM.into()],
            requires_fields: vec![],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 140.0),
                tnode("dept", "approve", "部门负责人审批", &["supervisor"], 230.0, 140.0),
                tnode("fin", "approve", "财务主管审批", &["supervisor"], 400.0, 140.0),
                tnode("cash", "approve", "出纳付款", &["cashier"], 570.0, 140.0),
            ],
            edges: vec![
                tedge("start", "dept"),
                tedge("dept", "fin"),
                tedge("fin", "cash"),
            ],
        },
        WfTemplate {
            key: "funds".into(),
            name: "资金单据（财务 → 出纳）".into(),
            desc: "⚠ 本模板需要企业里真有「财务主管」岗位；没有的话请改用「小微单级」/「小微两岗」。制单 → 财务主管 → 出纳复核。收付款单专用；出纳节点为流程留痕，硬门仍需账套参数「出纳签字」配合。".into(),
            biz_types: vec![BIZ_RECEIPT.into()],
            requires_fields: vec![],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 140.0),
                tnode("fin", "approve", "财务主管审批", &["supervisor"], 240.0, 140.0),
                tnode("cash", "approve", "出纳复核", &["cashier"], 420.0, 140.0),
                tnode("msg", "message", "通知付款", &[], 600.0, 140.0),
            ],
            edges: vec![
                tedge("start", "fin"),
                tedge("fin", "cash"),
                tedge("cash", "msg"),
            ],
        },
        WfTemplate {
            key: "order_standard".into(),
            name: "订单两级（业务 → 财务）".into(),
            desc: "⚠ 本模板需要企业里真有「财务主管」岗位；没有的话请改用「小微单级」。\
                  制单 → 业务主管 → 财务主管。采购/销售订单专用：订单一旦签出就是对外的\
                  商业承诺，金额风险高于报价与请购，所以固定两级而不是单级。".into(),
            biz_types: vec![
                BIZ_PURCHASE_ORDER.into(),
                BIZ_SALES_ORDER.into(),
            ],
            requires_fields: vec![],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 140.0),
                tnode("biz", "approve", "业务主管审批", &["supervisor"], 240.0, 140.0),
                tnode("fin", "approve", "财务主管审批", &["supervisor"], 420.0, 140.0),
            ],
            edges: vec![tedge("start", "biz"), tedge("biz", "fin")],
        },
        WfTemplate {
            key: "order_tiered".into(),
            name: "订单金额分级".into(),
            desc: "⚠ 本模板需要企业里真有「财务主管」岗位；没有的话请改用「小微单级」。\
                  按**价税合计**分级：≤5万 业务主管批完；>5万 加财务主管；>50万 再上总经理。\
                  生产订单没有金额，改用数量模板（见「生产订单两段审」）。".into(),
            biz_types: vec![BIZ_PURCHASE_ORDER.into(), BIZ_SALES_ORDER.into()],
            requires_fields: vec!["amount".into()],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 200.0),
                // 起点必须是数组里第一个 approve 节点（见约束②）
                tnode("biz", "approve", "业务主管审批", &["supervisor"], 230.0, 200.0),
                tnode("fin", "approve", "财务主管审批", &["supervisor"], 420.0, 120.0),
                tnode("gm", "approve", "总经理审批", &["supervisor"], 610.0, 60.0),
                tnode("done", "message", "主管批完归档", &[], 230.0, 300.0),
            ],
            // 三条出边 + 一条空条件兜底：兜底不可省（见约束①）
            edges: vec![
                tedge("start", "biz"),
                tedge_if("biz", "gm", "amount > 500000"),
                tedge_if("biz", "fin", "amount > 50000"),
                tedge("biz", "done"),
                tedge("fin", "gm"),
            ],
        },
        WfTemplate {
            key: "prod_two_stage".into(),
            name: "生产订单两段审（计划 → 开工）".into(),
            desc: "⚠ 本模板需要企业里真有「生产计划员/生产负责人/厂长」岗位；\
                  只有会计与出纳的话请改用「小微单级」。\
                  制单 → 生产计划员确认（核对 BOM/产能/库存）→ 生产负责人批准开工。\
                  条件用 planned_qty 数量而非金额——生产订单本来就没有金额字段。".into(),
            biz_types: vec![BIZ_PRODUCTION_ORDER.into()],
            requires_fields: vec!["planned_qty".into()],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 200.0),
                tnode("plan", "approve", "生产计划员确认", &["supervisor"], 240.0, 200.0),
                tnode("pm", "approve", "生产负责人批准", &["supervisor"], 430.0, 200.0),
                tnode("big", "approve", "厂长审批（大批量）", &["supervisor"], 430.0, 80.0),
                tnode("done", "message", "排产完成", &[], 620.0, 200.0),
            ],
            edges: vec![
                tedge("start", "plan"),
                tedge_if("plan", "big", "planned_qty > 1000"),
                tedge("plan", "pm"),
                tedge("big", "pm"),
                tedge("pm", "done"),
            ],
        },
        WfTemplate {
            key: "amount_tiered".into(),
            name: "金额分级审批".into(),
            desc: "⚠ 本模板需要企业里真有「财务主管」岗位；没有的话请改用「小微单级」/「小微两岗」。≤5000 主管批完归档；>5000 加财务主管；>50000 直接上总经理。阈值与节点均可在画布上改。".into(),
            biz_types: vec![BIZ_CLAIM.into(), BIZ_RECEIPT.into()],
            requires_fields: vec!["amount".into()],
            nodes: vec![
                tnode("start", "start", "制单", &[], 60.0, 200.0),
                // 注意顺序：lead 必须是数组里第一个 approve 节点（= 实例起点，见约束②）
                tnode("lead", "approve", "主管审批", &["supervisor"], 230.0, 200.0),
                tnode("fin", "approve", "财务主管审批", &["supervisor"], 420.0, 120.0),
                tnode("gm", "approve", "总经理审批", &["supervisor"], 610.0, 60.0),
                // 无出边的消息节点 = 流程终点（见约束①）
                tnode("done", "message", "主管批完归档", &[], 230.0, 300.0),
            ],
            // lead 三条出边：先判「>50000」再判「>5000」，最后一条空条件兜底。
            // 兜底不可省——省了小额单据会在审批时直接报错卡死。
            edges: vec![
                tedge("start", "lead"),
                tedge_if("lead", "gm", "amount > 50000"),
                tedge_if("lead", "fin", "amount > 5000"),
                tedge("lead", "done"),
                tedge("fin", "gm"),
            ],
        },
    ]
}

pub fn template_by_key(key: &str) -> Option<WfTemplate> {
    templates().into_iter().find(|t| t.key == key)
}

/// 按模板落一条流程（草稿态，需显式发布才生效）
///
/// 刻意**不自动发布**：预置模板描述的是行业惯例，不是这家企业的授权制度。
/// 直接发布等于替企业做了内控决策，而且一旦有单据已在流转，事后改流程会让
/// 在途实例的 `current_node` 指向不存在的节点（`intercept` 报「流程可能被改」）。
pub fn apply_template(db: &Db, key: &str, biz_type: &str, who: &str) -> DbResult<WfFlow> {
    let tpl = template_by_key(key.trim())
        .ok_or_else(|| fincore::FinError::not_found("流程模板不存在"))?;
    if ALL_BIZ.iter().all(|(k, _)| *k != biz_type) {
        return Err(fincore::FinError::validate("非法业务类型").into());
    }
    if tpl.biz_types.iter().all(|b| b != biz_type) {
        return Err(fincore::FinError::validate(format!(
            "模板【{}】不适用于「{}」，可选：{}",
            tpl.name,
            biz_label(biz_type),
            tpl.biz_types
                .iter()
                .map(|b| biz_label(b))
                .collect::<Vec<_>>()
                .join("、")
        ))
        .into());
    }
    // 依赖字段必须真的是该单据类型**会提供**的字段。
    //
    // 这条校验挡住的是「模板落草稿时一切正常，单据提交到审批才报『条件配置
    // 错误』把单卡死」——条件求值缺字段返回 Err，而实例已经建了，卡在第一个
    // 审批节点既过不去也退不出。生产订单没有金额就是典型：把金额分级模板
    // 套到生产订单上，planned_qty=5 会被拿去比 5000。
    if let Some(missing) = missing_cond_fields(biz_type, &tpl.requires_fields) {
        return Err(fincore::FinError::validate(format!(
            "模板【{}】依赖字段「{}」在「{}」上不存在（该单据可用：{}）",
            tpl.name,
            missing.join("、"),
            biz_label(biz_type),
            cond_fields_of(biz_type).join("、")
        ))
        .into());
    }
    let input = WfFlowInput {
        id: 0,
        name: format!("{}·{}", biz_label(biz_type), tpl.name),
        biz_type: biz_type.to_string(),
        nodes: tpl.nodes,
        edges: tpl.edges,
    };
    let id = flow_save(db, &input, who)?;
    flow_of(db.conn(), id)?.ok_or_else(|| fincore::FinError::msg("流程模板应用失败").into())
}

/// 保存流程（新建或更新）：整体替换节点与连线；校验 start 恰好一个、连线端点存在。
pub fn flow_save(db: &Db, f: &WfFlowInput, who: &str) -> DbResult<i64> {
    if f.name.trim().is_empty() {
        return Err(fincore::FinError::validate("流程名称必填").into());
    }
    if ALL_BIZ.iter().all(|(k, _)| *k != f.biz_type) {
        return Err(fincore::FinError::validate("非法业务类型").into());
    }
    if f.nodes.is_empty() {
        return Err(fincore::FinError::validate("至少需要一个节点").into());
    }
    let starts = f.nodes.iter().filter(|n| n.node_type == "start").count();
    if starts != 1 {
        return Err(fincore::FinError::validate("开始节点必须恰好 1 个").into());
    }
    let mut ids: std::collections::HashSet<&str> = f.nodes.iter().map(|n| n.id.as_str()).collect();
    if ids.len() != f.nodes.len() {
        return Err(fincore::FinError::validate("节点 id 重复").into());
    }
    for e in &f.edges {
        if !ids.contains(e.from_node.as_str()) || !ids.contains(e.to_node.as_str()) {
            return Err(fincore::FinError::validate(format!(
                "连线端点不存在：{} → {}",
                e.from_node, e.to_node
            ))
            .into());
        }
    }
    for n in &f.nodes {
        if !n.reject_to.is_empty() && !ids.contains(n.reject_to.as_str()) {
            return Err(fincore::FinError::validate(format!(
                "驳回目标节点不存在：{}",
                n.reject_to
            ))
            .into());
        }
    }
    // 条件边引用的字段必须真的是该单据类型**会提供**的字段。
    //
    // 与 apply_template 里的同一道校验是一对：那边拦模板，这边拦**手绘**的流程。
    // 只做一边都不够——模板能过校验不代表管理员不会自己在画布上写一条
    // `amount > 5000` 挂到生产订单上。
    //
    // 为什么必须在这里拦而不是等审批时：条件求值缺字段返回 Err，而实例**已经
    // 建好了**，单据卡在第一个审批节点既过不去也退不出，只能手工改库。
    // 「画布上少一个字段、月底发现一批单卡死」是比「保存时报错」差得多的结局。
    {
        let have = cond_fields_of(&f.biz_type);
        let mut bad: Vec<String> = Vec::new();
        for e in &f.edges {
            for field in condition_fields(&e.condition) {
                if !have.iter().any(|h| *h == field) {
                    bad.push(format!("{field}（连线 {} → {}）", e.from_node, e.to_node));
                }
            }
        }
        bad.dedup();
        if !bad.is_empty() {
            return Err(fincore::FinError::validate(format!(
                "条件字段「{}」在「{}」上不存在（该单据可用：{}）",
                bad.join("、"),
                biz_label(&f.biz_type),
                have.join("、")
            ))
            .into());
        }
    }
    ids.clear();
    let tx = db.write_tx()?;
    let flow_id = if f.id > 0 {
        let n = tx.execute(
            "UPDATE workflow_flow SET name=?2, biz_type=?3, updated_at=?4 WHERE id=?1",
            rusqlite::params![f.id, f.name.trim(), f.biz_type, now()],
        )?;
        if n == 0 {
            return Err(fincore::FinError::not_found("流程不存在").into());
        }
        f.id
    } else {
        tx.execute(
            "INSERT INTO workflow_flow(name,biz_type,status,created_by,created_at,updated_at)
             VALUES(?1,?2,'draft',?3,?4,?4)",
            rusqlite::params![f.name.trim(), f.biz_type, who, now()],
        )?;
        tx.last_insert_rowid()
    };
    tx.execute("DELETE FROM workflow_node WHERE flow_id=?1", [flow_id])?;
    tx.execute("DELETE FROM workflow_edge WHERE flow_id=?1", [flow_id])?;
    for (i, n) in f.nodes.iter().enumerate() {
        tx.execute(
            "INSERT INTO workflow_node(id,flow_id,type,name,participants,strategy,reject_to,seq,x,y)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                n.id,
                flow_id,
                n.node_type,
                n.name,
                serde_json::to_string(&n.participants)?,
                n.strategy,
                n.reject_to,
                i as i64,
                n.x,
                n.y
            ],
        )?;
    }
    for (i, e) in f.edges.iter().enumerate() {
        tx.execute(
            "INSERT INTO workflow_edge(id,flow_id,from_node,to_node,kind,condition,seq)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                e.id,
                flow_id,
                e.from_node,
                e.to_node,
                e.kind,
                e.condition,
                i as i64
            ],
        )?;
    }
    tx.commit()?;
    Ok(flow_id)
}

/// 发布 / 撤回发布（发布要求至少一个审批节点）
pub fn flow_set_status(db: &Db, id: i64, publish: bool, who: &str) -> DbResult<()> {
    let tx = db.write_tx()?;
    if publish {
        let nodes = load_nodes(&tx, id)?;
        if nodes.is_empty() {
            return Err(fincore::FinError::validate("流程不存在或没有节点").into());
        }
        if !nodes.iter().any(|n| n.node_type == "approve") {
            return Err(fincore::FinError::validate("发布前至少需要一个审批节点").into());
        }
    }
    let status = if publish { "published" } else { "draft" };
    let n = tx.execute(
        "UPDATE workflow_flow SET status=?2, updated_at=?3 WHERE id=?1",
        rusqlite::params![id, status, now()],
    )?;
    if n == 0 {
        return Err(fincore::FinError::not_found("流程不存在").into());
    }
    let _ = who;
    tx.commit()?;
    Ok(())
}

/// 删除流程（有运行中实例时拒绝）
pub fn flow_delete(db: &Db, id: i64) -> DbResult<()> {
    let tx = db.write_tx()?;
    let running: i64 = tx.query_row(
        "SELECT COUNT(*) FROM workflow_instance WHERE flow_id=?1 AND status='running'",
        [id],
        |r| r.get(0),
    )?;
    if running > 0 {
        return Err(fincore::FinError::state("该流程有运行中的审批实例，不能删除").into());
    }
    tx.execute("DELETE FROM workflow_instance WHERE flow_id=?1", [id])?;
    tx.execute("DELETE FROM workflow_edge WHERE flow_id=?1", [id])?;
    tx.execute("DELETE FROM workflow_node WHERE flow_id=?1", [id])?;
    let n = tx.execute("DELETE FROM workflow_flow WHERE id=?1", [id])?;
    if n == 0 {
        return Err(fincore::FinError::not_found("流程不存在").into());
    }
    tx.commit()?;
    Ok(())
}

/// 该业务类型已发布的流程（取最新发布）
pub fn published_flow_for(db: &Db, biz_type: &str) -> DbResult<Option<WfFlow>> {
    let id: Option<i64> = db
        .conn()
        .query_row(
            "SELECT id FROM workflow_flow WHERE biz_type=?1 AND status='published'
             ORDER BY id DESC LIMIT 1",
            [biz_type],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(id) => flow_of(db.conn(), id),
        None => Ok(None),
    }
}

/// 流程全部实例（含当前节点中文名与轨迹）
pub fn instances(db: &Db) -> DbResult<Vec<WfInstance>> {
    let mut st = db.conn().prepare(
        "SELECT id,flow_id,biz_type,biz_id,current_node,status,log_json,created_at
         FROM workflow_instance ORDER BY id DESC LIMIT 500",
    )?;
    let raw: Vec<(i64, i64, String, i64, String, String, String, String)> = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut flows: std::collections::HashMap<i64, Option<WfFlow>> =
        std::collections::HashMap::new();
    let mut out = Vec::new();
    for (id, flow_id, biz_type, biz_id, current_node, status, log_json, created_at) in raw {
        let flow = match flows.entry(flow_id) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let f = flow_of(db.conn(), flow_id)?;
                v.insert(f.clone());
                f
            }
        };
        let log: Vec<WfLogEntry> = serde_json::from_str(&log_json).unwrap_or_default();
        let (flow_name, current_label) = match &flow {
            Some(f) => (
                f.name.clone(),
                f.nodes
                    .iter()
                    .find(|n| n.id == current_node)
                    .map(node_label)
                    .unwrap_or_else(|| current_node.clone()),
            ),
            None => ("(流程已删除)".to_string(), current_node.clone()),
        };
        out.push(WfInstance {
            id,
            flow_id,
            flow_name,
            biz_type: biz_type.clone(),
            biz_label: biz_label(&biz_type).to_string(),
            biz_id,
            current_node,
            current_label,
            status,
            log,
            created_at,
        });
    }
    Ok(out)
}

fn node_label(n: &WfNode) -> String {
    if !n.name.trim().is_empty() {
        n.name.trim().to_string()
    } else {
        match n.node_type.as_str() {
            "start" => "开始".to_string(),
            "approve" => "审批".to_string(),
            "condition" => "条件".to_string(),
            _ => "消息".to_string(),
        }
    }
}

fn first_approve(flow: &WfFlow) -> Option<&WfNode> {
    flow.nodes.iter().find(|n| n.node_type == "approve")
}

/// 某业务类型**会提供**哪些条件字段。
///
/// 与 `cond_context` 严格对应：这里多列一个字段，条件求值就会在真单据上
/// 因缺字段报 Err；这里少列一个，用户配的条件边就静默走兜底分支。两个函数
/// 必须一起改——`cond_fields_match_context` 那个用例就是钉住这一点的。
pub fn cond_fields_of(biz_type: &str) -> Vec<&'static str> {
    match biz_type {
        BIZ_QUOTATION => vec!["qty", "amount", "customer_code", "item_code"],
        BIZ_CLAIM => vec!["amount", "applicant", "dept"],
        BIZ_RECEIPT => vec!["amount", "kind", "party"],
        BIZ_PURCHASE_REQ => vec!["qty", "item_code", "requester"],
        BIZ_PURCHASE_ORDER => vec![
            "amount",
            "net_amount",
            "tax",
            "supplier_code",
            "prepared_by",
        ],
        BIZ_SALES_ORDER => vec![
            "amount",
            "net_amount",
            "tax",
            "customer_code",
            "prepared_by",
        ],
        BIZ_PRODUCTION_ORDER => vec![
            "planned_qty",
            "item_code",
            "work_center",
            "order_kind",
            "supplier_code",
            "prepared_by",
        ],
        _ => vec![],
    }
}

/// 从条件串里抽出引用的字段名（`amount > 5000` → `["amount"]`；空条件 → 空）
///
/// 只取第一段：条件文法固定为 `字段 操作 值`，值里的引号内容不会被误认成字段。
fn condition_fields(cond: &str) -> Vec<String> {
    let c = cond.trim();
    if c.is_empty() {
        return Vec::new();
    }
    c.split_whitespace()
        .next()
        .map(|f| f.trim_matches('"').to_string())
        .into_iter()
        .filter(|f| !f.is_empty())
        .collect()
}

/// 返回 `need` 里该单据**不提供**的字段；全都有则返回 None
fn missing_cond_fields(biz_type: &str, need: &[String]) -> Option<Vec<String>> {
    if need.is_empty() {
        return None;
    }
    let have = cond_fields_of(biz_type);
    let missing: Vec<String> = need
        .iter()
        .filter(|f| !have.contains(&f.as_str()))
        .cloned()
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(missing)
    }
}

/// 条件分支上下文：按业务类型取单据属性（统一字符串；数值比较时解析）
fn cond_context(
    db: &Db,
    biz_type: &str,
    biz_id: i64,
) -> DbResult<std::collections::BTreeMap<String, String>> {
    let mut m: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let money = |v: Money| format!("{}", crate::workbench::money_f64(v));
    match biz_type {
        "quotation" => {
            if let Some(q) = crate::sales::quo_get(db, biz_id)? {
                m.insert("qty".into(), money(q.qty));
                m.insert("amount".into(), money(q.qty * q.unit_price));
                m.insert("customer_code".into(), q.customer_code);
                m.insert("item_code".into(), q.item_code);
            }
        }
        "claim" => {
            if let Some(c) = crate::business::claim_get(db, biz_id)? {
                m.insert("amount".into(), money(c.amount));
                m.insert("applicant".into(), c.applicant);
                m.insert("dept".into(), c.dept);
            }
        }
        "receipt" => {
            if let Some(r) = crate::receipt::receipt_list(db)?
                .into_iter()
                .find(|d| d.id == biz_id)
            {
                m.insert("amount".into(), money(r.amount));
                m.insert("kind".into(), r.kind);
                m.insert("party".into(), r.party);
            }
        }
        "purchase_req" => {
            if let Some(r) = crate::procurement::pr_get(db, biz_id)? {
                m.insert("qty".into(), money(r.qty));
                m.insert("item_code".into(), r.item_code);
                m.insert("requester".into(), r.requester);
            }
        }
        BIZ_PURCHASE_ORDER => {
            if let Some(o) = crate::scm::po_get(db, biz_id)? {
                // 条件字段用「未税金额」还是「价税合计」要选清楚：
                // 审批权限通常按合同/订单的**价税合计**授权（含税才是真实承诺），
                // 所以 amount 取 total_amount + total_tax，税额单列 tax 备用。
                m.insert("amount".into(), money(o.total_amount + o.total_tax));
                m.insert("net_amount".into(), money(o.total_amount));
                m.insert("tax".into(), money(o.total_tax));
                m.insert("supplier_code".into(), o.supplier_code);
                m.insert("prepared_by".into(), o.prepared_by);
            }
        }
        BIZ_SALES_ORDER => {
            if let Some(o) = crate::scm::so_get(db, biz_id)? {
                m.insert("amount".into(), money(o.total_amount + o.total_tax));
                m.insert("net_amount".into(), money(o.total_amount));
                m.insert("tax".into(), money(o.total_tax));
                m.insert("customer_code".into(), o.customer_code);
                m.insert("prepared_by".into(), o.prepared_by);
            }
        }
        BIZ_PRODUCTION_ORDER => {
            // 生产订单没有金额字段，条件只能用数量/物料/委外标记。
            // 刻意**不**塞 amount：塞一个「数量」冒充金额会让 amount_tiered
            // 这类按金额分级的模板在生产订单上跑出「10 件 > 5000 元」的荒谬判断。
            if let Some(o) = crate::scm::prod_get(db, biz_id)? {
                m.insert("planned_qty".into(), money(o.planned_qty));
                m.insert("item_code".into(), o.item_code);
                m.insert("work_center".into(), o.work_center);
                m.insert("order_kind".into(), o.order_kind.clone());
                m.insert("supplier_code".into(), o.supplier_code.clone());
                m.insert("prepared_by".into(), o.prepared_by.clone());
            }
        }
        _ => {}
    }
    Ok(m)
}

/// 求值单条条件：`字段 操作 值`——运算符 >= <= != == > <；值带引号=字符串；
/// 未带引号优先数值比较（双侧可解析），退化为字符串 ==/!=；字段缺失或类型不符 → Err
/// （配置错误在审批时立即暴露，不让流程带病推进）。
fn eval_condition(
    cond: &str,
    ctx: &std::collections::BTreeMap<String, String>,
) -> Result<bool, String> {
    let c = cond.trim();
    let mut op = "";
    let mut idx = 0;
    for candidate in [">=", "<=", "!=", "==", ">", "<"] {
        if let Some(p) = c.find(candidate) {
            op = candidate;
            idx = p;
            break;
        }
    }
    if op.is_empty() {
        return Err("缺少比较运算符（>= <= != == > <）".to_string());
    }
    let field = c[..idx].trim();
    let raw = c[idx + op.len()..].trim();
    if field.is_empty() || raw.is_empty() {
        return Err("条件应为「字段 运算符 值」".to_string());
    }
    let lv = ctx.get(field).ok_or_else(|| {
        let keys: Vec<&str> = ctx.keys().map(|s| s.as_str()).collect();
        format!(
            "未知字段 {field}（当前业务类型支持：{}）",
            if keys.is_empty() {
                "无可用字段".to_string()
            } else {
                keys.join("/")
            }
        )
    })?;
    let quoted = (raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2)
        || (raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2);
    let as_num = |s: &str| s.trim().parse::<f64>().ok();
    let result = if quoted {
        let rv = &raw[1..raw.len() - 1];
        match op {
            "==" => lv.as_str() == rv,
            "!=" => lv.as_str() != rv,
            _ => return Err("字符串字段只支持 == 与 !=".to_string()),
        }
    } else {
        match (as_num(lv), as_num(raw)) {
            (Some(a), Some(b)) => match op {
                ">" => a > b,
                ">=" => a >= b,
                "<" => a < b,
                "<=" => a <= b,
                "==" => a == b,
                "!=" => a != b,
                _ => return Err("不支持的运算符".to_string()),
            },
            _ => match op {
                // 非数值字段退化为字符串比较
                "==" => lv.as_str() == raw,
                "!=" => lv.as_str() != raw,
                _ => return Err(format!("字段 {field} 非数值，不能用 {op} 比较")),
            },
        }
    };
    Ok(result)
}

/// 条件分支出边：有条件边按插入序逐条求值，空条件边作兜底（最后匹配）；
/// 单条无条件边=直通（兼容既有流程）；全不匹配 → Err。
fn branch_next(
    db: &Db,
    flow: &WfFlow,
    node: &WfNode,
    biz_type: &str,
    biz_id: i64,
) -> DbResult<Option<String>> {
    let edges: Vec<&WfEdge> = flow
        .edges
        .iter()
        .filter(|e| e.from_node == node.id && e.kind == "normal")
        .collect();
    if edges.is_empty() {
        return Ok(None);
    }
    if edges.len() == 1 && edges[0].condition.trim().is_empty() {
        return Ok(Some(edges[0].to_node.clone()));
    }
    let ctx = cond_context(db, biz_type, biz_id)?;
    for e in edges.iter().filter(|e| !e.condition.trim().is_empty()) {
        match eval_condition(&e.condition, &ctx) {
            Ok(true) => return Ok(Some(e.to_node.clone())),
            Ok(false) => continue,
            Err(msg) => {
                return Err(fincore::FinError::state(format!(
                    "节点【{}】条件「{}」配置错误：{}",
                    node_label(node),
                    e.condition.trim(),
                    msg
                ))
                .into())
            }
        }
    }
    if let Some(e) = edges.iter().find(|e| e.condition.trim().is_empty()) {
        return Ok(Some(e.to_node.clone()));
    }
    Err(fincore::FinError::state("该节点所有条件分支均不满足，且未配置兜底（空条件）出边").into())
}

fn reject_next(flow: &WfFlow, from: &str) -> Option<String> {
    if let Some(e) = flow
        .edges
        .iter()
        .find(|e| e.from_node == from && e.kind == "reject")
    {
        return Some(e.to_node.clone());
    }
    flow.nodes
        .iter()
        .find(|n| n.id == from)
        .map(|n| n.reject_to.clone())
        .filter(|s| !s.is_empty())
}

fn instance_of(
    conn: &rusqlite::Connection,
    biz_type: &str,
    biz_id: i64,
) -> DbResult<Option<(i64, String, String, String)>> {
    let row = conn
        .query_row(
            "SELECT id,status,current_node,log_json FROM workflow_instance
             WHERE biz_type=?1 AND biz_id=?2",
            rusqlite::params![biz_type, biz_id],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(row)
}

/// 审批拦截器（核心）：单据审批动作先过工作流。
/// - 无已发布流程 → `NoFlow`（调用方执行原有直接审批 = 默认流，向后兼容）
/// - 有流程且未到终态 → `Pending`（仅推进实例，调用方返回下一节点名，不执行业务动作）
/// - 到终态 → `Final`（调用方执行其原有业务审批/驳回）
///   参与人规则：节点参与人为空 = 需要「审核权限」（VoucherAudit）；非空 = 命中参与人角色
///   或持有审核权限（审核人/主管/管理员可兜底）。会签策略字段存档，v1 单人通过即过；
///   条件连线字段存档，v1 按 normal 边顺序取第一条（分支选择后续迭代）。
pub fn intercept(
    db: &Db,
    biz_type: &str,
    biz_id: i64,
    user: &User,
    approve: bool,
    comment: &str,
) -> DbResult<Gate> {
    let flow = match published_flow_for(db, biz_type)? {
        Some(f) => f,
        None => return Ok(Gate::NoFlow),
    };
    let first = first_approve(&flow)
        .ok_or_else(|| fincore::FinError::state("流程未配置审批节点"))?
        .clone();
    let existing = instance_of(db.conn(), biz_type, biz_id)?;
    let tx = db.write_tx()?;
    let (inst_id, cur) = match existing {
        None => {
            tx.execute(
                "INSERT INTO workflow_instance(flow_id,biz_type,biz_id,current_node,status,log_json,created_at)
                 VALUES(?1,?2,?3,?4,'running','[]',?5)",
                rusqlite::params![flow.id, biz_type, biz_id, first.id, now()],
            )?;
            (tx.last_insert_rowid(), first.id.clone())
        }
        Some((id, ref status, ref cur, _)) if status == "running" => (id, cur.clone()),
        Some((id, ref status, _, _)) if status == "rejected" => {
            // 驳回后重新发起：重置回第一个审批节点
            let n = tx.execute(
                "UPDATE workflow_instance SET current_node=?2, status='running', log_json='[]'
                 WHERE id=?1 AND status='rejected'",
                rusqlite::params![id, first.id],
            )?;
            if n == 0 {
                return Err(fincore::FinError::state("实例状态已变化，请刷新重试").into());
            }
            (id, first.id.clone())
        }
        Some((_, _, _, _)) => return Ok(Gate::Final { approved: true }), // 已批准 → 终态幂等
    };
    let node = flow
        .nodes
        .iter()
        .find(|n| n.id == cur)
        .ok_or_else(|| fincore::FinError::state("流程当前节点不存在（流程可能被改）"))?
        .clone();
    // 审批权限：审核权限（审核人/主管/管理员）恒可批；否则必须命中节点参与人角色
    let audit_ok = user.can(Perm::VoucherAudit);
    let hit = !node.participants.is_empty()
        && user
            .all_roles()
            .iter()
            .any(|r| node.participants.contains(&role_code(r)));
    if !audit_ok && !hit {
        return Err(fincore::FinError::state(format!(
            "当前节点【{}】的审批人不含您{}",
            node_label(&node),
            if node.participants.is_empty() {
                "（该节点要求审核权限）".to_string()
            } else {
                format!("（参与人：{}）", node.participants.join("、"))
            }
        ))
        .into());
    }
    // 记轨迹（读-改-写 log_json）—— 会签判定复用旧票集
    let log_s: String = tx.query_row(
        "SELECT log_json FROM workflow_instance WHERE id=?1",
        [inst_id],
        |r| r.get(0),
    )?;
    let mut log: Vec<WfLogEntry> = serde_json::from_str(&log_s).unwrap_or_default();

    // 会签（strategy=all 且参与人非空）：每个参与角色各需一票；同一人不可重复批；
    // 未满票时停留在当前节点（Pending 文案带 已通过/总角色数）。
    let mut cosign_label: Option<String> = None;
    if approve && node.strategy == "all" && !node.participants.is_empty() {
        if log
            .iter()
            .any(|e| e.node == node.id && e.action == "approve" && e.who == user.username)
        {
            return Err(fincore::FinError::state("您已在该节点会签通过，不可重复审批").into());
        }
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        for e in log
            .iter()
            .filter(|e| e.node == node.id && e.action == "approve")
        {
            if let Some(u) = crate::users::get(db, &e.who)? {
                for r in u.all_roles() {
                    covered.insert(role_code(&r));
                }
            }
        }
        for r in user.all_roles() {
            covered.insert(role_code(&r));
        }
        // 审核权限兜底**必须**同样参与计票，不能只管准入门禁。
        //
        // 不这么做的后果很具体：预置模板的节点都写了 participants=["supervisor"]
        // 且默认 strategy="all"（会签），而平台管理员的 all_roles() 是 [Admin]
        // 不含 supervisor。于是管理员能进门禁（audit_ok）却凑不满票，实例永远
        // 停在第一个节点，单据卡死——「审核人/主管/管理员可兜底」这条规则在会签
        // 节点上等于没写。
        if audit_ok {
            for p in &node.participants {
                covered.insert(p.clone());
            }
        }
        let done = node
            .participants
            .iter()
            .filter(|p| covered.contains(*p))
            .count();
        if done < node.participants.len() {
            cosign_label = Some(format!(
                "{}（会签 {}/{}）",
                node_label(&node),
                done,
                node.participants.len()
            ));
        }
    }

    log.push(WfLogEntry {
        node: node.id.clone(),
        action: if approve { "approve" } else { "reject" }.to_string(),
        who: user.username.clone(),
        at: now(),
    });
    tx.execute(
        "UPDATE workflow_instance SET log_json=?2 WHERE id=?1",
        rusqlite::params![inst_id, serde_json::to_string(&log)?],
    )?;
    let next = if !approve {
        reject_next(&flow, &node.id)
    } else if cosign_label.is_some() {
        // 会签未满票：留在当前节点
        Some(node.id.clone())
    } else {
        branch_next(db, &flow, &node, biz_type, biz_id)?
    };
    match next {
        None => {
            // 无出边 = 流程终点（approve）；驳回无路径 = 终态 rejected
            let status = if approve { "approved" } else { "rejected" };
            tx.execute(
                "UPDATE workflow_instance SET status=?2 WHERE id=?1",
                rusqlite::params![inst_id, status],
            )?;
            tx.commit()?;
            Ok(Gate::Final { approved: approve })
        }
        Some(mut nid) => {
            // 消息节点（对标金蝶：到达即发通知，不阻塞流程）——写审计（进通知中心动态）并自动继续
            let mut hops = 0;
            while let Some(nnode) = flow.nodes.iter().find(|n| n.id == nid) {
                if nnode.node_type != "message" {
                    break;
                }
                crate::log_on(
                    &tx,
                    &user.username,
                    "工作流",
                    "消息",
                    &format!(
                        "流程【{}】{}#{} 到达消息节点【{}】{}",
                        flow.name,
                        biz_type,
                        biz_id,
                        node_label(nnode),
                        if comment.trim().is_empty() {
                            String::new()
                        } else {
                            format!("（{}）", comment.trim())
                        }
                    ),
                )?;
                hops += 1;
                if hops > 10 {
                    break;
                }
                match branch_next(db, &flow, nnode, biz_type, biz_id)? {
                    Some(x) => nid = x,
                    None => {
                        // 消息节点即终点：流程完成
                        tx.execute(
                            "UPDATE workflow_instance SET status='approved' WHERE id=?1",
                            [inst_id],
                        )?;
                        tx.commit()?;
                        return Ok(Gate::Final { approved: true });
                    }
                }
            }
            let label = if nid == node.id {
                cosign_label.clone().unwrap_or_else(|| node_label(&node))
            } else {
                flow.nodes
                    .iter()
                    .find(|n| n.id == nid)
                    .map(node_label)
                    .unwrap_or_else(|| nid.clone())
            };
            let n = tx.execute(
                "UPDATE workflow_instance SET current_node=?2 WHERE id=?1 AND status='running'",
                rusqlite::params![inst_id, nid],
            )?;
            if n == 0 {
                return Err(fincore::FinError::state("实例状态已变化，请刷新重试").into());
            }
            tx.commit()?;
            Ok(Gate::Pending { next: label })
        }
    }
}

/// 当前用户待审批的运行中实例（节点参与人匹配，与 intercept 同一规则：
/// 审核权限兜底 OR 命中参与人角色）。供工作台「我的待办」只读统计。
pub fn pending_for(db: &Db, user: &User) -> DbResult<Vec<WfInstance>> {
    let mut out = Vec::new();
    for it in instances(db)? {
        if it.status != "running" {
            continue;
        }
        let Some(flow) = flow_of(db.conn(), it.flow_id)? else {
            continue;
        };
        let Some(node) = flow.nodes.iter().find(|n| n.id == it.current_node) else {
            continue;
        };
        let audit_ok = user.can(Perm::VoucherAudit);
        let hit = !node.participants.is_empty()
            && user
                .all_roles()
                .iter()
                .any(|r| node.participants.contains(&role_code(r)));
        if audit_ok || hit {
            out.push(it);
        }
    }
    Ok(out)
}

/// 单据的流程实例状态（供单据流程条 / 列表行徽标）
#[derive(Clone, Debug, serde::Serialize)]
pub struct WfStatus {
    pub found: bool,
    /// running / approved / rejected（found=false 时为空串）
    pub status: String,
    pub flow_name: String,
    pub current_label: String,
    pub log: Vec<WfLogEntry>,
}

pub fn instance_for(db: &Db, biz_type: &str, biz_id: i64) -> DbResult<WfStatus> {
    let Some((id, status, cur, log_s)) = instance_of(db.conn(), biz_type, biz_id)? else {
        return Ok(WfStatus {
            found: false,
            status: String::new(),
            flow_name: String::new(),
            current_label: String::new(),
            log: Vec::new(),
        });
    };
    let flow_id: i64 = db.conn().query_row(
        "SELECT flow_id FROM workflow_instance WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    let flow = flow_of(db.conn(), flow_id)?;
    let (flow_name, current_label) = match &flow {
        Some(f) => (
            f.name.clone(),
            f.nodes
                .iter()
                .find(|n| n.id == cur)
                .map(node_label)
                .unwrap_or(cur.clone()),
        ),
        None => (String::new(), cur.clone()),
    };
    let log: Vec<WfLogEntry> = serde_json::from_str(&log_s).unwrap_or_default();
    Ok(WfStatus {
        found: true,
        status,
        flow_name,
        current_label,
        log,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn input(id: i64, name: &str, nodes: Vec<WfNode>, edges: Vec<WfEdge>) -> WfFlowInput {
        WfFlowInput {
            id,
            name: name.to_string(),
            biz_type: BIZ_QUOTATION.to_string(),
            nodes,
            edges,
        }
    }
    fn n(id: &str, t: &str, name: &str, parts: Vec<&str>) -> WfNode {
        WfNode {
            id: id.to_string(),
            node_type: t.to_string(),
            name: name.to_string(),
            participants: parts.into_iter().map(String::from).collect(),
            strategy: "all".to_string(),
            reject_to: String::new(),
            x: 0.0,
            y: 0.0,
        }
    }
    fn e(id: &str, from: &str, to: &str, kind: &str) -> WfEdge {
        WfEdge {
            id: id.to_string(),
            from_node: from.to_string(),
            to_node: to.to_string(),
            kind: kind.to_string(),
            condition: String::new(),
        }
    }

    #[test]
    fn flow_save_validation_and_publish() {
        let db = mem();
        // 无 start → 拒
        assert!(flow_save(
            &db,
            &input(0, "f", vec![n("a1", "approve", "审批", vec![])], vec![]),
            "u"
        )
        .is_err());
        // start 不唯一 → 拒
        assert!(flow_save(
            &db,
            &input(
                0,
                "f",
                vec![
                    n("s1", "start", "开始", vec![]),
                    n("s2", "start", "开始", vec![])
                ],
                vec![]
            ),
            "u",
        )
        .is_err());
        // 连线端点不存在 → 拒
        assert!(flow_save(
            &db,
            &input(
                0,
                "f",
                vec![n("s1", "start", "开始", vec![])],
                vec![e("e1", "s1", "ghost", "normal")]
            ),
            "u",
        )
        .is_err());
        // 正常保存 → 发布需要审批节点
        let ok = input(
            0,
            "报价审批流",
            vec![
                n("s1", "start", "开始", vec![]),
                n("a1", "approve", "主管审批", vec![]),
            ],
            vec![e("e1", "s1", "a1", "normal")],
        );
        let id = flow_save(&db, &ok, "u").unwrap();
        assert!(flow_set_status(&db, id, true, "u").is_ok());
        // 撤回 + 删除
        flow_set_status(&db, id, false, "u").unwrap();
        flow_delete(&db, id).unwrap();
        assert!(flow_list(&db).unwrap().is_empty());

        // 无审批节点发布 → 拒
        let no_approve = input(0, "g", vec![n("s1", "start", "开始", vec![])], vec![]);
        let id2 = flow_save(&db, &no_approve, "u").unwrap();
        assert!(
            flow_set_status(&db, id2, true, "u").is_err(),
            "无审批节点不能发布"
        );
    }

    #[test]
    fn intercept_lifecycle() {
        let db = mem();
        let nodes = vec![
            n("s1", "start", "开始", vec![]),
            n("a1", "approve", "初审", vec![]),
            n("a2", "approve", "复核", vec![]),
        ];
        let edges = vec![e("e1", "s1", "a1", "normal"), e("e2", "a1", "a2", "normal")];
        let id = flow_save(&db, &input(0, "两节点流", nodes, edges), "u").unwrap();
        flow_set_status(&db, id, true, "u").unwrap();

        // 无审核权限且参与人不含他 → 拒
        let viewer = User::new("v", "只读", Role::Viewer);
        assert!(
            intercept(&db, BIZ_QUOTATION, 1, &viewer, true, "").is_err(),
            "无审批权应被拒"
        );
        // 主管（含审核权）推进
        let sup = User::new("s", "主管", Role::Supervisor);
        match intercept(&db, BIZ_QUOTATION, 1, &sup, true, "").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "复核"),
            other => panic!("第一次应推进到下一节点：{other:?}"),
        }
        match intercept(&db, BIZ_QUOTATION, 1, &sup, true, "同意").unwrap() {
            Gate::Final { approved } => assert!(approved, "第二节点为终点 → Final(approved)"),
            other => panic!("第二次应到终态：{other:?}"),
        }
        // 终态幂等
        assert!(matches!(
            intercept(&db, BIZ_QUOTATION, 1, &sup, true, "").unwrap(),
            Gate::Final { approved: true }
        ));

        // 无已发布流程的类型 → NoFlow（默认流）
        assert!(matches!(
            intercept(&db, BIZ_CLAIM, 9, &sup, true, "").unwrap(),
            Gate::NoFlow
        ));
    }

    // ---- 预置模板 ----

    /// 每个预置模板都必须能被 flow_save 的校验通过（start 唯一、id 不重复、
    /// 连线端点存在、驳回目标存在）——否则用户点「从模板创建」当场失败。
    #[test]
    fn every_template_passes_flow_save_validation() {
        let db = mem();
        for tpl in templates() {
            for biz in &tpl.biz_types {
                let f = apply_template(&db, &tpl.key, biz, "u")
                    .unwrap_or_else(|e| panic!("模板【{}】@{} 应用失败：{e}", tpl.name, biz));
                assert_eq!(f.status, "draft", "模板应用后必须是草稿，不能自动发布");
                assert_eq!(f.biz_type, *biz);
                assert_eq!(
                    f.nodes.iter().filter(|n| n.node_type == "start").count(),
                    1,
                    "模板【{}】必须恰好一个开始节点",
                    tpl.name
                );
                assert!(
                    f.nodes.iter().any(|n| n.node_type == "approve"),
                    "模板【{}】没有审批节点，发布会被拒",
                    tpl.name
                );
                // 节点 id 唯一（flow_save 已校验，这里再确认模板本身没自相矛盾）
                let mut ids: Vec<&str> = f.nodes.iter().map(|n| n.id.as_str()).collect();
                ids.sort_unstable();
                let uniq = ids.len();
                ids.dedup();
                assert_eq!(ids.len(), uniq, "模板【{}】节点 id 重复", tpl.name);
                // 连线 id 唯一（否则同一条边会被写两遍）
                let mut eids: Vec<&str> = f.edges.iter().map(|e| e.id.as_str()).collect();
                eids.sort_unstable();
                let eu = eids.len();
                eids.dedup();
                assert_eq!(eids.len(), eu, "模板【{}】连线 id 重复", tpl.name);
            }
        }
    }

    /// `cond_fields_of` 与 `cond_context` 必须严格一致。
    ///
    /// 两个函数是两处独立实现的关系，天然会漂移：
    ///   · `cond_fields_of` 多列一个字段 → 用户配的条件边静默走兜底分支
    ///     （看起来流程「通了」，实际条件从没生效）
    ///   · `cond_fields_of` 少列一个字段 → 模板明明适用却被 apply_template 拒
    /// 这个用例用真单据跑一遍 `cond_context`，逐个比对声明的字段集。
    #[test]
    fn cond_fields_match_context() {
        use crate::business::{Claim, ClaimItem, ClaimStatus};
        use crate::procurement::PurchaseReq;
        use crate::sales::Quotation;
        use crate::scm::{PoLine, SoLine};

        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        // 报价单：qty / amount / customer_code / item_code
        let mut q = Quotation {
            id: 0,
            no: "QU-1".into(),
            period: p,
            date: d,
            customer_code: "C01".into(),
            customer_name: "客户甲".into(),
            item_code: "I01".into(),
            item_name: "料".into(),
            qty: fincore::Money::parse("10").unwrap(),
            unit_price: fincore::Money::parse("100").unwrap(),
            status: "approved".into(),
            prepared_by: "u".into(),
            memo: String::new(),
        };
        let qid = crate::sales::quo_save(&db, &mut q).unwrap();

        // 报销单：amount / applicant / dept
        let money = fincore::Money::parse("880").unwrap();
        let cid = crate::business::claim_insert(
            &db,
            &Claim {
                id: 0,
                period: p,
                no: "CL-1".into(),
                biz_date: d,
                applicant: "E01".into(),
                dept: "D01".into(),
                reason: "差旅".into(),
                amount: money,
                status: ClaimStatus::Submitted,
                items: vec![ClaimItem {
                    expense_account: "660203".into(),
                    amount: money,
                    memo: "机票".into(),
                }],
                approver: String::new(),
                approved_at: None,
                payer: String::new(),
                paid_at: None,
                voucher_id: None,
                created_at: String::new(),
            },
        )
        .unwrap();

        // 请购单：qty / item_code / requester
        let mut pr = PurchaseReq {
            id: 0,
            no: "PR-1".into(),
            period: p,
            date: d,
            item_code: "I01".into(),
            item_name: "料".into(),
            qty: fincore::Money::parse("5").unwrap(),
            status: "approved".into(),
            requester: "E01".into(),
            memo: String::new(),
        };
        let prid = crate::procurement::pr_save(&db, &mut pr).unwrap();

        // 采购订单：amount(含税) / net_amount / tax / supplier_code / prepared_by
        let mut po = crate::scm::PurchaseOrder::new(p, d, "S01", "供应商甲", "u");
        po.status = crate::scm::PoStatus::Confirmed;
        po.lines = vec![PoLine {
            id: 0,
            po_id: 0,
            item_code: "I01".into(),
            item_name: "料".into(),
            qty_ordered: fincore::Money::parse("10").unwrap(),
            qty_received: fincore::Money::ZERO,
            unit_price: fincore::Money::parse("100").unwrap(),
            tax_rate: fincore::Money::parse("0.13").unwrap(),
            amount: fincore::Money::parse("1000").unwrap(),
            tax_amount: fincore::Money::parse("130").unwrap(),
            memo: String::new(),
        }];
        let poid = crate::scm::po_save(&db, &mut po).unwrap();

        // 销售订单：amount / net_amount / tax / customer_code / prepared_by
        let mut so = crate::scm::SalesOrder::new(p, d, "C01", "客户甲", "u");
        so.status = crate::scm::SoStatus::Confirmed;
        so.lines = vec![SoLine {
            id: 0,
            so_id: 0,
            item_code: "I01".into(),
            item_name: "料".into(),
            qty_ordered: fincore::Money::parse("20").unwrap(),
            qty_shipped: fincore::Money::ZERO,
            unit_price: fincore::Money::parse("50").unwrap(),
            tax_rate: fincore::Money::parse("0.13").unwrap(),
            amount: fincore::Money::parse("1000").unwrap(),
            tax_amount: fincore::Money::parse("130").unwrap(),
            memo: String::new(),
        }];
        let soid = crate::scm::so_save(&db, &mut so).unwrap();

        // 生产订单：planned_qty 等，**没有** amount
        let mut mo = crate::scm::ProductionOrder {
            id: 0,
            no: "MO-1".into(),
            period: p,
            date: d,
            item_code: "F01".into(),
            item_name: "成品".into(),
            planned_qty: fincore::Money::parse("300").unwrap(),
            completed_qty: fincore::Money::ZERO,
            status: crate::scm::ProdStatus::Draft,
            work_center: "W01".into(),
            so_id: 0,
            prepared_by: "u".into(),
            memo: String::new(),
            order_kind: "inhouse".into(),
            supplier_code: String::new(),
            supplier_name: String::new(),
            plan_start: String::new(),
            plan_end: String::new(),
        };
        let moid = crate::scm::prod_save(&db, &mut mo).unwrap();

        // 收付款单：amount / kind / party
        let rid = crate::receipt::receipt_create(
            &db,
            "payment",
            d,
            "100201",
            "S01",
            "",
            "",
            fincore::Money::ZERO,
            fincore::Money::parse("500").unwrap(),
            "付供应商款",
            "u",
        )
        .unwrap();

        let cases: Vec<(&str, i64, Vec<&str>)> = vec![
            (
                BIZ_QUOTATION,
                qid,
                vec!["qty", "amount", "customer_code", "item_code"],
            ),
            (BIZ_CLAIM, cid, vec!["amount", "applicant", "dept"]),
            (
                BIZ_PURCHASE_REQ,
                prid,
                vec!["qty", "item_code", "requester"],
            ),
            (
                BIZ_PURCHASE_ORDER,
                poid,
                vec![
                    "amount",
                    "net_amount",
                    "tax",
                    "supplier_code",
                    "prepared_by",
                ],
            ),
            (
                BIZ_SALES_ORDER,
                soid,
                vec![
                    "amount",
                    "net_amount",
                    "tax",
                    "customer_code",
                    "prepared_by",
                ],
            ),
            (
                BIZ_PRODUCTION_ORDER,
                moid,
                vec![
                    "planned_qty",
                    "item_code",
                    "work_center",
                    "order_kind",
                    "supplier_code",
                    "prepared_by",
                ],
            ),
            (BIZ_RECEIPT, rid, vec!["amount", "kind", "party"]),
        ];

        for (biz, id, declared) in cases {
            let ctx = cond_context(&db, biz, id)
                .unwrap_or_else(|e| panic!("cond_context({biz}) 失败：{e}"));
            let mut actual: Vec<String> = ctx.keys().cloned().collect();
            actual.sort();
            let mut want: Vec<String> = declared.iter().map(|s| s.to_string()).collect();
            want.sort();
            assert_eq!(
                actual, want,
                "【{biz}】cond_context 实际提供的字段与 cond_fields_of 声明的不一致"
            );
        }
    }

    /// 持有审核权限的人在会签节点上**也能凑满票**（兜底规则要贯穿到计票）。
    ///
    /// 回归背景：预置模板的节点都写 `participants=["supervisor"]` + 默认
    /// `strategy="all"`（会签），而平台管理员的 `all_roles()` 是 `[Admin]`，
    /// 不含 supervisor。计票只看角色时，管理员能过门禁却永远凑不满票，实例
    /// 死死停在第一个节点——「配了流程、单据却过不去」里最难自查的一种。
    #[test]
    fn audit_permission_counts_toward_cosign_quorum() {
        let db = mem();
        let f = apply_template(&db, "order_standard", BIZ_PURCHASE_ORDER, "u").unwrap();
        flow_set_status(&db, f.id, true, "u").unwrap();
        let lead = f
            .nodes
            .iter()
            .find(|n| n.node_type == "approve")
            .expect("应有审批节点");
        assert!(
            !lead.participants.is_empty() && lead.strategy == "all",
            "本用例的前提：节点既指定了参与人角色、又是会签（否则测不到兜底）"
        );

        let admin = User::new("boss", "管理员", Role::Admin);
        assert!(admin.can(Perm::VoucherAudit), "管理员应持有审核权限");
        assert!(
            !admin
                .all_roles()
                .iter()
                .any(|r| role_code(r) == "supervisor"),
            "管理员的角色里本不该有 supervisor —— 正是这一点让本用例有意义"
        );

        // 一票就该过（走到第二个节点），而不是停在原地等一个永远不会来的主管票
        match intercept(&db, BIZ_PURCHASE_ORDER, 1, &admin, true, "").unwrap() {
            Gate::Pending { next } => assert!(
                !next.contains("业务主管"),
                "审核权限兜底应直接满足该节点票数，不该停在原节点：{next}"
            ),
            other => panic!("首个节点不应终态：{other:?}"),
        }
    }

    /// 生产订单**没有**金额字段：条件字段校验必须认出这一点。
    ///
    /// 挡的是「画布上少一个字段、月底发现一批单卡死」——条件求值缺字段返回 Err，
    /// 而实例**已经建好**，单据卡在第一个审批节点既过不去也退不出，只能手工改库。
    /// 所以要在**保存流程**那一刻就拒掉。
    #[test]
    fn condition_fields_validated_against_biz_type() {
        let db = mem();

        // 1) 字段表本身：生产订单有 planned_qty、没有 amount
        let have = cond_fields_of(BIZ_PRODUCTION_ORDER);
        assert!(have.contains(&"planned_qty"));
        assert!(
            !have.contains(&"amount"),
            "生产订单不该有 amount —— 塞一个数量冒充金额会让「金额分级」跑出「10 件 > 5000 元」"
        );
        assert_eq!(
            missing_cond_fields(BIZ_PRODUCTION_ORDER, &["amount".to_string()]).unwrap_or_default(),
            vec!["amount".to_string()]
        );
        assert!(missing_cond_fields(BIZ_PRODUCTION_ORDER, &["planned_qty".into()]).is_none());

        // 2) 手绘流程：在生产订单上写 `amount > 5000` 必须在保存时被拒
        let bad = WfFlowInput {
            id: 0,
            name: "手绘·生产金额分级".into(),
            biz_type: BIZ_PRODUCTION_ORDER.into(),
            nodes: vec![
                WfNode {
                    id: "start".into(),
                    node_type: "start".into(),
                    name: "制单".into(),
                    participants: vec![],
                    strategy: default_strategy(),
                    reject_to: String::new(),
                    x: 0.0,
                    y: 0.0,
                },
                WfNode {
                    id: "a".into(),
                    node_type: "approve".into(),
                    name: "主管".into(),
                    participants: vec![],
                    strategy: default_strategy(),
                    reject_to: String::new(),
                    x: 0.0,
                    y: 0.0,
                },
                WfNode {
                    id: "b".into(),
                    node_type: "approve".into(),
                    name: "财务".into(),
                    participants: vec![],
                    strategy: default_strategy(),
                    reject_to: String::new(),
                    x: 0.0,
                    y: 0.0,
                },
            ],
            edges: vec![tedge("start", "a"), tedge_if("a", "b", "amount > 5000")],
        };
        let e = flow_save(&db, &bad, "u").unwrap_err().to_string();
        assert!(e.contains("amount"), "应点名缺失字段：{e}");
        assert!(e.contains("生产订单"), "应说清是哪张单据：{e}");
        assert!(
            e.contains("planned_qty"),
            "应列出该单据实际可用的字段，别让用户自己猜：{e}"
        );

        // 3) 同一张单据上用 planned_qty 就该放行
        let mut ok = bad;
        ok.name = "手绘·生产数量分级".into();
        ok.edges[1].condition = "planned_qty > 1000".into();
        assert!(
            flow_save(&db, &ok, "u").is_ok(),
            "用该单据真有的字段就应该能保存"
        );

        // 4) 报销单上写 planned_qty 同样要拒（反向也不许错）
        let mut wrong = ok.clone();
        wrong.biz_type = BIZ_CLAIM.into();
        wrong.edges[1].condition = "planned_qty > 1000".into();
        let e = flow_save(&db, &wrong, "u").unwrap_err().to_string();
        assert!(e.contains("planned_qty"), "反向也必须拒：{e}");
    }

    /// 订单分级模板要真的按**价税合计**分流，而不是只看未税金额。
    ///
    /// 这里特意让税额占大头（未税 1000、13% 税 → 合计 1130），阈值卡在 1100：
    /// 只看未税会判「小额走快速通道」，看价税合计才走加签。差一个 13% 就分错档
    /// 的审批链，在实务里就是「该上财务主管的单没过财务主管」。
    #[test]
    fn order_tiered_branches_on_amount_including_tax() {
        use crate::scm::{PoLine, PoStatus};
        let db = mem();
        let f = apply_template(&db, "order_tiered", BIZ_PURCHASE_ORDER, "u").unwrap();
        flow_set_status(&db, f.id, true, "u").unwrap();
        let sup = User::new("s", "主管", Role::Supervisor);
        let p = fincore::Period::new(2026, 1).unwrap();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        // 传未税与税额两个数：po_save 直接把 line.amount / line.tax_amount 分别
        // 汇总成 total_amount / total_tax，所以「价税合计」= 两者之和。
        let mk = |no: &str, net: &str, tax: &str| -> i64 {
            let mut po = crate::scm::PurchaseOrder::new(p, d, "S01", "供应商甲", "u");
            // no 有 UNIQUE 约束：不取号的话第二张就撞 "UNIQUE constraint failed"
            po.no = crate::scm::po_next_no(&db, p).unwrap();
            po.status = PoStatus::Confirmed;
            po.lines = vec![PoLine {
                id: 0,
                po_id: 0,
                item_code: "I01".into(),
                item_name: "料".into(),
                qty_ordered: fincore::Money::parse("1").unwrap(),
                qty_received: fincore::Money::ZERO,
                unit_price: fincore::Money::parse(net).unwrap(),
                tax_rate: fincore::Money::parse("0.13").unwrap(),
                amount: fincore::Money::parse(net).unwrap(),
                tax_amount: fincore::Money::parse(tax).unwrap(),
                memo: String::new(),
            }];
            crate::scm::po_save(&db, &mut po).unwrap_or_else(|e| panic!("{no} 保存失败：{e}"))
        };
        // 未税 100000 + 税 13000 = 113000 > 100000 阈值 → 上总经理
        let big = mk("PO-1", "100000", "13000");
        let small = mk("PO-2", "100", "13"); // 合计 113 → 业务主管批完

        // 大额：业务主管 → 财务主管 → 总经理
        match intercept(&db, BIZ_PURCHASE_ORDER, big, &sup, true, "").unwrap() {
            Gate::Pending { next } => assert!(next.contains("财务"), "大额应加签财务主管：{next}"),
            other => panic!("大额单第一审不应终态：{other:?}"),
        }
        match intercept(&db, BIZ_PURCHASE_ORDER, big, &sup, true, "").unwrap() {
            Gate::Pending { next } => assert!(next.contains("总经理"), "大额应再上总经理：{next}"),
            other => panic!("大额单第二审不应终态：{other:?}"),
        }
        assert!(matches!(
            intercept(&db, BIZ_PURCHASE_ORDER, big, &sup, true, "").unwrap(),
            Gate::Final { approved: true }
        ));

        // 小额：业务主管批完即终态
        assert!(
            matches!(
                intercept(&db, BIZ_PURCHASE_ORDER, small, &sup, true, "").unwrap(),
                Gate::Final { approved: true }
            ),
            "小额采购订单应在业务主管这一节点结束"
        );
    }

    /// 生产订单两段审：大批量要加厂长，小批量不加。
    #[test]
    fn prod_two_stage_branches_on_planned_qty() {
        use crate::scm::ProdStatus;
        let db = mem();
        let f = apply_template(&db, "prod_two_stage", BIZ_PRODUCTION_ORDER, "u").unwrap();
        flow_set_status(&db, f.id, true, "u").unwrap();
        let sup = User::new("s", "主管", Role::Supervisor);
        let p = fincore::Period::new(2026, 1).unwrap();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        let mk = |no: &str, qty: &str| -> i64 {
            let mut mo = crate::scm::ProductionOrder {
                id: 0,
                no: no.into(),
                period: p,
                date: d,
                item_code: "F01".into(),
                item_name: "成品".into(),
                planned_qty: fincore::Money::parse(qty).unwrap(),
                completed_qty: fincore::Money::ZERO,
                status: ProdStatus::Draft,
                work_center: "W01".into(),
                so_id: 0,
                prepared_by: "u".into(),
                memo: String::new(),
                order_kind: "inhouse".into(),
                supplier_code: String::new(),
                supplier_name: String::new(),
                plan_start: String::new(),
                plan_end: String::new(),
            };
            crate::scm::prod_save(&db, &mut mo).unwrap_or_else(|e| panic!("{no} 保存失败：{e}"))
        };
        let big = mk("MO-BIG", "5000"); // > 1000 → 加厂长
        let small = mk("MO-SMALL", "10");

        match intercept(&db, BIZ_PRODUCTION_ORDER, big, &sup, true, "").unwrap() {
            Gate::Pending { next } => assert!(next.contains("厂长"), "大批量应加厂长：{next}"),
            other => panic!("首批不应终态：{other:?}"),
        }
        match intercept(&db, BIZ_PRODUCTION_ORDER, big, &sup, true, "").unwrap() {
            Gate::Pending { next } => {
                assert!(next.contains("生产负责人"), "接着是生产负责人：{next}")
            }
            other => panic!("次批不应终态：{other:?}"),
        }
        assert!(matches!(
            intercept(&db, BIZ_PRODUCTION_ORDER, big, &sup, true, "").unwrap(),
            Gate::Final { approved: true }
        ));

        match intercept(&db, BIZ_PRODUCTION_ORDER, small, &sup, true, "").unwrap() {
            Gate::Pending { next } => {
                assert!(next.contains("生产负责人"), "小批量不加厂长：{next}");
                assert!(!next.contains("厂长"), "小批量不该走厂长节点：{next}");
            }
            other => panic!("小批量首批不应终态：{other:?}"),
        }
    }

    /// 模板不能套到不适用的单据上——否则金额分级模板跑到报价单上，
    /// 审批时会因缺 `amount` 字段直接报「条件配置错误」，把单据卡死。
    #[test]
    fn template_rejects_inapplicable_biz_type() {
        let db = mem();
        // simple 只适用报价/请购，套到报销单应被拒且说清可选范围
        let e = apply_template(&db, "simple", BIZ_CLAIM, "u").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("不适用"), "错误信息应说明不适用：{msg}");
        assert!(
            msg.contains("报销单"),
            "错误信息应列出可用的单据类型：{msg}"
        );
        // 非法 biz_type 也拒
        assert!(apply_template(&db, "simple", "not_a_biz", "u").is_err());
        // 不存在的模板 key 也拒
        assert!(apply_template(&db, "no_such_tpl", BIZ_CLAIM, "u").is_err());
    }

    /// 金额分级模板必须真的按金额分流：小额主管批完即终态，大额要走到财务主管。
    ///
    /// 这是模板里最需要实证的一段——「条件边 + 该节点无后续出边 = 终点」这个
    /// 约定如果不成立，小额单据会永远卡在主管节点。
    #[test]
    fn amount_tiered_template_branches_by_amount() {
        use crate::business::{Claim, ClaimItem, ClaimStatus};
        let db = mem();
        let f = apply_template(&db, "amount_tiered", BIZ_CLAIM, "u").unwrap();
        flow_set_status(&db, f.id, true, "u").unwrap();
        let sup = User::new("s", "主管", Role::Supervisor);
        let p = fincore::Period::new(2026, 1).unwrap();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        // 造两张金额跨档的报销单，驱动真实条件求值（不是构造假条件）
        let mk = |no: &str, amt: &str| -> i64 {
            let money = fincore::Money::parse(amt).unwrap();
            crate::business::claim_insert(
                &db,
                &Claim {
                    id: 0,
                    period: p,
                    no: no.into(),
                    biz_date: d,
                    applicant: "E01".into(),
                    dept: "D01".into(),
                    reason: "差旅费".into(),
                    amount: money,
                    status: ClaimStatus::Submitted,
                    items: vec![ClaimItem {
                        expense_account: "660203".into(),
                        amount: money,
                        memo: "机票".into(),
                    }],
                    approver: String::new(),
                    approved_at: None,
                    payer: String::new(),
                    paid_at: None,
                    voucher_id: None,
                    created_at: "2026-01-15 09:00:00".into(),
                },
            )
            .unwrap()
        };
        let small = mk("BX202601-001", "3000");
        let mid = mk("BX202601-002", "8000");
        let big = mk("BX202601-003", "60000");

        // 3000 ≤ 5000：主管一批即终态（兜底走到无出边消息节点 = 归档完成）
        match intercept(&db, BIZ_CLAIM, small, &sup, true, "同意").unwrap() {
            Gate::Final { approved } => assert!(approved, "3000 元应主管批完即终态"),
            other => panic!("3000 元不应还有下一节点：{other:?}"),
        }
        // 8000 落在 (5000, 50000]：主管 → 财务主管
        match intercept(&db, BIZ_CLAIM, mid, &sup, true, "同意").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "财务主管审批", "8000 元应加财务主管"),
            other => panic!("8000 元应推进到财务主管：{other:?}"),
        }
        // 再批一次到总经理（fin 的出边无条件）
        match intercept(&db, BIZ_CLAIM, mid, &sup, true, "同意").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "总经理审批"),
            other => panic!("8000 元第二次应到总经理：{other:?}"),
        }
        // 60000 > 50000：主管一批直接跳总经理（先判 >50000 的边）
        match intercept(&db, BIZ_CLAIM, big, &sup, true, "同意").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "总经理审批", "60000 元应跳到总经理"),
            other => panic!("60000 元应跳到总经理：{other:?}"),
        }
    }

    /// 出纳节点不能被主管顶替：Cashier 角色没 VoucherAudit，必须命中参与人才行。
    ///
    /// 顺带锁住一条**已知边界**（不是缺陷，是引擎现状）：持 VoucherAudit 的
    /// 主管/审核人/管理员可批任意节点，所以工作流的「出纳复核」只是留痕，
    /// 真正拦住「会计自签现金凭证」的是账套参数 `require_cashier`。
    #[test]
    fn funds_template_cashier_node_gates_non_audit_roles() {
        let db = mem();
        let f = apply_template(&db, "funds", BIZ_RECEIPT, "u").unwrap();
        flow_set_status(&db, f.id, true, "u").unwrap();
        let sup = User::new("s", "主管", Role::Supervisor);
        let cashier = User::new("c", "出纳", Role::Cashier);

        // 主管批掉财务主管节点 → 推进到出纳复核（Pending，未终态）
        match intercept(&db, BIZ_RECEIPT, 1, &sup, true, "同意").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "出纳复核"),
            other => panic!("应推进到出纳复核：{other:?}"),
        }
        // 无 CashierSign 的角色（如会计）到出纳节点应被拒
        let acc = User::new("a", "会计", Role::Accountant);
        assert!(
            intercept(&db, BIZ_RECEIPT, 1, &acc, true, "同意").is_err(),
            "会计不该能过出纳复核节点"
        );
        // 出纳本人可以（先入库，会签票按 who 查套内角色）
        crate::users::insert(&db, &cashier).unwrap();
        // 消息节点不阻塞：出纳批完直接到终态
        match intercept(&db, BIZ_RECEIPT, 1, &cashier, true, "已付").unwrap() {
            Gate::Final { approved } => assert!(approved, "出纳批完应到终态"),
            other => panic!("出纳批完应到终态：{other:?}"),
        }
    }

    /// 「小微」模板必须真的能被**只有会计 + 出纳**的公司走完 ——
    /// 不是「模板存在」，而是「每个节点都有人能批、不卡死」。
    ///
    /// 这条是加 `micro_*` 模板的起因：预置模板原本清一色 `supervisor`，
    /// 而小企业没有财务主管。会计角色**没有** `VoucherAudit`（见
    /// `Role::Accountant` 的注释），所以会计批不了 `supervisor` 节点 ——
    /// 实际只能由平台管理员代批，审批链上挂个管理员却写着「财务主管审批」。
    #[test]
    fn micro_templates_are_walkable_by_accountant_and_cashier_only() {
        let db = mem();
        // 这家公司只有两个岗位：会计 + 出纳。**刻意不给 supervisor。**
        let acc = User::new("acc", "会计", Role::Accountant);
        let cash = User::new("cash", "出纳", Role::Cashier);
        crate::users::insert(&db, &acc).unwrap();
        crate::users::insert(&db, &cash).unwrap();

        // ① 小微单级：会计制单 → 会计审批 → 终态
        let f = apply_template(&db, "micro_single", BIZ_PURCHASE_ORDER, "acc").unwrap();
        flow_set_status(&db, f.id, true, "acc").unwrap();
        match intercept(&db, BIZ_PURCHASE_ORDER, 1, &acc, true, "同意").unwrap() {
            Gate::Final { approved } => assert!(approved, "会计应能自己批完小微单级"),
            other => panic!("小微单级应到终态：{other:?}"),
        }

        // ② 小微两岗：会计制单 → 会计复核 → 出纳付款 → 终态
        let f2 = apply_template(&db, "micro_two_role", BIZ_RECEIPT, "acc").unwrap();
        flow_set_status(&db, f2.id, true, "acc").unwrap();
        match intercept(&db, BIZ_RECEIPT, 1, &acc, true, "同意").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "出纳付款", "应推进到出纳节点"),
            other => panic!("应推进到出纳付款：{other:?}"),
        }
        // 出纳没到出纳节点前不该能批
        assert!(
            intercept(&db, BIZ_RECEIPT, 1, &cash, true, "已付").is_ok(),
            "出纳在非当前节点时不该报错卡死（应只是没票）"
        );
        match intercept(&db, BIZ_RECEIPT, 1, &cash, true, "已付").unwrap() {
            Gate::Final { approved } => assert!(approved, "出纳批完应到终态"),
            other => panic!("出纳批完应到终态：{other:?}"),
        }
    }

    /// 预置模板的节点参与人只能是**系统里真实存在的角色编码**。
    ///
    /// 写错一个字母（如 `supervisr`）不会编译失败、也不会在落流程时报错 ——
    /// 只会让那个节点永远没人能批，单据卡在系统里。要靠测试兜住。
    #[test]
    fn preset_template_participants_are_all_real_roles() {
        let valid: std::collections::HashSet<String> = [
            Role::Admin,
            Role::Supervisor,
            Role::Accountant,
            Role::Cashier,
            Role::Auditor,
            Role::OrderClerk,
            Role::Keeper,
            Role::Receivables,
            Role::Payables,
            Role::CostAccountant,
            Role::Production,
            Role::Viewer,
        ]
        .iter()
        .map(role_code)
        .collect();
        for t in templates() {
            for node in &t.nodes {
                for p in &node.participants {
                    assert!(
                        valid.contains(p),
                        "模板【{}】节点【{}】的参与人 {:?} 不是任何真实角色编码",
                        t.name,
                        node.name,
                        p
                    );
                }
            }
        }
    }

    /// 依赖 `supervisor` 的模板必须在描述里说明「需要这个岗位」。
    ///
    /// 理由：照搬一个自己不存在的岗位，等于替企业做了一个它并不存在的内控设计，
    /// 而且审批人实际会变成平台管理员（会签兜底），追溯时极具误导性。
    /// 模板名和描述是用户选择模板时唯一能看到的信息，警告必须在那里。
    #[test]
    fn templates_requiring_a_supervisor_say_so_in_their_description() {
        for t in templates() {
            let needs_supervisor = t
                .nodes
                .iter()
                .any(|n| n.participants.iter().any(|p| p == "supervisor"));
            if !needs_supervisor {
                continue;
            }
            assert!(
                t.desc.contains("财务主管") || t.desc.contains("岗位"),
                "模板【{}】用了 supervisor 岗位却没在描述里说明，\
                 用户会照搬一个公司里并不存在的岗位：{}",
                t.name,
                t.desc
            );
        }
    }

    /// 会签（strategy=all + 多参与角色）：每个角色各需一票；同人不可重复批；满票推进。
    #[test]
    fn cosign_all_needs_each_role() {
        let db = mem();
        let nodes = vec![
            n("s1", "start", "开始", vec![]),
            n("a1", "approve", "会签节点", vec!["order_clerk", "keeper"]),
        ];
        let edges = vec![e("e1", "s1", "a1", "normal")];
        let id = flow_save(&db, &input(0, "会签流", nodes, edges), "u").unwrap();
        flow_set_status(&db, id, true, "u").unwrap();

        let oc = User::new("s1", "订单员", Role::OrderClerk);
        let kp = User::new("a2", "仓管员", Role::Keeper);
        // 会签票按 who 查套内角色——审批人必是套内成员（现实场景），测试同步入库
        crate::users::insert(&db, &oc).unwrap();
        crate::users::insert(&db, &kp).unwrap();
        // 订单员首票 → 停留当前节点，文案带 会签 1/2
        match intercept(&db, BIZ_QUOTATION, 1, &oc, true, "").unwrap() {
            Gate::Pending { next } => assert!(next.contains("会签 1/2"), "会签文案：{next}"),
            other => panic!("应停留会签：{other:?}"),
        }
        // 同人重复批 → 拒
        assert!(
            intercept(&db, BIZ_QUOTATION, 1, &oc, true, "").is_err(),
            "重复审批应拒"
        );
        // 仓管员第二票 → 满票 → 终态（无出边 Final）
        match intercept(&db, BIZ_QUOTATION, 1, &kp, true, "").unwrap() {
            Gate::Final { approved } => assert!(approved, "满票应到终态"),
            other => panic!("满票应 Final：{other:?}"),
        }
    }

    /// 条件分支：有条件出线按序求值、空条件兜底；语法/未知字段报配置错误。
    #[test]
    fn condition_branch_routing() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mk = |qty: &str, price: &str| -> i64 {
            let mut q = crate::sales::Quotation {
                id: 0,
                no: String::new(),
                period: p,
                date: chrono::NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
                customer_code: "C01".into(),
                customer_name: "客户".into(),
                item_code: "140301".into(),
                item_name: "原料".into(),
                qty: Money::parse(qty).unwrap(),
                unit_price: Money::parse(price).unwrap(),
                status: "draft".into(),
                prepared_by: "u".into(),
                memo: String::new(),
            };
            q.no = crate::sales::quo_next_no(&db, p).unwrap();
            crate::sales::quo_save(&db, &mut q).unwrap()
        };
        let big = mk("10", "600"); // amount 6000
        let small = mk("10", "10"); // amount 100
        let bad_cond = mk("10", "900");
        let unknown_f = mk("10", "800");

        let cond_flow = |name: &str, second_cond: &str| -> i64 {
            let nodes = vec![
                n("s1", "start", "开始", vec![]),
                n("a1", "approve", "审批", vec![]),
                n("a2", "approve", "高额复核", vec![]),
                n("a3", "approve", "快速通过", vec![]),
            ];
            let mut edges = vec![e("e1", "s1", "a1", "normal")];
            if !second_cond.is_empty() {
                let mut c = e("e2", "a1", "a2", "normal");
                c.condition = second_cond.to_string();
                edges.push(c);
            }
            edges.push(e("e3", "a1", "a3", "normal"));
            let id = flow_save(&db, &input(0, name, nodes, edges), "u").unwrap();
            flow_set_status(&db, id, true, "u").unwrap();
            id
        };

        // 主流程：amount>5000 走高额复核，否则兜底快速通过
        cond_flow("条件流", "amount > 5000");
        let u = User::new("b1", "管理员", Role::Admin);
        match intercept(&db, BIZ_QUOTATION, big, &u, true, "").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "高额复核"),
            other => panic!("高额路由：{other:?}"),
        }
        match intercept(&db, BIZ_QUOTATION, small, &u, true, "").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "快速通过"),
            other => panic!("兜底路由：{other:?}"),
        }
        // 语法错误 → 保存时字段名合法（`amount` 确实存在），但运算符非法：
        // 这类**只能**在审批时才发现，所以审批时报配置错仍是对的行为。
        cond_flow("坏条件流", "amount >>> 5");
        assert!(
            intercept(&db, BIZ_QUOTATION, bad_cond, &u, true, "").is_err(),
            "语法错误应报配置错"
        );

        // 未知字段 → 现在在**保存流程**时就被拒（flow_save 校验条件字段）。
        // 比「审批时才发现」早一个数量级：早拒绝不会留下卡死的实例。
        let nodes = vec![
            n("s1", "start", "开始", vec![]),
            n("a1", "approve", "审批", vec![]),
            n("a2", "approve", "高额复核", vec![]),
        ];
        let mut edges = vec![e("e1", "s1", "a1", "normal"), e("e3", "a1", "a2", "normal")];
        edges[1].condition = "foo > 5".to_string();
        let e = flow_save(&db, &input(0, "未知字段流", nodes, edges), "u")
            .unwrap_err()
            .to_string();
        assert!(e.contains("foo"), "应点名不存在的字段：{e}");
        assert!(
            e.contains("报价单") && e.contains("amount"),
            "应说清是哪张单据、可用哪些字段：{e}"
        );

        // 剩下的「晚发现」缺口：字段**声明上有**、但这张单据取不到值
        // （如单据已被删除、aux 查不到）。这种保存时无法判断，只能审批时报。
        let ghost = 9_999_999i64;
        cond_flow("字段缺失流", "amount > 5000");
        assert!(
            intercept(&db, BIZ_QUOTATION, ghost, &u, true, "").is_err(),
            "单据取不到值时审批应报错，而不是静默走兜底分支"
        );
        let _ = unknown_f;
    }

    /// 驳回后重新发起：实例重置回第一个审批节点，正常推进。
    #[test]
    fn intercept_reject_and_restart() {
        let db = mem();
        let nodes = vec![
            n("s1", "start", "开始", vec![]),
            n("a1", "approve", "初审", vec![]),
            n("a2", "approve", "复核", vec![]),
        ];
        let edges = vec![e("e1", "s1", "a1", "normal"), e("e2", "a1", "a2", "normal")];
        let id = flow_save(&db, &input(0, "驳回重审流", nodes, edges), "u").unwrap();
        flow_set_status(&db, id, true, "u").unwrap();

        let sup = User::new("s", "主管", Role::Supervisor);
        // 首个动作即驳回（无 reject 路径）→ 终态 rejected
        assert!(matches!(
            intercept(&db, BIZ_QUOTATION, 7, &sup, false, "不同意").unwrap(),
            Gate::Final { approved: false }
        ));
        // 再次发起 → 实例重置回初审节点，正常推进
        match intercept(&db, BIZ_QUOTATION, 7, &sup, true, "").unwrap() {
            Gate::Pending { next } => assert_eq!(next, "复核"),
            other => panic!("驳回后重新发起应从头推进：{other:?}"),
        }
    }
}
