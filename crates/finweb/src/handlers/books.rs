//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

/// 深度探针（readiness）：真正去碰依赖，回答「数据层还能用吗」
///
/// ## 与 `/api/health` 的分工
///
/// - `/api/health` = **liveness**：进程还在收请求吗。**不碰任何依赖**，
///   所以数据库慢不会让它失败。
/// - `/api/health/ready` = **readiness**：账号库 / 账套库 / 数据目录还能读写吗。
///
/// 这个区分是硬要求，不是洁癖：liveness 一旦碰 DB，数据库变慢就会把容器判成
/// unhealthy，编排器随即重启应用 —— 于是「数据库一慢就重启应用，重启又让
/// 数据库更慢」，把一次本可自愈的抖动放大成宕机。负载均衡与监控该用 readiness。
///
/// ## 为什么原探针不够用
///
/// 原 `/api/health` 返回常量 `ok`，不碰 DB。于是下面这种最常见的故障它一律
/// 报「健康」：磁盘写满 → SQLite 写失败；账套文件损坏 → 打开即报错；
/// 账号库被锁 → 所有登录 500。进程活着、端口通，但系统已经不能用了。
/// healthcheck.sh 拿着这样的答案会一直显示「一切正常」—— 虚假的安全感比
/// 没有监控更危险，因为它让人不去查别的地方。
///
/// ## 探针自身必须廉价且无副作用
///
/// - 账套用 `open_no_migrate` 打开：只设连接级 PRAGMA，**不写库**。
///   探针绝不能触发迁移 —— 迁移是写事务，5 分钟一次的探针去抢写锁，
///   会在业务正忙时制造额外的锁竞争。
/// - 目录可写性用「建临时文件 → 立即删」，文件名不以 `.fbk` 结尾，
///   不会被账套扫描当成新账套。
pub(super) async fn health_ready(
    State(state): State<Arc<WebState>>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let mut checks = serde_json::Map::new();
    let mut ok = true;

    // ① 账号库：真发一条 SQL（realm.db 损坏 / 被锁 / 权限不对都会在这里暴露）
    match state.realm.count_users() {
        Ok(n) => {
            checks.insert("realm".into(), json!({ "ok": true, "users": n }));
        }
        Err(e) => {
            ok = false;
            checks.insert(
                "realm".into(),
                json!({ "ok": false, "error": e.to_string() }),
            );
        }
    }

    // ② 账套目录：能列出来，且至少一个账套能真的读
    let mut book_files: Vec<std::path::PathBuf> = std::fs::read_dir(&state.books_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "fbk").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    book_files.sort();
    checks.insert("books_count".into(), json!(book_files.len()));

    // ③ 抽一个账套做只读查询 —— 文件损坏 / 权限不对 / 磁盘 I/O 错误都在这里现形
    if let Some(p) = book_files.first() {
        match findb::Db::open_no_migrate(p).and_then(|db| {
            // 走一次真实查询：Connection::open 是惰性的，坏文件也要 SELECT 才报错
            db.conn()
                .query_row("SELECT COUNT(*) FROM voucher", [], |r| r.get::<_, i64>(0))
                .map_err(Into::into)
        }) {
            Ok(_) => {
                checks.insert("book_sample".into(), json!({ "ok": true }));
            }
            Err(e) => {
                ok = false;
                checks.insert(
                    "book_sample".into(),
                    json!({
                        "ok": false,
                        "file": p.file_name().unwrap_or_default().to_string_lossy(),
                        "error": e.to_string(),
                    }),
                );
            }
        }
    }

    // ④ 数据目录可写：磁盘满 / 只读挂载 / 属主不对时，服务看着健康却录不了凭证。
    //    healthcheck.sh 只能查宿主机视角的权限，这里查进程自己的真实能力。
    let probe = state.books_dir.join(".finbook-write-probe");
    let writable = std::fs::write(&probe, b"ok").is_ok() && std::fs::remove_file(&probe).is_ok();
    if !writable {
        ok = false;
    }
    checks.insert(
        "books_dir_writable".into(),
        json!({ "ok": writable, "path": state.books_dir.display().to_string() }),
    );

    // ⑤ schema 迁移：升级后所有账套是否都已迁到当前版本。
    //
    // 这一项是为了消灭「探针报绿、账套还没升级」这种状态。迁移原本是懒执行
    // 的（账套第一次被打开时才跑），于是部署完成、readiness 全绿，而用户第一次
    // 点某个功能才报 `no such column: xxx` —— 生产上真实发生过。
    //
    // 判定：
    // - 仍在进行 → 未就绪（503）：实例还没真正可用
    // - 有账套迁不动 → 未就绪（503），并**指名道姓**报出是哪个、报什么。
    //   财务系统里一个账套打不开不是「可以先用着」，必须让人看见。
    let mig = state.books.migration_status();
    // 权威判据是「有没有账套还没迁过」，而不是「批量任务跑过没有」——
    // 前者能抓到运行期新增/恢复的账套，后者只看启动那一刻的快照。
    let pending = state.books.unmigrated_books();
    if !mig.done() || mig.has_failures() || !pending.is_empty() {
        ok = false;
    }
    checks.insert(
        "migration".into(),
        json!({
            "ok": mig.done() && !mig.has_failures() && pending.is_empty(),
            "done": mig.done(),
            "running": mig.running,
            "total": mig.total,
            "migrated": mig.ok,
            "pending": pending,
            "failed": mig.failed
                .iter()
                .map(|(k, e)| json!({ "book": k, "error": e }))
                .collect::<Vec<_>>(),
            "schema_version": findb::schema::SCHEMA_VERSION,
        }),
    );

    let body = json!({ "ok": ok, "checks": checks });
    // 有依赖不可用时返回 503：编排器据此把实例摘出负载均衡，
    // 但**不**触发重启（那是 liveness 的职责）。返回 200 会让负载均衡
    // 继续往一个明确不能用的实例上打流量。
    Ok((
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(body),
    ))
}

