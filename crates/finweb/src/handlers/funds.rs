//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

/// 收付款单套打 HTML：`ids` 缺省 = 全部（可配 `period` 过滤）
pub(super) async fn print_receipt_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let company = user.company.clone();
    let ids = print_ids(&q);
    let period = q.get("period").and_then(|s| parse_period(s));
    let all = findb::receipt::receipt_list(&*db)?;
    let mut prints = Vec::new();
    for d in all.into_iter() {
        if !ids.is_empty() {
            if ids.contains(&d.id) {
                prints.push(receipt_to_print(d));
            }
        } else if period.map(|p| d.period == p).unwrap_or(true) {
            prints.push(receipt_to_print(d));
        }
    }
    if prints.is_empty() {
        return Err(AppError::bad_request("没有可打印的收付款单"));
    }
    let f = doc_fields_from_q(&q);
    let html = findb::printform::receipt_forms_sized_html(
        &company,
        &prints,
        &f,
        &doc_size_from_q(&q),
        q.get("auto").map(|s| s.as_str() == "1").unwrap_or(false),
        &db.options().print,
    );
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

pub(super) async fn list_bills(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    // 空串表示"全部"：UI 下拉默认值为空，不能当成具体类型去过滤
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let rows = findb::funds::bill_list(&db, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn save_bill(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BillReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let parse_date = |s: &str| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))
    };
    let mut b = findb::funds::Bill {
        id: req.id,
        kind: req.kind,
        no: req.no,
        period: period_checked(req.period)?,
        issue_date: parse_date(&req.issue_date)?,
        due_date: parse_date(&req.due_date)?,
        counterpart: req.counterpart,
        bank: req.bank,
        amount: parse_money_checked(&req.amount)?,
        status: if req.status.is_empty() {
            "in_hand".to_string()
        } else {
            req.status
        },
        handled_date: None,
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
        voucher_id: None,
    };
    let id = findb::funds::bill_save(&db, &mut b)?;
    db.log(
        user.username(),
        "资金",
        "保存票据",
        &format!("#{id} {}", b.no),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

pub(super) async fn bill_transition(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<BillTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    let to = findb::funds::BillStatus::parse(&req.status);
    // 资金动作（背书/贴现/兑付）在 findb 同事务自动生成台账凭证草稿
    let vid = findb::funds::bill_transition(&db, id, to, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "票据流转",
        &format!(
            "#{} → {}{}",
            id,
            to.label(),
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

pub(super) async fn delete_bill(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::bill_delete(&db, id)?;
    db.log(user.username(), "资金", "删除票据", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn list_loans(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    // 空串表示"全部"：UI 下拉默认值为空，不能当成具体类型去过滤
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let rows = findb::funds::loan_list(&db, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn save_loan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<LoanReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let parse_date = |s: &str| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))
    };
    let mut l = findb::funds::Loan {
        id: req.id,
        kind: req.kind,
        no: req.no,
        bank: req.bank,
        principal: parse_money_checked(&req.principal)?,
        rate_pct: parse_money_checked(&req.rate_pct)?,
        start_date: parse_date(&req.start_date)?,
        end_date: parse_date(&req.end_date)?,
        status: if req.status.is_empty() {
            "active".to_string()
        } else {
            req.status
        },
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
        voucher_id: None,
        settle_voucher_id: None,
        settle_date: None,
    };
    let id = findb::funds::loan_save(&db, &mut l)?;
    db.log(
        user.username(),
        "资金",
        "保存融资",
        &format!("#{id} {}", l.no),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// 结清融资（body 可选指定结清日期；findb 同事务自动生成还本凭证）
pub(super) async fn loan_settle(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let today = chrono::Local::now().date_naive();
    let date = if body.is_empty() {
        today
    } else {
        let req: LoanSettleReq = serde_json::from_slice(&body)
            .map_err(|_| AppError::bad_request("请求体应为 {\"date\":\"YYYY-MM-DD\"}"))?;
        match req.date {
            Some(s) => chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
            None => today,
        }
    };
    let vid = findb::funds::loan_settle(&db, id, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "结清融资",
        &format!(
            "#{id}{}",
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

/// 票据补出台账凭证（流转已自动生成；本端点用于存量台账回填）
pub(super) async fn bill_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let vid = findb::funds::bill_voucher(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "票据生成凭证",
        &format!("#{id} → 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "id": vid })))
}

/// 融资台账凭证（存续=到账凭证；已结清=还本凭证回填）
pub(super) async fn loan_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let vid = findb::funds::loan_voucher(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "融资生成凭证",
        &format!("#{id} → 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "id": vid })))
}

pub(super) async fn delete_cash_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::cash_count_delete(&db, id)?;
    db.log(user.username(), "资金", "删除盘点记录", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 接班确认（交班人不能自我确认）
pub(super) async fn confirm_cash_shift(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    findb::funds::cash_shift_confirm(&db, id, user.username())?;
    db.log(user.username(), "资金", "交接班确认", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 取消交班单（仅待确认状态）
pub(super) async fn cancel_cash_shift(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    findb::funds::cash_shift_cancel(&db, id)?;
    db.log(user.username(), "资金", "交接班取消", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn list_checks(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::funds::check_list(&*db)? })))
}

/// 支票登记簿新增/修改（备查簿，不入账）
pub(super) async fn save_check(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CheckReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    if req.no.trim().is_empty() {
        return Err(AppError::bad_request("支票号必填"));
    }
    let issued_date = if req.issued_date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.issued_date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let kind = if req.kind == "cash" {
        "cash"
    } else {
        "transfer"
    };
    let mut c = findb::funds::CheckRow {
        id: req.id,
        no: req.no.trim().to_string(),
        kind: kind.to_string(),
        bank_account: req.bank_account,
        payee: req.payee,
        amount: parse_money_checked(&req.amount)?,
        issued_date,
        status: "issued".to_string(),
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::check_save(&db, &mut c)?;
    db.log(
        user.username(),
        "资金",
        "保存支票",
        &format!("#{id} {}", c.no),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

pub(super) async fn check_status(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let status = req["status"].as_str().unwrap_or("");
    findb::funds::check_set_status(&db, id, status)?;
    db.log(
        user.username(),
        "资金",
        "支票状态",
        &format!("#{id} → {status}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_check(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::check_delete(&db, id)?;
    db.log(user.username(), "资金", "删除支票", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn list_advances(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::funds::advance_list(&*db)? })))
}

/// 新增/修改借支单（建单即 approved；支付/核销走独立端点）
pub(super) async fn save_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AdvanceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let no = if req.no.trim().is_empty() {
        format!("JZ{}", chrono::Local::now().format("%Y%m%d%H%M%S"))
    } else {
        req.no.trim().to_string()
    };
    let mut a = findb::funds::Advance {
        id: req.id,
        no,
        period: fincore::Period::from_date(date),
        date,
        employee: req.employee,
        purpose: req.purpose,
        amount: parse_money_checked(&req.amount)?,
        pay_account: if req.pay_account.trim().is_empty() {
            "1001".to_string()
        } else {
            req.pay_account.trim().to_string()
        },
        status: "approved".to_string(),
        paid_date: None,
        paid_voucher_id: None,
        settle_date: None,
        settle_voucher_id: None,
        expense_account: if req.expense_account.trim().is_empty() {
            "660201".to_string()
        } else {
            req.expense_account.trim().to_string()
        },
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::advance_save(&db, &mut a)?;
    db.log(
        user.username(),
        "资金",
        "借支建单",
        &format!("#{} {} {} {}", id, a.no, a.employee, a.amount.fmt_money()),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// 支付借支（body 可选 {date}，默认今天）；同事务生成支付凭证
pub(super) async fn pay_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = opt_body_date(&body)?.unwrap_or_else(|| chrono::Local::now().date_naive());
    let vid = findb::funds::advance_pay(&db, id, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "借支支付",
        &format!(
            "#{id}{}",
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

/// 核销借支（body: {expense_account, expense_amount, date?}）；同事务生成核销凭证
pub(super) async fn settle_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    if body.is_empty() {
        return Err(AppError::bad_request(
            "请求体应为 {\"expense_account\":\"660201\",\"expense_amount\":\"1500\",\"date\":\"YYYY-MM-DD\"?}",
        ));
    }
    let req: AdvanceSettleReq = serde_json::from_slice(&body)
        .map_err(|_| AppError::bad_request("请求体格式错误（见接口约定）"))?;
    let date = match req.date {
        Some(s) => chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
        None => chrono::Local::now().date_naive(),
    };
    let expense_account = if req.expense_account.trim().is_empty() {
        "660201".to_string()
    } else {
        req.expense_account.trim().to_string()
    };
    let expense = parse_money_checked(&req.expense_amount)?;
    let vid =
        findb::funds::advance_settle(&db, id, &expense_account, expense, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "借支核销",
        &format!(
            "#{id} 冲账 {expense}{}",
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

pub(super) async fn delete_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::advance_delete(&db, id)?;
    db.log(user.username(), "资金", "删除借支单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::receipt::receipt_delete(&db, id)?;
    db.log(user.username(), "资金", "删除收付款单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 审核收付款单（对标金蝶）：同事务生成资金凭证 + FIFO 自动核销。审核权 =
/// **VoucherAudit 或 VoucherPost**（见 `require_fund_doc_gate`）——出纳录单，
/// 有记账权的人把关；两者都没有时（出纳想审自己提的单）仍然拦住。
pub(super) async fn audit_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let db = &user.db;
    // 工作流优先：有流程 → 参与人/审核权由拦截器判定，终态才出凭证；
    // 无流程（默认流）→ 维持原审核权 VoucherAudit（出纳录单、审核人把关）
    match findb::workflow::intercept(&db, findb::workflow::BIZ_RECEIPT, id, &user.user, true, "")? {
        findb::workflow::Gate::Pending { next } => {
            db.log(
                user.username(),
                "审批",
                "工作流节点",
                &format!("收付款#{id} → {next}"),
            )?;
            return Ok(Json(json!({ "ok": true, "pending": next })));
        }
        findb::workflow::Gate::Final { approved: false } => {
            return Ok(Json(json!({ "ok": true, "rejected": true })));
        }
        findb::workflow::Gate::NoFlow => {
            require_fund_doc_gate(&user)?;
        }
        findb::workflow::Gate::Final { approved: true } => {}
    }
    let (vid, settled) = findb::receipt::receipt_audit(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "审核收付款单",
        &format!("#{id} 凭证 #{vid}（自动核销 {settled} 笔）"),
    )?;
    Ok(Json(
        json!({ "ok": true, "voucher_id": vid, "settled": settled }),
    ))
}

/// 撤销审核：删除未记账凭证（含核销配对清理）→ 单据回草稿
pub(super) async fn unaudit_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    findb::receipt::receipt_unaudit(&db, id)?;
    db.log(
        user.username(),
        "资金",
        "撤销审核收付款单",
        &format!("#{id}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

/// 审核资金调拨单：生成凭证（借 转入 / 贷 转出）。审核权同收付款单
/// （`require_fund_doc_gate`：VoucherAudit 或 VoucherPost）
pub(super) async fn audit_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    let vid = findb::transfer::transfer_audit(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "审核资金调拨单",
        &format!("#{id} 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

/// 撤销审核：删除未记账凭证 → 单据回草稿（判据与审核一致）
pub(super) async fn unaudit_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    findb::transfer::transfer_unaudit(&db, id)?;
    db.log(
        user.username(),
        "资金",
        "撤销审核资金调拨单",
        &format!("#{id}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::transfer::transfer_delete(&db, id)?;
    db.log(user.username(), "资金", "删除资金调拨单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_loan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::loan_delete(&db, id)?;
    // 融资记录挂着结算凭证与还款凭证，是资金模块里审计最敏感的一类，
    // 删除必须留痕（与 save_loan / delete_bill / delete_advance / delete_receipt 同口径）。
    // 这里保留 `?`：删除动作本身幂等（重试不会二次产生业务写入），
    // 与"提交后再写日志"导致的重复入账不是同一类问题。
    db.log(user.username(), "资金", "删除融资", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 资金预算 vs 执行（当期，仅现金/银行科目；实际=当期已记账净额）
pub(super) async fn get_funds_budget(
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
    let rows = findb::funds::funds_budget(&db, period)?;
    Ok(Json(json!({ "rows": rows })))
}

/// 资金日报（按日）：?date=YYYY-MM-DD（默认今天）→ 上日结余/本日收支/日末结存
pub(super) async fn get_funds_daily_date(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let date = match q.get("date") {
        Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
        None => chrono::Local::now().date_naive(),
    };
    let rows = findb::funds::funds_daily_by_date(&db, date)?;
    Ok(Json(json!({
        "date": date.format("%Y-%m-%d").to_string(),
        "rows": rows
    })))
}

pub(super) async fn get_funds_daily(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let rows = findb::funds::funds_daily(&db, period)?;
    Ok(Json(
        serde_json::json!({ "period": period.label(), "rows": rows }),
    ))
}

pub(super) async fn get_funds_forecast(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let fc = findb::funds::funds_forecast(&db, period)?;
    Ok(Json(
        serde_json::json!({ "period": period.label(), "forecast": fc }),
    ))
}

/// 银行流水生成凭证（出纳）
///
/// 只处理**未勾对**且**能从摘要里认出往来单位**的流水：
/// 进账认客户（贷应收）、支出认供应商（借应付）。认不出的原样留在对账页 ——
/// 刻意不给「待定」科目兜底，那种挂账要到期末才暴露，且往往没人处理。
///
/// 生成的凭证一律草稿：还要过审核、要过出纳签字（资金科目必然命中）才能记账。
/// **不自动记账** —— 拿银行流水直接记账等于把出纳签字这一关自动掉。
pub(super) async fn gen_bank_vouchers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = &user.db;
    let r = findb::bank::gen_vouchers(&db, period, &account, user.username())?;
    db.log(
        user.username(),
        "银行对账",
        "流水生成凭证",
        &format!("生成 {} 张 / 跳过 {} 条", r.generated, r.skipped.len()),
    )?;
    Ok(Json(json!({
        "generated": r.generated,
        "linked": r.linked,
        "voucher_ids": r.voucher_ids,
        "skipped": r.skipped.iter().map(|s| json!({
            "stmt_id": s.stmt_id,
            "date": s.biz_date,
            "summary": s.summary,
            "amount": s.amount.fmt_money(),
            "reason": s.reason,
        })).collect::<Vec<_>>(),
        // 把前提说清楚：生成的凭证只含「银行侧 + 往来侧」，不含收入/成本侧。
        // 往来未入账时应收/应付会出现负数 —— 这是这条路子的固有前提，不是 bug。
        "note": "生成的凭证只含银行侧与往来侧两行（借 银行/贷 应收 或 借 应付/贷 银行），\
                 不含收入与成本侧。适用于「往来已记好、只差银行侧没记」的场景；\
                 若销售/采购尚未入账，应收/应付会出现负数，请在记账前核对。\
                 凭证为草稿，需审核（若开启）并经出纳签字后才能记账。",
    })))
}

pub(super) async fn get_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let db = &user.db;
    let statements = findb::bank::list(&*db, period, account.trim())?;
    let book = findb::bank::book_side(&*db, period, account.trim())?;
    let recon = findb::bank::reconcile(&*db, period, account.trim())?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "account": account.trim(),
        "statements": statements.iter().map(stmt_json).collect::<Vec<_>>(),
        "book": book.iter().map(book_entry_json).collect::<Vec<_>>(),
        "reconcile": reconcile_json(&recon),
    })))
}

pub(super) async fn import_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankImportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = &user.db;
    let (n, warnings) = findb::bank::import_csv(&db, period, &account, &req.text)?;
    db.log(
        user.username(),
        "银行对账",
        "导入对账单",
        &format!("{account} {n} 条"),
    )?;
    Ok(Json(json!({ "imported": n, "warnings": warnings })))
}

pub(super) async fn auto_match_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = &user.db;
    let r = findb::bank::auto_match(&db, period, &account, req.tolerance, user.username())?;
    db.log(
        user.username(),
        "银行对账",
        "自动勾对",
        &format!("{account} 成功 {} 对", r.matched),
    )?;
    Ok(Json(json!({
        "matched": r.matched,
        "by_no": r.by_no,
        "by_amount_date": r.by_amount_date,
        "by_amount": r.by_amount,
        "ambiguous": r.ambiguous,
    })))
}

pub(super) async fn link_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankLinkReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::bank::link(&*db, req.stmt_id, req.entry_id, user.username())?;
    db.log(
        user.username(),
        "银行对账",
        "手工勾对",
        &format!("流水#{} ↔ 分录#{}", req.stmt_id, req.entry_id),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn unlink_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankStmtIdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::bank::unlink(&db, req.stmt_id)?;
    db.log(
        user.username(),
        "银行对账",
        "取消勾对",
        &format!("流水#{}", req.stmt_id),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn clear_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = &user.db;
    let n = findb::bank::clear(&db, period, &account)?;
    db.log(
        user.username(),
        "银行对账",
        "清空对账单",
        &format!("{account} {} 条", period_to_str(period)),
    )?;
    Ok(Json(json!({ "removed": n })))
}

pub(super) async fn list_cash_counts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    Ok(Json(
        serde_json::json!({ "rows": findb::funds::cash_count_list(&*db)? }),
    ))
}

/// 新增/修改现金盘点：账面余额与差异由服务端按资金日报（按日）口径重算
pub(super) async fn save_cash_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CashCountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let account = if req.account_code.trim().is_empty() {
        "1001".to_string()
    } else {
        req.account_code.trim().to_string()
    };
    let mut c = findb::funds::CashCount {
        id: req.id,
        period: fincore::Period::from_date(date),
        date,
        account_code: account,
        book_amount: Money::ZERO,
        counted: parse_money_checked(&req.counted)?,
        diff: Money::ZERO,
        memo: req.memo,
        voucher_id: None,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::cash_count_save(&db, &mut c)?;
    db.log(
        user.username(),
        "资金",
        "现金盘点",
        &format!(
            "{} {} 实盘 {}（账面 {} 差异 {}）",
            c.account_code,
            c.date.format("%Y-%m-%d"),
            c.counted.fmt_money(),
            c.book_amount.fmt_money(),
            c.diff.fmt_money()
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "book_amount": c.book_amount,
        "diff": c.diff
    })))
}

/// 盘盈盘亏差异生成凭证
pub(super) async fn cash_count_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let vid = findb::funds::cash_count_voucher(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "盘点差异出凭证",
        &format!("#{id} → 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "id": vid })))
}

/// 交接班列表（只读，财务报表权限）
pub(super) async fn list_cash_shifts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100);
    Ok(Json(
        json!({ "rows": findb::funds::cash_shift_list(&db, limit)? }),
    ))
}

/// 新建交班单：快照当日现金/银行结存、在库票据、未日清账户（服务端计算）
pub(super) async fn create_cash_shift(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CashShiftReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let id = findb::funds::cash_shift_create(
        &db,
        date,
        user.username(),
        req.to_user.trim(),
        req.memo.trim(),
    )?;
    let s = findb::funds::cash_shift_get(&db, id)?
        .ok_or_else(|| AppError::from(fincore::FinError::msg("交班单创建失败")))?;
    db.log(
        user.username(),
        "资金",
        "交接班",
        &format!(
            "交班 {} 现金 {} 银行 {} 票据 {} 张 {} / 未日清 {} 户",
            date.format("%Y-%m-%d"),
            s.cash_balance.fmt_money(),
            s.bank_balance.fmt_money(),
            s.bill_count,
            s.bill_amount.fmt_money(),
            s.uncleared
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "shift": s })))
}

/// 日清日期查询：?account=&from=YYYY-MM-DD&to=YYYY-MM-DD（缺省 today~today）
pub(super) async fn list_day_clear(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let account = q
        .get("account")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "1001".to_string());
    let parse = |k: &str| -> Result<Option<chrono::NaiveDate>, AppError> {
        match q.get(k) {
            Some(s) => Ok(Some(
                chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
            )),
            None => Ok(None),
        }
    };
    let today = chrono::Local::now().date_naive();
    let from = parse("from")?.unwrap_or(today);
    let to = parse("to")?.unwrap_or(today);
    let dates = findb::funds::day_clear_dates(&db, &account, from, to)?;
    Ok(Json(json!({ "dates": dates })))
}

/// 日清标记 / 取消
pub(super) async fn set_day_clear(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<DayClearReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let account = if req.account_code.trim().is_empty() {
        "1001".to_string()
    } else {
        req.account_code.trim().to_string()
    };
    let date = chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    findb::funds::day_clear_set(&db, &account, date, req.clear, user.username())?;
    db.log(
        user.username(),
        "资金",
        if req.clear {
            "日记账日清"
        } else {
            "取消日清"
        },
        &format!("{} {}", account, date.format("%Y-%m-%d")),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn list_receipts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::receipt::receipt_list(&*db)? })))
}

/// 新增收付款单：同事务生成资金凭证 + FIFO 自动核销（对标金蝶收款单/付款单）
pub(super) async fn create_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptCreateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let kind = if req.kind == "payment" {
        "payment"
    } else {
        "receipt"
    };
    let amount = parse_money_checked(&req.amount)?;
    // 审核流：只落草稿单据；凭证与自动核销在「审核」（VoucherAudit）时生成
    let id = findb::receipt::receipt_create(
        &db,
        kind,
        date,
        &req.fund_account,
        &req.party,
        &req.bank,
        &req.cross_party,
        parse_money_checked(&req.cross_amount)?,
        amount,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "资金",
        "新增收付款单（待审核）",
        &format!(
            "#{id} {} {}",
            if kind == "receipt" {
                "收款"
            } else {
                "付款"
            },
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "status": "draft" })))
}

pub(super) async fn list_transfers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    Ok(Json(
        json!({ "rows": findb::transfer::transfer_list(&*db)? }),
    ))
}

/// 新增资金调拨单：只落草稿单据；凭证在「审核」时生成（对标金蝶录单 → 审核）
pub(super) async fn create_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<TransferCreateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let amount = parse_money_checked(&req.amount)?;
    let id = findb::transfer::transfer_create(
        &db,
        date,
        &req.from_account,
        &req.to_account,
        &req.bank,
        amount,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "资金",
        "新增资金调拨单（待审核）",
        &format!(
            "#{id} {}→{} {}",
            req.from_account.trim(),
            req.to_account.trim(),
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "status": "draft" })))
}

/// 滚动资金预测（票据到期 + 融资起止按期间展开）
pub(super) async fn funds_forecast_rolling_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let n = q
        .get("periods")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(6);
    Ok(Json(json!({
        "from": period_to_str(from),
        "rows": findb::funds::funds_forecast_rolling(&db, from, n)?,
    })))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(Deserialize)]
pub(super) struct BillReq {
    #[serde(default)]
    pub id: i64,
    pub kind: String,
    pub no: String,
    pub period: i32,
    pub issue_date: String,
    pub due_date: String,
    #[serde(default)]
    pub counterpart: String,
    #[serde(default)]
    pub bank: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize)]
pub(super) struct BillTransitionReq {
    pub status: String,
    pub date: String,
}

#[derive(Deserialize)]
pub(super) struct LoanReq {
    #[serde(default)]
    pub id: i64,
    pub kind: String,
    pub no: String,
    #[serde(default)]
    pub bank: String,
    #[serde(default)]
    pub principal: String,
    #[serde(default)]
    pub rate_pct: String,
    pub start_date: String,
    pub end_date: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize)]
pub(super) struct LoanSettleReq {
    #[serde(default)]
    date: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CashCountReq {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    date: String,
    #[serde(default)]
    account_code: String,
    #[serde(default)]
    counted: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct DayClearReq {
    #[serde(default)]
    account_code: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    clear: bool,
}

#[derive(Deserialize)]
pub(super) struct CashShiftReq {
    #[serde(default)]
    date: String,
    #[serde(default)]
    to_user: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct CheckReq {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    no: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    bank_account: String,
    #[serde(default)]
    payee: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    issued_date: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct AdvanceReq {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    no: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    employee: String,
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    pay_account: String,
    #[serde(default)]
    expense_account: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct AdvanceSettleReq {
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    expense_account: String,
    #[serde(default)]
    expense_amount: String,
}

#[derive(Deserialize)]
pub(super) struct ReceiptCreateReq {
    #[serde(default)]
    date: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    fund_account: String,
    #[serde(default)]
    party: String,
    #[serde(default)]
    bank: String,
    #[serde(default)]
    cross_party: String,
    #[serde(default)]
    cross_amount: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct TransferCreateReq {
    #[serde(default)]
    date: String,
    pub from_account: String,
    pub to_account: String,
    #[serde(default)]
    pub bank: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize, Default)]
pub(super) struct BankImportReq {
    pub ymm: i32,
    pub account: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Deserialize)]
pub(super) struct BankAutoReq {
    pub ymm: i32,
    pub account: String,
    #[serde(default)]
    pub tolerance: i64,
}

#[derive(Deserialize)]
pub(super) struct BankLinkReq {
    pub stmt_id: i64,
    pub entry_id: i64,
}

#[derive(Deserialize)]
pub(super) struct BankStmtIdReq {
    pub stmt_id: i64,
}
