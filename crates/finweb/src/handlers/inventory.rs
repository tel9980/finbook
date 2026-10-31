//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

pub(super) async fn post_landed_cost(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let vid = findb::landedcost::post(&db, id, user.username())?;
    let after =
        findb::landedcost::get(&db, id)?.ok_or_else(|| AppError::not_found("到岸成本单不存在"))?;
    Ok(Json(json!({
        "ok": true,
        "voucher_id": vid,
        "total_to_stock": after.total_to_stock().fmt_money(),
        "total_to_expense": after.total_to_expense().fmt_money(),
        "row": lc_json(&after),
    })))
}

/// 从采购订单取可分摊的存货行
///
/// 刻意只返回**已收到货**的行：没收到的货没有附加成本可摊，摊上去就是给一份
/// 不存在的存货加成本。运费到了货还没到是常事，所以这个限制必须在取数层就做。
pub(super) async fn landed_cost_from_po(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let po_id = q
        .get("po_id")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("缺少采购订单 po_id"))?;
    let lines = findb::landedcost::fetch_lines_from_po(&db, po_id)?;
    Ok(Json(json!({
        "po_id": po_id,
        "rows": lines.iter().map(|l| json!({
            "po_id": l.po_id,
            "po_no": l.po_no,
            "item_code": l.item_code,
            "item_name": l.item_name,
            "qty": l.qty.fmt_qty(),
            "weight": l.weight.fmt_qty(),
            "amount": l.amount.fmt_money(),
        })).collect::<Vec<_>>(),
        "note": "只列出已收货的存货行 —— 未收到的货没有附加成本可摊",
    })))
}

pub(super) async fn items_master(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    Ok(Json(
        json!({ "rows": findb::inventory2::item_master(&*db)? }),
    ))
}

/// 单条出入库流水（差异分析点行看构成明细）
pub(super) async fn get_stock_move(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let m = findb::business::stock_move_get(&db, id)?
        .ok_or_else(|| AppError::not_found(format!("出入库流水不存在：{id}")))?;
    Ok(Json(json!({
        "id": m.id,
        "biz_date": m.biz_date.to_string(),
        "kind": m.kind.code(),
        "kind_label": m.kind.label(),
        "item": m.item,
        "qty": m.qty.fmt_qty(),
        "price": m.price.fmt_money(),
        "amount": m.amount.fmt_money(),
        "memo": m.memo,
    })))
}

pub(super) async fn stock_adjust_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<StockAdjustReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    if req.item.trim().is_empty() {
        return Err(AppError::bad_request("缺少存货 item"));
    }
    let delta = parse_money_checked(&req.delta)?;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let qty = parse_money_checked(&req.qty)?;
    let id = findb::business::stock_adjust_full(
        &db,
        period,
        date,
        &req.item,
        &req.warehouse,
        qty,
        delta,
        &req.memo,
    )?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "qty": qty, "delta": delta }),
    ))
}

pub(super) async fn serial_out_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SerialOutReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let n = findb::inventory2::serial_out(&db, &req.serials, date)?;
    Ok(Json(serde_json::json!({ "ok": true, "count": n })))
}

pub(super) async fn get_unit(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let item = q.get("item").cloned().unwrap_or_default();
    let db = &user.db;
    let u = findb::inventory2::unit_get(&db, &item)?;
    Ok(Json(serde_json::json!({ "unit": u })))
}

