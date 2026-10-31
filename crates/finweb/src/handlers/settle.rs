//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;
use crate::xlsxout::ColFmt;

/// 往来期初明细列表（账龄页管理区块；合计 ar/ap 分列）
pub(super) async fn list_arap_opening(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let kind = q.get("kind").map(String::as_str).filter(|k| !k.is_empty());
    let rows = findb::settle::arap_opening_list(&db, kind)?;
    // 合计按 kind 显式分列：原来的 `if ar {…} else {ap += …}` 会把空串、大小写笔误
    // （"AR"）等一切非 "ar" 的行默默并进应付合计，让用户看到一个凭空变大的数。
    // 未知 kind 单独计数回报，既不混进 ar/ap 也不因此整页报错（行明细照常返回，
    // 用户能自己看出来是哪几条有问题）。金额一律 fmt_money（fmt_qty 只留 6 位小数
    // 且去尾零，5000.00 会印成 "5000"，与本文件其它金额字段口径不一致）。
    let (mut ar, mut ap) = (Money::ZERO, Money::ZERO);
    let mut unknown_kind = 0usize;
    for r in &rows {
        match r.kind.as_str() {
            "ar" => ar += r.amount,
            "ap" => ap += r.amount,
            _ => unknown_kind += 1,
        }
    }
    Ok(Json(json!({
        "rows": rows,
        "total_ar": ar.fmt_money(),
        "total_ap": ap.fmt_money(),
        "unknown_kind": unknown_kind,
    })))
}

/// 删除往来期初明细（导错可删；会计权）
pub(super) async fn delete_arap_opening(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::settle::arap_opening_delete(&db, id)?;
    db.log(user.username(), "档案", "删除往来期初", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 存货档案一站式列表（独立存货档案页：档案+计划参数+现量+主单位）
// ---- 客户视图 ----------------------------------------------------------
//
// 两个都是**只读**接口。改客户档案仍然只有 `/api/aux` 一个入口 ——
// 这里若也能改，就会出现「销售单据认的客户」与「客户管理里的客户」两份主数据。
pub(super) async fn list_customers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Perm::Report 而不是 Perm::AuxEdit：出纳要看这张名单（催款要用）、
    // 要能核销，但不该能改客户档案。`Report` 在 Role::Cashier 的权限位里，
    // `aux_edit` 不在 —— 这正是设计 §4 要的分界。
    user.require(Perm::Report)?;
    let db = &user.db;
    let per = period_from_query(&state, &user, q.get("period"));
    let keyword = q.get("q").map(String::as_str).unwrap_or("");
    let only_open = matches!(q.get("only_open").map(String::as_str), Some("1" | "true"));
    let page = q
        .get("page")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1);
    let page_size = q
        .get("page_size")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50);
    let result = findb::customers::list(&db, per, keyword, only_open, page, page_size)?;
    Ok(Json(json!({
        "rows": result.rows,
        "period": per.ymm(),
        "total": result.total,
        "page": result.page,
        "page_size": result.page_size,
        "open_count": result.rows.iter().filter(|r| !r.open_amount.is_zero()).count(),
        "open_sum": result
            .rows
            .iter()
            .fold(fincore::Money::ZERO, |a, r| a + r.open_amount)
            .fmt_money(),
    })))
}

pub(super) async fn get_customer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let per = period_from_query(&state, &user, q.get("period"));
    let code = code.trim().to_string();
    // 编码空的处理：返回 400 而不是「查不到」。
    // `Path("")` 打不进这个路由（路由就匹配不到），但显式拒绝更清楚，
    // 且将来若加了 `/api/customers/` 这样的写法不会被静默吞掉。
    if code.is_empty() {
        return Err(AppError::bad_request("缺少客户编码 code"));
    }
    match findb::customers::detail(&db, &code, per)? {
        Some(d) => Ok(Json(json!({
            "customer": d,
            "period": per.ymm(),
            "credit": findb::sales::credit_check(&db, &code, per)
                .map(|(recv, lim, over)| json!({
                    "receivable": recv.fmt_money(),
                    "limit": lim.fmt_money(),
                    "over": over,
                }))
                .unwrap_or(serde_json::Value::Null),
        }))),
        None => Err(AppError::not_found(format!("客户 {code} 未建档"))),
    }
}

