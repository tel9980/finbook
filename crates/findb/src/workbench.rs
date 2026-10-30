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
    ps.iter()
        .map(|p| *map.get(&p.ymm()).unwrap_or(&0.0))
        .collect()
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

/// 区间内**已记账**凭证的借方发生额合计（Rust 侧 Decimal 累加，不走 SQL SUM）
///
/// 只认 `status='posted'`（全仓 H-3 口径），这样工作台的「本期发生额」与试算平衡、
/// 账簿、往来核销是同一个口径。金额是 TEXT 列，`SUM(CAST(x AS REAL))` 会走二进制
/// 浮点，累加后与试算平衡差分，所以必须逐行取回在 Rust 侧加。
fn sum_posted_debit(db: &Db, from: Period, to: Period) -> DbResult<Money> {
    let mut st = db.conn().prepare(
        "SELECT e.debit
         FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
         WHERE v.status='posted' AND e.period BETWEEN ?1 AND ?2",
    )?;
    let rows = st
        .query_map(rusqlite::params![from.ymm(), to.ymm()], |r| {
            Ok(Money::parse_or_zero(&r.get::<_, String>(0)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows.into_iter().sum())
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

    // ---- 凭证（财务操作/审核岗）----
    if user.can(Perm::VoucherNew) || user.can(Perm::VoucherAudit) {
        let (unposted, posted): (i64, i64) = db.conn().query_row(
            "SELECT COALESCE(SUM(CASE WHEN status IN ('draft','audited') THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN status='posted' THEN 1 ELSE 0 END),0)
             FROM voucher WHERE period=?1 AND status<>'void'",
            [cur.ymm()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // 本期发生额：只取**已记账**（H-3 口径，与试算平衡/账簿/往来核销一致），
        // 且金额在 Rust 侧用 Decimal 累加——借方发生额是 TEXT 列，
        // `SUM(CAST(... AS REAL))` 会走二进制浮点，与试算平衡差分。
        // 回归：旧写法是 `status<>'void'` + `SUM(CAST(e.debit AS REAL))`，
        // 工作台首页的「本期发生额」既含草稿又带浮点误差，与其他所有口径都对不上。
        let turnover: f64 = sum_posted_debit(db, cur, cur)?.to_f64();
        out.cards.push(card(
            "凭证",
            "unposted",
            "未记账凭证",
            unposted.to_string(),
            "张",
        ));
        out.cards.push(card(
            "凭证",
            "posted",
            "已记账凭证",
            posted.to_string(),
            "张",
        ));
        out.cards
            .push(card("凭证", "turnover", "本期发生额", fmt2(turnover), "元"));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        let mut st = db.conn().prepare(
            "SELECT e.period, e.debit
             FROM voucher_entry e JOIN voucher v ON v.id=e.voucher_id
             WHERE v.status='posted' AND e.period BETWEEN ?1 AND ?2",
        )?;
        let rows = st
            .query_map(rusqlite::params![y0, y1], |r| {
                Ok((
                    r.get::<_, i32>(0)?,
                    Money::parse_or_zero(&r.get::<_, String>(1)?),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(st);
        // 逐月在 Rust 侧累加后再转 f64（f64 只用于画图，不参与账务计算）
        let mut by_period: BTreeMap<i32, Money> = BTreeMap::new();
        for (p, v) in rows {
            *by_period.entry(p).or_insert(Money::ZERO) += v;
        }
        for (p, v) in by_period {
            m.insert(p, v.to_f64());
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
            out.todos.push(todo(
                "凭证",
                "voucher_unposted",
                "未记账凭证待记账",
                unposted,
                "vouchers",
            ));
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
        out.cards.push(card(
            "资金",
            "receipt_pending",
            "待审核收付款",
            draft.to_string(),
            "单",
        ));
        out.cards
            .push(card("资金", "receipt_in", "本期收款额", fmt2(rec), "元"));
        out.cards
            .push(card("资金", "receipt_out", "本期付款额", fmt2(pay), "元"));
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
            out.todos.push(todo(
                "资金",
                "receipt_todo",
                "待审核收付款单",
                draft,
                "funds",
            ));
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
            so_amt += o.total_amount;
            ship_amt += o.shipped_amount;
        }
        out.cards.push(card(
            "销售",
            "quotes",
            "本期报价单",
            quotes.len().to_string(),
            "份",
        ));
        out.cards.push(card(
            "销售",
            "orders",
            "本期销售订单",
            sos.len().to_string(),
            "份",
        ));
        out.cards.push(card(
            "销售",
            "order_amt",
            "本期订单金额",
            fmt2(money_f64(so_amt)),
            "元",
        ));
        out.cards.push(card(
            "销售",
            "ship_amt",
            "本期发货额",
            fmt2(money_f64(ship_amt)),
            "元",
        ));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        for p in &ps {
            let list = crate::scm::so_list(db, *p, None)?;
            let mut s = Money::ZERO;
            for o in &list {
                s += o.total_amount;
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
        out.todos.push(todo(
            "销售",
            "quote_todo",
            "待审批报价单",
            q_pending,
            "so-doc",
        ));
        out.todos.push(todo(
            "销售",
            "so_todo",
            "待确认销售订单",
            so_pending,
            "so-doc",
        ));
    }

    // ---- 采购（订单权限，单据类型区分）----
    if user.can(Perm::OrderOps) {
        let prs = crate::procurement::pr_list(db, cur)?;
        let pr_pending = prs.iter().filter(|p| p.status == "draft").count() as i64;
        let pos = crate::scm::po_list(db, cur, None)?;
        let mut po_amt = Money::ZERO;
        for o in &pos {
            po_amt += o.total_amount;
        }
        out.cards.push(card(
            "采购",
            "reqs",
            "本期请购单",
            prs.len().to_string(),
            "份",
        ));
        out.cards.push(card(
            "采购",
            "pos",
            "本期采购订单",
            pos.len().to_string(),
            "份",
        ));
        out.cards.push(card(
            "采购",
            "po_amt",
            "本期采购金额",
            fmt2(money_f64(po_amt)),
            "元",
        ));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        for p in &ps {
            let list = crate::scm::po_list(db, *p, None)?;
            let mut s = Money::ZERO;
            for o in &list {
                s += o.total_amount;
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
        out.todos.push(todo(
            "采购",
            "pr_todo",
            "待审批请购单",
            pr_pending,
            "po-doc",
        ));
    }

    // ---- 仓管 ----
    if user.can(Perm::Warehouse) {
        let exp = crate::batch::expiring_batches(db, 30)?;
        let batches = crate::batch::batch_list(db, "")?;
        let on = batches.iter().filter(|b| b.balance.is_positive()).count() as i64;
        let counts = crate::stocktake::count_list(db)?;
        let pending = counts.iter().filter(|c| c.status == "draft").count() as i64;
        let low = crate::inventory2::below_safety(db)?;
        out.cards.push(card(
            "仓管",
            "batch_expiry",
            "临期批次(30天)",
            exp.len().to_string(),
            "个",
        ));
        out.cards
            .push(card("仓管", "batch_on", "在库批次", on.to_string(), "个"));
        out.cards.push(card(
            "仓管",
            "count_pending",
            "未完成盘点",
            pending.to_string(),
            "单",
        ));
        out.cards.push(card(
            "仓管",
            "below_safety",
            "低于安全库存",
            low.len().to_string(),
            "项",
        ));
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
        out.todos.push(todo(
            "仓管",
            "expiry_todo",
            "临期批次处理",
            exp.len() as i64,
            "inv-batch",
        ));
        out.todos.push(todo(
            "仓管",
            "count_todo",
            "未完成盘点单",
            pending,
            "inv-count",
        ));
        out.todos.push(todo(
            "仓管",
            "safety_todo",
            "低于安全库存",
            low.len() as i64,
            "inv-warehouse",
        ));
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
        out.cards.push(card(
            "生产",
            "orders",
            "本期生产订单",
            pos.len().to_string(),
            "份",
        ));
        out.cards
            .push(card("生产", "running", "在产中", running.to_string(), "份"));
        out.cards
            .push(card("生产", "done", "已完工", done.to_string(), "份"));
        // 生产投入（料工费归集）按期 —— 同样在 Rust 侧用 Decimal 累加
        let mut cost_m: BTreeMap<i32, f64> = BTreeMap::new();
        let mut st = db.conn().prepare(
            "SELECT po.period, pc.amount
             FROM prod_cost pc JOIN production_order po ON po.id=pc.po_id
             WHERE po.period BETWEEN ?1 AND ?2",
        )?;
        let rows = st
            .query_map(rusqlite::params![y0, y1], |r| {
                Ok((
                    r.get::<_, i32>(0)?,
                    Money::parse_or_zero(&r.get::<_, String>(1)?),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(st);
        let mut by_period: BTreeMap<i32, Money> = BTreeMap::new();
        for (p, v) in rows {
            *by_period.entry(p).or_insert(Money::ZERO) += v;
        }
        for (p, v) in by_period {
            cost_m.insert(p, v.to_f64());
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
        out.todos.push(todo(
            "生产",
            "prod_todo",
            "在产生产订单",
            running,
            "work-report",
        ));
    }

    // ---- 成本 ----
    if user.can(Perm::CostOps) {
        let wip = crate::manufacturing::wip_cost(db, cur)?;
        let (mut mat, mut lab, mut oh, mut tot): (Money, Money, Money, Money) =
            (Money::ZERO, Money::ZERO, Money::ZERO, Money::ZERO);
        for r in &wip {
            mat += r.material;
            lab += r.labor;
            oh += r.overhead;
            tot += r.total;
        }
        out.cards.push(card(
            "成本",
            "wip_total",
            "在产品总额",
            fmt2(money_f64(tot)),
            "元",
        ));
        out.cards
            .push(card("成本", "wip_mat", "材料", fmt2(money_f64(mat)), "元"));
        out.cards
            .push(card("成本", "wip_lab", "人工", fmt2(money_f64(lab)), "元"));
        out.cards.push(card(
            "成本",
            "wip_oh",
            "制造费用",
            fmt2(money_f64(oh)),
            "元",
        ));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        for p in &ps {
            let rows = crate::manufacturing::wip_cost(db, *p)?;
            let mut s = Money::ZERO;
            for r in &rows {
                s += r.total;
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
            amt += c.amount;
            if matches!(c.status, crate::business::ClaimStatus::Submitted) {
                sub += 1;
            }
            if matches!(c.status, crate::business::ClaimStatus::Rejected) {
                rej += 1;
            }
        }
        out.cards
            .push(card("报销", "mine", "本期我的报销", cnt.to_string(), "份"));
        out.cards
            .push(card("报销", "submitted", "待审批", sub.to_string(), "份"));
        out.cards
            .push(card("报销", "rejected", "被驳回", rej.to_string(), "份"));
        out.cards.push(card(
            "报销",
            "amount",
            "本期报销额",
            fmt2(money_f64(amt)),
            "元",
        ));
        let mut m: BTreeMap<i32, f64> = BTreeMap::new();
        for p in &ps {
            let mut s = Money::ZERO;
            for c in crate::business::claim_list(db, *p, None)? {
                if c.applicant == me {
                    s += c.amount;
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
        out.todos.push(todo(
            "报销",
            "claim_todo",
            "我的报销 待审批/被驳回",
            sub + rej,
            "claims",
        ));
    }

    // ---- 审批（人人可见运行中；待办按参与人匹配）----
    {
        let running = crate::workflow::instances(db)?
            .iter()
            .filter(|i| i.status == "running")
            .count() as i64;
        let pend = crate::workflow::pending_for(db, user)?.len() as i64;
        out.cards.push(card(
            "审批",
            "wf_running",
            "运行中审批流程",
            running.to_string(),
            "个",
        ));
        out.todos.push(todo(
            "审批",
            "wf_pending",
            "待我审批的流程",
            pend,
            "workflow",
        ));
    }
    // ---- 税务：申报日历提醒（窗口期内才出现；与 collect_todos 同口径）----
    if user.can(Perm::FinReport) {
        let taxpayer = crate::taxdecl::TaxOptions::load(db)
            .map(|o| o.taxpayer)
            .unwrap_or_default();
        let today = chrono::Local::now().date_naive();
        for t in crate::taxdecl::deadline_todos(today, &taxpayer) {
            out.todos.push(todo("税务", t.key, &t.label, 1, "tax-decl"));
        }
    }

    Ok(out)
}

/// 待办聚合（轻量版，供通知中心 60s 轮询——不含卡片与趋势计算）。
/// 权限门槛、状态过滤与 collect 完全一致（同一套 Perm 口径，保持同步维护）。
pub fn collect_todos(db: &Db, user: &User, cur: Period) -> DbResult<Vec<WbTodo>> {
    let mut todos = Vec::new();
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
            todos.push(todo(
                "凭证",
                "voucher_unposted",
                "未记账凭证待记账",
                unposted,
                "vouchers",
            ));
        }
    }
    // 资金：待审核收付款
    if user.can(Perm::CashierSign) {
        let draft = crate::receipt::receipt_list(db)?
            .iter()
            .filter(|d| d.status == "draft" && d.period == cur)
            .count() as i64;
        if user.can(Perm::VoucherAudit) {
            todos.push(todo(
                "资金",
                "receipt_todo",
                "待审核收付款单",
                draft,
                "funds",
            ));
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
        todos.push(todo(
            "销售",
            "quote_todo",
            "待审批报价单",
            q_pending,
            "so-doc",
        ));
        todos.push(todo(
            "销售",
            "so_todo",
            "待确认销售订单",
            so_pending,
            "so-doc",
        ));
        let pr_pending = crate::procurement::pr_list(db, cur)?
            .iter()
            .filter(|p| p.status == "draft")
            .count() as i64;
        todos.push(todo(
            "采购",
            "pr_todo",
            "待审批请购单",
            pr_pending,
            "po-doc",
        ));
    }
    // 仓管
    if user.can(Perm::Warehouse) {
        let exp = crate::batch::expiring_batches(db, 30)?;
        let pending = crate::stocktake::count_list(db)?
            .iter()
            .filter(|c| c.status == "draft")
            .count() as i64;
        let low = crate::inventory2::below_safety(db)?;
        todos.push(todo(
            "仓管",
            "expiry_todo",
            "临期批次处理",
            exp.len() as i64,
            "inv-batch",
        ));
        todos.push(todo(
            "仓管",
            "count_todo",
            "未完成盘点单",
            pending,
            "inv-count",
        ));
        todos.push(todo(
            "仓管",
            "safety_todo",
            "低于安全库存",
            low.len() as i64,
            "inv-warehouse",
        ));
    }
    // 生产
    if user.can(Perm::ProductionOps) {
        let running = crate::scm::prod_list(db, cur, None)?
            .iter()
            .filter(|o| matches!(o.status, crate::scm::ProdStatus::InProgress))
            .count() as i64;
        todos.push(todo(
            "生产",
            "prod_todo",
            "在产生产订单",
            running,
            "work-report",
        ));
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
        todos.push(todo(
            "报销",
            "claim_todo",
            "我的报销 待审批/被驳回",
            sub + rej,
            "claims",
        ));
    }
    // 审批：待我审批的流程实例
    {
        let pend = crate::workflow::pending_for(db, user)?.len() as i64;
        todos.push(todo(
            "审批",
            "wf_pending",
            "待我审批的流程",
            pend,
            "workflow",
        ));
    }
    // 税务：申报日历提醒（窗口期内才出现；小规模/一般人按账套税务设置区分）
    if user.can(Perm::FinReport) {
        let taxpayer = crate::taxdecl::TaxOptions::load(db)
            .map(|o| o.taxpayer)
            .unwrap_or_default();
        let today = chrono::Local::now().date_naive();
        for t in crate::taxdecl::deadline_todos(today, &taxpayer) {
            todos.push(todo("税务", t.key, &t.label, 1, "tax-decl"));
        }
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

    /// 工作台的「本期发生额」必须与试算平衡一致。
    ///
    /// 回归两处：① `status<>'void'` 把草稿也算进发生额（与 H-3 口径的试算平衡
    /// 对不上）；② `SUM(CAST(e.debit AS REAL))` 走二进制浮点累加，与 Decimal
    /// 口径差分。这里直接构造「含分位尾数、且一半是草稿」的数据来卡这两点。
    #[test]
    fn turnover_matches_trial_balance_posted_only() {
        use fincore::{Entry, Voucher};
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();

        // 借方合计 = 0.1 + 0.2 = 0.3 这类分位尾数，REAL 累加会现形
        let mk = |no: i32, debit: &str| -> i64 {
            let mut v = Voucher::new(p, d, "记", no);
            v.push_entry(Entry {
                debit: Money::parse(debit).unwrap(),
                aux: fincore::AuxRef {
                    customer: Some("C01".into()),
                    ..Default::default()
                },
                ..Entry::new(1, "112201", "应收")
            });
            v.push_entry(Entry {
                credit: Money::parse(debit).unwrap(),
                aux: fincore::AuxRef {
                    customer: Some("C01".into()),
                    ..Default::default()
                },
                ..Entry::new(2, "112201", "应收冲减")
            });
            crate::vouchers::save(&db, &mut v).unwrap()
        };
        let posted_a = mk(1, "0.10");
        let posted_b = mk(2, "0.20");
        let draft = mk(3, "999.00"); // 草稿不得计入
        crate::vouchers::post(&db, posted_a, "u").unwrap();
        crate::vouchers::post(&db, posted_b, "u").unwrap();
        let _ = draft;

        // 期望 = 已记账两张的借方合计，Rust 侧 Decimal 累加
        let expect = Money::parse("0.10").unwrap() + Money::parse("0.20").unwrap();
        let got = sum_posted_debit(&db, p, p).unwrap();
        assert_eq!(got, expect, "已记账发生额应为 0.30，草稿的 999.00 不得计入");

        // 卡片上的「本期发生额」用的是同一个函数，且经 f64/2 位格式化后仍一致
        let admin = User::new("a", "管理员", Role::Admin);
        let out = collect(&db, &admin, p, 6).unwrap();
        let card = out
            .cards
            .iter()
            .find(|c| c.domain == "凭证" && c.key == "turnover")
            .expect("应有本期发生额卡片");
        assert_eq!(
            card.value, "0.30",
            "工作台本期发生额应为 0.30（含草稿会变成 999.30，REAL 累加会变成别的数）"
        );
    }

    #[test]
    fn role_domain_gating_and_series() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();

        // 管理员：全域 + 趋势期数 = n
        let admin = User::new("a", "管理员", Role::Admin);
        let out = collect(&db, &admin, p, 6).unwrap();
        let d = domains(&out);
        for expect in [
            "凭证", "资金", "销售", "采购", "仓管", "生产", "成本", "报销", "审批",
        ] {
            assert!(d.contains(&expect), "admin 应含 {expect} 域：{d:?}");
        }
        assert!(
            out.trends.iter().all(|t| t.periods.len() == 6),
            "趋势期数应为6"
        );
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

/// 专用查询：一次性算出 12 期 × 5 个报表趋势值
/// 避免 wb_report_domain 里 26 次完整报表渲染（每次都重扫 voucher_entry + 渲染）。
///
/// 返回：BTreeMap<period_ymm, (assets, liabilities, equity, revenue, net_profit)>
/// 金额单位：Money（后续在 web 层转 f64 画图）。
pub fn collect_report_trends(
    db: &Db,
    user: &User,
    cur: Period,
    n: i32,
) -> DbResult<BTreeMap<i32, (Money, Money, Money, Money, Money)>> {
    let n = n.clamp(3, 24);
    let ps = period_series(cur, n);
    let y0 = ps[0].ymm();
    let y1 = cur.ymm();

    // 预填 0，保证每期都有键
    let mut m: BTreeMap<i32, (Money, Money, Money, Money, Money)> = BTreeMap::new();
    for p in &ps {
        m.insert(
            p.ymm(),
            (
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
                Money::ZERO,
            ),
        );
    }

    // 获取用户的数据范围过滤条件
    let scope = &user.data_scope;
    let mut conds = vec!["v.status = 'posted'".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(y0), Box::new(y1)];
    let mut param_idx = 3;

    // 科目区间
    let lo = scope.account_from.trim();
    let hi = scope.account_to.trim();
    if !lo.is_empty() {
        params.push(Box::new(lo.to_string()));
        conds.push(format!("a.code >= ?{}", param_idx));
        param_idx += 1;
    }
    if !hi.is_empty() {
        params.push(Box::new(hi.to_string()));
        conds.push(format!("a.code <= ?{}", param_idx));
        param_idx += 1;
    }
    // 仅本人填制的凭证
    if scope.own_voucher_only {
        params.push(Box::new(user.username.clone()));
        conds.push(format!("v.prepared_by = ?{}", param_idx));
        param_idx += 1;
    }

    let where_clause = conds.join(" AND ");

    // 一条 SQL：按 period + category 聚合借/贷发生额（用 f64 聚合，仅用于趋势图展示，不参与账务计算）
    // category 来自 account 表（asset/liability/equity/income/expense）
    // 利润表口径：需要剔除结转损益凭证（v.source <> 'carry_forward'）
    // 资产负债表口径：需要年初至期末累计（此处只取各期发生额，累计在 Rust 侧做）
    let sql = format!(
        r#"
        SELECT e.period, a.category,
               SUM(CAST(e.debit AS REAL)) as sum_debit,
               SUM(CAST(e.credit AS REAL)) as sum_credit
        FROM voucher_entry e
        JOIN voucher v ON v.id = e.voucher_id
        JOIN account a ON a.code = e.account_code
        WHERE {where_clause}
          AND e.period BETWEEN ?1 AND ?2
          AND v.source <> 'carry_forward'
        GROUP BY e.period, a.category
        "#,
    );

    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), |r| {
            Ok((
                r.get::<_, i32>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?, // sum_debit as f64
                r.get::<_, f64>(3)?, // sum_credit as f64
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // 先按 period 分组聚合：每期得到 5 大类的借/贷发生额（f64）
    #[derive(Default)]
    struct CatAmt {
        asset_debit: f64,
        asset_credit: f64,
        liability_debit: f64,
        liability_credit: f64,
        equity_debit: f64,
        equity_credit: f64,
        income_debit: f64,
        income_credit: f64,
        expense_debit: f64,
        expense_credit: f64,
    }
    let mut by_period: BTreeMap<i32, CatAmt> = BTreeMap::new();
    for (p, cat, d, c) in rows {
        let entry = by_period.entry(p).or_default();
        match cat.as_str() {
            "Asset" => {
                entry.asset_debit += d;
                entry.asset_credit += c;
            }
            "Liability" => {
                entry.liability_debit += d;
                entry.liability_credit += c;
            }
            "Equity" => {
                entry.equity_debit += d;
                entry.equity_credit += c;
            }
            "Income" => {
                entry.income_debit += d;
                entry.income_credit += c;
            }
            "Expense" => {
                entry.expense_debit += d;
                entry.expense_credit += c;
            }
            _ => {}
        }
    }

    // 逐期累计得到资产/负债/权益的期末余额（年初余额为 0，简化：工作台趋势只看本年发生额累计）
    // 实际资产负债表包含期初余额（begin_balance），这里为趋势图简化只算本年发生额累计
    // 这与 wb_report_domain 的资产负债表有差异（后者含期初），但作为趋势对比足够
    let mut asset_cum = 0.0;
    let mut liability_cum = 0.0;
    let mut equity_cum = 0.0;

    for p in &ps {
        let cat = by_period.entry(p.ymm()).or_default();
        // 本期净增
        asset_cum += cat.asset_debit - cat.asset_credit;
        liability_cum += cat.liability_debit - cat.liability_credit;
        equity_cum += cat.equity_debit - cat.equity_credit;
        let revenue = cat.income_credit - cat.income_debit; // 收入类贷方为正
        let expense = cat.expense_debit - cat.expense_credit; // 费用类借方为正
        let net_profit = revenue - expense;

        // 转 Money（用 f64 字符串构造，仅用于趋势图展示，不参与账务计算）
        m.insert(
            p.ymm(),
            (
                Money::parse_or_zero(&asset_cum.to_string()),
                Money::parse_or_zero(&liability_cum.to_string()),
                Money::parse_or_zero(&equity_cum.to_string()),
                Money::parse_or_zero(&revenue.to_string()),
                Money::parse_or_zero(&net_profit.to_string()),
            ),
        );
    }

    Ok(m)
}
