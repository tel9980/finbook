//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

/// 删除预算行
pub(super) async fn delete_budget_row(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    findb::mgmt::budget_delete(&db, id)?;
    db.log(user.username(), "预算", "删除预算行", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 打印预览页上的调整直接落库。
///
/// 收的是 [`fincore::print::PrintPatch`]（全是 `Option`），只合并请求里带来的字段。
/// 为什么不做成「整份 `PrintOptions` 覆盖」：预览页不认识的字段（凭证/账簿/单据
/// 纸型）会被 `serde(default)` 悄悄重置成默认值，用户下次打凭证才发现纸型没了。
///
/// 权限跟「系统设置」一致：版式是**账套级**的，改一次所有人都变。没有这个权限的
/// 账号，预览页只给「本次打印」的临时调整（页面会写明）。
pub(super) async fn post_print_settings(
    user: CurrentUser,
    Json(patch): Json<fincore::print::PrintPatch>,
) -> Result<Json<fincore::PrintOptions>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let mut opts = db.options();
    opts.print.apply_patch(&patch);
    // 校验只走 `Db::set_options` 里那一道。
    //
    // 这里曾经又写了一遍 `opts.validate()`——两层校验看着更保险，实际是「两层都拦」
    // 谁也证明不了另一层有没有在干活：把外层删掉，测试照样绿，`set_options` 里那道
    // 就已经把越界值挡回去了。真正的守卫只有一处，也就只有一条变异能盯住它。
    db.set_options(&opts)?;
    Ok(Json(opts.print))
}

pub(super) async fn get_periods(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let start = db.options().start_period;
    let this_year = Period::default().year();
    let end = Period::new(this_year + 1, 12).unwrap_or_default();
    let list: Vec<String> = if end >= start {
        Period::range(start, end)
            .into_iter()
            .map(period_to_str)
            .collect()
    } else {
        vec![period_to_str(start)]
    };
    let cur = current_period(&state, &user);
    let closed = periods::closed_upto(&*db)?;
    Ok(Json(json!({
        "current": period_to_str(cur),
        "closed_upto": closed.map(period_to_str),
        "start": period_to_str(start),
        "list": list,
    })))
}

pub(super) async fn post_period(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (year, month) = (req.ymm / 100, req.ymm % 100);
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) {
        return Err(AppError::bad_request(format!(
            "非法期间 {ymm}：应为 YYYYMM（年份 1970-9999，月份 1-12）",
            ymm = req.ymm
        )));
    }
    state.sessions.set_period(&user.token, req.ymm);
    Ok(Json(
        json!({"ok": true, "period": period_to_str(period_checked(req.ymm)?)}),
    ))
}

/// 期末预检：返回结账前需要处理的问题清单与损益概况
/// 月结向导：返回带状态的步骤清单
///
/// 与 `/precheck` 的区别只是形状：precheck 是给人看的错误串，这里是给界面画的清单。
/// 判定口径必须一致 —— `findb::month_close` 有测试钉住这件事。
pub(super) async fn period_wizard(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let mc = findb::month_close::report(&user.db, period)?;
    let steps: Vec<serde_json::Value> = mc
        .steps
        .iter()
        .map(|s| {
            json!({
                "code": s.code,
                "label": s.label,
                "ok": s.ok,
                "blocking": s.blocking,
                "detail": s.detail,
                "hint": s.hint,
            })
        })
        .collect();
    Ok(Json(json!({
        "period": period_to_str(period),
        "closed": mc.closed,
        "can_close": mc.can_close(),
        "steps": steps,
        "finance_ready": mc.can_close(),
    })))
}

pub(super) async fn period_precheck(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    let issues = periods::precheck(&db, period, true)?;
    let chart = accounts::chart(&db)?;
    // 预检展示与【结转】按钮同口径：结转按含草稿取数（见 period_carry_forward）
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(period).with_posted_only(false))?;
    let pl_rows = snap.carry_forward_rows(&chart);
    let profit = snap
        .for_account(fincore::engine::period_end::PROFIT_ACCOUNT, None)
        .end();
    Ok(Json(json!({
        "period": period_to_str(period),
        "issues": issues,
        "pl_count": pl_rows.len(),
        "profit_balance": profit.fmt_money(),
        "closed_upto": periods::closed_upto(&*db)?.map(period_to_str),
    })))
}

