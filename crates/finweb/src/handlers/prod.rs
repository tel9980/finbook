//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

/// 在制品成本（WIP）：按期间列未完工订单的料/工/费
pub(super) async fn cost_wip_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": findb::manufacturing::wip_cost(&db, period)?,
    })))
}

/// 销售订单下推生产订单
///
/// 下推数量封顶在引擎里（`scm::prod_from_so`），前端再封一次只会变成两处口径。
pub(super) async fn prod_from_so_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ProdFromSoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.trim().is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let want = if req.qty.trim().is_empty() {
        None
    } else {
        let q = parse_money_checked(&req.qty)?;
        if !q.is_positive() {
            return Err(AppError::bad_request("下推数量必须大于 0"));
        }
        Some(q)
    };
    let id = findb::scm::prod_from_so(&db, req.so_id, period, date, want, user.username())?;
    let o = findb::scm::prod_get(&db, id)?.ok_or_else(|| AppError::not_found("生产订单不存在"))?;
    let atp = findb::scm::atp(&db, &o.item_code)?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "no": o.no,
        "item_code": o.item_code,
        "item_name": o.item_name,
        "planned_qty": o.planned_qty,
        "so_id": o.so_id,
        "atp": atp,
    })))
}

/// 成本差异：实际 vs 标准（按订单）
pub(super) async fn cost_variance_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": findb::manufacturing::cost_variance_report(&db, period)?,
    })))
}

/// 成本预测：BOM 参考料本 × 计划量
pub(super) async fn cost_forecast_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": findb::manufacturing::cost_forecast_report(&db, period)?,
    })))
}

/// 制造费用分摊：按基准试算/应用（写入 CostType::Overhead 归集）
pub(super) async fn cost_overhead_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OverheadReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = period_checked(req.period)?;
    let amount = parse_money_checked(&req.amount)?;
    if !amount.is_positive() {
        return Err(AppError::bad_request("分摊金额必须大于 0"));
    }
    let base = findb::manufacturing::OverheadBase::parse(&req.base);
    let rows = findb::manufacturing::overhead_allocate_with(&db, period, amount, base, req.apply)?;
    if req.apply {
        db.log(
            user.username(),
            "成本",
            "制造费用分摊",
            &format!(
                "{} {} {} 笔",
                period_to_str(period),
                amount.fmt_money(),
                rows.len()
            ),
        )?;
    }
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|(id, m)| json!({ "po_id": id, "amount": m.fmt_money() }))
        .collect();
    Ok(Json(json!({
        "ok": true,
        "applied": req.apply,
        "base": base.label(),
        "rows": items,
    })))
}

/// 标准成本差异分析
///
/// 读权限与报表同级：差异分析用的是收发存汇总与成本配置，看它等于看成本结构。
pub(super) async fn get_cost_variance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let r = findb::stdcost::variance_report(&db, period)?;
    Ok(Json(json!({
        "period": r.period,
        "rows": r.rows,
        "total_price": r.total_price,
        "total_spend": r.total_spend,
        "missing": r.missing,
        "warnings": r.warnings,
        // 「算了但没算的」要能在界面上说清楚，而不是让人以为差异表是完整的
        "std_items": findb::business::cost_configs(&*db)?,
    })))
}

// ---------------- 可视化工作流（对标金蝶审批流设计器） ----------------

/// 流程列表（含节点/连线）。查看=Report；保存/发布/删除=SysOption（账套配置权）。
pub(super) async fn list_workflows(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::workflow::flow_list(&*db)? })))
}

