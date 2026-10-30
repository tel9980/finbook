//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

pub(super) async fn list_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<business::Payroll>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let rows = business::payroll_list(&db, period)?;
    let rows: Vec<business::Payroll> = scope_payroll(&user, &rows).into_iter().cloned().collect();
    Ok(Json(rows))
}

/// 银行代发文件（CSV：账号,户名,金额；缺账号的员工跳过并计数）
pub(super) async fn export_payroll_bank_file(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let rows = business::payroll_list(&db, period)?;
    let rows = scope_payroll(&user, &rows);
    let mut out = vec![vec![
        "账号".to_string(),
        "户名".to_string(),
        "金额".to_string(),
    ]];
    let mut skipped = 0usize;
    for p in &rows {
        if !p.net.is_positive() {
            continue;
        }
        let (acc, bank) = employee_bank(&db, &p.employee)?;
        if acc.is_empty() {
            skipped += 1;
            continue;
        }
        out.push(vec![
            acc,
            if bank.is_empty() {
                p.employee.clone()
            } else {
                bank
            },
            p.net.fmt_plain(),
        ]);
    }
    db.log(
        user.username(),
        "工资",
        "银行代发文件",
        &format!(
            "{} 共 {} 人（缺账号跳过 {skipped}）",
            period_to_str(period),
            out.len().saturating_sub(1)
        ),
    )?;
    Ok(csv_response(
        &format!("bank_payroll_{}.csv", period.ymm()),
        out,
    ))
}

/// 工资条（单人）：本期工资 + 本年累计
pub(super) async fn get_payroll_slip(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let employee = q.get("employee").map(String::as_str).unwrap_or("").trim();
    if employee.is_empty() {
        return Err(AppError::bad_request("缺少 employee 参数"));
    }
    if !doc_in_scope(&user, employee) {
        return Err(AppError::forbidden("数据范围受限，不能查看他人的工资条"));
    }
    let p = business::payroll_get(&db, period, employee)?
        .ok_or_else(|| AppError::not_found("该员工本期无工资记录"))?;
    let ytd = business::payroll_ytd(&db, period, employee)?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "payroll": {
            "employee": p.employee, "dept": p.dept,
            "gross": p.gross.fmt_money(), "social": p.social.fmt_money(),
            "housing": p.housing.fmt_money(), "deduction": p.deduction.fmt_money(),
            "additional": p.additional.fmt_money(), "tax_base": p.tax_base.fmt_money(),
            "tax": p.tax.fmt_money(), "net": p.net.fmt_money(),
            "social_co": p.social_co.fmt_money(), "housing_co": p.housing_co.fmt_money(),
            "memo": p.memo,
        },
        "ytd": {
            "income": ytd.income.fmt_money(), "special": ytd.special.fmt_money(),
            "additional": ytd.additional.fmt_money(), "withheld": ytd.withheld.fmt_money(),
            "months": ytd.months,
        },
    })))
}

/// 个税申报表（全员工资薪金，本期口径）
pub(super) async fn get_payroll_tax_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let rows = business::payroll_list(&db, period)?;
    // 数据范围：受限账号只出自己（或本部门）的工资行，与列表口径一致
    let rows = scope_payroll(&user, &rows);
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|p| {
            json!({
                "employee": p.employee, "dept": p.dept,
                "income": p.gross.fmt_money(),
                "special": (p.social + p.housing).fmt_money(),
                "additional": p.additional.fmt_money(),
                "tax_base": p.tax_base.fmt_money(),
                "tax": p.tax.fmt_money(),
                "net": p.net.fmt_money(),
            })
        })
        .collect();
    Ok(Json(
        json!({ "period": period_to_str(period), "rows": items }),
    ))
}

