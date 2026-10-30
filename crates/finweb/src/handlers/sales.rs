//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

pub(super) async fn list_invoices(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<InvoiceListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    // 上限 500 条（与凭证报表列表同口径）：limit=None 等于不限，把全库发票一次性
    // 读进内存并整包序列化。超限时按开票日期倒序截断，truncated 回报给调用方，
    // 让它收窄筛选条件而不是误以为"就这么多"。
    const INVOICE_LIST_MAX: i64 = 500;
    let rows = findb::invoices::list(
        &db,
        &findb::invoices::InvoiceQuery {
            kind: if q.kind.is_empty() {
                None
            } else {
                Some(q.kind)
            },
            status: if q.status.is_empty() {
                None
            } else {
                Some(q.status)
            },
            keyword: if q.keyword.is_empty() {
                None
            } else {
                Some(q.keyword)
            },
            limit: Some(INVOICE_LIST_MAX),
        },
    )?;
    let items: Vec<serde_json::Value> = rows.iter().map(invoice_json).collect();
    Ok(Json(json!({
        "rows": items,
        "total": items.len(),
        "limit": INVOICE_LIST_MAX,
        "truncated": items.len() as i64 >= INVOICE_LIST_MAX,
    })))
}

pub(super) async fn invoice_summary(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let sum = findb::invoices::summary(&*db)?;
    let by_kind: serde_json::Map<String, serde_json::Value> = sum
        .into_iter()
        .map(|(k, tax_total, tax_amt, n)| {
            (
                k.clone(),
                json!({ "amount_tax": tax_total.fmt_money(), "tax": tax_amt.fmt_money(), "count": n }),
            )
        })
        .collect();
    Ok(Json(json!({ "by_kind": by_kind })))
}

/// 下推开票：采购订单 → 进项发票（金额=订单整单，状态=待认证），记录单据勾稽
pub(super) async fn invoice_from_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<InvoiceFromPo>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let id = findb::invoices::push_from_po(&db, req.po_id, user.username())?;
    db.log(
        user.username(),
        "发票",
        "下推采购发票",
        &format!("PO#{} → 发票#{id}", req.po_id),
    )?;
    Ok(Json(json!({ "ok": true, "invoice_id": id })))
}

/// 下推开票：销售订单 → 销项发票（金额=订单整单，状态=待认证），记录单据勾稽
pub(super) async fn invoice_from_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<InvoiceFromSo>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let id = findb::invoices::push_from_so(&db, req.so_id, user.username())?;
    db.log(
        user.username(),
        "发票",
        "下推销售发票",
        &format!("SO#{} → 发票#{id}", req.so_id),
    )?;
    Ok(Json(json!({ "ok": true, "invoice_id": id })))
}

pub(super) async fn create_invoice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<InvoiceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let inv = invoice_from_req(&req)?;
    let id = findb::invoices::insert(&db, &inv, user.username())?;
    Ok(Json(json!({ "id": id })))
}

pub(super) async fn invoice_set_status(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<InvoiceStatusReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = &user.db;
    // 归属校验：状态机接口同样不得改他人录入的发票（否则绕过 update_invoice 的守卫）
    let existing =
        findb::invoices::get(&db, id)?.ok_or_else(|| AppError::not_found("发票不存在"))?;
    if existing.created_by != user.username() && !user.user.is_admin() {
        return Err(AppError::forbidden("无权修改他人录入的发票状态"));
    }
    let inv = findb::invoices::set_status(&db, id, &req.status, user.username())?;
    Ok(Json(invoice_json(&inv)))
}

/// 某一税率档的构成发票：回答「这个汇总数由哪几张票构成」
pub(super) async fn get_tax_vat_detail(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let kind = q.get("kind").map(|s| s.trim()).unwrap_or("out");
    if kind != "out" && kind != "in" {
        return Err(AppError::bad_request(
            "kind 只能是 out（销项）或 in（进项）",
        ));
    }
    let rate = q
        .get("rate")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("缺少参数 rate（税率档，如 13）"))?;
    let rows = findb::taxdecl::bucket_detail(&db, kind, period, rate)?;
    Ok(Json(
        json!({ "kind": kind, "rate": rate, "rows": rows, "count": rows.len() }),
    ))
}

