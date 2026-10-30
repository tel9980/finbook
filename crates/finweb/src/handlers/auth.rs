//! 由 `handlers.rs` 拆分而来。
//!
//! 为什么拆：这个文件曾到 18119 行 / 525 个顶层项，任何一次改动都要在十几屏里
//! 找目标，`clippy::too_many_lines` 报了 17 处、也无法按域做 code review。
//! 拆分只搬位置、不改一行逻辑 —— 判据是全量测试与变异套件在拆前拆后一致。
//! HTTP 请求处理函数

use super::*;

pub(super) async fn get_setup_status(
    State(state): State<Arc<WebState>>,
) -> Result<Json<SetupStatus>, AppError> {
    // 管理员是否已初始化（首次启动已引导）
    let admin_set = state.realm.count_users()? > 0;
    Ok(Json(SetupStatus {
        admin_set,
        needs_setup: false,
        company: String::new(),
        version: state.version.clone(),
        book: None,
    }))
}

pub(super) async fn post_login(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Result<Response, AppError> {
    // 全局登录（认账号库，而非某一套账）
    let username = req.username.trim().to_string();
    // 设备指纹必须非空：空串会被写进绑定名单，等于给这个账号留了一条万能通道
    let device_id = req.device_id.trim().to_string();
    if device_id.is_empty() || device_id.len() > 128 {
        return Err(AppError::bad_request(
            "缺少有效的设备标识，请刷新页面后重试",
        ));
    }
    let ip = client_ip(&headers);
    // 平台持久锁（realm 层，跨重启生效）：策略 max_fail 次连续失败后由本接口写入
    let policy = state.policy();
    if let Ok(remain) = state.realm.lock_remaining_min(&username) {
        if remain > 0 {
            return Err(AppError::rate_limited(
                format!("账号已锁定，请约 {remain} 分钟后再试（管理员可在安全中心解锁）"),
                (remain as u64) * 60,
            ));
        }
    }
    // 登录限流：先查是否已被锁定，避免锁定期内继续做昂贵/可枚举的密码校验
    if let Err(secs) = state.login_limiter.check(&username) {
        let mins = secs.div_ceil(60).max(1);
        return Err(AppError::rate_limited(
            format!("尝试过于频繁，请约 {mins} 分钟后再试"),
            secs,
        ));
    }
    if let Some(ip) = &ip {
        if let Err(secs) = state.login_ip_limiter.check(ip) {
            let mins = secs.div_ceil(60).max(1);
            return Err(AppError::rate_limited(
                format!("该来源尝试过于频繁，请约 {mins} 分钟后再试"),
                secs,
            ));
        }
    }
    let ru = match state.realm.authenticate(&username, &req.password) {
        Ok(Some(ru)) => ru,
        Ok(None) => {
            state.login_limiter.record_failure(&username);
            if let Some(ip) = &ip {
                state.login_ip_limiter.record_failure(ip);
            }
            // 登录审计（平台层）+ 连续失败达标 → 持久锁
            let _ = state
                .realm
                .record_login_attempt(&username, false, ip.as_deref().unwrap_or(""));
            if let Ok(fails) = state.realm.recent_fail_count(&username) {
                if fails >= policy.max_fail.max(1) {
                    let _ = state.realm.lock_user(&username, policy.lock_minutes);
                }
            }
            return Err(AppError::unauthorized("用户名或口令错误"));
        }
        Err(e) => return Err(AppError::from(e)),
    };
    // 登录成功，清空该账号的失败计数（内存限流 + 平台失败记录 + 遗留锁）
    state.login_limiter.clear(&username);
    if let Some(ip) = &ip {
        state.login_ip_limiter.clear(ip);
    }
    let _ = state.realm.unlock_user(&username);
    let _ = state
        .realm
        .record_login_attempt(&username, true, ip.as_deref().unwrap_or(""));
    // 设备绑定（平台层）：首次登录自动登记，此后新设备一律拒绝（v1.8.53，M-4）——
    // 换设备走管理员「重置设备绑定」，不给「悄悄多绑一台」留口子
    if !ru.is_admin {
        match state
            .realm
            .bind_device(&username, &device_id, &req.device_name)?
        {
            Ok(()) => {}
            Err(msg) => return Err(AppError::forbidden(msg)),
        }
    }
    // 顶掉「同一台设备」上的旧会话，但**保留其它设备的会话**。
    //
    // v1.8.53 之后普通账号只会绑一台设备，所以这条「保留其它设备」在当前政策下
    // 普通用户观察不到（换成 `remove_by_username` 也一样）—— 留着是因为管理员不受
    // 设备绑定约束（一人多机是运维常态），那一支走的就是这里。
    //
    // 它的前身是一次事故：这里原本是 `remove_by_username`（清该账号全部会话），
    // 与当时「一个账号可绑两台」的语义直接冲突 —— 第 2 台一登录就把第 1 台踢下线。
    // 现在政策收紧了，但函数保持"只动本设备"的语义，别改回去。
    if !ru.is_admin {
        state
            .sessions
            .remove_by_username_and_device(&username, &device_id);
    }
    let token = state.sessions.create(
        &username,
        ru.is_admin,
        &device_id,
        state.default_period,
        "", // 账套登录后由"选择账套"设定
    );
    // 返回该用户可进入的账套列表（管理员=全部，普通=本人创建 + 被邀请加入的成员）
    let books = visible_books(&state, &username, ru.is_admin)?;
    let book_list: Vec<serde_json::Value> = books
        .iter()
        .map(|b| json!({ "key": b.key, "company": b.company, "owner": b.owner_username }))
        .collect();
    let resp = LoginResp {
        user: PlatformUser {
            username: ru.username,
            display_name: ru.display_name,
            is_admin: ru.is_admin,
            must_change_pwd: ru.must_change_pwd,
        },
        must_change_pwd: ru.must_change_pwd,
        setup: false,
        books: book_list,
    };
    let mut r = Json(resp).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        crate::state::cookie_header(&token, SESSION_SECS),
    );
    Ok(r)
}

