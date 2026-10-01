//! 我的工作台（对标金蝶工作台/我的看板）：按账号岗位与权限动态聚合
//! **业务卡片 / 我的待办 / 多期趋势**。
//!
//! 设计约束：
//! - **无新权限位**：每域以既有 `Perm` 为门槛（凭证/资金/销售/采购/仓管/生产/成本），
//!   报表域（语句表）在 web 层注入；报销域=本人视角（"我的"天然自洽）；
//!   审批域人人可见运行中实例，待办按节点参与人匹配（workflow::pending_for）。
//! - 趋势 = 近 n 个会计期间（含当期），期间序列 `Period::add_months` 递推，
//!   空期间补 0，前端用既有 `lineChartSvg` 渲染。

use std::collections::BTreeMap;

use fincore::{Money, Period, Perm, User};
use serde::Serialize;

use crate::{Db, DbResult};

/// 期间标签："202601" → "2026-01"
fn p_label(ymm: i32) -> String {
    format!("{}-{:02}", ymm / 100, ymm % 100)
}

/// Money → f64（趋势点位）
pub fn money_f64(m: Money) -> f64 {
    m.inner().to_string().parse::<f64>().unwrap_or(0.0)
}

fn fmt2(v: f64) -> String {
    format!("{v:.2}")
}

/// 期间序列（近 n 期，含当期）
pub fn period_series(cur: Period, n: i32) -> Vec<Period> {
    (0..n).map(|i| cur.add_months(-(n - 1 - i))).collect()
}

/// 期间序列标签
pub fn period_labels(ps: &[Period]) -> Vec<String> {
    ps.iter().map(|p| p_label(p.ymm())).collect()
}