/// 年末结转：把「本年利润」余额转入「利润分配—未分配利润」
pub(super) async fn period_year_end(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CarryForward)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    let chart = accounts::chart(&db)?;
    // 同 period_carry_forward：含草稿取数，保证第一次生成的结转草稿能清零 4103，
    // 第二次调用（余额 0）才会被拒，否则同一笔利润会被结转两次。
    // 取数与落库放同一事务，避免并发请求都读到结转前余额。
    let tx = db.write_tx()?;
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(period).with_posted_only(false))?;
    let profit = snap
        .for_account(fincore::engine::period_end::PROFIT_ACCOUNT, None)
        .end();
    let date = period.last_day();
    let word = db
        .voucher_words()
        .first()
        .cloned()
        .unwrap_or_else(|| "记".to_string());
    let no = vouchers::next_no_of(&tx, period, &word)?;
    let mut v = fincore::engine::period_end::generate_year_end_carry(
        period,
        date,
        &word,
        no,
        profit,
        fincore::engine::period_end::UNDISTRIBUTED_ACCOUNT,
        &chart,
        user.username(),
    )?;
    let id = vouchers::save_in(&tx, &mut v)?;
    tx.commit().map_err(findb::DbError::from)?;
    db.log(
        user.username(),
        "期末",
        "年末结转",
        &format!("{} 凭证 #{}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id, "voucher_no": v.voucher_no() })))
}

/// 期末结账：有未处理问题时返回 400 + 问题清单
pub(super) async fn period_close(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
    Json(req): Json<ClosePeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    let issues = periods::close(&db, period, user.username(), req.require_carry)?;
    if !issues.is_empty() {
        return Err(AppError::bad_request(issues.join("；")));
    }
    Ok(Json(json!({ "ok": true, "closed": period_to_str(period) })))
}

/// 反结账
pub(super) async fn period_unclose(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    periods::unclose(&db, period, user.username())?;
    Ok(Json(
        json!({ "ok": true, "unclosed": period_to_str(period) }),
    ))
}

pub(super) async fn get_trial_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let rows: Vec<TrialRow> = rows.iter().map(TrialRow::from_row).collect();
    Ok(Json(json!({
        "from": q.get("from").cloned().unwrap_or_default(),
        "to": q.get("to").cloned().unwrap_or_default(),
        "rows": rows,
        "totals": {
            "begin_debit": totals.begin_debit.fmt_money(), "begin_credit": totals.begin_credit.fmt_money(),
            "debit": totals.period_debit.fmt_money(), "credit": totals.period_credit.fmt_money(),
            "end_debit": totals.end_debit.fmt_money(), "end_credit": totals.end_credit.fmt_money(),
        },
    })))
}

/// 科目明细账（链7 数字钻取）：期初 + 分录逐笔 + 合计；行点击可开凭证
pub(super) async fn account_detail_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let account = q
        .get("account")
        .map(String::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少 account 科目编码"));
    }
    if !user.user.can_see_account(&account) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let (begin, rows) = findb::balances::account_detail(&db, &account, from, to)?;
    let (td, tc): (Money, Money) = rows.iter().fold((Money::ZERO, Money::ZERO), |(ad, ac), r| {
        (ad + r.debit, ac + r.credit)
    });
    Ok(Json(json!({
        "account": account,
        "from": period_to_str(from),
        "to": period_to_str(to),
        "begin": begin,
        "rows": rows,
        "total_debit": td,
        "total_credit": tc,
    })))
}