/// 录入/修改一条工资：后端按累计预扣预缴法算个税与实发，前端无需自己算税
pub(super) async fn save_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollInput>,
) -> Result<Json<business::Payroll>, AppError> {
    user.require(Perm::VoucherNew)?;
    let employee = r.employee.trim();
    if employee.is_empty() {
        return Err(AppError::bad_request("员工编码必填"));
    }
    if !doc_in_scope(&user, employee) {
        return Err(AppError::forbidden("数据范围受限，不能录入他人的工资行"));
    }
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let p = business::payroll_calc(
        &db,
        period,
        employee,
        r.dept.trim(),
        parse_money_checked(&r.gross)?,
        parse_money_checked(&r.social)?,
        parse_money_checked(&r.housing)?,
        parse_money_checked(&r.deduction)?,
        parse_money_checked(&r.additional)?,
        parse_money_checked(&r.social_co)?,
        parse_money_checked(&r.housing_co)?,
        r.memo.trim(),
    )?;
    let id = business::payroll_upsert(&db, &p)?;
    db.log(
        user.username(),
        "工资",
        "保存工资行",
        &format!(
            "{} {} 应发 {} 实发 {}",
            period.label(),
            employee,
            p.gross,
            p.net
        ),
    )?;
    let mut out = p;
    out.id = id;
    Ok(Json(out))
}

/// 批量生成本月工资表（逐条累计预扣预缴算税后落库）
pub(super) async fn generate_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<GeneratePayrollReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    if user.user.data_scope.own_doc_only {
        return Err(AppError::forbidden("数据范围受限的账号不能批量生成工资表"));
    }
    let period = req
        .period
        .as_deref()
        .and_then(parse_period)
        .unwrap_or_else(|| current_period(&state, &user));
    let mut rows: Vec<(
        String,
        String,
        Money,
        Money,
        Money,
        Money,
        Money,
        Money,
        Money,
    )> = Vec::with_capacity(req.rows.len());
    for r in &req.rows {
        if r.employee.trim().is_empty() {
            continue;
        }
        rows.push((
            r.employee.trim().to_string(),
            r.dept.trim().to_string(),
            parse_money_checked(&r.gross)?,
            parse_money_checked(&r.social)?,
            parse_money_checked(&r.housing)?,
            parse_money_checked(&r.deduction)?,
            parse_money_checked(&r.additional)?,
            parse_money_checked(&r.social_co)?,
            parse_money_checked(&r.housing_co)?,
        ));
    }
    if rows.is_empty() {
        return Err(AppError::bad_request("没有可导入的工资行"));
    }
    let n = business::payroll_generate(&db, period, &rows)?;
    db.log(
        user.username(),
        "工资",
        "批量生成工资表",
        &format!("{} 生成 {} 条", period.label(), n),
    )?;
    Ok(Json(json!({ "ok": true, "count": n })))
}

