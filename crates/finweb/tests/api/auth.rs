//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

#[tokio::test]
async fn unauthenticated_me_is_401() {
    let (state, _bd, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// M-9：未登录探测不能区分"接口是否存在 / 方法是否注册"（401/404/405 统一 401）
#[tokio::test]
async fn m9_unauthenticated_probe_uniform_401() {
    let (state, _bd, _dir) = test_state();

    // 真实接口未登录 → 401（既有行为，取其响应作为基准）
    let resp_me = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp_me.status(), StatusCode::UNAUTHORIZED);
    let body_me = body_string(resp_me).await;

    // 不存在的接口 → 401 且响应与真实接口逐字一致（而不是 404）
    let resp_fake = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/definitely-not-a-route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp_fake.status(),
        StatusCode::UNAUTHORIZED,
        "不存在的接口对未登录探测也应 401"
    );
    assert_eq!(body_string(resp_fake).await, body_me);

    // 存在但方法未注册 → 401（而不是 405）
    let resp_method = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method(axum::http::Method::PUT)
                .uri("/api/books")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp_method.status(),
        StatusCode::UNAUTHORIZED,
        "方法不匹配对未登录探测也应 401"
    );

    // 已登录并选定账套后：不存在的接口回 404（功能可见性对登录用户不隐藏）
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(select_book(&state, &sid, "b1").await, StatusCode::OK);
    let resp_authed = handlers::router(state.clone())
        .oneshot(authed_get("/api/definitely-not-a-route", &sid))
        .await
        .unwrap();
    assert_eq!(resp_authed.status(), StatusCode::NOT_FOUND);

    // 公开门露：健康检查无需会话
    let resp_health = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp_health.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_login_returns_platform_identity() {
    let (state, _bd, _dir) = test_state();
    let (status, sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(status, StatusCode::OK, "平台登录应成功");
    assert!(!sid.is_empty());
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"is_admin\":true"), "应返回平台身份：{s}");
    assert!(s.contains("b1"), "管理员应看到预置账套：{s}");
    // 未选账套前，进入账套级接口应被引导去选账套
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "未选账套时 /me 应 401"
    );
}