pub(super) async fn print_trial_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 打印预览：所有有「账簿报表」权限的角色都可使用（不落地文件）
    user.require(Perm::Report)?;
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let po = print_opts_from_q(user.db.options().print, &q);
    let html = trial_balance_html(&user.company, &q, &rows, &totals, &po, can_edit);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

// ---------------------------------------------------------------------------
// 高级功能：多栏账 / 摘要汇总表 / 财务指标 / 工艺路线 / 报工 / MRP / 预算版本 /
// 审批流 / 报表附注 / 电子档案
// ---------------------------------------------------------------------------

pub(super) async fn get_multi_column(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let main = q.get("main").cloned().unwrap_or_default();
    if main.is_empty() {
        return Err(AppError::bad_request("缺少主科目 main"));
    }
    let cols: Vec<String> = q
        .get("cols")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if cols.is_empty() {
        return Err(AppError::bad_request("缺少栏目科目 cols（逗号分隔）"));
    }
    let (from, to) = period_range_param(
        &q,
        current_period(&state, &user),
        current_period(&state, &user),
    )?;
    let db = &user.db;
    // 数据范围：主科目与各栏目科目均须在可见范围内
    if !user.user.can_see_account(&main) || cols.iter().any(|c| !user.user.can_see_account(c)) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let rows = advanced::multi_column_table(&db, &main, &cols, from, to, Some(&user.user))?;
    Ok(Json(
        serde_json::json!({ "main": main, "cols": cols, "rows": rows }),
    ))
}

pub(super) async fn get_summary_table(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let (from, to) = period_range_param(
        &q,
        current_period(&state, &user),
        current_period(&state, &user),
    )?;
    let db = &user.db;
    let rows = advanced::summary_table(&db, from, to, Some(&user.user))?;
    Ok(Json(
        serde_json::json!({ "from": period_to_str(from), "to": period_to_str(to), "rows": rows }),
    ))
}

pub(super) async fn get_fin_ratios(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| {
            // 默认年初（同一会计年度 1 月）
            fincore::Period::new(period.year(), 1).unwrap_or(period)
        });
    let db = &user.db;
    let rows = advanced::fin_ratios(&db, period, from, Some(&user.user))?;
    Ok(Json(
        serde_json::json!({ "period": period_to_str(period), "ratios": rows }),
    ))
}

// ---- 所有者权益变动表 / 报表对比 / 科目日报表 / 期末对账 ----

pub(super) async fn get_equity_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    // 原本只有 from 是区间参数、to 固定等于 period，这里保持不变（不凭空加 to 参数）
    let year_start = fincore::Period::new(period.year(), 1).unwrap_or(period);
    let from = period_param(q.get("from"), year_start)?;
    if from > period {
        return Err(AppError::bad_request(format!(
            "起始期间不能晚于结束期间：{} > {}",
            period_to_str(from),
            period_to_str(period)
        )));
    }
    let db = &user.db;
    let stmt = findb::reports::equity_statement(&db, from, period, Some(&user.user))?;
    Ok(Json(serde_json::json!({
        "from": period_to_str(from),
        "to": period_to_str(period),
        // 前端要显示勾稽结果；ties() 是方法，不会随 stmt 一起序列化。
        "ties": stmt.ties(),
        "statement": stmt,
    })))
}

pub(super) async fn get_report_compare(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let key = q
        .get("key")
        .cloned()
        .unwrap_or_else(|| "balance_sheet".to_string());
    let cur = period_param(q.get("period"), current_period(&state, &user))?;
    let prev = period_param(q.get("prev"), cur.prev())?;
    let yearly = q
        .get("yearly")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    // 默认按年累计：当前期 1 月→当前期；上期 1 月→上期
    let (cur_from, prev_from) = if yearly {
        (
            fincore::Period::new(cur.year(), 1).unwrap_or(cur),
            fincore::Period::new(prev.year(), 1).unwrap_or(prev),
        )
    } else {
        (cur, prev)
    };
    let db = &user.db;
    let rows = findb::reports::report_compare(
        &db,
        &key,
        cur_from,
        cur,
        prev_from,
        prev,
        Some(&user.user),
    )?;
    Ok(Json(serde_json::json!({
        "key": key,
        "current": period_to_str(cur),
        "previous": period_to_str(prev),
        "rows": rows,
    })))
}