/// 客户账龄。桶与算法全部来自 `findb::customers::aging_of` → `settle::aging`，
/// 本 handler **不参与任何计算**（只挑桶标签与金额）。
pub(super) async fn get_customer_aging(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let per = period_from_query(&state, &user, q.get("period"));
    let code = code.trim().to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少客户编码 code"));
    }
    // 账龄页与客户页对「客户不存在」的处理必须一致：一个不存在的客户，
    // 详情页报 404，账龄 Tab 也得报 404 —— 否则界面上会出现
    // 「有档案、明细有数据、账龄说此人没有未核销余额」这种自相矛盾的三件套。
    if findb::auxs::get(&db, fincore::AuxKind::Customer, &code)?.is_none() {
        return Err(AppError::not_found(format!("客户 {code} 未建档")));
    }
    // as_of 缺省用**期末**而不是今天：as_of 决定「账龄天数怎么算」，
    // 用今天算历史期间会得到一份「天数按今天算」的老账龄。
    // （账龄页 /settle/aging 用的是今天，两处口径不同是有意的：
    //   那页是「当前视角」，这页是「这个期间视角」。文档 §3.2 有记。）
    let as_of = match q.get("as_of").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request(format!("as_of 不是合法日期：{s}")))?,
        None => per.last_day(),
    };
    let buckets = if q.get("scheme").map(|s| s == "year").unwrap_or(false) {
        fincore::engine::aging::buckets_by_year()
    } else {
        fincore::engine::aging::buckets_by_days()
    };
    let a = findb::customers::aging_of(&db, &code, per, as_of, &buckets)?;
    // 金额一律走 `fmt_money()`，与 `/api/settle/aging` **同一个格式化**。
    // 直接把 `Money` 序列化出去会得到 `"3000.00"` / `"0"`（无千分位、零值没有
    // 两位小数），而账龄页给的是 `"3,000.00"` / `"0.00"` —— 同一份数据两种长相，
    // 用户把两个页面对着看会以为算错了。
    Ok(Json(json!({
        "code": code,
        "period": per.ymm(),
        "as_of": as_of.format("%Y-%m-%d").to_string(),
        "aging": {
            "buckets": a.buckets,
            "amounts": a.amounts.iter().map(|m| m.fmt_money()).collect::<Vec<_>>(),
            "total": a.total.fmt_money(),
            "credit_total": a.credit_total.fmt_money(),
            "net": (a.total - a.credit_total).fmt_money(),
            "max_days": a.max_days,
            "empty": a.empty,
        },
    })))
}

/// 客户交易历史：报价 / 销售订单 / 发货 / 收款。
///
/// 期间口径：`to` 缺省用 `period`（会话当前期间），`from` 缺省用 `to` 的**年初**
/// —— 与资产负债表第二列「年初余额」同一个理由：交易历史看的是「本年买过什么」，
/// 默认给一个 12 个月的窗口，而不是单月。
/// 只读，权限与另外两个客户视图一致（`Perm::Report`）。
pub(super) async fn get_customer_trade(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let code = code.trim().to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少客户编码 code"));
    }
    if findb::auxs::get(&db, fincore::AuxKind::Customer, &code)?.is_none() {
        return Err(AppError::not_found(format!("客户 {code} 未建档")));
    }
    let per = period_from_query(&state, &user, q.get("period"));
    let to = match q.get("to").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => per,
        Some(s) => Period::from_ymm_checked(period_checked_digits(s))
            .map_err(|_| AppError::bad_request(format!("to 不是合法期间：{s}")))?,
    };
    let from = match q.get("from").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Period::new(to.year(), 1).unwrap_or(to),
        Some(s) => Period::from_ymm_checked(period_checked_digits(s))
            .map_err(|_| AppError::bad_request(format!("from 不是合法期间：{s}")))?,
    };
    let rows = findb::customers::trade(&db, &code, from, to)?;
    Ok(Json(json!({
        "code": code,
        "from": from.ymm(),
        "to": to.ymm(),
        "rows": rows
            .iter()
            .map(|r| json!({
                "date": r.date,
                "kind": r.kind,
                "kind_label": r.kind_label,
                "doc_no": r.doc_no,
                "summary": r.summary,
                "amount": r.amount.fmt_money(),
                "qty": r.qty.fmt_money(),
                "status": r.status,
                "status_label": r.status_label,
            }))
            .collect::<Vec<_>>(),
        "total": rows.len(),
    })))
}

/// 客户催款记录：催款单 + 往来对账单。只读展示，状态由各自模块决定。
pub(super) async fn get_customer_dunning(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let code = code.trim().to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少客户编码 code"));
    }
    if findb::auxs::get(&db, fincore::AuxKind::Customer, &code)?.is_none() {
        return Err(AppError::not_found(format!("客户 {code} 未建档")));
    }
    let rows = findb::customers::dunning_records(&db, &code)?;
    Ok(Json(json!({
        "code": code,
        "rows": rows
            .iter()
            .map(|r| json!({
                "date": r.date,
                "kind": r.kind,
                "kind_label": r.kind_label,
                "no": r.no,
                "summary": r.summary,
                "amount": r.amount.fmt_money(),
                "item_count": r.item_count,
                "status": r.status,
                "status_label": r.status_label,
            }))
            .collect::<Vec<_>>(),
        "total": rows.len(),
    })))
}

pub(super) async fn auto_settle_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SettleAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = if req.ymm > 0 {
        period_checked(req.ymm)?
    } else {
        current_period(&state, &user)
    };
    let tolerance = if req.tolerance.trim().is_empty() {
        Money::parse("0.01").unwrap_or(Money::ZERO)
    } else {
        parse_money_checked(&req.tolerance)?
    };
    let db = &user.db;
    let r = findb::settle::auto_settle(&db, &account, upto, tolerance, user.username())?;
    db.log(
        user.username(),
        "往来",
        "自动核销",
        &format!("{account} {} 对 {}", r.pairs, r.amount.fmt_money()),
    )?;
    Ok(Json(json!({
        "pairs": r.pairs,
        "amount": r.amount.fmt_money(),
        "exact": r.exact,
        "written_off": r.written_off,
    })))
}