fn fill(ps: &[Period], map: &BTreeMap<i32, f64>) -> Vec<f64> {
    ps.iter().map(|p| *map.get(&p.ymm()).unwrap_or(&0.0)).collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct WbCard {
    pub domain: String,
    pub key: String,
    pub label: String,
    pub value: String,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WbTodo {
    pub domain: String,
    pub key: String,
    pub label: String,
    pub count: i64,
    pub view: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WbSeries {
    pub name: String,
    pub color: String,
    pub points: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WbTrend {
    pub domain: String,
    pub key: String,
    pub title: String,
    pub unit: String,
    pub periods: Vec<String>,
    pub series: Vec<WbSeries>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WbOut {
    pub period: String,
    pub cards: Vec<WbCard>,
    pub todos: Vec<WbTodo>,
    pub trends: Vec<WbTrend>,
}

fn card(domain: &str, key: &str, label: &str, value: String, unit: &str) -> WbCard {
    WbCard {
        domain: domain.to_string(),
        key: key.to_string(),
        label: label.to_string(),
        value,
        unit: unit.to_string(),
    }
}

fn todo(domain: &str, key: &str, label: &str, count: i64, view: &str) -> WbTodo {
    WbTodo {
        domain: domain.to_string(),
        key: key.to_string(),
        label: label.to_string(),
        count,
        view: view.to_string(),
    }
}

fn trend(
    domain: &str,
    key: &str,
    title: &str,
    unit: &str,
    ps: &[Period],
    series: Vec<WbSeries>,
) -> WbTrend {
    WbTrend {
        domain: domain.to_string(),
        key: key.to_string(),
        title: title.to_string(),
        unit: unit.to_string(),
        periods: period_labels(ps),
        series,
    }
}

fn series(name: &str, color: &str, points: Vec<f64>) -> WbSeries {
    WbSeries {
        name: name.to_string(),
        color: color.to_string(),
        points,
    }
}

/// 按权限聚合（凭证/资金/销售/采购/仓管/生产/成本/报销/审批；报表域由 web 层追加）
pub fn collect(db: &Db, user: &User, cur: Period, n: i32) -> DbResult<WbOut> {
    let n = n.clamp(3, 24);
    let ps = period_series(cur, n);
    let y0 = ps[0].ymm();
    let y1 = cur.ymm();
    let mut out = WbOut {
        period: p_label(cur.ymm()),
        cards: Vec::new(),
        todos: Vec::new(),
        trends: Vec::new(),
    };

    // ---- 期初：一条都没录时提醒（否则做到月末才发现期初没建，报表全错）----
    if user.can(Perm::Opening) {
        let n_begin: i64 = db.conn().query_row("SELECT COUNT(*) FROM begin_balance", [], |r| r.get(0))?;
        if n_begin == 0 {
            out.todos.push(todo("期初", "begin_missing", "期初余额未建账", 1, "begin"));
        }
    }

    // ---- 凭证（财务操作/审核岗）----
    if user.can(Perm::VoucherNew) || user.can(Perm::VoucherAudit) {
        let (unposted, posted): (i64, i64) = db.conn().query_row(
            "SELECT COALESCE(SUM(CASE WHEN status IN ('draft','audited') THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN status='posted' THEN 1 ELSE 0 END),0)
             FROM voucher WHERE period=?1 AND status<>'void'",
            [cur.ymm()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let turnover: f64 = db.conn().query_row(
            "SELECT COALESCE(SUM(CAST(e.debit AS REAL)),0)
             FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
             WHERE v.status<>'void' AND e.period=?1",
            [cur.ymm()],
            |r| r.get(0),
        )?;
        out.cards.push(card("凭证", "unposted", "未记账凭证", unposted.to_string(), "张"));
        out.cards.push(card("凭证", "posted", "已记账凭证", posted.to_string(), "张"));
        out.cards.push(card("凭证", "turnover", "本期发生额", fmt2(turnover), "元"));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        let mut st = db.conn().prepare(
            "SELECT e.period, COALESCE(SUM(CAST(e.debit AS REAL)),0)
             FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
             WHERE v.status<>'void' AND e.period BETWEEN ?1 AND ?2
             GROUP BY e.period",
        )?;
        let rows = st
            .query_map(rusqlite::params![y0, y1], |r| {
                Ok((r.get::<_, i32>(0)?, r.get::<_, f64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (p, v) in rows {
            m.insert(p, v);
        }
        out.trends.push(trend(
            "凭证",
            "voucher_turnover",
            "发生额走势",
            "元",
            &ps,
            vec![series("发生额", "#1976d2", fill(&ps, &m))],
        ));
        if user.can(Perm::VoucherPost) {
            out.todos.push(todo("凭证", "voucher_unposted", "未记账凭证待记账", unposted, "vouchers"));
        }
    }

    // ---- 资金（出纳）----
    if user.can(Perm::CashierSign) {
        let docs = crate::receipt::receipt_list(db)?;
        let mut draft: i64 = 0;
        let (mut rec, mut pay): (f64, f64) = (0.0, 0.0);
        let mut rec_m: BTreeMap<i32, f64> = BTreeMap::new();
        let mut pay_m: BTreeMap<i32, f64> = BTreeMap::new();
        for d in &docs {
            if d.status == "draft" && d.period == cur {
                draft += 1;
            }
            if d.status == "audited" {
                let a = money_f64(d.amount);
                if d.kind == "receipt" {
                    rec += a;
                    *rec_m.entry(d.period.ymm()).or_insert(0.0) += a;
                } else if d.kind == "payment" {
                    pay += a;
                    *pay_m.entry(d.period.ymm()).or_insert(0.0) += a;
                }
            }
        }
        out.cards.push(card("资金", "receipt_pending", "待审核收付款", draft.to_string(), "单"));
        out.cards.push(card("资金", "receipt_in", "本期收款额", fmt2(rec), "元"));
        out.cards.push(card("资金", "receipt_out", "本期付款额", fmt2(pay), "元"));
        out.trends.push(trend(
            "资金",
            "funds_flow",
            "收付款走势",
            "元",
            &ps,
            vec![
                series("收款额", "#2e7d32", fill(&ps, &rec_m)),
                series("付款额", "#e64a19", fill(&ps, &pay_m)),
            ],
        ));
        if user.can(Perm::VoucherAudit) {
            out.todos.push(todo("资金", "receipt_todo", "待审核收付款单", draft, "funds"));
        }
    }

    // ---- 销售（订单权限）----
    if user.can(Perm::OrderOps) {
        let quotes = crate::sales::quo_list(db, cur)?;
        let q_pending = quotes.iter().filter(|q| q.status == "draft").count() as i64;
        let sos = crate::scm::so_list(db, cur, None)?;
        let so_pending = sos
            .iter()
            .filter(|o| o.status == crate::scm::SoStatus::Draft)
            .count() as i64;
        let (mut so_amt, mut ship_amt): (Money, Money) = (Money::ZERO, Money::ZERO);
        for o in &sos {
            so_amt = so_amt + o.total_amount;
            ship_amt = ship_amt + o.shipped_amount;
        }
        out.cards.push(card("销售", "quotes", "本期报价单", quotes.len().to_string(), "份"));
        out.cards.push(card("销售", "orders", "本期销售订单", sos.len().to_string(), "份"));
        out.cards.push(card("销售", "order_amt", "本期订单金额", fmt2(money_f64(so_amt)), "元"));
        out.cards.push(card("销售", "ship_amt", "本期发货额", fmt2(money_f64(ship_amt)), "元"));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        for p in &ps {
            let list = crate::scm::so_list(db, *p, None)?;
            let mut s = Money::ZERO;
            for o in &list {
                s = s + o.total_amount;
            }
            m.insert(p.ymm(), money_f64(s));
        }
        out.trends.push(trend(
            "销售",
            "sales_amt",
            "订单金额走势",
            "元",
            &ps,
            vec![series("订单金额", "#1565c0", fill(&ps, &m))],
        ));
        out.todos.push(todo("销售", "quote_todo", "待审批报价单", q_pending, "so-doc"));
        out.todos.push(todo("销售", "so_todo", "待确认销售订单", so_pending, "so-doc"));
    }

    // ---- 采购（订单权限，单据类型区分）----
    if user.can(Perm::OrderOps) {
        let prs = crate::procurement::pr_list(db, cur)?;
        let pr_pending = prs.iter().filter(|p| p.status == "draft").count() as i64;
        let pos = crate::scm::po_list(db, cur, None)?;
        let mut po_amt = Money::ZERO;
        for o in &pos {
            po_amt = po_amt + o.total_amount;
        }
        out.cards.push(card("采购", "reqs", "本期请购单", prs.len().to_string(), "份"));
        out.cards.push(card("采购", "pos", "本期采购订单", pos.len().to_string(), "份"));
        out.cards.push(card("采购", "po_amt", "本期采购金额", fmt2(money_f64(po_amt)), "元"));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        for p in &ps {
            let list = crate::scm::po_list(db, *p, None)?;
            let mut s = Money::ZERO;
            for o in &list {
                s = s + o.total_amount;
            }
            m.insert(p.ymm(), money_f64(s));
        }
        out.trends.push(trend(
            "采购",
            "po_amt",
            "采购金额走势",
            "元",
            &ps,
            vec![series("采购金额", "#6a1b9a", fill(&ps, &m))],
        ));
        out.todos.push(todo("采购", "pr_todo", "待审批请购单", pr_pending, "po-doc"));
    }

    // ---- 仓管 ----
    if user.can(Perm::Warehouse) {
        let exp = crate::batch::expiring_batches(db, 30)?;
        let batches = crate::batch::batch_list(db, "")?;
        let on = batches.iter().filter(|b| b.balance.is_positive()).count() as i64;
        let counts = crate::stocktake::count_list(db)?;
        let pending = counts.iter().filter(|c| c.status == "draft").count() as i64;
        let low = crate::inventory2::below_safety(db)?;
        out.cards.push(card("仓管", "batch_expiry", "临期批次(30天)", exp.len().to_string(), "个"));
        out.cards.push(card("仓管", "batch_on", "在库批次", on.to_string(), "个"));
        out.cards.push(card("仓管", "count_pending", "未完成盘点", pending.to_string(), "单"));
        out.cards.push(card("仓管", "below_safety", "低于安全库存", low.len().to_string(), "项"));
        let mut in_m: BTreeMap<i32, f64> = BTreeMap::new();
        let mut out_m: BTreeMap<i32, f64> = BTreeMap::new();
        let mut st = db.conn().prepare(
            "SELECT period,
                    COALESCE(SUM(CASE WHEN CAST(qty AS REAL) > 0 THEN CAST(qty AS REAL) ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN CAST(qty AS REAL) < 0 THEN -CAST(qty AS REAL) ELSE 0 END),0)
             FROM stock_move WHERE period BETWEEN ?1 AND ?2 GROUP BY period",
        )?;
        let rows = st
            .query_map(rusqlite::params![y0, y1], |r| {
                Ok((
                    r.get::<_, i32>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (p, a, b) in rows {
            in_m.insert(p, a);
            out_m.insert(p, b);
        }
        out.trends.push(trend(
            "仓管",
            "stock_flow",
            "出入库量走势",
            "件",
            &ps,
            vec![
                series("入库量", "#00838f", fill(&ps, &in_m)),
                series("出库量", "#ef6c00", fill(&ps, &out_m)),
            ],
        ));
        out.todos.push(todo("仓管", "expiry_todo", "临期批次处理", exp.len() as i64, "inv-batch"));
        out.todos.push(todo("仓管", "count_todo", "未完成盘点单", pending, "inv-count"));
        out.todos.push(todo("仓管", "safety_todo", "低于安全库存", low.len() as i64, "inv-warehouse"));
    }

    // ---- 生产 ----
    if user.can(Perm::ProductionOps) {
        let pos = crate::scm::prod_list(db, cur, None)?;
        let running = pos
            .iter()
            .filter(|o| matches!(o.status, crate::scm::ProdStatus::InProgress))
            .count() as i64;
        let done = pos
            .iter()
            .filter(|o| matches!(o.status, crate::scm::ProdStatus::Completed))
            .count() as i64;
        out.cards.push(card("生产", "orders", "本期生产订单", pos.len().to_string(), "份"));
        out.cards.push(card("生产", "running", "在产中", running.to_string(), "份"));
        out.cards.push(card("生产", "done", "已完工", done.to_string(), "份"));
        // 生产投入（料工费归集）按期
        let mut cost_m: BTreeMap<i32, f64> = BTreeMap::new();
        let mut st = db.conn().prepare(
            "SELECT po.period, COALESCE(SUM(CAST(pc.amount AS REAL)),0)
             FROM prod_cost pc JOIN production_order po ON po.id=pc.po_id
             WHERE po.period BETWEEN ?1 AND ?2 GROUP BY po.period",
        )?;
        let rows = st
            .query_map(rusqlite::params![y0, y1], |r| {
                Ok((r.get::<_, i32>(0)?, r.get::<_, f64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (p, v) in rows {
            cost_m.insert(p, v);
        }
        out.trends.push(trend(
            "生产",
            "prod_input",
            "生产投入走势",
            "元",
            &ps,
            vec![series("生产投入", "#2e7d32", fill(&ps, &cost_m))],
        ));
        // 完工入库量（prod_complete 的其他入库流水）
        let mut out_m: BTreeMap<i32, f64> = BTreeMap::new();
        let mut st = db.conn().prepare(
            "SELECT period, COALESCE(SUM(CAST(qty AS REAL)),0)
             FROM stock_move
             WHERE kind='other_in' AND memo LIKE '完工入库%' AND period BETWEEN ?1 AND ?2
             GROUP BY period",
        )?;
        let rows = st
            .query_map(rusqlite::params![y0, y1], |r| {
                Ok((r.get::<_, i32>(0)?, r.get::<_, f64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (p, v) in rows {
            out_m.insert(p, v);
        }
        out.trends.push(trend(
            "生产",
            "prod_output",
            "完工入库走势",
            "件",
            &ps,
            vec![series("完工入库量", "#66bb6a", fill(&ps, &out_m))],
        ));
        out.todos.push(todo("生产", "prod_todo", "在产生产订单", running, "work-report"));
    }

    // ---- 成本 ----
    if user.can(Perm::CostOps) {
        let wip = crate::manufacturing::wip_cost(db, cur)?;
        let (mut mat, mut lab, mut oh, mut tot): (Money, Money, Money, Money) =
            (Money::ZERO, Money::ZERO, Money::ZERO, Money::ZERO);
        for r in &wip {
            mat = mat + r.material;
            lab = lab + r.labor;
            oh = oh + r.overhead;
            tot = tot + r.total;
        }
        out.cards.push(card("成本", "wip_total", "在产品总额", fmt2(money_f64(tot)), "元"));
        out.cards.push(card("成本", "wip_mat", "材料", fmt2(money_f64(mat)), "元"));
        out.cards.push(card("成本", "wip_lab", "人工", fmt2(money_f64(lab)), "元"));
        out.cards.push(card("成本", "wip_oh", "制造费用", fmt2(money_f64(oh)), "元"));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        for p in &ps {
            let rows = crate::manufacturing::wip_cost(db, *p)?;
            let mut s = Money::ZERO;
            for r in &rows {
                s = s + r.total;
            }
            m.insert(p.ymm(), money_f64(s));
        }
        out.trends.push(trend(
            "成本",
            "wip_trend",
            "在产品走势",
            "元",
            &ps,
            vec![series("在产品成本", "#5d4037", fill(&ps, &m))],
        ));
    }

    // ---- 报销（本人视角，人人有）----
    {
        let me = user.username.as_str();
        let (mut cnt, mut sub, mut rej): (i64, i64, i64) = (0, 0, 0);
        let mut amt = Money::ZERO;
        for c in crate::business::claim_list(db, cur, None)? {
            if c.applicant != me {
                continue;
            }
            cnt += 1;
            amt = amt + c.amount;
            if matches!(c.status, crate::business::ClaimStatus::Submitted) {
                sub += 1;
            }
            if matches!(c.status, crate::business::ClaimStatus::Rejected) {
                rej += 1;
            }
        }
        out.cards.push(card("报销", "mine", "本期我的报销", cnt.to_string(), "份"));
        out.cards.push(card("报销", "submitted", "待审批", sub.to_string(), "份"));
        out.cards.push(card("报销", "rejected", "被驳回", rej.to_string(), "份"));
        out.cards.push(card("报销", "amount", "本期报销额", fmt2(money_f64(amt)), "元"));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        for p in &ps {
            let mut s = Money::ZERO;
            for c in crate::business::claim_list(db, *p, None)? {
                if c.applicant == me {
                    s = s + c.amount;
                }
            }
            m.insert(p.ymm(), money_f64(s));
        }
        out.trends.push(trend(
            "报销",
            "claim_amt",
            "我的报销走势",
            "元",
            &ps,
            vec![series("报销额", "#ad1457", fill(&ps, &m))],
        ));
        out.todos.push(todo("报销", "claim_todo", "我的报销 待审批/被驳回", sub + rej, "claims"));
    }

    // ---- 审批（人人可见运行中；待办按参与人匹配）----
    {
        let running = crate::workflow::instances(db)?
            .iter()
            .filter(|i| i.status == "running")
            .count() as i64;
        let pend = crate::workflow::pending_for(db, user)?.len() as i64;
        out.cards.push(card("审批", "wf_running", "运行中审批流程", running.to_string(), "个"));
        out.todos.push(todo("审批", "wf_pending", "待我审批的流程", pend, "workflow"));
    }

    Ok(out)
}

/// 待办聚合（轻量版，供通知中心 60s 轮询——不含卡片与趋势计算）。
/// 权限门槛、状态过滤与 collect 完全一致（同一套 Perm 口径，保持同步维护）。
pub fn collect_todos(db: &Db, user: &User, cur: Period) -> DbResult<Vec<WbTodo>> {
    let mut todos = Vec::new();
    // 期初：启用期初一条都没录时提醒（否则做到月末才发现期初没建，报表全错）
    if user.can(Perm::Opening) {
        let n: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM begin_balance",
            [],
            |r| r.get(0),
        )?;
        if n == 0 {
            todos.push(todo("期初", "begin_missing", "期初余额未建账", 1, "begin"));
        }
    }
    // 凭证：未记账待记账
    if user.can(Perm::VoucherNew) || user.can(Perm::VoucherAudit) {
        let (unposted, _): (i64, i64) = db.conn().query_row(
            "SELECT COALESCE(SUM(CASE WHEN status IN ('draft','audited') THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN status='posted' THEN 1 ELSE 0 END),0)
             FROM voucher WHERE period=?1 AND status<>'void'",
            [cur.ymm()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if user.can(Perm::VoucherPost) {
            todos.push(todo("凭证", "voucher_unposted", "未记账凭证待记账", unposted, "vouchers"));
        }
    }
    // 资金：待审核收付款
    if user.can(Perm::CashierSign) {
        let draft = crate::receipt::receipt_list(db)?
            .iter()
            .filter(|d| d.status == "draft" && d.period == cur)
            .count() as i64;
        if user.can(Perm::VoucherAudit) {
            todos.push(todo("资金", "receipt_todo", "待审核收付款单", draft, "funds"));
        }
    }
    // 销售 / 采购
    if user.can(Perm::OrderOps) {
        let q_pending = crate::sales::quo_list(db, cur)?
            .iter()
            .filter(|q| q.status == "draft")
            .count() as i64;
        let sos = crate::scm::so_list(db, cur, None)?;
        let so_pending = sos
            .iter()
            .filter(|o| matches!(o.status, crate::scm::SoStatus::Draft))
            .count() as i64;
        todos.push(todo("销售", "quote_todo", "待审批报价单", q_pending, "so-doc"));
        todos.push(todo("销售", "so_todo", "待确认销售订单", so_pending, "so-doc"));
        let pr_pending = crate::procurement::pr_list(db, cur)?
            .iter()
            .filter(|p| p.status == "draft")
            .count() as i64;
        todos.push(todo("采购", "pr_todo", "待审批请购单", pr_pending, "po-doc"));
    }
    // 仓管
    if user.can(Perm::Warehouse) {
        let exp = crate::batch::expiring_batches(db, 30)?;
        let pending = crate::stocktake::count_list(db)?
            .iter()
            .filter(|c| c.status == "draft")
            .count() as i64;
        let low = crate::inventory2::below_safety(db)?;
        todos.push(todo("仓管", "expiry_todo", "临期批次处理", exp.len() as i64, "inv-batch"));
        todos.push(todo("仓管", "count_todo", "未完成盘点单", pending, "inv-count"));
        todos.push(todo("仓管", "safety_todo", "低于安全库存", low.len() as i64, "inv-warehouse"));
    }
    // 生产
    if user.can(Perm::ProductionOps) {
        let running = crate::scm::prod_list(db, cur, None)?
            .iter()
            .filter(|o| matches!(o.status, crate::scm::ProdStatus::InProgress))
            .count() as i64;
        todos.push(todo("生产", "prod_todo", "在产生产订单", running, "work-report"));
    }
    // 报销（本人视角）
    {
        let (mut sub, mut rej) = (0i64, 0i64);
        for c in crate::business::claim_list(db, cur, None)? {
            if c.applicant != user.username {
                continue;
            }
            if matches!(c.status, crate::business::ClaimStatus::Submitted) {
                sub += 1;
            }
            if matches!(c.status, crate::business::ClaimStatus::Rejected) {
                rej += 1;
            }
        }
        todos.push(todo("报销", "claim_todo", "我的报销 待审批/被驳回", sub + rej, "claims"));
    }
    // 审批：待我审批的流程实例
    {
        let pend = crate::workflow::pending_for(db, user)?.len() as i64;
        todos.push(todo("审批", "wf_pending", "待我审批的流程", pend, "workflow"));
    }
    Ok(todos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use fincore::Role;

    fn domains(out: &WbOut) -> Vec<&str> {
        out.cards.iter().map(|c| c.domain.as_str()).collect()
    }

    #[test]
    fn role_domain_gating_and_series() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();

        // 管理员：全域 + 趋势期数 = n
        let admin = User::new("a", "管理员", Role::Admin);
        let out = collect(&db, &admin, p, 6).unwrap();
        let d = domains(&out);
        for expect in ["凭证", "资金", "销售", "采购", "仓管", "生产", "成本", "报销", "审批"] {
            assert!(d.contains(&expect), "admin 应含 {expect} 域：{d:?}");
        }
        assert!(out.trends.iter().all(|t| t.periods.len() == 6), "趋势期数应为6");
        assert_eq!(out.trends[0].periods[0], "2025-08", "序列从当期往前第6期");
        assert_eq!(out.trends[0].periods[5], "2026-01");
        // 待办结构存在
        assert!(out.todos.iter().any(|t| t.key == "wf_pending"));

        // 仓管：只有 仓管/报销/审批
        let keeper = User::new("k", "仓管", Role::Keeper);
        let out = collect(&db, &keeper, p, 12).unwrap();
        let d = domains(&out);
        assert!(d.contains(&"仓管"));
        assert!(d.contains(&"报销"));
        assert!(!d.contains(&"凭证"), "仓管不应有凭证域：{d:?}");
        assert!(!d.contains(&"资金"));
        assert!(!d.contains(&"销售"));
        assert!(!d.contains(&"成本"));

        // 订单专员：销售+采购，无仓管/凭证/资金
        let clerk = User::new("c", "订单", Role::OrderClerk);
        let out = collect(&db, &clerk, p, 12).unwrap();
        let d = domains(&out);
        assert!(d.contains(&"销售") && d.contains(&"采购"));
        assert!(!d.contains(&"仓管") && !d.contains(&"凭证") && !d.contains(&"资金"));

        // 只读：仅 报销 + 审批（报表域在 web 层注入）
        let viewer = User::new("v", "只读", Role::Viewer);
        let vout = collect(&db, &viewer, p, 12).unwrap();
        let mut d = domains(&vout);
        d.sort();
        d.dedup();
        assert_eq!(d, vec!["审批", "报销"], "Viewer 只见本人报销与审批");
    }
}
