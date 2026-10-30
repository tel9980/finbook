//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

pub(super) async fn next_voucher_no(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let word = q.get("word").cloned().unwrap_or_else(|| "记".to_string());
    let no = vouchers::next_no(&db, period, &word)?;
    Ok(Json(json!({"no": no})))
}

pub(super) async fn list_vouchers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<VoucherListItem>>, AppError> {
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = &user.db;
    let mut query = voucher_query_from(&q, &user);
    // 上限 1000：limit 传负数时 SQLite 视为不限制，会把全库凭证+分录读进内存
    query.limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .map(|v| v.clamp(1, 1000))
        .or(Some(200));
    let mut list = vouchers::list(&db, &query)?;
    // 列表需要摘要与借贷合计：`vouchers::list` 只读表头，这里批量补充分录
    vouchers::fill_entries(&db, &mut list)?;
    // 数据范围：科目区间等限制需逐张过滤（查询层只处理了「仅本人凭证」）
    list.retain(|v| user.user.can_see_voucher(v));
    let list = list.iter().map(to_item).collect();
    Ok(Json(list))
}

/// 凭证列表（分页 + 全量合计）。
///
/// 为什么必须有这个端点：老的 `GET /api/vouchers` 默认 `limit=200`，而排序
/// 是 `asc=true`（日期+凭证号升序）。于是一个期间超过 200 张凭证后，
/// **后面的凭证通过界面完全看不到**，而且界面上没有任何提示说「还有更多」。
/// 对一个月两三百张的单据，这事每天都在发生。
///
/// 参数非法一律 400（`page=abc` 不能静默当第 1 页——那会让调用方拿到
/// 「看起来正常但其实是首页」的数据，漏拉很难发现）。
pub(super) async fn list_vouchers_page(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<VoucherPage>, AppError> {
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = &user.db;
    let num = |k: &str, def: i64| -> Result<i64, AppError> {
        match q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            None => Ok(def),
            Some(t) => t
                .parse::<i64>()
                .map_err(|_| AppError::bad_request(format!("参数 {k} 必须是整数，收到：{t}"))),
        }
    };
    let page = num("page", 1)?.max(1);
    // 单页上限 500：一次拉太多只会把浏览器和服务端一起拖垮。
    // 要全量导出走 export 端点，不该靠调大 page_size。
    let page_size = num("page_size", 50)?.clamp(1, 500);
    let offset = (page - 1) * page_size;

    let query = voucher_query_from(&q, &user);
    let (mut rows, total) = vouchers::list_page(&db, &query, offset, page_size)?;
    vouchers::fill_entries(&db, &mut rows)?;

    // 兜底：科目区间已由 SQL 下推（vouchers::build_where），这里再 retain 一次。
    // 若真的过滤掉了行，说明 SQL 条件与 can_see_voucher 不一致 —— 那时 total 就是
    // 错的，页码会算错。与其默默给用户一个打不开的末页，不如把 9 张说成 10 张这
    // 类错误暴露出来。所以这里显式断言一致。
    let before = rows.len();
    rows.retain(|v| user.user.can_see_voucher(v));
    debug_assert_eq!(
        before,
        rows.len(),
        "SQL 下推的数据范围与 can_see_voucher 不一致，total 会算错页码"
    );

    // 合计走全量（不分页）：财务界面要核对的是本期间总账数字
    let (debit_total, credit_total) = vouchers::totals(&db, &query)?;
    Ok(Json(VoucherPage {
        items: rows.iter().map(to_item).collect(),
        page,
        page_size,
        total,
        debit_total: debit_total.fmt_money(),
        credit_total: credit_total.fmt_money(),
    }))
}

pub(super) async fn get_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<VoucherDetail>, AppError> {
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    // 数据权限：非全量权限用户不得查看自己不可见的凭证
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权查看该凭证"));
    }
    let mut detail = VoucherDetail::from_voucher(v);
    // 更正链不是凭证本身的内容（`Voucher` 结构里没有这三个字段），
    // 所以查凭证接口顺带补上，前端一次请求就能画出链。
    detail.amend = vouchers::amend_link(&db, id)?;
    Ok(Json(detail))
}