#[tokio::test]
async fn normal_user_creates_and_enters_own_book() {
    let (state, _bd, _dir) = test_state();
    // 管理员开通普通用户
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({
                "username": "zhang",
                "display_name": "张会计",
                "password": "Zhang123456",
                "is_admin": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通平台用户应成功");

    // 普通用户登录（首登强制改密）→ 改密
    let (status, sid) = login(&state, "zhang", "Zhang123456").await;
    assert_eq!(status, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Zhang123456", "new": "Zhang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "强制改密应成功");

    // 账号模型二元化：普通账号不能自建账套
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/books",
            &sid,
            serde_json::json!({ "key": "", "company": "张记贸易直建", "start_period": 202601 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通账号不能新建账套");

    // 管理员代建 + 邀请进入：进入者是套内会计成员（不再是"创建者即管理员"）
    let (status, body) = create_book(&state, &sid, "张记贸易").await;
    assert_eq!(status, StatusCode::OK, "管理员代建账套应成功：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();

    let status = select_book(&state, &sid, &key).await;
    assert_eq!(status, StatusCode::OK, "受邀进入账套应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(
        s.contains("\"is_admin\":false"),
        "受邀成员应为套内非管理员：{s}"
    );
    assert!(
        s.contains("\"role\":\"accountant\""),
        "受邀成员应为会计岗位：{s}"
    );
}

#[tokio::test]
async fn admin_can_enter_any_book() {
    let (state, books_dir, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    // boss 既是平台管理员也是 b1 的归属者；再开一个普通用户建账套验证跨账套
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "li", "display_name": "李会计", "password": "Li123456" }),
        ))
        .await
        .unwrap();
    let (_, li_sid) = login(&state, "li", "Li123456").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &li_sid,
            serde_json::json!({ "old": "Li123456", "new": "Li654321" }),
        ))
        .await
        .unwrap();
    let (_, body) = create_book(&state, &li_sid, "李记商行").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();

    // 第二个管理员进入该套：临时管理员身份，不写入账套 user 表
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "m2", "display_name": "管理员二", "password": "M2aa123456", "is_admin": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通第二管理员应成功");
    let (_, m2_sid) = login(&state, "m2", "M2aa123456").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &m2_sid,
            serde_json::json!({ "old": "M2aa123456", "new": "M2aa654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "m2 改密应成功");
    let status = select_book(&state, &m2_sid, &key).await;
    assert_eq!(status, StatusCode::OK, "管理员应能进入任意账套");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &m2_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员进入后 /me 应可用");

    // 无痕迹验证：归属者行（owner seed，建套时写入）保留；以临时身份进入的访问者 m2 不留行
    let db = findb::Db::open(books_dir.join(format!("{key}.fbk"))).expect("打开账套文件失败");
    assert!(
        findb::users::get(&db, "boss").unwrap().is_some(),
        "归属者（管理员）的套内行应保留"
    );
    assert!(
        findb::users::get(&db, "m2").unwrap().is_none(),
        "访问他人账套的管理员不应留下账号记录"
    );
}

/// 平台层设备绑定：一个账号最多绑 **2 台**，第三台被拒，管理员重置后可重新绑定。
///
/// 为什么不是 1 台：真实场景里同一个人两台机器是常态（公司台式机 + 家里笔记本）。
/// 锁死 1 台的结果是用户隔几天就要找管理员重置一次 —— 一个安全功能如果天天要人工
/// 介入，实际效果就是被绕过。
///
/// 语义（v1.8.53 起）：**首次登录自动绑定，此后不再自动放新设备进来**；换设备只能
/// 由管理员在「账号」页重置。
///
/// 这条测试自己就是被改的第三版，三个版本的差别值得记住：
/// 1. 最初「一人一机」——第二台直接拒。用户实际是手机+电脑，太硬；
/// 2. 后来「最多两台」——第二台自动绑。**这一版是错的**：拿到口令的人可以悄悄绑
///    一台自己的设备长期用，名单里那行看起来跟用户自己的手机没区别（M-4）；
/// 3. 现在「首次绑定后禁改」——既不限制用户首登，也不给扩张留后门。
///
/// 为什么不是不限：能自动绑就是能被悄悄多用，上限只是「悄悄多用几台」的数字。
#[tokio::test]
async fn platform_device_binding_and_reset() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "emp", "display_name": "员工", "password": "Emp123456" }),
        ))
        .await
        .unwrap();

    // 第一台设备登录：绑定该设备（首登强制改密，先改密再测权限边界）
    let (status, s1) = login_with_device(&state, "emp", "Emp123456", "dev-A").await;
    assert_eq!(status, StatusCode::OK, "首台设备登录应成功并完成绑定");
    assert!(!s1.is_empty());
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &s1,
            serde_json::json!({ "old": "Emp123456", "new": "Emp654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 改密会清掉该账号的全部会话，所以先拿一个新的 dev-A 会话再测「两台并存」。
    // 不重新登录的话 s1 早就死了 —— 那是改密的语义，与设备绑定无关，
    // 混在一起测就会把「改密踢会话」误读成「新设备踢掉旧设备」。
    let (status, s1b) = login_with_device(&state, "emp", "Emp654321", "dev-A").await;
    assert_eq!(status, StatusCode::OK, "原设备重新登录应成功（已绑定）");

    // 已绑定设备继续可用（这是「手机 + 电脑」的正常场景，不该被这次收紧干掉）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &s1b))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "已绑定设备的会话应仍然有效"
    );

    // 第二台设备**必须被拒**：拿到口令的人不能悄悄多绑一台自己的机器
    let (status, _) = login_with_device(&state, "emp", "Emp654321", "dev-B").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "未授权的新设备应被拒绝（首次绑定后禁改）"
    );

    // 拒绝信息要能让用户知道下一步做什么，而不是笼统的「登录失败」
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "username": "emp", "password": "Emp654321",
                        "device_id": "dev-B", "device_name": "第二台"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_string(r).await;
    assert!(
        body.contains("重置设备绑定"),
        "提示要写清找管理员重置：{body}"
    );

    // 非管理员不能重置设备
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users/emp/reset-device",
            &s1b,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通用户不能重置设备");

    // 管理员重置设备绑定（同时踢掉该用户全部会话）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users/emp/reset-device",
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员重置设备应成功");
    // 旧会话已失效
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &s1b))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "重置设备后旧会话应失效"
    );

    // 重置后「先到的那台」重新绑定成功，紧随其后的另一台仍会被拒
    // —— 这正是新语义：重置只重启一次首绑机会，不是把上限清零重来
    let (status, _) = login_with_device(&state, "emp", "Emp654321", "dev-B").await;
    assert_eq!(status, StatusCode::OK, "重置后新设备应可重新绑定");
    let (status, _) = login_with_device(&state, "emp", "Emp654321", "dev-C").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "重置再绑一台之后，其它设备仍应被拒"
    );
}

