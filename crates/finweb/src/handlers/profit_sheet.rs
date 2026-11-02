//! 月份利润分析表：行维护 + 出表 + 一键生成科目。

use super::*;

#[derive(serde::Deserialize)]
pub(super) struct ProfitSheetRowReq {
    #[serde(default)]
    pub id: i64,
    pub seq: i64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub accounts: String,
    #[serde(default)]
    pub disabled: bool,
}

fn row_json(r: &findb::profit_sheet::Row) -> serde_json::Value {
    json!({
        "id": r.id,
        "seq": r.seq,
        "label": r.label,
        "hint": r.hint,
        "kind": r.kind,
        "accounts": r.accounts,
        "disabled": r.disabled,
    })
}

/// 行清单
pub(super) async fn list_profit_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let _ = &state;
    user.require(Perm::FinReport)?;
    let rows = findb::profit_sheet::list(&user.db)?;
    Ok(Json(json!({
        "rows": rows.iter().map(row_json).collect::<Vec<_>>(),
    })))
}

/// 保存一行（新增或改）
pub(super) async fn save_profit_sheet_row(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ProfitSheetRowReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _ = &state;
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let mut row = if req.id > 0 {
        findb::profit_sheet::get(db, req.id)?
            .ok_or_else(|| AppError::bad_request(format!("第 {} 行不存在", req.id)))?
    } else {
        findb::profit_sheet::Row {
            id: 0,
            seq: req.seq,
            label: String::new(),
            hint: String::new(),
            kind: findb::profit_sheet::KIND_EXPENSE.to_string(),
            accounts: String::new(),
            disabled: false,
        }
    };
    row.seq = req.seq;
    row.label = req.label.trim().to_string();
    row.hint = req.hint.trim().to_string();
    if !req.kind.trim().is_empty() {
        row.kind = req.kind.trim().to_string();
    }
    row.accounts = req.accounts.trim().to_string();
    row.disabled = req.disabled;
    let id = findb::profit_sheet::save(db, &mut row)?;
    db.log(
        user.username(),
        "利润分析表",
        "保存行",
        &format!("{}（{}）", row.seq, row.label),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "row": row_json(&row) })))
}

/// 删一行
pub(super) async fn delete_profit_sheet_row(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _ = &state;
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    findb::profit_sheet::delete(db, id)?;
    db.log(user.username(), "利润分析表", "删除行", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

/// 一键给没挂科目的行建同名末级科目
pub(super) async fn create_profit_sheet_accounts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let _ = &state;
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let (created, touched) = findb::profit_sheet::create_accounts(db)?;
    db.log(
        user.username(),
        "利润分析表",
        "按表生成科目",
        &format!("新建 {created} 个，回写 {touched} 行"),
    )?;
    Ok(Json(
        json!({ "ok": true, "created": created, "touched": touched }),
    ))
}

#[derive(serde::Deserialize)]
pub(super) struct AutoMapReq {
    /// false = 只预览（不写库）；true = 落地
    #[serde(default)]
    pub apply: bool,
}

/// 按现有科目名自动挂科目（先预览、再落地）
pub(super) async fn auto_map_profit_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AutoMapReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _ = &state;
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let r = findb::profit_sheet::auto_map(db, req.apply)?;
    if req.apply {
        db.log(
            user.username(),
            "利润分析表",
            "自动挂科目",
            &format!("改动 {} 行，没找到 {} 行", r.touched, r.unmatched.len()),
        )?;
    }
    let v = serde_json::to_value(&r).unwrap_or(serde_json::Value::Null);
    Ok(Json(v))
}

/// 恢复默认行
pub(super) async fn reset_profit_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let _ = &state;
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let n = findb::profit_sheet::reset_defaults(db)?;
    db.log(
        user.username(),
        "利润分析表",
        "恢复默认",
        &format!("{n} 行"),
    )?;
    Ok(Json(json!({ "ok": true, "rows": n })))
}

/// 出表
pub(super) async fn profit_sheet_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    let sheet = findb::profit_sheet::report(db, period)?;
    let lines = sheet
        .rows
        .iter()
        .map(|l| {
            json!({
                "seq": l.seq,
                "label": l.label,
                "hint": l.hint,
                "amount": l.amount.fmt_money(),
                "qty": l.qty.map(|q| q.fmt_qty()),
                "unmapped": l.unmapped,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": lines,
        "revenue": sheet.revenue.fmt_money(),
        "expense": sheet.expense.fmt_money(),
        "profit": sheet.profit.fmt_money(),
        "unmapped": sheet.unmapped,
    })))
}
