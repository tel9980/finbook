//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

/// 可承诺量（ATP）
///
/// 读权限与库存报表同级：ATP 由现货、在途、已占用三项算出，实质是库存视图。
/// 三个分量一并返回而不是只给结果——只看到一个 ATP 数字，被算错了也不知道错在哪。
pub(super) async fn get_atp(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let item = q.get("item").map(|s| s.trim()).unwrap_or("");
    if item.is_empty() {
        return Err(AppError::bad_request("缺少存货编码 item"));
    }
    let a = findb::scm::atp(&db, item)?;
    let mut out = json!({
        "item": a.item,
        "on_hand": a.on_hand,
        "incoming": a.incoming,
        "incoming_dated": a.incoming_dated,
        "incoming_undated": a.incoming_undated,
        "earliest_ready": a.earliest_ready,
        "committed": a.committed,
        "atp": a.atp,
        "formula": "ATP = 现有可用库存 + 在途（未完工生产计划） − 已占用（已确认订单未发货）",
        "note": "待检（来料检验未转正）库存不计入可用；草稿/作废订单不占用；在途含已下推的草稿产单（草稿也是已认领的产能）",
    });
    // 带 date 时改用「到该日为止可承诺量」口径：只认**有计划完工日且不晚于该日**的
    // 在途。没排期的那部分答不出「什么时候到」，算进来等于给一个自己都给不出的
    // 交期承诺 —— 所以它单列在 incoming_undated，由界面提示「不能用来承诺交期」。
    if let Some(d) = q.get("date").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let on = NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
        let u = findb::scm::atp_until(&db, item, on)?;
        out["as_of"] = json!(d);
        out["incoming"] = json!(u.incoming);
        out["atp"] = json!(u.atp);
        out["date_formula"] = json!("到该日为止 = 现有可用 + 计划完工日 ≤ 该日的在途 − 已占用（**未排期的在途不计入**，它没有交期）");
    }
    Ok(Json(out))
}

/// 立即执行计划任务（写 books_dir/exports/）
pub(super) async fn run_export_schedule(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let dir = state.books_dir.join("exports");
    let path = findb::exports::sched_run(&db, id, &dir)?;
    db.log(user.username(), "导出", "立即执行计划任务", &path)?;
    Ok(Json(json!({ "ok": true, "path": path })))
}

pub(super) async fn get_dashboard(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Dashboard>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let (v, e, a) = db.stats()?;
    let start = db.options().start_period;
    let cur = current_period(&state, &user);
    let closed = periods::closed_upto(&*db)?;
    Ok(Json(Dashboard {
        company: user.company.clone(),
        start_period: period_to_str(start),
        current_period: period_to_str(cur),
        closed_upto: closed.map(period_to_str),
        vouchers: v,
        entries: e,
        accounts: a,
        brief: home_brief(db, cur, &user.user).ok(),
    }))
}

/// 我的工作台：按岗位权限动态聚合业务卡片 / 我的待办 / 多期趋势。
/// 凭证/资金/销售/采购/仓管/生产/成本/报销/审批 域在 findb::workbench（复用既有 Perm），
/// 报表域（语句表）在此注入——门槛 Report（人人首页可进），域内再按各域权限裁剪。
pub(super) async fn get_workbench(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<findb::workbench::WbOut>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let cur = current_period(&state, &user);
    let n = q
        .get("periods")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(12)
        .clamp(3, 24);
    let mut out = findb::workbench::collect(&db, &user.user, cur, n)?;
    if user.can(Perm::FinReport) {
        wb_report_domain(&db, &user, cur, n, &mut out)?;
    }
    Ok(Json(out))
}

/// Ctrl+K 快速搜索：凭证（复用 VoucherQuery → data_scope 落地）/ 采购订单 / 销售订单 /
/// 请购（后三类需 OrderOps）/ 我的报销（doc_in_scope 数据范围，防跨人泄露）。
pub(super) async fn quick_search(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let kw = q.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    if kw.is_empty() {
        return Ok(Json(json!({ "rows": [] })));
    }
    let db = &user.db;
    let cur = current_period(&state, &user);
    let like = format!("%{kw}%");
    let mut rows: Vec<serde_json::Value> = Vec::new();

    // 凭证：复用列表口径（关键字 = 摘要/科目/凭证号，数据范围同步生效）
    let vq = findb::vouchers::VoucherQuery {
        keyword: Some(kw.clone()),
        limit: Some(4),
        asc: false,
        ..Default::default()
    }
    .with_data_scope(&user.user);
    for v in findb::vouchers::list(&db, &vq)? {
        rows.push(json!({
            "kind": "voucher",
            "id": v.id,
            "label": format!("{}-{} {}", v.word, v.no, v.memo),
            "sub": v.date.format("%Y-%m-%d").to_string(),
            "view": "vouchers",
        }));
    }

    if user.can(Perm::OrderOps) {
        let pack = |kind: &str,
                    view: &str,
                    sql: &str,
                    db: &findb::Db,
                    like: &str|
         -> Vec<serde_json::Value> {
            let mut out = Vec::new();
            if let Ok(mut st) = db.conn().prepare(sql) {
                if let Ok(iter) = st.query_map(rusqlite::params![like, like], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                }) {
                    for row in iter.flatten() {
                        out.push(json!({
                            "kind": kind,
                            "id": row.0,
                            "label": format!("{} {}", row.1, row.2),
                            "sub": "",
                            "view": view,
                        }));
                    }
                }
            }
            out
        };
        rows.extend(pack(
            "po",
            "po-doc",
            "SELECT id, no, supplier_name FROM purchase_order WHERE no LIKE ?1 OR supplier_name LIKE ?2 ORDER BY id DESC LIMIT 3",
            &db,
            &like,
        ));
        rows.extend(pack(
            "so",
            "so-doc",
            "SELECT id, no, customer_name FROM sales_order WHERE no LIKE ?1 OR customer_name LIKE ?2 ORDER BY id DESC LIMIT 3",
            &db,
            &like,
        ));
        rows.extend(pack(
            "req",
            "po-doc",
            "SELECT id, no, item_name FROM purchase_req WHERE no LIKE ?1 OR item_name LIKE ?2 ORDER BY id DESC LIMIT 3",
            &db,
            &like,
        ));
    }

    // 档案：编码/名称直达辅助档案页（kind 带到前端切页签）。
    // 门槛与档案页的新增按钮一致（AuxEdit），只读账号不打扰。
    if user.can(Perm::AuxEdit) {
        let kind_label = |k: &str| match k {
            "customer" => "客户",
            "supplier" => "供应商",
            "dept" => "部门",
            "employee" => "职员",
            "project" => "项目",
            "item" => "存货",
            "bank" => "银行账户",
            _ => "档案",
        };
        if let Ok(mut st) = db.conn().prepare(
            "SELECT id, kind, code, name FROM aux_entity
             WHERE disabled = 0 AND (code LIKE ?1 OR name LIKE ?2)
             ORDER BY kind, code LIMIT 6",
        ) {
            if let Ok(iter) = st.query_map(rusqlite::params![like, like], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            }) {
                for row in iter.flatten() {
                    rows.push(json!({
                        "kind": "aux",
                        "id": row.0,
                        "label": format!("{} {} {}", kind_label(&row.1), row.2, row.3),
                        "sub": "",
                        "view": "aux",
                        "aux_kind": row.1,
                    }));
                }
            }
        }
    }

    // 我的报销（数据范围与列表页一致）
    let mut mine = 0;
    for c in findb::business::claim_list(&db, cur, None)? {
        if mine >= 3 {
            break;
        }
        if !claim_visible(&user, &c) {
            continue;
        }
        if c.no.contains(&kw) || c.reason.contains(&kw) {
            rows.push(json!({
                "kind": "claim",
                "id": c.id,
                "label": format!("{} {}", c.no, c.reason),
                "sub": c.applicant,
                "view": "claims",
            }));
            mine += 1;
        }
    }

    Ok(Json(json!({ "rows": rows })))
}