pub(super) async fn get_account_daily(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少科目编码 code"));
    }
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let to = q.get("to").and_then(|s| parse_period(s)).unwrap_or(from);
    let db = &user.db;
    // 数据范围：仅可见科目
    if !user.user.can_see_account(&code) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    // H-3：科目日报默认只统计已记账，与账簿「只含已记账」开关同参
    let posted_only = q
        .get("posted_only")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let rows =
        findb::reports::account_daily_report(&db, &code, from, to, Some(&user.user), posted_only)?;
    Ok(Json(serde_json::json!({ "code": code, "rows": rows })))
}

pub(super) async fn get_period_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let db = &user.db;
    let items = findb::reports::period_reconcile(&db, period)?;
    Ok(Json(
        serde_json::json!({ "period": period_to_str(period), "items": items }),
    ))
}

/// 辅助账：按辅助核算维度汇总各单位的期初/发生/期末
pub(super) async fn get_aux_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let kind_code = q.get("kind").map(String::as_str).unwrap_or("customer");
    let kind = fincore::AuxKind::from_code(kind_code)
        .ok_or_else(|| AppError::bad_request("非法辅助核算维度 kind"))?;
    if kind == fincore::AuxKind::CashFlow {
        return Err(AppError::bad_request("现金流量项目不是余额维度"));
    }
    let (from, to) = report_range(&state, &user, &q)?;
    let db = &user.db;
    let rows = balances::aux_balance(&db, kind, from, to, Some(&user.user))?;
    Ok(Json(json!({
        "kind": kind.code(),
        "kind_label": kind.label(),
        "from": period_to_str(from),
        "to": period_to_str(to),
        "rows": rows,
    })))
}

/// 数量金额账：数量核算科目的数量与金额对照
pub(super) async fn get_qty_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let (from, to) = report_range(&state, &user, &q)?;
    let db = &user.db;
    let rows = balances::qty_balance_sheet(&db, from, to, Some(&user.user))?;
    Ok(Json(json!({
        "from": period_to_str(from),
        "to": period_to_str(to),
        "rows": rows,
    })))
}

pub(super) async fn get_custom_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let r = findb::mgmt::custom_get(&db, &key)?
        .ok_or_else(|| AppError::not_found("自定义报表不存在"))?;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    // 用 detailed 版本：公式写错/取数失败（数据库忙、脏数据）时该单元格按 0 占位，
    // 但**必须把原因回传**。旧的 `custom_report_values` 直接丢弃 warnings，
    // 报表会打印出一片像样的 0 而用户完全看不出哪里错了——财务报表最坏的失败模式。
    let (values, warnings) =
        findb::mgmt::custom_report_values_detailed(&db, &r, period, Some(&user.user))?;
    let matrix: Vec<Vec<String>> = values
        .iter()
        .map(|row| row.iter().map(|m| m.fmt_money()).collect())
        .collect();
    Ok(Json(json!({
        "report": custom_json(&r),
        "period": period_to_str(period),
        "values": matrix,
        "warnings": warnings,
    })))
}

// ---- 预算预警 ----

pub(super) async fn get_budget_alerts(
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
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| fincore::Period::new(period.year(), 1).unwrap_or(period));
    let threshold = q
        .get("threshold")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(90);
    let rows = findb::mgmt::budget_alerts(&db, period, from, threshold)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn copy_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BCopyReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let n = advanced::bversion_copy(&db, &req.from, &req.to)?;
    Ok(Json(serde_json::json!({ "ok": true, "copied": n })))
}

// ---- 报表附注 ----

