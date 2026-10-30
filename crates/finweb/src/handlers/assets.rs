use super::*;

pub(super) async fn list_assets(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let cards: Vec<serde_json::Value> = findb::assets::list(&*db)?.iter().map(asset_json).collect();
    let ledger: Vec<serde_json::Value> = findb::assets::ledger(&db, period)?
        .iter()
        .map(|r| {
            json!({
                "asset": asset_json(&r.asset),
                "accum": r.accum.fmt_money(),
                "net": r.net.fmt_money(),
                "months": r.months,
            })
        })
        .collect();
    let plan: Vec<serde_json::Value> = findb::assets::dep_plan(&db, period)?
        .iter()
        .map(|p| {
            json!({
                "asset_id": p.asset_id, "code": p.code, "name": p.name, "dept": p.dept,
                "expense_account": p.expense_account, "dep_account": p.dep_account,
                "amount": p.amount.fmt_money(), "accum": p.accum.fmt_money(), "net": p.net.fmt_money(),
            })
        })
        .collect();
    let deps: Vec<serde_json::Value> = findb::assets::dep_list_period(&db, period)?
        .iter()
        .map(|d| {
            json!({
                "asset_id": d.asset_id, "period": period_to_str(d.period),
                "amount": d.amount.fmt_money(), "accum": d.accum.fmt_money(),
                "net_value": d.net_value.fmt_money(), "voucher_id": d.voucher_id,
            })
        })
        .collect();
    Ok(Json(json!({
        "period": period_to_str(period),
        "cards": cards,
        "ledger": ledger,
        "plan": plan,
        "deps": deps,
    })))
}

pub(super) async fn create_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssetReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let a = asset_from_req(&req)?;
    if findb::assets::get_by_code(&db, &a.code)?.is_some() {
        return Err(AppError::bad_request("资产编码已存在"));
    }
    let id = findb::assets::insert(&db, &a)?;
    db.log(
        user.username(),
        "固定资产",
        "新增卡片",
        &format!("{} {}", a.code, a.name),
    )?;
    Ok(Json(json!({ "id": id })))
}

pub(super) async fn update_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AssetReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let existing =
        findb::assets::get(&db, id)?.ok_or_else(|| AppError::not_found("资产卡片不存在"))?;
    if existing.status == findb::assets::AssetStatus::Disposed {
        return Err(AppError::bad_request("已清理的卡片不能修改"));
    }
    let mut a = asset_from_req(&req)?;
    a.id = id;
    a.status = existing.status;
    a.disposed_period = existing.disposed_period;
    a.dispose_amount = existing.dispose_amount;
    a.voucher_id = existing.voucher_id;
    // 已计提过折旧：影响计价的字段冻结，避免历史折旧与台账对不上
    if !findb::assets::dep_list(&db, id)?.is_empty() {
        let frozen = a.original_value != existing.original_value
            || a.residual_rate != existing.residual_rate
            || a.life_months != existing.life_months
            || a.start_period != existing.start_period
            || a.method != existing.method;
        if frozen {
            return Err(AppError::bad_request(
                "已计提折旧的卡片不能修改原值/残值率/年限/启用期间/折旧方法",
            ));
        }
    }
    findb::assets::update_logged(&db, &existing, &a, user.username(), "卡片编辑")?;
    db.log(
        user.username(),
        "固定资产",
        "修改卡片",
        &format!("{} {}", a.code, a.name),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let a = findb::assets::get(&db, id)?.ok_or_else(|| AppError::not_found("资产卡片不存在"))?;
    findb::assets::delete(&db, id)?;
    db.log(
        user.username(),
        "固定资产",
        "删除卡片",
        &format!("{} {}", a.code, a.name),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn list_asset_deps(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let rows: Vec<serde_json::Value> = findb::assets::dep_list(&db, id)?
        .iter()
        .map(|d| {
            json!({
                "period": period_to_str(d.period),
                "amount": d.amount.fmt_money(),
                "accum": d.accum.fmt_money(),
                "net_value": d.net_value.fmt_money(),
                "voucher_id": d.voucher_id,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
}

/// 资产卡片变更历史（对标金蝶固定资产变动历史）
pub(super) async fn list_asset_changes(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::assets::changes(&db, id)? })))
}

/// 固定资产 ↔ 总账对账（原值/累计折旧逐科目差异）
pub(super) async fn asset_gl_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let r = findb::assets::gl_reconcile(&db, period)?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": r.rows,
        "cost_asset": r.cost_asset.fmt_money(),
        "cost_gl": r.cost_gl.fmt_money(),
        "cost_diff": r.cost_diff.fmt_money(),
        "dep_asset": r.dep_asset.fmt_money(),
        "dep_gl": r.dep_gl.fmt_money(),
        "dep_diff": r.dep_diff.fmt_money(),
    })))
}