/// 账套列表（需登录）：管理员返回全部，普通用户返回自己创建的 + 被邀请加入的
/// 同时返回平台身份摘要（前端在"已登录未选账套"状态下据此渲染选择页）
pub(super) async fn list_books(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let books = visible_books(&state, &user.username, user.is_admin)?;
    let items: Vec<serde_json::Value> = books
        .iter()
        .map(|b| json!({ "key": b.key, "company": b.company, "owner": b.owner_username }))
        .collect();
    Ok(Json(json!({
        "user": {
            "username": user.username,
            "display_name": user.display_name,
            "is_admin": user.is_admin,
            "must_change_pwd": user.must_change_pwd,
        },
        "books": items,
    })))
}

pub(super) async fn consolidate_preview(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该入口仅限系统管理员使用"));
    }
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| Period::from_ymm(state.default_period));
    let keys: Vec<String> = q
        .get("books")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if keys.is_empty() {
        return Err(AppError::bad_request("请选择要合并的账套（books=b1,b2）"));
    }
    // 逐套取数：科目 → (名称, 期末余额)
    let mut per_book: Vec<(
        String,
        String,
        std::collections::BTreeMap<String, (String, Money)>,
    )> = Vec::new();
    let mut book_meta = Vec::new();
    for key in &keys {
        let db = state
            .db_for(key)
            .map_err(|_| AppError::bad_request(format!("账套 {key} 不存在或无法打开")))?;
        let company = db.options().company.clone();
        let chart = accounts::chart(&db)?;
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(period))?;
        let mut map: std::collections::BTreeMap<String, (String, Money)> =
            std::collections::BTreeMap::new();
        for a in chart.all() {
            if !chart.is_leaf(&a.code) {
                continue;
            }
            let end = snap.for_account(&a.code, None).end();
            if end.is_zero() {
                continue;
            }
            map.insert(a.code.clone(), (a.name.clone(), end));
        }
        book_meta.push(json!({ "key": key, "company": company }));
        per_book.push((key.clone(), company, map));
    }
    // 科目并集 → 行（账套列 + 合计）
    let mut codes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (_, _, m) in &per_book {
        for c in m.keys() {
            codes.insert(c.clone());
        }
    }
    let mut rows = Vec::new();
    for code in &codes {
        let mut values = serde_json::Map::new();
        let mut total = Money::ZERO;
        let mut name = String::new();
        for (key, _, m) in &per_book {
            let v = m
                .get(code)
                .map(|(n, v)| {
                    if name.is_empty() {
                        name = n.clone();
                    }
                    *v
                })
                .unwrap_or(Money::ZERO);
            values.insert(key.clone(), json!(v.fmt_money()));
            total += v;
        }
        rows.push(json!({
            "account_code": code,
            "account_name": name,
            "values": values,
            "total": total.fmt_money(),
        }));
    }
    Ok(Json(json!({
        "period": period_to_str(period),
        "books": book_meta,
        "rows": rows,
        "note": "汇总口径：各账套仅已记账余额（H-3）；v1 为跨账套汇总，内部往来抵销留待 v2",
    })))
}

/// 普通用户自建账套：创建者为该账套的所有者，并成为账套内管理员
pub(super) async fn create_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<CreateBookReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 账号模型二元化：仅管理员可新建账套（普通账号由管理员开通并邀请进入账套工作）
    if !user.is_admin {
        return Err(AppError::forbidden("仅管理员可新建账套"));
    }
    let owner = user.username.clone();
    // 限制每账号账套数量，防止无限建账占满磁盘
    let owned = state.realm.count_books_of(&owner)?;
    if owned >= MAX_BOOKS_PER_USER {
        return Err(AppError::bad_request(format!(
            "每个账号最多创建 {MAX_BOOKS_PER_USER} 个账套（当前已创建 {owned} 个）"
        )));
    }
    let company = req.company.trim().to_string();
    let key = make_book_key(&req.key, &owner, &state.realm)?;
    let path = state.books_dir.join(format!("{key}.fbk"));
    if path.exists() {
        return Err(AppError::bad_request("账套文件已存在"));
    }
    let mut opts = BookOptions::default();
    if !company.is_empty() {
        opts.company = company.clone();
    }
    if req.start_period > 0 {
        opts.start_period = period_checked(req.start_period)?;
    }
    let db = Db::create_no_admin(&path, &opts)?;
    // 种子所有者为账套内管理员（复制平台口令哈希，便于必要时直接登账套）
    let mut u = User::new(&owner, &user.display_name, Role::Admin);
    if let Ok(Some(ru)) = state.realm.get_user(&owner) {
        u.password_hash = ru.password_hash;
    }
    users::insert(&db, &u)?;
    drop(db);
    // 注册进运行期账套表 + 平台账套目录
    state.books.register(&path, 16);
    state
        .realm
        .register_book(&key, &path.to_string_lossy(), &owner, &company)?;
    Ok(Json(json!({ "key": key, "company": company })))
}