/// 预置流程模板清单（不返回 nodes/edges 全文，前端只要元信息做选择卡片；
/// 真正的图在 apply 之后随流程一起返回）
pub(super) async fn list_wf_templates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    // 带上「该单据是否已有流程」，前端好提示是新建还是会被替换
    let existing = findb::workflow::flow_list(&*db)?;
    let rows: Vec<serde_json::Value> = findb::workflow::templates()
        .into_iter()
        .map(|t| {
            let used: Vec<serde_json::Value> = t
                .biz_types
                .iter()
                .map(|b| {
                    let hit = existing.iter().find(|f| &f.biz_type == b);
                    json!({
                        "biz_type": b,
                        "biz_label": findb::workflow::biz_label(b),
                        "flow_id": hit.map(|f| f.id).unwrap_or(0),
                        "flow_status": hit.map(|f| f.status.clone()).unwrap_or_default(),
                    })
                })
                .collect();
            json!({
                "key": t.key, "name": t.name, "desc": t.desc,
                "biz_types": t.biz_types,
                "requires_fields": t.requires_fields,
                "node_count": t.nodes.len(),
                "applicable": used,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
}

/// 按预置模板落一条流程（草稿态）
///
/// 落的是草稿而非直接发布：模板是行业惯例，不是这家企业的授权制度；而且
/// 已有单据在流转时改流程会让在途实例指向不存在的节点。
pub(super) async fn apply_wf_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<WfTemplateApplyReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let f = findb::workflow::apply_template(&db, &req.key, &req.biz_type, user.username())?;
    db.log(
        user.username(),
        "工作流",
        "应用模板",
        &format!("{}（{}）→ #{} 草稿", f.biz_type, f.name, f.id),
    )?;
    Ok(Json(json!({ "flow": f })))
}

pub(super) async fn save_workflow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(f): Json<findb::workflow::WfFlowInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let id = findb::workflow::flow_save(&db, &f, user.username())?;
    db.log(
        user.username(),
        "工作流",
        "保存流程",
        &format!(
            "{}（{}，节点 {}）",
            f.name,
            findb::workflow::biz_label(&f.biz_type),
            f.nodes.len()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

pub(super) async fn publish_workflow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    findb::workflow::flow_set_status(&db, id, true, user.username())?;
    db.log(user.username(), "工作流", "发布流程", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn unpublish_workflow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    findb::workflow::flow_set_status(&db, id, false, user.username())?;
    db.log(user.username(), "工作流", "撤回发布", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_workflow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    findb::workflow::flow_delete(&db, id)?;
    db.log(user.username(), "工作流", "删除流程", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 运行实例（当前节点/状态/轨迹），供「工作流」页回放
pub(super) async fn list_wf_instances(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::workflow::instances(&*db)? })))
}

/// 某单据类型可用的条件字段（画布上配条件边时的候选清单）
///
/// 字段集**按单据类型不同**：生产订单没有 amount、报销单没有 planned_qty。
/// 前端以前是写死一句「amount/qty/customer_code 等」，用户在生产订单上照着写
/// `amount > 5000` 会一路保存成功、直到单据提交审批才报「条件字段不存在」——
/// 流程已经建了、实例已经起了，单据卡死。所以这里给前端一份真清单。
pub(super) async fn get_wf_cond_fields(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let biz = q.get("biz_type").map(|s| s.trim()).unwrap_or_default();
    if findb::workflow::ALL_BIZ.iter().all(|(k, _)| *k != biz) {
        return Err(AppError::bad_request(format!(
            "非法业务类型：{biz}；可选：{}",
            findb::workflow::ALL_BIZ
                .iter()
                .map(|(k, l)| format!("{k}（{l}）"))
                .collect::<Vec<_>>()
                .join("、")
        )));
    }
    let _ = &state;
    Ok(Json(json!({
        "biz_type": biz,
        "biz_label": findb::workflow::biz_label(biz),
        "fields": findb::workflow::cond_fields_of(biz),
    })))
}

pub(super) async fn list_prod_orders(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let orders = findb::scm::prod_list(&db, period, None)?;
    let items: Vec<serde_json::Value> = orders
        .iter()
        .map(|o| {
            serde_json::json!({
                "id": o.id,
                "no": o.no,
                "item_code": o.item_code,
                "item_name": o.item_name,
                "planned_qty": o.planned_qty.fmt_qty(),
                "completed_qty": o.completed_qty.fmt_qty(),
                "status": format!("{:?}", o.status),
                "order_kind": o.order_kind,
                "so_id": o.so_id,
                "so_no": if o.so_id > 0 {
                    findb::scm::so_get(&db, o.so_id)
                        .ok()
                        .flatten()
                        .map(|s| s.no)
                        .unwrap_or_default()
                } else {
                    String::new()
                },
                "supplier_name": o.supplier_name,
                "plan_start": o.plan_start,
                "plan_end": o.plan_end,
            })
        })
        .collect();
    Ok(Json(
        serde_json::json!({ "period": period_to_str(period), "orders": items }),
    ))
}

pub(super) async fn get_prod_ops(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let ops = advanced::prod_op_list(&db, id)?;
    Ok(Json(serde_json::json!({ "po_id": id, "ops": ops })))
}

pub(super) async fn report_prod_op(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OpReportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    advanced::prod_op_report(
        &db,
        req.op_id,
        parse_money_checked(&req.qty)?,
        parse_money_checked(&req.hours)?,
    )?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn finish_prod_op(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OpFinishReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    advanced::prod_op_finish(&db, req.op_id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 委外加工费确认：借 500102 生产成本-直接人工 / 贷 应付科目（供应商辅助）；
/// 金额并入该订单人工要素，完工时随 140501 结转（委外=生产的变体，全链复用）。
pub(super) async fn outsource_fee_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<OutsourceFeeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let date = prod_act_date(&req.date);
    let amount = parse_money_checked(&req.amount)?;
    let vid = findb::manufacturing::outsource_fee(&db, id, amount, date, user.username())?;
    db.log(
        user.username(),
        "生产",
        "委外加工费",
        &format!("PO#{id} ×{} 凭证#{vid}", amount.fmt_qty()),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

/// 细排：批量写回计划开工/完工日（仅未完工订单可排）
pub(super) async fn schedule_prod(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ScheduleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    if req.items.is_empty() {
        return Err(AppError::bad_request("排产清单为空"));
    }
    let db = &user.db;
    let items: Vec<(i64, String, String)> = req
        .items
        .into_iter()
        .map(|i| (i.id, i.start.trim().to_string(), i.end.trim().to_string()))
        .collect();
    let n = findb::scm::prod_schedule(&db, &items)?;
    db.log(
        user.username(),
        "生产",
        "生产排产",
        &format!("写回 {n} 单计划日期"),
    )?;
    Ok(Json(json!({ "ok": true, "updated": n })))
}

/// 下达生产订单（MRP 结果页「下达」/ 手工）：状态=已下达。
/// 此前生产订单全系统无创建入口（仅 MRP 计划与测试直造），此端点补齐工厂链第一环。
pub(super) async fn create_prod_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ProdCreateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let code = req.item_code.trim();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少存货编码 item_code"));
    }
    let qty = parse_money_checked(&req.qty)?;
    if !qty.is_positive() {
        return Err(AppError::bad_request("计划数量必须大于 0"));
    }
    let period = current_period(&state, &user);
    let mut order = findb::scm::ProductionOrder {
        id: 0,
        no: String::new(),
        period,
        date: prod_act_date(&req.date),
        item_code: code.to_string(),
        item_name: code.to_string(),
        planned_qty: qty,
        completed_qty: Money::ZERO,
        status: findb::scm::ProdStatus::Released,
        work_center: req.work_center.trim().to_string(),
        so_id: 0, // 独立建单；来自销售订单的走 /api/prod/from-so
        prepared_by: user.username().to_string(),
        memo: String::new(),
        order_kind: if req.kind.trim() == "outsourcing" {
            "outsourcing".to_string()
        } else {
            "inhouse".to_string()
        },
        supplier_code: req.supplier_code.trim().to_string(),
        supplier_name: req.supplier_name.trim().to_string(),
        plan_start: String::new(),
        plan_end: String::new(),
    };
    order.no = findb::scm::prod_next_no(&db, period)?;
    let id = findb::scm::prod_save(&db, &mut order)?;
    db.log(
        user.username(),
        "生产",
        "下达生产订单",
        &format!("{} {} ×{}", order.no, code, qty.fmt_qty()),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "no": order.no })))
}

/// 下达生产订单：草稿 → 已下达。**这是生产订单的审批动作**。
///
/// 为什么要单开一个端点：原来只有 `create_prod_ep`，且创建即 `Released`——
/// 没有一个「下达前把关」的动作点，审批流根本挂不上去（配了流程也永远不生效，
/// 且不报任何错）。这里把「下达」显式化成一步，与金蝶星空「生产订单下达」一致。
///
/// 老路径不动：`create_prod_ep` 仍直接建 `Released`，避免既有调用方行为变化。
pub(super) async fn prod_release_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    // 先过工作流：未到终态时只推进流程，不改订单状态
    match workflow_gate(
        &db,
        &user,
        findb::workflow::BIZ_PRODUCTION_ORDER,
        id,
        "生产订单",
    )? {
        WfGate::Pending(next) => return Ok(Json(json!({ "ok": true, "pending": next }))),
        WfGate::Rejected => return Ok(Json(json!({ "ok": true, "rejected": true }))),
        WfGate::Continue => {}
    }
    findb::scm::prod_release(&db, id)?;
    db.log(user.username(), "生产", "下达生产订单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true, "status": "已下达" })))
}

/// 开工：已下达 → 生产中（完工入库的前置状态；条件更新防并发）
pub(super) async fn prod_start_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    findb::manufacturing::prod_start(&db, id)?;
    db.log(user.username(), "生产", "开工", &format!("PO#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 生产订单变更（草稿/已下达）：数量不得低于已完工；逐字段留痕
pub(super) async fn update_prod_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ProdUpdateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    if findb::manufacturing::get_prod_order(&db, id)?.is_none() {
        return Err(AppError::not_found("生产订单不存在"));
    }
    let qty = if req.qty.trim().is_empty() {
        None
    } else {
        Some(parse_money_checked(&req.qty)?)
    };
    findb::manufacturing::prod_update(
        &db,
        id,
        qty,
        req.plan_start.as_deref(),
        req.plan_end.as_deref(),
        req.memo.as_deref(),
        user.username(),
    )?;
    db.log(user.username(), "生产", "生产订单变更", &format!("PO#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 生产订单取消（仅草稿/已下达）
pub(super) async fn cancel_prod_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    if findb::manufacturing::get_prod_order(&db, id)?.is_none() {
        return Err(AppError::not_found("生产订单不存在"));
    }
    findb::manufacturing::prod_cancel(&db, id, user.username())?;
    db.log(user.username(), "生产", "生产订单取消", &format!("PO#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 生产订单变更历史
pub(super) async fn list_prod_changes(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows: Vec<serde_json::Value> = findb::scm2::change_log_list(&db, "prod", id)?
        .iter()
        .map(|(field, old_v, new_v, by, at)| {
            json!({
                "field": field, "old_value": old_v, "new_value": new_v,
                "changed_by": by, "changed_at": at,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
}

/// 工序检验（仅生产中订单；不合格必选处置；报废同步扣减计划量）
pub(super) async fn prod_qc_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ProdQcReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    if findb::manufacturing::get_prod_order(&db, id)?.is_none() {
        return Err(AppError::not_found("生产订单不存在"));
    }
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let qty_fail = if req.qty_fail.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&req.qty_fail)?
    };
    let (qid, no, result) = findb::manufacturing::prod_qc_save(
        &db,
        id,
        parse_money_checked(&req.qty_insp)?,
        qty_fail,
        &req.disposition,
        date,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "生产",
        "工序检验",
        &format!("{no} PO#{id} 结论 {result}"),
    )?;
    Ok(Json(
        json!({ "ok": true, "id": qid, "no": no, "result": result }),
    ))
}

/// 工序检验记录
pub(super) async fn list_prod_qc(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(
        json!({ "rows": findb::manufacturing::prod_qc_list(&db, id)? }),
    ))
}

/// 领料出库：按 BOM × 计划量 × (1+损耗) 展开，同事务 扣库存 + 归集生产成本 +
/// 出领料凭证（借 500101 生产成本-直接材料 / 贷各物料科目，数量核算）。
/// 凭证期间随订单；仅 已下达/生产中 可领。
pub(super) async fn prod_issue_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ProdActReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let order = findb::manufacturing::get_prod_order(&db, id)?
        .ok_or_else(|| AppError::bad_request("生产订单不存在"))?;
    if !matches!(
        order.status,
        findb::scm::ProdStatus::Released | findb::scm::ProdStatus::InProgress
    ) {
        return Err(AppError::bad_request("仅已下达/生产中的订单可领料"));
    }
    // 按单限额领料（v1：BOM 全量一次领齐，每订单仅可领一次——重复领料按累计流水拦截；
    // 带数量的部分领/超额补料留待领料单流程迭代）
    let issued = findb::manufacturing::prod_issue_count(&db, &order.no)?;
    if issued > 0 {
        return Err(AppError::bad_request(format!(
            "该订单已领过料（累计 {issued} 笔流水）：按单限额只允许领料一次，补料走退料/人工调整"
        )));
    }
    let date = prod_act_date(&req.date);
    let rows =
        findb::manufacturing::prod_issue_materials(&db, id, date, order.period, user.username())?;
    let total: Money = rows.iter().map(|(_, _, a)| *a).sum();
    db.log(
        user.username(),
        "生产",
        "领料出库",
        &format!("{} 项数 {} 成本 {}", order.no, rows.len(), total.fmt_qty()),
    )?;
    Ok(Json(json!({
        "ok": true,
        "items": rows.len(),
        "total": total.fmt_qty(),
        "rows": rows
            .iter()
            .map(|(c, q, a)| json!({ "item": c, "qty": q.fmt_qty(), "amount": a.fmt_qty() }))
            .collect::<Vec<_>>()
    })))
}

/// 完工入库：qty 留空 = 其余未完工数量；同事务 入库 + 订单推进 completed +
/// 完工结转凭证（借 140501 库存商品数量核算 / 贷 500101~03 各要素，只出非零）。
/// 仅生产中订单可完工（条件更新防重复完工）。
pub(super) async fn prod_complete_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ProdActReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let order = findb::manufacturing::get_prod_order(&db, id)?
        .ok_or_else(|| AppError::bad_request("生产订单不存在"))?;
    // 完工数量必须落在 [1, 未完工数量] 区间：prod_complete 把 completed_qty 直接覆盖写，
    // 其唯一守卫是条件更新 status='in_progress'——那只防重复完工，拦不住超完工。
    // 不设上限时 {"qty":"999999"} 会把 100 件的订单推进 999999，并按 total_cost/999999
    // 摊出单位成本、连带虚增库存商品。
    let remain = order.planned_qty - order.completed_qty;
    let qty = if req.qty.trim().is_empty() {
        remain
    } else {
        let q = parse_money_checked(&req.qty)?;
        if q > remain {
            return Err(AppError::bad_request(format!(
                "完工数量 {} 超过订单未完工数量 {}",
                q.fmt_qty(),
                remain.fmt_qty()
            )));
        }
        q
    };
    if !qty.is_positive() {
        return Err(AppError::bad_request(
            "完工数量必须大于 0（或订单已全部完工）",
        ));
    }
    let date = prod_act_date(&req.date);
    let move_id =
        findb::manufacturing::prod_complete(&db, id, date, order.period, qty, user.username())?;
    db.log(
        user.username(),
        "生产",
        "完工入库",
        &format!("{} ×{}", order.no, qty.fmt_qty()),
    )?;
    Ok(Json(
        json!({ "ok": true, "move_id": move_id, "qty": qty.fmt_qty() }),
    ))
}

// ---- 审批流 ----

pub(super) async fn list_approvals(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = advanced::approval_list(&db, 100)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn list_approval_todo(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = advanced::approval_todo(&db, user.username())?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn get_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let ap = advanced::approval_get(&db, id)?
        .ok_or_else(|| AppError::NotFound("审批流不存在".to_string()))?;
    Ok(Json(serde_json::json!({ "approval": ap })))
}

pub(super) async fn start_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ApprovalStartReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let id = advanced::approval_start(
        &db,
        &req.biz_kind,
        req.biz_id,
        &req.title,
        user.username(),
        &req.approvers,
    )?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

pub(super) async fn act_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ApprovalActReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let ap = advanced::approval_act(&db, id, user.username(), req.approve, &req.comment)?;
    Ok(Json(serde_json::json!({ "approval": ap })))
}

pub(super) async fn cancel_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    advanced::approval_cancel(&db, id, user.username())?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn clear_cost_method(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    findb::business::item_cost_method_clear(&db, &item)?;
    Ok(Json(json!({ "ok": true })))
}

/// 单据的流程实例状态（流程条 / 列表行徽标）：仅工作流四类业务
pub(super) async fn get_wf_instance_for(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let biz_type = q.get("biz_type").map(String::as_str).unwrap_or("");
    if !matches!(biz_type, "quotation" | "purchase_req" | "claim" | "receipt") {
        return Err(AppError::bad_request(
            "biz_type 只能是 quotation / purchase_req / claim / receipt",
        ));
    }
    let biz_id: i64 = q
        .get("id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::bad_request("缺少 id"))?;
    let st = findb::workflow::instance_for(&db, biz_type, biz_id)?;
    Ok(Json(json!({
        "found": st.found,
        "status": st.status,
        "flow_name": st.flow_name,
        "current_label": st.current_label,
        "log": st.log,
    })))
}

/// 存货核算 ↔ 总账 对账（CostOps）：库存流水金额 vs 存货辅助余额，差异定位
pub(super) async fn gl_reconcile_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = match q
        .get("period")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(p) => parse_period(&p)
            .ok_or_else(|| AppError::bad_request("期间格式应为 202601 或 2026-01"))?,
        None => current_period(&state, &user),
    };
    let rep = findb::balances::gl_reconcile(&db, period, Some(&user.user))?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "stock_total": rep.stock_total,
        "gl_total": rep.gl_total,
        "diff_total": rep.diff_total,
        "rows": rep.rows,
    })))
}

/// 销售成本结转：借 主营业务成本(6401) / 贷 库存商品(140501，数量+存货辅助)，
/// 按计价方法计算销售发出成本。口径已在 stock_summary 收敛为 kind=sale
///（领料/形态转换/调拨不属销售成本）；本期无销售出库 → 不生成凭证；同期间防重复结转。
pub(super) async fn sales_cost_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SalesCostReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.trim().is_empty() {
        period.last_day()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let method = fincore::engine::costing::CostMethod::parse(&req.method);
    let vid = findb::business::stock_cost_voucher(
        &db,
        period,
        date,
        method,
        "6401",
        "140501",
        user.username(),
    )?;
    match vid {
        Some(id) => {
            // 结转凭证已由 stock_cost_voucher 提交；此处日志失败不能再把请求打成 500，
            // 否则客户端重试会重复结转（findb 只按期间防重，重试得到的是另一个错误）。
            if let Err(e) = db.log(
                user.username(),
                "存货",
                "结转销售成本",
                &format!("{} 凭证#{id}", period_to_str(period)),
            ) {
                error!(voucher = id, error = %e, "写操作日志失败（结转销售成本凭证）");
            }
            Ok(Json(json!({ "ok": true, "voucher_id": id })))
        }
        None => Ok(Json(json!({
            "ok": true,
            "none": true,
            "message": "本期无销售出库，未生成结转凭证"
        }))),
    }
}


pub(super) async fn list_cost_configs(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let rows = findb::business::cost_configs(&*db)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}


pub(super) async fn save_cost_method(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CostMethodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let sc = parse_money_checked(&req.standard_cost)?;
    findb::business::item_cost_method_set(&db, &req.item, Some(&req.method), sc)?;
    db.log(
        user.username(),
        "成本",
        "设置计价方式",
        &format!("{} → {}", req.item, req.method),
    )?;
    Ok(Json(json!({ "ok": true })))
}


pub(super) async fn run_period_end_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    method: axum::http::Method,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let apply = q
        .get("apply")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(false);
    // 落库动作不能藏在 GET 里：顶层导航/预取都可能带 Cookie 触发写操作
    if apply && method != axum::http::Method::POST {
        return Err(AppError::bad_request(
            "期末结价落库请使用 POST /api/cost/period-end?apply=1",
        ));
    }
    let rows = findb::business::period_end_cost(&db, period, apply)?;
    Ok(Json(
        serde_json::json!({ "period": period.label(), "rows": rows, "apply": apply }),
    ))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(serde::Deserialize)]