pub(super) async fn save_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SaveVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 新增需 VoucherNew，修改需 VoucherEdit
    if req.id > 0 {
        user.require(Perm::VoucherEdit)?;
    } else {
        user.require(Perm::VoucherNew)?;
    }
    let db = &user.db;
    // 期间非法（如 202513）直接 400：静默落到当前期间会把凭证写进错误的账期。
    // 0 保持原语义 = 当前期间（与 renumber / reverse 一致）。
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    let word = if req.word.is_empty() {
        "记".to_string()
    } else {
        req.word.clone()
    };

    // 修改时保留原分录的未提交要素（辅助核算/数量/外币/结算号等）
    let mut prev_entries: Vec<Entry> = Vec::new();
    let mut v = if req.id > 0 {
        let mut existing = vouchers::get(&db, req.id)?
            .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
        // 数据权限校验：非全量权限用户不得修改自己不可见的凭证
        if !user.user.can_see_voucher(&existing) {
            return Err(AppError::forbidden("无权修改该凭证"));
        }
        if !existing.status.can_edit() {
            return Err(AppError::forbidden(
                "该凭证已记账或已作废，不能修改（已记账请先反记账）",
            ));
        }
        // 启用审核环节的账套：已审核凭证须先反审核（预检给 400，避免落到引擎错误的 500）
        if existing.status == VoucherStatus::Audited && db.options().enable_audit {
            return Err(AppError::bad_request("已审核凭证不能修改，请先反审核"));
        }
        // 日期不能漂移到凭证期间之外（期间本身不可改，改的是日期）
        if (date.year(), date.month()) != (existing.period.year(), existing.period.month()) {
            return Err(AppError::bad_request(format!(
                "凭证日期 {} 不在其所属期间 {} 内",
                date.format("%Y-%m-%d"),
                existing.period.label()
            )));
        }
        // 凭证字在修改时不可变：号段按 (期间, 凭证字, 号码) 唯一，中途换字会让本凭证
        // 占住另一字下的同一号（那可能已经是别人的凭证），或在该字下留一个永久断号——
        // 因为 existing.no 不会跟着重新取号。与上面"期间不可改"同一口径的处理：
        // 要换字请作废后重录。客户端不传 word（空串）时视为不表达意图，保留原字。
        let explicit_word = req.word.trim();
        if !explicit_word.is_empty() && explicit_word != existing.word {
            return Err(AppError::bad_request(format!(
                "凭证字不可由「{}」改为「{}」（号段按凭证字独立编号），如需换字请作废后重录",
                existing.word, explicit_word
            )));
        }
        existing.date = date;
        existing.attachments = req.attachments;
        existing.memo = req.memo.clone();
        prev_entries = existing.entries.clone();
        existing.entries.clear();
        existing
    } else {
        if (date.year(), date.month()) != (period.year(), period.month()) {
            return Err(AppError::bad_request(format!(
                "凭证日期 {} 不在所选期间 {} 内",
                date.format("%Y-%m-%d"),
                period.label()
            )));
        }
        let no = if req.no > 0 {
            // 调用方自选号码撞上 UNIQUE(period,word,no) 时 rusqlite 约束错误会一路冒成
            // AppError::Db → HTTP 500（看着像服务端坏了，其实是用户填重了）。
            // 预先查一次占用并回 400；并发下仍可能漏判，那时唯一索引继续兜底。
            if vouchers::no_taken(&db, period, &word, req.no, 0)? {
                return Err(AppError::bad_request(format!(
                    "凭证号 {}-{:04} 已被占用，请换一个号或留空自动取号",
                    word, req.no
                )));
            }
            req.no
        } else {
            vouchers::next_no(&db, period, &word)?
        };
        let mut v = Voucher::new(period, date, word, no);
        v.attachments = req.attachments;
        v.memo = req.memo.clone();
        v
    };
    v.prepared_by = user.username().to_string();
    // 加载科目表：银行尾号简写需要按科目表解析为完整编码
    let chart = accounts::chart(&db)?;
    for (i, e) in req.entries.iter().enumerate() {
        let account_code = normalize_bank_account(&chart, &e.account_code)
            .unwrap_or_else(|| e.account_code.clone());
        let mut en = Entry::new(e.line, account_code.clone(), e.summary.clone());
        en.debit = parse_money_checked(&e.debit)?;
        en.credit = parse_money_checked(&e.credit)?;
        // 未提交的要素沿用原行：Web 编辑桌面录入的凭证时，辅助/数量/外币/结算号不丢
        if let Some(prev) = prev_entries.get(i) {
            en.aux = prev.aux.clone();
            en.qty = prev.qty;
            en.price = prev.price;
            en.currency = prev.currency.clone();
            en.rate = prev.rate;
            en.amount_for = prev.amount_for;
            en.settle_type = prev.settle_type.clone();
            en.settle_no = prev.settle_no.clone();
            en.biz_date = prev.biz_date;
        }
        if let Some(aux) = &e.aux {
            en.aux = aux.clone();
        }
        if let Some(q) = e.qty {
            en.qty = Some(q);
        }
        if let Some(p) = e.price {
            en.price = Some(p);
        }
        if let Some(cur) = e
            .currency
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            en.currency = Some(cur.to_ascii_uppercase());
        }
        if let Some(r) = e.rate {
            en.rate = Some(r);
        }
        if let Some(a) = e.amount_for {
            en.amount_for = Some(a);
        }
        if let Some(cf) = e.cf.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            en.aux.cash_flow = Some(cf.to_string());
        }
        // 银行科目：尾号存入辅助核算银行字段（仅在未显式提供时兜底）
        if en.account_code.starts_with("1002") && en.aux.bank.is_none() {
            en.aux.bank = Some(account_code);
        }
        v.entries.push(en);
    }
    if !v.balanced() {
        return Err(AppError::bad_request("借贷不平衡，请检查分录金额"));
    }
    // 预算控制（账套参数 budget_control：off/warn/strong）：费用/成本类借方分录比对预算执行
    let budget_ctl = db.options().budget_control.trim().to_string();
    if budget_ctl == "warn" || budget_ctl == "strong" {
        let adds: Vec<(String, String, Money)> = v
            .entries
            .iter()
            .filter(|e| e.debit.is_positive())
            .map(|e| {
                (
                    e.account_code.clone(),
                    e.aux.dept.clone().unwrap_or_default(),
                    e.debit,
                )
            })
            .collect();
        let overs = findb::mgmt::budget_check(&db, v.period, &adds)?;
        if !overs.is_empty() {
            let msg = overs
                .iter()
                .map(|o| {
                    format!(
                        "{} {} 预算 {} 已执行 {} 本次 {} 超 {}",
                        o.account_code,
                        if o.dept.is_empty() {
                            ""
                        } else {
                            o.dept.as_str()
                        },
                        o.budget.fmt_money(),
                        o.actual.fmt_money(),
                        o.add.fmt_money(),
                        o.over.fmt_money()
                    )
                })
                .collect::<Vec<_>>()
                .join("；");
            if budget_ctl == "strong" {
                return Err(AppError::bad_request(format!("预算强控：{msg}")));
            }
            db.log(user.username(), "预算", "超预算提醒", &msg)?;
        }
    }
    // 保存为「未记账」，核对无误后在界面点「记账」确认入账（无审核环节）
    v.status = VoucherStatus::Draft;
    let id = vouchers::save(&db, &mut v)?;
    // 凭证已提交：日志失败不得把请求打成 500，否则客户端重试会多录一张凭证
    if let Err(e) = db.log(
        user.username(),
        "凭证",
        if req.id > 0 { "修改" } else { "新增" },
        &v.voucher_no(),
    ) {
        error!(voucher = id, error = %e, "写操作日志失败（凭证）");
    }
    Ok(Json(json!({"id": id})))
}