pub(super) async fn manual_settle_endpoint(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SettleRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let amount = parse_money_checked(&req.amount)?;
    let diff = if req.diff.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&req.diff)?
    };
    let db = &user.db;
    // 差异为 0 时 settle_with_diff 内部退化成普通核销，不必分支
    let r = findb::settle::settle_with_diff(
        &db,
        req.from_entry,
        req.to_entry,
        amount,
        diff,
        &req.diff_account,
        user.username(),
    )?;
    if diff.is_positive() {
        db.log(
            user.username(),
            "往来",
            "差异核销",
            &format!(
                "分录#{} ↔ 分录#{} {}（差异 {} 记入 {}，凭证 #{:?}）",
                req.from_entry,
                req.to_entry,
                amount.fmt_money(),
                diff.fmt_money(),
                req.diff_account.trim(),
                r.diff_voucher_id
            ),
        )?;
    } else {
        db.log(
            user.username(),
            "往来",
            "手工核销",
            &format!(
                "分录#{} ↔ 分录#{} {}",
                req.from_entry,
                req.to_entry,
                amount.fmt_money()
            ),
        )?;
    }
    Ok(Json(
        json!({ "id": r.settle_id, "diff_voucher_id": r.diff_voucher_id }),
    ))
}

/// 通用往来互抵：跨往来单位 / 跨科目（代收代付、三方抵账）。
/// 自动生成一张过渡凭证把两边接上，再各自核销。
pub(super) async fn cross_settle_endpoint(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SettleCrossReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let amount = parse_money_checked(&req.amount)?;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let db = &user.db;
    let r = findb::settle::settle_cross(
        &db,
        req.debit_entry,
        req.credit_entry,
        amount,
        date,
        user.username(),
    )?;
    db.log(
        user.username(),
        "往来",
        "跨单位互抵",
        &format!(
            "借#{} ↔ 贷#{} {}（过渡凭证 #{:?}）",
            req.debit_entry,
            req.credit_entry,
            amount.fmt_money(),
            r.bridge_voucher_id
        ),
    )?;
    Ok(Json(json!({
        "bridge_voucher_id": r.bridge_voucher_id,
        "settle_ids": r.settle_ids,
    })))
}

pub(super) async fn list_offsets(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::offset::offset_list(&*db)? })))
}

/// 新增往来对冲单：只落草稿单据；凭证与两端 FIFO 核销在「审核」时生成
pub(super) async fn create_offset(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OffsetCreateReq>,
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
    let id = findb::offset::offset_create(
        &db,
        date,
        &req.customer,
        &req.supplier,
        &req.ar_account,
        &req.ap_account,
        amount,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "往来",
        "新增往来对冲单（待审核）",
        &format!(
            "#{id} 客户{} ↔ 供应商{} {}",
            req.customer.trim(),
            req.supplier.trim(),
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "status": "draft" })))
}

/// 审核往来对冲单：生成凭证（借 应付 / 贷 应收）+ 两端 FIFO 核销。
/// 判据同资金单据（`require_fund_doc_gate`：VoucherAudit 或 VoucherPost）
pub(super) async fn audit_offset(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    let (vid, n) = findb::offset::offset_audit(&db, id, user.username())?;
    db.log(
        user.username(),
        "往来",
        "审核往来对冲单",
        &format!("#{id} 凭证 #{vid} 核销 {n} 笔"),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid, "settled": n })))
}