pub(super) struct ProdFromSoReq {
    so_id: i64,
    /// 计划数量；留空 = 下推该订单尚未认领的全部余量
    #[serde(default)]
    qty: String,
    #[serde(default)]
    date: String,
}

#[derive(Deserialize)]
pub(super) struct OverheadReq {
    period: i32,
    amount: String,
    /// cost（默认，按已归集成本）/ labor（按直接人工）/ qty（按计划产量）
    #[serde(default)]
    base: String,
    /// true = 写入归集；false = 仅试算
    #[serde(default)]
    apply: bool,
}

#[derive(Deserialize)]
pub(super) struct WfTemplateApplyReq {
    key: String,
    biz_type: String,
}

#[derive(Deserialize)]
pub(super) struct OpReportReq {
    pub op_id: i64,
    #[serde(default)]
    pub qty: String,
    #[serde(default)]
    pub hours: String,
}

#[derive(Deserialize)]
pub(super) struct OpFinishReq {
    pub op_id: i64,
}

#[derive(Deserialize)]
pub(super) struct OutsourceFeeReq {
    amount: String,
    #[serde(default)]
    date: String,
}

#[derive(Deserialize)]
pub(super) struct ProdCreateReq {
    item_code: String,
    #[serde(default)]
    qty: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    work_center: String,
    /// inhouse（默认）/ outsourcing 委外
    #[serde(default)]
    kind: String,
    #[serde(default)]
    supplier_code: String,
    #[serde(default)]
    supplier_name: String,
}

