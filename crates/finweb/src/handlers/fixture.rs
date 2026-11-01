//! 挂具台账（挂具 / 挂篮 / 夹具：只记数量与使用次数，不进存货、不出凭证）

use super::*;

#[derive(Deserialize)]
pub struct ToolItemReq {
    pub code: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tool_kind: String,
    #[serde(default)]
    pub spec: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub life_times: i64,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize)]
pub struct ToolMoveReq {
    pub tool_code: String,
    pub kind: String,
    /// 留空 = 今天
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub customer: String,
    pub qty: String,
    #[serde(default)]
    pub times: i64,
    #[serde(default)]
    pub memo: String,
}

fn kinds_json() -> serde_json::Value {
    json!(findb::fixture::KINDS
        .iter()
        .map(|(k, l)| json!({ "code": k, "label": l }))
        .collect::<Vec<_>>())
}

fn q_param<'a>(q: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    q.get(key).map(|s| s.trim()).filter(|s| !s.is_empty())
}

/// 档案 + 结存 + 单据类型，一次给前端（页面进来就要这三样）
pub(super) async fn list_tools(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let items = findb::fixture::item_list(db)?;
    let rows = findb::fixture::balances(db)?;
    Ok(Json(
        json!({ "items": items, "rows": rows, "kinds": kinds_json() }),
    ))
}

/// 保存档案
pub(super) async fn save_tool(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ToolItemReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let mut it = findb::fixture::ToolItem {
        id: 0,
        code: req.code.trim().to_string(),
        name: req.name.trim().to_string(),
        tool_kind: req.tool_kind.trim().to_string(),
        spec: req.spec.trim().to_string(),
        unit: req.unit.trim().to_string(),
        life_times: req.life_times,
        disabled: req.disabled,
        memo: req.memo,
    };
    findb::fixture::item_save(db, &mut it)?;
    db.log(user.username(), "挂具", "保存档案", &it.code)?;
    Ok(Json(json!({ "ok": true, "item": it })))
}

/// 删除档案（有流水的会被拦）
pub(super) async fn delete_tool(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    findb::fixture::item_delete(db, &code)?;
    db.log(user.username(), "挂具", "删除档案", &code)?;
    Ok(Json(json!({ "ok": true })))
}

/// 收发流水（可按挂具编号 / 客户过滤）
pub(super) async fn list_tool_moves(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let rows = findb::fixture::moves(&user.db, q_param(&q, "tool"), q_param(&q, "customer"))?;
    Ok(Json(json!({ "moves": rows, "kinds": kinds_json() })))
}

/// 保存一条收发记录（领用/归还/报废都在这里做余额校验）
pub(super) async fn save_tool_move(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ToolMoveReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let qty = parse_money_checked(&req.qty).map_err(|_| AppError::bad_request("数量格式不正确"))?;
    let mut m = findb::fixture::ToolMove::new(req.tool_code.trim(), req.kind.trim(), date);
    m.customer = req.customer.trim().to_string();
    m.qty = qty;
    m.times = req.times;
    m.memo = req.memo;
    let id = findb::fixture::move_save(db, &mut m, user.username())?;
    db.log(
        user.username(),
        "挂具",
        "收发",
        &format!(
            "{} {} {} {}",
            m.no,
            findb::fixture::kind_label(&m.kind),
            m.tool_code,
            m.qty.fmt_qty()
        ),
    )?;
    let bal = findb::fixture::balance_of(db, &m.tool_code)?;
    Ok(Json(
        json!({ "ok": true, "id": id, "move": m, "balance": bal }),
    ))
}

/// 删除一条流水
pub(super) async fn delete_tool_move(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    findb::fixture::move_delete(db, id)?;
    db.log(user.username(), "挂具", "删除流水", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

/// 该换新的挂具（次数用完的）
pub(super) async fn tools_need_replace(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let rows = findb::fixture::need_replace(&user.db)?;
    Ok(Json(json!({ "rows": rows })))
}