/// 撤销审核：删除未记账凭证（含核销清理）→ 单据回草稿（判据与审核一致）
pub(super) async fn unaudit_offset(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    findb::offset::offset_unaudit(&db, id)?;
    db.log(
        user.username(),
        "往来",
        "撤销审核往来对冲单",
        &format!("#{id}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn delete_offset(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::offset::offset_delete(&db, id)?;
    db.log(user.username(), "往来", "删除往来对冲单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn list_settle_records(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let db = &user.db;
    let rows: Vec<serde_json::Value> = findb::settle::list(&db, account.trim())?
        .iter()
        .map(|r| {
            json!({
                "id": r.id, "period": period_to_str(r.period), "account": r.account_code,
                "aux_key": r.aux_key, "from_entry": r.from_entry, "to_entry": r.to_entry,
                "amount": r.amount.fmt_money(), "settled_by": r.settled_by, "settled_at": r.settled_at,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
}

pub(super) async fn unsettle_endpoint(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<UnsettleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    // 带了金额就是部分撤销；没带或为空串就是整条撤销
    let part = match req.amount.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => Some(
            fincore::Money::parse(s)
                .map_err(|e| AppError::bad_request(format!("撤销金额「{s}」不是合法金额：{e}")))?,
        ),
        _ => None,
    };
    match part {
        Some(amount) => {
            let left = findb::settle::unsettle_part(&db, req.id, amount)?;
            db.log(
                user.username(),
                "往来",
                "部分撤销核销",
                &format!("记录#{} 撤销 {amount}，余 {left}", req.id),
            )?;
            Ok(Json(json!({ "ok": true, "left": left.fmt_plain() })))
        }
        None => {
            findb::settle::unsettle(&db, req.id)?;
            db.log(
                user.username(),
                "往来",
                "取消核销",
                &format!("记录#{}", req.id),
            )?;
            Ok(Json(json!({ "ok": true, "left": "0.00" })))
        }
    }
}

pub(super) async fn get_settle_aging(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = q
        .get("upto")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    // 基准日缺省 = 查询期间的**最后一天**，不是「今天」。
    //
    // 桌面端一直是 `upto.last_day()`（crates/finui/src/views/settle_view.rs:208）。
    // 网页端缺省「今天」的后果：查 2026-03 的账龄，所有单据都显示成「210 天」，
    // 全落在最老的那一档；同一份数据桌面端显示 3 天、网页显示 210 天，
    // 导出的 Excel 跟网页一致 —— 两个答案，谁也不敢拿去给老板看。
    let as_of = q
        .get("as_of")
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| upto.last_day());
    let by_year = q.get("scheme").map(|s| s == "year").unwrap_or(false);
    let buckets = if by_year {
        fincore::engine::aging::buckets_by_year()
    } else {
        fincore::engine::aging::buckets_by_days()
    };
    let db = &user.db;
    // 数据范围：科目不在可见范围就当没这个科目。
    //
    // 用 403 而不是「返回空清单」：空清单会让会计以为「这户真的没欠款」，
    // 而真相是「你没有这个科目的权限」——两种情况的下一步动作完全不同。
    if !user.user.can_see_account(account.trim()) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let lines = findb::settle::aging(&db, account.trim(), upto, as_of, &buckets, None)?;
    let labels: Vec<&str> = buckets.iter().map(|b| b.label).collect();
    // key 是**裸编码**（`C01`），名称另查 —— 页面上显示 `customer=C01` 是内部存储格式，
    // 对用户没有意义。以前 key 本身就是整条 aux_key，前端想显示名称也做不到。
    let party = findb::settle::party_kind_in(&db, account.trim());
    let names: std::collections::HashMap<String, String> =
        findb::auxs::list(&db, &fincore::AuxQuery::kind(party))?
            .into_iter()
            .map(|a| (a.code, a.name))
            .collect();
    let rows: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| {
            json!({
                "key": l.key,
                "name": names.get(&l.key).cloned().unwrap_or_default(),
                "amounts": l.amounts.iter().map(|m| m.fmt_money()).collect::<Vec<_>>(),
                "total": l.total.fmt_money(),
                "credit_total": l.credit_total.fmt_money(),
                "net": l.net().fmt_money(),
                "max_days": l.max_days,
                "pay_days": l.pay_days,
                "overdue_days": l.overdue_days,
            })
        })
        .collect();
    Ok(Json(json!({
        "account": account.trim(),
        "as_of": as_of.format("%Y-%m-%d").to_string(),
        "buckets": labels,
        "rows": rows,
    })))
}

/// 往来账龄**导出成 Excel**（同一份取数，只是换成文件）。
///
/// 为什么单做一件事：账龄表现在只能截图。老板要发给客户、或者存档给会计，
/// 截图既不能改也不能算。这里与 `/api/settle/aging` 走同一条取数路径
/// （`findb::settle::aging`），所以界面上的数字与文件里的数字不会分叉。
pub(super) async fn export_settle_aging_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let account = q.get("account").cloned().unwrap_or_else(|| "1122".into());
    let account = account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = q
        .get("upto")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let as_of = q
        .get("as_of")
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| chrono::Local::now().date_naive());
    let by_year = q.get("scheme").map(|s| s == "year").unwrap_or(false);
    let buckets = if by_year {
        fincore::engine::aging::buckets_by_year()
    } else {
        fincore::engine::aging::buckets_by_days()
    };
    let db = &user.db;
    // 数据范围：科目不在可见范围就当没这个科目。
    //
    // 用 403 而不是「返回空清单」：空清单会让会计以为「这户真的没欠款」，
    // 而真相是「你没有这个科目的权限」——两种情况的下一步动作完全不同。
    if !user.user.can_see_account(account.trim()) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let lines = findb::settle::aging(db, &account, upto, as_of, &buckets, None)?;
    let party = findb::settle::party_kind_in(db, &account);
    let names: std::collections::HashMap<String, String> =
        findb::auxs::list(db, &fincore::AuxQuery::kind(party))?
            .into_iter()
            .map(|a| (a.code, a.name))
            .collect();

    let mut headers: Vec<String> = vec![
        if party == fincore::AuxKind::Customer {
            "客户编码".into()
        } else {
            "供应商编码".into()
        },
        if party == fincore::AuxKind::Customer {
            "客户名称".into()
        } else {
            "供应商名称".into()
        },
    ];
    for b in &buckets {
        headers.push(b.label.to_string());
    }
    headers.push("往来余额".into());
    headers.push("其中预收预付".into());
    headers.push("净额".into());
    headers.push("最久天数".into());
    headers.push("账期".into());
    headers.push("逾期天数".into());

    let mut sheet = crate::xlsxout::Sheet::plain(
        "往来账龄",
        &headers.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &{
            let mut c = vec![ColFmt::Text, ColFmt::Text];
            for _ in &buckets {
                c.push(ColFmt::Money);
            }
            c.push(ColFmt::Money);
            c.push(ColFmt::Money);
            c.push(ColFmt::Money);
            c.push(ColFmt::Int);
            // 账期（没填就空着，不要写 0 —— 0 天账期和「没约定」是两件事）
            c.push(ColFmt::Int);
            c.push(ColFmt::Int);
            c
        },
    );
    let mut rows: Vec<Vec<String>> = Vec::new();
    let (mut t_sum, mut t_net, mut t_credit) = (Money::ZERO, Money::ZERO, Money::ZERO);
    let mut bucket_sum = vec![Money::ZERO; buckets.len()];
    for l in &lines {
        let mut r = vec![
            l.key.clone(),
            names.get(&l.key).cloned().unwrap_or_default(),
        ];
        for (i, m) in l.amounts.iter().enumerate() {
            r.push(m.fmt_plain());
            if let Some(slot) = bucket_sum.get_mut(i) {
                *slot += *m;
            }
        }
        r.push(l.total.fmt_plain());
        r.push(l.credit_total.fmt_plain());
        r.push(l.net().fmt_plain());
        r.push(l.max_days.to_string());
        r.push(l.pay_days.map(|d| d.to_string()).unwrap_or_default());
        r.push(l.overdue_days.to_string());
        t_sum += l.total;
        t_credit += l.credit_total;
        t_net += l.net();
        rows.push(r);
    }
    let mut total_row = vec!["".to_string(), "合计".to_string()];
    for m in &bucket_sum {
        total_row.push(m.fmt_plain());
    }
    total_row.push(t_sum.fmt_plain());
    total_row.push(t_credit.fmt_plain());
    total_row.push(t_net.fmt_plain());
    total_row.push(String::new());
    total_row.push(String::new());
    total_row.push(String::new());
    rows.push(total_row);
    let em = vec![rows.len() - 1];
    sheet.rows = rows;
    sheet = sheet.with_emphasis(em);
    // 导出是数据外带：应收/应付会计现在也有这个权限了，所以要留痕 ——
    // 谁在什么时候把多少家的往来余额带出去，事后要查得到。
    db.log(
        user.username(),
        "导出",
        "导出往来账龄",
        &format!(
            "{} 至 {} · 科目 {} · {} 行",
            upto.label(),
            as_of,
            account,
            lines.len()
        ),
    )?;
    let fname = format!("往来账龄_{}_{}.xlsx", account, as_of.format("%Y%m%d"));
    build_xlsx(&[sheet], &fname, "往来账龄")
}

/// 催款单状态流转：draft → sent/settled/cancelled；sent → settled/cancelled
pub(super) async fn dunning_status_ep(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<DunningStatusReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    if findb::settle::dunning_get(&db, id)?.is_none() {
        return Err(AppError::not_found("催款单不存在"));
    }
    findb::settle::dunning_status(&db, id, req.status.trim())?;
    db.log(
        user.username(),
        "应收",
        "催款单状态",
        &format!("#{id} → {}", req.status.trim()),
    )?;
    Ok(Json(json!({ "ok": true })))
}

/// 催款函 / 对账函打印页（一页 A4，可直接发给客户）
///
/// 以前催款单只能导出 Excel：一张表，客户收到不知道这是一封函。这页把它渲染成有
/// 抬头、有致语、有落款、有合计的文书；纸张方向/边距/字号/落款开关仍读账套参数。
pub(super) async fn print_dunning(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let Some(d) = findb::settle::dunning_get(db, id)? else {
        return Err(AppError::not_found("找不到这张催款单"));
    };
    let po = print_opts_from_q(db.options().print, &q);
    let html = crate::report_html::dunning_letter_html(&d, &user.company, "元", &po);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

pub(super) async fn list_begin(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<BeginRow>>, AppError> {
    user.require(Perm::Opening)?;
    let db = &user.db;
    // 数据范围：科目区间外的期初不可见（与账簿/报表同口径）
    let rows = balances::list_begin(&*db)?
        .into_iter()
        .filter(|r| user.user.can_see_account(&r.account_code))
        .collect::<Vec<_>>();
    Ok(Json(rows))
}

pub(super) async fn save_begin(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BeginSaveReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Opening)?;
    let db = &user.db;
    let (rows, delete_ids) = match req {
        BeginSaveReq::Wrapped { rows, delete_ids } => (rows, delete_ids),
        BeginSaveReq::Rows(rows) => (rows, Vec::new()),
    };
    // 数据范围：范围外的科目不得写期初（防越权维护）
    for r in &rows {
        let code = r.account_code.trim();
        if !code.is_empty() && !user.user.can_see_account(code) {
            return Err(AppError::forbidden(format!(
                "无权维护科目 {code} 的期初余额"
            )));
        }
    }
    // 科目必须真实存在。
    //
    // 为什么这道必须有：期初是所有报表的起点。凭证录入那条路上有「非末级科目不能
    // 记账」「核算数量科目必须填数量」「借贷不平衡」一整套校验，唯独期初这条路
    // 原来是裸的。实测写一个不存在的 `999999` → 200 ok → 试算平衡**没有这一行**、
    // 总额照样报「平衡」→ 资产负债表也查不到。也就是**钱从所有报表里静默消失，
    // 而唯一的兜底守卫（试算平衡）报告一切正常**。会计把科目编码打错一位就踩到。
    //
    // 只校验「存在」，不校验「是不是末级」：期初按汇总层科目（1405 库存商品、
    // 1122 应收账款这类）建余额是常规做法。
    let codes: Vec<String> = rows
        .iter()
        .map(|r| r.account_code.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    if !codes.is_empty() {
        let mut missing: Vec<String> = Vec::new();
        for c in &codes {
            if accounts::get(&db, c)?.is_none() && !missing.contains(c) {
                missing.push(c.clone());
            }
        }
        if !missing.is_empty() {
            return Err(AppError::bad_request(format!(
                "科目 {} 不存在，期初未保存。请先在「会计科目」建好该科目，或检查编码是否输错",
                missing.join("、")
            )));
        }
    }
    // 期初整批导入必须原子：upsert_begin 里是裸 conn.execute，每行各自成隐式事务，
    // 第 k 行失败时前 k-1 行已经提交，客户端既无法判断落地了哪些行，也无法安全重试。
    // 改为在调用方事务里逐行 upsert_begin_on（事务可 Deref 成 Connection），
    // 整批要么全成、要么全回滚；操作日志同事务写入，不会出现"数据进了、日志没进"。
    let tx = db.write_tx()?;
    // 先删后写，且与写入同事务：任一步失败整体回滚，不会出现「删了但没补上」
    let mut deleted = 0usize;
    for id in &delete_ids {
        if *id > 0 {
            balances::delete_begin_on(&tx, *id)?;
            deleted += 1;
        }
    }
    let mut n = 0;
    for r in rows {
        let code = r.account_code.trim();
        if code.is_empty() {
            continue;
        }
        let yb = parse_money_checked(&r.yb)?;
        let year_begin = match r.dir {
            Direction::Debit => yb,
            Direction::Credit => -yb,
        };
        let br = BeginRow {
            id: 0,
            account_code: code.to_string(),
            aux: r.aux,
            year_begin,
            debit_accum: parse_money_checked(&r.ad)?,
            credit_accum: parse_money_checked(&r.ac)?,
            qty_begin: r
                .qty
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                .map(|s| parse_money_checked(s))
                .transpose()?,
        };
        balances::upsert_begin_on(&tx, &br)?;
        n += 1;
    }
    findb::log_on(
        &tx,
        user.username(),
        "期初",
        "保存期初余额",
        &format!(
            "保存 {} 条{}",
            n,
            if deleted > 0 {
                format!("，删除 {deleted} 条")
            } else {
                String::new()
            }
        ),
    )?;
    tx.commit().map_err(findb::DbError::from)?;
    Ok(Json(json!({"ok": true, "count": n, "deleted": deleted})))
}

/// 坏账准备计提：按应收账龄生成「借 资产减值损失 / 贷 坏账准备」凭证
pub(super) async fn bad_debt_provision_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = chrono::Local::now().date_naive();
    match findb::settle::bad_debt_provision_voucher(&db, period, date, user.username())? {
        Some(id) => {
            db.log(
                user.username(),
                "往来",
                "计提坏账准备",
                &format!("凭证 #{id}"),
            )?;
            Ok(Json(json!({"ok": true, "voucher_id": id})))
        }
        None => Ok(Json(
            json!({"ok": true, "voucher_id": 0, "message": "无可计提的坏账准备"}),
        )),
    }
}

/// 对账单列表（可按 kind=ar|ap 过滤）
pub(super) async fn list_statements(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let rows = findb::statement::list(&db, kind, None)?;
    Ok(Json(json!({
        "rows": rows.iter().map(statement_json).collect::<Vec<_>>(),
    })))
}

/// 生成对账单（服务端算期初/本期发生/期末并快照，不信前端金额）
pub(super) async fn create_statement(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<StatementReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let from = period_checked(req.from)?;
    let to = period_checked(req.to)?;
    let s = findb::statement::create(
        &db,
        &req.kind,
        &req.account,
        &req.party_code,
        &req.party_name,
        from,
        to,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "往来",
        "生成对账单",
        &format!(
            "{} {} {}~{}",
            s.no,
            s.kind_label(),
            s.party_code,
            period_to_str(to)
        ),
    )?;
    Ok(Json(statement_json(&s)))
}

/// 对账单状态流转：发出 / 客户回签确认 / 作废
pub(super) async fn statement_status_ep(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<StatementStatusReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    // 回签确认必须留痕是谁确认的（客户签字 or 代签人），不能空着
    let who = if req.status == "confirmed" {
        let c = req.confirmed_by.trim();
        if c.is_empty() {
            return Err(AppError::bad_request("回签确认必须填写确认人"));
        }
        c
    } else {
        user.username()
    };
    findb::statement::set_status(&db, id, &req.status, who)?;
    db.log(
        user.username(),
        "往来",
        "对账单状态",
        &format!(
            "#{} → {}（确认人 {}）",
            id,
            findb::statement::status_label(&req.status),
            who
        ),
    )?;
    let s = findb::statement::get(&db, id)?.ok_or_else(|| AppError::not_found("对账单"))?;
    Ok(Json(statement_json(&s)))
}

/// 催款单/对账函列表（可按 kind=ar|ap 过滤）
pub(super) async fn list_dunnings(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::settle::dunning_list(&db, q.get("kind").map(String::as_str), None)?;
    Ok(Json(json!({ "rows": rows })))
}

/// 生成催款单：按客商快照未核销分录 + 往来期初（服务端计算，无欠款拒绝）
pub(super) async fn create_dunning(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<DunningReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let d = findb::settle::dunning_create(
        &db,
        &req.kind,
        &req.account,
        &req.party_code,
        &req.party_name,
        date,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "应收",
        "催款单",
        &format!(
            "{} {} {} 共 {} 笔（{}）",
            d.no,
            d.party_code,
            d.amount.fmt_money(),
            d.item_count,
            if d.kind == "ar" {
                "催款"
            } else {
                "对账函"
            }
        ),
    )?;
    Ok(Json(json!({ "ok": true, "dunning": d })))
}

/// 期初批量粘贴：解析（预览）
pub(super) async fn parse_begin_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BeginBatchParseReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let (dirs, leaves) = chart_ctx(&*db)?;
    let dir_of = |code: &str| -> Option<Direction> {
        match dirs.get(code).map(|s| s.as_str()) {
            Some("debit") => Some(Direction::Debit),
            Some("credit") => Some(Direction::Credit),
            _ => None,
        }
    };
    // 库里已有期初数的编码 —— 别覆盖用户填过的
    let existing: Vec<String> = balances::list_begin(&*db)?
        .into_iter()
        .map(|r| r.account_code)
        .collect();
    let parsed = fincore::batch::begin::parse(&req.text, &dir_of, &leaves, &existing);

    // 数据范围：范围外的科目连「能导入」都不该出现在预览里 ——
    // 否则用户会看到一条他无权写、点了才报错的行
    let rows: Vec<serde_json::Value> = parsed
        .rows
        .iter()
        .filter(|r| user.user.can_see_account(&r.code))
        .map(|r| {
            json!({
                "code": r.code,
                "dir": match r.dir { Direction::Debit => "debit", Direction::Credit => "credit" },
                "dir_label": r.dir.label(),
                "amount": r.amount.fmt_money(),
                "skip": r.skip.as_ref().map(|s| s.label()),
            })
        })
        .collect();
    let importable = parsed
        .rows
        .iter()
        .filter(|r| r.skip.is_none() && user.user.can_see_account(&r.code))
        .count();
    let (d, c, diff) = parsed.trial();
    Ok(Json(json!({
        "rows": rows,
        "total": rows.len(),
        "importable": importable,
        "skipped": rows.len() - importable,
        "empty": parsed.is_empty(),
        "trial": {
            "debit": d.fmt_money(),
            "credit": c.fmt_money(),
            "diff": diff.fmt_money(),
            "balanced": diff.is_zero(),
        },
    })))
}

/// 期初批量粘贴：导入前端确认过的行
///
/// 走和「保存」一样的校验路径（`upsert_begin` + 越权检查），而不是直接写 ——
/// 批量导入的量更大，但它并不比手工一行行填更特殊，特殊的是**一次能错几百行**。
pub(super) async fn import_begin_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BeginBatchImportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Opening)?;
    let db = &user.db;
    let (dirs, leaves) = chart_ctx(&*db)?;
    let mut seen: Vec<String> = Vec::new();
    let mut to_save: Vec<BeginRow> = Vec::new();
    let mut skipped: Vec<serde_json::Value> = Vec::new();

    for r in &req.rows {
        let code = r.code.trim().to_string();
        if code.is_empty() {
            continue;
        }
        if !user.user.can_see_account(&code) {
            return Err(AppError::forbidden(format!(
                "无权维护科目 {code} 的期初余额"
            )));
        }
        if !leaves.contains(&code) && !dirs.contains_key(&code) {
            skipped.push(json!({"code": code, "why": "无此科目"}));
            continue;
        }
        if seen.contains(&code) || req.existing.contains(&code) {
            skipped.push(json!({"code": code, "why": "已填过"}));
            continue;
        }
        seen.push(code.clone());
        let dir = if r.dir == "credit" {
            Direction::Credit
        } else {
            Direction::Debit
        };
        let amt = Money::parse_or_zero(&r.amount);
        if amt.is_zero() {
            skipped.push(json!({"code": code, "why": "金额为零"}));
            continue;
        }
        to_save.push(BeginRow {
            id: 0,
            account_code: code,
            aux: AuxRef::default(),
            year_begin: fincore::dir_amount_to_signed(dir, amt),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        });
    }

    if to_save.is_empty() {
        return Ok(json!({
            "ok": true, "inserted": 0, "skipped": skipped.len(), "detail": skipped
        })
        .into());
    }

    let n = to_save.len();
    for br in &to_save {
        balances::upsert_begin(&db, br)?;
    }
    db.log(
        user.username(),
        "期初",
        "批量粘贴期初余额",
        &format!("{n} 行"),
    )?;
    Ok(json!({
        "ok": true, "inserted": n, "skipped": skipped.len(), "detail": skipped
    })
    .into())
}