/// 导出申报表 CSV（带 BOM，Excel 双击直接打开不乱码）
pub(super) async fn export_tax_vat(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let f = findb::taxdecl::vat_main_form(&db, period)?;
    let company = db.options().company;

    let mut out = String::from("\u{feff}");
    let mut row = |cells: &[String]| {
        out.push_str(
            &cells
                .iter()
                .map(|c| csv_escape(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    };
    let s = |v: &fincore::Money| v.fmt_money();

    row(&[
        "增值税一般纳税人申报表（主表）".into(),
        format!("税款所属期：{}", f.period),
        format!("纳税人名称：{company}"),
        format!("纳税人识别号：{}", db.options().tax_no),
    ]);
    row(&[]);
    row(&["一、销项税额".into(), "本期数".into()]);
    row(&["一般计税方法计税销售额".into(), s(&f.sales_general)]);
    row(&["简易计税方法计税销售额".into(), s(&f.sales_simple)]);
    row(&["免征增值税销售额".into(), s(&f.sales_exempt)]);
    row(&["销项税额".into(), s(&f.output_tax)]);
    row(&[]);
    row(&["二、进项税额".into(), "本期数".into()]);
    row(&["本期认证相符的进项税额".into(), s(&f.input_tax)]);
    row(&["上期留抵税额".into(), "（人工填报）".into()]);
    row(&["进项税额转出额".into(), "（人工填报，见附列资料三）".into()]);
    row(&[]);
    row(&["三、税额计算".into(), "本期数".into()]);
    row(&["应纳税额".into(), s(&f.payable)]);
    row(&["未开票销售额".into(), s(&f.unbilled_sales)]);
    row(&["未开票收入销项税额".into(), s(&f.unbilled_tax)]);
    row(&[]);
    row(&["四、附加税费".into(), "本期数".into()]);
    row(&[
        format!("城市维护建设税（{}）", f.surcharge.city_rate),
        s(&f.surcharge.city_tax),
    ]);
    row(&[
        format!("教育费附加（{}）", f.surcharge.edu_rate),
        s(&f.surcharge.edu),
    ]);
    row(&[
        format!("地方教育附加（{}）", f.surcharge.local_edu_rate),
        s(&f.surcharge.local_edu),
    ]);
    row(&["附加税费合计".into(), s(&f.surcharge.total)]);

    for t in [&f.details_out, &f.details_in] {
        row(&[]);
        row(&[
            t.title.clone(),
            "税率".into(),
            "张数".into(),
            "销售额".into(),
            "税额".into(),
            "价税合计".into(),
        ]);
        for b in &t.buckets {
            row(&[
                String::new(),
                format!("{}%", b.rate),
                b.count.to_string(),
                s(&b.net),
                s(&b.tax),
                s(&b.gross),
            ]);
        }
        row(&[
            "合计".into(),
            String::new(),
            t.count.to_string(),
            s(&t.net_total),
            s(&t.tax_total),
            s(&t.gross_total),
        ]);
    }

    // 提示随表导出：拿到 CSV 的人（不一定是出表的会计）也要知道哪些数没算
    if !f.warnings.is_empty() {
        row(&[]);
        row(&["口径说明（以下各项系统不代算，需人工填报）".into()]);
        for w in &f.warnings {
            row(&[format!("· {w}")]);
        }
    }

    // 中文文件名必须用 filename*=UTF-8'' 百分号编码，否则浏览器下下来名字乱码。
    // 与附件下载（send_attachment）同一套写法，不引新依赖。
    let fname = format!("增值税申报表_{}_{}.csv", company, f.period);
    let encoded: String = fname.bytes().map(|b| format!("%{b:02X}")).collect();
    let mut resp = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/csv; charset=utf-8")],
        out,
    )
        .into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
        "attachment; filename=\"vat_{}.csv\"; filename*=UTF-8''{encoded}",
        f.period
    )) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

pub(super) async fn get_so_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::scm2::so_reconcile(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn list_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let rows = findb::sales::quo_list(&db, period)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn save_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(mut req): Json<findb::sales::Quotation>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    if req.no.is_empty() {
        req.no = findb::sales::quo_next_no(&db, req.period)?;
    }
    if req.prepared_by.is_empty() {
        req.prepared_by = user.user.display_name.clone();
    }
    let id = findb::sales::quo_save(&db, &mut req)?;
    Ok(Json(serde_json::json!({ "id": id, "no": req.no })))
}

pub(super) async fn approve_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    // 工作流拦截：有已发布流程 → 先走节点链（Pending 只推进不批单；终态才执行原审批）
    match findb::workflow::intercept(
        &db,
        findb::workflow::BIZ_QUOTATION,
        id,
        &user.user,
        true,
        "",
    )? {
        findb::workflow::Gate::Pending { next } => {
            db.log(
                user.username(),
                "审批",
                "工作流节点",
                &format!("报价#{id} → {next}"),
            )?;
            return Ok(Json(json!({ "ok": true, "pending": next })));
        }
        findb::workflow::Gate::Final { approved: false } => {
            return Ok(Json(json!({ "ok": true, "rejected": true })));
        }
        _ => {}
    }
    findb::sales::quo_approve(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn add_so_shipment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ShipmentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
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
    if qty.is_negative() || qty.is_zero() {
        return Err(AppError::bad_request("发货数量必须为正数"));
    }
    // 对标金蝶：未确认订单不能出库（草稿/作废订单先确认）
    let so =
        findb::scm::so_get(&db, req.so_id)?.ok_or_else(|| AppError::not_found("销售订单不存在"))?;
    if matches!(
        so.status,
        findb::scm::SoStatus::Draft | findb::scm::SoStatus::Cancelled
    ) {
        return Err(AppError::bad_request(
            "订单未确认，不能发货（请先「确认」订单）",
        ));
    }
    // 收入确认凭证 + 发货执行行 + 销售出库流水**同一事务**：
    // 早先拆成 so_income_voucher（自行提交）+ so_shipment_with_stock（另一个事务），
    // 第二步失败（仓库不存在等）时收入与应收已入账提交，却没有任何发货记录，
    // 留下无法自动撤销的孤儿凭证。
    let (id, ivid) = findb::sales::so_shipment_all(
        &db,
        req.so_id,
        period,
        date,
        qty,
        &req.memo,
        &req.warehouse,
        user.username(),
    )?;
    findb::scm::so_progress_update(&db, req.so_id)?;
    // 出库成功 → 完成该订单最早一条待发通知（备货指令闭环）
    let _ = findb::sales::notice_fulfill_on_shipment(&db, req.so_id)?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "voucher_id": ivid }),
    ))
}