pub(super) async fn set_unit(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<UnitReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::inventory2::unit_set(
        &db,
        &findb::inventory2::ItemUnit {
            item: req.item,
            base_unit: req.base_unit,
            alt_unit: req.alt_unit,
            factor: parse_money_checked(&req.factor)?,
        },
    )?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn get_inv_aging(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::inventory2::inv_aging(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn get_abc(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::inventory2::abc_analysis(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn assemble_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssembleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let mut children: Vec<(String, Money)> = Vec::with_capacity(req.children.len());
    for (i, q) in req.children {
        children.push((i, parse_money_checked(&q)?));
    }
    findb::inventory2::assemble(&db, period, date, &req.parent, &children, &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn disassemble_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssembleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let mut children: Vec<(String, Money)> = Vec::with_capacity(req.children.len());
    for (i, q) in req.children {
        children.push((i, parse_money_checked(&q)?));
    }
    findb::inventory2::disassemble(&db, period, date, &req.parent, &children, &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn get_warehouse_stock(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let item = q.get("item").cloned().unwrap_or_default();
    if item.is_empty() {
        return Err(AppError::bad_request("缺少 item"));
    }
    let db = &user.db;
    let rows = findb::inventory2::warehouse_stock(&db, &item)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

// ---- 仓库主数据（v30） ----

/// 仓库档案列表（读：Report；默认仓在前）
pub(super) async fn list_warehouses(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::warehouse::list(&*db)? })))
}

/// 新增/修改仓库（写：Warehouse；设默认仓自动清其它默认标记）
pub(super) async fn save_warehouse(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(w): Json<findb::warehouse::Warehouse>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::warehouse::save(&db, &w)?;
    db.log(
        user.username(),
        "库存",
        "仓库档案",
        &format!("{} {}（默认={}）", w.code, w.name, w.is_default),
    )?;
    Ok(Json(json!({ "ok": true })))
}

/// 删除仓库：默认仓不可删、被流水/盘点引用不可删
pub(super) async fn delete_warehouse(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::warehouse::delete(&db, &code)?;
    db.log(user.username(), "库存", "删除仓库", &code)?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn get_routing(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let ops = advanced::routing_list(&db, &item)?;
    Ok(Json(serde_json::json!({ "item_code": item, "ops": ops })))
}

pub(super) async fn post_routing(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
    Json(req): Json<Vec<RoutingOpDto>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let mut ops: Vec<advanced::RoutingOp> = Vec::with_capacity(req.len());
    for d in req {
        ops.push(advanced::RoutingOp {
            id: 0,
            item_code: item.clone(),
            version: String::new(),
            seq: d.seq,
            op_code: d.op_code,
            op_name: d.op_name,
            work_center: d.work_center,
            std_hours: parse_money_checked(&d.std_hours)?,
            rate: parse_money_checked(&d.rate)?,
            qc_required: d.qc_required,
        });
    }
    advanced::routing_save(&db, &item, &ops)?;
    Ok(Json(serde_json::json!({ "ok": true, "count": ops.len() })))
}

pub(super) async fn delete_routing(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    advanced::routing_delete(&db, &item)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- MRP ----

pub(super) async fn get_mrp_latest(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let rows = advanced::mrp_latest(&*db)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn run_mrp(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<MrpRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let mut demands: Vec<(String, Money, String)> = Vec::with_capacity(req.demands.len());
    for d in req.demands {
        demands.push((d.item_code, parse_money_checked(&d.qty)?, d.source));
    }
    if demands.is_empty() && req.from_sales {
        demands = advanced::mrp_demands_from_sales(&db, current_period(&state, &user))?;
    }
    if demands.is_empty() {
        return Err(AppError::bad_request(
            "请提供需求清单，或勾选「从销售订单收集」",
        ));
    }
    let run_at = advanced::mrp_run(&db, &demands)?;
    let rows = advanced::mrp_by_run(&db, &run_at)?;
    Ok(Json(serde_json::json!({ "run_at": run_at, "rows": rows })))
}

/// MRP 采购建议下推请购单（草稿，幂等）
pub(super) async fn mrp_to_req_ep(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    if findb::advanced::mrp_get(&db, id)?.is_none() {
        return Err(AppError::not_found("MRP 结果行不存在"));
    }
    let (req_id, no) = findb::advanced::mrp_to_req(&db, id, user.username())?;
    db.log(
        user.username(),
        "采购",
        "MRP 下推请购",
        &format!("MRP#{id} → 请购单 {no}"),
    )?;
    Ok(Json(json!({ "ok": true, "req_id": req_id, "no": no })))
}

/// 形态转换：源物料出库 → 目标物料入库（同数量，数量口径，无总账凭证）
pub(super) async fn form_convert_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<FormConvertReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let qty = parse_money_checked(&req.qty)?;
    findb::inventory2::form_convert(
        &db,
        period,
        date,
        req.from_item.trim(),
        req.to_item.trim(),
        qty,
        &req.memo,
    )?;
    db.log(
        user.username(),
        "库存",
        "形态转换",
        &format!("{} → {} ×{}", req.from_item, req.to_item, qty.fmt_qty()),
    )?;
    Ok(Json(json!({ "ok": true })))
}

/// 质检单：合格留库；不合格**自动按订单单价退货**（负到货 + 负采购入库同事务）
pub(super) async fn qc_order_ep(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<QcReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let insp = parse_money_checked(&req.qty_insp)?;
    let fail = if req.qty_fail.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&req.qty_fail)?
    };
    let inspector = if req.inspector.trim().is_empty() {
        user.username().to_string()
    } else {
        req.inspector.trim().to_string()
    };
    let id = findb::procurement::qc_save(
        &db,
        req.po_id,
        insp,
        fail,
        &inspector,
        date,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "库存",
        "质检",
        &format!(
            "PO#{} 检验 {} 合格 {} 不合格 {}",
            req.po_id,
            insp.fmt_qty(),
            (insp - fail).fmt_qty(),
            fail.fmt_qty()
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "qty_pass": (insp - fail).fmt_qty(),
        "qty_fail": fail.fmt_qty(),
    })))
}

/// 低于安全库存（item_plan 安全量 vs 现有库存）
pub(super) async fn below_safety_ep(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let rows = findb::inventory2::below_safety(&*db)?;
    Ok(Json(json!({
        "rows": rows
            .iter()
            .map(|(i, on, s)| json!({ "item": i, "on_hand": on.fmt_qty(), "safety": s.fmt_qty() }))
            .collect::<Vec<_>>()
    })))
}

/// MPS 运行：需求聚合（销售未发量 + 手工行）→ 净算计划量（扣现有库存与在制）
pub(super) async fn run_mps(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<MpsRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let mut extra: Vec<(String, Money, String)> = Vec::with_capacity(req.demands.len());
    for d in &req.demands {
        extra.push((
            d.item_code.clone(),
            parse_money_checked(&d.qty)?,
            d.source.clone(),
        ));
    }
    if req.from_sales {
        extra.extend(advanced::mrp_demands_from_sales(
            &db,
            current_period(&state, &user),
        )?);
    }
    if extra.is_empty() {
        return Err(AppError::bad_request(
            "请提供手工需求，或勾选「从销售订单收集」",
        ));
    }
    let due = if req.due_date.trim().is_empty() {
        (chrono::Local::now().date_naive() + chrono::Duration::days(7))
            .format("%Y-%m-%d")
            .to_string()
    } else {
        req.due_date.trim().to_string()
    };
    // 销售需求已在 handler 合并，findb 侧不再取（sales_period=0）
    let run_at = advanced::mps_run(&db, &extra, Period::from_ymm(0), &due)?;
    let rows = advanced::mps_latest(&*db)?;
    db.log(
        user.username(),
        "生产",
        "MPS 运算",
        &format!("{} 行，建议交期 {due}", rows.len()),
    )?;
    Ok(Json(
        json!({ "run_at": run_at, "due_date": due, "rows": rows }),
    ))
}

/// MPS 最近一次结果
pub(super) async fn get_mps_latest(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": advanced::mps_latest(&*db)? })))
}

/// MPS 下达：状态 open → converted 并一键生成已下达生产订单（数量可改）
pub(super) async fn convert_mps(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<MpsConvertReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let q = if req.qty.trim().is_empty() {
        None
    } else {
        Some(parse_money_checked(&req.qty)?)
    };
    let (oid, no) =
        advanced::mps_convert_to_order(&db, id, q, current_period(&state, &user), user.username())?;
    db.log(
        user.username(),
        "生产",
        "MPS 下达",
        &format!("行 #{id} → 生产订单 {no}"),
    )?;
    Ok(Json(json!({ "ok": true, "order_id": oid, "order_no": no })))
}

/// 粗排：件/日产能顺排（v1 口径——工艺路线暂无标准工时，工时口径留待迭代）
pub(super) async fn rough_mps(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<RoughReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let daily = parse_money_checked(if req.daily_qty.trim().is_empty() {
        "10"
    } else {
        req.daily_qty.trim()
    })?;
    let (orders, load) = advanced::rough_schedule(&db, period, daily)?;
    Ok(Json(
        json!({ "orders": orders, "load": load, "daily_qty": daily }),
    ))
}

/// BOM 查询（报工页「维护BOM」回显）
pub(super) async fn get_bom_ep(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let parent = q.get("parent").map(|s| s.trim()).unwrap_or("");
    if parent.is_empty() {
        return Err(AppError::bad_request("缺少 parent（父件编码）"));
    }
    let rows = findb::scm::bom_list(&db, parent)?;
    Ok(Json(json!({
        "rows": rows
            .iter()
            .map(|b| json!({
                "child_code": b.child_code,
                "qty": b.qty.fmt_qty(),
                "loss_rate": b.loss_rate.fmt_qty(),
            }))
            .collect::<Vec<_>>()
    })))
}

/// BOM 保存（覆盖当前版本；领料与 MRP 均按 BOM 展开）
pub(super) async fn save_bom_ep(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BomReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let parent = req.parent.trim();
    if parent.is_empty() {
        return Err(AppError::bad_request("缺少父件编码"));
    }
    let mut items: Vec<(String, Money, Money)> = Vec::new();
    for c in &req.children {
        let code = c.child.trim();
        if code.is_empty() {
            continue;
        }
        let qty = parse_money_checked(&c.qty)?;
        if !qty.is_positive() {
            return Err(AppError::bad_request(format!("子件 {code} 用量必须大于 0")));
        }
        let loss = if c.loss.trim().is_empty() {
            Money::ZERO
        } else {
            parse_money_checked(&c.loss)?
        };
        items.push((code.to_string(), qty, loss));
    }
    if items.is_empty() {
        return Err(AppError::bad_request("至少一个子件"));
    }
    findb::scm::bom_save(&db, parent, &items)?;
    db.log(
        user.username(),
        "生产",
        "维护BOM",
        &format!("{parent}：{}个子件", items.len()),
    )?;
    Ok(Json(json!({ "ok": true, "children": items.len() })))
}

// ---- 会计电子档案 ----

pub(super) async fn list_archives(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    // 空串表示"全部"：UI 下拉默认值为空，不能当成具体类型去过滤
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let db = &user.db;
    let rows = advanced::archive_list(&db, period, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn create_archive(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ArchiveReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = period_checked(req.period)?;
    let file_no = if req.file_no.trim().is_empty() {
        advanced::archive_next_no(&db, period, &req.kind)?
    } else {
        req.file_no
    };
    let id = advanced::archive_create(
        &db,
        period,
        &req.kind,
        &req.title,
        &file_no,
        &req.payload,
        user.username(),
    )?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "file_no": file_no }),
    ))
}