const DEFAULT_SETTLE_OPEN_LIMIT: usize = 200;
const MAX_SETTLE_OPEN_LIMIT: usize = 2000;

pub(super) async fn get_settle_open(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    // 数据范围：科目不在可见范围就当没这个科目。
    //
    // 用 403 而不是「返回空清单」：空清单会让会计以为「这户真的没欠款」，
    // 而真相是「你没有这个科目的权限」——两种情况的下一步动作完全不同。
    if !user.user.can_see_account(account.trim()) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let upto = q
        .get("upto")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let include_all = q
        .get("all")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(false);
    // 「含未记账」只读开关（对标金蝶单据驱动模型：未审核单据在列表里看得见，
    // 但不参与核销）。默认关闭——核销/账龄/催款一律只认已记账（H-3）。
    // 注意：即使打开，这里返回的未记账行也只是**展示**，核销入口走
    // `auto_settle`/`settle`，它们固定用 posted-only 的 `open_entries`。
    let include_draft = q
        .get("draft")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(false);
    // 可选的往来单位过滤（customer=C01 / supplier=S01），推送到 SQL 层面过滤
    let party_filter = q.get("party").map(|s| s.trim()).filter(|s| !s.is_empty());
    // 关键字过滤：单据号 / 摘要 / 往来单位编码。核销时会计常常只记得「那张 0031 号单」，
    // 让他能在 200 行里捞，而不是让前端去遍历全量（大账套那是 700KB）。
    let kw = q
        .get("q")
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let db = &user.db;
    let mut rows = findb::settle::open_entries_with(
        &db,
        account.trim(),
        upto,
        include_all,
        !include_draft,
        party_filter.as_deref(),
    )?;
    if !include_all {
        rows.retain(|e| e.is_open());
    }
    if let Some(kw) = kw.as_deref() {
        rows.retain(|e| {
            e.settle_no.to_lowercase().contains(kw)
                || e.summary.to_lowercase().contains(kw)
                || e.aux_key.to_lowercase().contains(kw)
                || format!("{}-{}", e.word, e.no).to_lowercase().contains(kw)
                || e.no.to_string().contains(kw)
        });
    }
    // 合计与笔数在**分页之前**算：核销页要显示的「共 N 笔、应收合计 X」不能随着
    // 你往后翻页而变小 —— 那会让人以为钱变少了。
    let total = rows.len();
    let draft_cnt = rows.iter().filter(|e| !e.posted).count();
    let sum_debit: fincore::Money = rows.iter().fold(fincore::Money::ZERO, |a, r| {
        a + r.open().max(fincore::Money::ZERO)
    });
    let sum_credit: fincore::Money = rows.iter().fold(fincore::Money::ZERO, |a, r| {
        a + (-r.open()).max(fincore::Money::ZERO)
    });
    // 分页：默认给 200 行。**为什么默认就截断而不保持「全量」**：v1.8.36 实测
    // 单科目 1980 张凭证时这个接口返回 222.9KB，6000 张会到近 700KB，
    // 而核销时人一次只看得到十几行。要全量就显式传 limit=0。
    let limit = q
        .get("limit")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_SETTLE_OPEN_LIMIT)
        .min(MAX_SETTLE_OPEN_LIMIT);
    let offset = q
        .get("offset")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let page: Vec<&findb::settle::OpenEntry> = if limit == 0 {
        rows.iter().collect()
    } else {
        rows.iter().skip(offset).take(limit).collect()
    };
    Ok(Json(json!({
        "account": account.trim(),
        "upto": period_to_str(upto),
        "include_draft": include_draft,
        "draft_count": draft_cnt,
        "total": total,
        "returned": page.len(),
        "offset": offset,
        "limit": limit,
        "sum_debit": sum_debit.fmt_plain(),
        "sum_credit": sum_credit.fmt_plain(),
        "truncated": limit != 0 && offset + page.len() < total,
        "rows": page
            .iter()
            .map(|e| open_entry_json_with_days(e, upto.last_day()))
            .collect::<Vec<_>>(),
    })))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(Deserialize, Default)]