/// 选择当前账套（登录后进入某套账前调用）
pub(super) async fn select_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let book = state
        .realm
        .get_book(&key)?
        .ok_or_else(|| AppError::not_found("账套不存在"))?;
    // 授权三层：管理员 / 归属者 / 账套内已有该用户的行（被邀请成员）。
    // 注意：账套打不开（文件损坏等）必须透出 500，不能吞成"无权访问"误导用户。
    let is_member = match state.db_for(&key) {
        Ok(db) => users::get(&db, &user.username).ok().flatten().is_some(),
        Err(e) => return Err(e.into()),
    };
    let allowed = user.is_admin || book.owner_username == user.username || is_member;
    if !allowed {
        return Err(AppError::forbidden("无权访问该账套"));
    }
    // 进入账套时把会话当前期间同步为该账套的**当前期间**（最后一个已结账
    // 期间的下一期），不是启用期间。桌面版就是这么做的
    // （crates/finui/src/state.rs 的 `periods::current_period`）。
    //
    // 曾经这里取值是 `db.options().start_period`，后果是：账套结账推到 2026-10
    // 之后，网页一进来仍停在 2025-01，首页整块「本期经营结果」「钱在哪」
    // 显示的是建账首月的数，且不会自己动——老板以为系统算错了。
    if let Ok(db) = state.db_for(&key) {
        let ymm = periods::current_period(&db)
            .unwrap_or_else(|_| db.options().start_period)
            .ymm();
        state.sessions.set_period(&user.token, ymm);
        drop(db);
    }
    state.sessions.set_book_key(&user.token, &key);
    Ok(Json(json!({ "ok": true })))
}

/// 删除账套（管理员 或 账套归属者）：解除「有账套的用户无法删除」的死锁
pub(super) async fn delete_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let book = state
        .realm
        .get_book(&key)?
        .ok_or_else(|| AppError::not_found("账套不存在"))?;
    if !user.is_admin && book.owner_username != user.username {
        return Err(AppError::forbidden("无权删除该账套"));
    }
    // 删除前先记下本套引用的大附件；共享目录按内容寻址，不能整目录删。
    let own_attach = state
        .db_for(&key)
        .ok()
        .and_then(|db| book_attachment_paths(&db).ok())
        .unwrap_or_default();
    // 顺序要点：先摘运行期注册（否则后续请求会把已删文件重新打开成空库），
    // 再清会话、删目录记录，最后落盘删除文件。
    state.books.unregister(&key);
    state.sessions.clear_book_key(&key);
    state.realm.delete_book(&key)?;
    let p = std::path::PathBuf::from(&book.path);
    if p.exists() {
        if let Err(e) = std::fs::remove_file(&p) {
            error!(path = %p.display(), error = %e, "删除账套文件失败");
        }
        let _ = std::fs::remove_file(format!("{}-wal", p.display()));
        let _ = std::fs::remove_file(format!("{}-shm", p.display()));
    }

    // 删除该账套的手工/自动备份：否则同名 key 被重新创建后，新租户能在
    // 备份列表里看到并恢复上一个租户的数据。
    let backup_dir = state.books_dir.join("backups");
    if let Ok(entries) = std::fs::read_dir(&backup_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if backup_belongs_to(&name, &key) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    // 清理仅属于该账套的大附件。附件是跨账套共享的内容寻址目录，
    // 必须确认其他账套没引用同一个文件；有任何一个账套扫不开就整体放弃，
    // 宁可留下孤儿文件，也不能删掉别的租户还在用的附件。
    if !own_attach.is_empty() {
        let mut used_elsewhere = std::collections::HashSet::new();
        let mut scan_complete = true;
        match std::fs::read_dir(&state.books_dir) {
            Ok(entries) => {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) != Some("fbk") {
                        continue;
                    }
                    if p.file_stem().and_then(|s| s.to_str()) == Some(key.as_str()) {
                        continue;
                    }
                    let conn = match rusqlite::Connection::open_with_flags(
                        &p,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    ) {
                        Ok(c) => c,
                        Err(_) => {
                            scan_complete = false;
                            break;
                        }
                    };
                    let mut stmt = match conn.prepare(
                        "SELECT DISTINCT path FROM attachment \
                         WHERE inline=0 AND path IS NOT NULL AND path <> ''",
                    ) {
                        Ok(s) => s,
                        Err(_) => {
                            scan_complete = false;
                            break;
                        }
                    };
                    let rows = match stmt.query_map([], |r| r.get::<_, String>(0)) {
                        Ok(r) => r,
                        Err(_) => {
                            scan_complete = false;
                            break;
                        }
                    };
                    for row in rows.flatten() {
                        used_elsewhere.insert(row);
                    }
                }
            }
            Err(_) => scan_complete = false,
        }
        if scan_complete {
            let attach_dir = state.books_dir.join(".attachments");
            for rel in own_attach {
                if rel.contains("..") || rel.contains('/') || rel.contains('\\') {
                    continue;
                }
                if !used_elsewhere.contains(&rel) {
                    let _ = std::fs::remove_file(attach_dir.join(&rel));
                }
            }
        }
    }
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// 账套参数 / 仪表盘 / 期间
// ---------------------------------------------------------------------------