pub(super) async fn add_so_return(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ShipmentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
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
    if qty.is_negative() || qty.is_zero() {
        return Err(AppError::bad_request("退货数量必须为正数"));
    }
    // 对标金蝶：未确认订单不能退货
    let so =
        findb::scm::so_get(&db, req.so_id)?.ok_or_else(|| AppError::not_found("销售订单不存在"))?;
    if matches!(
        so.status,
        findb::scm::SoStatus::Draft | findb::scm::SoStatus::Cancelled
    ) {
        return Err(AppError::bad_request(
            "订单未确认，不能退货（请先「确认」订单）",
        ));
    }
    // 收入冲回凭证 + 退货执行行 + 回库流水**同一事务**。
    // 关键收益：超退校验现在在任何写入之前。旧写法先提交收入冲回凭证，
    // 再由 so_return_with_stock 报「退货数量超过净发货」——收入和应收已被冲掉，
    // 却没有退货单，用户只看到一个 400。
    let (id, ivid) = findb::sales::so_return_all(
        &db,
        req.so_id,
        period,
        date,
        qty,
        &req.memo,
        &req.warehouse,
        user.username(),
    )?;
    findb::scm::so_progress_update(&db, req.so_id)?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "voucher_id": ivid }),
    ))
}

pub(super) async fn add_so_payment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SoPaymentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
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
    let amount = parse_money_checked(&req.amount)?;
    // 对标金蝶：收款动作即出台账凭证并按客户自动核销（默认资金账户取账套配置）
    let so =
        findb::scm::so_get(&db, req.so_id)?.ok_or_else(|| AppError::not_found("销售订单不存在"))?;
    // 审核流：先落草稿收付款单（凭证由审核人审核时同事务生成并自动核销）
    //
    // 写入顺序与 add_po_payment 刻意一致：依赖数据（收款流水）先落，草稿收付款单最后建。
    // findb 没有 so_payment_add / receipt_create 的 `_in(tx,…)` 变体，三步无法共事务，
    // 只能靠顺序收敛失败面——建单在前时，建流水失败会留下可被审核并生成付款凭证的孤立草稿单。
    // 残留的非原子性：建单成功后勾稽建边失败，仅缺一条勾稽边，业务数据仍自洽。
    let id = findb::sales::so_payment_add(&db, req.so_id, period, date, amount, &req.memo)?;
    let doc_id = findb::receipt::receipt_create(
        &db,
        "receipt",
        date,
        "",
        &so.customer_code,
        "",
        "",
        fincore::Money::ZERO,
        amount,
        &req.memo,
        user.username(),
    )?;
    // 票↔款勾稽：本订单已下推的销项发票与本次收付款单自动建边
    findb::docflow::link_receipt_to_src_invoice(&db, "so", req.so_id, doc_id)?;
    // 审计日志失败不得把已提交的收款变成 500：客户端重试会重复入账
    if let Err(e) = db.log(
        user.username(),
        "销售",
        "销售收款登记",
        &format!(
            "#{} {} 收款单 #{doc_id}（待审核）",
            req.so_id,
            amount.fmt_money()
        ),
    ) {
        error!(doc_id, error = %e, "写操作日志失败（销售收款登记）");
    }
    Ok(Json(
        json!({ "ok": true, "id": id, "doc_id": doc_id, "status": "draft" }),
    ))
}