pub(super) struct SettleAutoReq {
    pub account: String,
    #[serde(default)]
    pub ymm: i32,
    #[serde(default)]
    pub tolerance: String,
}

#[derive(Deserialize)]
pub(super) struct SettleRunReq {
    pub from_entry: i64,
    pub to_entry: i64,
    pub amount: String,
    /// 差异（现金折扣 / 抹零）金额：> 0 时自动出一张差异凭证把零头记入损益
    #[serde(default)]
    pub diff: String,
    /// 差异科目（如 660305 财务费用-其他 / 6711 营业外支出）；差异为 0 时可空
    #[serde(default)]
    pub diff_account: String,
}

#[derive(Deserialize)]
pub(super) struct UnsettleReq {
    pub id: i64,
    /// 只撤销这么多（缺省 = 整条撤销）。
    ///
    /// 核销三张单共 5 万，事后发现其中一张该是 3 万 —— 以前只能整条撤销再重录，
    /// 撤销的瞬间三张单全变「未核销」，账龄立刻虚高。
    #[serde(default)]
    pub amount: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SettleCrossReq {
    /// 借方挂账分录（被冲减的应收 / 预付等）
    pub debit_entry: i64,
    /// 贷方挂账分录（被冲减的应付 / 预收等）
    pub credit_entry: i64,
    pub amount: String,
    /// 过渡凭证日期（`YYYY-MM-DD`，缺省今天）
    #[serde(default)]
    pub date: String,
}

#[derive(Deserialize)]
pub(super) struct OffsetCreateReq {
    #[serde(default)]
    date: String,
    pub customer: String,
    pub supplier: String,
    #[serde(default)]
    pub ar_account: String,
    #[serde(default)]
    pub ap_account: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}

#[derive(Deserialize)]
pub(super) struct StatementReq {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    account: String,
    party_code: String,
    #[serde(default)]
    party_name: String,
    /// 起始期间 ymm
    from: i32,
    /// 截止期间 ymm
    to: i32,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct StatementStatusReq {
    status: String,
    /// 回签确认人（confirmed 时必填，用于留痕"谁替客户签的"）
    #[serde(default)]
    confirmed_by: String,
}

#[derive(Deserialize)]
pub(super) struct DunningReq {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    account: String,
    party_code: String,
    #[serde(default)]
    party_name: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct DunningStatusReq {
    status: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum BeginSaveReq {
    Wrapped {
        #[serde(default)]
        rows: Vec<BeginRowInput>,
        #[serde(default)]
        delete_ids: Vec<i64>,
    },
    Rows(Vec<BeginRowInput>),
}

#[derive(Deserialize)]
pub(super) struct BeginBatchParseReq {
    text: String,
}

#[derive(Deserialize)]
pub(super) struct BeginBatchImportReq {
    rows: Vec<BeginBatchRow>,
    /// 前端草稿里已有的行也要避开 —— 与桌面的 `existing` 口径一致
    #[serde(default)]
    existing: Vec<String>,
}
