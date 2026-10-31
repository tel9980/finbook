//! 计件工资（工序报工完工量 × 计件单价，按期间汇总到人）

use super::*;

#[derive(Deserialize)]
pub struct PieceRateReq {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub item_code: String,
    #[serde(default)]
    pub op_name: String,
    pub rate: String,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub memo: String,
}

/// 计件单价列表
pub(super) async fn list_piece_rates(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let rates = findb::piecepay::rate_list(&user.db)?;
    Ok(Json(json!({ "rates": rates })))
}

/// 新建/修改单价
pub(super) async fn save_piece_rate(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PieceRateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let mut r = findb::piecepay::PieceRate {
        id: req.id,
        item_code: req.item_code.trim().to_string(),
        op_name: req.op_name.trim().to_string(),
        rate: parse_money_checked(&req.rate)?,
        disabled: req.disabled,
        memo: req.memo,
    };
    let id = findb::piecepay::rate_save(db, &mut r)?;
    db.log(
        user.username(),
        "生产",
        "计件单价",
        &format!("{} / {} = {}", r.item_code, r.op_name, r.rate),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// 删除单价
pub(super) async fn delete_piece_rate(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    findb::piecepay::rate_delete(db, id)?;
    db.log(user.username(), "生产", "删除计件单价", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

/// 本期计件汇总（按人）
///
/// 数据范围口径与工资表一致：部门范围看该员工本期工资行的部门，
/// 「仅看本人经手的业务单据」看员工编码。
pub(super) async fn piece_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let mut rows = findb::piecepay::compute(db, period.ymm())?;
    rows.retain(|p| {
        let dept = business::payroll_get(db, period, &p.employee)
            .ok()
            .flatten()
            .map(|x| x.dept)
            .unwrap_or_default();
        user.user.data_scope.allows_dept(&dept) && doc_in_scope(&user, &p.employee)
    });
    let total = rows.iter().fold(Money::ZERO, |acc, p| acc + p.amount);
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": rows,
        "total": total,
    })))
}

/// 把本期计件金额回填进工资表（应发取「不低于计件额」，社保等仍按现有行重算个税）
pub(super) async fn piece_fill(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let pays = findb::piecepay::compute(db, period.ymm())?;
    let mut filled = Vec::new();
    let mut skipped = Vec::new();
    for p in pays {
        if !p.amount.is_positive() {
            continue;
        }
        // 数据范围先拦：不在范围内的人不该被这双手改写工资表
        if !doc_in_scope(&user, &p.employee) {
            skipped.push(json!({ "employee": p.employee, "reason": "不在数据范围内" }));
            continue;
        }
        let dept = business::payroll_get(db, period, &p.employee)?
            .map(|o| o.dept)
            .unwrap_or_default();
        if !user.user.data_scope.allows_dept(&dept) {
            skipped.push(json!({ "employee": p.employee, "reason": "部门不在数据范围内" }));
            continue;
        }
        // 算钱与写库都留在 findb：Web 与桌面端共用一份，别在界面层重写一遍
        match findb::piecepay::fill_one(db, period, &p)? {
            Some(f) => {
                db.log(
                    user.username(),
                    "工资",
                    "计件回填",
                    &format!(
                        "{} {} 应发 {} → {}",
                        period.label(),
                        f.employee,
                        f.before,
                        f.after
                    ),
                )?;
                filled.push(json!({
                    "employee": f.employee,
                    "qty": f.qty,
                    "piece": f.piece,
                    "before": f.before,
                    "after": f.after,
                }));
            }
            None => skipped.push(json!({ "employee": p.employee, "reason": "应发已不低于计件额" })),
        }
    }
    Ok(Json(
        json!({ "period": period_to_str(period), "filled": filled, "skipped": skipped }),
    ))
}