pub(super) async fn get_archive(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let a = advanced::archive_get(&db, id)?
        .ok_or_else(|| AppError::NotFound("档案不存在".to_string()))?;
    Ok(Json(serde_json::json!({ "archive": a })))
}

pub(super) async fn verify_archive(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let a = advanced::archive_get(&db, id)?
        .ok_or_else(|| AppError::NotFound("档案不存在".to_string()))?;
    let ok = advanced::archive_verify(&a);
    Ok(Json(
        serde_json::json!({ "ok": ok, "content_hash": a.content_hash }),
    ))
}

/// 盘点单列表（含明细：账面快照 vs 实盘）
pub(super) async fn list_counts(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::stocktake::count_list(&*db)? })))
}

/// 新建盘点单：服务端按仓库快照账面数量（Warehouse = 仓管作业）
pub(super) async fn create_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let mut lines: Vec<(String, String, Money, String)> = Vec::new();
    for l in &req.lines {
        if l.item.trim().is_empty() {
            continue;
        }
        lines.push((
            l.item.trim().to_string(),
            l.batch_no.trim().to_string(),
            parse_money_checked(&l.count_qty)?,
            l.memo.clone(),
        ));
    }
    let (id, no) = findb::stocktake::count_create(
        &db,
        period,
        date,
        req.warehouse.trim(),
        &req.memo,
        &lines,
        user.username(),
    )?;
    db.log(
        user.username(),
        "库存",
        "新建盘点单",
        &format!("#{no} {} 行", lines.len()),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "no": no })))
}