/// 管理员「账目总览」：只读视角的账目全貌（仅系统管理员可访问）
pub(super) async fn get_overview(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.user.is_admin() {
        return Err(AppError::forbidden("该入口仅限系统管理员使用"));
    }
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    let o = findb::reports::overview(&db, period)?;
    let recent: Vec<VoucherListItem> = o.recent.iter().map(to_item).collect();
    let a = findb::advanced::financial_analysis(&db, period, Some(&user.user))?;
    let trend: Vec<serde_json::Value> = a
        .trend
        .iter()
        .map(|t| {
            json!({
                "period": period_to_str(t.period),
                "revenue": t.revenue.fmt_money(),
                "cost": t.cost.fmt_money(),
                "net_profit": t.net_profit.fmt_money(),
                "cum_revenue": t.cum_revenue.fmt_money(),
                "cum_cost": t.cum_cost.fmt_money(),
                "cum_net_profit": t.cum_net_profit.fmt_money(),
                "anomaly_revenue": t.anomaly_revenue,
                "anomaly_cost": t.anomaly_cost,
                "anomaly_net_profit": t.anomaly_net_profit,
            })
        })
        .collect();
    let driver_json = |d: &findb::advanced::DriverItem| {
        json!({
            "name": d.name,
            "amount": d.amount.fmt_money(),
            "prev_amount": d.prev_amount.fmt_money(),
        })
    };
    Ok(Json(json!({
        "company": o.company,
        "period": period_to_str(o.period),
        "closed_upto": o.closed_upto.map(period_to_str),
        "vouchers": o.vouchers,
        "entries": o.entries,
        "accounts": o.accounts,
        "unposted": o.unposted,
        "posted": o.posted,
        "totals": {
            "total_asset": o.totals.total_asset.fmt_money(),
            "total_liab": o.totals.total_liab.fmt_money(),
            "equity": o.totals.equity.fmt_money(),
            "revenue": o.totals.revenue.fmt_money(),
            "cost": o.totals.cost.fmt_money(),
            "net_profit": o.totals.net_profit.fmt_money(),
        },
        "invoice_in": {"amount_tax": o.invoice_in.0.fmt_money(), "count": o.invoice_in.1},
        "invoice_out": {"amount_tax": o.invoice_out.0.fmt_money(), "count": o.invoice_out.1},
        "recent": recent,
        "analysis": {
            "trend": trend,
            "revenue_drivers": a.revenue_drivers.iter().map(driver_json).collect::<Vec<_>>(),
            "cost_drivers": a.cost_drivers.iter().map(driver_json).collect::<Vec<_>>(),
            "profit_drivers": a.profit_drivers.iter().map(driver_json).collect::<Vec<_>>(),
            "anomaly_notes": a.anomaly_notes,
            "ratios": a.ratios.iter().map(|r| json!({
                "key": r.key, "name": r.name, "value": r.value.fmt_plain(), "display": r.display, "formula": r.formula,
            })).collect::<Vec<_>>(),
        },
    })))
}

// ---------------------------------------------------------------------------
// 科目 / 凭证
// ---------------------------------------------------------------------------

pub(super) async fn list_accounts(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<fincore::Account>>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(accounts::list(&*db)?))
}

/// 补齐内置科目表（旧账套补入新版本新增的科目，仅管理员）
pub(super) async fn fill_default_accounts(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.user.is_admin() {
        return Err(AppError::forbidden("该操作仅限系统管理员"));
    }
    let db = &user.db;
    let before = accounts::list(&*db)?.len();
    let inserted = accounts::fill_missing_defaults(&*db)?;
    if inserted > 0 {
        db.log(
            user.username(),
            "科目",
            "补齐科目表",
            &format!(
                "补入 {inserted} 个内置科目（{before} → {}）",
                before + inserted
            ),
        )?;
    }
    Ok(Json(json!({
        "inserted": inserted,
        "total": before + inserted,
    })))
}

