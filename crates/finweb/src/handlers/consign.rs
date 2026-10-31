//! 客供料台账（受托加工的客户来料：只记数量，不进存货、不出凭证）

use super::*;

#[derive(Deserialize)]
pub struct ConsignLineReq {
    pub item: String,
    #[serde(default)]
    pub item_name: String,
    #[serde(default)]
    pub unit: String,
    pub qty: String,
    #[serde(default)]
    pub batch_no: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize)]
pub struct ConsignDocReq {
    #[serde(default)]
    pub id: i64,
    pub customer: String,
    pub kind: String,
    pub date: String,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub lines: Vec<ConsignLineReq>,
}

fn kinds_json() -> serde_json::Value {
    json!(findb::consign::KINDS
        .iter()
        .map(|(k, l)| json!({ "code": k, "label": l }))
        .collect::<Vec<_>>())
}

fn customer_param(q: &HashMap<String, String>) -> Option<&str> {
    q.get("customer")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
}

/// 单据列表（可按客户过滤），顺带把单据类型清单给前端
pub(super) async fn list_consign(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let docs = findb::consign::doc_list(&user.db, customer_param(&q))?;
    Ok(Json(json!({ "docs": docs, "kinds": kinds_json() })))
}

/// 单张单据
pub(super) async fn get_consign(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let doc = findb::consign::doc_get(&user.db, id)?
        .ok_or_else(|| AppError::NotFound("客供料单不存在".to_string()))?;
    Ok(Json(json!({ "doc": doc, "kinds": kinds_json() })))
}

/// 新建/修改单据（交货类会校验现存数量）
pub(super) async fn save_consign(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ConsignDocReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let date = NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    let mut doc = findb::consign::ConsignDoc::new(req.customer.trim(), req.kind.trim(), date);
    doc.id = req.id;
    doc.memo = req.memo;
    for (i, l) in req.lines.iter().enumerate() {
        doc.lines.push(findb::consign::ConsignLine {
            id: 0,
            doc_id: 0,
            item: l.item.trim().to_string(),
            item_name: l.item_name.trim().to_string(),
            unit: l.unit.trim().to_string(),
            qty: parse_money_checked(&l.qty)
                .map_err(|_| AppError::bad_request(format!("第 {} 行数量格式不正确", i + 1)))?,
            batch_no: l.batch_no.trim().to_string(),
            memo: l.memo.clone(),
        });
    }
    let id = findb::consign::doc_save(db, &mut doc, user.username())?;
    let doc = findb::consign::doc_get(db, id)?
        .ok_or_else(|| AppError::NotFound("客供料单不存在".to_string()))?;
    db.log(
        user.username(),
        "客供料",
        "保存单据",
        &format!(
            "{} {} {} {}",
            doc.no,
            findb::consign::kind_label(&doc.kind),
            doc.customer,
            doc.lines.len()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "doc": doc })))
}

/// 删除单据
pub(super) async fn delete_consign(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    findb::consign::doc_delete(db, id)?;
    db.log(user.username(), "客供料", "删除单据", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

/// 结存（客户 + 物料 + 批次）
pub(super) async fn consign_balances(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let rows = findb::consign::balances(&user.db, customer_param(&q))?;
    Ok(Json(json!({ "rows": rows })))
}

/// 客户对账单（期间区间）
pub(super) async fn consign_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let customer = customer_param(&q)
        .ok_or_else(|| AppError::bad_request("对账单必须指定客户"))?
        .to_string();
    let cur = period_param(None, current_period(&state, &user))?;
    let from = period_param(q.get("from"), cur)?;
    let to = period_param(q.get("to"), cur)?;
    let rows = findb::consign::statement(&user.db, &customer, from, to)?;
    Ok(Json(json!({
        "customer": customer,
        "from": period_to_str(from),
        "to": period_to_str(to),
        "rows": rows,
    })))
}