/// 应用盘点单：生成其他入库/出库流水 + 盘盈盘亏凭证（金额=差异×标准价）
pub(super) async fn apply_count(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let (rid, vid, value) = findb::stocktake::count_apply(&db, id, user.username())?;
    db.log(
        user.username(),
        "库存",
        "应用盘点",
        &format!(
            "#{rid} 价值 {} {}",
            value.fmt_money(),
            vid.map(|v| format!("凭证 #{v}"))
                .unwrap_or_else(|| "未出凭证".into())
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": rid,
        "voucher_id": vid,
        "value": value.fmt_money(),
        "message": if vid.is_some() { "" } else { "未配置标准价，仅调整库存流水" },
    })))
}

pub(super) async fn delete_count(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::stocktake::count_delete(&db, id)?;
    db.log(user.username(), "库存", "删除盘点单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 批次出入登记：批号空=自动 BT+日期+序号；同事务写库存流水（带批号，与普通库存同一本账）
pub(super) async fn register_batch(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BatchRegisterReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let direction = if req.direction == "out" { "out" } else { "in" };
    // 期间取**账套当前期间**，与 stock_adjust / 盘点 / 采购到货同一口径。
    // 曾经传的是 batch_register 内部自己算的「真实当天」，于是账套停在 2026-01
    // 而今天 2026-10 时，批次流水落在 202610：批次台账能看到余额
    //（它按 (item,batch) 汇总、不过滤期间），按期间查的数量金额账却看不到。
    let period = current_period(&state, &user);
    let (id, no, bal) = findb::batch::batch_register(
        &db,
        period,
        &req.item,
        &req.batch_no,
        &req.production_date,
        req.warehouse.trim(),
        req.location.trim(),
        parse_money_checked(&req.qty)?,
        direction,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "库存",
        if direction == "in" {
            "批次入库"
        } else {
            "批次出库"
        },
        &format!("{} {} ×{} 余额 {}", req.item, no, req.qty, bal.fmt_qty()),
    )?;
    Ok(Json(
        json!({ "ok": true, "id": id, "batch_no": no, "balance": bal.fmt_qty() }),
    ))
}

/// 批次成本勾稽（CostOps）：批次层价值 vs 存货辅助账期末，逐存货差异
pub(super) async fn batch_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let to = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let (detail, totals) = findb::inventory2::batch_cost_report(&db, to)?;
    Ok(Json(
        json!({ "detail": detail, "totals": totals, "period": to.ymm() }),
    ))
}

/// 批次列表（含余额；?item= 过滤）
pub(super) async fn list_batches(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let item = q.get("item").cloned().unwrap_or_default();
    Ok(Json(
        json!({ "rows": findb::batch::batch_list(&db, &item)? }),
    ))
}

/// FEFO 推荐（近效期先出）
pub(super) async fn fefo_batches(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<FefoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    if req.item.trim().is_empty() {
        return Err(AppError::bad_request("缺少存货编码 item"));
    }
    let qty = parse_money_checked(&req.qty)?;
    let rows = findb::batch::fefo_recommend(&db, &req.item, qty)?;
    Ok(Json(json!({
        "rows": rows
            .iter()
            .map(|(b, e, t)| json!({ "batch_no": b, "expiry_date": e, "take": t.fmt_qty() }))
            .collect::<Vec<_>>()
    })))
}

/// 临期批次（默认30天窗口）
pub(super) async fn expiring_batches_ep(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let days = q
        .get("days")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(30);
    Ok(Json(
        json!({ "rows": findb::batch::expiring_batches(&db, days)? }),
    ))
}

pub(super) async fn delete_location(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::batch::location_delete(&db, id)?;
    db.log(user.username(), "库存", "删除库位", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 存货计划参数读取（编辑器回显）：安全库存 / 前置期 / 批量
pub(super) async fn get_item_plan(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let code = q
        .get("item")
        .map(String::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少 item 存货编码"));
    }
    let db = &user.db;
    let p = findb::advanced::item_plan_get(&db, &code)?;
    Ok(Json(json!({
        "plan": p.as_ref().map(|p| json!({
            "safety_stock": p.safety_stock.fmt_qty(),
            "lead_days": p.lead_days,
            "lot_size": p.lot_size.fmt_qty(),
        })),
    })))
}

/// 存货计划参数保存（补齐「低库存预警 ↔ 设置入口」断链——迁移/建档配套）
pub(super) async fn save_item_plan(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ItemPlanReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let code = req.item_code.trim().to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少 item_code"));
    }
    let db = &user.db;
    findb::advanced::item_plan_upsert(
        &db,
        &findb::advanced::ItemPlan {
            item_code: code.clone(),
            safety_stock: parse_money_checked(&req.safety_stock)?,
            lead_days: req.lead_days.max(0),
            lot_size: parse_money_checked(&req.lot_size)?,
        },
    )?;
    db.log(
        user.username(),
        "档案",
        "存货计划参数",
        &format!(
            "{code} 安全库存 {} 前置期 {} 批量 {}",
            req.safety_stock, req.lead_days, req.lot_size
        ),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn list_landed_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let rows = findb::landedcost::list(&db, period)?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": rows.iter().map(lc_json).collect::<Vec<_>>(),
        "total_charges": rows.iter().map(|r| r.total_charges()).sum::<fincore::Money>().fmt_money(),
        "total_to_stock": rows.iter().map(|r| r.total_to_stock()).sum::<fincore::Money>().fmt_money(),
        "total_to_expense": rows.iter().map(|r| r.total_to_expense()).sum::<fincore::Money>().fmt_money(),
    })))
}