/// 销售订单套打 HTML：`ids` 逗号分隔（缺省 = 期间内全部）；`fields` 选字段；`pack=0` 一单一页
pub(super) async fn print_so_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let company = user.company.clone();
    let ids = print_ids(&q);
    let mut orders = Vec::new();
    if ids.is_empty() {
        let period = q
            .get("period")
            .and_then(|s| parse_period(s))
            .unwrap_or_else(|| current_period(&state, &user));
        for o in findb::scm::so_list(&db, period, None)? {
            orders.push(so_to_print(o));
        }
    } else {
        for id in ids {
            let o = findb::scm::so_get(&db, id)?
                .ok_or_else(|| AppError::bad_request(format!("销售订单 #{id} 不存在")))?;
            orders.push(so_to_print(o));
        }
    }
    if orders.is_empty() {
        return Err(AppError::bad_request("没有可打印的订单"));
    }
    let mut f = doc_fields_from_q(&q);
    // 字段级价格权限：无「价格查看」者，套打强制裁掉价格类列（单价/税率/金额/税额/合计）
    if !user.user.can(Perm::PriceView) {
        f.col_price = false;
        f.col_rate = false;
        f.col_amount = false;
        f.col_tax = false;
        f.totals = false;
    }
    let html = findb::printform::order_forms_sized_html(
        &company,
        &orders,
        &f,
        &doc_size_from_q(&q),
        q.get("auto").map(|s| s.as_str() == "1").unwrap_or(false),
        &db.options().print,
    );
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 报价单转销售订单（approved → converted）：生成草稿订单，税率 0 可在订单中再调整
pub(super) async fn convert_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let so_id = findb::sales::quo_to_order(&db, id, user.username())?;
    db.log(
        user.username(),
        "销售",
        "报价转订单",
        &format!("报价 #{id} → 销售订单 #{so_id}"),
    )?;
    Ok(Json(json!({ "ok": true, "so_id": so_id })))
}

// 状态机本身在 findb：scm::so_transition_ok / po_transition_ok 由
// so_set_status / po_set_status 强制，草稿 → 已完成这类跳变由 findb 拒（FinError::state
// → 400）。这里不再复制一份流转表——两份图必然漂移，且 HTTP 层无从得知 findb 何时调整。

pub(super) async fn list_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let mut rows = findb::scm::so_list(&db, period, None)?;
    // 字段级价格权限：无「价格查看」——列表中单价/税额/金额服务端置零（数量保留）
    if !user.user.can(Perm::PriceView) {
        for o in &mut rows {
            o.total_amount = Money::ZERO;
            o.total_tax = Money::ZERO;
            for l in &mut o.lines {
                l.unit_price = Money::ZERO;
                l.tax_rate = Money::ZERO;
                l.amount = Money::ZERO;
                l.tax_amount = Money::ZERO;
            }
        }
    }
    Ok(Json(json!({ "rows": rows })))
}