/// 资产减值（记录减值累计；净值口径由卡片原值-累计折旧-减值体现）
pub(super) async fn impair_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AssetImpairReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    if findb::assets::get(&db, id)?.is_none() {
        return Err(AppError::not_found("资产卡片不存在"));
    }
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let amount = parse_money_checked(&req.amount)?;
    let aid = findb::assets::impair(&db, id, period, amount, &req.memo)?;
    db.log(
        user.username(),
        "固定资产",
        "资产减值",
        &format!("#{id} {} {}", period_to_str(period), amount.fmt_money()),
    )?;
    Ok(Json(json!({ "ok": true, "id": aid })))
}

/// 资产盘点单（草稿）：行 = 卡片 + 是否盘实
pub(super) async fn save_asset_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssetCountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    if req.lines.is_empty() {
        return Err(AppError::bad_request("盘点明细不能为空"));
    }
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let mut lines = Vec::with_capacity(req.lines.len());
    for l in &req.lines {
        if findb::assets::get(&db, l.asset_id)?.is_none() {
            return Err(AppError::bad_request(format!(
                "资产 #{} 不存在",
                l.asset_id
            )));
        }
        lines.push((l.asset_id, l.found.unwrap_or(true), l.memo.clone()));
    }
    let mut c = findb::assets::AssetCount {
        id: 0,
        no: findb::assets::ac_next_no(&db, period)?,
        period,
        date,
        status: "draft".into(),
        prepared_by: user.username().to_string(),
        memo: req.memo,
        lines,
    };
    let id = findb::assets::ac_save(&db, &mut c)?;
    db.log(
        user.username(),
        "固定资产",
        "资产盘点",
        &format!("{} 共 {} 行", c.no, c.lines.len()),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "no": c.no })))
}

/// 资产盘点过账：盘亏（found=false）标记为停用
pub(super) async fn post_asset_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let n = findb::assets::ac_post(&db, id)?;
    db.log(
        user.username(),
        "固定资产",
        "资产盘点过账",
        &format!("#{id} 盘亏 {n}"),
    )?;
    Ok(Json(json!({ "ok": true, "lost": n })))
}

/// 资产盘点单列表（含明细）
pub(super) async fn list_asset_counts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::assets::ac_list(&*db)? })))
}

pub(super) async fn dispose_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AssetDisposeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = period_checked(req.ymm)?;
    let amount = if req.amount.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&req.amount)?
    };
    let vid = findb::assets::dispose(&db, id, period, amount, user.username())?;
    db.log(
        user.username(),
        "固定资产",
        "资产清理",
        &format!(
            "#{id} {} 金额 {} → 转销凭证 #{vid}",
            period_to_str(period),
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

pub(super) async fn depreciate_assets(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = period_checked(req.ymm)?;
    let res = findb::assets::depreciate_period(&db, period, user.username())?;
    Ok(Json(json!({
        "already": res.already,
        "count": res.count,
        "total": res.total.fmt_money(),
        "voucher_id": res.voucher_id,
        "voucher_no": res.voucher_no,
    })))
}

pub(super) async fn delete_asset_deps_period(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = period_checked(req.ymm)?;
    let removed = findb::assets::dep_delete_period(&db, period)?;
    db.log(
        user.username(),
        "固定资产",
        "删除本期折旧",
        &format!("{} 共 {removed} 条", period_to_str(period)),
    )?;
    Ok(Json(json!({ "removed": removed })))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(Deserialize, Default)]
pub(super) struct AssetDisposeReq {
    pub ymm: i32,
    #[serde(default)]
    pub amount: String,
}

#[derive(Deserialize)]
pub(super) struct AssetImpairReq {
    #[serde(default)]
    period: i32,
    amount: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct AssetCountReq {
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
    lines: Vec<AssetCountLineReq>,
}