#[tokio::test]
async fn member_revoke_takes_effect_immediately() {
    let (state, _bd, _dir) = test_state();
    // 开通 zhang（owner）与 acc1（成员）
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    for (u, pwd) in [("zhang", "Zhang123456"), ("acc1", "Acc1123456")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &admin_sid,
                serde_json::json!({ "username": u, "display_name": u, "password": pwd }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通 {u} 应成功");
    }

    // zhang 首登改密 → 建账套 → 进入
    let (status, zhang_sid) = login(&state, "zhang", "Zhang123456").await;
    assert_eq!(status, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &zhang_sid,
            serde_json::json!({ "old": "Zhang123456", "new": "Zhang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (_, body) = create_book(&state, &zhang_sid, "张记撤销").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();
    assert_eq!(select_book(&state, &zhang_sid, &key).await, StatusCode::OK);
    // 管理员进入该套（后续以管理员身份执行邀请/停用等治理操作）
    assert_eq!(select_book(&state, &admin_sid, &key).await, StatusCode::OK);

    // 管理员邀请 acc1（会计，不强制改密）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &admin_sid,
            serde_json::json!({
                "username": "acc1", "display_name": "小会", "password": "Acc1123456",
                "role": "accountant", "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // acc1 登录改密进入账套
    let (status, acc_sid) = login(&state, "acc1", "Acc1123456").await;
    assert_eq!(status, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &acc_sid,
            serde_json::json!({ "old": "Acc1123456", "new": "Acc1654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(select_book(&state, &acc_sid, &key).await, StatusCode::OK);

    // 任何账号都不能停用自己/改自己的授权（自改保护——由管理员自身触发验证：
    // 新模型下管理员还是账套归属者，命中归属者/自改保护同样是 400）
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/boss",
            &admin_sid,
            serde_json::json!({ "disabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不能停用自己");

    // 管理员停用成员 → 成员现有会话立即下线
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/acc1",
            &admin_sid,
            serde_json::json!({ "disabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员停用成员应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &acc_sid))
        .await
        .unwrap();
    // 停用即移除其全部会话（remove_by_username），下一次请求在会话层就被拒绝；
    // 即使会话残留，身份对账后的 disabled 复核也会 403 兜底
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "被停用成员的会话应立即下线"
    );

    // 管理员移除成员 → 成员重新登录也进不了账套（账号本身不受影响）
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/users/acc1")
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "移除成员应成功");
    let (status, acc_sid2) = login(&state, "acc1", "Acc1654321").await;
    assert_eq!(status, StatusCode::OK, "平台账号本身仍可登录");
    let status = select_book(&state, &acc_sid2, &key).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "被移除的成员不再能进入账套");
}

#[tokio::test]
async fn book_member_collaboration() {
    let (state, _bd, _dir) = test_state();
    // 1) 管理员开通平台账号：zhang（账套归属者）与 acc1（被邀请成员）
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    for (u, name, pwd) in [
        ("zhang", "张会计", "Zhang123456"),
        ("acc1", "小会", "Acc1123456"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &admin_sid,
                serde_json::json!({ "username": u, "display_name": name, "password": pwd }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台账号 {u} 应成功");
    }

    // 2) zhang 首登改密 → 自建账套 → 进入
    let (status, zhang_sid) = login(&state, "zhang", "Zhang123456").await;
    assert_eq!(status, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &zhang_sid,
            serde_json::json!({ "old": "Zhang123456", "new": "Zhang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (_, body) = create_book(&state, &zhang_sid, "张记贸易").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();
    let status = select_book(&state, &zhang_sid, &key).await;
    assert_eq!(status, StatusCode::OK, "归属者应能进入自己的账套");
    // 管理员进入该套（后续以管理员身份执行邀请等治理操作）
    assert_eq!(select_book(&state, &admin_sid, &key).await, StatusCode::OK);

    // 3) 管理员邀请 acc1 进入账套（账套内建「会计」角色，不强制改密）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &admin_sid,
            serde_json::json!({
                "username": "acc1",
                "display_name": "小会",
                "password": "Acc1123456",
                "role": "accountant",
                "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请成员应成功");
    // 未开通账号的子账号应被拒绝（避免产生无法登录的死账号）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &admin_sid,
            serde_json::json!({
                "username": "ghost",
                "display_name": "幽灵",
                "password": "Ghost123456",
                "role": "accountant",
                "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "无平台账号的子账号应被拒绝"
    );

    // 4) acc1 平台登录 → 改密 → 进入 zhang 的账套
    let (status, acc_sid) = login(&state, "acc1", "Acc1123456").await;
    assert_eq!(status, StatusCode::OK, "成员平台登录应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &acc_sid,
            serde_json::json!({ "old": "Acc1123456", "new": "Acc1654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let status = select_book(&state, &acc_sid, &key).await;
    assert_eq!(status, StatusCode::OK, "被邀请成员应能进入账套");

    // 5) 成员身份：非管理员、会计角色、无用户管理权限
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &acc_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"is_admin\":false"), "成员应为非管理员：{s}");
    assert!(
        s.contains("\"role\":\"accountant\""),
        "成员角色应为会计：{s}"
    );
    assert!(
        !s.contains("\"user_manage\""),
        "成员不应有用户管理权限：{s}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/platform/users", &acc_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "成员不应能管平台账号");
}

#[tokio::test]
async fn login_rate_limited_after_repeated_failures() {
    let (state, _bd, _dir) = test_state();
    // 连续失败达到平台策略阈值（默认 max_fail=5，与桌面默认一致）→ 写入持久锁；
    // 阈值内每次都应是 401
    for i in 0..5 {
        let (status, _sid) = login(&state, "boss", "WrongPass!").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "第 {} 次失败应 401",
            i + 1
        );
    }
    // 第 6 次即使密码正确，也被持久锁拦截 → 429 + Retry-After（锁定前置，不再做 argon2）
    let resp = handlers::router(state.clone())
        .oneshot(post_json(
            "/api/login",
            serde_json::json!({
                "username": "boss",
                "password": "Admin!2026",
                "device_id": "dev-test-0001",
                "device_name": "测试机",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS, "锁定后应 429");
    assert!(
        resp.headers().contains_key(header::RETRY_AFTER),
        "429 响应应带 Retry-After 头"
    );
    let s = body_string(resp).await;
    assert!(s.contains("retry_after"), "响应体应含剩余等待秒数：{s}");
}

/// 通过 extra_perms 拿到 UserManage 的普通用户，不能改自己的角色 / 权限矩阵 /
/// 数据范围（那等于一步自我提权）；改他人的授权也已收紧为仅管理员（2026-09 审计）。
#[tokio::test]
async fn user_manager_cannot_escalate_self() {
    let (state, _bd, _dir) = test_state();
    let (_, boss_sid) = login(&state, "boss", "Admin!2026").await;

    for u in ["sup1", "acc9"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss_sid,
                serde_json::json!({ "username": u, "display_name": u, "password": "Init@123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台账号 {u}");
    }
    assert_eq!(select_book(&state, &boss_sid, "b1").await, StatusCode::OK);
    for (u, role, extra) in [
        ("sup1", "accountant", vec!["user_manage"]),
        ("acc9", "accountant", vec![]),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &boss_sid,
                serde_json::json!({
                    "username": u, "display_name": u, "password": "Init@123456",
                    "role": role, "must_change_pwd": false, "extra_perms": extra,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {u}");
    }

    let mut sid_sup = String::new();
    for u in ["sup1", "acc9"] {
        let (st, sid) = login(&state, u, "Init@123456").await;
        assert_eq!(st, StatusCode::OK, "{u} 登录");
        // 平台账号建号时强制首登改密，不改密会被拦在账套之外
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/change-password",
                &sid,
                serde_json::json!({ "old": "Init@123456", "new": "Pass123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{u} 首登改密");
        let (st, sid) = login(&state, u, "Pass123456").await;
        assert_eq!(st, StatusCode::OK, "{u} 改密后重登");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 进账套"
        );
        if u == "sup1" {
            sid_sup = sid;
        }
    }

    // 给自己加授权：三种载体都要被拦
    for body in [
        serde_json::json!({ "role": "admin" }),
        serde_json::json!({ "extra_perms": ["user_manage", "backup"] }),
        serde_json::json!({ "deny_perms": [] }),
        serde_json::json!({ "data_scope": {} }),
        serde_json::json!({ "disabled": true }),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_put("/api/users/sup1", &sid_sup, body.clone()))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "自我提权应被拒，body={body}"
        );
    }

    // 改自己的显示名/备注不涉及授权，放行
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/sup1",
            &sid_sup,
            serde_json::json!({ "display_name": "我自己改的" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "自助改显示名不该被拦");

    // 改他人权限：收紧为仅管理员（2026-09 审计：只有 admin 可以调整其他账号的权限）。
    // sup1 虽被额外授予 UserManage，但角色是会计 → 越权改他人角色应被拒。
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/acc9",
            &sid_sup,
            serde_json::json!({ "role": "viewer" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "非管理员改他人权限应被拒"
    );
}

/// 权限调整收归管理员 + 会计出纳不相容（2026-09 审计双需求）。
#[tokio::test]
async fn admin_only_perm_changes_and_duty_separation() {
    let (state, _bd, _dir) = test_state();
    let boss_sid = boss_in_b1(&state).await;

    // ① 会计出纳互斥（建号路径）：出纳签字 × 会计核心 → 拒绝
    for (u, role, extra) in [
        ("mix1", "accountant", vec!["cashier_sign"]),
        ("mix2", "cashier", vec!["voucher_post"]),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss_sid,
                serde_json::json!({ "username": u, "display_name": u, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台 {u}");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &boss_sid,
                serde_json::json!({
                    "username": u, "display_name": u, "password": "",
                    "role": role, "must_change_pwd": false, "extra_perms": extra,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "{role} + {extra:?} 混合应被拒"
        );
        let s = body_string(resp).await;
        assert!(s.contains("会计与出纳"), "应提示互斥：{s}");
    }

    // 合法组合：mgr1 = 会计 + UserManage（无出纳签字）；vic1 = 纯出纳
    for (u, role, extra) in [
        ("mgr1", "accountant", Some(vec!["user_manage"])),
        ("vic1", "cashier", None),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss_sid,
                serde_json::json!({ "username": u, "display_name": u, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台 {u}");
        let mut body = serde_json::json!({
            "username": u, "display_name": u, "password": "",
            "role": role, "must_change_pwd": false,
        });
        if let Some(x) = extra {
            body["extra_perms"] = serde_json::json!(x);
        }
        let resp = handlers::router(state.clone())
            .oneshot(authed_post("/api/users", &boss_sid, body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {role} 应成功");
    }

    // mgr1 登录 → 首登改密 → 进 b1
    let (st, mgr) = login(&state, "mgr1", "Test12345").await;
    assert_eq!(st, StatusCode::OK, "mgr1 登录");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &mgr,
            serde_json::json!({ "old": "Test12345", "new": "Pass123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "mgr1 首登改密");
    assert_eq!(
        select_book(&state, &mgr, "b1").await,
        StatusCode::OK,
        "mgr1 进账套"
    );

    // ② 权限调整仅管理员：mgr1 持有 UserManage 但非管理员
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/vic1",
            &mgr,
            serde_json::json!({ "role": "viewer" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "非管理员改他人角色应403"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &mgr,
            serde_json::json!({ "username": "x9", "display_name": "x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "非管理员建号应403");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/users/vic1", &mgr))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "非管理员删号应403");
    // 非授权字段仍可自助
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/mgr1",
            &mgr,
            serde_json::json!({ "display_name": "自改名" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "自助改显示名应放行");

    // 管理员放行对照 + update 路径互斥校验
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/vic1",
            &boss_sid,
            serde_json::json!({ "extra_perms": ["audit_log"] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员改权限应放行");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/vic1",
            &boss_sid,
            serde_json::json!({ "extra_perms": ["voucher_post"] }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "出纳+记账 update 路径应拒绝"
    );
    let s = body_string(resp).await;
    assert!(s.contains("会计与出纳"), "应提示互斥：{s}");

    // 收尾：管理员删号放行
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/users/vic1", &boss_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员删号应放行");
}

/// 管理员不能把另一个管理员拉进账套（口令为全局口令，管理员入套会形成提权链）
#[tokio::test]
async fn cannot_invite_platform_admin_into_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    // 开通第二个管理员
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "boss2", "display_name": "管理员二", "password": "Bb12345678", "is_admin": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通第二个管理员应成功");
    let (status, body) = create_book(&state, &admin_sid, "张记").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &admin_sid, &key).await, StatusCode::OK);

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &admin_sid,
            serde_json::json!({
                "username": "boss2", "display_name": "管理员二", "password": "Bb12345678",
                "role": "accountant", "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "不应允许邀请管理员进账套"
    );
}

/// 密钥签发/停用/删除：只有平台管理员能做，且签发响应才带明文
#[tokio::test]
async fn api_key_endpoints_require_platform_admin() {
    let (state, _bd, _dir) = test_state();

    // 未登录
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/api-keys")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // 已登录的普通会计（非管理员）。账号治理是二元的：先在**平台**开通账号，
    // 再在账套内邀请（/api/users 建的是账套内成员，平台没有同名账号会被拒）。
    let boss_sid0 = login(&state, "boss", "Admin!2026").await.1;
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss_sid0,
            serde_json::json!({ "username": "clerk", "display_name": "小会", "password": "Clerk2026a" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "开通平台账号应成功：{}",
        body_string(r).await
    );
    let (st, body) = create_book(&state, &boss_sid0, "权限用例").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let bkey = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &boss_sid0, &bkey).await, StatusCode::OK);
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss_sid0,
            serde_json::json!({
                "username": "clerk", "display_name": "小会",
                "password": "Clerk2026a", "role": "accountant", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "邀请成员应成功：{}",
        body_string(r).await
    );
    let (st, clerk_sid) = login(&state, "clerk", "Clerk2026a").await;
    assert_eq!(st, StatusCode::OK, "普通会计应能登录");
    // 平台新开的账号强制首登改密（账套里的 must_change_pwd=false 管不到这一层），
    // 不改的话后续请求全被 401 挡掉，用例测的就不是权限而是改密流程了。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &clerk_sid,
            serde_json::json!({ "old": "Clerk2026a", "new": "Clerk2026b" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::OK,
        "首登改密应成功：{}",
        body_string(r).await
    );
    let (st, clerk_sid) = login(&state, "clerk", "Clerk2026b").await;
    assert_eq!(st, StatusCode::OK, "改密后应能重新登录");

    // 确认它确实不是管理员（否则这条用例测的是别的东西）。
    // 用 /api/platform/users 而不是 /api/me：后者要 CurrentUser（需先选中账套），
    // 而 platform/users 只认 RealmUser，正好隔离出「平台管理员」这一层。
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/platform/users", &clerk_sid))
        .await
        .unwrap();
    let st = r.status();
    let me = body_string(r).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "clerk 不该能列出平台账号 —— 它必须确实是非管理员，本用例才测到「普通会计越权」：{me}"
    );

    for (method, uri) in [("GET", "/api/api-keys"), ("POST", "/api/api-keys")] {
        let r = handlers::router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::COOKIE, clerk_sid.clone())
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"name":"偷钥匙"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            r.status(),
            StatusCode::FORBIDDEN,
            "普通会计不该能 {method} {uri} —— 否则任何会计都能给外部系统开后门"
        );
    }

    // 管理员可以
    let boss_sid = login(&state, "boss", "Admin!2026").await.1;
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/api-keys")
                .header(header::COOKIE, boss_sid.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v = serde_json::from_str::<serde_json::Value>(&body_string(r).await).unwrap();
    assert_eq!(v["keys"].as_array().map(|a| a.len()), Some(0), "初始应为空");
}

/// 身份操作收归套内管理员：持有 UserManage 的**非管理员**不能重置口令 / 重置设备 /
/// 解锁停用账号（与建号/改权/删号的既有 admin 闸同口径）；套内管理员正常能力保留。
#[tokio::test]
async fn non_admin_usermanage_cannot_touch_identities() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;

    // sup9：会计 + 额外 user_manage（非管理员）；vic9：普通成员
    let _ = provision_plain_user(&state, &admin_sid, "sup9", "S912345678").await;
    let _ = provision_plain_user(&state, &admin_sid, "vic9", "V912345678").await;
    // 账套端点需要账套上下文：先进 b1 再邀请
    assert_eq!(select_book(&state, &admin_sid, "b1").await, StatusCode::OK);
    for (u, extra) in [("sup9", vec!["user_manage"]), ("vic9", vec![])] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &admin_sid,
                serde_json::json!({
                    "username": u, "display_name": u, "password": "Init@123456",
                    "role": "accountant", "must_change_pwd": false, "extra_perms": extra
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {u}");
    }
    let (st, sup_sid) = login(&state, "sup9", "S912345678x").await;
    assert_eq!(st, StatusCode::OK, "sup9 登录");
    let (st, vic_sid) = login(&state, "vic9", "V912345678x").await;
    assert_eq!(st, StatusCode::OK, "vic9 登录");
    assert_eq!(select_book(&state, &sup_sid, "b1").await, StatusCode::OK);
    assert_eq!(select_book(&state, &vic_sid, "b1").await, StatusCode::OK);

    // 非管理员 + UserManage：三项身份操作全拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users/vic9/reset-password",
            &sup_sid,
            serde_json::json!({ "new": "Hacked9999999" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "非管理员不能重置口令");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users/vic9/reset-device",
            &sup_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "非管理员不能重置设备");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users/vic9/unlock",
            &sup_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "非管理员不能解锁停用账号"
    );

    // 闸精准：非授权字段（显示名）仍可代改
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/vic9",
            &sup_sid,
            serde_json::json!({ "display_name": "改名九" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "显示名代改不受影响");

    // 套内管理员正常能力保留：boss 重置套内普通成员口令 → 成功
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users/vic9/reset-password",
            &admin_sid,
            serde_json::json!({ "new": "NewPass123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员重置套内成员应成功");
}

/// 跨租户口令接管回归（账号模型二元化）：普通账号不能自建账套、不能邀请成员、
/// 不能重置他人口令——历史漏洞（自建套→拉人→改全局口令→跨租户接管）的三个
/// 前置步骤在新模型下全部不存在。
#[tokio::test]
async fn book_admin_cannot_reset_global_password_of_other_book_owner() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let attacker = provision_plain_user(&state, &admin_sid, "att1", "At12345678").await;
    let victim = provision_plain_user(&state, &admin_sid, "vic1", "Vc12345678").await;

    // 前置一：普通账号不能自建账套（治理收编，攻击链第一步即断）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/books",
            &attacker,
            serde_json::json!({ "key": "", "company": "攻击者账套", "start_period": 202601 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通账号不能自建账套");

    // 管理员建套并邀请双方入套（均为会计成员）
    let (st, body) = create_book(&state, &admin_sid, "工作账套").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let key = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &admin_sid, &key).await, StatusCode::OK);
    for (u, name) in [("att1", "攻击者"), ("vic1", "受害者")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &admin_sid,
                serde_json::json!({
                    "username": u, "display_name": name, "password": "",
                    "role": "accountant", "must_change_pwd": false
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "管理员邀请 {u} 应成功");
    }
    assert_eq!(select_book(&state, &attacker, &key).await, StatusCode::OK);
    assert_eq!(select_book(&state, &victim, &key).await, StatusCode::OK);

    // 前置二：普通成员没有 UserManage，不能邀请他人进套
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &attacker,
            serde_json::json!({
                "username": "vic1", "display_name": "受害者", "password": "Init@123456",
                "role": "accountant", "must_change_pwd": false,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通成员不能邀请成员");

    // 前置三：普通成员无权重置他人的全局口令
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users/vic1/reset-password",
            &attacker,
            serde_json::json!({ "new": "Hacked123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "普通成员不得重置他人平台口令"
    );

    // 受害者原口令仍可登录（未被改写）
    let (st, _) = login(&state, "vic1", "Vc12345678x").await;
    assert_eq!(st, StatusCode::OK, "受害者口令不应被改写");
}

/// 安全边界：CSRF 跨站拒绝、退出登录、强制改密拦截、来源 IP 限流。
#[tokio::test]
async fn web_security_boundaries() {
    let (state, _bd, _dir) = test_state();

    // CSRF：跨站 Origin 拒绝（需带 Host 才能比较）
    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://evil.example")
        .body(Body::from(
            serde_json::json!({ "username": "x", "password": "y", "device_id": "d1" }).to_string(),
        ))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "跨站 Origin 应被拒");

    // CSRF：Sec-Fetch-Site 跨站拒绝
    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header("sec-fetch-site", "cross-site")
        .body(Body::from(
            serde_json::json!({ "username": "x", "password": "y", "device_id": "d1" }).to_string(),
        ))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "跨站请求应被拒");

    // 同源 Origin 放行（凭据错误应为 401，而不是被 CSRF 拦成 403）
    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "localhost")
        .header(header::ORIGIN, "http://localhost")
        .body(Body::from(
            serde_json::json!({
                "username": "ghost", "password": "bad",
                "device_id": "d1", "device_name": "测试机"
            })
            .to_string(),
        ))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "同源请求应进入登录校验"
    );

    // 退出登录后会话失效
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/logout", &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "退出后旧会话应失效"
    );

    // 强制改密：未改密前除改密/退出外一律 401
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "mc1", "display_name": "待改密", "password": "Init@123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let (st, mc_sid) = login(&state, "mc1", "Init@123456").await;
    assert_eq!(st, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &mc_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "未改密应被拦截");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &mc_sid,
            serde_json::json!({ "old": "Init@123456", "new": "Init654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改密应放行");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &mc_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改密后应可访问");

    // 来源 IP 限流：同一 XFF 连续失败 50 次后 429（用户名各不相同，避开账号维度）
    let mut last = StatusCode::UNAUTHORIZED;
    for i in 0..51 {
        let req = Request::builder()
            .method("POST")
            .uri("/api/login")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", "10.9.9.9")
            .body(Body::from(
                serde_json::json!({
                    "username": format!("ghost{i}"),
                    "password": "bad",
                    "device_id": "dev-xff",
                    "device_name": "测试机"
                })
                .to_string(),
            ))
            .unwrap();
        let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
        last = resp.status();
        if last == StatusCode::TOO_MANY_REQUESTS {
            break;
        }
    }
    assert_eq!(
        last,
        StatusCode::TOO_MANY_REQUESTS,
        "同一来源连续失败应被限流"
    );
}

/// 被邀请进账套（成员而非归属者）的普通用户，在 `GET /api/books` 与登录返回里都能看到该账套。
///
/// 修复前 `RealmDb::list_books_for` 只按 `owner_username` 过滤，被邀请成员登录后
/// 账套列表是空的（`select_book` 却认他），前端选账套页直接空掉。
#[tokio::test]
async fn invited_book_member_sees_book_in_list() {
    let (state, _bd, _dir) = test_state();
    let boss_sid = boss_in_b1(&state).await;

    // 平台开通一个非管理员账号（非归属者）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss_sid,
            serde_json::json!({ "username": "inv1", "display_name": "受邀会计", "password": "Init@123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通平台账号");
    // boss（b1 管理员）把他拉进 b1，角色只读、非归属者
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss_sid,
            serde_json::json!({
                "username": "inv1", "display_name": "受邀会计", "password": "",
                "role": "viewer", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "邀请为 b1 成员：{b}");

    // 首登强制改密 → 改密
    let (st, sid) = login(&state, "inv1", "Init@123456").await;
    assert_eq!(st, StatusCode::OK, "inv1 平台登录");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Init@123456", "new": "Pass123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "inv1 首登改密");

    // 登录返回的账套列表应含 b1
    let (st, sid2) = login(&state, "inv1", "Pass123456").await;
    assert_eq!(st, StatusCode::OK, "inv1 再次登录");
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/books")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "未带会话应 401");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &sid2))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "GET /api/books：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(
        r["books"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["key"] == "b1"),
        "被邀请成员应看到 b1：{b}"
    );

    // 选账套仍然可用（两条路径口径一致）
    assert_eq!(
        select_book(&state, &sid2, "b1").await,
        StatusCode::OK,
        "被邀请成员应能进入 b1"
    );
}

/// 下载导入模板也要登录 + 账簿报表权限（原先是唯一无鉴权提取器的处理器）。
#[tokio::test]
async fn import_template_requires_login_and_book() {
    let (state, _bd, _dir) = test_state();

    // ① 完全未登录（网关已拦，补一道断言确保不退化）
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/import/template?kind=account")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "未登录应 401");

    // ② 已登录但没选账套：原先照样返回模板，现在必须拒（CurrentUser 要求已选账套）
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/import/template?kind=account", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "未选账套不应能取模板"
    );

    // ③ 进了账套的只读成员仍可下载（Report 在 viewer 权限集里）
    let boss_sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss_sid,
            serde_json::json!({ "username": "tpl1", "display_name": "模板员", "password": "Init@123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通平台账号");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss_sid,
            serde_json::json!({
                "username": "tpl1", "display_name": "模板员", "password": "",
                "role": "viewer", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请为 b1 只读成员");
    let (st, tpl_sid) = login(&state, "tpl1", "Init@123456").await;
    assert_eq!(st, StatusCode::OK, "tpl1 登录");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &tpl_sid,
            serde_json::json!({ "old": "Init@123456", "new": "Pass123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "tpl1 首登改密");
    assert_eq!(
        select_book(&state, &tpl_sid, "b1").await,
        StatusCode::OK,
        "tpl1 进入 b1"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/import/template?kind=account", &tpl_sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "只读成员应能下模板：{b}");
    assert!(b.contains("库存现金"), "模板内容应照常返回：{b}");
}

/// ⑦ 账套层的「免首登改密」必须真正生效。
///
/// 实测：管理员在账套里建号时明确传 `must_change_pwd:false`（"口令是我设的，不用改"），
/// 用户仍被拦在「必须先改密」上。根因：登录门禁读的是**平台层**
/// `realm_user.must_change_pwd`（见 `post_login` 的 LoginResp），账套内那一位
/// 只影响账套内的记录，完全不参与登录判断。
#[tokio::test]
async fn book_level_no_password_change_flag_reaches_the_login_gate() {
    let (state, _bd, _dir) = test_state();
    let boss_sid = boss_in_b1(&state).await;

    // 平台开通账号（平台层默认 must_change_pwd = true）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss_sid,
            serde_json::json!({ "username": "acc7", "display_name": "会计", "password": "Init@123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通平台账号");
    assert!(
        state
            .realm
            .get_user("acc7")
            .unwrap()
            .unwrap()
            .must_change_pwd,
        "平台开号默认应要求首登改密（对照组：本测试要证明账套层能把它关掉）"
    );

    // 拉进账套，明确不要首登改密
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss_sid,
            serde_json::json!({
                "username": "acc7", "display_name": "会计", "password": "",
                "role": "accountant", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "拉进账套：{b}");

    // 关键断言：平台层的开关也被关掉了
    assert!(
        !state
            .realm
            .get_user("acc7")
            .unwrap()
            .unwrap()
            .must_change_pwd,
        "账套层设了免改密，平台层必须同步 —— 否则登录门禁读到的仍是 true"
    );

    // 且登录响应里不再要求改密
    let (st, _sid) = login(&state, "acc7", "Init@123456").await;
    assert_eq!(st, StatusCode::OK, "acc7 应能直接登录");
}

/// ⑤ 会计必须能读账套参数（否则看不到「本账套开了审核环节」这个事实）。
///
/// 实测：会计访问 `/api/options` 返回 403，前端 `state.bookOptions` 一直是 null，
/// 界面按「未开审核」处理，用户点记账被拒时才知道有审核环节。
#[tokio::test]
async fn accountant_can_read_book_options_to_see_audit_flag() {
    let (state, _bd, _dir) = test_state();
    let boss_sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss_sid,
            serde_json::json!({ "username": "acc8", "display_name": "会计", "password": "Init@123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通平台账号");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss_sid,
            serde_json::json!({
                "username": "acc8", "display_name": "会计", "password": "",
                "role": "accountant", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "拉进账套");
    let (_st, acc_sid) = login(&state, "acc8", "Init@123456").await;
    // 登录只到平台层，进账套要再选一次（没选账套时所有账套内端点返回 401「请先选择账套」）
    let st = select_book(&state, &acc_sid, "b1").await;
    assert_eq!(st, StatusCode::OK, "acc8 应能进入 b1");

    // 会计应能读到 enable_audit 这个开关
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &acc_sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "会计应能读账套参数：{b}");
    assert!(
        b.contains("enable_audit"),
        "响应里必须有 enable_audit，否则界面无从知道要不要先审核：{b}"
    );
    let opts: serde_json::Value = serde_json::from_str(&b).unwrap();

    // 但写仍然要 SysOption —— 读放宽不能顺带把写也放开。
    //
    // 用**刚读到的完整参数**原样 PUT，而不是随手编一个残缺对象：
    // axum 的 Json<T> 提取器先于 handler 里的权限检查执行，body 不合法会先拿到
    // 422，那时测到的是校验不是权限，白测。
    let mut attempt = opts.clone();
    attempt["company"] = serde_json::json!("改名试试");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &acc_sid, attempt))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "会计不得改账套参数（读放宽≠写放宽）：{b}"
    );
    // 顺带确认参数真没被改
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &boss_sid))
        .await
        .unwrap();
    let after: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_ne!(
        after["company"], "改名试试",
        "被拒的写入不该有任何效果：{after}"
    );
}