/// 凭证作废 / 恢复（已结账期间拒绝；作废后不参与账簿汇总，可恢复）
pub(super) async fn voucher_void(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<VoidReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权操作该凭证"));
    }
    vouchers::set_void(&db, id, req.void, user.username())?;
    db.log(
        user.username(),
        "凭证",
        if req.void { "作废" } else { "恢复作废" },
        &format!("凭证 #{id}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

/// 凭证更正：作废原凭证 + 生成一张带链接的新凭证（对标 ERPNext 的 Cancel -> Amend）
///
/// 权限沿用 `VoucherDelete`（作废凭证的权限）：更正是比作废更重的动作 ——
/// 作废只是把一张凭证移出账外，更正还会**生成**一张进入流程的新凭证。
/// 单独的权限点会让「谁能改历史账」变成配置问题，先复用既有口径。
pub(super) async fn voucher_amend(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AmendReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    // 权限校验必须在 amend 之前：数据权限不只管「看」，也管「改」
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权更正该凭证"));
    }
    let new_id = vouchers::amend(&db, id, &req.reason, user.username())?;
    // 操作日志由引擎在**同一事务内**写入（amend 里 log_on）：
    // 更正已提交、日志还没写就崩溃的话，账改了却查不到谁改的 ——
    // 那正是这套更正链要解决的审计洞，不能自己再捅一个。这里再写一条会重复。
    Ok(Json(json!({ "ok": true, "new_id": new_id })))
}

pub(super) async fn voucher_post(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherPost)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权记账该凭证"));
    }
    if v.status == VoucherStatus::Posted {
        return Ok(Json(json!({"ok": true, "already_posted": true})));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("该凭证不参与账簿汇总"));
    }
    // 启用审核环节的账套：先审核再记账（预检给 400，避免落到引擎错误的 500）
    if db.options().enable_audit && v.status != VoucherStatus::Audited {
        return Err(AppError::bad_request(
            "该账套启用了审核环节，请先审核凭证再记账",
        ));
    }
    vouchers::post(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 审核（未记账 → 已审核）
pub(super) async fn voucher_audit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherAudit)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权审核该凭证"));
    }
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能审核"));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("已作废凭证不能审核"));
    }
    vouchers::audit(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 反审核（已审核 → 未记账）
pub(super) async fn voucher_unaudit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherUnaudit)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权反审核该凭证"));
    }
    if v.status != VoucherStatus::Audited {
        return Err(AppError::bad_request("只有已审核凭证才能反审核"));
    }
    vouchers::unaudit(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 出纳签字（未记账凭证记录签字人；require_cashier 账套作为现金/银行凭证记账前置）
pub(super) async fn voucher_sign(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权对该凭证签字"));
    }
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能签字，请先反记账"));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("已作废凭证不能签字"));
    }
    vouchers::sign(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 取消出纳签字
pub(super) async fn voucher_unsign(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权对该凭证取消签字"));
    }
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能取消签字，请先反记账"));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("已作废凭证不能取消签字"));
    }
    vouchers::unsign(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn voucher_batch_post(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BatchPostReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherPost)?;
    let db = &user.db;
    // 数据范围过滤：仅记账本人可见的凭证，越权 id 直接拒掉并计数
    let mut allowed: Vec<i64> = Vec::new();
    let mut denied = 0usize;
    for id in &req.ids {
        match vouchers::get(&db, *id)? {
            Some(v) if user.user.can_see_voucher(&v) => allowed.push(*id),
            _ => denied += 1,
        }
    }
    let (n, errs) = vouchers::post_many(&db, &allowed, user.username())?;
    db.log(
        user.username(),
        "凭证",
        "批量记账",
        &format!("{n} 张（拒绝 {denied} 张越权）"),
    )?;
    Ok(Json(json!({ "ok": n, "errors": errs, "denied": denied })))
}