pub(super) async fn list_notes(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let report_key = q
        .get("report_key")
        .cloned()
        .unwrap_or_else(|| "balance_sheet".to_string());
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let db = &user.db;
    let rows = advanced::note_list(&db, &report_key, period)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

pub(super) async fn save_note(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<NoteReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let mut n = advanced::ReportNote {
        id: req.id,
        report_key: req.report_key,
        period: period_checked(req.period)?,
        seq: req.seq,
        title: req.title,
        content: req.content,
        updated_by: user.username().to_string(),
        updated_at: String::new(),
    };
    let id = advanced::note_save(&db, &mut n)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

pub(super) async fn delete_note(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    advanced::note_delete(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// 预算分析
// ---------------------------------------------------------------------------

pub(super) async fn get_budget_analysis(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let year = q
        .get("year")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or_else(|| current_period(&state, &user).year());
    let version = q.get("version").cloned().unwrap_or_default();
    let upto = q.get("upto").and_then(|s| parse_period(s));
    let rows = findb::mgmt::budget_analysis(&db, year, &version, upto)?;
    let summary = findb::mgmt::budget_analysis_summary(&db, year, &version)?;
    Ok(Json(
        serde_json::json!({ "year": year, "rows": rows, "summary": summary }),
    ))
}

pub(super) async fn get_balance_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let t = statement_table(
        &db,
        &user,
        "balance_sheet",
        from,
        to,
        vec![
            Box::new(fincore::report::identity),
            Box::new(fincore::report::to_begin),
        ],
    )?;
    Ok(Json(
        json!({ "from": period_to_str(from), "to": period_to_str(to), "table": table_json(&t) }),
    ))
}

pub(super) async fn print_balance_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let t = statement_table(
        &db,
        &user,
        "balance_sheet",
        from,
        to,
        vec![
            Box::new(fincore::report::identity),
            Box::new(fincore::report::to_begin),
        ],
    )?;
    let html = crate::report_html::report_table_html(&t, &user.company, "元", &print_opts_from_q(db.options().print, &q), can_edit);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

pub(super) async fn get_income_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let t = statement_table(
        &db,
        &user,
        "income_statement",
        from,
        to,
        vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
    )?;
    Ok(Json(
        json!({ "from": period_to_str(from), "to": period_to_str(to), "table": table_json(&t) }),
    ))
}

pub(super) async fn get_cash_flow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    // 本期 + 上期一起算：只给本期净额没法判断「是好是坏」
    let (cf, prev_cf) = findb::reports::cash_flow_compare(&db, from, to, Some(&user.user))?;
    let (pf, pt) = findb::reports::prior_range(from, to);
    let line = |l: &fincore::report::cashflow::CashFlowLine| json!({ "code": l.code, "name": l.name, "inflow": l.inflow.fmt_money(), "outflow": l.outflow.fmt_money(), "net": l.net.fmt_money() });
    Ok(Json(json!({
        "from": period_to_str(from), "to": period_to_str(to),
        "operating": cf.operating.iter().map(line).collect::<Vec<_>>(),
        "operating_net": cf.operating_net.fmt_money(),
        "investing": cf.investing.iter().map(line).collect::<Vec<_>>(),
        "investing_net": cf.investing_net.fmt_money(),
        "financing": cf.financing.iter().map(line).collect::<Vec<_>>(),
        "financing_net": cf.financing_net.fmt_money(),
        "net_increase": cf.net_increase.fmt_money(),
        "begin_cash": cf.begin_cash.fmt_money(),
        "end_cash": cf.end_cash.fmt_money(),
        "unassigned": cf.unassigned.fmt_money(),
        "auto_assigned": cf.auto_assigned,
        "ambiguous": cf.ambiguous,
        "fully_assigned": cf.fully_assigned(),
        "assignment_note": cf.assignment_note(),
        "ties": cf.ties(),
        "prev": {
            "from": period_to_str(pf), "to": period_to_str(pt),
            "operating_net": prev_cf.operating_net.fmt_money(),
            "investing_net": prev_cf.investing_net.fmt_money(),
            "financing_net": prev_cf.financing_net.fmt_money(),
            "net_increase": prev_cf.net_increase.fmt_money(),
        },
        "delta": {
            "operating_net": (cf.operating_net - prev_cf.operating_net).fmt_money(),
            "net_increase": (cf.net_increase - prev_cf.net_increase).fmt_money(),
        },
    })))
}

pub(super) async fn print_cash_flow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    // 打印页也带上期对比：Excel 有、网页有，打印页没有的话，拿纸去开会的人看到的
    // 是唯一一份「只有本期」的表。
    let (cf, prev) = findb::reports::cash_flow_compare(&db, from, to, Some(&user.user))?;
    let subtitle = format!("{} 至 {}", from.label(), to.label());
    let html = crate::report_html::cash_flow_html(
        &cf,
        &user.company,
        &subtitle,
        &print_opts_from_q(db.options().print, &q),
        can_edit,
        Some(&prev),
    );
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

pub(super) async fn print_equity_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let stmt = findb::reports::equity_statement(&db, from, to, Some(&user.user))?;
    let subtitle = format!("{} 至 {}", from.label(), to.label());
    let html = crate::report_html::equity_html(&stmt, &user.company, &subtitle, &print_opts_from_q(db.options().print, &q), can_edit);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 科目余额表导出
pub(super) async fn export_trial_balance_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let from = q.get("from").cloned().unwrap_or_default();
    let to = q.get("to").cloned().unwrap_or_default();
    let sheet = crate::sheets::trial_balance_sheet(&rows, &totals, &from, &to);
    let n = rows.len();
    user.db.log(
        user.username(),
        "导出",
        "导出科目余额表",
        &format!("{from} 至 {to} {n} 行"),
    )?;
    build_xlsx(&[sheet], &format!("科目余额表_{to}.xlsx"), "科目余额表")
}

