use super::*;




// ---------------------------------------------------------------------------
// 平台安全中心（口令策略 / 登录审计 / 账号解锁；均仅管理员）
// ---------------------------------------------------------------------------

/// 口令策略读取
pub(super) async fn get_security_policy(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    Ok(Json(json!(state.realm.policy()?)))
}


/// 口令策略保存（值域校验：过小会把全员锁死，过大等于策略失效）
pub(super) async fn set_security_policy(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(p): Json<fincore::user::PasswordPolicy>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if !(1..=64).contains(&p.min_len) {
        return Err(AppError::bad_request("最小口令长度需在 1-64 之间"));
    }
    if !(1..=20).contains(&p.max_fail) {
        return Err(AppError::bad_request("连续失败次数需在 1-20 之间"));
    }
    if !(1..=1440).contains(&p.lock_minutes) {
        return Err(AppError::bad_request("锁定时长需在 1-1440 分钟之间"));
    }
    if !(0..=1440).contains(&p.idle_minutes) {
        return Err(AppError::bad_request(
            "空闲超时需在 0-1440 分钟之间（0=不自动登出）",
        ));
    }
    if !(0..=3650).contains(&p.max_age_days) {
        return Err(AppError::bad_request(
            "口令有效期需在 0-3650 天之间（0=永不过期）",
        ));
    }
    state.realm.set_policy(&p)?;
    Ok(Json(json!({ "ok": true })))
}


/// 当前处于锁定状态的平台账号
pub(super) async fn list_locked_security_users(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    let mut items = Vec::new();
    for u in state.realm.list_users()? {
        let remain = state.realm.lock_remaining_min(&u.username)?;
        if remain > 0 {
            items.push(json!({
                "username": u.username,
                "display_name": u.display_name,
                "locked_until": u.locked_until,
                "remaining_min": remain,
            }));
        }
    }
    Ok(Json(json!({ "items": items })))
}


// ---------------------------------------------------------------------------
// 账号管理（仅管理员）
// ---------------------------------------------------------------------------

pub(super) async fn list_platform_users(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<Vec<PlatformUserItem>>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    let list = state
        .realm
        .list_users()?
        .into_iter()
        .map(|u| PlatformUserItem::from_realm(&u))
        .collect();
    Ok(Json(list))
}


pub(super) async fn create_platform_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<PlatformUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if req.username.trim().is_empty() {
        return Err(AppError::bad_request("用户名不能为空"));
    }
    check_password(&state, &req.password)?;
    // 管理员开的号，口令是管理员定的——首次登录必须自己改一次
    let id = state.realm.create_user(
        &req.username,
        &req.display_name,
        &req.password,
        req.is_admin,
        true,
        &state.policy(),
    )?;
    Ok(Json(json!({ "id": id })))
}


pub(super) async fn update_platform_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
    Json(req): Json<UpdatePlatformUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if username == user.username {
        return Err(AppError::bad_request(
            "不能修改当前登录的账号，请使用改密功能",
        ));
    }
    state.realm.update_user(
        &username,
        req.display_name.as_deref(),
        req.disabled,
        req.is_admin,
    )?;
    Ok(Json(json!({"ok": true})))
}


pub(super) async fn delete_platform_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if username == user.username {
        return Err(AppError::bad_request("不能删除当前登录的账号"));
    }
    if state.realm.count_books_of(&username)? > 0 {
        return Err(AppError::bad_request(
            "该用户仍拥有账套，请先删除其账套后再删除账号",
        ));
    }
    state.realm.delete_user(&username)?;
    state.sessions.remove_by_username(&username);
    Ok(Json(json!({"ok": true})))
}


pub(super) async fn reset_platform_password(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
    Json(req): Json<ResetPwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    check_password(&state, &req.new)?;
    state
        .realm
        .reset_password(&username, &req.new, &state.policy())?;
    // 口令已变，立即吊销该账号的全部旧会话（被盗会话不能继续用满 7 天）
    state.sessions.remove_by_username(&username);
    if let Ok(Some(ru)) = state.realm.get_user(&username) {
        let _ = state.realm.sync_password_to_books(
            &state.books_dir,
            &username,
            &ru.password_hash,
            ru.must_change_pwd,
        );
    }
    Ok(Json(json!({"ok": true})))
}


/// 平台层重置设备绑定：解绑**全部**已绑定设备后，该账号可在任意设备重新登录
/// （重置只重启一次首绑机会：该账号下一台登录的设备会被自动登记，再往后仍会被拒）
pub(super) async fn reset_platform_device(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if username == user.username {
        return Err(AppError::bad_request("不能重置当前登录账号的设备"));
    }
    state.realm.clear_device(&username)?;
    // 强制重新登录：旧会话不能再沿用
    state.sessions.remove_by_username(&username);
    Ok(Json(json!({"ok": true})))
}


pub(super) async fn reset_user_password(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
    Json(req): Json<ResetPwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    // 口令即身份：与建号/改权/删号同口径收归套内管理员（持有 UserManage 的非管理员
    // 不得改写任何成员的全局口令——口令同步全部账套）
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以重置账号口令"));
    }
    let db = &user.db;
    // 单密码统一：账套内没有独立口令，重置即重置该用户的平台口令，
    // 再同步到其出现过的所有账套。目标必须是当前账套成员，防越权重置陌生人口令。
    if users::get(&db, &username)?.is_none() {
        return Err(AppError::not_found("该用户不在当前账套"));
    }
    let ru = state
        .realm
        .get_user(&username)?
        .ok_or_else(|| AppError::bad_request("该账号不存在"))?;
    // 管理员的口令只能由管理员在「账号」中重置。账套管理员若能把
    // 管理员邀请进本套再调本接口，就能重置其平台口令并同步到全部账套。
    if ru.is_admin {
        return Err(AppError::forbidden(
            "管理员的口令请由管理员在「账号管理」中重置",
        ));
    }
    // 平台口令是全局的：账套管理员只允许重置「仅属于本账套、且不拥有任何账套」的
    // 成员。否则"把任意账号邀请进自己的账套，再重置其全局口令"即可跨租户
    // 接管/锁死他人账号（受害者口令被改，且会同步覆盖其名下所有账套）。
    let caller_is_platform_admin = state
        .realm
        .get_user(user.username())?
        .map(|u| u.is_admin)
        .unwrap_or(false);
    if !caller_is_platform_admin {
        if state.realm.count_books_of(&username)? > 0 {
            return Err(AppError::forbidden(
                "该账号拥有自己的账套，账套管理员不能重置其口令，请由管理员处理",
            ));
        }
        if state
            .realm
            .count_books_containing(&state.books_dir, &username)?
            > 1
        {
            return Err(AppError::forbidden(
                "该账号还属于其他账套，账套管理员不能重置其口令，请由管理员处理",
            ));
        }
    }
    check_password(&state, &req.new)?;
    state
        .realm
        .reset_password(&username, &req.new, &state.policy())?;
    // 口令已变，立即吊销该账号的全部旧会话（被盗会话不能继续用满 7 天）
    state.sessions.remove_by_username(&username);
    if let Ok(Some(ru)) = state.realm.get_user(&username) {
        let _ = state.realm.sync_password_to_books(
            &state.books_dir,
            &username,
            &ru.password_hash,
            ru.must_change_pwd,
        );
    }
    db.log(
        user.username(),
        "安全",
        "重置口令",
        &format!("重置「{username}」的口令（平台口令）"),
    )?;
    Ok(Json(json!({"ok": true})))
}