pub(super) async fn voucher_unpost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 反记账（已记账 → 未记账），修改前需先反记账
    user.require(Perm::VoucherUnpost)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权反记账该凭证"));
    }
    vouchers::unpost(&db, id)?;
    Ok(Json(json!({"ok": true, "action": "unposted"})))
}

pub(super) async fn voucher_reverse(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ReverseVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权冲销该凭证"));
    }
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    // 缺省用期间末日：红字冲销常发生在已结账的历史期间，用"今天"会与所属期间不一致
    let date = if req.date.is_empty() {
        period.last_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let nid = vouchers::reverse(&db, id, user.username(), period, date)?;
    Ok(Json(json!({"id": nid})))
}

pub(super) async fn voucher_renumber(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<RenumberVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = &user.db;
    // 断号重排会重排该期间全部字号（含他人凭证），数据范围受限的账号不得操作
    let scope = &user.user.data_scope;
    if scope.own_voucher_only
        || !scope.account_from.trim().is_empty()
        || !scope.account_to.trim().is_empty()
    {
        return Err(AppError::forbidden("数据范围受限的账号不能执行断号重排"));
    }
    let period = if req.period > 0 {
        // 非法期间（如 202513）必须 400：静默落到当前期间会重排错期间的凭证号
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let word = if req.word.trim().is_empty() {
        "记".to_string()
    } else {
        req.word.trim().to_string()
    };
    let n = vouchers::renumber(&db, period, &word)?;
    Ok(Json(json!({"ok": true, "renumbered": n})))
}

pub(super) async fn voucher_delete(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    // 加载后做守卫校验：状态、结账期间、数据权限
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权删除该凭证"));
    }
    // 启用审核环节的账套：已审核凭证须先反审核
    if v.status == VoucherStatus::Audited && db.options().enable_audit {
        return Err(AppError::bad_request("已审核凭证不能删除，请先反审核"));
    }
    // 已记账凭证需先反记账才能删除（未记账凭证可直接删除）
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能直接删除，请先反记账"));
    }
    // 期间是否已结账
    if let Some(closed) = findb::periods::closed_upto(&*db)? {
        if v.period <= closed {
            return Err(AppError::bad_request(format!(
                "{} 及以前期间已结账，不能删除该凭证",
                closed.label()
            )));
        }
    }
    vouchers::delete(&db, id)?;
    db.log(user.username(), "凭证", "删除", &v.voucher_no())?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn get_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::balance::LedgerRow>>, AppError> {
    user.require(Perm::FinReport)?;
    let (db, chart, lq) = ledger_query_from(&state, &user, &q)?;
    let rows = balances::ledger(&db, &chart, &lq)?;
    Ok(Json(rows))
}