/// 下载导入模板（列头 + 示例行；主数据三平台列头一致，CSV 带 BOM 供 Excel 直接打开）
///
/// 与其余导入接口一样挂登录 + Report 权限：模板虽只含静态列头，但它是导入入口的
/// 一半（另一半 `import_analyze` 严管），放行未登录请求会让"能否探测本系统用哪些
/// 科目/存货字段"绕过所有鉴权。
pub(super) async fn import_template(
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let kind = q.get("kind").map(String::as_str).unwrap_or("");
    let rows: Vec<Vec<&str>> = match kind {
        // 模板的列头必须与 `findb::imports` 的解析器和 `sheets.rs` 的导出一致 ——
        // 三处任何一处单独改，用户就会拿到「导得出来、导不回去」的表。
        //
        // 档案模板把客户/供应商/银行/存货各自的扩展列并成一张宽表（模板只能给
        // 一种形状）；解析器先认列名，所以哪一种档案都能照着只填自己那几列。
        // 老的四列文件仍然能导（认不出列名就退回按位置取）。
        "aux" => vec![
            vec![
                "类型",
                "编码",
                "名称",
                "备注",
                "上级编码",
                "状态",
                "税点%",
                "开票情况",
                "信用额度",
                "开户行",
                "银行账号",
                "规格型号",
                "计量单位",
            ],
            vec![
                "客户",
                "C01",
                "示例客户",
                "月结 30 天",
                "",
                "启用",
                "13",
                "每次都开票",
                "50000",
                "",
                "",
                "",
                "",
            ],
            vec![
                "供应商",
                "S01",
                "示例供应商",
                "",
                "",
                "启用",
                "3",
                "有时开有时不开",
                "",
                "",
                "",
                "",
                "",
            ],
            vec![
                "银行",
                "B01",
                "基本户",
                "",
                "",
                "启用",
                "",
                "",
                "",
                "工商银行江门分行",
                "6222xxxx",
                "",
                "",
            ],
            vec![
                "存货",
                "I01",
                "示例存货",
                "",
                "",
                "启用",
                "",
                "",
                "",
                "",
                "",
                "φ20 圆钢",
                "吨",
            ],
        ],
        "item" => vec![
            vec![
                "编码",
                "名称",
                "保质期天",
                "安全库存",
                "备注",
                "计量单位",
                "状态",
            ],
            vec!["I01", "示例存货", "365", "100", "", "吨", "启用"],
        ],
        "account" => vec![
            vec![
                "编码",
                "名称",
                "类别",
                "方向",
                "备注",
                "辅助核算维度",
                "数量单位",
                "币种",
                "现金科目",
                "银行科目",
                "状态",
            ],
            vec![
                "1001",
                "库存现金",
                "资产",
                "借",
                "",
                "",
                "",
                "",
                "是",
                "",
                "启用",
            ],
            vec![
                "112201",
                "应收账款-货款",
                "资产",
                "借",
                "",
                "客户",
                "",
                "",
                "",
                "",
                "启用",
            ],
            vec![
                "140501",
                "原材料",
                "资产",
                "借",
                "",
                "存货",
                "吨",
                "",
                "",
                "",
                "启用",
            ],
        ],
        "opening_stock" => vec![
            vec![
                "存货编码",
                "仓库",
                "数量",
                "单价",
                "批次号",
                "生产日期",
                "备注",
            ],
            vec![
                "I01",
                "W01",
                "100",
                "12.5",
                "BT0001",
                "2026-01-01",
                "盘点转入",
            ],
        ],
        "begin" => vec![
            vec![
                "科目编码",
                "科目名称",
                "方向",
                "期初余额",
                "累计借方",
                "累计贷方",
                "数量",
                "客户编码",
                "供应商编码",
                "存货编码",
                "项目编码",
                "部门编码",
                "职员编码",
                "银行账户",
            ],
            vec![
                "1001",
                "库存现金",
                "借",
                "1000",
                "0",
                "0",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
            ],
            // 分户期初：金额挂在应收账款上、客户靠「客户编码」这一列分。
            // 小厂最常搬的就是这笔「华达还欠我 5 万」，没有这一列搬过去就查不到分户。
            vec![
                "112201",
                "应收账款-货款",
                "借",
                "50000",
                "0",
                "0",
                "",
                "C01",
                "",
                "",
                "",
                "",
                "",
                "",
            ],
        ],
        "arap_opening" => vec![
            vec![
                "类型",
                "客商编码",
                "单据号",
                "单据日期",
                "金额",
                "客商名称",
                "备注",
            ],
            vec![
                "应收",
                "C01",
                "XSQ-0001",
                "2025-12-31",
                "5000",
                "客户甲",
                "期初欠款",
            ],
            vec![
                "应付",
                "S01",
                "CGQ-0001",
                "2025-12-31",
                "3000",
                "供应商甲",
                "",
            ],
        ],
        "voucher" => vec![
            vec!["日期", "凭证字", "摘要", "科目编码", "借方", "贷方"],
            vec!["2026-01-31", "记", "期初入库", "1001", "100", "0"],
            vec!["2026-01-31", "记", "期初入库", "1405", "0", "100"],
        ],
        _ => {
            return Err(AppError::bad_request(
                "未知模板 kind：aux / item / account / opening_stock / begin / voucher",
            ))
        }
    };
    let mut body = String::from("\u{feff}");
    for row in &rows {
        body.push_str(&row.join(","));
        body.push('\n');
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{kind}_template.csv\"").as_str(),
            ),
        ],
        body,
    )
        .into_response())
}

/// 预检：写入**之前**把会失败 / 要留意的问题一次说清楚。
///
/// 原来只回答「有没有不存在的科目」，而导入真正会失败的常见原因还有几个：
/// 期间已结账、科目已停用、编码在文件里重复。用户点完「执行导入」才发现时，
/// 错的那部分数据可能已经写进去了（档案类是逐条幂等写入、凭证类更麻烦）。
///
/// 缺失科目不算阻断 —— 它可以靠映射解决，所以由 `blocking()` 排除。
pub(super) async fn import_analyze(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ImportAnalyzeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(import_perm(&req.kind)?)?;
    let has_file = req
        .file
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !has_file && req.text.trim().is_empty() {
        return Err(AppError::bad_request("请选择 Excel 文件或粘贴 CSV 内容"));
    }
    let file_bytes: Option<Vec<u8>> = if has_file {
        Some(b64_decode(req.file.as_deref().unwrap_or(""))?)
    } else {
        None
    };
    let key = user.book_key.clone();
    let state2 = state.clone();
    // 预检也要解析整份 Excel：同样放阻塞池
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, AppError> {
        let db = state2.db_for(&key)?;
        let tmpl = findb::imports::ImportTemplate::parse(&req.template);
        let is_begin = req.kind != "voucher";
        let text = if let Some(bytes) = &file_bytes {
            let rows = findb::imports::read_xlsx_bytes(bytes)?;
            findb::imports::xlsx_to_csv_text(&rows)
        } else {
            req.text.clone()
        };
        // 主数据类（aux/item/account/opening_stock）无科目引用——按科目列的那部分
        // 预检跳过（行级错误由执行时 warnings 呈现），但**期间结账检查对所有类型
        // 都要做**：凭证与期初都落到具体期间，闭着的时候导进去也记账不了。
        let has_account_refs = !matches!(
            req.kind.as_str(),
            "aux" | "item" | "account" | "opening_stock" | "arap_opening"
        );
        let ymm = if req.period > 0 {
            period_checked(req.period)?.ymm()
        } else {
            current_period(&state2, &user).ymm()
        };
        let pv = findb::imports::analyze_import_mapped(
            &db,
            if has_account_refs { &text } else { "" },
            tmpl,
            is_begin,
            Some(fincore::Period::from_ymm(ymm)),
            &req.mapping,
        )?;
        let items: Vec<serde_json::Value> = pv
            .missing_accounts
            .iter()
            .map(|m| json!({ "code": m.code, "count": m.count }))
            .collect();
        let issues: Vec<serde_json::Value> = pv
            .issues
            .iter()
            .map(|i| json!({ "level": i.level, "category": i.category, "detail": i.detail }))
            .collect();
        Ok(json!({
            "missing": items,
            // 新增：逐条问题 + 汇总。前端把 issues 按 category 分组显示，
            // blocking=true 时禁用「执行导入」按钮（但不阻断 API 调用 ——
            // 真正把关仍在写入路径，这里是「早告诉」而不是「唯一拦点」）。
            "issues": issues,
            "errors": pv.errors(),
            "warns": pv.warns(),
            "blocking": pv.blocking(),
            "total_rows": pv.total_rows,
        }))
    })
    .await
    .map_err(|e| AppError::Internal(format!("导入预检失败：{e}")))??;
    Ok(Json(out))
}