pub(super) async fn get_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<fincore::BookOptions>, AppError> {
    let db = &user.db;
    // 读参数放宽到「能记账的人」，写仍然要 SysOption。
    //
    // 为什么：前端的凭证编辑器要用 `enable_audit` 决定「记账」按钮的行为，
    // 而 `afterLogin()` 也要读参数。原来会计/出纳读参数一律 403，于是
    // `state.bookOptions = null`、界面按「未开审核」处理 —— 用户完全看不到
    // 「本账套开了审核环节」这个事实，只在点记账被拒时才第一次知道。
    // 让人能读自己每天都要面对的那个开关，成本几乎为零。
    if !user.can(Perm::SysOption) && !(user.can(Perm::VoucherPost) || user.can(Perm::VoucherAudit))
    {
        return Err(AppError::forbidden("没有「账套参数」权限"));
    }
    Ok(Json(db.options()))
}

pub(super) async fn put_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(opts): Json<fincore::BookOptions>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    // 落库前校验：非法启用期间会灌进会话并污染年度累计口径，后续 first_day/last_day panic
    period_checked(opts.start_period.ymm())?;
    // 字段校验统一走 `BookOptions::validate()`。
    //
    // 原来这里手写了一遍「级长 / 凭证字号」检查，桌面端参数页又只校了
    // 「期间能解析」「级长能解析成数字」—— 同一个字段两端规则不同，这类分叉
    // 最后都表现成「网页存得进、桌面存不进」或者反过来，使用者只会觉得
    // 「保存按钮有时候不灵」。校验留在 fincore，两端共用。
    let problems = opts.validate();
    if !problems.is_empty() {
        return Err(AppError::bad_request(problems.join("；")));
    }
    // set_options 内部还会补一道「业务默认科目必须存在且是末级」的查库校验
    db.set_options(&opts)?;
    Ok(Json(json!({"ok": true})))
}

/// 导出账套参数成 JSON 文件
///
/// 为什么不是「页面上的复制按钮」：换台机器要把它放到指定位置，一个下载文件比
/// 一段要手动粘进编辑器的文本可靠。整个 `BookOptions` 原样出去，没有删减——
/// 少导出一个字段，导入后就会静默回到默认值。
pub(super) async fn export_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let opts = db.options();
    let body = serde_json::to_vec_pretty(&opts)
        .map_err(|e| AppError::Internal(format!("序列化账套参数失败：{e}")))?;
    let name = format!(
        "{}_账套参数_{}.json",
        if opts.company.trim().is_empty() {
            "账套"
        } else {
            opts.company.trim()
        },
        chrono::Local::now().format("%Y%m%d")
    );
    let mut resp =
        Response::builder().header(header::CONTENT_TYPE, "application/json; charset=utf-8");
    if let Some(hv) = attachment_header(&name) {
        resp = resp.header(header::CONTENT_DISPOSITION, hv);
    }
    resp.body(Body::from(body))
        .map_err(|e| AppError::Internal(format!("生成下载响应失败：{e}")))
}

/// 导入账套参数
///
/// 结构项（编码级长 / 启用期间）与当前账套不同就整体拒绝，一个字段都不改 ——
/// 见 `BookOptions::adopt_config`。
pub(super) async fn import_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(incoming): Json<fincore::BookOptions>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let mut opts = db.options();
    if let Err(problems) = opts.adopt_config(&incoming) {
        return Err(AppError::bad_request(problems.join("；")));
    }
    period_checked(opts.start_period.ymm())?;
    db.set_options(&opts)?;
    Ok(Json(json!({"ok": true, "options": opts})))
}

/// 恢复默认（只清偏好类设置）
///
/// 企业名称、编码级长、启用期间、默认科目、审核/出纳开关都不动 ——
/// 它们与已经记过的账绑在一起，见 `BookOptions::reset_preferences`。
pub(super) async fn reset_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let mut opts = db.options();
    opts.reset_preferences();
    db.set_options(&opts)?;
    Ok(Json(json!({"ok": true, "options": opts})))
}

/// 单据链：`?kind=req|po|so&id=` —— 上游源单（doc_link）+ 下游/执行单据（到货付款/发货收款）
pub(super) async fn get_doc_links(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let kind = q.get("kind").cloned().unwrap_or_default();
    if !matches!(kind.as_str(), "req" | "po" | "so" | "invoice" | "receipt") {
        return Err(AppError::bad_request(
            "kind 只能是 req / po / so / invoice / receipt",
        ));
    }
    let id: i64 = q
        .get("id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::bad_request("缺少 id"))?;
    let rows = findb::docflow::doc_chain(&db, &kind, id)?;
    Ok(Json(json!({ "rows": rows })))
}

// ---------------- 操作日志 ----------------
pub(super) async fn list_logs(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::user::AuditLog>>, AppError> {
    user.require(Perm::AuditLog)?;
    let db = &user.db;
    let limit: i64 = q
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let logs = match q.get("q") {
        Some(kw) if !kw.trim().is_empty() => db.search_logs(kw.trim(), limit)?,
        _ => db.recent_logs(limit)?,
    };
    Ok(Json(logs))
}

pub(super) async fn list_backups(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Backup)?;
    let dir = state.books_dir.join("backups");
    // 备份目录是全局共享的，只能列出属于当前账套的备份。
    //
    // 不能用 `name.ends_with("_{key}.fbk")`：key 允许下划线，`alice_202610` 的
    // 备份会被 key=`202610` 的账套命中（后缀碰撞 → 跨租户读/恢复）。这里按
    // 固定时间戳前缀解析出真正的 key，再逐字符比较。
    let mut items = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("fbk") {
                let name = e.file_name().to_string_lossy().to_string();
                if !backup_belongs_to(&name, &user.book_key) {
                    continue;
                }
                if let Ok(meta) = std::fs::metadata(&p) {
                    items.push(json!({
                        "name": name,
                        "size": meta.len(),
                        "mtime": meta.modified().map(|t| format!("{:?}", t)).unwrap_or_default(),
                    }));
                }
            }
        }
    }
    items.sort_by(|a, b| b["name"].as_str().cmp(&a["name"].as_str()));
    Ok(Json(json!({ "items": items })))
}

