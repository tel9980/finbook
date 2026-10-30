//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

/// 逐行列出暂估状态：可暂估额度 / 已暂估 / 还能登多少。
///
/// 收货与暂估之间原本**没有任何联动** —— 收完货不会提示「该登暂估了」，
/// 而登记暂估的金额还要用户手敲（敲错也没人拦）。这个端点把三笔数摆出来，
/// 让界面能显示「可暂估 / 已暂估 / 未暂估」，并可一键带出金额。
pub(super) async fn estimate_status(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let po_id = q
        .get("po_id")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if po_id == 0 {
        return Err(AppError::bad_request("缺少 po_id"));
    }
    let db = &user.db;
    let rows = findb::scm2::estimate_status_by_line(&db, po_id)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn settle_estimate(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let today = chrono::Local::now().date_naive();
    let date = if body.is_empty() {
        today
    } else {
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        match v.get("date").and_then(|d| d.as_str()) {
            Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
            None => today,
        }
    };
    // 冲回同事务生成反向凭证（借 应付 / 贷 存货）
    let vid = findb::scm2::po_estimate_settle(&db, id, date, user.username())?;
    db.log(
        user.username(),
        "采购",
        "暂估冲回",
        &format!(
            "#{id}{}",
            vid.map(|v| format!("，冲回凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(serde_json::json!({ "ok": true, "voucher_id": vid })))
}

pub(super) async fn get_po_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::scm2::po_reconcile(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn get_quota(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let supplier = q.get("supplier").cloned().unwrap_or_default();
    let item = q.get("item").cloned().unwrap_or_default();
    let remaining = findb::scm2::quota_remaining(&db, period, &supplier, &item)?;
    Ok(Json(serde_json::json!({ "remaining": remaining })))
}

pub(super) async fn set_quota(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<QuotaReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    findb::scm2::quota_set(
        &db,
        period,
        &req.supplier,
        &req.item,
        parse_money_checked(&req.quota_qty)?,
    )?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn approve_purchase_req(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    // 工作流拦截：有已发布流程 → 先走节点链，终态才执行原审批
    match findb::workflow::intercept(
        &db,
        findb::workflow::BIZ_PURCHASE_REQ,
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
                &format!("请购#{id} → {next}"),
            )?;
            return Ok(Json(json!({ "ok": true, "pending": next })));
        }
        findb::workflow::Gate::Final { approved: false } => {
            return Ok(Json(json!({ "ok": true, "rejected": true })));
        }
        _ => {}
    }
    findb::procurement::pr_approve(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub(super) async fn add_po_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptReq>,
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
    let id = findb::procurement::po_receipt_with_stock(
        &db,
        &findb::procurement::PoReceipt {
            id: 0,
            po_id: req.po_id,
            period,
            date,
            qty: parse_money_checked(&req.qty)?,
            memo: req.memo,
            item_code: req.item_code,
            warehouse: req.warehouse.clone(),
        },
        &req.warehouse,
    )?;
    // 待检提示：首行存货勾选了来料检验 → 入库为待检状态（质检转正后方可领用）
    let qc_pending = findb::scm::po_get(&db, req.po_id)?
        .and_then(|p| {
            p.lines
                .first()
                .map(|l| findb::business::item_qc_required(&db, &l.item_code))
        })
        .unwrap_or(false);
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "qc_pending": qc_pending }),
    ))
}

pub(super) async fn add_po_return(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptReq>,
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
    let id = findb::procurement::po_return_with_stock(
        &db,
        req.po_id,
        period,
        date,
        parse_money_checked(&req.qty)?,
        &req.memo,
        &req.warehouse,
        &req.item_code,
    )?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

pub(super) async fn add_po_payment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PaymentReq>,
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
    // 对标金蝶：付款动作即出台账凭证并按供应商自动核销（默认资金账户取账套配置）
    let po =
        findb::scm::po_get(&db, req.po_id)?.ok_or_else(|| AppError::not_found("采购订单不存在"))?;
    // 审核流：先落草稿收付款单（凭证由审核人审核时同事务生成并自动核销）
    //
    // 写入顺序刻意是「依赖数据先、单据最后」：findb 目前没有 receipt_create /
    // po_payment_add 的 `_in(tx,…)` 变体，无法开一个事务把三步包在一起，于是只能
    // 调换顺序收敛失败面——先建单、后建付款流水，一旦建付款流水失败就会留下一个
    // 没有任何付款流水的孤立草稿单，而它仍可被
    // `POST /api/funds/receipts/:id/audit` 审核并生成付款凭证。
    // 残留的非原子性：建单成功后若勾稽建边失败，会留下「有付款流水、无勾稽边」的记录，
    // 业务数据本身仍自洽（单据与流水成对），仅勾稽边缺失，可在「票款勾稽」手工补。
    let id = findb::procurement::po_payment_add(
        &db,
        &findb::procurement::PoPayment {
            id: 0,
            po_id: req.po_id,
            period,
            date,
            amount,
            memo: req.memo.clone(),
        },
    )?;
    let doc_id = findb::receipt::receipt_create(
        &db,
        "payment",
        date,
        "",
        &po.supplier_code,
        "",
        "",
        fincore::Money::ZERO,
        amount,
        &req.memo,
        user.username(),
    )?;
    // 票↔款勾稽：本订单已下推进项发票与本次收付款单自动建边
    findb::docflow::link_receipt_to_src_invoice(&db, "po", req.po_id, doc_id)?;
    // 审计日志失败不得把已提交的付款变成 500：客户端重试会重复入账（见 sales_cost_ep 同理）
    if let Err(e) = db.log(
        user.username(),
        "采购",
        "采购付款登记",
        &format!(
            "#{} {} 付款单 #{doc_id}（待审核）",
            req.po_id,
            amount.fmt_money()
        ),
    ) {
        error!(doc_id, error = %e, "写操作日志失败（采购付款登记）");
    }
    Ok(Json(
        json!({ "ok": true, "id": id, "doc_id": doc_id, "status": "draft" }),
    ))
}

pub(super) async fn get_price_history(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let item = q.get("item").cloned().unwrap_or_default();
    let rows = findb::procurement::price_history(&db, &item)?;
    let rows: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(s, p, d)| serde_json::json!({ "supplier": s, "unit_price": p.fmt_money(), "date": d }))
        .collect();
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn get_po_track(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::procurement::po_execution_track(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn get_purchase_stats(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::procurement::purchase_stats(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

/// 采购订单套打 HTML（同销售订单：ids / fields / pack）
pub(super) async fn print_po_form(
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
        for o in findb::scm::po_list(&db, period, None)? {
            orders.push(po_to_print(o));
        }
    } else {
        for id in ids {
            let o = findb::scm::po_get(&db, id)?
                .ok_or_else(|| AppError::bad_request(format!("采购订单 #{id} 不存在")))?;
            orders.push(po_to_print(o));
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

pub(super) async fn list_po(
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
    let mut rows = findb::scm::po_list(&db, period, None)?;
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

/// 保存采购订单（行金额/税额服务端计算，同销售订单）
pub(super) async fn save_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PoInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let code = req.supplier_code.trim();
    if code.is_empty() {
        return Err(AppError::bad_request("供应商编码必填"));
    }
    let name = if req.supplier_name.trim().is_empty() {
        code.to_string()
    } else {
        req.supplier_name.trim().to_string()
    };
    let mut po =
        findb::scm::PurchaseOrder::new(period, order_date(&req.date), code, &name, user.username());
    po.id = req.id;
    po.memo = req.memo;
    po.status = po_status_parse(&req.status)?;
    // 字段级价格权限：无「价格修改」者——已有行保留原单价/税率，新行置零（数量照常可改）
    let price_ok = user.user.can(Perm::PriceEdit);
    let mut old_price: std::collections::HashMap<String, (Money, Money)> =
        std::collections::HashMap::new();
    if po.id > 0 {
        let old =
            findb::scm::po_get(&db, po.id)?.ok_or_else(|| AppError::not_found("采购订单不存在"))?;
        po.no = old.no;
        po.received_amount = old.received_amount;
        for l in &old.lines {
            old_price.insert(l.item_code.clone(), (l.unit_price, l.tax_rate));
        }
    } else {
        po.no = findb::scm::po_next_no(&db, period)?;
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
        po.lines.push(findb::scm::PoLine {
            id: 0,
            po_id: 0,
            item_code: l.item_code.trim().to_string(),
            item_name: if l.item_name.trim().is_empty() {
                l.item_code.trim().to_string()
            } else {
                l.item_name.trim().to_string()
            },
            qty_ordered: qty,
            qty_received: if l.qty_received.trim().is_empty() {
                Money::ZERO
            } else {
                parse_money_checked(&l.qty_received)?
            },
            unit_price: price,
            tax_rate: rate,
            amount,
            tax_amount: tax,
            memo: l.memo.clone(),
        });
    }
    if po.lines.is_empty() {
        return Err(AppError::bad_request("订单至少一行明细"));
    }
    let id = findb::scm::po_save(&db, &mut po)?;
    // 价格历史沉淀（正价行 + 有供应商）：采购编辑器「最近价带出」的数据源
    for l in &po.lines {
        if l.unit_price.is_positive() && !po.supplier_code.trim().is_empty() {
            let _ = findb::procurement::price_history_record(
                &db,
                &l.item_code,
                &po.supplier_code,
                l.unit_price,
                po.date,
            );
        }
    }
    // 审计日志失败不得把已提交的订单变成 500：客户端重试会重复下单
    if let Err(e) = db.log(
        user.username(),
        "采购",
        "保存采购订单",
        &format!(
            "#{} {} {} 不含税 {}",
            id,
            po.no,
            code,
            po.total_amount.fmt_money()
        ),
    ) {
        error!(id, error = %e, "写操作日志失败（保存采购订单）");
    }
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "total_amount": po.total_amount,
        "total_tax": po.total_tax
    })))
}

pub(super) async fn transition_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<OrderTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let to = po_status_parse(&req.status)?;
    // 同 transition_so：只在「确认」这个审批动作上走工作流
    if to == findb::scm::PoStatus::Confirmed {
        match workflow_gate(
            &db,
            &user,
            findb::workflow::BIZ_PURCHASE_ORDER,
            id,
            "采购订单",
        )? {
            WfGate::Pending(next) => return Ok(Json(json!({ "ok": true, "pending": next }))),
            WfGate::Rejected => return Ok(Json(json!({ "ok": true, "rejected": true }))),
            WfGate::Continue => {}
        }
    }
    // 同 transition_so：状态机由 findb 的 po_set_status 把关（非法跳变 → 400）
    findb::scm::po_set_status(&db, id, to)?;
    // 状态已提交：日志失败不得把请求打成 500，否则客户端重试会重复流转
    if let Err(e) = db.log(
        user.username(),
        "采购",
        "采购订单状态",
        &format!("#{} → {}", id, to.label()),
    ) {
        error!(id, error = %e, "写操作日志失败（采购订单状态）");
    }
    Ok(Json(json!({ "ok": true, "status": to.label() })))
}

pub(super) async fn delete_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    findb::scm::po_delete(&db, id)?;
    db.log(user.username(), "采购", "删除采购订单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

// ---- 单据下推与追溯（对标金蝶 源单→目标单） ----

/// 请购单下推采购订单：审批后可推，拆单允许多次（每次新订单 + 新勾稽）
pub(super) async fn push_req_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let (po_id, po_no) = findb::docflow::req_push_po(&db, id, period, user.username())?;
    db.log(
        user.username(),
        "采购",
        "下推采购订单",
        &format!("请购#{id} → 采购订单 {po_no}"),
    )?;
    Ok(Json(json!({ "ok": true, "po_id": po_id, "po_no": po_no })))
}

/// 最近采购价历史（按日期倒序）：采购订单编辑器「单价留空自动带出」数据源
pub(super) async fn price_history_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let item = q.get("item").map(|s| s.trim()).unwrap_or("");
    if item.is_empty() {
        return Err(AppError::bad_request("缺少 item"));
    }
    let rows = findb::procurement::price_history(&db, item)?;
    Ok(Json(json!({
        "rows": rows
            .iter()
            .map(|(s, p, d)| json!({ "supplier": s, "price": p.fmt_qty(), "date": d }))
            .collect::<Vec<_>>()
    })))
}


pub(super) async fn add_estimate(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<EstimateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    // 登记暂估即同事务生成暂估凭证（借 存货 / 贷 应付-订单供应商）
    let (est_id, vid) = findb::scm2::po_estimate_add(
        &db,
        req.po_id,
        period,
        &req.item,
        parse_money_checked(&req.est_amount)?,
        user.username(),
    )?;
    db.log(
        user.username(),
        "采购",
        "登记暂估",
        &format!(
            "PO#{} {} {} 凭证 #{vid}",
            req.po_id, req.item, req.est_amount
        ),
    )?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": est_id, "voucher_id": vid }),
    ))
}


pub(super) async fn list_estimates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let po_id = q
        .get("po_id")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if po_id == 0 {
        return Err(AppError::bad_request("缺少 po_id"));
    }
    let db = &user.db;
    let rows = findb::scm2::po_estimate_list(&db, po_id)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}


// ---- 采购/销售全生命周期：请购 / 报价 / 到货 / 发货 / 付款 / 收款 / 退货 / 信用 ----

pub(super) async fn list_purchase_req(
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
    let rows = findb::procurement::pr_list(&db, period)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}


pub(super) async fn save_purchase_req(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(mut req): Json<findb::procurement::PurchaseReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    if req.no.is_empty() {
        req.no = findb::procurement::pr_next_no(&db, req.period)?;
    }
    if req.requester.is_empty() {
        req.requester = user.user.display_name.clone();
    }
    let id = findb::procurement::pr_save(&db, &mut req)?;
    Ok(Json(serde_json::json!({ "id": id, "no": req.no })))
}