pub(super) async fn export_balance_sheet_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    export_statement_xlsx(&state, &user, &q, StatementKind::BalanceSheet)
}

pub(super) async fn export_cash_flow_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    export_statement_xlsx(&state, &user, &q, StatementKind::CashFlow)
}

pub(super) async fn export_equity_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    export_statement_xlsx(&state, &user, &q, StatementKind::Equity)
}


/// 预算行列表（当前激活版本）
pub(super) async fn list_budget_rows(
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
    let ver = findb::advanced::bversion_current(&*db)?;
    let rows: Vec<serde_json::Value> = findb::mgmt::budget_list(&db, period)?
        .into_iter()
        .filter(|b| b.version == ver)
        .map(|b| {
            json!({
                "id": b.id, "period": period_to_str(b.period),
                "account_code": b.account_code, "dept": b.dept,
                "amount": b.amount.fmt_money(), "memo": b.memo, "version": b.version,
            })
        })
        .collect();
    Ok(Json(json!({
        "period": period_to_str(period),
        "version": ver,
        "rows": rows,
    })))
}


/// 新增/修改预算行（按 期间+科目+部门+版本 upsert；版本 = 当前激活版本）
pub(super) async fn save_budget_row(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BudgetRowReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = period_checked(req.period)?;
    if req.account_code.trim().is_empty() {
        return Err(AppError::bad_request("科目编码不能为空"));
    }
    let amount = parse_money_checked(&req.amount)?;
    if amount.is_negative() {
        return Err(AppError::bad_request("预算金额不能为负"));
    }
    let ver = findb::advanced::bversion_current(&*db)?;
    let b = findb::mgmt::Budget {
        id: 0,
        period,
        account_code: req.account_code.trim().to_string(),
        dept: req.dept.trim().to_string(),
        amount,
        memo: req.memo,
        version: ver,
    };
    let id = findb::mgmt::budget_upsert_version(&db, &b)?;
    db.log(
        user.username(),
        "预算",
        "预算编制",
        &format!(
            "{} {} {} {}",
            period_to_str(period),
            b.account_code,
            if b.dept.is_empty() {
                ""
            } else {
                b.dept.as_str()
            },
            b.amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}


/// 结转损益：生成一张把损益类科目净额转入「本年利润」的凭证
pub(super) async fn period_carry_forward(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CarryForward)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    let chart = accounts::chart(&db)?;
    // 结转是"操作"不是报表：按含未记账（排除作废）取数。结转生成的凭证本身是草稿，
    // 若按已记账口径取数，草稿看不见 → 第二次调用时损益仍未清零 → 重复结转。
    // 结账前 checklist 要求全部记账，届时两种口径结果相同（H-3 定案的已记账口径
    // 用于余额表/报表/账簿，不用于本操作的幂等判定）。
    // 取数、幂等判断、取号、落库必须同事务：原实现先读余额再单独 save，
    // 两个并发请求都能读到“有损益”，各自生成一张结转凭证；等第二张提交时，
    // 损益已经被第一张清零，账上就多了一笔重复结转。BEGIN IMMEDIATE 让后来者
    // 等前者提交后再取数，第二次看到的净额已是 0。
    let tx = db.write_tx()?;
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(period).with_posted_only(false))?;
    let rows = snap.carry_forward_rows(&chart);
    if rows.is_empty() {
        return Err(AppError::bad_request("本期损益类（含成本类）科目没有发生额，无需结转"));
    }
    // 已结转过就先说清楚，不要让用户对着「没有可结转的分录」去猜。
    //
    // 账结法天生幂等：结转凭证把各损益科目的本期净发生额清零，再点一次自然没有
    // 分录可结。功能上没错，错的是提示——原来只说「净发生额均为 0」，用户无法
    // 区分「本期压根没做业务」和「已经结过了」，只能去翻凭证。实测重复点击时账面
    // 明明有 658,000 的损益额，提示却说「损益发生额均为零」。
    //
    // 这里主动查本期已有的结转凭证，把因果和补救一起给出。
    let carried: Vec<String> = vouchers::list(&db, &vouchers::VoucherQuery::period(period))?
        .into_iter()
        .filter(|v| v.source == VoucherSource::CarryForward && v.status != VoucherStatus::Void)
        .map(|v| format!("{} #{}", v.word, v.no))
        .collect();
    if !carried.is_empty() && rows.iter().all(|r| r.debit.round2().eq(&r.credit.round2())) {
        return Err(AppError::bad_request(format!(
            "本期损益已结转过（{}），各损益科目净发生额已清零，无需重复结转。\
             如需重新结转，请先删除该凭证再执行",
            carried.join("、")
        )));
    }
    let date = period.last_day();
    let word = db
        .voucher_words()
        .first()
        .cloned()
        .unwrap_or_else(|| "记".to_string());
    let no = vouchers::next_no_of(&tx, period, &word)?;
    let mut v = fincore::engine::period_end::generate_carry_forward(
        period,
        date,
        &word,
        no,
        &rows,
        &chart,
        fincore::engine::period_end::PROFIT_ACCOUNT,
        user.username(),
    )?;
    let id = vouchers::save_in(&tx, &mut v)?;
    tx.commit().map_err(findb::DbError::from)?;
    db.log(
        user.username(),
        "期末",
        "结转损益",
        &format!("{} 凭证 #{}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id, "voucher_no": v.voucher_no() })))
}


pub(super) async fn export_trial_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 导出数据：仅管理员与财务主管（防止把整表数据带出）
    user.require(Perm::Export)?;
    let (rows, _totals) = trial_balance_data(&state, &user, &q)?;
    let csv = trial_balance_csv(&rows);
    let body = csv.into_bytes();
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"trial_balance.csv\"",
            ),
        ],
        body,
    )
        .into_response())
}