pub(super) async fn save_landed_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<LcSaveReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.trim().is_empty() {
        period.first_day().format("%Y-%m-%d").to_string()
    } else {
        req.date.trim().to_string()
    };
    let mut lines = Vec::new();
    for l in &req.lines {
        lines.push(findb::landedcost::LcLine {
            po_id: l.po_id,
            item_code: l.item_code.trim().to_string(),
            item_name: l.item_name.trim().to_string(),
            qty: parse_money_checked(&l.qty)?,
            weight: parse_money_checked(&l.weight)?,
            amount: parse_money_checked(&l.amount)?,
            manual_amount: parse_money_checked(&l.manual_amount)?,
            memo: l.memo.clone(),
            ..Default::default()
        });
    }
    // 借方/贷方科目：留空则退到账套的存货科目与资金账户
    let biz = db.options().biz_accounts.clone();
    let mut lc = findb::landedcost::LandedCost {
        id: req.id,
        no: String::new(),
        period: period.ymm(),
        date: date.clone(),
        supplier_code: req.supplier_code.trim().to_string(),
        supplier_name: req.supplier_name.trim().to_string(),
        basis: if req.basis.trim().is_empty() {
            "amount".to_string()
        } else {
            req.basis.trim().to_string()
        },
        freight: parse_money_checked(&req.freight)?,
        duty: parse_money_checked(&req.duty)?,
        insurance: parse_money_checked(&req.insurance)?,
        clearing: parse_money_checked(&req.clearing)?,
        other: parse_money_checked(&req.other)?,
        dr_account: if req.dr_account.trim().is_empty() {
            biz.material.clone()
        } else {
            req.dr_account.trim().to_string()
        },
        cr_account: if req.cr_account.trim().is_empty() {
            biz.fund.clone()
        } else {
            req.cr_account.trim().to_string()
        },
        expense_account: req.expense_account.trim().to_string(),
        bank_account: req.bank_account.trim().to_string(),
        status: "draft".to_string(),
        voucher_id: None,
        memo: req.memo.clone(),
        prepared_by: user.username().to_string(),
        created_at: String::new(),
        updated_at: String::new(),
        lines: Vec::new(),
    };
    let id = findb::landedcost::save(&db, &mut lc, &lines, user.username())?;
    let after =
        findb::landedcost::get(&db, id)?.ok_or_else(|| AppError::not_found("到岸成本单不存在"))?;
    Ok(Json(
        json!({ "ok": true, "id": id, "row": lc_json(&after) }),
    ))
}