/// 执行导入（期初余额表 / 凭证），带科目映射；支持 CSV 文本或 Excel 文件
pub(super) async fn import_run(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ImportRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(import_perm(&req.kind)?)?;
    let has_file = req
        .file
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !has_file && req.text.trim().is_empty() {
        return Err(AppError::bad_request("请粘贴 CSV 内容或选择 Excel 文件"));
    }
    // 文件解码放请求线程，重活（解析 Excel + 批量写库）整体进阻塞池
    let file_bytes: Option<Vec<u8>> = if has_file {
        Some(b64_decode(req.file.as_deref().unwrap_or(""))?)
    } else {
        None
    };
    let key = user.book_key.clone();
    let who = user.username().to_string();
    let fallback_ymm = current_period(&state, &user).ymm();
    let explicit_ymm = if req.period > 0 {
        Some(period_checked(req.period)?.ymm())
    } else {
        None
    };
    let state2 = state.clone();
    let kind = req.kind.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, AppError> {
        let db = state2.db_for(&key)?;
        let tmpl = findb::imports::ImportTemplate::parse(&req.template);
        let res = match req.kind.as_str() {
            "voucher" => {
                let period = fincore::Period::from_ymm(explicit_ymm.unwrap_or(fallback_ymm));
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_vouchers_bytes(
                        &db,
                        period,
                        bytes,
                        &who,
                        &req.mapping,
                        tmpl,
                    )?
                } else {
                    findb::imports::import_vouchers(
                        &db,
                        period,
                        &req.text,
                        &who,
                        &req.mapping,
                        tmpl,
                    )?
                }
            }
            "aux" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_aux_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_aux(&db, &req.text, &who)?
                }
            }
            "item" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_items_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_items(&db, &req.text, &who)?
                }
            }
            "account" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_accounts_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_accounts(&db, &req.text, &who)?
                }
            }
            "opening_stock" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_opening_stock_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_opening_stock(&db, &req.text, &who)?
                }
            }
            "arap_opening" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_arap_opening_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_arap_opening(&db, &req.text, &who)?
                }
            }
            // "begin" 及未列出的既有类型（kind 已由 import_perm 校验，未知 kind 到不了这里）
            _ => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_begin_bytes(&db, bytes, &who, &req.mapping, tmpl)?
                } else {
                    findb::imports::import_begin(&db, &req.text, &who, &req.mapping, tmpl)?
                }
            }
        };
        // 明确告诉用户「导完之后该干什么」。
        //
        // 为什么要加这个：导入的凭证落 `Draft`（与手工录入一致，导入不绕过内控），
        // 但「导入成功」很容易被理解成「已经进账了」。实际上还要审核 → 记账，
        // 资金类凭证还要出纳签字。不说清楚，用户会以为导完就算完，下个月
        // 对账才发现一笔没上。
        let next_steps: Vec<String> = match kind.as_str() {
            "voucher" => {
                let opts = findb::options_of(db.conn());
                let mut v = vec![format!(
                    "{} 张凭证已导入为**草稿**（导入不绕过内控，与手工录入一致）",
                    res.ok
                )];
                if opts.enable_audit {
                    v.push("本账套开启了审核环节 → 需先「审核」再「记账」".to_string());
                }
                if opts.require_cashier {
                    v.push(
                        "本账套要求出纳签字 → 涉及现金/银行科目的凭证，出纳签字后才能记账"
                            .to_string(),
                    );
                }
                v.push("核对无误后到「凭证」页批量审核 → 批量记账".to_string());
                v
            }
            "begin" | "opening_stock" | "arap_opening" => {
                vec![
                    "期初数据已写入。请确认「期初余额」页的合计与对方账套一致后再启用记账"
                        .to_string(),
                ]
            }
            _ => vec![
                "档案类导入为逐条幂等写入：中断后重跑会跳过已存在的编码，可安全重导".to_string(),
            ],
        };
        Ok(json!({
            "ok": res.ok,
            "skipped": res.skipped,
            "warnings": res.warnings,
            "next_steps": next_steps,
        }))
    })
    .await
    .map_err(|e| AppError::Internal(format!("导入任务失败：{e}")))??;
    Ok(Json(out))
}

