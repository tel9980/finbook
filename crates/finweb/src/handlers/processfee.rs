//! 加工费规则（按面积/件计价 + 最低收费 + 加急费）

use super::*;

#[derive(Deserialize)]
pub struct PriceRuleReq {
    #[serde(default)]
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub item_code: String,
    #[serde(default)]
    pub customer_code: String,
    #[serde(default)]
    pub unit: String,
    pub unit_price: String,
    #[serde(default)]
    pub min_charge: String,
    #[serde(default)]
    pub rush_fee: String,
    #[serde(default)]
    pub rush_rate: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub memo: String,
}

fn money_or_zero(s: &str) -> Result<Money, AppError> {
    if s.trim().is_empty() {
        return Ok(Money::ZERO);
    }
    parse_money_checked(s)
}

/// 规则列表
pub(super) async fn list_price_rules(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceView)?;
    let rules = findb::processfee::rule_list(&user.db)?;
    Ok(Json(json!({ "rules": rules })))
}

/// 新建/修改规则
pub(super) async fn save_price_rule(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PriceRuleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceEdit)?;
    let db = &user.db;
    let mut r = findb::processfee::PriceRule {
        id: req.id,
        name: req.name.trim().to_string(),
        item_code: req.item_code.trim().to_string(),
        customer_code: req.customer_code.trim().to_string(),
        unit: req.unit.trim().to_string(),
        unit_price: parse_money_checked(&req.unit_price)?,
        min_charge: money_or_zero(&req.min_charge)?,
        rush_fee: money_or_zero(&req.rush_fee)?,
        rush_rate: money_or_zero(&req.rush_rate)?,
        disabled: req.disabled,
        memo: req.memo,
    };
    let id = findb::processfee::rule_save(db, &mut r)?;
    db.log(
        user.username(),
        "销售",
        "加工费规则",
        &format!(
            "{} 单价 {}/{} 起步 {} 加急 {}+{}%",
            r.name, r.unit_price, r.unit, r.min_charge, r.rush_fee, r.rush_rate
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// 删除规则
pub(super) async fn delete_price_rule(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceEdit)?;
    let db = &user.db;
    findb::processfee::rule_delete(db, id)?;
    db.log(user.username(), "销售", "删除加工费规则", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

/// 报价：按物料/客户匹配规则，算出基础价、起步补差、加急加价与合计
pub(super) async fn quote_fee(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PriceView)?;
    let get = |k: &str| q.get(k).map(|s| s.trim()).unwrap_or("");
    let qty = parse_money_checked(get("qty"))?;
    let rush = matches!(get("rush"), "1" | "true" | "yes" | "on");
    let quote = findb::processfee::quote_by(&user.db, get("item"), get("customer"), qty, rush)?;
    Ok(Json(json!({ "quote": quote })))
}