pub(super) async fn serial_in_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SerialInReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let n = findb::inventory2::serial_in(&db, &req.item, &req.serials, &req.batch_no, date)?;
    Ok(Json(serde_json::json!({ "ok": true, "count": n })))
}

pub(super) async fn list_serial(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let item = q.get("item").cloned().unwrap_or_default();
    if item.is_empty() {
        return Err(AppError::bad_request("缺少 item"));
    }
    let db = &user.db;
    let rows = findb::inventory2::serial_list(&db, &item)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn get_transfer_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::inventory2::transfer_report(&db, current_period(&state, &user))?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|m| {
            serde_json::json!({
                "date": m.biz_date.format("%Y-%m-%d").to_string(),
                "item": m.item,
                "batch_no": m.batch_no,
                "warehouse": m.warehouse,
                "qty": m.qty.fmt_qty(),
                "memo": m.memo,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "rows": items })))
}

/// 批次调拨（Warehouse）：batch_no 空 → FEFO 近效期自动选批（首条不足量 400 提示分批）
pub(super) async fn do_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<TransferReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.trim().is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let qty = parse_money_checked(&req.qty)?;
    let mut bn = req.batch_no.trim().to_string();
    if bn.is_empty() {
        let rec = findb::batch::fefo_recommend(&db, &req.item, qty)?;
        let first = rec
            .first()
            .ok_or_else(|| AppError::not_found("该存货没有可用批次"))?;
        if first.2 < qty {
            return Err(AppError::bad_request(format!(
                "近效期批次仅余 {}，不足调拨 {}——请分批调拨或指定批号",
                first.2.fmt_qty(),
                qty.fmt_qty()
            )));
        }
        bn = first.0.clone();
    }
    let (out_id, in_id) = findb::inventory2::transfer_do(
        &db,
        period,
        date,
        &req.item,
        &bn,
        &req.from_warehouse,
        &req.to_warehouse,
        qty,
        &req.memo,
    )?;
    db.log(
        user.username(),
        "库存",
        "批次调拨",
        &format!(
            "{} {} {}→{} ×{}（流水 {out_id}/{in_id}）",
            req.item,
            bn,
            req.from_warehouse,
            req.to_warehouse,
            qty.fmt_qty()
        ),
    )?;
    Ok(Json(
        json!({ "ok": true, "out_id": out_id, "in_id": in_id, "batch_no": bn }),
    ))
}

