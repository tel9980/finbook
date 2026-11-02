//! 返工件成本单（返工单独归集材料/人工/制造费用，按「返工成本 ÷ 原单成本」看占比）

use super::*;

#[derive(Deserialize)]
pub struct ReworkReq {
    #[serde(default)]
    pub id: i64,
    pub po_id: i64,
    /// 留空 = 今天
    #[serde(default)]
    pub date: String,
    pub qty: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize)]
pub struct ReworkCostReq {
    pub cost_type: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}

fn kinds_json() -> serde_json::Value {
    json!(findb::rework::COST_KINDS
        .iter()
        .map(|(k, l)| json!({ "code": k, "label": l }))
        .collect::<Vec<_>>())
}

fn statuses_json() -> serde_json::Value {
    json!(findb::rework::STATUSES
        .iter()
        .map(|(k, l)| json!({ "code": k, "label": l }))
        .collect::<Vec<_>>())
}

/// 日期：留空 = 今天（界面上默认填今天，接口也要容忍不填）
fn date_or_today(s: &str) -> Result<NaiveDate, AppError> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(chrono::Local::now().date_naive());
    }
    NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))
}

/// 期间内的返工单列表（页面进来先要「口径下拉 + 列表」）
pub(super) async fn list_rework(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let p = period_param(q.get("period"), current_period(&state, &user))?;
    let orders = findb::rework::list(&user.db, Some(p))?;
    Ok(Json(json!({
        "period": p.label(),
        "orders": orders,
        "kinds": kinds_json(),
        "statuses": statuses_json(),
    })))
}

/// 单张返工单的明细（订单 + 成本明细 + 三段汇总）
pub(super) async fn get_rework(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let d = findb::rework::detail(&user.db, id)?
        .ok_or_else(|| AppError::not_found(format!("返工单 {id} 不存在")))?;
    Ok(Json(json!({
        "order": d.order,
        "costs": d.costs,
        "material": d.material,
        "labor": d.labor,
        "overhead": d.overhead,
        "total": d.total,
        "kinds": kinds_json(),
    })))
}

/// 新建/修改返工单
pub(super) async fn save_rework(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReworkReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let date = date_or_today(&req.date)?;
    let mut o = if req.id > 0 {
        findb::rework::get(db, req.id)?
            .ok_or_else(|| AppError::not_found(format!("返工单 {} 不存在", req.id)))?
    } else {
        findb::rework::ReworkOrder::new(req.po_id, date)
    };
    o.po_id = req.po_id;
    o.date = date;
    o.period = fincore::Period::from_date(date);
    o.qty = parse_money_checked(&req.qty)?;
    o.reason = req.reason.trim().to_string();
    o.status = if req.status.trim().is_empty() {
        "open".to_string()
    } else {
        req.status.trim().to_string()
    };
    o.memo = req.memo.trim().to_string();
    let id = findb::rework::save(db, &mut o, user.username())?;
    db.log(
        user.username(),
        "生产",
        "返工单",
        &format!(
            "{} 来源 {} 数量 {}（{}）",
            o.no,
            o.po_no,
            o.qty.fmt_qty(),
            o.status_label()
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "no": o.no,
        "po_no": o.po_no,
        "item_code": o.item_code,
        "total": findb::rework::total(db, id)?,
    })))
}

/// 删除返工单（连同成本明细；已结案的会被拦下）
pub(super) async fn delete_rework(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    findb::rework::delete(db, id)?;
    db.log(user.username(), "生产", "返工单", &format!("删除 {id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 追加一条成本
pub(super) async fn add_rework_cost(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ReworkCostReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let amount = parse_money_checked(&req.amount)?;
    let cost_id = findb::rework::add_cost(db, id, req.cost_type.trim(), amount, req.memo.trim())?;
    db.log(
        user.username(),
        "生产",
        "返工成本",
        &format!(
            "返工单 {id} {} {}",
            findb::rework::cost_kind_label(req.cost_type.trim()),
            amount
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": cost_id,
        "total": findb::rework::total(db, id)?,
    })))
}

/// 删除一条成本
pub(super) async fn delete_rework_cost(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    findb::rework::delete_cost(db, id)?;
    db.log(user.username(), "生产", "返工成本", &format!("删除 {id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 返工成本报表（区间内每张单的三段成本 + 占原单成本的比例）
pub(super) async fn rework_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let p = current_period(&state, &user);
    let from = period_param(q.get("from").or_else(|| q.get("period")), p)?;
    let to = period_param(q.get("to").or_else(|| q.get("period")), from)?;
    if from > to {
        return Err(AppError::bad_request(format!(
            "起始期间 {} 不能晚于结束期间 {}",
            from.label(),
            to.label()
        )));
    }
    let rows = findb::rework::report(&user.db, from, to)?;
    let mut total = fincore::Money::ZERO;
    for r in &rows {
        total += r.total;
    }
    Ok(Json(json!({
        "from": from.label(),
        "to": to.label(),
        "rows": rows,
        "total": total,
    })))
}