pub(super) async fn create_backup(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Backup)?;
    let key = user.book_key.clone();
    if !state.books_dir.join(format!("{key}.fbk")).exists() {
        return Err(AppError::not_found("账套文件不存在"));
    }
    let state2 = state.clone();
    let username = user.username().to_string();
    // checkpoint + fs::copy 都是阻塞 IO，放阻塞池，别占死 async worker
    let name = tokio::task::spawn_blocking(move || -> Result<String, AppError> {
        let src = state2.books_dir.join(format!("{key}.fbk"));
        let dir = state2.books_dir.join("backups");
        let _ = std::fs::create_dir_all(&dir);
        // 先 checkpoint 把 WAL 合并回主文件，再复制主文件即可得到完整一致快照
        let db = state2.db_for(&key)?;
        checkpoint_wal(&db)?;
        drop(db);
        let stamp = backup_stamp();
        let dst_name = format!("{}_{}.fbk", stamp, key);
        std::fs::copy(&src, dir.join(&dst_name))?;
        let db = state2.db_for(&key)?;
        db.log(&username, "系统", "备份账套", &format!("备份 {dst_name}"))?;
        Ok(dst_name)
    })
    .await
    .map_err(|e| AppError::Internal(format!("备份任务失败：{e}")))??;
    Ok(Json(json!({"ok": true, "name": name})))
}

pub(super) async fn restore_backup(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BackupRestoreReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Backup)?;
    // 路径安全：仅允许 backups 目录下的纯文件名，禁止任何路径穿越；
    // 且只接受属于当前账套的备份（按固定时间戳前缀解析 key，见 backup_belongs_to），
    // 防止跨租户恢复。
    let file_name = match std::path::Path::new(&req.file)
        .file_name()
        .and_then(|s| s.to_str())
    {
        Some(n) if !n.contains("..") && !n.contains('/') && !n.contains('\\') => n.to_string(),
        _ => return Err(AppError::bad_request("非法的备份文件名")),
    };
    if !backup_belongs_to(&file_name, &user.book_key) {
        return Err(AppError::bad_request("该备份不属于当前账套"));
    }
    let src = state.books_dir.join("backups").join(&file_name);
    if !src.exists() {
        return Err(AppError::not_found("备份文件不存在"));
    }
    let key = user.book_key.clone();
    let username = user.username().to_string();
    let state2 = state.clone();
    // 覆盖主库 + 删 WAL + 重注册账套都是阻塞操作，放阻塞池
    tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        // 先验证备份文件本身可读、结构完整，避免把坏文件覆盖到当前库上。
        {
            let conn = rusqlite::Connection::open_with_flags(
                &src,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|e| AppError::bad_request(format!("备份文件无法打开：{e}")))?;
            let integrity: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .map_err(|e| AppError::bad_request(format!("备份文件校验失败：{e}")))?;
            if integrity != "ok" {
                return Err(AppError::bad_request(format!(
                    "备份文件已损坏，未恢复：{integrity}"
                )));
            }
            let has_voucher: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='voucher'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| AppError::bad_request(format!("备份文件校验失败：{e}")))?;
            if has_voucher == 0 {
                return Err(AppError::bad_request("该文件不是 FinBook 账套备份"));
            }
        }

        let dst = state2.books_dir.join(format!("{key}.fbk"));
        let backups_dir = state2.books_dir.join("backups");
        std::fs::create_dir_all(&backups_dir)
            .map_err(|e| AppError::Internal(format!("创建备份目录失败：{e}")))?;
        // 恢复前先 checkpoint 并自动备份一次，避免覆盖无法回退。
        // 自动备份失败必须中止 —— 否则用户以为有退路，实际没有。
        let db = state2.db_for(&key)?;
        checkpoint_wal(&db)?;
        drop(db);
        let stamp = backup_stamp();
        let auto_name = format!("auto_{}_{}.fbk", stamp, key);
        let auto_path = backups_dir.join(&auto_name);
        std::fs::copy(&dst, &auto_path).map_err(|e| {
            AppError::Internal(format!(
                "恢复前自动备份失败，已中止（当前账套未被改动）：{e}"
            ))
        })?;

        // 先复制到临时文件，成功后再替换主库：copy 中途失败不会留下半个 .fbk。
        let tmp = state2.books_dir.join(format!("{key}.fbk.restore.tmp"));
        std::fs::copy(&src, &tmp)
            .map_err(|e| AppError::Internal(format!("写入恢复临时文件失败：{e}")))?;
        state2.books.unregister(&key);
        for ext in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(state2.books_dir.join(format!("{key}.fbk{ext}")));
        }
        if let Err(e) = std::fs::rename(&tmp, &dst) {
            let _ = std::fs::remove_file(&tmp);
            if let Ok(path) = std::fs::canonicalize(&dst) {
                state2.books.register(&path, 16);
            }
            return Err(AppError::Internal(format!("替换账套文件失败：{e}")));
        }
        // 重新注册账套，使后续请求以新文件重新打开
        if let Ok(path) = std::fs::canonicalize(&dst) {
            state2.books.register(&path, 16);
        }
        let db = match state2.db_for(&key) {
            Ok(db) => db,
            Err(e) => {
                // 新库打不开：用刚生成的自动备份回滚，保证用户至少回到恢复前状态。
                let rolled = (|| -> std::io::Result<()> {
                    std::fs::copy(&auto_path, &dst)?;
                    let _ = std::fs::remove_file(state2.books_dir.join(format!("{key}.fbk-wal")));
                    let _ = std::fs::remove_file(state2.books_dir.join(format!("{key}.fbk-shm")));
                    Ok(())
                })();
                if let Ok(path) = std::fs::canonicalize(&dst) {
                    state2.books.unregister(&key);
                    state2.books.register(&path, 16);
                }
                if let Err(re) = rolled {
                    return Err(AppError::Internal(format!(
                        "恢复后账套无法打开（{e}），且回滚失败（{re}），请用 {auto_name} 手工恢复"
                    )));
                }
                return Err(AppError::Internal(format!(
                    "恢复后账套无法打开，已回滚到恢复前状态：{e}"
                )));
            }
        };
        // 账套文件已被覆盖，恢复已完成；此处日志失败不能把请求打成 500，
        // 否则客户端看到失败会重试恢复，重复覆盖刚恢复好的库。
        if let Err(e) = db.log(
            &username,
            "系统",
            "恢复账套",
            &format!("从 {file_name} 恢复"),
        ) {
            error!(file = %file_name, error = %e, "写操作日志失败（恢复账套）");
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(format!("恢复任务失败：{e}")))??;
    Ok(Json(json!({"ok": true})))
}