/// 科目余额表导出 PDF（仅管理员与财务主管）
pub(super) async fn export_trial_balance_pdf(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let from = q.get("from").cloned().unwrap_or_default();
    let to = q.get("to").cloned().unwrap_or_default();
    let company = user.company.clone();
    let bytes = crate::pdf::trial_balance_pdf(&company, &from, &to, &rows, &totals)
        .map_err(AppError::bad_request)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"trial_balance.pdf\"",
            ),
        ],
        bytes,
    )
        .into_response())
}


pub(super) async fn list_custom_reports(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let rows: Vec<serde_json::Value> = findb::mgmt::custom_list(&*db)?
        .iter()
        .map(custom_json)
        .collect();
    Ok(Json(rows))
}


pub(super) async fn save_custom_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CustomReportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    if req.name.trim().is_empty() {
        return Err(AppError::bad_request("报表名称不能为空"));
    }
    let key = if req.key.trim().is_empty() {
        findb::mgmt::custom_next_key(&*db)?
    } else {
        req.key.trim().to_string()
    };
    let r = findb::mgmt::CustomReport {
        key: key.clone(),
        name: req.name.trim().to_string(),
        columns: req.columns.iter().map(|c| c.trim().to_string()).collect(),
        lines: req
            .lines
            .iter()
            .map(|l| findb::mgmt::CustomLine {
                name: l.name.trim().to_string(),
                indent: l.indent,
                formulas: l.formulas.clone(),
                bold: l.bold,
            })
            .collect(),
    };
    let errors: Vec<serde_json::Value> = findb::mgmt::custom_check(&r)
        .into_iter()
        .map(|(li, ci, e)| json!({ "line": li, "column": ci, "error": e }))
        .collect();
    findb::mgmt::custom_save(&db, &r)?;
    db.log(
        user.username(),
        "报表",
        "保存自定义报表",
        &format!("{} {}（{} 行）", key, r.name, r.lines.len()),
    )?;
    Ok(Json(json!({ "key": key, "errors": errors })))
}