/// 总账（按期间汇总）
pub(super) async fn get_general_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::GeneralLedgerRow>>, AppError> {
    user.require(Perm::FinReport)?;
    let (db, _chart, lq) = ledger_query_from(&state, &user, &q)?;
    Ok(Json(balances::general_ledger(&db, &lq)?))
}

/// 日记账（现金/银行）
pub(super) async fn get_journal(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::JournalRow>>, AppError> {
    user.require(Perm::FinReport)?;
    let (db, chart, lq) = ledger_query_from(&state, &user, &q)?;
    Ok(Json(balances::journal(&db, &chart, &lq)?))
}

pub(super) async fn print_voucher_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = &user.db;
    let company = user.company.clone();
    let chart = accounts::chart(&db)?;
    let aux_names = findb::auxs::full_name_map(&*db)?;

    let mut query = VoucherQuery::default().with_data_scope(&user.user);
    query.asc = true;
    let from = period_param(q.get("from"), current_period(&state, &user))?;
    let to = period_param(q.get("to"), from)?;
    if from > to {
        return Err(AppError::bad_request(format!(
            "起始期间不能晚于结束期间：{} > {}",
            period_to_str(from),
            period_to_str(to)
        )));
    }
    query.from = Some(from);
    query.to = Some(to);
    query.limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .map(|v| v.clamp(1, 1000))
        .or(Some(500));
    let mut list = vouchers::list(&db, &query)?;
    vouchers::fill_entries(&db, &mut list)?;
    list.retain(|v| user.user.can_see_voucher(v));
    if list.is_empty() {
        return Err(AppError::bad_request("没有可打印的凭证"));
    }

    let aux_label = |aux: &fincore::AuxRef| {
        let mut parts = Vec::new();
        for k in fincore::account::AuxKind::ALL {
            if *k == fincore::account::AuxKind::CashFlow {
                continue;
            }
            if let Some(v) = aux.get(*k) {
                let name = aux_names
                    .get(&format!("{}:{}", k.code(), v))
                    .cloned()
                    .unwrap_or_default();
                parts.push(if name.is_empty() { v.to_string() } else { name });
            }
        }
        parts.join("/")
    };
    let prints = findb::printform::vouchers_to_print(&list, &chart, &aux_label);
    let html = findb::printform::voucher_form_html(
        &company,
        &format!("{}~{}", period_to_str(from), period_to_str(to)),
        &prints,
        true,
        &db.options().print,
    );
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 账簿套打（明细账 / 总账 / 日记账）HTML
pub(super) async fn print_ledger_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::FinReport)?;
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少科目编码参数 code"));
    }
    if !user.user.can_see_account(&code) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let db = &user.db;
    let company = user.company.clone();
    let chart = accounts::chart(&db)?;
    let from = period_param(q.get("from"), current_period(&state, &user))?;
    let to = period_param(q.get("to"), from)?;
    if from > to {
        return Err(AppError::bad_request(format!(
            "起始期间不能晚于结束期间：{} > {}",
            period_to_str(from),
            period_to_str(to)
        )));
    }
    let ktype = q.get("type").map(String::as_str).unwrap_or("detail");
    let include_children = q
        .get("include_children")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let posted_only = q
        .get("posted_only")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let lq = LedgerQuery {
        code: code.clone(),
        include_children,
        aux: None,
        from,
        to,
        posted_only,
        prepared_by: None,
        code_from: None,
        code_to: None,
    }
    .with_user_scope(&user.user);
    let acct = chart
        .get(&code)
        .map(|a| format!("{} {}", code, a.name))
        .unwrap_or(code.clone());

    // 期初余额
    let snap = BalanceSnapshot::load(
        &db,
        &BalanceQuery::range(from, from).with_user_scope(&user.user),
    )?;
    let (bd, bamt) = fincore::signed_to_dir_amount(snap.for_account(&code, None).begin);
    let begin_dir = if bamt.is_zero() {
        "平".to_string()
    } else {
        bd.label().to_string()
    };

    let (title, rows) = match ktype {
        "general" => {
            let list = balances::general_ledger(&db, &lq)?;
            let rows = list
                .into_iter()
                .map(|r| findb::printform::LedgerPrintRow {
                    date: r.period.code(),
                    voucher_no: String::new(),
                    summary: r.summary,
                    debit: r.debit,
                    credit: r.credit,
                    dir: if r.balance.is_zero() {
                        "平".to_string()
                    } else {
                        r.dir.label().to_string()
                    },
                    balance: r.balance,
                })
                .collect::<Vec<_>>();
            ("总账".to_string(), rows)
        }
        "journal" => {
            let list = balances::journal(&db, &chart, &lq)?;
            let rows = list
                .into_iter()
                .map(|r| findb::printform::LedgerPrintRow {
                    date: r.date.format("%Y-%m-%d").to_string(),
                    voucher_no: r.voucher_no,
                    summary: r.summary,
                    debit: r.debit,
                    credit: r.credit,
                    dir: if r.balance.is_zero() {
                        "平".to_string()
                    } else {
                        r.dir.label().to_string()
                    },
                    balance: r.balance,
                })
                .collect::<Vec<_>>();
            ("日记账".to_string(), rows)
        }
        _ => {
            let list = balances::ledger(&db, &chart, &lq)?;
            let rows = list
                .into_iter()
                .map(|r| findb::printform::LedgerPrintRow {
                    date: r.date.format("%Y-%m-%d").to_string(),
                    voucher_no: r.voucher_no,
                    summary: r.summary,
                    debit: r.debit,
                    credit: r.credit,
                    dir: if r.balance.is_zero() {
                        "平".to_string()
                    } else {
                        r.dir.label().to_string()
                    },
                    balance: r.balance,
                })
                .collect::<Vec<_>>();
            ("明细账".to_string(), rows)
        }
    };

    let ledger = findb::printform::LedgerPrint {
        title,
        account_name: acct,
        period_label: format!("{}~{}", period_to_str(from), period_to_str(to)),
        begin_dir,
        begin_balance: bamt,
        rows,
        page_from_1: true,
    };
    let html = findb::printform::ledger_form_html(&company, &ledger, &db.options().print);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