/// 下载账套备份包：把 .fbk 主文件与 .attachments/ 附件目录打成一个 zip，
/// 一次下载完整账套，避免只备份主库而漏掉附件。
pub(super) async fn download_backup(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    user.require(Perm::Backup)?;
    let key = user.book_key.clone();
    let src = state.books_dir.join(format!("{key}.fbk"));
    if !src.exists() {
        return Err(AppError::not_found("账套文件不存在"));
    }
    let attach_dir = src.parent().unwrap().join(".attachments");

    // checkpoint 把 WAL 合并回主文件，保证打包的 .fbk 是完整一致快照
    let db = state.db_for(&key)?;
    checkpoint_wal(&db)?;
    // 只打包**当前账套引用**的大附件。`.attachments` 是所有账套共享的
    // 内容寻址目录（sha256 前缀命名），整目录打包会把其他租户的附件一起拖走。
    // 小附件（inline=1）本来就在 .fbk 里，不需要单独打包。
    let attach_paths = book_attachment_paths(&db)?;
    drop(db);

    let stamp = backup_stamp();
    let zip_name = format!("{key}_backup_{stamp}.zip");

    let buf = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, AppError> {
        let mut buf = Vec::new();
        let mut zip = ZipWriter::new(io::Cursor::new(&mut buf));
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(CompressionMethod::Deflated);
        let zerr = |e| AppError::Internal(format!("zip: {e}"));

        // .fbk 主文件
        zip.start_file(format!("{key}.fbk"), options)
            .map_err(&zerr)?;
        let mut f = File::open(&src)?;
        io::copy(&mut f, &mut zip)?;

        // 当前账套引用的大附件（路径是 attach.rs 生成的扁平 sha 文件名）。
        for rel in attach_paths {
            if rel.contains("..") || rel.contains('/') || rel.contains('\\') {
                continue;
            }
            let p = attach_dir.join(&rel);
            if !p.is_file() {
                // 数据库有记录但磁盘文件缺失：不阻断整包下载，跳过即可。
                continue;
            }
            let name = format!(".attachments/{rel}");
            zip.start_file(name, options).map_err(&zerr)?;
            let mut f = File::open(&p)?;
            io::copy(&mut f, &mut zip)?;
        }

        zip.finish().map_err(&zerr)?;
        Ok(buf)
    })
    .await
    .map_err(|e| AppError::Internal(format!("打包备份失败：{e}")))?; // -> Result<Vec<u8>, AppError>
    let buf = buf?; // -> Vec<u8> // buf: Vec<u8>

    let body = Body::from(buf);
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{zip_name}\""),
        )
        .body(body)
        .map_err(|e| AppError::Internal(format!("构建响应失败：{e}")))?)
}

pub(super) async fn opening_sheet_template(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    user.require(Perm::Opening)?;
    let chart = findb::accounts::chart(&user.db)?;
    let accounts: Vec<&fincore::account::Account> = chart
        .all()
        .into_iter()
        .filter(|a| chart.is_leaf(&a.code))
        .collect();
    let customers = findb::auxs::list(
        &user.db,
        &AuxQuery {
            kind: Some(fincore::AuxKind::Customer),
            include_disabled: false,
            ..Default::default()
        },
    )?;
    let suppliers = findb::auxs::list(
        &user.db,
        &AuxQuery {
            kind: Some(fincore::AuxKind::Supplier),
            include_disabled: false,
            ..Default::default()
        },
    )?;
    let sheets = crate::sheets::opening_collection_template(&accounts, &customers, &suppliers);
    build_xlsx(&sheets, "期初收集表.xlsx", "期初收集表")
}