pub(super) async fn delete_custom_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    findb::mgmt::custom_delete(&db, &key)?;
    db.log(user.username(), "报表", "删除自定义报表", &key)?;
    Ok(Json(json!({ "ok": true })))
}


// ---- 预算版本 ----

pub(super) async fn list_budget_versions(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let versions = advanced::bversion_list(&*db)?;
    let current = advanced::bversion_current(&*db)?;
    Ok(Json(
        serde_json::json!({ "versions": versions, "current": current }),
    ))
}


// 预算版本 / 审批流 / 报表附注 / 会计档案的写操作统一要求 Perm::AccountEdit，
// 与其余业务写路由保持一致。不要用 Perm::Report 把关：Report 是只读权限，
// 且每个角色（含只读 Viewer）都自带它，等于对只读账号开放了写入。
pub(super) async fn save_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BVersionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    advanced::bversion_save(
        &db,
        &advanced::BudgetVersion {
            key: req.key,
            name: req.name,
            is_current: req.is_current,
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            memo: req.memo,
        },
    )?;
    Ok(Json(serde_json::json!({ "ok": true })))
}


pub(super) async fn delete_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    advanced::bversion_delete(&db, &key)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}


pub(super) async fn activate_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let mut v = advanced::bversion_list(&*db)?
        .into_iter()
        .find(|v| v.key == key)
        .ok_or_else(|| AppError::NotFound(format!("预算版本 {key} 不存在")))?;
    v.is_current = true;
    advanced::bversion_save(&db, &v)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}


pub(super) async fn print_income_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let t = statement_table(
        &db,
        &user,
        "income_statement",
        from,
        to,
        vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
    )?;
    let html = crate::report_html::report_table_html(&t, &user.company, "元", &print_opts_from_q(db.options().print, &q), can_edit);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}


pub(super) async fn update_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(t): Json<template::Template>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let mut t = t;
    t.id = id;
    template::update(&db, &t)?;
    db.log(user.username(), "凭证模板", "修改模板", &t.name)?;
    Ok(Json(json!({"ok": true})))
}


pub(super) async fn delete_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    template::delete(&db, id)?;
    db.log(user.username(), "凭证模板", "删除模板", &id.to_string())?;
    Ok(Json(json!({"ok": true})))
}


pub(super) async fn export_income_statement_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    export_statement_xlsx(&state, &user, &q, StatementKind::Income)
}