pub(super) async fn export_vouchers(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let mut query = VoucherQuery::default().with_data_scope(&user.user);
    if let Some(p) = q.get("period").and_then(|s| parse_period(s)) {
        query.from = Some(p);
        query.to = Some(p);
    }
    if let Some(f) = q.get("from").and_then(|s| parse_period(s)) {
        query.from = Some(f);
    }
    if let Some(t) = q.get("to").and_then(|s| parse_period(s)) {
        query.to = Some(t);
    }
    if let Some(kw) = q.get("q") {
        let kw = kw.trim().to_string();
        if !kw.is_empty() {
            query.keyword = Some(kw);
        }
    }
    if let Some(st) = q.get("status").and_then(|s| parse_voucher_status(s)) {
        query.status = Some(st);
    }
    query.asc = true;
    query.limit = Some(5000);
    let mut list = vouchers::list(&db, &query)?;
    vouchers::fill_entries(&db, &mut list)?;
    let chart = accounts::chart(&db)?;
    let mut rows: Vec<Vec<String>> = vec![[
        "日期",
        "凭证号",
        "状态",
        "摘要",
        "科目编码",
        "科目名称",
        "借方",
        "贷方",
        "制单人",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()];
    let mut n = 0usize;
    for v in &list {
        if !user.user.can_see_voucher(v) {
            continue;
        }
        for e in &v.entries {
            if e.is_blank() {
                continue;
            }
            let name = chart
                .get(&e.account_code)
                .map(|a| a.name.clone())
                .unwrap_or_default();
            rows.push(vec![
                v.date.format("%Y-%m-%d").to_string(),
                v.voucher_no(),
                v.status.label().to_string(),
                e.summary.clone(),
                e.account_code.clone(),
                name,
                e.debit.fmt_plain(),
                e.credit.fmt_plain(),
                v.prepared_by.clone(),
            ]);
            n += 1;
        }
    }
    db.log(user.username(), "导出", "导出凭证", &format!("{n} 行"))?;
    Ok(csv_response("vouchers.csv", rows))
}

pub(super) async fn export_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let (db, chart, lq) = ledger_query_from(&state, &user, &q)?;
    let acct = chart
        .get(&lq.code)
        .map(|a| format!("{} {}", a.code, a.name))
        .unwrap_or_else(|| lq.code.clone());
    let list = balances::ledger(&db, &chart, &lq)?;
    let has_qty = list.iter().any(|r| r.qty_balance.is_some());
    let mut header: Vec<String> = ["日期", "凭证号", "摘要", "借方", "贷方", "方向", "余额"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if has_qty {
        header.push("数量余额".to_string());
    }
    let mut rows = vec![header];
    for r in &list {
        let mut row = vec![
            r.date.format("%Y-%m-%d").to_string(),
            r.voucher_no.clone(),
            r.summary.clone(),
            r.debit.fmt_plain(),
            r.credit.fmt_plain(),
            r.dir.label().to_string(),
            r.balance.fmt_plain(),
        ];
        if has_qty {
            row.push(r.qty_balance.map(|q| q.fmt_qty()).unwrap_or_default());
        }
        rows.push(row);
    }
    db.log(
        user.username(),
        "导出",
        "导出明细账",
        &format!("{acct} {} 行", rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response("ledger.csv", rows))
}

pub(super) async fn export_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    let list = business::payroll_list(&db, period)?;
    let list = scope_payroll(&user, &list);
    let mut rows = vec![[
        "员工",
        "部门",
        "应发",
        "社保(个人)",
        "公积金(个人)",
        "专项附加",
        "个税",
        "实发",
        "社保(单位)",
        "公积金(单位)",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>()];
    for p in &list {
        rows.push(vec![
            p.employee.clone(),
            p.dept.clone(),
            p.gross.fmt_plain(),
            p.social.fmt_plain(),
            p.housing.fmt_plain(),
            p.additional.fmt_plain(),
            p.tax.fmt_plain(),
            p.net.fmt_plain(),
            p.social_co.fmt_plain(),
            p.housing_co.fmt_plain(),
        ]);
    }
    db.log(
        user.username(),
        "导出",
        "导出工资表",
        &format!("{} {} 行", period.label(), rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response("payroll.csv", rows))
}

pub(super) async fn export_claims(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    // 空串 = 全部状态：UI 下拉默认值为空，直接 parse 会落到 Draft 只显示草稿
    let status = q
        .get("status")
        .filter(|s| !s.is_empty())
        .map(|s| business::ClaimStatus::parse(s));
    let list = business::claim_list(&db, period, status)?;
    // 与 list_claims 同一个口径（部门范围 + 仅本人经手），不再各写一遍
    let list: Vec<&business::Claim> = scope_claims(&user, &list);
    let mut rows = vec![[
        "单号",
        "业务日期",
        "申请人",
        "部门",
        "事由",
        "金额",
        "状态",
        "审批人",
        "付款人",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>()];
    for c in &list {
        rows.push(vec![
            c.no.clone(),
            c.biz_date.format("%Y-%m-%d").to_string(),
            c.applicant.clone(),
            c.dept.clone(),
            c.reason.clone(),
            c.amount.fmt_plain(),
            c.status.label().to_string(),
            c.approver.clone(),
            c.payer.clone(),
        ]);
    }
    db.log(
        user.username(),
        "导出",
        "导出报销单",
        &format!("{} {} 行", period.label(), rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response("claims.csv", rows))
}

pub(super) async fn export_dunning(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    let kind = q
        .get("kind")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("ar");
    if kind != "ar" && kind != "ap" {
        return Err(AppError::bad_request("kind 必须是 ar 或 ap"));
    }
    let csv = findb::exports::build_csv(&db, &format!("dunning_{}", kind), period)?;
    let (_, rows) = csv;
    db.log(
        user.username(),
        "导出",
        "导出催款记录",
        &format!("{} {} 行", period.label(), rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response(
        &format!("dunning_{}_{}.csv", kind, period.ymm()),
        rows,
    ))
}

/// 催款单 / 对账函导出成 Excel：**带明细**。
///
/// CSV 那份（`/api/export/dunning`）只有一行汇总 —— 客户问「是哪几张单凑的 5 万」
/// 时没法回答，会计只能回凭证里手工抄单一号。这里出两个 Sheet：汇总 + 逐单明细。
///
/// 明细里带「方向」列：预收/预付是我方欠对方的钱，**不计入**应收合计。
/// 没有这一列的话，客户预存 3 万会被算成欠 3 万（v1.8.36 之前的真实 bug）。
pub(super) async fn export_dunning_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    let kind = q
        .get("kind")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("ar");
    if kind != "ar" && kind != "ap" {
        return Err(AppError::bad_request("kind 必须是 ar 或 ap"));
    }
    let all = findb::settle::dunning_list(db, Some(kind), None)?;
    let list: Vec<_> = all
        .into_iter()
        .filter(|d| d.period.ymm() == period.ymm())
        .collect();
    let type_label = if kind == "ar" {
        "催款单"
    } else {
        "对账函"
    };
    let head = if kind == "ar" {
        "应收金额"
    } else {
        "应付金额"
    };
    let owed_head = if kind == "ar" { "应收" } else { "应付" };
    let adv_head = if kind == "ar" { "预收" } else { "预付" };

    let mut summary_rows: Vec<Vec<String>> = Vec::new();
    let mut detail_rows: Vec<Vec<String>> = Vec::new();
    let mut mean: Money = Money::ZERO;
    for d in &list {
        mean = mean + d.owed_total();
        summary_rows.push(vec![
            d.no.clone(),
            d.date.format("%Y-%m-%d").to_string(),
            type_label.to_string(),
            d.account.clone(),
            d.party_code.clone(),
            d.party_name.clone(),
            d.owed_total().fmt_plain(),
            d.advance_total().fmt_plain(),
            d.item_count.to_string(),
            match d.status.as_str() {
                "draft" => "草稿",
                "sent" => "已发出",
                "settled" => "已结清",
                "cancelled" => "已作废",
                other => other,
            }
            .to_string(),
            d.memo.clone(),
        ]);
        for it in &d.detail {
            detail_rows.push(vec![
                d.no.clone(),
                d.party_code.clone(),
                d.party_name.clone(),
                it.doc_no.clone(),
                it.date.clone(),
                it.summary.clone(),
                it.amount.fmt_plain(),
                if findb::settle::dunning_item_is_owed(&it.direction) {
                    owed_head.to_string()
                } else {
                    adv_head.to_string()
                },
            ]);
        }
    }

    let mut cols = vec![crate::xlsxout::ColFmt::Text; 6];
    cols.push(crate::xlsxout::ColFmt::Money);
    cols.push(crate::xlsxout::ColFmt::Money);
    cols.push(crate::xlsxout::ColFmt::Int);
    cols.push(crate::xlsxout::ColFmt::Text);
    cols.push(crate::xlsxout::ColFmt::Text);
    let sheet = crate::xlsxout::Sheet::titled(
        type_label,
        &format!("{} {}", type_label, period.label()),
        &format!(
            "共 {} 张 · {} 合计 {}",
            list.len(),
            owed_head,
            mean.fmt_money()
        ),
        &[
            "单号",
            "日期",
            "类型",
            "科目",
            "客商编码",
            "客商名称",
            head,
            adv_head,
            "笔数",
            "状态",
            "备注",
        ],
        &cols,
    )
    .with_rows(summary_rows);

    let dcols = vec![
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Money,
        crate::xlsxout::ColFmt::Text,
    ];
    let dsheet = crate::xlsxout::Sheet::titled(
        "明细",
        &format!("{}明细 {}", type_label, period.label()),
        "方向为「预收/预付」的行是我方欠对方的钱，不计入合计",
        &[
            "单号",
            "客商编码",
            "客商名称",
            "单据号",
            "日期",
            "摘要",
            "金额",
            "方向",
        ],
        &dcols,
    )
    .with_rows(detail_rows.clone());

    db.log(
        user.username(),
        "导出",
        "导出催款单 Excel",
        &format!(
            "{} {} {} 张 {} 行明细",
            period.label(),
            type_label,
            list.len(),
            detail_rows.len()
        ),
    )?;
    let fname = format!("{}_{}_{}.xlsx", kind, type_label, period.ymm());
    build_xlsx(&[sheet, dsheet], &fname, "催款单")
}

// ===========================================================================
// 账套内基础资料与系统功能（对齐桌面端 finui 补齐）
// ===========================================================================

// ---------------- 会计科目 ----------------
pub(super) async fn create_account(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AccountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let mut acc = req.account;
    acc.aux = build_aux_mask(&req.aux_kinds);
    let db = &user.db;
    if accounts::get(&db, &acc.code)?.is_some() {
        return Err(AppError::bad_request("科目已存在"));
    }
    // 与桌面端同一套校验：编码必须是数字且符合级长方案、名称非空、上级存在。
    // 少了这一层，非数字编码会写进库，后续 BalanceSnapshot 的前缀聚合按字节
    // 切片时会 panic，整个账套的报表都打不开。
    accounts::chart(&db)?.validate_new(&acc).into_result()?;
    accounts::insert(&db, &acc)?;
    db.log(
        user.username(),
        "基础资料",
        "新建科目",
        &format!("{} {}", acc.code, acc.name),
    )?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn update_account(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AccountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let mut acc = req.account;
    acc.aux = build_aux_mask(&req.aux_kinds);
    let db = &user.db;
    if accounts::get(&db, &acc.code)?.is_none() {
        return Err(AppError::not_found("科目不存在"));
    }
    accounts::chart(&db)?.validate_edit(&acc).into_result()?;
    accounts::update(&db, &acc)?;
    db.log(
        user.username(),
        "基础资料",
        "修改科目",
        &format!("{} {}", acc.code, acc.name),
    )?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn delete_account(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    if accounts::get(&db, &code)?.is_none() {
        return Err(AppError::not_found("科目不存在"));
    }
    let (vouchers, entries) = accounts::usage(&db, &code)?;
    if vouchers > 0 || entries > 0 {
        return Err(AppError::bad_request("科目已被凭证使用，无法删除"));
    }
    accounts::delete(&db, &code)?;
    db.log(user.username(), "基础资料", "删除科目", &code)?;
    Ok(Json(json!({"ok": true})))
}

// ---------------- 凭证模板 ----------------
pub(super) async fn list_templates(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<template::Template>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    Ok(Json(template::list(&*db)?))
}

pub(super) async fn create_template(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(t): Json<template::Template>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let id = template::insert(&db, &t)?;
    db.log(user.username(), "凭证模板", "新建模板", &t.name)?;
    Ok(Json(json!({"id": id})))
}

/// 由模板直接生成凭证并回写 last_period（周期性模板据此推进「本期到期」判断）
pub(super) async fn generate_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<TemplateGenerateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = match req.period.filter(|ym| *ym > 0) {
        Some(ym) => period_checked(ym)?,
        None => current_period(&state, &user),
    };
    let date = req_date(&req.date, period.last_day())?;
    let word = "记";
    // 生成凭证 + 回写 last_period 必须原子；同时把“本期已生成”判断放进
    // BEGIN IMMEDIATE，避免双击/并发各生成一张。手工模板允许多次调用，
    // 只有周期性模板才按 last_period 拒绝重复。
    let tx = db.write_tx()?;
    let t = template::get(&db, id)?.ok_or_else(|| AppError::not_found("模板不存在"))?;
    if t.freq != template::Freq::Manual {
        if let Some(last) = t.last_period {
            if period.ymm() <= last.ymm() {
                return Err(AppError::bad_request(format!(
                    "模板「{}」在 {} 已生成过（上次 {}），无需重复生成",
                    t.name,
                    period.label(),
                    last.label()
                )));
            }
        }
    }
    let no = vouchers::next_no_of(&tx, period, word)?;
    let mut v = t.to_voucher(period, date, word, no as i64, user.username())?;
    let vid = vouchers::save_in(&tx, &mut v)?;
    template::mark_generated_on(&tx, id, period)?;
    tx.commit().map_err(findb::DbError::from)?;
    db.log(
        user.username(),
        "凭证模板",
        "生成凭证",
        &format!("{} → 凭证 #{vid}", t.name),
    )?;
    Ok(Json(json!({ "id": vid })))
}

pub(super) async fn due_templates(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<template::Template>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = match q.get("period").and_then(|s| s.parse::<i32>().ok()) {
        Some(ym) => period_checked(ym)?,
        None => db.options().start_period,
    };
    Ok(Json(template::due_list(&db, period)?))
}

// ---------------- 辅助核算档案 ----------------
pub(super) async fn list_aux(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<AuxEntity>>, AppError> {
    // 档案列表是**只读参照**（凭证/收付款单都要选客户、供应商、职员……），
    // 任何可记账角色都需要；增删改仍由 AuxEdit 把关。
    user.require(Perm::Report)?;
    let db = &user.db;
    let kind = q
        .get("kind")
        .and_then(|s| AuxKind::from_code(s))
        .unwrap_or(AuxKind::Customer);
    // 默认只列启用的（凭证/收付款单选客商时不该出现停用的），但基础资料页
    // 必须能看见停用档案 —— 否则「停用」在网页端等于**单向操作**：停掉之后
    // 再也找不到它，也就没法恢复。
    let include_disabled = matches!(
        q.get("include_disabled").map(|s| s.as_str()),
        Some("1") | Some("true")
    );
    Ok(Json(auxs::list(
        &db,
        &AuxQuery {
            kind: Some(kind),
            include_disabled,
            ..Default::default()
        },
    )?))
}

pub(super) async fn create_aux(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(e): Json<AuxEntity>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    let id = auxs::insert(&db, &e)?;
    db.log(
        user.username(),
        "档案",
        "新建档案",
        &format!("{} {}", e.kind.label(), e.name),
    )?;
    Ok(Json(json!({"id": id})))
}

/// 该类别可预置的常用档案清单（只读，不落库）
///
/// 前端据此决定要不要显示「预置常用档案」按钮、以及把条目名列在按钮旁边 ——
/// 清单只有 fincore 一份（`AuxPreset`），前端不自己硬编码，改一处就够。
pub(super) async fn get_aux_presets(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<KindQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _ = (state, &user);
    let kind = AuxKind::from_code(&q.kind).unwrap_or(AuxKind::Dept);
    let items: Vec<serde_json::Value> = fincore::auxiliary::preset_aux(kind)
        .iter()
        .map(|p| json!({"code": p.code, "name": p.name, "memo": p.memo}))
        .collect();
    Ok(Json(json!({
        "kind": kind,
        "label": kind.label(),
        "items": items,
        "available": !items.is_empty(),
    })))
}

/// 一键预置常用档案（跳过已存在，不覆盖）
pub(super) async fn preset_aux(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(q): Json<KindReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    let (n, skipped) = auxs::insert_presets(&db, q.kind)?;
    db.log(
        user.username(),
        "档案",
        &format!("预置{}常用档案", q.kind.label()),
        &format!("{n} 条"),
    )?;
    Ok(Json(json!({"inserted": n, "skipped": skipped})))
}

pub(super) async fn update_aux(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(e): Json<AuxEntity>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    let mut e = e;
    e.id = id;
    auxs::update(&db, &e)?;
    db.log(user.username(), "档案", "修改档案", &e.name)?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn delete_aux(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    auxs::delete(&db, id)?;
    db.log(user.username(), "档案", "删除档案", &id.to_string())?;
    Ok(Json(json!({"ok": true})))
}

/// 辅助核算档案导出（客户/供应商/部门/职员/项目/存货/现金流量/银行账户）
///
/// `kind` 省略 = 全部维度导在一张表里。两种用法都有意义：按维度导出便于
/// 单独维护某类档案，全量导出用于整套搬运（导回时 `import_aux` 会按类型列
/// 分派，不要求一张表里只有一种）。
pub(super) async fn export_aux_xlsx(
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let kind = match q.get("kind").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => Some(
            AuxKind::from_code(s)
                .ok_or_else(|| AppError::bad_request(format!("未知的档案类型：{s}")))?,
        ),
        None => None,
    };
    let rows = auxs::list(
        &user.db,
        &AuxQuery {
            kind,
            include_disabled: true,
            ..Default::default()
        },
    )?;
    // 停用档案也导出：导出的目的是搬账套/留底，漏掉停用的会让历史凭证的
    // 辅助核算在目标账套里指向不存在的档案（余额表拆不出那一行）。
    let label = kind.map(|k| k.label()).unwrap_or("全部");
    let sheet = crate::sheets::aux_sheet(&rows, label);
    let n = rows.len();
    user.db.log(
        user.username(),
        "导出",
        "导出基础档案",
        &format!("{label} {n} 条"),
    )?;
    build_xlsx(
        &[sheet],
        &format!("基础档案_{}.xlsx", kind.map(|k| k.code()).unwrap_or("all")),
        "基础档案",
    )
}

/// 存货档案导出（含保质期天与安全库存）
pub(super) async fn export_items_xlsx(user: CurrentUser) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let rows = auxs::list(
        &user.db,
        &AuxQuery {
            kind: Some(AuxKind::Item),
            include_disabled: true,
            ..Default::default()
        },
    )?;
    // 安全库存单独存在 item_plan 表里，一次全取再做内存查表
    let plans: HashMap<String, Money> = advanced::item_plan_all(&user.db)?
        .into_iter()
        .map(|p| (p.item_code, p.safety_stock))
        .collect();
    let sheet = crate::sheets::items_sheet(&rows, &|code| plans.get(code).copied());
    let n = rows.len();
    user.db
        .log(user.username(), "导出", "导出存货档案", &format!("{n} 条"))?;
    build_xlsx(&[sheet], "存货档案.xlsx", "存货档案")
}

/// 会计科目导出（停用的不导：导入器不接受 disabled 列，
/// 把停用科目混进去会在目标账套里变成启用状态，反而更错）
pub(super) async fn export_accounts_xlsx(user: CurrentUser) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let chart = accounts::chart(&user.db)?;
    let all: Vec<&Account> = chart
        .all()
        .into_iter()
        .filter(|a| user.user.can_see_account(&a.code))
        .collect();
    let sheet = crate::sheets::accounts_sheet(&all);
    let n = all.len();
    user.db
        .log(user.username(), "导出", "导出会计科目", &format!("{n} 条"))?;
    build_xlsx(&[sheet], "会计科目.xlsx", "会计科目")
}

/// 期初余额导出（列头与通用导入模板一致，可直接改完导回）
pub(super) async fn export_begin_xlsx(user: CurrentUser) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let chart = accounts::chart(&user.db)?;
    let rows: Vec<BeginRow> = balances::list_begin(&user.db)?
        .into_iter()
        .filter(|r| user.user.can_see_account(&r.account_code))
        .collect();
    let sheet = crate::sheets::begin_sheet(&rows, &|code| {
        chart.get(code).map(|a| a.name.clone()).unwrap_or_default()
    });
    let n = rows.len();
    user.db
        .log(user.username(), "导出", "导出期初余额", &format!("{n} 行"))?;
    build_xlsx(&[sheet], "期初余额.xlsx", "期初余额")
}

// ---- 导出计划任务（每日定时 CSV） ----

/// 计划任务列表
pub(super) async fn list_export_schedules(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::exports::sched_list(&*db)? })))
}