/// 期初导入后的对账视图。
///
/// 为什么不并进「预览」接口：预览回答「这份表能不能导」，对账回答「导完之后账上是不是
/// 真的变成了表里那个样子」。前者看的是文件，后者看的是库 —— 两次来的数据都可能对，
/// 但中间任何一步出问题，只有后者能发现。
pub(super) async fn opening_sheet_reconcile(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<ReconcileDto>, AppError> {
    user.require(Perm::Opening)?;
    let r = findb::opening_check::reconcile_begin(&user.db)?;
    Ok(Json(ReconcileDto {
        ok: !r.empty && r.issues.is_empty(),
        one_liner: findb::opening_check::one_liner(&r),
        empty: r.empty,
        sheets: r
            .sheets
            .into_iter()
            .map(|s| ReconcileSheetDto {
                label: s.label,
                rows: s.rows,
                total: s.total.fmt_plain(),
                hint: s.hint,
            })
            .collect(),
        debit_total: r.debit_total.fmt_plain(),
        credit_total: r.credit_total.fmt_plain(),
        balanced: r.balanced,
        asset_total: r.asset_total.fmt_plain(),
        liab_total: r.liab_total.fmt_plain(),
        equity_total: r.equity_total.fmt_plain(),
        checks: r
            .checks
            .into_iter()
            .map(|c| ReconcileCheckDto {
                name: c.name,
                ok: c.ok,
                detail: c.detail,
            })
            .collect(),
        issues: r.issues,
    }))
}

/// 只体检不落库 —— 与正式导入走同一条代码路径，只是不写。
pub(super) async fn preview_opening_sheet(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Opening)?;
    let data = sheet_upload(multipart).await?;
    let r = findb::opening_book::preview_collection_sheet(&user.db, &data)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "dry_run": true,
        "ar": r.ar.ok,
        "ap": r.ap.ok,
        "begin": r.begin_ok,
        "arap_ok": r.arap_ok,
        "debit_total": r.debit_total.fmt_plain(),
        "credit_total": r.credit_total.fmt_plain(),
        "diff": (r.debit_total - r.credit_total).fmt_plain(),
        "auto_profit": r.auto_profit.fmt_plain(),
        "warnings": r.ar.warnings.iter().chain(r.ap.warnings.iter())
            .chain(r.balances.warnings.iter()).chain(r.equity.warnings.iter())
            .cloned().collect::<Vec<_>>(),
    })))
}

pub(super) async fn import_opening_sheet(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Opening)?;
    let data = sheet_upload(multipart).await?;
    let r = findb::opening_book::import_collection_sheet(&user.db, &data, &user.username())?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "ar": r.ar.ok,
        "ap": r.ap.ok,
        "begin": r.begin_ok,
        "arap_ok": r.arap_ok,
        "debit_total": r.debit_total.fmt_plain(),
        "credit_total": r.credit_total.fmt_plain(),
        "auto_profit": r.auto_profit.fmt_plain(),
        "warnings": r.ar.warnings.iter().chain(r.ap.warnings.iter())
            .chain(r.balances.warnings.iter()).chain(r.equity.warnings.iter())
            .cloned().collect::<Vec<_>>(),
    })))
}

