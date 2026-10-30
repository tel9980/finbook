//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

#[tokio::test]
async fn normal_user_cannot_manage_platform_users() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "wang", "display_name": "王出纳", "password": "Wang123456" }),
        ))
        .await
        .unwrap();
    let (_, sid) = login(&state, "wang", "Wang123456").await;
    // 首登强制改密后再访问（强制改密期间会被 401 拦截）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Wang123456", "new": "Wang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/platform/users", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "普通用户不应能管平台账号"
    );
}

#[tokio::test]
async fn platform_user_lifecycle() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    // 开通 → 停用 → 重置口令
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "emp", "display_name": "员工", "password": "Emp123456" }),
        ))
        .await
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users/emp/reset-password",
            &admin_sid,
            serde_json::json!({ "new": "Emp999999" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重置口令应成功");
    // 重置后旧口令不可用、新口令可登录
    let (status, _) = login(&state, "emp", "Emp123456").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "旧口令应失效");
    // 重置口令清除了 must_change_pwd，可直接登录
    let (status, sid) = login(&state, "emp", "Emp999999").await;
    assert_eq!(status, StatusCode::OK, "新口令应可登录");
    // 删除自己应被拒绝
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/boss")
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "删除自己应被拒绝");
    drop(sid);
}

/// P0：Web 平台安全中心——口令策略可配（值域校验）+ 登录审计 + 持久锁定/解锁。
#[tokio::test]
async fn web_security_center() {
    let (state, _bd, _dir) = test_state();
    let (_, boss_sid) = login(&state, "boss", "Admin!2026").await;

    // 默认策略（与桌面默认一致）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/security/policy", &boss_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let p: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(p["min_len"], 8);
    assert_eq!(p["max_fail"], 5);

    // 保存自定义策略（max_fail=3 便于测锁定）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/security/policy",
            &boss_sid,
            serde_json::json!({
                "min_len": 10, "need_letter": true, "need_digit": true, "need_symbol": true,
                "max_age_days": 90, "max_fail": 3, "lock_minutes": 15, "idle_minutes": 30
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存自定义策略");
    // 回读生效
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/security/policy", &boss_sid))
        .await
        .unwrap();
    let p: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(p["min_len"], 10);
    assert_eq!(p["max_fail"], 3);
    // 非法值 400（min_len/max_fail 超界）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/security/policy",
            &boss_sid,
            serde_json::json!({
                "min_len": 0, "need_letter": true, "need_digit": true, "need_symbol": false,
                "max_age_days": 90, "max_fail": 0, "lock_minutes": 15, "idle_minutes": 30
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非法策略应 400");

    // 开一个测试账号
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss_sid,
            serde_json::json!({ "username": "sec1", "display_name": "安全测试", "password": "Init@123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通 sec1");

    // 连续 3 次错 → 触发持久锁（策略 max_fail=3）
    for i in 0..3 {
        let (status, _) = login_with_device(&state, "sec1", "WrongPass123", "dev-sec1-0001").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "第 {} 次失败应 401",
            i + 1
        );
    }
    // 第 4 次即使口令正确也被锁 → 429
    let (status, _) = login_with_device(&state, "sec1", "Init@123456", "dev-sec1-0001").await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "锁定后正确口令也应 429"
    );

    // 登录审计：sec1 的失败记录齐全
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/security/login-attempts?username=sec1&limit=50",
            &boss_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let items = r["items"].as_array().unwrap();
    assert!(items.len() >= 3, "审计应含 3 次失败记录：{}", items.len());
    assert!(
        items.iter().all(|x| x["ok"] == false),
        "锁定前应全为失败记录"
    );

    // 锁定名单可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/security/locked-users", &boss_sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["username"] == "sec1"),
        "锁定名单应含 sec1"
    );

    // 解锁 → 正确口令可登录
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/security/unlock",
            &boss_sid,
            serde_json::json!({ "username": "sec1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "解锁");
    let (status, sec1_sid) =
        login_with_device(&state, "sec1", "Init@123456", "dev-sec1-0001").await;
    assert_eq!(status, StatusCode::OK, "解锁后应能登录");

    // 审计含本次成功记录
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/security/login-attempts?username=sec1",
            &boss_sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["ok"] == true),
        "应有成功登录记录"
    );

    // 非管理员不得读策略/审计（先完成首登改密，否则会先被强制改密拦截为 401）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sec1_sid,
            serde_json::json!({ "old": "Init@123456", "new": "Sec1@pass99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "首登改密应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/security/policy", &sec1_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "非管理员读策略应 403");

    // 解锁不存在的账号 → 404
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/security/unlock",
            &boss_sid,
            serde_json::json!({ "username": "ghost99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "解锁陌生账号应 404");
}

/// 平台经营总览：跨账套 × 各业务域可视化（对标金蝶星空云「多组织集团管控视图」）。
///
/// 锁三件事：① 权限闸门（仅平台管理员）② 多账套聚合与 `books=` 过滤
/// ③ **单套打不开不影响整体** —— 整体 500 会让管理员看不到其他账套的真实状况。
#[tokio::test]
async fn platform_overview_aggregates_and_survives_broken_book() {
    let (state, books_dir, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;

    // 第二个账套
    let (status, body) = create_book(&state, &admin_sid, "张记").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let zk = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();

    // --- ① 权限闸门：非平台管理员一律 403 ---
    let plain_sid = provision_plain_user(&state, &admin_sid, "zhangy", "Zy12345678").await;
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/platform/overview", &plain_sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "非管理员不得看平台总览");

    // --- ② 多账套聚合 ---
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/platform/overview", &admin_sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let ov: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(ov["book_count"], 2, "应看到两个账套：{ov}");
    assert_eq!(ov["failed"], 0, "两个账套都应打得开");
    let books = ov["books"].as_array().unwrap();
    assert_eq!(books.len(), 2);
    for b in books {
        assert_eq!(b["ok"], true, "账套 {} 应正常：{b}", b["key"]);
        // 每个账套都要出业务域卡片，且带可聚合的数值
        let cards = b["cards"].as_array().unwrap();
        assert!(!cards.is_empty(), "账套 {} 没有任何业务卡片", b["key"]);
        for c in cards {
            assert!(c["domain"].is_string() && c["label"].is_string());
            assert!(c["num"].is_f64() || c["num"].is_i64(), "卡片缺少数值：{c}");
        }
        // 业务域覆盖：凭证/资金/销售/采购/仓管/生产/成本/报销/审批
        let doms: std::collections::HashSet<&str> =
            cards.iter().filter_map(|c| c["domain"].as_str()).collect();
        for want in ["凭证", "资金", "销售", "采购", "仓管", "生产", "成本"] {
            assert!(
                doms.contains(want),
                "账套 {} 缺业务域「{want}」：{doms:?}",
                b["key"]
            );
        }
    }
    // 汇总与逐套同源：每个 (域,指标) 的合计 == 各套之和
    let totals = ov["totals"].as_array().unwrap();
    assert!(!totals.is_empty(), "跨账套汇总不应为空");
    for t in totals {
        let (d, k) = (t["domain"].as_str().unwrap(), t["key"].as_str().unwrap());
        let sum: f64 = books
            .iter()
            .flat_map(|b| b["cards"].as_array().unwrap())
            .filter(|c| c["domain"] == d && c["key"] == k)
            .map(|c| c["num"].as_f64().unwrap_or(0.0))
            .sum();
        let got = t["value"].as_f64().unwrap_or(0.0);
        assert!(
            (sum - got).abs() < 0.02,
            "汇总「{d}/{k}」= {got}，但各套相加 = {sum}"
        );
    }

    // books= 过滤：只统计指定账套
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/platform/overview?books={zk}"),
            &admin_sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let one: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(one["book_count"], 1, "过滤后应只剩 1 个账套：{one}");
    assert_eq!(one["books"][0]["key"], zk.as_str());

    // --- ③ 单套文件损坏：整体仍 200，坏套标记原因，好套数据照常 ---
    std::fs::write(
        books_dir.join(format!("{zk}.fbk")),
        b"this is not a sqlite database",
    )
    .expect("写坏账套文件");
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/platform/overview", &admin_sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "一个账套坏掉不应让整体 500");
    let mixed: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(mixed["failed"], 1, "应记 1 个取数失败：{mixed}");
    let bad = mixed["books"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["key"] == zk.as_str())
        .unwrap();
    assert_eq!(bad["ok"], false);
    assert!(
        !bad["error"].as_str().unwrap_or("").is_empty(),
        "坏套必须带原因，不能静默"
    );
    let good = mixed["books"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["key"] == "b1")
        .unwrap();
    assert_eq!(good["ok"], true, "好套应照常出数：{good}");
    assert!(!good["cards"].as_array().unwrap().is_empty());
}