#[derive(Deserialize)]
pub(super) struct ScheduleReq {
    items: Vec<SchedItem>,
}

#[derive(Deserialize)]
pub(super) struct ProdUpdateReq {
    #[serde(default)]
    qty: String,
    /// 缺省=不变；传空串=清空计划日期
    #[serde(default)]
    plan_start: Option<String>,
    #[serde(default)]
    plan_end: Option<String>,
    /// 缺省=不变
    #[serde(default)]
    memo: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct ProdQcReq {
    qty_insp: String,
    #[serde(default)]
    qty_fail: String,
    #[serde(default)]
    disposition: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct ProdActReq {
    #[serde(default)]
    date: String,
    #[serde(default)]
    qty: String,
}

#[derive(Deserialize)]
pub(super) struct ApprovalStartReq {
    pub biz_kind: String,
    pub biz_id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub approvers: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct ApprovalActReq {
    pub approve: bool,
    #[serde(default)]
    pub comment: String,
}

#[derive(Deserialize)]
pub(super) struct CostMethodReq {
    pub item: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub standard_cost: String,
}

#[derive(Deserialize)]
pub(super) struct SalesCostReq {
    /// yyyymm，0/缺省 = 当前期间
    #[serde(default)]
    period: i32,
    /// 计价方法 code（moving_average/fifo/…，缺省移动加权）
    #[serde(default)]
    method: String,
    /// 结转日期，缺省 = 期间末日
    #[serde(default)]
    date: String,
}