pub(super) async fn list_locations(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::batch::location_list(&*db)? })))
}

pub(super) async fn save_location(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<LocationReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let kind = if req.kind.is_empty() {
        "storage"
    } else {
        &req.kind
    };
    let id = findb::batch::location_save(&db, &req.code, &req.name, kind, &req.memo)?;
    db.log(
        user.username(),
        "库存",
        "保存库位",
        &format!("{} {}", req.code, req.name),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(serde::Deserialize)]
pub(super) struct LcSaveReq {
    #[serde(default)]
    id: i64,
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    supplier_code: String,
    #[serde(default)]
    supplier_name: String,
    #[serde(default)]
    basis: String,
    #[serde(default)]
    freight: String,
    #[serde(default)]
    duty: String,
    #[serde(default)]
    insurance: String,
    #[serde(default)]
    clearing: String,
    #[serde(default)]
    other: String,
    #[serde(default)]
    dr_account: String,
    #[serde(default)]
    cr_account: String,
    /// 付款账户（贷方核算银行账户时必填）
    #[serde(default)]
    bank_account: String,
    #[serde(default)]
    expense_account: String,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    lines: Vec<LcLineReq>,
}

#[derive(Deserialize)]
pub(super) struct StockAdjustReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub item: String,
    #[serde(default)]
    pub warehouse: String,
    /// 调整数量（正=盘盈/调增，负=盘亏/调减）。**建账与盘盈必填**：
    /// 只给金额不给数量会造出「数量 0、金额 > 0」的幽灵存货。
    #[serde(default)]
    pub qty: String,
    /// 调整金额（正=调增，负=调减），十进制字符串
    pub delta: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize)]