/// 发货通知（订单确认后）：仓库备货指令，出库后自动完成最早一条待发通知
pub(super) async fn create_notice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<NoticeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let qty = parse_money_checked(&req.qty)?;
    let nid = findb::sales::notice_create(&db, id, qty, date, &req.memo, user.username())?;
    db.log(
        user.username(),
        "销售",
        "发货通知",
        &format!("SO#{id} ×{} 通知#{nid}", qty.fmt_qty()),
    )?;
    Ok(Json(json!({ "ok": true, "id": nid })))
}

/// 待发/历史发货通知列表
pub(super) async fn list_notices(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::sales::notice_list(&*db)? })))
}

/// 保存销售订单：服务端计算行金额（数量×单价）与税额（金额×税率）；
/// 非草稿状态保存时 so_save 内做客户信用检查（对标金蝶卡控）。
pub(super) async fn save_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SoInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let code = req.customer_code.trim();
    if code.is_empty() {
        return Err(AppError::bad_request("客户编码必填"));
    }
    let name = if req.customer_name.trim().is_empty() {
        code.to_string()
    } else {
        req.customer_name.trim().to_string()
    };
    let mut so =
        findb::scm::SalesOrder::new(period, order_date(&req.date), code, &name, user.username());
    so.id = req.id;
    so.memo = req.memo;
    so.status = so_status_parse(&req.status)?;
    // 字段级价格权限：无「价格修改」者——已有行保留原单价/税率，新行置零（数量照常可改）
    let price_ok = user.user.can(Perm::PriceEdit);
    let mut old_price: std::collections::HashMap<String, (Money, Money)> =
        std::collections::HashMap::new();
    if so.id > 0 {
        let old =
            findb::scm::so_get(&db, so.id)?.ok_or_else(|| AppError::not_found("销售订单不存在"))?;
        so.no = old.no;
        so.shipped_amount = old.shipped_amount;
        for l in &old.lines {
            old_price.insert(l.item_code.clone(), (l.unit_price, l.tax_rate));
        }
    } else {
        so.no = findb::scm::so_next_no(&db, period)?;
    }
    for l in &req.lines {
        if l.item_code.trim().is_empty() {
            continue;
        }
        let qty = parse_money_checked(&l.qty_ordered)?;
        let mut price = parse_money_checked(&l.unit_price)?;
        let mut rate = if l.tax_rate.trim().is_empty() {
            Money::ZERO
        } else {
            parse_money_checked(&l.tax_rate)?
        };
        if !price_ok {
            match old_price.get(l.item_code.trim()) {
                Some((op, orate)) => {
                    price = *op;
                    rate = *orate;
                }
                None => {
                    price = Money::ZERO;
                    rate = Money::ZERO;
                }
            }
        }
        let amount = (qty * price).round2();
        let tax = (amount * rate).round2();
        so.lines.push(findb::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: l.item_code.trim().to_string(),
            item_name: if l.item_name.trim().is_empty() {
                l.item_code.trim().to_string()
            } else {
                l.item_name.trim().to_string()
            },
            qty_ordered: qty,
            qty_shipped: if l.qty_shipped.trim().is_empty() {
                Money::ZERO
            } else {
                parse_money_checked(&l.qty_shipped)?
            },
            unit_price: price,
            tax_rate: rate,
            amount,
            tax_amount: tax,
            memo: l.memo.clone(),
        });
    }
    if so.lines.is_empty() {
        return Err(AppError::bad_request("订单至少一行明细"));
    }
    let id = findb::scm::so_save(&db, &mut so)?;
    db.log(
        user.username(),
        "销售",
        "保存销售订单",
        &format!(
            "#{} {} {} 不含税 {}",
            id,
            so.no,
            code,
            so.total_amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "total_amount": so.total_amount,
        "total_tax": so.total_tax
    })))
}