/// 导入账套：上传 .fbk（或含 .fbk 的备份 zip）→ 校验 → 落库到 books_dir → 注册
pub(super) async fn import_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    // 与 `create_book` 同一套门禁：仅管理员、每账号有账套数上限
    if !user.is_admin {
        return Err(AppError::forbidden("仅管理员可导入账套"));
    }
    let owner = user.username.clone();
    let owned = state.realm.count_books_of(&owner)?;
    if owned >= MAX_BOOKS_PER_USER {
        return Err(AppError::bad_request(format!(
            "每个账号最多创建 {MAX_BOOKS_PER_USER} 个账套（当前已有 {owned} 个），请先删除不用的账套"
        )));
    }

    let mut filename = String::new();
    let mut want_name = String::new();
    let mut data: Option<Vec<u8>> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("上传解析失败：{e}")))?
    {
        match field.name() {
            Some("file") => {
                filename = field.file_name().unwrap_or("book.fbk").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::bad_request(format!("读取上传内容失败：{e}")))?;
                data = Some(bytes.to_vec());
            }
            Some("name") => {
                want_name = field.text().await.unwrap_or_default();
            }
            _ => {}
        }
    }
    let data = data.ok_or_else(|| AppError::bad_request("缺少文件字段 file"))?;
    if data.is_empty() {
        return Err(AppError::bad_request("上传文件为空"));
    }
    if data.len() > MAX_IMPORT_BOOK_BYTES {
        return Err(AppError::bad_request(format!(
            "文件超过 {} MB 上限",
            MAX_IMPORT_BOOK_BYTES / 1024 / 1024
        )));
    }
    let (book_bytes, attach) = unpack_book_upload(&data, &filename)?;

    // 先写临时文件再校验：校验要用 SQLite 打开，而校验不过时不能留下
    // 一个已注册、已出现在账套列表里的坏账套。
    let key = make_book_key(&want_name, &owner, &state.realm)?;
    let final_path = state.books_dir.join(format!("{key}.fbk"));
    if final_path.exists() {
        return Err(AppError::bad_request("账套文件已存在"));
    }
    let tmp_path = state.books_dir.join(format!(".import_{key}.tmp"));
    std::fs::write(&tmp_path, &book_bytes)
        .map_err(|e| AppError::Internal(format!("写入临时文件失败：{e}")))?;
    if let Err(e) = validate_book_file(&tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    // `Db::open` 会把老版本账套迁到当前 schema；放在 rename 之前，
    // 迁移失败就还没动过正式文件名。
    let company = match Db::open(&tmp_path) {
        Ok(db) => db.options().company,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(AppError::bad_request(format!("打开账套失败：{e}")));
        }
    };
    // `Db::open` 会在这个临时文件旁边留一对 `-wal` / `-shm`。连接关掉时
    // SQLite 一般会自己收走，但**收不走也不该留在账套目录里**：`books_dir`
    // 里躺着一堆 `.import_xxx.tmp-wal` 半年后没人敢删。
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", tmp_path.display()));
    }
    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| AppError::Internal(format!("落盘失败：{e}")))?;

    // 账套自己的 user 表用的是它自己那套口令。平台管理员要用平台口令进来，
    // 所以按 `create_book` 同样的做法，把平台身份同步成账套内管理员。
    //
    // 「账套里已有同名用户就一个字段都不动」曾经是这里的做法，它会让**从桌面版
    // 搬来的账套根本进不去**：桌面版建的 admin 那行 `must_change_pwd = 1`
    // （首次登录必须改密，而在桌面端从没改过），Web 端登录走的是平台口令，
    // 平台那边 `must_change_pwd = 0`，两边一比就卡在账套层的闸门上 ——
    // 每个账套内接口都 401「你的口令已过期或需首次设置」，前端表现是
    // 点了账套又弹回选择页，看起来就是「导入后进不了工作页面」。
    // 所以这里必须把平台身份**对齐**进去（与 `post_change_password` 里
    // `sync_password_to_books` 的口径一致：平台是唯一真身，账套内是同名的影子）。
    {
        let db = Db::open(&final_path)?;
        let realm_user = state.realm.get_user(&owner).ok().flatten();
        match users::get(&db, &owner)? {
            None => {
                let mut u = User::new(&owner, &user.display_name, Role::Admin);
                if let Some(ru) = &realm_user {
                    u.password_hash = ru.password_hash.clone();
                }
                users::insert(&db, &u)?;
            }
            Some(mut u) => {
                // **口令哈希故意不动**：Web 端从不校验账套内口令（登录只认平台
                // 口令），改它唯一的效果是让桌面端原本能用的口令失效 —— 那是把
                // 一个「进不去」换成了另一个「进不去」。
                // 要动的是那个闸门标记：它才是 Web 端真正会读的字段。
                let mut touched: Vec<&str> = Vec::new();
                if let Some(ru) = &realm_user {
                    if u.must_change_pwd != ru.must_change_pwd {
                        u.must_change_pwd = ru.must_change_pwd;
                        touched.push("改密标记");
                    }
                }
                // 停用状态也一起放开：这是刚导入的、属于当前账号的账套，
                // 账套里那行停用只可能来自桌面端的旧配置，留着就是把人锁在门外。
                if u.disabled {
                    u.disabled = false;
                    touched.push("停用");
                }
                if !touched.is_empty() {
                    let what = touched.join("、");
                    users::update(&db, &u)?;
                    db.log(
                        &owner,
                        "账套",
                        "导入账套",
                        &format!("账套内同名账号已与平台身份对齐（{what}）"),
                    )?;
                }
            }
        }
        db.log(
            &owner,
            "账套",
            "导入账套",
            &format!("从 {filename} 导入，编码 {key}，公司 {company}"),
        )?;
    }

    // 附件：内容寻址目录是全账套共享的，写入前先确认文件真的在（同名即同内容）
    if !attach.is_empty() {
        let dir = state.books_dir.join(findb::attach::DIR_NAME);
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Internal(format!("创建附件目录失败：{e}")))?;
        for (rel, bytes) in &attach {
            let p = dir.join(rel);
            if !p.exists() {
                std::fs::write(&p, bytes)
                    .map_err(|e| AppError::Internal(format!("写入附件 {rel} 失败：{e}")))?;
            }
        }
    }

    state.books.register(&final_path, 16);
    state
        .realm
        .register_book(&key, &final_path.to_string_lossy(), &owner, &company)?;

    Ok(Json(json!({
        "key": key,
        "company": company,
        "attachments": attach.len(),
    })))
}

// ===== 本域的请求/响应结构（从 handlers.rs 收进来，v1.8.59）=====

#[derive(Deserialize)]
pub(super) struct BackupRestoreReq {
    file: String,
}

#[derive(serde::Serialize)]
pub(super) struct ReconcileDto {
    /// 全部校验通过且不空
    ok: bool,
    one_liner: String,
    empty: bool,
    sheets: Vec<ReconcileSheetDto>,
    debit_total: String,
    credit_total: String,
    balanced: bool,
    asset_total: String,
    liab_total: String,
    equity_total: String,
    checks: Vec<ReconcileCheckDto>,
    issues: Vec<String>,
}