/// 新增/修改计划任务
pub(super) async fn save_export_schedule(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ExportScheduleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let s = findb::exports::ExportSchedule {
        id: req.id,
        kind: req.kind.trim().to_string(),
        period_mode: req.period_mode.trim().to_string(),
        at_time: if req.at_time.trim().is_empty() {
            "08:00".to_string()
        } else {
            req.at_time.trim().to_string()
        },
        enabled: req.enabled.unwrap_or(true),
        last_run: String::new(),
        memo: req.memo,
        created_by: String::new(),
        created_at: String::new(),
    };
    let id = findb::exports::sched_save(&db, &s, user.username())?;
    db.log(
        user.username(),
        "导出",
        "计划任务",
        &format!("#{id} {} {}", s.kind, s.at_time),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// 删除计划任务
pub(super) async fn delete_export_schedule(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    findb::exports::sched_delete(&db, id)?;
    db.log(user.username(), "导出", "删除计划任务", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 解析并返回预览。不写库 —— 前端把预览给用户看，确认后再调 PUT 导入。
///
/// 解析逻辑复用 `fincore::batch::auxiliary`，与桌面端**同一份代码**：两端解析结果
/// 必须一致，否则用户会发现「桌面上能粘的，网页上粘不了」。
pub(super) async fn parse_aux_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AuxBatchParseReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    // 库里已有的编码 —— 用来标「这个编码已经有了」，避免用户以为导进去了
    let existing = auxs::codes(&db, req.kind)?;
    let parsed = fincore::batch::auxiliary::parse(&req.text, req.kind, &existing);
    let rows: Vec<AuxBatchRowView> = parsed
        .rows
        .iter()
        .map(|r| AuxBatchRowView {
            code: r.code.clone(),
            name: r.name.clone(),
            skip: r.skip.as_ref().map(|s| s.label().to_string()),
            parent_code: r.parent_code.clone(),
            memo: r.memo.clone(),
            disabled: r.disabled,
        })
        .collect();
    let importable = parsed.importable().len();
    Ok(Json(json!({
        "rows": rows,
        "total": rows.len(),
        "importable": importable,
        "skipped": rows.len() - importable,
        "empty": parsed.is_empty(),
    })))
}

/// 导入前端确认过的那些行
pub(super) async fn import_aux_batch(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AuxBatchImportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    // 再查一遍已存在的编码：解析到导入之间可能有人新增了档案。
    // insert_many_skip_existing 本身也会跳过，但那份结果只给「跳过了哪些」，
    // 不给「有多少行本该导入却没进」—— 前端就报不出「你以为导了 10 条，
    // 实际只进了 8 条」。这里自己先筛，好把差额说清楚。
    //
    // ⚠️ 两个集合必须分开：`in_book` 是「真的没进去」，`seen` 是「本批已经处理过
    // 这个编码，用来防重复」。第一版只用一个数组，装成功后被当成 skipped 报出去，
    // 用户看到「已导入 2 条，跳过 2 条」—— 2 + 2 ≠ 2，数字对不上，
    // 第一反应是「这软件出 bug 了」。
    let existing = auxs::codes(&db, req.kind)?;
    let mut items: Vec<AuxEntity> = Vec::new();
    let mut in_book: Vec<String> = Vec::new();
    let mut dup_in_batch: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for r in &req.rows {
        let code = r.code.trim().to_string();
        let name = r.name.trim().to_string();
        if code.is_empty() || name.is_empty() {
            continue;
        }
        if seen.contains(&code) {
            dup_in_batch.push(code);
            continue;
        }
        seen.push(code.clone());
        if existing.contains(&code) {
            in_book.push(code);
            continue;
        }
        let mut e = AuxEntity::new(req.kind, code, name);
        e.parent_code = r
            .parent_code
            .as_ref()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        e.memo = r.memo.trim().to_string();
        e.disabled = r.disabled;
        items.push(e);
    }
    if items.is_empty() {
        return Ok(Json(json!({
            "inserted": 0,
            "skipped": in_book.len() + dup_in_batch.len(),
            "already": in_book,
            "duplicate_in_batch": dup_in_batch,
        })));
    }
    let (n, skipped) = auxs::insert_many_skip_existing(&db, &items)?;
    db.log(
        user.username(),
        "档案",
        &format!("批量导入{}档案", req.kind.label()),
        &format!("{n} 条"),
    )?;
    let mut all_skipped = in_book;
    all_skipped.extend(skipped);
    Ok(Json(json!({
        "inserted": n,
        "skipped": all_skipped.len() + dup_in_batch.len(),
        "already": all_skipped,
        "duplicate_in_batch": dup_in_batch,
    })))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(Deserialize)]
pub(super) struct ExportScheduleReq {
    #[serde(default)]
    id: i64,
    kind: String,
    #[serde(default)]
    period_mode: String,
    #[serde(default)]
    at_time: String,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    memo: String,
}

#[derive(Deserialize)]
pub(super) struct AccountReq {
    account: Account,
    #[serde(default)]
    aux_kinds: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct TemplateGenerateReq {
    /// 期间 ymm，如 202601，缺省当前期间
    #[serde(default)]
    period: Option<i32>,
    /// 凭证日期，缺省期间末日
    #[serde(default)]
    date: String,
}

#[derive(serde::Deserialize)]
pub(super) struct KindReq {
    kind: AuxKind,
}

#[derive(Deserialize)]
pub(super) struct AuxBatchParseReq {
    kind: AuxKind,
    text: String,
}

#[derive(Deserialize)]
pub(super) struct AuxBatchImportReq {
    kind: AuxKind,
    /// 只导入前端回传的这些行。**不接受「重新解析一遍文本」**：
    /// 用户在预览之后可能改过文本（比如去掉一行），若后端重新解析，
    /// 实际导入的就不是他看到并确认过的那批 —— 正是「确认了 A 写进去 B」
    /// 这类最难查的错。
    rows: Vec<AuxBatchRow>,
}

#[derive(serde::Serialize)]
pub(super) struct AuxBatchRowView {
    code: String,
    name: String,
    /// None = 可导入；Some = 跳过原因（中文文案）
    skip: Option<String>,
    parent_code: Option<String>,
    memo: String,
    disabled: bool,
}