pub(super) async fn list_voucher_attachments(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权查看该凭证的附件"));
    }
    let list: Vec<serde_json::Value> = findb::attach::list(&db, id)?
        .iter()
        .map(attachment_json)
        .collect();
    Ok(Json(list))
}

pub(super) async fn upload_voucher_attachment(
    State(_state): State<Arc<WebState>>,
    Path(id): Path<i64>,
    user: CurrentUser,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    // 注意：`&findb::Db` 持有的是 !Sync 的连接引用，**不能跨 await 持有**，
    // 否则整个 future 变成 !Send，Handler 不成立（路由注册处会报 trait 未实现）。
    // 所以先在块内做完「这张凭证能不能动」的校验，再解析上传体，最后落库。
    let voucher_no = {
        let db = &user.db;
        let v =
            vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
        if !user.user.can_see_voucher(&v) {
            return Err(AppError::forbidden("无权为该凭证上传附件"));
        }
        v.voucher_no().to_string()
    };

    // 用 axum 自带的 multipart 提取器（背后是 multer）：边界解析、分片、
    // Content-Disposition 都按 RFC 处理，且不引入任何新依赖
    // （旧的 multipart 0.18 会拖进 hyper 0.10/iron/time 0.1 等 4 个 RUSTSEC 漏洞）。
    // 体大小已由路由层的 DefaultBodyLimit（12 MB）兜底。
    let mut name = String::new();
    let mut data: Option<Vec<u8>> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("上传解析失败：{e}")))?
    {
        if field.name() == Some("file") {
            name = field.file_name().unwrap_or("attachment").to_string();
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad_request(format!("读取上传内容失败：{e}")))?;
            data = Some(bytes.to_vec());
            break;
        }
    }
    let data = data.ok_or_else(|| AppError::bad_request("缺少文件字段 file"))?;
    if data.is_empty() {
        return Err(AppError::bad_request("上传文件为空"));
    }
    if data.len() > findb::attach::MAX_FILE_SIZE {
        return Err(AppError::bad_request("文件超过 10MB 上限"));
    }
    let db = &user.db;
    let aid = findb::attach::add(&db, id, &name, &data, user.username())?;
    db.log(
        user.username(),
        "凭证",
        "上传附件",
        &format!("{} 附件#{} {}", voucher_no, aid, name),
    )?;
    Ok(Json(json!({ "id": aid })))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(Deserialize)]
pub(super) struct AmendReq {
    /// 更正原因（必填）。没有原因的更正等于没更正。
    reason: String,
}

#[derive(Deserialize)]
pub(super) struct VoidReq {
    /// 缺省 = true（作废）；恢复请显式传 false
    #[serde(default = "default_true")]
    void: bool,
}

#[derive(Deserialize, Default)]
pub(super) struct ReverseVoucherReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
}

#[derive(Deserialize, Default)]
pub(super) struct RenumberVoucherReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub word: String,
}