pub(super) async fn transition_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<OrderTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let to = so_status_parse(&req.status)?;
    // 工作流拦截：**只在「确认」这一个动作上**拦。
    // 「确认」是订单的审批动作（签出即对外承诺）；partialShip / completed /
    // cancelled 是确认之后的执行动作，再走一遍审批链等于让仓库收货要财务再批一次。
    if to == findb::scm::SoStatus::Confirmed {
        match workflow_gate(&db, &user, findb::workflow::BIZ_SALES_ORDER, id, "销售订单")? {
            WfGate::Pending(next) => return Ok(Json(json!({ "ok": true, "pending": next }))),
            WfGate::Rejected => return Ok(Json(json!({ "ok": true, "rejected": true }))),
            WfGate::Continue => {}
        }
    }
    // 状态机由 findb 的 so_set_status 把关（非法跳变 → 400）
    findb::scm::so_set_status(&db, id, to)?;
    // 状态已提交：日志失败不得把请求打成 500，否则客户端重试会重复流转
    if let Err(e) = db.log(
        user.username(),
        "销售",
        "销售订单状态",
        &format!("#{} → {}", id, to.label()),
    ) {
        error!(id, error = %e, "写操作日志失败（销售订单状态）");
    }
    Ok(Json(json!({ "ok": true, "status": to.label() })))
}

pub(super) async fn delete_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    findb::scm::so_delete(&db, id)?;
    db.log(user.username(), "销售", "删除销售订单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn get_credit_check(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let customer = q.get("customer").cloned().unwrap_or_default();
    let (receivable, limit, over) =
        findb::sales::credit_check(&db, &customer, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({
        "receivable": receivable.fmt_money(),
        "limit": limit.fmt_money(),
        "over": over,
    })))
}

pub(super) async fn get_so_track(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::sales::so_execution_track(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn get_sales_stats(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::sales::sales_stats(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

/// 通知中心：待办（按岗位实时聚合）+ 动态（审计日志按可见性过滤：AuditLog 权看全量，
/// 否则只看自己的操作）+ 未读计数。since=上次已读水位（前端 localStorage 存服务端 now，
/// 同格式同钟保证字典序=时间序）；缺省=今天内未读。
pub(super) async fn get_notices(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let cur = current_period(&state, &user);
    let todos = findb::workbench::collect_todos(&db, &user.user, cur)?;
    let see_all = user.can(Perm::AuditLog);
    let me = user.username().to_string();
    let events = db
        .recent_logs(50)?
        .into_iter()
        .filter(|l| see_all || l.user == me)
        .collect::<Vec<_>>();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let since = q.get("since").cloned().filter(|s| !s.is_empty());
    let today = now.get(..10).unwrap_or("").to_string();
    // 未读判定只有一处定义：水位之后（缺省=今天内）。
    // 以前前端给每条事件都画未读点、再用 inline style 遮——重渲染就露回来了，
    // 所以「全部已读」看着像没生效。未读必须由服务端按水位判定并逐条下发。
    let is_unread = |ts: &str| -> bool {
        match &since {
            Some(s) => ts > s.as_str(),
            None => ts.starts_with(&today),
        }
    };
    let unread_events = events.iter().filter(|l| is_unread(&l.ts)).count();
    let ev: Vec<serde_json::Value> = events
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "ts": l.ts,
                "user": l.user,
                "module": l.module,
                "action": l.action,
                "detail": l.detail,
                "unread": is_unread(&l.ts),
            })
        })
        .collect();
    Ok(Json(json!({
        "todos": todos,
        "events": ev,
        "unread_events": unread_events,
        "todo_count": todos.iter().filter(|t| t.count > 0).count(),
        "now": now,
    })))
}