pub(super) async fn delete_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    // 删除前拦截已生成凭证的工资行
    let p =
        business::payroll_get_by_id(&db, id)?.ok_or_else(|| AppError::not_found("工资行不存在"))?;
    if !doc_in_scope(&user, &p.employee) {
        return Err(AppError::forbidden("无权操作他人的工资行"));
    }
    if p.voucher_id.is_some() || p.social_voucher_id.is_some() || p.paid_voucher_id.is_some() {
        return Err(AppError::bad_request(
            "该工资行已生成凭证，不能删除（请先删除/反记账相关凭证）",
        ));
    }
    business::payroll_delete(&db, id)?;
    db.log(user.username(), "工资", "删除工资行", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn payroll_ytd(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<business::YtdPayroll>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let employee = q.get("employee").cloned().unwrap_or_default();
    if employee.trim().is_empty() {
        return Err(AppError::bad_request("缺少 employee 参数"));
    }
    if !doc_in_scope(&user, employee.trim()) {
        return Err(AppError::forbidden(
            "数据范围受限，不能查看他人的年度工资累计",
        ));
    }
    Ok(Json(business::payroll_ytd(&db, period, employee.trim())?))
}

/// 工资计提凭证
pub(super) async fn payroll_accrue(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollAccrueReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let date = req_date(&r.date, period.last_day())?;
    let id = business::payroll_accrue_voucher(
        &db,
        period,
        date,
        r.expense.trim(),
        r.wage_payable.trim(),
        r.social_payable.trim(),
        r.housing_payable.trim(),
        user.username(),
    )?;
    db.log(
        user.username(),
        "工资",
        "生成计提凭证",
        &format!("{} 凭证 {:?}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id })))
}

/// 缴纳社保公积金凭证
pub(super) async fn payroll_social_pay(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollSocialReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let date = req_date(&r.date, period.last_day())?;
    let id = business::payroll_social_voucher(
        &db,
        period,
        date,
        r.social_payable.trim(),
        r.housing_payable.trim(),
        r.personal_payable.trim(),
        r.bank_account.trim(),
        user.username(),
    )?;
    db.log(
        user.username(),
        "工资",
        "生成社保缴纳凭证",
        &format!("{} 凭证 {:?}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id })))
}

/// 工资发放凭证
pub(super) async fn payroll_pay(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollPayReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let date = req_date(&r.date, period.last_day())?;
    let id = business::payroll_pay_voucher(
        &db,
        period,
        date,
        r.payable_account.trim(),
        r.bank_account.trim(),
        r.tax_account.trim(),
        r.social_account.trim(),
        user.username(),
    )?;
    db.log(
        user.username(),
        "工资",
        "生成发放凭证",
        &format!("{} 凭证 {:?}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id })))
}

pub(super) async fn list_claims(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<business::Claim>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    // 空串 = 全部状态：UI 下拉默认值为空，直接 parse 会落到 Draft 只显示草稿
    let status = q
        .get("status")
        .filter(|s| !s.is_empty())
        .map(|s| business::ClaimStatus::parse(s));
    let rows = business::claim_list(&db, period, status)?;
    let rows: Vec<business::Claim> = scope_claims(&user, &rows).into_iter().cloned().collect();
    Ok(Json(rows))
}

pub(super) async fn next_claim_no(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let no = business::claim_next_no(&db, period)?;
    Ok(Json(json!({ "no": no })))
}

pub(super) async fn create_claim(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(r): Json<ClaimInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let applicant = r.applicant.trim();
    if applicant.is_empty() {
        return Err(AppError::bad_request("申请人必填"));
    }
    if !doc_in_scope(&user, applicant) {
        return Err(AppError::forbidden("数据范围受限，不能为他人填报销单"));
    }
    // 部门范围同样要管：否则受限会计能给范围外的部门开单，开完自己又看不到
    if !user.user.data_scope.allows_dept(r.dept.trim()) {
        return Err(AppError::forbidden(format!(
            "数据范围受限，不能为「{}」部门填报销单",
            r.dept.trim()
        )));
    }
    let db = &user.db;
    let period = match r.period.filter(|ym| *ym > 0) {
        Some(ym) => period_checked(ym)?,
        None => current_period(&state, &user),
    };
    let date = req_date(&r.biz_date, period.last_day())?;
    let c = business::Claim {
        id: 0,
        period,
        no: business::claim_next_no(&db, period)?,
        biz_date: date,
        applicant: applicant.to_string(),
        dept: r.dept.trim().to_string(),
        reason: r.reason.trim().to_string(),
        amount: parse_money_checked(&r.amount)?,
        status: business::ClaimStatus::Draft,
        items: claim_items(&r.items)?,
        approver: String::new(),
        approved_at: None,
        payer: String::new(),
        paid_at: None,
        voucher_id: None,
        created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    let id = business::claim_insert(&db, &c)?;
    db.log(
        user.username(),
        "报销",
        "新增报销单",
        &format!("{} {} {}", c.no, applicant, c.amount),
    )?;
    Ok(Json(json!({ "id": id, "no": c.no })))
}

pub(super) async fn update_claim(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(r): Json<ClaimInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let mut c = business::claim_get(&db, id)?.ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !claim_visible(&user, &c) {
        return Err(AppError::forbidden("数据范围受限，不能修改他人的报销单"));
    }
    if !c.status.editable() {
        return Err(AppError::bad_request("当前状态不允许修改内容"));
    }
    if c.voucher_id.is_some() {
        return Err(AppError::bad_request("已生成凭证的报销单不能修改"));
    }
    let date = req_date(&r.biz_date, c.period.last_day())?;
    c.biz_date = date;
    c.applicant = r.applicant.trim().to_string();
    if !claim_visible(&user, &c) {
        return Err(AppError::forbidden("数据范围受限，不能改由他人作为申请人"));
    }
    c.dept = r.dept.trim().to_string();
    c.reason = r.reason.trim().to_string();
    c.amount = parse_money_checked(&r.amount)?;
    c.items = claim_items(&r.items)?;
    business::claim_update(&db, &c)?;
    db.log(
        user.username(),
        "报销",
        "修改报销单",
        &format!("{} {}", c.no, c.amount),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_claim(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    let c = business::claim_get(&db, id)?.ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !claim_visible(&user, &c) {
        return Err(AppError::forbidden("数据范围受限，不能删除他人的报销单"));
    }
    // 预检：引擎的通用错误会映射成 500，这里给出明确的 400
    if c.voucher_id.is_some() {
        return Err(AppError::bad_request("该报销单已生成凭证，请先删除凭证"));
    }
    let label = c.no.clone();
    business::claim_delete(&db, id)?;
    db.log(user.username(), "报销", "删除报销单", &label)?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn claim_transition(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(r): Json<ClaimTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let to = business::ClaimStatus::parse(&r.status);
    let c = business::claim_get(&db, id)?.ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !claim_visible(&user, &c) {
        return Err(AppError::forbidden("数据范围受限，不能操作他人的报销单"));
    }
    // 工作流拦截（仅 批准/驳回 两个目标）：有流程 → Pending 只推进；NoFlow/Final → 原流转
    let gate_for = if to == business::ClaimStatus::Approved {
        Some(true)
    } else if to == business::ClaimStatus::Rejected {
        Some(false)
    } else {
        None
    };
    if let Some(appr) = gate_for {
        if let findb::workflow::Gate::Pending { next } =
            findb::workflow::intercept(&db, findb::workflow::BIZ_CLAIM, id, &user.user, appr, "")?
        {
            db.log(
                user.username(),
                "报销",
                "工作流节点",
                &format!("#{id} → {next}"),
            )?;
            return Ok(Json(json!({ "ok": true, "status": to, "pending": next })));
        }
    }
    business::claim_transition(&db, id, to, user.username())?;
    db.log(
        user.username(),
        "报销",
        "状态流转",
        &format!("#{id} → {}", to.label()),
    )?;
    Ok(Json(json!({ "ok": true, "status": to })))
}

pub(super) async fn claim_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(r): Json<ClaimVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    // 先验权限再验参数：范围外的单子连"参数对不对"都不该告诉调用方
    let c = business::claim_get(&db, id)?.ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !claim_visible(&user, &c) {
        return Err(AppError::forbidden(
            "数据范围受限，不能为他人报销单生成凭证",
        ));
    }
    let pay = r.pay_account.trim();
    if pay.is_empty() {
        return Err(AppError::bad_request("请填写支付科目"));
    }
    if let Some(existing) = c.voucher_id {
        // 幂等：支付时已自动出过凭证 → 返回同一张（不新增）
        return Ok(Json(json!({ "id": existing, "already": true })));
    }
    let vid = business::claim_voucher(&db, id, pay, user.username())?;
    db.log(
        user.username(),
        "报销",
        "生成凭证",
        &format!("#{id} 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "id": vid })))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(Deserialize)]
pub(super) struct GeneratePayrollReq {
    #[serde(default)]
    period: Option<String>,
    rows: Vec<PayrollInput>,
}

#[derive(Deserialize)]
pub(super) struct PayrollAccrueReq {
    #[serde(default)]
    date: String,
    expense: String,
    wage_payable: String,
    social_payable: String,
    housing_payable: String,
}

#[derive(Deserialize)]
pub(super) struct PayrollSocialReq {
    #[serde(default)]
    date: String,
    social_payable: String,
    housing_payable: String,
    personal_payable: String,
    bank_account: String,
}

#[derive(Deserialize)]
pub(super) struct PayrollPayReq {
    #[serde(default)]
    date: String,
    payable_account: String,
    bank_account: String,
    tax_account: String,
    social_account: String,
}

#[derive(Deserialize)]
pub(super) struct ClaimInput {
    /// 期间 ymm，如 202601
    #[serde(default)]
    period: Option<i32>,
    /// 业务日期 YYYY-MM-DD
    biz_date: String,
    applicant: String,
    #[serde(default)]
    dept: String,
    reason: String,
    amount: String,
    #[serde(default)]
    items: Vec<ClaimItemInput>,
}

#[derive(Deserialize)]
pub(super) struct ClaimTransitionReq {
    /// draft / submitted / approved / rejected / paid
    status: String,
}

#[derive(Deserialize)]
pub(super) struct ClaimVoucherReq {
    /// 贷方支付科目（如 100201 银行存款）
    pay_account: String,
}