/// 登录审计：最近 N 条平台登录尝试（成功/失败 + 来源 IP）
pub(super) async fn list_security_attempts(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Query(q): Query<LoginAttemptsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 500) as usize;
    let rows = state.realm.login_attempts(q.username.as_deref(), limit)?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|(id, username, ts, ok, ip)| {
            json!({ "id": id, "username": username, "ts": ts, "ok": ok, "ip": ip })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

/// 解锁平台账号（同时清空失败计数）
pub(super) async fn unlock_security_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<UnlockSecurityReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    let name = req.username.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("账号不能为空"));
    }
    if state.realm.get_user(name)?.is_none() {
        return Err(AppError::not_found("账号不存在"));
    }
    state.realm.unlock_user(name)?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// 跨账套合并汇总（平台级，仅管理员；v1 汇总，不含内部往来抵销）
// ---------------------------------------------------------------------------

/// 合并汇总：对选中账套按期间汇总各科目期末余额（各套独立取数，仅已记账 H-3）。
/// `books=b1,b2`；返回科目 × 账套矩阵 + 合计。
/// 平台经营总览（跨账套 × 各业务域可视化；仅平台管理员）
///
/// 复用 `workbench::collect`——它已经按 9 个业务域（凭证/资金/销售/采购/仓管/生产/
/// 成本/报销/审批）出卡片，不必为每个域重写一套查询。多账套时逐套打开取数再按
/// (域, 指标, 单位) 聚合。
///
/// 两个刻意的设计：
/// - **单套失败不影响整体**：某个账套文件损坏/被占用时记 `ok:false` + 原因，
///   继续取下一套。整体 500 会让管理员看不到其他账套的真实状况。
/// - **上限 50 套**：逐套 open 是 IO 密集操作，账套数量失控时宁可截断并明确告知，
///   也不让一个请求把连接数/文件句柄打满。
pub(super) async fn platform_overview(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该入口仅限系统管理员使用"));
    }
    const MAX_BOOKS: usize = 50;
    let want_period: Option<i32> = q
        .get("period")
        .and_then(|s| parse_period(s))
        .map(|p| p.ymm());

    let mut books = visible_books(&state, &user.username, true)?;
    // 可选：只统计指定账套
    if let Some(list) = q.get("books") {
        let want: Vec<&str> = list
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if !want.is_empty() {
            books.retain(|b| want.contains(&b.key.as_str()));
        }
    }
    books.sort_by(|a, b| a.company.cmp(&b.company));
    let total = books.len();
    let truncated = total > MAX_BOOKS;
    books.truncate(MAX_BOOKS);

    let mut per_book: Vec<serde_json::Value> = Vec::with_capacity(books.len());
    // 聚合键：(域, 指标, 单位) —— 单位进键，避免把「元」和「张」加到一起
    let mut agg: std::collections::BTreeMap<(String, String, String), f64> = Default::default();
    let mut labels: std::collections::BTreeMap<(String, String, String), String> =
        Default::default();
    let mut domains: std::collections::BTreeMap<String, usize> = Default::default();
    let mut failed = 0usize;

    for b in &books {
        let db = match state.db_for(&b.key) {
            Ok(d) => d,
            Err(e) => {
                failed += 1;
                per_book.push(json!({
                    "key": b.key, "company": b.company, "owner": b.owner_username,
                    "ok": false, "error": e.to_string(),
                    "period": "", "cards": [], "todos": [],
                }));
                continue;
            }
        };
        // 期间：请求指定的优先；早于该账套启用期间时回落到启用期间（否则全是空）
        let opts = db.options();
        let start = opts.start_period;
        let cur = match want_period {
            Some(p) if p >= start.ymm() => Period::from_ymm_checked(p).unwrap_or(start),
            _ => start,
        };
        // 账套注册表里公司名可能没填，回落到账套自身参数
        let company = if b.company.trim().is_empty() {
            opts.company.clone()
        } else {
            b.company.clone()
        };
        // 以临时管理员身份看全部业务域（与 UI「全部账套」的进入语义一致）
        let admin = fincore::User::new("__platform_admin__", "平台管理员", fincore::Role::Admin);
        let wbo = match findb::workbench::collect(&db, &admin, cur, 6) {
            Ok(w) => w,
            Err(e) => {
                failed += 1;
                per_book.push(json!({
                    "key": b.key, "company": company, "owner": b.owner_username,
                    "ok": false, "error": e.to_string(),
                    "period": period_to_str(cur), "cards": [], "todos": [],
                }));
                continue;
            }
        };
        for c in &wbo.cards {
            // 卡片值是已格式化的纯数字（fmt2 无千分位 / 整数 to_string），可安全解析
            let n: f64 = c.value.replace(',', "").parse().unwrap_or(0.0);
            let k = (c.domain.clone(), c.key.clone(), c.unit.clone());
            *agg.entry(k.clone()).or_insert(0.0) += n;
            labels.entry(k).or_insert_with(|| c.label.clone());
            *domains.entry(c.domain.clone()).or_insert(0) += 1;
        }
        per_book.push(json!({
            "key": b.key, "company": company, "owner": b.owner_username,
            "ok": true, "error": "",
            "period": wbo.period,
            "cards": wbo.cards.iter().map(|c| json!({
                "domain": c.domain, "key": c.key, "label": c.label,
                "value": c.value, "unit": c.unit,
                "num": c.value.replace(',', "").parse::<f64>().unwrap_or(0.0),
            })).collect::<Vec<_>>(),
            "todos": wbo.todos.iter().map(|t| json!({
                "domain": t.domain, "key": t.key, "label": t.label,
                "count": t.count, "view": t.view,
            })).collect::<Vec<_>>(),
            // 趋势：近 6 期各业务域走势，供前端画迷你折线（跨账套对比用）
            "trends": wbo.trends.iter().map(|t| json!({
                "domain": t.domain, "key": t.key, "title": t.title, "unit": t.unit,
                "periods": t.periods,
                "series": t.series.iter().map(|s| json!({
                    "name": s.name, "color": s.color, "points": s.points,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }));
    }

    // 跨账套汇总（保持领域顺序稳定，便于前端分组渲染）
    let mut totals: Vec<serde_json::Value> = agg
        .into_iter()
        .map(|((domain, key, unit), v)| {
            json!({
                "domain": domain, "key": key, "unit": unit,
                "label": labels.get(&(domain.clone(), key.clone(), unit.clone())).cloned().unwrap_or_default(),
                "value": (v * 100.0).round() / 100.0,
            })
        })
        .collect();
    totals.sort_by(|a, b| {
        let da = a["domain"].as_str().unwrap_or("");
        let db2 = b["domain"].as_str().unwrap_or("");
        da.cmp(db2).then(
            a["key"]
                .as_str()
                .unwrap_or("")
                .cmp(b["key"].as_str().unwrap_or("")),
        )
    });

    // 汇总待办（跨账套累计待处理条数）
    let mut todo_total: std::collections::BTreeMap<String, i64> = Default::default();
    for b in &per_book {
        for t in b["todos"].as_array().cloned().unwrap_or_default() {
            let d = t["domain"].as_str().unwrap_or("").to_string();
            *todo_total.entry(d).or_insert(0) += t["count"].as_i64().unwrap_or(0);
        }
    }

    Ok(Json(json!({
        "book_count": total,
        "scanned": books.len(),
        "truncated": truncated,
        "max_books": MAX_BOOKS,
        "failed": failed,
        "domains": domains.into_iter().map(|(k, n)| json!({ "domain": k, "cards": n })).collect::<Vec<_>>(),
        "totals": totals,
        "todos": todo_total.into_iter().map(|(k, v)| json!({ "domain": k, "count": v })).collect::<Vec<_>>(),
        "books": per_book,
    })))
}

pub(super) async fn post_logout(
    State(state): State<Arc<WebState>>,
    jar: axum_extra::extract::cookie::CookieJar,
) -> Response {
    if let Some(c) = jar.get("finbook_sid") {
        state.sessions.remove(c.value());
    }
    let mut r = Json(json!({"ok": true})).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, clear_cookie_header());
    r
}

pub(super) async fn get_me(user: CurrentUser) -> Result<Json<PublicUser>, AppError> {
    // 直接使用身份对账后的用户：管理员查看他人账套时是临时身份
    // （不写入账套 user 表），回查数据库会 404
    Ok(Json(PublicUser::from_user(&user.user)))
}

pub(super) async fn post_change_password(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<ChangePwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 改的是平台口令（与登录身份一致）
    check_password(&state, &req.new)?;
    let r = state
        .realm
        .change_password(&user.username, &req.old, &req.new, &state.policy())?;
    match r {
        Ok(()) => {
            // 同步到该用户出现过的所有账套内的同名用户行，保持单密码一致
            if let Ok(Some(ru)) = state.realm.get_user(&user.username) {
                let _ = state.realm.sync_password_to_books(
                    &state.books_dir,
                    &user.username,
                    &ru.password_hash,
                    ru.must_change_pwd,
                );
            }
            // 口令已变：吊销本人其他设备上的旧会话，当前设备保留
            state.sessions.remove_others(&user.username, &user.token);
            Ok(Json(json!({"ok": true})))
        }
        Err(msg) => Err(AppError::bad_request(msg)),
    }
}

// ---------------------------------------------------------------------------
// 用户管理（UserManage）
// ---------------------------------------------------------------------------

pub(super) async fn list_users(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<PublicUser>>, AppError> {
    user.require(Perm::UserManage)?;
    let db = &user.db;
    let list = users::list(&*db)?
        .into_iter()
        .map(|u| PublicUser::from_user(&u))
        .collect();
    Ok(Json(list))
}

pub(super) async fn create_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CreateUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    // 权限调整收归管理员：即使被额外授予 UserManage，非管理员也不能铸号分配角色/权限
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以创建账号并分配权限"));
    }
    let username = req.username.trim().to_string();
    if username.is_empty() {
        return Err(AppError::bad_request("用户名不能为空"));
    }
    // Web 端登录只认账号库：账套内子账号必须对应一个已存在的账号，
    // 否则开出来的账号无法登录（死账号）。单密码统一后账套内不再独立设口令，
    // 直接沿用平台口令哈希，前端若传了 password 则忽略（兼容旧前端）。
    let ru = state.realm.get_user(&username)?.ok_or_else(|| {
        AppError::bad_request("该账号尚未开通，请先让管理员在「账号管理」中开通同名账号")
    })?;
    // 账套管理员不能把管理员拉进自己的账套：账套内重置口令会重置平台口令，
    // 否则任何能建账的用户都能借此接管管理员账号。
    if ru.is_admin {
        return Err(AppError::forbidden("不能将管理员加入账套"));
    }
    let db = &user.db;
    if users::get(&db, &username)?.is_some() {
        return Err(AppError::bad_request("该用户名已存在"));
    }
    let mut u = User::new(&username, &req.display_name, req.role);
    u.roles = req
        .roles
        .iter()
        .copied()
        .filter(|r| *r != req.role)
        .collect();
    u.password_hash = ru.password_hash.clone();
    // 管理员开的号，口令是管理员定的——首次登录必须自己改一次
    u.must_change_pwd = req.must_change_pwd;
    u.memo = req.memo;
    // 凭证可见性默认放开（多岗位协作）：需要收紧的账号由管理员在「用户编辑 →
    // 数据范围」逐账号勾选「仅看本人填制的凭证」（过滤机制见 DataScope）
    // 角色基础上的逐项覆盖（管理员角色忽略，避免把自己锁在门外）
    if !u.is_admin() {
        u.extra_perms = req.extra_perms;
        u.deny_perms = req.deny_perms;
    }
    let id = users::insert(&db, &u)?;
    // 账套层的「首登改密」要同步到平台账号，否则不生效。
    //
    // 为什么不生效：登录门禁 `post_login` 读的是**平台层**的
    // `ru.must_change_pwd`（见 post_login 的 LoginResp），账套里那一位只影响
    // 账套内的记录。实测：建号时账套层明确传 `must_change_pwd:false`，平台层
    // 开号时没传（默认 true），结果仍然被拦在「必须先改密」上。
    //
    // 为什么在这里同步而不是在登录时取交集：平台账号是为这个账套的身份而存在
    // 的，管理员在账套里表达的「这个人不用改密」就是他的真实意图；让登录门禁
    // 去扫所有账套成员关系，既多一次 IO，又会出现「A 账套开着、B 账套关着」
    // 这种没法解释的组合。只在**显式传值**时同步，不做隐式覆盖。
    if !req.must_change_pwd && ru.must_change_pwd {
        state.realm.set_must_change_pwd(&username, false)?;
        db.log(
            user.username(),
            "安全",
            "新建用户",
            &format!("账号「{username}」设为免首登改密（同步到平台账号）"),
        )?;
    }
    Ok(Json(json!({"id": id})))
}

pub(super) async fn update_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
    Json(req): Json<UpdateUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let db = &user.db;
    let mut u =
        users::get(&db, &username)?.ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;
    // 账套归属者不可被停用 / 降权 / 改名，否则账套会失去主人
    if let Some(owner) = state.realm.book_owner(&user.book_key)? {
        if username == owner {
            return Err(AppError::bad_request("不能修改账套归属者的账套内身份"));
        }
    }
    // 不能改自己的授权字段：`UserManage` 是一个可单独授予主管的权限，若能改自己，
    // 把 role 设成 Admin 或清空 deny_perms 就是一次自我提权。显示名/备注不涉及授权，
    // 允许自助修改。
    if username == user.username() {
        let touches_grant = req.role.is_some()
            || req.roles.is_some()
            || req.extra_perms.is_some()
            || req.deny_perms.is_some()
            || req.data_scope.is_some()
            || req.disabled == Some(true);
        if touches_grant {
            return Err(AppError::bad_request(
                "不能修改自己的角色、权限矩阵或停用本人，请由其他管理员操作",
            ));
        }
    }
    // 权限调整收归管理员（2026-09 审计）：非管理员即使持有 UserManage，也不能调整
    // 其他账号的角色/权限矩阵/数据范围/停用；显示名、备注等非授权字段仍可代改。
    if username != user.username() {
        let touches_grant = req.role.is_some()
            || req.roles.is_some()
            || req.extra_perms.is_some()
            || req.deny_perms.is_some()
            || req.data_scope.is_some()
            || req.disabled.is_some();
        if touches_grant && !user.user.is_admin() {
            return Err(AppError::forbidden("只有管理员可以调整其他账号的权限"));
        }
    }
    if let Some(d) = req.display_name {
        u.display_name = d;
    }
    if let Some(r) = req.role {
        // 不允许把最后一个管理员改成其他角色
        if u.is_admin() && r != Role::Admin {
            let admins = users::list(&*db)?
                .into_iter()
                .filter(|x| x.is_admin())
                .count();
            if admins <= 1 {
                return Err(AppError::bad_request("至少保留一个系统管理员账号"));
            }
        }
        u.role = r;
    }
    if let Some(rs) = req.roles {
        u.roles = rs.into_iter().filter(|r| *r != u.role).collect();
    }
    if let Some(d) = req.disabled {
        u.disabled = d;
        if d {
            // 停用账号：立即下线其全部会话
            state.sessions.remove_by_username(&username);
        }
    }
    if let Some(m) = req.must_change_pwd {
        u.must_change_pwd = m;
    }
    if let Some(m) = req.memo {
        u.memo = m;
    }
    if let Some(s) = req.data_scope {
        u.data_scope = s;
    }
    if let Some(p) = req.extra_perms {
        u.extra_perms = p;
    }
    if let Some(p) = req.deny_perms {
        u.deny_perms = p;
    }
    users::update(&db, &u)?;
    db.log(
        user.username(),
        "安全",
        "修改用户",
        &format!("更新「{username}」的信息"),
    )?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn reset_user_device(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    // 设备绑定 + 会话下线 = 账号安全操作，收归套内管理员（非管理员可借此踢任意成员下线）
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以重置设备绑定"));
    }
    let db = &user.db;
    // 目标必须是本账套成员：否则凭用户名即可强制下线任意平台用户的全部会话
    if users::get(&db, &username)?.is_none() {
        return Err(AppError::not_found("该用户不在当前账套"));
    }
    users::reset_device(&db, &username)?;
    // 立刻下线该用户**全部**会话（含其它已绑定设备）：旧设备不能靠存量会话继续用
    state.sessions.remove_by_username(&username);
    db.log(
        user.username(),
        "安全",
        "重置设备绑定",
        &format!("重置「{username}」的设备绑定"),
    )?;
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn unlock_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    // 解锁被停用账号 = 抵消管理员的停用决定，属授权变更，收归套内管理员
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以解锁账号"));
    }
    let db = &user.db;
    security::unlock_user(&db, &username)?;
    db.log(
        user.username(),
        "安全",
        "解锁用户",
        &format!("解锁「{username}」"),
    )?;
    Ok(Json(json!({"ok": true})))
}

/// 角色 → 权限矩阵（供前端在新建/改角色时实时预览）
pub(super) async fn list_roles(user: CurrentUser) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let roles: Vec<serde_json::Value> = Role::all()
        .iter()
        .map(|r| {
            let perms: Vec<serde_json::Value> = r
                .perms()
                .iter()
                .map(|p| {
                    let code = serde_json::to_value(*p)
                        .ok()
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    json!({ "code": code, "label": p.label() })
                })
                .collect();
            json!({ "role": serde_json::to_value(*r).ok().and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default(), "label": r.label(), "perms": perms })
        })
        .collect();
    Ok(Json(json!(roles)))
}

pub(super) async fn delete_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    if username == user.username() {
        return Err(AppError::bad_request("不能删除当前登录的账号"));
    }
    // 删号 = 调整他人账号，收归管理员
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以删除账号"));
    }
    let db = &user.db;
    let u =
        users::get(&db, &username)?.ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;
    // 账套归属者不可删除（删了账套就没有主人了）
    if let Some(owner) = state.realm.book_owner(&user.book_key)? {
        if username == owner {
            return Err(AppError::bad_request("不能删除账套归属者"));
        }
    }
    if u.is_admin() {
        let admins = users::list(&*db)?
            .into_iter()
            .filter(|x| x.is_admin())
            .count();
        if admins <= 1 {
            return Err(AppError::bad_request("至少保留一个系统管理员账号"));
        }
    }
    users::delete(&db, u.id)?;
    state.sessions.remove_by_username(&username);
    db.log(
        user.username(),
        "安全",
        "删除用户",
        &format!("删除账号「{username}」"),
    )?;
    Ok(Json(json!({"ok": true})))
}