pub(super) async fn update_invoice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<InvoiceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = &user.db;
    // 归属校验 + 保留服务端字段：普通用户不能改他人发票，
    // 且表单未提交的 attach_id/created_by/status 不能被覆盖清零
    let existing =
        findb::invoices::get(&db, id)?.ok_or_else(|| AppError::not_found("发票不存在"))?;
    if existing.created_by != user.username() && !user.user.is_admin() {
        return Err(AppError::forbidden("无权修改他人录入的发票"));
    }
    let mut inv = invoice_from_req(&req)?;
    inv.id = id;
    inv.attach_id = existing.attach_id;
    inv.created_by = existing.created_by.clone();
    inv.created_at = existing.created_at.clone();
    if inv.status != existing.status {
        // 状态变更走专用接口（带状态机校验），此处不接受表单覆盖
        inv.status = existing.status.clone();
    }
    findb::invoices::update(&db, &inv)?;
    db.log(user.username(), "发票", "更新", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_invoice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    let existing =
        findb::invoices::get(&db, id)?.ok_or_else(|| AppError::not_found("发票不存在"))?;
    if existing.created_by != user.username() && !user.user.is_admin() {
        return Err(AppError::forbidden("无权删除他人录入的发票"));
    }
    findb::invoices::delete(&db, id)?;
    db.log(user.username(), "发票", "删除", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn get_tax_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    Ok(Json(json!(findb::taxdecl::TaxOptions::load(&*db)?)))
}

pub(super) async fn save_tax_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<findb::taxdecl::TaxOptions>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    // 附加税率上限取 10%，不是 100%：教育费附加法定 3%、地方教育附加各省不超过 2%，
    // 10% 已经远高于任何合法情形，再高基本是「3% 误录成 30%」这类手误。
    // 不设上限的后果很直接：附加税费算成几十倍，申报表上一眼就是错的。
    const MAX_SURTAX_RATE: &str = "0.10";
    for (name, v) in [
        ("教育费附加", &req.edu_rate),
        ("地方教育附加", &req.local_edu_rate),
    ] {
        let m = fincore::Money::parse(v.trim())
            .map_err(|_| AppError::bad_request(format!("{name}税率无法解析：{v}")))?;
        if m.is_negative() {
            return Err(AppError::bad_request(format!("{name}税率不能为负：{v}")));
        }
        if m > fincore::Money::parse(MAX_SURTAX_RATE).unwrap() {
            return Err(AppError::bad_request(format!(
                "{name}税率应在 0（不征）到 {MAX_SURTAX_RATE}（10%）之间，收到：{v}"
            )));
        }
    }
    if !["general", "small"].contains(&req.taxpayer.as_str()) {
        return Err(AppError::bad_request(
            "纳税人身份只能是 general（一般纳税人）或 small（小规模纳税人）",
        ));
    }
    if !["city", "county", "other"].contains(&req.city_tax_zone.as_str()) {
        return Err(AppError::bad_request(
            "城建税适用地区只能是 city / county / other",
        ));
    }
    let db = &user.db;
    req.save(&*db)?;
    db.log(user.username(), "税务", "税务参数", "更新申报表参数")?;
    Ok(Json(json!({ "ok": true, "options": req })))
}

pub(super) async fn get_change_log(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let order_type = q.get("type").cloned().unwrap_or_else(|| "po".to_string());
    let order_id = q.get("id").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let rows = findb::scm2::change_log_list(&db, &order_type, order_id)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn get_tax_vat(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let form = findb::taxdecl::vat_main_form(&db, period)?;
    let opts = findb::taxdecl::TaxOptions::load(&*db)?;
    let company = db.options().company;
    let tax_no = db.options().tax_no;
    Ok(Json(json!({
        "form": form,
        "options": opts,
        "company": company,
        "tax_no": tax_no,
        // 「不代算」的项在 UI 上要能填回去（留痕，不参与计算）
        "manual_fields": ["上期留抵税额", "进项税额转出额", "附加税费减免", "留抵结转下期"],
    })))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(Deserialize)]
pub(super) struct InvoiceFromPo {
    po_id: i64,
}

#[derive(Deserialize)]
pub(super) struct InvoiceFromSo {
    so_id: i64,
}

#[derive(Deserialize, Default)]
pub(super) struct InvoiceListQuery {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub keyword: String,
}

#[derive(Deserialize)]
pub(super) struct ShipmentReq {
    pub so_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub qty: String,
    #[serde(default)]
    pub memo: String,
    /// 仓库编码（留空 = 默认仓）
    #[serde(default)]
    pub warehouse: String,
}

#[derive(Deserialize)]
pub(super) struct SoPaymentReq {
    pub so_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize)]
pub(super) struct SoInput {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    customer_code: String,
    #[serde(default)]
    customer_name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    lines: Vec<OrderLineInput>,
}

#[derive(Deserialize)]
pub(super) struct NoticeReq {
    qty: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}