pub(super) struct SerialInReq {
    pub item: String,
    pub serials: Vec<String>,
    #[serde(default)]
    pub batch_no: String,
    #[serde(default)]
    pub date: String,
}

#[derive(Deserialize)]
pub(super) struct SerialOutReq {
    pub serials: Vec<String>,
    #[serde(default)]
    pub date: String,
}

#[derive(Deserialize)]
pub(super) struct UnitReq {
    pub item: String,
    #[serde(default)]
    pub base_unit: String,
    #[serde(default)]
    pub alt_unit: String,
    #[serde(default)]
    pub factor: String,
}

#[derive(Deserialize)]
pub(super) struct AssembleReq {
    pub parent: String,
    pub children: Vec<(String, String)>,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub date: String,
}

#[derive(Deserialize)]
pub(super) struct RoutingOpDto {
    #[serde(default)]
    pub seq: i32,
    pub op_code: String,
    pub op_name: String,
    #[serde(default)]
    pub work_center: String,
    #[serde(default)]
    pub std_hours: String,
    #[serde(default)]
    pub rate: String,
    /// 工序检验点（完工前需录工序检验单）
    #[serde(default)]
    pub qc_required: bool,
}

#[derive(Deserialize)]
pub(super) struct MrpRunReq {
    #[serde(default)]
    pub demands: Vec<MrpDemandDto>,
    /// 若为 true 且 demands 为空，则从已确认销售订单收集需求
    #[serde(default)]
    pub from_sales: bool,
}

#[derive(Deserialize)]
pub(super) struct FormConvertReq {
    from_item: String,
    to_item: String,
    qty: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct QcReq {
    po_id: i64,
    qty_insp: String,
    #[serde(default)]
    qty_fail: String,
    #[serde(default)]
    inspector: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct MpsRunReq {
    #[serde(default)]
    pub demands: Vec<MrpDemandDto>,
    /// true = 合并已确认销售订单未发量（与手工行并集）
    #[serde(default)]
    pub from_sales: bool,
    /// 建议交期（默认今天 +7）
    #[serde(default)]
    pub due_date: String,
}

#[derive(Deserialize)]
pub(super) struct MpsConvertReq {
    /// 空 = 用行净算计划量
    #[serde(default)]
    qty: String,
}

#[derive(Deserialize)]
pub(super) struct RoughReq {
    /// 日产能（件/日），空 = 10
    #[serde(default)]
    daily_qty: String,
    #[serde(default)]
    period: i32,
}

#[derive(Deserialize)]
pub(super) struct BomReq {
    parent: String,
    #[serde(default)]
    children: Vec<BomChildReq>,
}

#[derive(Deserialize)]
pub(super) struct ArchiveReq {
    pub period: i32,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub file_no: String,
    pub payload: String,
}

#[derive(Deserialize)]
pub(super) struct CountReq {
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    warehouse: String,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    lines: Vec<CountLineInput>,
}

#[derive(Deserialize)]
pub(super) struct BatchRegisterReq {
    #[serde(default)]
    item: String,
    #[serde(default)]
    batch_no: String,
    #[serde(default)]
    production_date: String,
    #[serde(default)]
    warehouse: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    qty: String,
    #[serde(default)]
    direction: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct TransferReq {
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    item: String,
    #[serde(default)]
    batch_no: String,
    #[serde(default)]
    from_warehouse: String,
    #[serde(default)]
    to_warehouse: String,
    #[serde(default)]
    qty: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct FefoReq {
    #[serde(default)]
    item: String,
    #[serde(default)]
    qty: String,
}

#[derive(Deserialize)]
pub(super) struct LocationReq {
    #[serde(default)]
    code: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct ItemPlanReq {
    #[serde(default)]
    item_code: String,
    #[serde(default)]
    safety_stock: String,
    #[serde(default)]
    lead_days: i32,
    #[serde(default)]
    lot_size: String,
}
