//! finweb 关键 API 集成测试（多租户模型）
//!
//! 覆盖：平台身份库引导、平台登录、普通用户自建账套、归属隔离、
//! 成员协作（邀请账套内成员）、管理员跨账套查看（不留痕迹）、
//! 平台账号管理权限边界、「一人一机」设备绑定与重置、导入等核心业务链路。
//! 这是「多租户 + 归属隔离 + 设备绑定」部署级特性的回归防线。

// 集成测试共用助手（test_state / boss_in_b1 / authed_* / body_string / money_num /
// create_book / login / select_book）。2026-10-01 从本文件提取，供 customers.rs 复用。
mod common;
use common::*;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use fincore::BookOptions;
use tower::ServiceExt;

use finweb::handlers;
use finweb::state::WebState;

/// 把 JSON 包成 POST 请求
fn post_json(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// 从登录响应的 set-cookie 里取出 `finbook_sid=xxx` 段
fn sid_from(resp: &axum::http::Response<Body>) -> String {
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("登录应返回 set-cookie")
        .to_str()
        .unwrap();
    cookie.split(';').next().unwrap().to_string()
}

async fn login_with_device(
    state: &Arc<WebState>,
    username: &str,
    password: &str,
    device_id: &str,
) -> (StatusCode, String) {
    let resp = handlers::router(state.clone())
        .oneshot(post_json(
            "/api/login",
            serde_json::json!({
                "username": username,
                "password": password,
                "device_id": device_id,
                "device_name": "测试机",
            }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let sid = if status.is_success() {
        sid_from(&resp)
    } else {
        String::new()
    };
    (status, sid)
}

/// 带 Bearer 密钥的 GET 请求（开放 API 用；secret 传空串表示不带 Authorization）
fn bearer_get(uri: &str, secret: &str) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri(uri);
    if !secret.is_empty() {
        b = b.header(header::AUTHORIZATION, format!("Bearer {secret}"));
    }
    b.body(Body::empty()).unwrap()
}

/// 平台级密钥 + X-Book-Key 头
fn bearer_get_for_book(uri: &str, secret: &str, book_key: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {secret}"))
        .header("x-book-key", book_key)
        .body(Body::empty())
        .unwrap()
}

/// 带 sid 的 PUT JSON 请求
fn authed_put(uri: &str, sid: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, sid)
        .body(Body::from(body.to_string()))
        .unwrap()
}

// ---------------------------------------------------------------------------
// 基础
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_endpoint() {
    let (state, _bd, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "ok");
}

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
async fn wrong_password_rejected() {
    let (state, _bd, _dir) = test_state();
    let (status, _) = login(&state, "boss", "WrongPass123!").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn setup_status_after_bootstrap() {
    let (state, _bd, _dir) = test_state();
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/setup/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"admin_set\":true"), "引导后应有管理员：{s}");
}

// ---------------------------------------------------------------------------
// 平台登录 + 归属可见性
// ---------------------------------------------------------------------------

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
async fn owner_enters_own_book_and_sees_company() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let status = select_book(&state, &sid, "b1").await;
    assert_eq!(status, StatusCode::OK, "进入自己的账套应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let s = body_string(resp).await;
    assert!(s.contains("\"vouchers\""), "仪表盘应正常返回：{s}");
}

// ---------------------------------------------------------------------------
// 自建账套 + 隔离
// ---------------------------------------------------------------------------

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
async fn normal_user_cannot_enter_others_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    // 开通 wang（无账套）
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "wang", "display_name": "王出纳", "password": "Wang123456" }),
        ))
        .await
        .unwrap();
    let (_, sid) = login(&state, "wang", "Wang123456").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Wang123456", "new": "Wang654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 试图进入 boss 的账套 → 403
    let status = select_book(&state, &sid, "b1").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "普通用户不应能进入他人账套");

    // 自己的账套列表应为空
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("\"books\":[]"), "无账套用户列表应为空：{s}");
}

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

// ---------------------------------------------------------------------------
// 管理员跨账套 + 平台账号管理
// ---------------------------------------------------------------------------

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

#[tokio::test]
async fn deleting_book_unblocks_user_deletion() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &admin_sid,
            serde_json::json!({ "username": "zhang", "display_name": "张会计", "password": "Zhang123456" }),
        ))
        .await
        .unwrap();
    let (_, sid) = login(&state, "zhang", "Zhang123456").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": "Zhang123456", "new": "Zhang654321" }),
        ))
        .await
        .unwrap();
    // 自建账套（治理收归：helper 由管理员代建并邀请 zhang 入套）
    let (_, body) = create_book(&state, &sid, "张记贸易").await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().unwrap().to_string();
    // 名下有账套的账号不能被删（新模型下账套只归管理员——删管理员本人被自身保护拦下）
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
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "不能删除当前登录的管理员"
    );

    // 新模型：账套归管理员——普通成员无权删套，管理员可删自己的套
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/books/{}", key))
                .header(header::COOKIE, &sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "普通成员不能删除账套");
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/books/{}", key))
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员应能删除账套");
    // 该套应已从列表消失（管理员名下仍有 b1 等既有账套）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &admin_sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(!s.contains(&key), "删除后列表不应再含该套：{s}");
    // 此时账号可删
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/zhang")
                .header(header::COOKIE, &admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "账套清空后应能删除账号");
}

#[tokio::test]
async fn last_admin_cannot_be_removed() {
    let (state, _bd, _dir) = test_state();
    // 开通第二个管理员，用它来删 boss（boss 不是其当前会话账号）
    let (_, boss_sid) = login(&state, "boss", "Admin!2026").await;
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss_sid,
            serde_json::json!({ "username": "m2", "display_name": "管理员二", "password": "M2aa123456", "is_admin": true }),
        ))
        .await
        .unwrap();
    let (_, m2_sid) = login(&state, "m2", "M2aa123456").await;
    // 首登强制改密
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &m2_sid,
            serde_json::json!({ "old": "M2aa123456", "new": "M2aa654321" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "m2 改密应成功");
    // 平台管理员可删除他人账套（boss 名下有预置账套 b1，先清掉才能删账号）
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/books/b1")
                .header(header::COOKIE, &m2_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "平台管理员应能删除他人账套");
    // 两个管理员时可删 boss
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/boss")
                .header(header::COOKIE, &m2_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "存在两个管理员时可删其一");
    // 只剩 m2 一个管理员时，删除自己应被拒绝
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/platform/users/m2")
                .header(header::COOKIE, &m2_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "最后一个管理员不可删除"
    );
}

/// 平台层设备绑定：一个账号最多绑 **2 台**，第三台被拒，管理员重置后可重新绑定。
///
/// 为什么不是 1 台：真实场景里同一个人两台机器是常态（公司台式机 + 家里笔记本）。
/// 锁死 1 台的结果是用户隔几天就要找管理员重置一次 —— 一个安全功能如果天天要人工
/// 介入，实际效果就是被绕过。
///
/// 为什么不是不限：上限是这个功能**全部**的控制力来源。不限台数等于没有绑定，
/// 口令泄露后攻击者从任何机器都能登。
///
/// 原测试断言的是「第二台被拒」（一人一机），已随语义变更改写。
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

    // 第二台设备**应当能登**（上限 2 台）
    let (status, s2) = login_with_device(&state, "emp", "Emp654321", "dev-B").await;
    assert_eq!(status, StatusCode::OK, "第二台设备应可登录并绑定");

    // 两台都在用：各自会话互不影响（这是「一人两机」的核心诉求）
    for (label, sid) in [("dev-A", &s1b), ("dev-B", &s2)] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get("/api/books", sid))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "{label} 的会话应仍然有效（两台设备互不踢下线）"
        );
    }

    // 第三台设备被拒：上限就是上限
    let (status, _) = login_with_device(&state, "emp", "Emp654321", "dev-C").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "第三台设备应被拒绝（上限 2 台）"
    );

    // 拒绝信息要说清是几台上限，而不是笼统的「已绑定其它设备」——
    // 用户看到「已绑定 2 台设备（上限 2 台）」才知道该找谁解绑
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "username": "emp", "password": "Emp654321",
                        "device_id": "dev-C", "device_name": "第三台"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_string(r).await;
    assert!(
        body.contains("2 台设备"),
        "提示要说明已绑台数与上限：{body}"
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

    // 重置后两台都能重新绑定（不是只放行一台）
    for dev in ["dev-A", "dev-B"] {
        let (status, _) = login_with_device(&state, "emp", "Emp654321", dev).await;
        assert_eq!(status, StatusCode::OK, "重置后 {dev} 应可重新绑定");
    }
    let (status, _) = login_with_device(&state, "emp", "Emp654321", "dev-C").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "重置后仍应保持 2 台上限");
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

// ---------------------------------------------------------------------------
// 业务回归（导入）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn import_run_kingdee_template_csv() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let status = select_book(&state, &sid, "b1").await;
    assert_eq!(status, StatusCode::OK);
    // 金蝶模板期初：科目编码,科目名称,方向,期初余额,累计借方,累计贷方
    let csv = "\u{feff}科目编码,科目名称,方向,期初余额,累计借方,累计贷方\n\
               1001,库存现金,借,10000,50000,30000\n\
               100201,银行存款-工行,贷,2000,0,2000\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "begin",
                "template": "kingdee",
                "text": csv,
                "mapping": {},
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "金蝶模板导入应成功");
    let s = body_string(resp).await;
    assert!(s.contains("\"ok\":2"), "应导入 2 条：{s}");
}

#[tokio::test]
async fn import_run_excel_file_upload() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let _ = select_book(&state, &sid, "b1").await;
    // 用 findb 的 fixture：用友模板期初表（Excel）
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../findb/tests/fixtures/yonyou_begin.xlsx"
    );
    let bytes = std::fs::read(fixture).expect("读取 fixture xlsx 失败");
    let b64 = base64_encode(&bytes);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "begin",
                "template": "yonyou",
                "text": "",
                "file": b64,
                "mapping": {},
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "Excel 上传导入应成功");
    let s = body_string(resp).await;
    assert!(s.contains("\"ok\":2"), "应导入 2 条：{s}");
}

/// 无依赖 base64 编码（测试辅助）
fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ===========================================================================
// 功能对齐 finui：科目 / 期初 / 日志 / 备份 / 参数 / 模板 / 档案 / 工资 / 报销
// ===========================================================================

/// 带 sid 的 DELETE 请求
fn authed_delete(uri: &str, sid: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(header::COOKIE, sid)
        .body(Body::empty())
        .unwrap()
}

/// 录一张借贷 100 元的两行凭证（借 debit_acc / 贷 1001）
async fn post_simple_voucher(state: &Arc<WebState>, sid: &str, debit_acc: &str) -> StatusCode {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-31", "word": "记",
                "no": 99, "attachments": 0, "memo": "测试凭证",
                "entries": [
                    { "line": 1, "account_code": debit_acc, "summary": "购入", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "购入", "debit": "0", "credit": "100" }
                ],
            }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    if !status.is_success() {
        let body = body_string(resp).await;
        panic!("录凭证失败 {status}：{body}");
    }
    status
}

#[tokio::test]
async fn accounts_crud_usage_guard_and_begin() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 新增科目（带辅助核算维度）
    let acc = |name: &str| {
        serde_json::json!({
            "account": {
                "code": "8888", "name": name, "category": "asset", "dir": "debit",
                "aux": 0, "unit": null, "currency": null, "has_qty": false,
                "is_cash": false, "is_bank": false, "cash_flow_item": null,
                "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
            },
            "aux_kinds": ["item", "supplier"]
        })
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, acc("原材料")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增科目应成功");

    // 重复编码 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, acc("重复")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复科目应拒绝");

    // 列表包含新科目，掩码 = item(32) | supplier(2) = 34
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/accounts", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("8888"), "科目列表应含 8888：{s}");
    let list: serde_json::Value = serde_json::from_str(&s).unwrap();
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["code"] == "8888")
        .expect("科目 8888 应存在");
    assert_eq!(row["aux"], 34, "辅助核算掩码应为 34");

    // 修改名称
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/accounts", &sid, acc("原材料改良")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 未使用 → 可删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/accounts/8888", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "未使用科目应可删除");

    // 建一个无辅助核算的科目，被凭证引用后 → 删除被拦截
    let plain = serde_json::json!({
        "account": {
            "code": "1409", "name": "测试物料", "category": "asset", "dir": "debit",
            "aux": 0, "unit": null, "currency": null, "has_qty": false,
            "is_cash": false, "is_bank": false, "cash_flow_item": null,
            "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
        },
        "aux_kinds": []
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/accounts", &sid, plain))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        post_simple_voucher(&state, &sid, "1409").await,
        StatusCode::OK
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/accounts/1409", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已用科目不可删除");

    // 期初录入 + 回读（贷方应为负数）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([
                { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "1000", "ac": "0", "qty": null },
                { "account_code": "2001", "dir": "credit", "yb": "3000", "ad": "0", "ac": "0", "qty": null }
            ]),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    let rows: serde_json::Value = serde_json::from_str(&s).unwrap();
    let arr = rows.as_array().unwrap();
    let cash = arr.iter().find(|r| r["account_code"] == "1001").unwrap();
    let loan = arr.iter().find(|r| r["account_code"] == "2001").unwrap();
    assert_eq!(
        cash["year_begin"].as_str().unwrap().parse::<f64>().unwrap(),
        5000.0,
        "借方期初应为正"
    );
    assert_eq!(
        loan["year_begin"].as_str().unwrap().parse::<f64>().unwrap(),
        -3000.0,
        "贷方期初应为负"
    );

    // 操作日志可查且非空（含科目操作记录）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/logs?limit=50&q=%E7%A7%91%E7%9B%AE", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    let logs: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(!logs.as_array().unwrap().is_empty(), "日志搜索应非空：{s}");

    // 备份 → 列表落地 → 恢复
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/backups", &sid, serde_json::json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "备份应成功");
    let s = body_string(resp).await;
    let name: String = serde_json::from_str::<serde_json::Value>(&s).unwrap()["name"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains(&name), "备份列表应包含 {name}：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/restore",
            &sid,
            serde_json::json!({ "file": name }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "恢复应成功");
    // 路径穿越必须被拒绝（file_name 归一化后落入 backups 内且不存在 → 404）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/restore",
            &sid,
            serde_json::json!({ "file": "../../etc/passwd" }),
        ))
        .await
        .unwrap();
    assert!(
        !resp.status().is_success(),
        "路径穿越必须被拒绝：{}",
        resp.status()
    );

    // 账套参数读写
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    let mut opts: serde_json::Value = serde_json::from_str(&s).unwrap();
    opts["company"] = serde_json::json!("改名公司");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("改名公司"), "参数应已更新：{s}");
}

#[tokio::test]
async fn templates_and_aux_lifecycle() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 模板 CRUD
    let tpl = serde_json::json!({
        "id": 0, "name": "月度计提房租", "memo": "每月房租",
        "entries": [
            { "summary": "计提房租", "account_code": "660201", "dir": "debit", "amount": "5000", "aux": {} },
            { "summary": "计提房租", "account_code": "221101", "dir": "credit", "amount": "5000", "aux": {} }
        ],
        "freq": "monthly", "start_period": 202601, "end_period": null,
        "last_period": null, "active": true
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/templates", &sid, tpl.clone()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建模板应成功");
    let id: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();

    // 到期列表应含月度模板
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/templates/due?period=202601", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("月度计提房租"), "本期到期应包含模板：{s}");

    // 生成凭证 → 回写 last_period，本期到期列表不再包含
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/templates/{id}/generate"),
            &sid,
            serde_json::json!({ "period": 202601, "date": "2026-01-31" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "模板生成凭证应成功");
    let s = body_string(resp).await;
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&s).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert!(vid > 0, "应返回凭证 id：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/templates/due?period=202601", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(
        !s.contains("月度计提房租"),
        "生成后本期到期列表应移除该模板（last_period 已推进）：{s}"
    );

    // 修改
    let mut tpl2 = tpl.clone();
    tpl2["name"] = serde_json::json!("月度计提房租(改)");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(&format!("/api/templates/{id}"), &sid, tpl2))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/templates/{id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 辅助档案 CRUD
    let ent = serde_json::json!({
        "id": 0, "kind": "customer", "code": "C001", "name": "客户一",
        "parent_code": null, "disabled": false, "props": {}, "memo": ""
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/aux", &sid, ent))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建档案应成功");
    let aid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/aux?kind=customer", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("C001"), "客户档案应存在：{s}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/aux/{aid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn payroll_lifecycle_with_tax_and_vouchers() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 前置：建职员档案
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "employee", "code": "E001", "name": "张三",
                "parent_code": null, "disabled": false, "props": {}, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 录工资（后端算税）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202601",
            &sid,
            serde_json::json!({
                "employee": "E001", "dept": "销售部", "gross": "10000",
                "social": "800", "housing": "500", "deduction": "0",
                "additional": "1000", "social_co": "2000", "housing_co": "500", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "录工资应成功");
    let s = body_string(resp).await;
    let row: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(
        row["tax_base"].as_str().unwrap().parse::<f64>().unwrap() > 0.0,
        "计税基数应为正：{s}"
    );
    let net: f64 = row["net"].as_str().unwrap().parse().unwrap();
    assert!(net > 0.0 && net < 10000.0, "实发应在 0 与应发之间：{net}");

    // 列表回读
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll?period=202601", &sid))
        .await
        .unwrap();
    assert!(body_string(resp).await.contains("E001"), "工资表应含 E001");

    // 累计（202602 的 ytd 应含 202601 的 10000）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/payroll/ytd?period=202602&employee=E001",
            &sid,
        ))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("\"income\":\"10000"), "累计收入应为 10000：{s}");

    // 计提凭证 → 工资行挂上凭证 → 删除被拦截
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/accrue?period=202601",
            &sid,
            serde_json::json!({
                "date": "2026-01-31", "expense": "660201", "wage_payable": "221101",
                "social_payable": "221103", "housing_payable": "221104"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提凭证应成功");
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    assert!(vid > 0, "应返回凭证 id");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll?period=202601", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains(&vid.to_string()), "工资行应挂上凭证 id：{s}");
    let pid: i64 = {
        let rows: serde_json::Value = serde_json::from_str(&s).unwrap();
        rows[0]["id"].as_i64().unwrap()
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/payroll/{pid}"), &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已挂凭证的工资行不可删除"
    );

    // 发放凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "2026-01-31", "payable_account": "221101", "bank_account": "100201",
                "tax_account": "222103", "social_account": "2241"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发放凭证应成功");

    // 社保缴纳凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/social-pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "2026-01-31", "social_payable": "221103", "housing_payable": "221104",
                "personal_payable": "2241", "bank_account": "100201"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "社保缴纳凭证应成功");
}

#[tokio::test]
async fn claim_lifecycle_to_voucher() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 新增草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claims",
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-15", "applicant": "张三",
                "dept": "销售部", "reason": "差旅费", "amount": "500",
                // 明细备注留空：生成凭证时应回退用事由做摘要，不能因摘要为空被拒
                "items": [ { "expense_account": "660201", "amount": "500", "memo": "" } ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建报销单应成功");
    let s = body_string(resp).await;
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let cid = v["id"].as_i64().unwrap();
    assert_eq!(v["no"], "BX202601-001", "单据号应自增：{s}");

    // 草稿期可改
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/claims/{cid}"),
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-16", "applicant": "张三",
                "dept": "销售部", "reason": "差旅费(改)", "amount": "600",
                "items": [ { "expense_account": "660201", "amount": "600", "memo": "机票" } ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿应可修改");

    // 提交 → 审批 → 支付
    for st in ["submitted", "approved", "paid"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/claims/{cid}/transition"),
                &sid,
                serde_json::json!({ "status": st }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "流转到 {st} 应成功");
    }

    // 支付即自动落账：已付列表应带凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claims?period=202601&status=paid", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(
        !s.contains("\"voucher_id\":null"),
        "支付应自动生成付款凭证：{s}"
    );

    // 支付后不可再改内容
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/claims/{cid}"),
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-16", "applicant": "张三",
                "dept": "销售部", "reason": "再改", "amount": "600", "items": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已支付单据不可修改");

    // 生成凭证（明细合计与金额一致）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/claims/{cid}/voucher"),
            &sid,
            serde_json::json!({ "pay_account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "生成凭证应成功");
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    assert!(vid > 0);

    // 幂等：重复请求返回同一张凭证（支付时已自动出账，手动请求不再新增）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/claims/{cid}/voucher"),
            &sid,
            serde_json::json!({ "pay_account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重复请求应幂等返回");
    let vid2: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    assert_eq!(vid2, vid, "幂等返回同一张凭证");

    // 已生成凭证 → 删除被拦截
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/claims/{cid}"), &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已挂凭证的报销单不可删除"
    );

    // 列表状态过滤
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claims?period=202601&status=paid", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("BX202601-001"), "已付列表应含该单：{s}");

    // 空状态 = 全部（UI 下拉默认空串），不能退化成只看草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claims?period=202601&status=", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("BX202601-001"), "空状态应返回全部单据：{s}");
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

/// P0：出纳交接班——交班快照（现金结存/在库票据/未日清）+ 接班确认（不能自确认）。
#[tokio::test]
async fn cash_shift_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    // 当期与测试日期
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 造一笔现金收款（借 1001 500 / 贷 2001 500）并记账 → 快照应有 500
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记",
                "no": 90, "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "shift-in", "debit": "500", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "shift-in", "debit": "0", "credit": "500" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造现金凭证");
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账");

    // 交班单 A：快照 + 自己不能确认 → 取消
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/shifts",
            &sid,
            serde_json::json!({ "date": d15, "to_user": "sh1", "memo": "晚班交接" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "创建交班单 A");
    let a: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let aid = a["id"].as_i64().unwrap();
    assert_eq!(
        a["shift"]["cash_balance"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap(),
        500.0,
        "现金结存快照：{a}"
    );
    assert_eq!(a["shift"]["status"], "open");
    assert!(
        a["shift"]["uncleared"].as_i64().unwrap() >= 1,
        "未日清账户数应 ≥1：{a}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/shifts/{aid}/confirm"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "交班人不能自确认");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/shifts/{aid}/cancel"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消交班单 A");

    // 出纳 sh1（平台开号 → 首登改密 → 加入账套 cashier）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({ "username": "sh1", "display_name": "出纳一", "password": "Sh1@pass99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "平台开号 sh1");
    let (_, sh1_sid) = login(&state, "sh1", "Sh1@pass99").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sh1_sid,
            serde_json::json!({ "old": "Sh1@pass99", "new": "Sh1@pass99x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "sh1 首登改密");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "sh1", "display_name": "出纳一", "password": "",
                "role": "cashier", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请 sh1 入套为出纳");
    assert_eq!(select_book(&state, &sh1_sid, "b1").await, StatusCode::OK);

    // 交班单 B：boss 交班 → sh1 确认 → 重复确认 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/shifts",
            &sid,
            serde_json::json!({ "date": d15, "to_user": "sh1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "创建交班单 B");
    let b: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bid = b["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/shifts/{bid}/confirm"),
            &sh1_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "接班人确认");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/shifts/{bid}/confirm"),
            &sh1_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复确认应 400");

    // 列表：B 已确认（确认人 sh1），A 已取消
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/shifts", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    let br = rows
        .iter()
        .find(|x| x["id"] == bid)
        .expect("列表应含交班单 B");
    assert_eq!(br["status"], "confirmed");
    assert_eq!(br["confirmed_by"], "sh1");
    let ar = rows
        .iter()
        .find(|x| x["id"] == aid)
        .expect("列表应含交班单 A");
    assert_eq!(ar["status"], "cancelled");
}

/// P0：固定资产三件套——变更历史 + 总账对账 + 处置清理凭证。
#[tokio::test]
async fn asset_p0_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());
    let num = |v: &serde_json::Value| -> f64 {
        v.as_str().unwrap().replace(',', "").parse::<f64>().unwrap()
    };

    // 建卡：160101 / 1602 / 660201，原值 12000，36 期，残值 5%
    let card = serde_json::json!({
        "id": 0, "code": "P0A01", "name": "测试设备", "category": "电子设备", "spec": "",
        "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
        "expense_account": "660201", "original_value": "12000", "residual_rate": "5",
        "life_months": 36, "method": "straight", "start_period": cur_ymm, "memo": ""
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/assets", &sid, card.clone()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建卡");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 资本化入账：借 160101 / 贷 1002（对账基准）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 91,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "160101", "summary": "购入资产", "debit": "12000", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "购入资产", "debit": "0", "credit": "12000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "资本化凭证：{body}");
    let cap_vid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cap_vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资本化凭证记账");

    // 变更历史：改名 + 换部门 → 至少 2 条字段级记录
    let mut edited = card.clone();
    edited["id"] = serde_json::json!(id);
    edited["name"] = serde_json::json!("测试设备-改");
    edited["dept"] = serde_json::json!("生产部");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(&format!("/api/assets/{id}"), &sid, edited))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改卡");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/assets/{id}/changes"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let fields: Vec<String> = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["field"].as_str().unwrap().to_string())
        .collect();
    assert!(
        fields.contains(&"名称".to_string()),
        "应记录名称变更：{fields:?}"
    );
    assert!(
        fields.contains(&"使用部门".to_string()),
        "应记录部门变更：{fields:?}"
    );

    // 计提折旧（草稿）→ 记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/depreciate",
            &sid,
            serde_json::json!({ "ymm": cur_ymm }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提折旧");
    let dep: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dep_vid = dep["voucher_id"].as_i64().expect("折旧凭证 id");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{dep_vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "折旧凭证记账");

    // 对账（清理前）：原值 12000/12000、累计折旧 316.67/316.67，双向对平
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/assets/gl-reconcile?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        (num(&r["cost_asset"]) - 12000.0).abs() < 0.005,
        "原值资产侧：{r}"
    );
    assert!(
        (num(&r["cost_gl"]) - 12000.0).abs() < 0.005,
        "原值总账侧：{r}"
    );
    assert!(num(&r["cost_diff"]).abs() < 0.005, "原值应平：{r}");
    assert!(
        (num(&r["dep_asset"]) - 316.67).abs() < 0.005,
        "累计折旧资产侧：{r}"
    );
    assert!(
        (num(&r["dep_gl"]) - 316.67).abs() < 0.005,
        "累计折旧总账侧：{r}"
    );
    assert!(num(&r["dep_diff"]).abs() < 0.005, "折旧应平：{r}");

    // 清理 → 转销凭证（借 1602 + 借 1606 净值 / 贷 160101）→ 记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/{id}/dispose"),
            &sid,
            serde_json::json!({ "ymm": cur_ymm, "amount": "1000" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "清理");
    let dis: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dis_vid = dis["voucher_id"].as_i64().expect("清理转销凭证 id");
    assert!(dis_vid > 0);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{dis_vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "转销凭证记账");

    // 对账（清理后）：资产侧与总账侧同时归零
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/assets/gl-reconcile?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        num(&r["cost_asset"]).abs() < 0.005,
        "清理后原值资产侧=0：{r}"
    );
    assert!(num(&r["cost_gl"]).abs() < 0.005, "清理后原值总账侧=0：{r}");
    assert!(
        num(&r["dep_asset"]).abs() < 0.005,
        "清理后折旧资产侧=0：{r}"
    );
    assert!(num(&r["dep_gl"]).abs() < 0.005, "清理后折旧总账侧=0：{r}");

    // 重复清理 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/{id}/dispose"),
            &sid,
            serde_json::json!({ "ymm": cur_ymm, "amount": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复清理应 400");
}

/// P0：数据范围全量接入——科目区间过滤账簿/报表/期初，范围外直接 403。
#[tokio::test]
async fn data_scope_ledgers() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 造凭证：借 1001 100 / 贷 2001 100 → 记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 92,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "范围测试", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "范围测试", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造范围测试凭证");
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账");

    // sc1：平台开号 → 首登改密 → 入套为会计 → 科目范围 1001..1001
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({ "username": "sc1", "display_name": "范围会计", "password": "Sc1@pass99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "平台开号 sc1");
    let (_, sc1_sid) = login(&state, "sc1", "Sc1@pass99").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sc1_sid,
            serde_json::json!({ "old": "Sc1@pass99", "new": "Sc1@pass99x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "sc1 首登改密");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "sc1", "display_name": "范围会计", "password": "",
                "role": "accountant", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请 sc1 入套");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/sc1",
            &sid,
            serde_json::json!({ "data_scope": { "account_from": "1001", "account_to": "1001" } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "设置科目范围");
    assert_eq!(select_book(&state, &sc1_sid, "b1").await, StatusCode::OK);

    // 试算平衡：只含 1001，不含 2001
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/trial-balance?from={cur_ymm}&to={cur_ymm}"),
            &sc1_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let codes: Vec<String> = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["account_code"].as_str().unwrap().to_string())
        .collect();
    assert!(
        codes.iter().all(|c| c.starts_with("1001")),
        "范围外科目不应出现：{codes:?}"
    );
    assert!(
        !codes.iter().any(|c| c == "2001"),
        "2001 应不可见：{codes:?}"
    );

    // 明细账：1001 可看；2001 → 403
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/ledger/journal?code=1001&from={cur_ymm}&to={cur_ymm}"),
            &sc1_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "范围内科目可查");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/ledger/journal?code=2001&from={cur_ymm}&to={cur_ymm}"),
            &sc1_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "范围外科目应 403");

    // 数字钻取：2001 → 403
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/account-detail?account=2001&from={cur_ymm}&to={cur_ymm}"),
            &sc1_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "范围外钻取应 403");

    // 期初列表：范围外不可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sc1_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        rows.iter()
            .all(|x| x["account_code"].as_str().unwrap_or("").starts_with("1001")),
        "期初列表不应含范围外科目"
    );

    // 管理员视角：2001 正常可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/trial-balance?from={cur_ymm}&to={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["account_code"] == "2001"),
        "管理员应可见 2001"
    );
}

/// P1：MRP 采购建议下推请购——仅采购行可推、幂等、生产行拒绝。
#[tokio::test]
async fn mrp_purchase_push() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 无 BOM 物料 → 采购建议
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mrp/run",
            &sid,
            serde_json::json!({ "demands": [{ "item_code": "140301", "qty": "6", "source": "手工" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "MRP 运行");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_code"] == "140301")
        .expect("140301 行");
    assert_eq!(row["action"], "purchase", "无 BOM 应为采购建议：{r}");
    let rid = row["id"].as_i64().unwrap();

    // 下推请购 → 200 + 单号
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mrp/{rid}/to-req"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "下推请购：{b}");
    let pr: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(pr["req_id"].as_i64().unwrap() > 0, "应返回请购单 id：{pr}");
    let no = pr["no"].as_str().unwrap().to_string();
    assert!(!no.is_empty(), "应返回请购单号");

    // 重复下推 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mrp/{rid}/to-req"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复下推应 400");

    // 未知 id → 404
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mrp/999999/to-req",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "未知 MRP 行应 404");

    // 建 BOM 后：140501=生产建议，生产行下推请购 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建 BOM");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mrp/run",
            &sid,
            serde_json::json!({ "demands": [{ "item_code": "140501", "qty": "2", "source": "手工" }] }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let prod = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_code"] == "140501")
        .expect("140501 行");
    assert_eq!(prod["action"], "produce", "有 BOM 应为生产建议：{r}");
    let pid = prod["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mrp/{pid}/to-req"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "生产建议不可下推请购"
    );
}

/// 发货/退货的收入凭证与库存流水必须**同一事务**。
/// 回归：早先 `add_so_shipment` 先调 `so_income_voucher`（自行提交）再调
/// `so_shipment_with_stock`（另一个事务）。第二步失败时收入与应收已入账提交，
/// 却没有发货/出库记录 → 孤儿凭证。退货同理：超退报错但收入冲回已提交。
#[tokio::test]
async fn sales_shipment_and_return_are_atomic() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let dash: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_get("/api/dashboard", &sid))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d13 = format!("{}-13", dash["current_period"].as_str().unwrap());

    // 建一张已确认的销售订单，返回 (id, 单号)
    async fn mk_so(
        state: &Arc<WebState>,
        sid: &str,
        cur_ymm: i32,
        d13: &str,
        qty: &str,
    ) -> (i64, String) {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/sales/so",
                sid,
                serde_json::json!({
                    "id": 0, "period": cur_ymm, "date": d13,
                    "customer_code": "C01", "customer_name": "客户甲",
                    "status": "Confirmed", "memo": "原子性造数",
                    "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": qty, "unit_price": "25", "tax_rate": "0" }]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建销售订单");
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let id = v["id"].as_i64().unwrap();
        // save_so 的响应里没有 `no` 字段，从列表接口取单号
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/sales/so?period={cur_ymm}"), sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "列销售订单");
        let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let no = list["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("订单列表(rows)不是数组：{list}"))
            .iter()
            .find(|x| x["id"].as_i64() == Some(id))
            .and_then(|x| x["no"].as_str())
            .unwrap_or_else(|| panic!("列表里找不到 id={id} 的订单：{list}"))
            .to_string();
        (id, no)
    }

    // 统计摘要里提到该订单号的凭证数（收入确认凭证的摘要形如「发货确认 SO-xxx」）
    async fn count_vouchers_for(
        state: &Arc<WebState>,
        sid: &str,
        cur_ymm: i32,
        so_no: &str,
    ) -> usize {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(
                &format!("/api/vouchers?period={cur_ymm}&limit=500"),
                sid,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "列凭证");
        let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        list.as_array()
            .unwrap()
            .iter()
            .filter(|v| v["summary"].as_str().unwrap_or("").contains(so_no))
            .count()
    }

    // ── 场景 1：发货时仓库不存在 → 400，且不得留下任何收入凭证 ──
    let (so1, no1) = mk_so(&state, &sid, cur_ymm, &d13, "10").await;
    let before = count_vouchers_for(&state, &sid, cur_ymm, &no1).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so1, "period": cur_ymm, "date": d13, "qty": "4", "memo": "", "warehouse": "NOPE" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非法仓库应 400");
    assert_eq!(
        before,
        count_vouchers_for(&state, &sid, cur_ymm, &no1).await,
        "发货失败不得留下收入凭证（原子性）"
    );

    // ── 场景 2：正常发货成功，凭证 + 出库都在 ──
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so1, "period": cur_ymm, "date": d13, "qty": "4", "memo": "", "warehouse": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货应成功");
    let ok_body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        ok_body["voucher_id"].is_i64(),
        "发货应生成收入凭证：{ok_body}"
    );
    assert!(
        count_vouchers_for(&state, &sid, cur_ymm, &no1).await > before,
        "发货后凭证数应增加"
    );

    // ── 场景 3：退货超量 → 400，且不得留下收入冲回凭证 ──
    let (so2, no2) = mk_so(&state, &sid, cur_ymm, &d13, "10").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so2, "period": cur_ymm, "date": d13, "qty": "4", "memo": "", "warehouse": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "先发货 4");
    let before_ret = count_vouchers_for(&state, &sid, cur_ymm, &no2).await;
    // 净发货只有 4，退 50 必须被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/return",
            &sid,
            serde_json::json!({ "so_id": so2, "period": cur_ymm, "date": d13, "qty": "50", "memo": "", "warehouse": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超退应 400");
    assert_eq!(
        before_ret,
        count_vouchers_for(&state, &sid, cur_ymm, &no2).await,
        "超退失败不得留下收入冲回凭证（原子性）"
    );
}

/// 往来核销列表的「含未记账」只读开关（对标金蝶单据驱动模型）。
///
/// 核销/账龄/催款只认已记账分录（全仓 H-3 口径）；勾上「含未记账」能把待记账的
/// 往来一并列出来**查看**，但它们不可被核销——服务端 `settle` 会拒绝未记账的
/// 被核销方。回归：早先 `open_entries` 写 `status != 'void'`，草稿直接进了核销列表。
#[tokio::test]
async fn settle_open_include_draft_is_readonly() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 录一张应收凭证（112201 核算客户，必须带辅助核算），可选是否记账
    async fn mk(state: &Arc<WebState>, sid: &str, no: i32, post_it: bool) -> i64 {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                    "no": no, "attachments": 0, "memo": "含未记账开关造数",
                    "entries": [
                        { "line": 1, "account_code": "112201", "summary": "应收", "debit": "100", "credit": "0",
                          "aux": { "customer": "C01" } },
                        { "line": 2, "account_code": "600101", "summary": "应收", "debit": "0", "credit": "100" }
                    ]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "录应收凭证");
        let id: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            ["id"]
            .as_i64()
            .unwrap();
        if post_it {
            let r = handlers::router(state.clone())
                .oneshot(authed_post(
                    &format!("/api/vouchers/{id}/post"),
                    sid,
                    serde_json::json!({}),
                ))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK, "记账");
        }
        id
    }
    let posted_vid = mk(&state, &sid, 1, true).await;
    let draft_vid = mk(&state, &sid, 2, false).await;

    // 抓草稿凭证在 112201 上的分录 id
    let draft_eid: i64 = {
        let r = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/vouchers/{draft_vid}"), &sid))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["account_code"] == "112201")
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    // 已记账那张的分录 id
    let posted_eid: i64 = {
        let r = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/vouchers/{posted_vid}"), &sid))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["account_code"] == "112201")
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    };

    // 默认：只有已记账那笔
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(
        d["rows"].as_array().unwrap().len(),
        1,
        "默认不应列出未记账行"
    );
    assert_eq!(d["draft_count"], 0);
    assert_eq!(d["rows"][0]["posted"], true);

    // 开关打开：两笔都在，草稿那笔 posted=false
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601&draft=1",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(d["include_draft"], true);
    assert_eq!(
        d["rows"].as_array().unwrap().len(),
        2,
        "开关打开应列出未记账行"
    );
    assert_eq!(d["draft_count"], 1);
    let draft_row = d["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["posted"] == false)
        .expect("应有 posted=false 的行");
    assert_eq!(draft_row["entry_id"].as_i64(), Some(draft_eid));

    // 关键：草稿那笔不能当「原单」被核销（手工核销 entry id 由前端传入，
    // 绕过了列表过滤，只能靠 settle 自己兜）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/run",
            &sid,
            serde_json::json!({ "from_entry": draft_eid, "to_entry": posted_eid, "amount": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "未记账债权不得被核销");
    let b: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert!(
        b["error"].as_str().unwrap_or("").contains("已记账"),
        "应说明需先记账：{b}"
    );

    // 核销记录不得留下
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/records?account=112201", &sid))
        .await
        .unwrap();
    let d: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let recs = d["rows"].as_array().cloned().unwrap_or_default();
    assert!(recs.is_empty(), "被拒的核销不得留下记录：{d}");
}

/// 往来对账单（对标金蝶「客户对账单」）：按期间出账单 → 发出 → 客户回签确认。
///
/// 与催款单不同：对账单走**完整流水**（含已核销），期初 + 本期增 - 减 = 期末，
/// 且创建时快照，之后改凭证不影响已发出的单子。
#[tokio::test]
async fn statement_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    /// 造一张 112201 应收凭证（带客户辅助核算），对方科目固定 1001 库存现金（无辅助核算）。
    /// `dr`/`cr` 是 112201 侧的借贷方，对方科目自动取相反方向保证借贷必平。
    async fn ar_voucher(
        state: &Arc<WebState>,
        sid: &str,
        date: &str,
        no: i32,
        dr: &str,
        cr: &str,
        post_it: bool,
    ) -> i64 {
        let opp_dr = if dr == "0" { cr } else { "0" };
        let opp_cr = if cr == "0" { dr } else { "0" };
        // 期间必须与日期一致（否则被「日期与所属期间不一致」校验挡下）：YYYYMM
        let ymm: i32 = format!("{}{}", &date[0..4], &date[5..7]).parse().unwrap();
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                sid,
                serde_json::json!({
                    "id": 0, "period": ymm, "date": date, "word": "记",
                    "no": no, "attachments": 0, "memo": "对账单造数",
                    "entries": [
                        { "line": 1, "account_code": "112201", "summary": "应收", "debit": dr, "credit": cr,
                          "aux": { "customer": "C01" } },
                        { "line": 2, "account_code": "1001", "summary": "对方", "debit": opp_dr, "credit": opp_cr }
                    ]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "录应收凭证：{date}");
        let id: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            ["id"]
            .as_i64()
            .unwrap();
        if post_it {
            let r = handlers::router(state.clone())
                .oneshot(authed_post(
                    &format!("/api/vouchers/{id}/post"),
                    sid,
                    serde_json::json!({}),
                ))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK, "记账");
        }
        id
    }

    // 1 月形成 1000 欠款；1 月收 400；2 月草稿 900（不进对账单）
    ar_voucher(&state, &sid, "2026-01-10", 1, "1000", "0", true).await;
    ar_voucher(&state, &sid, "2026-01-20", 2, "0", "400", true).await;
    ar_voucher(&state, &sid, "2026-02-10", 3, "900", "0", false).await;

    // 无往来的客商 → 400
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/statements",
            &sid,
            serde_json::json!({ "kind": "ar", "account": "112201", "party_code": "NOBODY",
                               "party_name": "", "from": 202601, "to": 202601, "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "无往来不应出对账单");

    // 正常出账：期初 0 / 增 1000 / 减 400 / 期末 600
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/statements",
            &sid,
            serde_json::json!({ "kind": "ar", "account": "112201", "party_code": "C01",
                               "party_name": "客户甲", "from": 202601, "to": 202601, "memo": "1月对账" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "生成对账单");
    let s: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let sid_id = s["id"].as_i64().unwrap();
    assert!(s["no"].as_str().unwrap().starts_with("DB202601"));
    assert_eq!(s["status"], "draft");
    assert_eq!(s["begin_balance"], "0.00");
    assert_eq!(s["period_increase"], "1,000.00");
    assert_eq!(s["period_decrease"], "400.00");
    assert_eq!(s["end_balance"], "600.00");
    // 草稿那笔不得进对账单
    let lines = s["lines"].as_array().unwrap();
    assert!(
        !lines
            .iter()
            .any(|l| l["increase"].as_str() == Some("900.00")),
        "草稿不得入账：{lines:?}"
    );
    // 末行滚动余额 == 期末
    assert_eq!(
        lines.last().unwrap()["balance"].as_str(),
        Some("600.00"),
        "逐行滚动余额必须与期末一致"
    );

    // 状态机：草稿 → 已发出
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "sent" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "发出对账单");
    // 重复发出 → 400
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "sent" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "已发出不应重复发出");

    // 回签确认必须填确认人
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "confirmed", "confirmed_by": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "回签必须填确认人");

    // 正常回签
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "confirmed", "confirmed_by": "客户王五" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "客户回签");
    let s: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(s["status"], "confirmed");
    assert_eq!(s["confirmed_by"], "客户王五");
    // 已确认是终态：不能作废
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "cancelled" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "已确认对账单不得作废");

    // 快照语义：出账后再补一笔已记账，已发出的对账单数字不变
    ar_voucher(&state, &sid, "2026-01-25", 4, "500", "0", true).await;
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/statements?kind=ar", &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let got = list["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(sid_id))
        .unwrap();
    assert_eq!(
        got["end_balance"], "600.00",
        "已发出的对账单不得被后续业务改动"
    );
}

/// P1：催款单/对账函——按客商快照未核销、状态流转、无欠款拒绝。
#[tokio::test]
async fn dunning_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d13 = format!("{}-13", dash["current_period"].as_str().unwrap());

    // 销售链造应收：SO C01 4×25 → 确认 → 发货（自动生成应收凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d13,
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "催款造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "4", "unit_price": "25", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建销售订单");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d13, "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货");

    // 发货生成的应收凭证是草稿，往来核销/催款单只认**已记账**分录（H-3 口径），
    // 所以必须先记账，否则催款快照查不到任何未核销单据。
    // 回归：open_entries 早先写 `status != 'void'`，草稿也被算成欠款 ——
    // 那意味着可以对一张还没入账、随时可能删掉的凭证发催款函。
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers?period={cur_ymm}&status=draft"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "列草稿凭证");
    let drafts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ar_vid = drafts
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["summary"].as_str().unwrap_or("").contains("发货确认"))
        .and_then(|v| v["id"].as_i64())
        .expect("应存在发货确认的应收凭证草稿");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{ar_vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账应收凭证");

    // 生成催款单：快照未核销应收 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/dunnings",
            &sid,
            serde_json::json!({ "kind": "ar", "account": "1122", "party_code": "C01", "party_name": "客户甲", "date": "", "memo": "首次催收" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "生成催款单：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    let d = &r["dunning"];
    let did = d["id"].as_i64().unwrap();
    let no = d["no"].as_str().unwrap().to_string();
    assert!(no.starts_with("CK"), "单号前缀：{no}");
    assert_eq!(d["status"], "draft");
    assert!(
        (d["amount"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 100.0)
            .abs()
            < 0.005,
        "应收快照 100：{d}"
    );
    assert!(d["item_count"].as_i64().unwrap() >= 1, "明细至少一笔");
    assert!(!d["detail"].as_array().unwrap().is_empty(), "明细快照");

    // 无欠款客商 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/dunnings",
            &sid,
            serde_json::json!({ "kind": "ar", "account": "1122", "party_code": "C99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "无欠款应 400");

    // 状态流转：draft → sent → settled；重复/越级 → 400
    for target in ["sent", "settled"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/settle/dunnings/{did}/status"),
                &sid,
                serde_json::json!({ "status": target }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "流转到 {target}");
    }
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/dunnings/{did}/status"),
            &sid,
            serde_json::json!({ "status": "settled" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复结清应 400");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/dunnings/{did}/status"),
            &sid,
            serde_json::json!({ "status": "cancelled" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已结清不可作废");

    // 列表包含该单
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/dunnings?kind=ar", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"].as_array().unwrap().iter().any(|x| x["no"] == no),
        "列表应含 {no}"
    );

    // 未知 id → 404
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/dunnings/999999/status",
            &sid,
            serde_json::json!({ "status": "sent" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "未知催款单应 404");
}

/// P1：供应链补链——报价→订单 doc_link + 到货推进采购订单状态（部分入库/已完成，退货回落）。
#[tokio::test]
async fn quote_link_and_po_status() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d13 = format!("{}-13", dash["current_period"].as_str().unwrap());

    // 报价 → 审批 → 转订单 → 单据链上游含报价
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d13, "customer_code": "C01",
                "customer_name": "客户甲", "item_code": "140301", "item_name": "原料",
                "qty": "3", "unit_price": "20", "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建报价");
    let qid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审批报价");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/to-order"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "报价转订单");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["so_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=so&id={so_id}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "quote" && n["dir"] == "up"),
        "订单链上游应见报价：{r}"
    );

    // 采购订单 10 @9：到货 4 → 部分入库；到货 6 → 已完成；退货 2 → 部分入库
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d13, "supplier_code": "S01", "supplier_name": "供应商甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    async fn po_status_of(state: &Arc<WebState>, sid: &str, cur_ymm: i32, po_id: i64) -> String {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(
                &format!("/api/procure/po?period={cur_ymm}"),
                sid,
            ))
            .await
            .unwrap();
        let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == po_id)
            .map(|x| x["status"].as_str().unwrap().to_string())
            .unwrap_or_default()
    }
    assert_eq!(po_status_of(&state, &sid, cur_ymm, po_id).await, "Draft");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货 4");
    assert_eq!(
        po_status_of(&state, &sid, cur_ymm, po_id).await,
        "PartialIn",
        "部分到货应为部分入库"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "6", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货 6");
    assert_eq!(
        po_status_of(&state, &sid, cur_ymm, po_id).await,
        "Completed",
        "全量到货应为已完成"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/return",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "2", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "退货 2");
    assert_eq!(
        po_status_of(&state, &sid, cur_ymm, po_id).await,
        "PartialIn",
        "退货后应回落到部分入库"
    );
}

/// P1：仓库主数据——默认仓、CRUD 守卫、出入库带仓/非法仓拒绝、默认仓兜底。
#[tokio::test]
async fn warehouse_master_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d13 = format!("{}-13", dash["current_period"].as_str().unwrap());

    // 默认仓已种（01 主仓）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/warehouses", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let def = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["code"] == "01")
        .expect("应有默认仓 01");
    assert_eq!(def["is_default"], true);
    assert_eq!(def["name"], "主仓");

    // 新增 02 成品仓 → upsert 改名
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/warehouses",
            &sid,
            serde_json::json!({ "code": "02", "name": "成品仓" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增仓库 02");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/warehouses",
            &sid,
            serde_json::json!({ "code": "02", "name": "成品仓A", "is_default": false, "disabled": false, "memo": "m" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改名 upsert");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/warehouses", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["code"] == "02" && x["name"] == "成品仓A"),
        "名称应已更新：{r}"
    );

    // 采购 5@9：到货指定 02 → 分仓库存可见 02
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d13, "supplier_code": "S01", "supplier_name": "供应商甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "5", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "5", "warehouse": "02", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货指定仓库 02");
    let wh_qty = |state: Arc<WebState>, sid: String| async move {
        let resp = handlers::router(state)
            .oneshot(authed_get(
                "/api/inventory/warehouse-stock?item=140301",
                &sid,
            ))
            .await
            .unwrap();
        let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                (
                    x["warehouse"].as_str().unwrap().to_string(),
                    x["qty"]
                        .as_str()
                        .unwrap()
                        .replace(',', "")
                        .parse::<f64>()
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let rows = wh_qty(state.clone(), sid.clone()).await;
    assert!(
        rows.iter()
            .any(|(w, q)| w == "02" && (*q - 5.0).abs() < 0.005),
        "02 仓应有 5：{rows:?}"
    );

    // 不填仓库 → 默认仓 01 兜底
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "1", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "默认仓到货");
    let rows = wh_qty(state.clone(), sid.clone()).await;
    assert!(
        rows.iter()
            .any(|(w, q)| w == "01" && (*q - 1.0).abs() < 0.005),
        "默认仓 01 应有 1：{rows:?}"
    );

    // 非法仓 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "1", "warehouse": "ZZ" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不存在仓库应 400");

    // 停用后写入 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/warehouses",
            &sid,
            serde_json::json!({ "code": "02", "name": "成品仓A", "disabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "停用 02");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "1", "warehouse": "02" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "停用仓应 400");

    // 删除守卫：默认仓 400；被引用仓 400；未引用仓 200
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/warehouses/01", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "默认仓不可删");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/warehouses/02", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "被引用仓不可删");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/warehouses",
            &sid,
            serde_json::json!({ "code": "03", "name": "临时仓" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增 03");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/warehouses/03", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "未引用仓可删");
}

/// P1：生产订单变更/取消——变更留痕、状态守卫、未知订单 404。
#[tokio::test]
async fn prod_change_cancel_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建单（已下达）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "10", "date": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下达生产订单");
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 变更：数量 + 计划日期 + 备注 → 变更历史至少 4 条
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/prod/{pid}"),
            &sid,
            serde_json::json!({ "qty": "12", "plan_start": "2026-01-10", "plan_end": "2026-01-20", "memo": "改期" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "变更订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod/{pid}/changes"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let fields: Vec<String> = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["field"].as_str().unwrap().to_string())
        .collect();
    for f in ["计划数量", "计划开工", "计划完工", "备注"] {
        assert!(
            fields.contains(&f.to_string()),
            "变更历史应含 {f}：{fields:?}"
        );
    }

    // 非法数量 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/prod/{pid}"),
            &sid,
            serde_json::json!({ "qty": "0" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "数量 0 应 400");

    // 取消 → 200；取消后不可再变更/取消
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/cancel"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/prod/{pid}"),
            &sid,
            serde_json::json!({ "qty": "13" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已取消不可变更");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/cancel"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复取消应 400");

    // 已开工订单：变更/取消均 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "5", "date": "" }),
        ))
        .await
        .unwrap();
    let pid2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid2}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/prod/{pid2}"),
            &sid,
            serde_json::json!({ "qty": "6" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已开工不可变更");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid2}/cancel"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已开工不可取消");

    // 未知订单 → 404
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/prod/999999",
            &sid,
            serde_json::json!({ "qty": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "未知订单变更应 404");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/999999/cancel",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "未知订单取消应 404");
}

/// P1：合并报表（跨账套汇总）——两套独立取数按科目合计；非管理员 403；未知账套 400。
#[tokio::test]
async fn consolidate_books() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // b1：借 1001 100 / 贷 2001 100 → 记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记", "no": 95,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "合并造数", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "合并造数", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "b1 造数");
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "b1 记账");

    // 建 b2 并进入：借 1001 200 / 贷 2001 200 → 记账
    let (st, body) = create_book(&state, &sid, "第二公司").await;
    assert_eq!(st, StatusCode::OK, "建第二账套");
    let b2 = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        select_book(&state, &sid, &b2).await,
        StatusCode::OK,
        "进入 b2"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记", "no": 95,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "合并造数", "debit": "200", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "合并造数", "debit": "0", "credit": "200" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "b2 造数");
    let vid2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    // b2 走生产建账路径 → **两道闸门默认都开**（审核 + 出纳签字），与 b1 夹具不同。
    // 夹具在 test_state() 里显式关掉了两道闸门，所以 b1 那张不需要这两步 ——
    // 这也正是夹具必须显式声明、不能靠继承默认值的原因。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid2}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "b2 审核");
    // 出纳签字：b2 的凭证借记 1001 库存现金，生产默认开着出纳闸门。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid2}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(resp, "b2 出纳签字").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid2}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(resp, "b2 记账").await;

    // 合并汇总：1001 合计 300、2001 合计 -300（借正贷负）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/consolidate/preview?period=202601&books=b1,{b2}"),
            &sid,
        ))
        .await
        .unwrap();
    let st = resp.status();
    let body = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "合并汇总：{body}");
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["books"].as_array().unwrap().len(), 2, "两套：{r}");
    let row = |code: &str| {
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["account_code"] == code)
            .cloned()
            .unwrap()
    };
    let n = |v: &serde_json::Value| v.as_str().unwrap().replace(',', "").parse::<f64>().unwrap();
    assert!(
        (n(&row("1001")["total"]) - 300.0).abs() < 0.005,
        "1001 合计 300：{r}"
    );
    assert!(
        (n(&row("2001")["total"]) + 300.0).abs() < 0.005,
        "2001 合计 -300：{r}"
    );

    // 未知账套 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/consolidate/preview?period=202601&books=b1,nope",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知账套应 400");

    // 非管理员 → 403（先完成首登改密）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({ "username": "cu1", "display_name": "合并测试", "password": "Cu1@pass99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开号 cu1");
    let (_, cu_sid) = login(&state, "cu1", "Cu1@pass99").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &cu_sid,
            serde_json::json!({ "old": "Cu1@pass99", "new": "Cu1@pass99x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "cu1 改密");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/consolidate/preview?period=202601&books=b1",
            &cu_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "非管理员应 403");
}

/// P1：工序质检——检验点门槛、合格/部分/全不合格、报废扣减计划量、状态守卫。
#[tokio::test]
async fn prod_qc_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 工艺路线含检验点
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/routing/140501",
            &sid,
            serde_json::json!([{ "seq": 1, "op_code": "OP1", "op_name": "车", "work_center": "WC1", "std_hours": "1", "rate": "10", "qc_required": true }]),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存工艺路线");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/routing/140501", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ops"][0]["qc_required"], true, "检验点应回显：{r}");

    // 订单 10 → 开工
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "10" }),
        ))
        .await
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");

    // 完工被工序检验门槛拦截
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/complete"),
            &sid,
            serde_json::json!({ "qty": "10", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "无检验记录应拦完工");

    // 非法检验：不合格 > 检验 / 不合格无处置
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/qc"),
            &sid,
            serde_json::json!({ "qty_insp": "10", "qty_fail": "11", "disposition": "scrap", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "不合格超检验量应 400"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/qc"),
            &sid,
            serde_json::json!({ "qty_insp": "10", "qty_fail": "2", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不合格无处置应 400");

    // 正式检验：10 检 2 报废 → 计划量 8 + 变更留痕
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/qc"),
            &sid,
            serde_json::json!({ "qty_insp": "10", "qty_fail": "2", "disposition": "scrap", "date": d15, "memo": "首检" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "录检验单：{b}");
    let qc: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(qc["result"], "partial", "部分合格：{qc}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let o = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(pid))
        .unwrap();
    assert_eq!(o["planned_qty"], "8", "报废扣减计划量：{o}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod/{pid}/changes"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["field"] == "计划数量（报废扣减）"),
        "报废应留痕：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod/{pid}/qc"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1, "检验记录 1 条");

    // 完工 8 → 200；完工后再检验 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/complete"),
            &sid,
            serde_json::json!({ "qty": "8", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "有检验记录后可完工");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/qc"),
            &sid,
            serde_json::json!({ "qty_insp": "1", "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "完工后不可再检验");
}

/// P2：补口子——凭证作废/恢复、资产减值、资产盘点接线。
#[tokio::test]
async fn p2_gap_fill_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 1) 凭证作废 / 恢复（引擎口径：未记账可作废；已记账请先反记账）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 96,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "作废测试", "debit": "10", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "作废测试", "debit": "0", "credit": "10" }
                ]
            }),
        ))
        .await
        .unwrap();
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/void"),
            &sid,
            serde_json::json!({ "void": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "未记账作废");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["status"], "void", "作废后状态：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/void"),
            &sid,
            serde_json::json!({ "void": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "恢复作废");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["status"], "draft", "恢复后回到草稿：{r}");
    // 已记账 → 作废被引擎拒绝（先反记账）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/void"),
            &sid,
            serde_json::json!({ "void": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已记账应先反记账");

    // 2) 资产减值
    let card = serde_json::json!({
        "id": 0, "code": "P2A01", "name": "减值测试资产", "category": "电子设备", "spec": "",
        "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
        "expense_account": "660201", "original_value": "12000", "residual_rate": "5",
        "life_months": 36, "method": "straight", "start_period": cur_ymm, "memo": ""
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/assets", &sid, card))
        .await
        .unwrap();
    let aid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/{aid}/impair"),
            &sid,
            serde_json::json!({ "amount": "500", "period": cur_ymm, "memo": "测试减值" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资产减值");

    // 3) 资产盘点：盘亏 → 过账置停用
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/counts",
            &sid,
            serde_json::json!({ "period": cur_ymm, "date": d15, "memo": "测试盘点", "lines": [{ "asset_id": aid, "found": false }] }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建盘点单：{b}");
    let cid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/counts/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let pr: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(pr["lost"], 1, "盘亏 1 张：{pr}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/assets?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let a = r["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(aid))
        .unwrap();
    assert_eq!(a["status"], "idle", "盘亏后置停用：{a}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/assets/counts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"].as_i64() == Some(cid)),
        "盘点单列表应含：{r}"
    );
    // 未知资产 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/counts",
            &sid,
            serde_json::json!({ "period": cur_ymm, "lines": [{ "asset_id": 999999 }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知资产应 400");
}

/// P2：制造成本 Web 化——WIP / 差异 / 预测 / 制造费用分摊（试算与应用）。
#[tokio::test]
async fn cost_web_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 标准价 + BOM + 订单 → 开工 → 领料（材料成本 5×2×10 = 100）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "标准价");
    // 存货档案参考成本（BOM 成本汇总口径：props.ref_cost）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "item", "code": "140301", "name": "原料",
                "parent_code": null, "disabled": false, "props": { "ref_cost": "10" }, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "存货参考成本");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "BOM");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "5", "date": d15 }),
        ))
        .await
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/issue"),
            &sid,
            serde_json::json!({ "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "领料");

    // WIP：材料 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/cost/wip?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let w = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["po_id"].as_i64() == Some(pid))
        .expect("WIP 应含该订单");
    assert!(
        (w["material"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 100.0)
            .abs()
            < 0.005,
        "WIP 材料 100：{w}"
    );

    // 差异 + 预测
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/cost/variance?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["po_id"].as_i64() == Some(pid)),
        "差异应含该订单：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/cost/forecast?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let f = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["po_id"].as_i64() == Some(pid))
        .expect("预测应含该订单");
    assert!(
        (f["forecast"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 100.0)
            .abs()
            < 0.005,
        "预测料本 100：{f}"
    );

    // 制造费用分摊：试算 50（按成本占比，唯一订单全额）→ 应用 → WIP overhead 50
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/overhead",
            &sid,
            serde_json::json!({ "period": cur_ymm, "amount": "50", "base": "cost", "apply": false }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["applied"], false);
    assert_eq!(r["rows"].as_array().unwrap().len(), 1, "试算 1 单：{r}");
    assert!(
        (r["rows"][0]["amount"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 50.0)
            .abs()
            < 0.005,
        "试算 50：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/overhead",
            &sid,
            serde_json::json!({ "period": cur_ymm, "amount": "50", "base": "cost", "apply": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "应用分摊");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/cost/wip?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let w = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["po_id"].as_i64() == Some(pid))
        .unwrap();
    assert!(
        (w["overhead"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 50.0)
            .abs()
            < 0.005,
        "应用后 WIP 制造费用 50：{w}"
    );
    // 金额 0 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/overhead",
            &sid,
            serde_json::json!({ "period": cur_ymm, "amount": "0", "base": "cost", "apply": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "分摊金额 0 应 400");
}

/// P2：预算硬控制——预算行编制 + 凭证保存按 warn/strong 校验（执行口径=已记账）。
#[tokio::test]
async fn budget_control_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 编制预算行：660201 202601 预算 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/budget/rows",
            &sid,
            serde_json::json!({ "period": 202601, "account_code": "660201", "dept": "", "amount": "100", "memo": "测试" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "编制预算行");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/budget/rows?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1, "预算行列表：{r}");

    // 账套参数：预算控制 = warn
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    opts["budget_control"] = serde_json::json!("warn");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts.clone()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存参数（warn）");

    let save_voucher = |amount: &'static str| {
        let state = state.clone();
        let sid = sid.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_post(
                    "/api/vouchers",
                    &sid,
                    serde_json::json!({
                        "id": 0, "period": 202601, "date": "2026-01-15", "word": "记", "no": 0,
                        "attachments": 0, "memo": "", "entries": [
                            { "line": 1, "account_code": "660201", "summary": "预算测试", "debit": amount, "credit": "0" },
                            { "line": 2, "account_code": "1001", "summary": "预算测试", "debit": "0", "credit": amount }
                        ]
                    }),
                ))
                .await
                .unwrap();
            resp.status()
        }
    };

    // warn：超预算放行（120 > 100）
    assert_eq!(save_voucher("120").await, StatusCode::OK, "warn 超预算放行");
    // warn 下未超（50）也放行
    assert_eq!(save_voucher("50").await, StatusCode::OK, "warn 未超放行");

    // 切 strong：超预算拒绝（120），未超放行（50）
    opts["budget_control"] = serde_json::json!("strong");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存参数（strong）");
    assert_eq!(
        save_voucher("120").await,
        StatusCode::BAD_REQUEST,
        "strong 超预算拒绝"
    );
    assert_eq!(save_voucher("50").await, StatusCode::OK, "strong 未超放行");
}

/// P2：银行代发文件 + 工资条 + 个税申报表。
#[tokio::test]
async fn payroll_bank_slip_tax() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 员工档案：E001 带银行账号，E002 无
    for (code, props) in [
        (
            "E001",
            serde_json::json!({ "bank_account": "6222001", "bank_name": "张三" }),
        ),
        ("E002", serde_json::json!({})),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/aux",
                &sid,
                serde_json::json!({
                    "id": 0, "kind": "employee", "code": code, "name": code,
                    "parent_code": null, "disabled": false, "props": props, "memo": ""
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "员工档案 {code}");
    }
    // 工资两条
    for (emp, gross) in [("E001", "10000"), ("E002", "8000")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/payroll?period=202601",
                &sid,
                serde_json::json!({
                    "employee": emp, "dept": "财务部", "gross": gross,
                    "social": "500", "housing": "300", "deduction": "0",
                    "additional": "1000", "social_co": "800", "housing_co": "300", "memo": ""
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "工资 {emp}");
    }

    // 银行代发：CSV 含账号与户名；无账号员工跳过
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll/bank-file?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let csv = body_string(resp).await;
    assert!(csv.contains("6222001"), "代发应含账号：{csv}");
    assert!(csv.contains("张三"), "户名取档案 bank_name：{csv}");
    assert!(!csv.contains("E002"), "无账号员工应跳过：{csv}");

    // 工资条：先补 202602 一条，验证 YTD 口径 = 截至上月（2 月时累计 1 个月）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202602",
            &sid,
            serde_json::json!({
                "employee": "E001", "dept": "财务部", "gross": "10000",
                "social": "500", "housing": "300", "deduction": "0",
                "additional": "1000", "social_co": "800", "housing_co": "300", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "2 月工资");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/payroll/slip?period=202602&employee=E001",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["payroll"]["net"].as_str().is_some(), "工资条实发：{r}");
    assert_eq!(r["ytd"]["months"], 1, "2 月时本年累计 1 个月：{r}");
    assert!(
        r["ytd"]["income"].as_str().unwrap().contains("10,000"),
        "累计收入=1 月应发：{r}"
    );
    // 缺 employee → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll/slip?period=202602", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "缺 employee 应 400");

    // 个税申报表：2 人
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll/tax-report?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 2, "申报表 2 人：{r}");
}

/// P2：单据编码规则自定义 + 工作流消息节点（到达即通知并自动继续）。
#[tokio::test]
async fn doc_prefix_and_wf_message() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 1) 单据前缀：采购改 CGP，销售留默认
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    opts["doc_prefixes"] = serde_json::json!({ "po": "CGP" });
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存前缀");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "supplier_code": "S01", "supplier_name": "供应商甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "1", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/po?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po_no = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(po_id))
        .unwrap()["no"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        po_no.starts_with("CGP202601"),
        "采购单号应带自定义前缀：{po_no}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "1", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let so_no = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(so_id))
        .unwrap()["no"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        so_no.starts_with("XS202601"),
        "销售单号保持默认前缀：{so_no}"
    );

    // 2) 工作流消息节点：start → 初审 → 消息 → 复核；消息节点自动跳过并留痕
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows",
            &sid,
            serde_json::json!({
                "id": 0, "name": "报价带消息节点", "biz_type": "quotation",
                "nodes": [
                    { "id": "n1", "type": "start", "name": "开始" },
                    { "id": "n2", "type": "approve", "name": "初审" },
                    { "id": "n3", "type": "message", "name": "通知业务员" },
                    { "id": "n4", "type": "approve", "name": "复核" }
                ],
                "edges": [
                    { "id": "e1", "from": "n1", "to": "n2", "kind": "normal" },
                    { "id": "e2", "from": "n2", "to": "n3", "kind": "normal" },
                    { "id": "e3", "from": "n3", "to": "n4", "kind": "normal" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存流程");
    let flow_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/workflows/{flow_id}/publish"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发布流程");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06", "customer_code": "C03", "customer_name": "客户丙",
                "item_code": "140501", "item_name": "成品", "qty": "1", "unit_price": "5",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let q = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["pending"], "复核", "消息节点应自动跳过、停在复核：{r}");
    // 消息留痕（审计 → 通知中心动态）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/logs?limit=20", &sid))
        .await
        .unwrap();
    let logs = body_string(resp).await;
    assert!(
        logs.contains("工作流") && logs.contains("消息"),
        "消息节点应留痕：{logs}"
    );
    // 复核通过 → 终态批准
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["ok"] == true, "终态批准：{r}");
}

/// P2：滚动资金预测——票据到期 + 融资起止按期间展开。
#[tokio::test]
async fn funds_rolling_forecast() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收票据 1000 到期 202602；借款 500 起 202602 止 202603
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/bills",
            &sid,
            serde_json::json!({
                "kind": "receivable", "no": "B1", "period": 202601,
                "issue_date": "2026-01-05", "due_date": "2026-02-20",
                "counterpart": "客户甲", "bank": "工行", "amount": "1000", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建票据");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/loans",
            &sid,
            serde_json::json!({
                "kind": "borrow", "no": "L1", "bank": "工行", "principal": "500",
                "rate_pct": "4.5", "start_date": "2026-02-01", "end_date": "2026-03-31", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建融资");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/funds/forecast-rolling?from=202601&periods=3",
            &sid,
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "滚动预测：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let n = |v: &serde_json::Value| v.as_str().unwrap().replace(',', "").parse::<f64>().unwrap();
    // 202602：票据收 1000 + 融资到账 500 → 净 1500
    let feb = &rows[1];
    assert!(
        (n(&feb["bill_in"]) - 1000.0).abs() < 0.005,
        "2 月票据到期：{feb}"
    );
    assert!(
        (n(&feb["loan_in"]) - 500.0).abs() < 0.005,
        "2 月融资到账：{feb}"
    );
    assert!(
        (n(&feb["net"]) - 1500.0).abs() < 0.005,
        "2 月净流 1500：{feb}"
    );
    assert!(
        (n(&feb["balance"]) - 1500.0).abs() < 0.005,
        "2 月结存 1500：{feb}"
    );
    // 202603：融资偿还 500 → 净 -500、结存 1000
    let mar = &rows[2];
    assert!(
        (n(&mar["loan_out"]) - 500.0).abs() < 0.005,
        "3 月偿还：{mar}"
    );
    assert!(
        (n(&mar["net"]) + 500.0).abs() < 0.005,
        "3 月净流 -500：{mar}"
    );
    assert!(
        (n(&mar["balance"]) - 1000.0).abs() < 0.005,
        "3 月结存 1000：{mar}"
    );
}

/// P2：导出计划任务——CRUD + 立即执行写文件 + 校验。
#[tokio::test]
async fn export_schedule_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/export/schedules",
            &sid,
            serde_json::json!({ "kind": "vouchers", "period_mode": "current", "at_time": "00:00", "enabled": true, "memo": "测试" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增计划任务");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/schedules", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1, "列表 1 条：{r}");

    // 立即执行：写文件到 books_dir/exports/
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/export/schedules/{id}/run"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "立即执行：{b}");
    let path = serde_json::from_str::<serde_json::Value>(&b).unwrap()["path"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(path.contains("exports"), "路径应位于 exports：{path}");
    assert!(std::fs::metadata(&path).is_ok(), "导出文件应存在：{path}");

    // 校验：未知类型 / 非法时刻
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/export/schedules",
            &sid,
            serde_json::json!({ "kind": "nope", "at_time": "08:00" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知类型 400");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/export/schedules",
            &sid,
            serde_json::json!({ "kind": "trial", "at_time": "25:00" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非法时刻 400");

    // 删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/export/schedules/{id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除");
}

/// 越权回归：只读（Viewer）与出纳不得写入这些端点。
///
/// 历史上预算版本 / 审批 / 报表附注 / 档案的写路由只用只读权限 Perm::Report 把关，
/// 而 Report 是每个角色（含 Viewer）都自带的最低权限，等于对只读账号开放了写入；
/// 模板 / 工资 / 报销的 DELETE 又误用了 Perm::VoucherNew，使无 VoucherDelete
/// 的出纳也能删。
#[tokio::test]
async fn readonly_roles_cannot_write() {
    let (state, _bd, _dir) = test_state();
    let (_, boss_sid) = login(&state, "boss", "Admin!2026").await;

    // 平台账号（首登强制改密）
    for u in ["view1", "cash1"] {
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
    // boss 进入名下账套 b1，把两人拉成 Viewer / 出纳
    assert_eq!(
        select_book(&state, &boss_sid, "b1").await,
        StatusCode::OK,
        "boss 应能进入 b1"
    );
    for (u, role) in [("view1", "viewer"), ("cash1", "cashier")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &boss_sid,
                serde_json::json!({
                    "username": u, "display_name": u, "password": "Init@123456",
                    "role": role, "must_change_pwd": false,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {u} 为 {role}");
    }

    let mut sids = Vec::new();
    for u in ["view1", "cash1"] {
        let (st, sid) = login(&state, u, "Init@123456").await;
        assert_eq!(st, StatusCode::OK, "{u} 平台登录");
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
            "{u} 应能进入 b1"
        );
        sids.push(sid);
    }
    let view_sid = sids[0].clone();
    let cash_sid = sids[1].clone();

    // Viewer 读权限仍在
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/budget/versions", &view_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "Viewer 应能读预算版本");

    // 但所有写入口一律 403
    for (uri, body) in [
        (
            "/api/budget/versions",
            serde_json::json!({ "key": "v1", "name": "回归" }),
        ),
        (
            "/api/approvals",
            serde_json::json!({ "biz_kind": "purchase", "biz_id": 1, "title": "t", "approvers": ["boss"] }),
        ),
        (
            "/api/reports/notes",
            serde_json::json!({ "report_key": "balance-sheet", "period": 202601, "content": "x" }),
        ),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(uri, &view_sid, body))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "Viewer 写 {uri} 应被拒"
        );
    }

    // 出纳无 VoucherDelete，不能删凭证模板
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/templates/1", &cash_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "出纳删凭证模板应被拒");
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

// ---------------------------------------------------------------------------
// 回归：租户边界（2026-09 审查修复）
// ---------------------------------------------------------------------------

/// 平台管理员开通一个非管理员账号，完成首登改密后返回可用的 sid（尚未选账套）
async fn provision_plain_user(
    state: &Arc<WebState>,
    admin_sid: &str,
    username: &str,
    init_pwd: &str,
) -> String {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            admin_sid,
            serde_json::json!({ "username": username, "display_name": username, "password": init_pwd }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "开通平台账号 {username} 应成功"
    );
    let (st, sid) = login(state, username, init_pwd).await;
    assert_eq!(st, StatusCode::OK, "{username} 首次登录应成功");
    let new_pwd = format!("{init_pwd}x");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid,
            serde_json::json!({ "old": init_pwd, "new": new_pwd }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{username} 首登改密应成功");
    sid
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

/// 工作流预置模板：一键落流程（草稿态）+ 拒绝不适用单据。
///
/// 关键契约：模板描述的是行业惯例而非本企业授权制度，所以**落的是草稿不是已发布**——
/// 擅自发布等于替企业做了内控决策，且有单据在流转时改流程会让在途实例的
/// current_node 指向不存在的节点（引擎报「流程可能被改」）。
#[tokio::test]
async fn workflow_template_apply_and_reject_mismatch() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 模板清单
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/workflows/templates", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let tpls: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let rows = tpls["rows"].as_array().unwrap();
    assert!(rows.len() >= 4, "预置模板不应少于 4 套：{tpls}");
    let keys: Vec<&str> = rows.iter().filter_map(|t| t["key"].as_str()).collect();
    for want in ["simple", "standard", "claim_full", "funds", "amount_tiered"] {
        assert!(keys.contains(&want), "缺少模板 {want}：{keys:?}");
    }
    // 每个模板都要说明适用单据与依赖字段，前端据此筛选
    for t in rows {
        assert!(!t["name"].as_str().unwrap_or("").is_empty());
        assert!(!t["desc"].as_str().unwrap_or("").is_empty());
        assert!(
            t["node_count"].as_i64().unwrap_or(0) >= 2,
            "模板节点过少：{t}"
        );
        assert!(!t["applicable"].as_array().unwrap().is_empty(), "{t}");
    }

    // 金额分级模板不适用报价单（报价单无 amount 字段，套上去审批时会直接报错卡死）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows/templates/apply",
            &sid,
            serde_json::json!({ "key": "amount_tiered", "biz_type": "quotation" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "不适用单据应被拒");

    // 正常应用：落草稿
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows/templates/apply",
            &sid,
            serde_json::json!({ "key": "claim_full", "biz_type": "claim" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let whole: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let f = &whole["flow"];
    assert_eq!(f["status"], "draft", "模板应用后必须是草稿，不得自动发布");
    assert_eq!(f["biz_type"], "claim");
    let nodes = f["nodes"].as_array().unwrap();
    // 部门 → 财务 → 出纳 三级
    let names: Vec<&str> = nodes
        .iter()
        .filter(|n| n["type"] == "approve")
        .map(|n| n["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(names.len(), 3, "应有三个审批节点：{names:?}");
    assert!(
        names.iter().any(|n| n.contains("出纳")),
        "缺出纳节点：{names:?}"
    );
    // 出纳节点的参与人必须是 cashier 角色
    let cash = nodes
        .iter()
        .find(|n| n["name"].as_str().unwrap_or("").contains("出纳"))
        .unwrap();
    assert_eq!(
        cash["participants"][0], "cashier",
        "出纳节点参与人应为 cashier：{cash}"
    );

    // 不存在的模板 → 404/400
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows/templates/apply",
            &sid,
            serde_json::json!({ "key": "nope", "biz_type": "claim" }),
        ))
        .await
        .unwrap();
    assert!(
        r.status() == StatusCode::BAD_REQUEST || r.status() == StatusCode::NOT_FOUND,
        "不存在的模板应被拒，实际 {}",
        r.status()
    );

    // 应用后能在流程列表里看到
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/workflows", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert!(
        list["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["biz_type"] == "claim"),
        "模板落地的流程应出现在列表里"
    );
}

/// 审核环节默认开（对标金蝶星空 + 会计内控底线：制单/审核/记账三权分离）。
///
/// 锁两件事：① 通过生产建账接口创建的账套，审核环节默认是**开**的
///         ② 默认配置下「草稿直接记账」被拒，必须先审核
///
/// 这两个用例是 api.rs 夹具显式 `enable_audit: false` 的正当性来源——夹具关掉
/// 是为了让别的用例专注自己的主题，默认值本身由这里盯住。
#[tokio::test]
async fn audit_default_on_for_new_books() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;

    // 生产路径：POST /api/books 建的新账套
    let (status, body) = create_book(&state, &admin_sid, "默认审核").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &admin_sid, &key).await, StatusCode::OK);

    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &admin_sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let o: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(
        o["enable_audit"], true,
        "新建账套的审核环节必须默认开启（制单/审核/记账三权分离）"
    );
    // 出纳签字现在也**默认开**。原来是 false，理由是「资金域节点不是全行业默认」——
    // 但那等于新建账套一出生就在默认绕过出纳：谁都不必做任何决定，就已经在绕了。
    // 与审核环节保持同一取向（默认开、要关得在账套参数里显式关，是一次有意识的决定）。
    assert_eq!(
        o["require_cashier"], true,
        "新建账套的出纳签字必须默认开启（现金/银行是内控风险最高的一块）"
    );
    // 建账向导要问「有没有出纳」：选了不启用才写 false
    let mut o2 = o.clone();
    o2["require_cashier"] = serde_json::json!(false);
    let r = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &admin_sid, o2))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "显式关掉出纳签字应当允许");
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &admin_sid))
        .await
        .unwrap();
    let back: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(
        back["require_cashier"], false,
        "关掉后应能读回 false（显式关闭必须真的生效）"
    );
}

/// 默认配置下审核是硬闸门：未审核不能记账；审核后可以；且错误信息要说清下一步。
#[tokio::test]
async fn audit_gate_blocks_post_until_audited() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let (status, body) = create_book(&state, &admin_sid, "审核闸门").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &admin_sid, &key).await, StatusCode::OK);

    // 录一张草稿凭证（对方科目用 1001 库存现金，无需辅助核算）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &admin_sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记", "no": 1,
                "attachments": 0, "memo": "审核闸门用例",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "应收", "debit": "1000", "credit": "0",
                      "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "1001", "summary": "收现", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(r).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 草稿直接记账 → 400，且提示要先审核
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "未审核不应能记账");
    let msg = body_string(r).await;
    assert!(
        msg.contains("审核"),
        "错误信息应指向「先审核」这个下一步动作，而不是只说失败：{msg}"
    );

    // 审核通过后再记账 → 200
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/audit"),
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "审核应成功");
    // 本用例自建账套，走的是**生产默认**（两道闸门都开）。凭证贷记 1001 库存现金，
    // 所以审核过了还要过出纳签字那道闸 —— 补上这一步，用例才真的走完「记账」这条路。
    //
    // 顺带钉住一件事：出纳闸门**不是**审核的替代品。反过来（没审核就签字能不能记）
    // 由前面的 400 断言已经覆盖。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/sign"),
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(r, "出纳签字应成功").await;
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(r, "审核 + 签字后应能记账").await;
}

// ═══════════════════════════════════════════════════════════════
// 开放 API（/api/v1/*）
// ═══════════════════════════════════════════════════════════════

/// 签一把绑 b1 账套的只读密钥，返回明文
async fn issue_ro_key(state: &Arc<WebState>, book_key: &str) -> String {
    let (_, secret) = state
        .realm
        .api_key_create("测试只读", book_key, "", "", "boss")
        .expect("签发密钥失败");
    secret
}

async fn v1_get(state: &Arc<WebState>, uri: &str, secret: &str) -> (StatusCode, String) {
    let r = handlers::router(state.clone())
        .oneshot(bearer_get(uri, secret))
        .await
        .unwrap();
    (r.status(), body_string(r).await)
}

/// 无密钥 / 错密钥 / 已停用一律 401，且提示要带 Authorization
#[tokio::test]
async fn v1_requires_bearer_key() {
    let (state, _bd, _dir) = test_state();

    let (st, body) = v1_get(&state, "/api/v1/accounts", "").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "不带密钥应 401：{body}");
    assert!(
        body.contains("Authorization"),
        "401 应说清缺什么，而不是只说未授权：{body}"
    );

    let (st, _) = v1_get(&state, "/api/v1/accounts", "fbk_0000000000000000").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "错密钥应 401");

    // 格式对但库里没有的 32 字节密钥
    let ghost = format!("fbk_{}", "0".repeat(64));
    let (st, _) = v1_get(&state, "/api/v1/accounts", &ghost).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "不存在的密钥应 401");

    // 停用后立即失效——「停用」若不立刻生效，泄露事故的处置窗口就不存在了
    let key = issue_ro_key(&state, "b1").await;
    let (st, _) = v1_get(&state, "/api/v1/accounts", &key).await;
    assert_eq!(st, StatusCode::OK, "刚签发的密钥应可用");

    let id = state
        .realm
        .api_key_list()
        .expect("列密钥失败")
        .into_iter()
        .find(|k| k.book_key == "b1")
        .expect("应能找到刚签的密钥")
        .id;
    state
        .realm
        .api_key_set_disabled(id, true)
        .expect("停用失败");
    let (st, body) = v1_get(&state, "/api/v1/accounts", &key).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "停用后应立即 401：{body}");
}

/// 过期密钥不能过——expire_at 不是装饰字段
#[tokio::test]
async fn v1_expired_key_rejected() {
    let (state, _bd, _dir) = test_state();
    let past = (chrono::Local::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let (_, key) = state
        .realm
        .api_key_create("已过期", "b1", "", &past, "boss")
        .expect("签发失败");
    let (st, body) = v1_get(&state, "/api/v1/accounts", &key).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "过期密钥应 401：{body}");
}

/// 库只存哈希：库里任何地方都不该出现明文密钥
#[tokio::test]
async fn v1_key_stored_as_hash_only() {
    let (state, _bd, _dir) = test_state();
    let key = issue_ro_key(&state, "b1").await;
    assert!(key.starts_with("fbk_"), "密钥应有可辨识前缀：{key}");

    let listed = state.realm.api_key_list().expect("列密钥失败");
    let row = listed.first().expect("应有一把密钥");
    // 列表接口只回前缀，不回任何可用于鉴权的完整凭据
    assert!(
        !row.prefix.contains(&key) || row.prefix.len() < key.len(),
        "列表不应暴露完整密钥：{:?}",
        row.prefix
    );
    // 直接查库：哈希不等于明文
    let dump = { state.realm.debug_dump_api_keys() };
    assert!(
        !dump.contains(&key),
        "库中出现了明文密钥（只该存哈希）：{dump}"
    );
}

/// 绑账套的密钥只能读自己那个账套；平台级密钥必须显式给 X-Book-Key
#[tokio::test]
async fn v1_key_is_scoped_to_its_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let (st, body) = create_book(&state, &admin_sid, "第二账套").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let other = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();

    // 绑 b1 的密钥请求另一个账套不存在——v1 路径里没有账套参数，
    // 目标账套只由密钥绑定决定，所以这里验证的是「换个密钥才能换账套」
    let key_b1 = issue_ro_key(&state, "b1").await;
    let (st, body) = v1_get(&state, "/api/v1/accounts", &key_b1).await;
    assert_eq!(st, StatusCode::OK);
    let company = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert!(company.is_object(), "科目表应返回对象：{body}");

    // 平台级密钥（book_key 空）：不带 X-Book-Key → 400；带了就正常
    let (_, plat) = state
        .realm
        .api_key_create("平台级", "", "", "", "boss")
        .expect("签发失败");
    let r = handlers::router(state.clone())
        .oneshot(bearer_get("/api/v1/accounts", &plat))
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::BAD_REQUEST,
        "平台级密钥不给账套应 400，而不是默默挑一个"
    );
    let msg = body_string(r).await;
    assert!(msg.contains("X-Book-Key"), "应指明用哪个头：{msg}");

    for k in [&other, "b1"] {
        let r = handlers::router(state.clone())
            .oneshot(bearer_get_for_book("/api/v1/accounts", &plat, k))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK, "X-Book-Key={k} 应可用");
    }
}

/// 只读密钥能读基础档案，但碰不到需要 fin_report 的报表
#[tokio::test]
async fn v1_readonly_scope_blocks_reports() {
    let (state, _bd, _dir) = test_state();
    let key = issue_ro_key(&state, "b1").await;

    for uri in [
        "/api/v1/accounts",
        "/api/v1/items",
        "/api/v1/me",
        "/api/v1/vouchers",
        "/api/v1/statements",
    ] {
        let (st, body) = v1_get(&state, uri, &key).await;
        assert_eq!(st, StatusCode::OK, "只读密钥应能读 {uri}：{body}");
    }

    for uri in ["/api/v1/trial-balance", "/api/v1/ledger?code=1001"] {
        let (st, body) = v1_get(&state, uri, &key).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "只读密钥不应能读 {uri}：{body}");
        assert!(
            body.contains("只读"),
            "403 应说清当前是只读范围，而不是只说权限不足：{body}"
        );
    }
}

/// 给了 fin_report 就能读报表
#[tokio::test]
async fn v1_fin_report_scope_allows_reports() {
    let (state, _bd, _dir) = test_state();
    let (_, key) = state
        .realm
        .api_key_create("报表只读", "b1", "fin_report", "", "boss")
        .expect("签发失败");

    let (st, body) = v1_get(&state, "/api/v1/trial-balance", &key).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert!(v["totals"]["begin_debit"].is_string(), "应含合计：{body}");

    let (st, body) = v1_get(&state, "/api/v1/ledger?code=1001", &key).await;
    assert_eq!(st, StatusCode::OK, "{body}");

    // 缺必填参数要给 400 并说明缺哪个，不能 500
    let (st, body) = v1_get(&state, "/api/v1/ledger", &key).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("code"), "应指明缺 code：{body}");
}

/// 非法输入返回 400 而不是 500：开放 API 的调用方是程序，报错要能自纠
#[tokio::test]
async fn v1_bad_input_is_400_not_500() {
    let (state, _bd, _dir) = test_state();
    let (_, key) = state
        .realm
        .api_key_create("报表", "b1", "fin_report", "", "boss")
        .expect("签发失败");

    for uri in [
        "/api/v1/vouchers?status=bogus",
        "/api/v1/vouchers?from=not-a-period",
        "/api/v1/vouchers?page=abc",
    ] {
        let (st, body) = v1_get(&state, uri, &key).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{uri} 应 400：{body}");
    }

    // 账套不存在 → 404（不是 401：密钥本身是有效的，是目标不存在）
    let (_, ghost_key) = state
        .realm
        .api_key_create("孤儿", "no_such_book", "", "", "boss")
        .expect("签发失败");
    let (st, body) = v1_get(&state, "/api/v1/accounts", &ghost_key).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
}

/// 凭证列表分页不重不漏，且不泄露 v1 之外的数据
#[tokio::test]
async fn v1_vouchers_pagination() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(select_book(&state, &admin_sid, "b1").await, StatusCode::OK);

    // 录 7 张草稿。用「银行存款 → 库存现金」这类不需要辅助核算的科目对：
    // 分页用例的主题是分页，不该被辅助核算档案的初始化状态左右。
    for i in 1..=7 {
        let r = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                &admin_sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": format!("2026-01-{:02}", i),
                    "word": "记", "no": i, "attachments": 0, "memo": format!("分页用例{i}"),
                    "entries": [
                        { "line": 1, "account_code": "100201", "summary": "转存", "debit": "100", "credit": "0" },
                        { "line": 2, "account_code": "1001", "summary": "收现", "debit": "0", "credit": "100" }
                    ]
                }),
            ))
            .await
            .unwrap();
        let st = r.status();
        let body = body_string(r).await;
        assert_eq!(st, StatusCode::OK, "第 {i} 张应录入成功：{body}");
    }

    let key = issue_ro_key(&state, "b1").await;
    let mut seen: Vec<i64> = Vec::new();
    for page in 1..=3 {
        let (st, body) = v1_get(
            &state,
            &format!("/api/v1/vouchers?page={page}&page_size=3"),
            &key,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let v = serde_json::from_str::<serde_json::Value>(&body).unwrap();
        let items = v["items"].as_array().expect("items 应是数组");
        for it in items {
            seen.push(it["id"].as_i64().expect("凭证应有 id"));
        }
        if page < 3 {
            assert_eq!(items.len(), 3, "第 {page} 页应满页：{body}");
            assert_eq!(v["has_more"], serde_json::json!(true), "{body}");
        }
    }
    let uniq: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(seen.len(), 7, "三页共应 7 条，实际 {}", seen.len());
    assert_eq!(uniq.len(), 7, "分页结果不应重复：{seen:?}");

    // page_size 上限：page_size=99999 不该真的返回 99999 行
    let (st, body) = v1_get(&state, "/api/v1/vouchers?page_size=99999", &key).await;
    assert_eq!(st, StatusCode::OK);
    let v = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(
        v["page_size"],
        serde_json::json!(500),
        "page_size 应被夹到 500"
    );

    // 单张取回含分录；不存在 → 404
    // 这条同时是**参数段路由**的回归守卫：axum 0.7 的 matchit 只认 `:id`，
    // 写 `{id}` 会被当字面量，`/api/v1/vouchers/1` 匹配不上 → 掉进
    // spa_fallback → 未登录态下返回 401「未登录或会话已失效」。
    // 症状是「单张凭证查不到」，根因却在一个字符，且没有编译错、没有警告。
    let (st, body) = v1_get(&state, &format!("/api/v1/vouchers/{}", seen[0]), &key).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "单张凭证应查得到（参数段路由是否接上？）：{body}"
    );
    let v = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(v["entries"].as_array().map(|a| a.len()), Some(2), "{body}");

    let (st, body) = v1_get(&state, "/api/v1/vouchers/999999", &key).await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "不存在的凭证应 404 而不是别的：{body}"
    );
}

/// v1 不含写操作：任何 /api/v1/* 的 POST 都必须被拒
///
/// 这是 v1 最要紧的一条边界。开放接口一旦有写入口，「谁能改账」就由接口设计决定，
/// 而外部系统数量不可控——第一版只承诺「读得到」，不承诺「写得进」。
#[tokio::test]
async fn v1_has_no_write_endpoints() {
    let (state, _bd, _dir) = test_state();
    let key = issue_ro_key(&state, "b1").await;

    for uri in ["/api/v1/vouchers", "/api/v1/accounts", "/api/v1/items"] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let r = handlers::router(state.clone())
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header(header::AUTHORIZATION, format!("Bearer {key}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            // 405（方法不允许）或 404（无此路由）都算「没有写入口」，
            // 但绝不能是 2xx
            assert!(
                !r.status().is_success(),
                "{method} {uri} 不应成功，实际 {} —— v1 承诺只读",
                r.status()
            );
        }
    }
}

/// v1 绕开会话门禁但**不**绕开鉴权：没有密钥的 /api/v1 一律 401
///
/// 单独盯这一条：api_auth_gate 里给 /api/v1/ 加了前缀豁免（密钥没有 cookie，
/// 套 session_of 会把合法外部调用全 401）。豁免不能变成「不鉴权」。
#[tokio::test]
async fn v1_session_gate_exemption_does_not_mean_open() {
    let (state, _bd, _dir) = test_state();
    // 即便带着一个真实有效的浏览器会话，v1 也要密钥——两条鉴权是物理隔离的
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/accounts")
                .header(header::COOKIE, admin_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "只有 cookie 没有密钥，v1 应 401 —— 会话不能顶替密钥"
    );
}

/// 只有 health 与 openapi.json 是免鉴权的，其余都要密钥
#[tokio::test]
async fn v1_public_endpoints_limited_to_health_and_spec() {
    let (state, _bd, _dir) = test_state();
    for uri in ["/api/v1/health", "/api/v1/openapi.json"] {
        let (st, body) = v1_get(&state, uri, "").await;
        assert_eq!(st, StatusCode::OK, "{uri} 应免鉴权：{body}");
    }
    let v = serde_json::from_str::<serde_json::Value>(
        &v1_get(&state, "/api/v1/openapi.json", "").await.1,
    )
    .unwrap();
    assert_eq!(v["openapi"], serde_json::json!("3.0.3"));
    let paths = v["paths"].as_object().expect("paths 应是对象");
    assert!(paths.contains_key("/api/v1/health"));

    let key = issue_ro_key(&state, "b1").await;
    for (p, spec) in paths {
        assert!(p.starts_with("/api/v1/"), "文档路径应在 v1 下：{p}");
        // OpenAPI 规范里参数段写作 {id}，实现（axum 0.7/matchit 0.7）要写 :id。
        // 这里把 {x} 换成真值，既符合规范也顺便验证参数段路由真的接上了。
        let method = p.replace("{id}", "1");
        let public = spec["get"]["security"].as_array().is_some(); // "security": [] = 显式免鉴权
                                                                   // 判据用「带真密钥也该通」而不是「不带密钥该 401」——
                                                                   // 后者有个陷阱：路径拼错（如把 :id 写成 {id}）会掉进 spa_fallback，
                                                                   // 未登录时**也**返回 401，于是「缺密钥→401」这个检查照样通过，
                                                                   // 路由其实是死的。带密钥再打一次才能区分「鉴权生效」和「路由不存在」。
        let (st, body) = v1_get(&state, &method, &key).await;
        // 判「路由没接上」不能只看状态码：handler 自己的 404（凭证不存在）
        // 与 spa_fallback 的 404（路由不存在）状态码完全一样，只有消息不同。
        assert!(
            !(st == StatusCode::UNAUTHORIZED || body.contains("接口不存在")),
            "文档里的 {p} 带合法密钥返回 {st}「{body}」—— 路由没接上"
        );
        // 不带密钥：免鉴权的两条照常通，其余必须 401
        let (st2, body2) = v1_get(&state, &method, "").await;
        if public {
            assert_eq!(st2, StatusCode::OK, "{p} 声明免鉴权却 {st2}：{body2}");
        } else {
            assert_eq!(st2, StatusCode::UNAUTHORIZED, "{p} 无密钥应 401：{body2}");
        }
    }
}

/// 浏览器那套端点没有被 v1 的豁免顺带放开
#[tokio::test]
async fn v1_exemption_does_not_loosen_browser_api() {
    let (state, _bd, _dir) = test_state();
    // 未登录访问普通 /api/* 仍应是 401
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/accounts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "加 /api/v1/ 豁免不应放宽 /api/accounts"
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

/// 签发响应带明文，列表响应不带；停用后立刻 401；删除后彻底不见
#[tokio::test]
async fn api_key_lifecycle_over_http() {
    let (state, _bd, _dir) = test_state();
    let boss_sid = login(&state, "boss", "Admin!2026").await.1;

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/api-keys",
            &boss_sid,
            serde_json::json!({ "name": "仓库对接", "book_key": "b1", "scopes": "", "expires_at": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v = serde_json::from_str::<serde_json::Value>(&body_string(r).await).unwrap();
    let secret = v["secret"].as_str().expect("签发响应应带明文").to_string();
    let id = v["id"].as_i64().expect("应带 id");
    assert!(secret.starts_with("fbk_"), "{secret}");

    // 列表里绝不能有明文
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/api-keys", &boss_sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let list_body = body_string(r).await;
    assert!(
        !list_body.contains(&secret),
        "列表泄露了明文密钥：{list_body}"
    );

    // 停用 → 立刻 401
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/api-keys/{id}"),
            &boss_sid,
            serde_json::json!({ "disabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let (st, _) = v1_get(&state, "/api/v1/accounts", &secret).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "停用应立即生效");

    // 重新启用 → 恢复
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/api-keys/{id}"),
            &boss_sid,
            serde_json::json!({ "disabled": false }),
        ))
        .await
        .unwrap();
    let (st, _) = v1_get(&state, "/api/v1/accounts", &secret).await;
    assert_eq!(st, StatusCode::OK, "启用后应恢复");

    // 删除 → 彻底不可用
    let r = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/api-keys/{id}"))
                .header(header::COOKIE, boss_sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let (st, _) = v1_get(&state, "/api/v1/accounts", &secret).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "删除后应不可用");
}

/// 签发参数校验：不给名字、给不存在的账套、给未知 scope、给坏日期都要 400
#[tokio::test]
async fn api_key_create_validates_input() {
    let (state, _bd, _dir) = test_state();
    let boss_sid = login(&state, "boss", "Admin!2026").await.1;

    // 每条带上期望状态码：账套不存在是 404（对象不存在），其余是 400（参数不合法）
    for (body, want, why) in [
        (
            serde_json::json!({ "book_key": "b1" }),
            StatusCode::BAD_REQUEST,
            "空名称",
        ),
        (
            serde_json::json!({ "name": "x", "book_key": "no_such" }),
            StatusCode::NOT_FOUND,
            "账套不存在",
        ),
        (
            serde_json::json!({ "name": "x", "book_key": "b1", "scopes": "drop_table" }),
            StatusCode::BAD_REQUEST,
            "未知 scope",
        ),
        (
            serde_json::json!({ "name": "x", "book_key": "b1", "expires_at": "明年某天" }),
            StatusCode::BAD_REQUEST,
            "坏日期",
        ),
    ] {
        let r = handlers::router(state.clone())
            .oneshot(authed_post("/api/api-keys", &boss_sid, body))
            .await
            .unwrap();
        let st = r.status();
        let got = body_string(r).await;
        assert_eq!(st, want, "{why}：{got}");
    }

    // 合法 scope 能签发
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/api-keys",
            &boss_sid,
            serde_json::json!({ "name": "报表对接", "book_key": "b1", "scopes": "fin_report" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "{:?}", body_string(r).await);
}

/// 落一套模板并发布，返回流程
async fn apply_tpl(
    state: &Arc<WebState>,
    sid: &str,
    key: &str,
    biz_type: &str,
) -> serde_json::Value {
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows/templates/apply",
            sid,
            serde_json::json!({ "key": key, "biz_type": biz_type }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let body: serde_json::Value = serde_json::from_str(&body_string(r).await)
        .unwrap_or_else(|e| panic!("模板【{key}】@ {biz_type} 应用失败（{st}）：{e}"));
    // 端点包一层 {flow:{…}}，流程本身在 flow 里
    let f = body
        .get("flow")
        .cloned()
        .unwrap_or_else(|| panic!("响应里应含 flow：{body}"));
    assert_eq!(f["status"], serde_json::json!("draft"), "模板应落草稿");
    let id = f["id"].as_i64().expect("流程应带 id");
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/workflows/{id}/publish"),
            sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    assert_eq!(st, StatusCode::OK, "发布流程应成功（{st}）");
    f
}

async fn po_status_of(state: &Arc<WebState>, book: &str, id: i64) -> String {
    state
        .db_for(book)
        .unwrap()
        .conn()
        .query_row(
            "SELECT status FROM purchase_order WHERE id=?1",
            rusqlite::params![id],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
}

async fn prod_status_of(state: &Arc<WebState>, book: &str, id: i64) -> String {
    state
        .db_for(book)
        .unwrap()
        .conn()
        .query_row(
            "SELECT status FROM production_order WHERE id=?1",
            rusqlite::params![id],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
}

/// 走真实 HTTP 入口建一张采购订单，返回 id
async fn make_po(state: &Arc<WebState>, sid: &str) -> i64 {
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-15",
                "supplier_code": "S01", "supplier_name": "供应商甲",
                "status": "Draft",
                "lines": [{
                    "item_code": "140301", "item_name": "原材料",
                    "qty_ordered": "10", "unit_price": "100", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value =
        serde_json::from_str(&body_string(r).await).unwrap_or_else(|e| panic!("{st}：{e}"));
    assert_eq!(st, StatusCode::OK, "建采购订单应成功：{v}");
    v["id"].as_i64().expect("应返回订单 id")
}

/// 审批流覆盖到采购订单：发布流程后，「确认」不再立即改状态，而是先走节点链。
///
/// 这是新增单据类型最容易漏的一环——引擎支持了 `intercept`，但业务入口
/// 没插闸门，结果配了流程却永远不生效、且不报任何错。所以这里从 HTTP 层
/// 走一遍：建订单 → 发流程 → 首次确认返回 pending 且状态**不变** → 批到终态
/// 才真的确认。
#[tokio::test]
async fn workflow_gates_purchase_order_confirm() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let po_id = make_po(&state, &sid).await;

    apply_tpl(&state, &sid, "order_standard", "purchase_order").await;

    // 第一次「确认」：只推进流程，不改订单状态
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v["pending"].is_string(),
        "配了流程后首次确认应返回下一节点：{v}"
    );
    // 关键：订单状态**不能**已经被确认
    let s = po_status_of(&state, "b1", po_id).await;
    assert_ne!(s, "Confirmed", "未批完就确认 = 审批流形同虚设（当前 {s}）");

    // 第二次确认：走到终态，订单才真的被确认
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let body2 = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "{body2}");
    assert_eq!(
        po_status_of(&state, "b1", po_id).await,
        "Confirmed",
        "批到终态后应真的确认"
    );
}

/// 生产订单的「下达」是审批动作：草稿必须先过流程才能下达
#[tokio::test]
async fn workflow_gates_production_order_release() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建一张生产订单（老入口创建即「已下达」，改成草稿以走新路径）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "F01", "qty": "300", "work_center": "W01" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "建生产订单应成功：{v}");
    let pid = v["id"].as_i64().expect("应返回 id");
    state
        .db_for("b1")
        .unwrap()
        .conn()
        .execute(
            "UPDATE production_order SET status='draft' WHERE id=?1",
            rusqlite::params![pid],
        )
        .unwrap();

    // 先验：草稿状态下不能直接开工（开工要求已下达）——否则审批就是个摆设
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ne!(
        r.status(),
        StatusCode::OK,
        "草稿不该能直接开工 —— 那是绕过审批的口子"
    );

    apply_tpl(&state, &sid, "prod_two_stage", "production_order").await;

    // 下达：第一次只推进流程，状态仍是草稿
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/release"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["pending"].is_string(), "应返回下一节点：{v}");
    assert_eq!(
        prod_status_of(&state, "b1", pid).await,
        "draft",
        "未批完就下达 = 审批流形同虚设"
    );

    // 批到终态才真的下达（计划员 → 厂长 → 生产负责人 三段）
    for _ in 0..5 {
        let _ = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/prod/{pid}/release"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        if prod_status_of(&state, "b1", pid).await == "released" {
            break;
        }
    }
    assert_eq!(
        prod_status_of(&state, "b1", pid).await,
        "released",
        "批到终态后应真的下达"
    );
}

/// 没有已发布流程时，行为必须与从前完全一致（向后兼容）
#[tokio::test]
async fn no_flow_means_order_confirms_immediately() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let po_id = make_po(&state, &sid).await;
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v.get("pending").is_none(),
        "没有已发布流程时不该返回 pending：{v}"
    );
    assert_eq!(po_status_of(&state, "b1", po_id).await, "Confirmed");
}

/// 条件字段校验：把依赖 amount 的模板套到生产订单上要被拒（它没有金额）
#[tokio::test]
async fn template_requiring_amount_rejected_on_production_order() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 订单分级模板本来就不适用于生产订单（biz_types 不含它）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows/templates/apply",
            &sid,
            serde_json::json!({ "key": "order_tiered", "biz_type": "production_order" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let body = body_string(r).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("不适用"), "应说明不适用：{body}");

    // 模板清单里生产订单可用的是数量模板，且它声明的依赖字段该是 planned_qty
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/workflows/templates", &sid))
        .await
        .unwrap();
    let tpls: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let prod_tpls: Vec<&serde_json::Value> = tpls["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| {
            t["applicable"]
                .as_array()
                .map(|a| a.iter().any(|x| x["biz_type"] == "production_order"))
                .unwrap_or(false)
        })
        .collect();
    assert!(!prod_tpls.is_empty(), "至少应有一套模板适用于生产订单");
    for t in prod_tpls {
        let need: Vec<&str> = t["requires_fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str())
            .collect();
        assert!(
            !need.contains(&"amount"),
            "【{}】声明了 amount，但生产订单没有金额字段——配上去审批即卡死：{t}",
            t["name"]
        );
    }
}

/// 条件字段清单端点：字段集按单据不同，且非法单据要被拒
///
/// 前端以前写死一句「amount/qty/customer_code 等」。用户在生产订单上照着写
/// `amount > 5000` 会一路保存成功、直到单据提交审批才报「条件字段不存在」——
/// 流程建了、实例起了，单据卡死。所以清单必须按单据取，不能写死。
#[tokio::test]
async fn cond_fields_endpoint_is_per_biz_type() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let get = |biz: &str| {
        let state = state.clone();
        let sid = sid.clone();
        let biz = biz.to_string();
        async move {
            let r = handlers::router(state)
                .oneshot(authed_get(
                    &format!("/api/workflows/cond-fields?biz_type={biz}"),
                    &sid,
                ))
                .await
                .unwrap();
            let st = r.status();
            let body = body_string(r).await;
            (st, body)
        }
    };

    let (st, body) = get("claim").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let claim_fields: Vec<String> = v["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    assert!(
        claim_fields.contains(&"amount".to_string()),
        "{claim_fields:?}"
    );

    let (st, body) = get("production_order").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let prod_fields: Vec<String> = v["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect();
    assert!(
        prod_fields.contains(&"planned_qty".to_string()),
        "{prod_fields:?}"
    );
    assert!(
        !prod_fields.contains(&"amount".to_string()),
        "生产订单没有金额，清单里不该出现 amount：{prod_fields:?}"
    );
    assert!(
        claim_fields != prod_fields,
        "两张单据的字段集不该相同，否则说明清单写死了"
    );

    // 采购/销售订单走金额口径（价税合计）
    for biz in ["purchase_order", "sales_order"] {
        let (st, body) = get(biz).await;
        assert_eq!(st, StatusCode::OK, "{biz}：{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let f: Vec<String> = v["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect();
        assert!(f.contains(&"amount".to_string()), "{biz} 缺 amount：{f:?}");
    }

    // 非法单据类型 400 并列出可选值（别让用户面对空清单猜）
    let (st, body) = get("nope").await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("生产订单"), "应列出可选单据：{body}");
}

/// 税务申报表端点：进项只认已认证、参数校验、导出带 BOM
#[tokio::test]
async fn tax_vat_form_endpoints() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造两张销项（含 pending）+ 一张已认证进项 + 一张未认证进项
    async fn add(
        state: &Arc<WebState>,
        sid: &str,
        kind: &str,
        number: &str,
        date: &str,
        net: &str,
        tax: &str,
        status: &str,
    ) {
        let net = fincore::Money::parse(net).unwrap();
        let tax = fincore::Money::parse(tax).unwrap();
        let r = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/invoices",
                sid,
                serde_json::json!({
                    "id": 0,
                    "kind": kind, "code": "044001900111", "number": number, "date": date,
                    "buyer": if kind == "out" { "客户甲" } else { "本企业" },
                    "seller": if kind == "out" { "本企业" } else { "供应商甲" },
                    "amount_tax": format!("{}", net + tax), "amount": format!("{net}"),
                    "tax": format!("{tax}"), "tax_rate": "0.13", "status": status,
                    "memo": ""
                }),
            ))
            .await
            .unwrap();
        let st = r.status();
        let b = body_string(r).await;
        assert_eq!(st, StatusCode::OK, "建发票应成功：{b}");
    }
    add(
        &state,
        &sid,
        "out",
        "FP000001",
        "2026-01-05",
        "1000",
        "130",
        "pending",
    )
    .await;
    add(
        &state,
        &sid,
        "in",
        "FP000002",
        "2026-01-06",
        "500",
        "65",
        "verified",
    )
    .await;
    add(
        &state,
        &sid,
        "in",
        "FP000003",
        "2026-01-07",
        "300",
        "39",
        "pending",
    )
    .await;

    // 取申报表
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/tax/vat?period=2026-01", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let f = &v["form"];
    assert_eq!(f["output_tax"], serde_json::json!("130.00"), "销项 {f}");
    assert_eq!(
        f["input_tax"],
        serde_json::json!("65.00"),
        "进项只应计已认证的 65：{f}"
    );
    assert_eq!(f["payable"], serde_json::json!("65.00"));
    // 未认证进项必须在提示里说出来
    let warns: Vec<&str> = f["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        warns.iter().any(|w| w.contains("尚未认证")),
        "未认证进项必须提示人工处理（否则会计以为数算全了）：{warns:?}"
    );

    // 构成明细：13% 销项档
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/tax/vat/detail?period=2026-01&kind=out&rate=13",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(d["count"], serde_json::json!(1), "{d}");
    // 缺参数 / 非法 kind 要 400
    for uri in [
        "/api/tax/vat/detail?period=2026-01&kind=out",
        "/api/tax/vat/detail?period=2026-01&kind=xx&rate=13",
    ] {
        let r = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{uri} 应 400");
    }

    // 导出：带 BOM（Excel 双击不乱码）+ 口径说明随表走
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/tax/vat/export?period=2026-01", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers().get(header::CONTENT_DISPOSITION).is_some(),
        "导出应带 attachment 头"
    );
    let bytes = axum::body::to_bytes(r.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(&bytes[0..3], &[0xEF, 0xBB, 0xBF], "CSV 应带 UTF-8 BOM");
    let text = String::from_utf8_lossy(&bytes).to_string();
    for want in [
        "销项税额",
        "130.00",
        "城市维护建设税",
        "进项税额明细",
        "口径说明",
    ] {
        assert!(text.contains(want), "导出表应含「{want}」");
    }
}

/// 税务参数：税率越界 / 非法枚举要 400，且必须能从接口写入
#[tokio::test]
async fn tax_options_validation() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    for (body, why) in [
        (
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "city", "edu_rate": "0.5", "local_edu_rate": "0.02" }),
            "教育费附加 50%",
        ),
        (
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "city", "edu_rate": "-0.03", "local_edu_rate": "0.02" }),
            "负税率",
        ),
        (
            serde_json::json!({ "taxpayer": "alien", "city_tax_zone": "city", "edu_rate": "0.03", "local_edu_rate": "0.02" }),
            "非法纳税人身份",
        ),
        (
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "mars", "edu_rate": "0.03", "local_edu_rate": "0.02" }),
            "非法城建区",
        ),
        (
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "city", "edu_rate": "abc", "local_edu_rate": "0.02" }),
            "非数字税率",
        ),
    ] {
        let r = handlers::router(state.clone())
            .oneshot(authed_post("/api/tax/options", &sid, body))
            .await
            .unwrap();
        let st = r.status();
        let b = body_string(r).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{why} 应 400：{b}");
    }

    // 合法值可写入，且**真的影响附加税费计算**
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/tax/options",
            &sid,
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "county", "edu_rate": "0.03", "local_edu_rate": "0.02" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices",
            &sid,
            serde_json::json!({
                "id": 0,
                "kind": "out", "code": "044001900111", "number": "FP900001",
                "date": "2026-01-05", "buyer": "客户甲", "seller": "本企业",
                "amount_tax": "1130", "amount": "1000", "tax": "130",
                "tax_rate": "0.13", "status": "pending", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "{b}");

    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/tax/vat?period=2026-01", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    // 县城档 5%：130 × 5% = 6.50（默认市区是 9.10，能看出参数确实生效）
    assert_eq!(
        v["form"]["surcharge"]["city_tax"],
        serde_json::json!("6.50"),
        "改了适用地区就该改税率：{v}"
    );
}

/// 标准成本差异端点：价格差异可正可负，且必须能追到构成流水
#[tokio::test]
async fn cost_variance_endpoint() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 配一个标准成本存货：入库价 9，标准 7
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "standard", "standard_cost": "7" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "配置标准成本应成功");

    // 录一张采购入库 100 × 9。
    // 出入库流水是**凭证/采购入库驱动**的（没有直接的 HTTP 库存入库端点），
    // 所以这里用引擎造数，端点只负责读+算——这正是它要验的东西。
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("9").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: "买贵了".into(),
            },
        )
        .unwrap();
    }

    // 出差异表：买贵 2 元 × 100 = 200
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/cost/material-variance?period=2026-01",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let row = v["rows"]
        .as_array()
        .expect("rows 应是数组")
        .iter()
        .find(|x| x["item"] == "140301")
        .expect("应有 140301 的差异行");
    assert_eq!(row["actual_unit"], serde_json::json!("9.00"));
    assert_eq!(
        row["price_variance"],
        serde_json::json!("200.00"),
        "买贵 2×100=200"
    );
    assert_eq!(v["total_price"], serde_json::json!("200.00"));
    // 差异必须能追到单据，否则「超支 200」不是可行动的信息
    let ids = row["move_ids"].as_array().expect("应带 move_ids");
    assert!(!ids.is_empty(), "差异行必须带构成流水 id");
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/inventory/move/{}", ids[0].as_i64().unwrap()),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let m: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(m["item"], serde_json::json!("140301"));
    assert_eq!(m["kind_label"], serde_json::json!("采购入库"));

    // 响应里必须声明「算了但没算的」差异项
    let missing: Vec<&str> = v["missing"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    for want in ["用量差异", "效率差异", "固定制造费用差异"] {
        assert!(
            missing.iter().any(|x| x.contains(want)),
            "必须点名没算的差异：{want}；实际 {missing:?}"
        );
    }
}

/// 设了标准成本方法但没录单价 → 差异表点名提示，不静默给 0
#[tokio::test]
async fn cost_variance_flags_missing_standard_cost() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 只设方法，不给标准成本单价
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "standard", "standard_cost": "0" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "standard", "standard_cost": "0" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("9").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
    }

    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/cost/material-variance?period=2026-01",
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert!(
        v["rows"].as_array().map(|a| a.is_empty()).unwrap_or(false),
        "没有标准单价的存货不该进差异表"
    );
    let warns: Vec<&str> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    assert!(
        warns
            .iter()
            .any(|w| w.contains("140301") && w.contains("标准成本单价")),
        "必须点名是哪个存货：{warns:?}"
    );
}

/// 订单驱动全链条勾稽：销售订单 → 发货 → 开票 → 收款 → 应收对账
///
/// 这条用例的主题不是「每个环节能不能跑」，而是**贯穿全程的数字能不能对上**。
/// 单环节测试全绿但勾稽对不上，是 ERP 最常见也最难自查的一类错：
///   销售订单 100 件 / 价税合计 113000
///     → 发货 60 件应确认 60% 收入与应收
///     → 开票应与发货金额一致
///     → 收款应冲减应收
///     → 期末应收余额 = 113000 − 已收，未发货部分仍是应收
/// 任何一环口径错了，这条会红，而单环节测试不会。
#[tokio::test]
async fn order_driven_chain_ties_out() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 1) 销售订单：100 件 × 1000（13% 税）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Draft",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140301", "item_name": "成品甲",
                    "qty_ordered": "100", "unit_price": "1000", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "建销售订单应成功：{v}");
    let so_id = v["id"].as_i64().expect("应返回订单 id");

    // 2) 确认订单（此时不该有任何收入/应收）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "确认订单应成功：{b}");

    // 3) 发货 60 件 → 按比例确认收入与应收
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({
                "so_id": so_id, "period": 202601, "date": "2026-01-10", "qty": "60"
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "发货应成功：{b}");

    // 订单执行状态与执行进度应一致推进。
    //
    // 数值比较而不是字符串比较：之前这里写的是 `assert_ne!(shipped, "0")`，
    // 而实际值是 "0.00" —— **字符串不等，断言通过，bug 却还在**。这类
    // 「能通过但证明不了结论」的断言比没有断言更坏。
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so", &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let so = list["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == so_id)
        .expect("应能查到该订单")
        .clone();
    let shipped: f64 = so["shipped_amount"]
        .as_str()
        .unwrap_or("0")
        .replace(",", "")
        .parse()
        .unwrap_or(-1.0);
    assert!(
        shipped > 0.0,
        "发货后累计发货额不该是 0（状态已 PartialShip，两者是矛盾的）：{so}"
    );
    assert_eq!(
        so["status"],
        serde_json::json!("PartialShip"),
        "发 60/100 应是部分发货：{so}"
    );
    // 行级已发数量也要推进（订单执行进度落到行上）
    let line_shipped: f64 = so["lines"][0]["qty_shipped"]
        .as_str()
        .unwrap_or("0")
        .parse()
        .unwrap_or(-1.0);
    assert!(line_shipped > 0.0, "行级已发数量也不该是 0：{so}");

    // 4) 开票：从销售订单下推，金额应与订单一致
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "从销售订单下推发票应成功：{b}");
    let inv_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["invoice_id"]
        .as_i64()
        .expect("应返回发票 id");
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices", &sid))
        .await
        .unwrap();
    let invs: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let inv = invs["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == inv_id)
        .expect("应能查到该发票")
        .clone();
    assert_eq!(
        inv["kind"],
        serde_json::json!("out"),
        "从销售订单下推的必须是销项发票：{inv}"
    );
    assert_eq!(
        inv["seller"],
        serde_json::json!("测试公司"),
        "销项发票的卖方应是本企业（账套公司名）：{inv}"
    );
    // 销项税额必须与订单税额一致（税率没被算歪）。
    // 用数值比而不是字符串比：订单列表与发票列表的千分位格式不同
    // （"13000.00" vs "13,000.00"），字符串比会给出假失败。
    let so_tax: f64 = so["total_tax"]
        .as_str()
        .unwrap_or("0")
        .replace(",", "")
        .parse()
        .unwrap_or(-1.0);
    let inv_tax: f64 = inv["tax"]
        .as_str()
        .unwrap_or("0")
        .replace(",", "")
        .parse()
        .unwrap_or(-2.0);
    assert!(
        (so_tax - inv_tax).abs() < 0.01,
        "下推发票的税额应等于订单税额：订单 {so_tax} vs 发票 {inv_tax}"
    );

    // 5) 收款 30000 → 应收应等额减少
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/payment",
            &sid,
            serde_json::json!({
                "so_id": so_id, "period": 202601, "date": "2026-01-20", "amount": "30000"
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "收款应成功：{b}");
    let doc_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["doc_id"]
        .as_i64()
        .expect("收款应返回收付款单 id");

    // 5b) 审核收付款单 —— 凭证在这一步才生成（设计如此：审核后生成凭证 + 自动核销）。
    //     漏掉这一步会看到「应收没有减少」，那是流程没走完，不是链路断了。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "审核收付款单应成功：{b}");

    // 6) 把本链路产生的凭证全部记账。
    //    必须记：对账单只统计**已记账**凭证。链路上生成的收入/应收凭证、以及
    //    审核收付款单生成的收款凭证，此刻都还是草稿。
    //    这也正是真实流程：业务单据驱动出凭证 → 财务审核 → 记账。
    {
        let db = state.db_for("b1").unwrap();
        let ids: Vec<i64> = db
            .conn()
            .prepare("SELECT id FROM voucher WHERE period=202601 AND status='draft' ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!ids.is_empty(), "订单链应至少产生收入/应收凭证");
        for vid in ids {
            let r = handlers::router(state.clone())
                .oneshot(authed_post(
                    &format!("/api/vouchers/{vid}/post"),
                    &sid,
                    serde_json::json!({}),
                ))
                .await
                .unwrap();
            let st = r.status();
            let b = body_string(r).await;
            assert_eq!(st, StatusCode::OK, "凭证 {vid} 记账应成功：{b}");
        }
    }

    // 7) 应收对账：能生成，且期末余额 = 订单价税合计 − 已收
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/statements",
            &sid,
            serde_json::json!({
                "kind": "ar", "account": "112201",
                "party_code": "C01", "party_name": "客户甲",
                "from": 202601, "to": 202601
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "生成应收对账单应成功：{b}");
    let sm: serde_json::Value = serde_json::from_str(&b).unwrap();
    // 字段是 end_balance（不是 end_amount）；订单价税合计 = 不含税 + 税额
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };
    let gross_num: f64 = num(&so["total_amount"]) + num(&so["total_tax"]);
    let end_num: f64 = num(&sm["end_balance"]);
    // 应收期末 = **已发货确认的应收** − 已收
    //
    // 关键业务规则：**按发货比例确认收入/应收**，未发货的 40% 不形成应收。
    // 所以这里不能用订单全额算 —— 用全额断言会把「按发货确认」这个正确的行为
    // 判成 bug，或者反过来：哪天有人改成订单全额确认应收，这条也会红。
    // 两条都写死，才能锁住口径本身而不只是锁住数字。
    let shipped_qty: f64 = num(&so["lines"][0]["qty_shipped"]);
    let ordered_qty: f64 = num(&so["lines"][0]["qty_ordered"]);
    assert!(ordered_qty > 0.0, "应能读到订购数量：{so}");
    let shipped_ratio = shipped_qty / ordered_qty;
    let ar_confirmed = gross_num * shipped_ratio;
    let expect = ar_confirmed - 30000.0;
    assert!(
        (end_num - expect).abs() < 0.01,
        "应收期末应为 {expect}（已发货 {}% × 价税合计 {gross_num} = {ar_confirmed} − 已收 30000），\
         实际 {end_num} —— 订单链某处口径断了",
        (shipped_ratio * 100.0).round()
    );
    // 反向守卫：未发货部分不该形成应收（若哪天改成订单全额确认，这条会红）
    assert!(
        end_num < gross_num - 30000.0,
        "未发货部分形成了应收：期末 {end_num} ≥ 订单全额减已收 —— 收入确认口径变了"
    );
}

/// 超发必须被封顶：超发不重复确认收入，但**发货流水也不能多出库存**
///
/// 回归背景：`so_income_voucher_in` 明确「超发不重复确认」（`delta_qty.min(remaining)`），
/// 这只挡住了**收入**。如果发货流水按原始 qty 落库，就会出现「收入没多确认、
/// 库存却多发了」——账实不符，而且没有任何提示。
#[tokio::test]
async fn over_shipment_is_capped_in_stock_too() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Confirmed",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140301", "item_name": "成品甲",
                    "qty_ordered": "10", "unit_price": "100", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(r).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 发 6（合法）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": 202601, "date": "2026-01-10", "qty": "6" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // 再发 6（超发：只剩 4 未发）。接受还是拒绝都不重要，
    // 重要的是**累计发货不能超过订购量**。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": 202601, "date": "2026-01-11", "qty": "6" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    let db = state.db_for("b1").unwrap();
    let shipped_qty: f64 = db
        .conn()
        .query_row(
            "SELECT COALESCE(SUM(ABS(qty)),0) FROM stock_move WHERE item='140301' AND qty<0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        shipped_qty <= 10.0 + 1e-9,
        "累计发货 {shipped_qty} 超过订购量 10 —— 超发只挡了收入没挡库存，账实不符（请求返回 {st}：{b}）"
    );
}

/// 销售订单下推生产订单：产销两单在数据上连起来，且下推量封顶
///
/// 回归背景：`production_order` 以前**没有 so_id**，产出的货与谁要它没有任何
/// 记录对应，「这批货是哪张订单要的」只能靠人肉记忆，缺货时更无法反查
/// 「哪些订单正等着这批料」。
#[tokio::test]
async fn sales_order_pushes_to_production_order() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Confirmed",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140501", "item_name": "成品甲",
                    "qty_ordered": "100", "unit_price": "100", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "建销售订单应成功：{b}");
    let so_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 下推前 ATP：无现货、无在途、已占用 100 → 负数（承诺不了）
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let a: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };
    assert_eq!(num(&a["committed"]), 100.0, "已确认订单应占用 100");
    assert_eq!(num(&a["atp"]), -100.0, "没现货没在途时承诺不了 100 件");

    // 下推生产订单（不填 qty = 认领全部余量）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id, "date": "2026-01-06" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "下推生产订单应成功：{b}");
    let pushed: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        pushed["so_id"],
        serde_json::json!(so_id),
        "必须记住来源销售订单"
    );
    assert_eq!(num(&pushed["planned_qty"]), 100.0);
    // 返回里带上 ATP：在途 +100 正好抵掉占用
    assert_eq!(num(&pushed["atp"]["atp"]), 0.0, "在途抵掉占用后才够承诺");

    // 生产订单列表里能看到来源单号
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod?period=2026-01", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let row = list["orders"]
        .as_array()
        .expect("产单列表应返回数组")
        .iter()
        .find(|x| x["so_id"] == so_id)
        .expect("列表里应能看到这张下推来的产单");
    assert!(
        !row["so_no"].as_str().unwrap_or("").is_empty(),
        "应回显来源销售订单号，否则产销还是对不上：{row}"
    );

    // 全部认领后再推必须被拒（否则产销对不上）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id, "date": "2026-01-07" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert!(
        st.is_client_error(),
        "已全部下推后不该还能再推（拿到 {st}：{b}）"
    );
}

/// 草稿销售订单不能下推生产
#[tokio::test]
async fn draft_sales_order_cannot_push_production() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Draft",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140501", "item_name": "成品甲",
                    "qty_ordered": "10", "unit_price": "100", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(r).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert!(
        st.is_client_error(),
        "草稿订单不该能下推生产（拿到 {st}：{b}）"
    );
}

/// ATP 端点：待检库存不算可用，草稿订单不占用
///
/// 这两条是「宁可少承诺也不能虚承诺」的底线：待检货领不走，草稿还不是承诺。
#[tokio::test]
async fn atp_endpoint_excludes_qc_pending_and_draft() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造待检入库 80 件
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("80").unwrap();
        let p = fincore::Money::parse("5").unwrap();
        let id = findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        db.conn()
            .execute(
                "UPDATE stock_move SET qc_status='pending' WHERE id=?1",
                [id],
            )
            .unwrap();
    }
    // 一张草稿订单要 30 件（不占用）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Draft",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140301", "item_name": "材料甲",
                    "qty_ordered": "30", "unit_price": "5", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140301", &sid))
        .await
        .unwrap();
    let a: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };
    assert_eq!(
        num(&a["on_hand"]),
        0.0,
        "待检库存不可领用，不能算进可承诺量：{a}"
    );
    assert_eq!(num(&a["committed"]), 0.0, "草稿订单还不是承诺：{a}");
    assert_eq!(num(&a["atp"]), 0.0);
    assert!(
        a["formula"].as_str().unwrap_or("").contains("在途"),
        "响应要带上公式，否则数字没法核对：{a}"
    );
}

/// 到岸成本全链：建采购单→入库→录入运费→过账→存货金额被抬高、凭证出
///
/// 这是到岸成本模块存在的理由：不摊运费，存货估值偏低、销售成本低估，
/// **而且「采购价格差异」会算歪** —— 货价便宜、运费贵，差异表却显示「节约」。
#[tokio::test]
async fn landed_cost_end_to_end() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };

    // 存货 100 件 × 8 元 → 结存价值 800
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("8").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
    }
    // 造一张已收货的采购订单
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-03", "status": "Confirmed",
                "supplier_code": "S001", "supplier_name": "供应商甲",
                "lines": [{
                    "item_code": "140301", "item_name": "材料甲",
                    "qty_ordered": "100", "unit_price": "8", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "建采购订单应成功：{b}");
    let po_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .expect("应返回采购订单 id");
    // 记一笔到货（让 fetch_lines_from_po 有已收货行）
    {
        let db = state.db_for("b1").unwrap();
        db.conn()
            .execute(
                "INSERT INTO po_receipt(po_id, period, date, qty, memo) VALUES(?1,202601,'2026-01-05',100,'')",
                [po_id],
            )
            .unwrap();
    }

    // 从采购订单取行：只列已收货的
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/landed-cost/from-po?po_id={po_id}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let from_po: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let rows = from_po["rows"].as_array().expect("rows 应是数组");
    assert_eq!(rows.len(), 1, "已收货 100 件应能取到 1 行：{from_po}");
    assert_eq!(rows[0]["item_code"], serde_json::json!("140301"));
    assert_eq!(num(&rows[0]["qty"]), 100.0, "取到的是已收货量");

    // 保存草稿
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/landed-cost",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-08",
                "supplier_code": "S001", "supplier_name": "供应商甲",
                "basis": "qty", "freight": "100",
                "dr_account": "140301", "cr_account": "220201",
                "lines": [{
                    "po_id": po_id, "item_code": "140301", "item_name": "材料甲",
                    "qty": "100", "amount": "800", "manual_amount": "0"
                }]
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "保存到岸成本单应成功：{b}");
    let lcv_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_eq!(
        num(&serde_json::from_str::<serde_json::Value>(&b).unwrap()["row"]["total_charges"]),
        100.0
    );

    // 过账前存货金额没变
    {
        let db = state.db_for("b1").unwrap();
        let st_ = findb::business::stock_state(
            &db,
            "140301",
            fincore::Period::new(2026, 1).unwrap(),
            fincore::engine::costing::CostMethod::MovingAverage,
        )
        .unwrap();
        assert_eq!(
            st_.amount,
            fincore::Money::parse("800").unwrap(),
            "过账前不该变"
        );
    }

    // 过账
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/landed-cost/{lcv_id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "过账应成功：{b}");
    let posted: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(num(&posted["total_to_stock"]), 100.0);
    assert_eq!(num(&posted["total_to_expense"]), 0.0, "货还在，全部进存货");
    assert!(posted["voucher_id"].as_i64().unwrap_or(0) > 0, "应生成凭证");

    // 存货金额被抬高
    {
        let db = state.db_for("b1").unwrap();
        let st_ = findb::business::stock_state(
            &db,
            "140301",
            fincore::Period::new(2026, 1).unwrap(),
            fincore::engine::costing::CostMethod::MovingAverage,
        )
        .unwrap();
        assert_eq!(
            st_.amount,
            fincore::Money::parse("900").unwrap(),
            "结存金额 800 + 运费 100 = 900"
        );
        assert_eq!(st_.qty, fincore::Money::parse("100").unwrap(), "不该动数量");
    }

    // 重复过账被拒
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/landed-cost/{lcv_id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert!(r.status().is_client_error(), "重复过账必须被拒");
}

/// 到岸成本要计入采购价格差异，否则「节约」是假的
///
/// 回归背景：到岸成本以 `kind=adjust`（qty=0、只有金额）落库，而差异表原来只把
/// `qty > 0` 的入库行算进成本口径 —— 运费被完全排除。货价便宜 5%、运费贵 20% 时
/// 差异表显示「节约」，实际成本反而更高，且数字看着完全正常。
#[tokio::test]
async fn landed_cost_shows_up_in_purchase_variance() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };

    // 标准成本 7；货价 6（便宜 1 元）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "standard", "standard_cost": "7" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("6").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        // 500 到岸成本，memo 必须带「到岸成本」才能被差异表识别
        findb::business::stock_adjust(
            &db,
            fincore::Period::new(2026, 1).unwrap(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 8).unwrap(),
            "140301",
            "",
            fincore::Money::parse("500").unwrap(),
            "到岸成本 LC2026010001 分摊",
        )
        .unwrap();
    }
    findb::business::stock_summary(
        &state.db_for("b1").unwrap(),
        fincore::Period::new(2026, 1).unwrap(),
        fincore::engine::costing::CostMethod::Standard,
    )
    .unwrap();

    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/cost/material-variance?period=2026-01",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let row = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item"] == "140301")
        .expect("应有 140301 差异行");
    assert_eq!(num(&row["landed_cost"]), 500.0, "到岸成本要单列出来");
    assert_eq!(num(&row["in_amount"]), 1100.0, "600 货款 + 500 运费");
    assert_eq!(
        num(&row["price_variance"]),
        400.0,
        "含运费后是超支 4 元 × 100 = 400；不含运费会误报成「节约 100」"
    );
}

/// 货已出清时过账：整笔转费用，存货里一分钱都不能留
#[tokio::test]
async fn landed_cost_on_sold_goods_goes_to_expense() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };
    let p = fincore::Period::new(2026, 1).unwrap();
    let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let db = state.db_for("b1").unwrap();
    let q = fincore::Money::parse("10").unwrap();
    let pr = fincore::Money::parse("8").unwrap();
    for (kind, qty, price, amount) in [
        (
            findb::business::StockKind::Purchase,
            q,
            pr,
            (q * pr).round2(),
        ),
        (
            findb::business::StockKind::Sale,
            q.negated(),
            fincore::Money::ZERO,
            fincore::Money::ZERO,
        ),
    ] {
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: p,
                biz_date: d,
                kind,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty,
                price,
                amount,
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
    }
    // 到岸成本 60，货已卖光 → 必须整笔转费用
    let mut lc = findb::landedcost::LandedCost {
        period: p.ymm(),
        date: "2026-01-08".to_string(),
        supplier_code: "S001".to_string(),
        basis: "qty".to_string(),
        freight: fincore::Money::parse("60").unwrap(),
        dr_account: "140301".to_string(),
        cr_account: "220201".to_string(),
        expense_account: "660201".to_string(),
        ..Default::default()
    };
    let lines = vec![findb::landedcost::LcLine {
        po_id: 1,
        item_code: "140301".into(),
        item_name: "材料甲".into(),
        qty: fincore::Money::parse("10").unwrap(),
        amount: fincore::Money::parse("80").unwrap(),
        ..Default::default()
    }];
    let id = findb::landedcost::save(&db, &mut lc, &lines, "boss").unwrap();
    let vid = findb::landedcost::post(&db, id, "boss").unwrap();
    let after = findb::landedcost::get(&db, id).unwrap().unwrap();
    assert_eq!(
        after.total_to_stock(),
        fincore::Money::ZERO,
        "货已出清不得入存货"
    );
    assert_eq!(
        after.total_to_expense(),
        fincore::Money::parse("60").unwrap()
    );
    // 凭证：借费用 / 贷应付，没有借存货那条
    let v = findb::vouchers::get(&db, vid).unwrap().unwrap();
    assert!(v.balanced());
    assert_eq!(v.entries[0].account_code, "660201", "借方应是费用科目");
    // 结存必须是 0 / 0（幽灵存货检查）
    let st = findb::business::stock_state(
        &db,
        "140301",
        p,
        fincore::engine::costing::CostMethod::MovingAverage,
    )
    .unwrap();
    assert!(
        st.qty.is_zero() && st.amount.is_zero(),
        "不得留下幽灵存货：数量 {} 金额 {}",
        st.qty,
        st.amount
    );
    let _ = (sid, num);
}

/// ATP 的日期口径：未排期的在途**不能**用来承诺交期
///
/// 回归背景：仓里没有工作中心产能数据（`work_center` 是自由文本、`routing.std_hours`
/// 只参与算成本），所以系统编不出真实完工日 —— 只有计划员自己排的 `plan_end` 才
/// 有依据。把有排期和没排期的在途合成一个「在途 500」拿去承诺客户，就是编日期。
#[tokio::test]
async fn atp_endpoint_splits_dated_incoming_and_honours_date_param() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };

    // 现货 100
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("1").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140501".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
    }
    // 两张产单：300 排到 1/20 完工，200 没排期
    {
        let db = state.db_for("b1").unwrap();
        for (qty, plan_end) in [("300", "2026-01-20"), ("200", "")] {
            let per = fincore::Period::new(2026, 1).unwrap();
            let mut o = findb::scm::ProductionOrder {
                id: 0,
                // `prod_save` 不自动取号（handler 才取），直调引擎时必须自己给，
                // 否则撞 UNIQUE 约束、报的是 SQLite 原始错误而不是可读的提示
                no: findb::scm::prod_next_no(&db, per).unwrap(),
                period: fincore::Period::new(2026, 1).unwrap(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 1, 6).unwrap(),
                item_code: "140501".into(),
                item_name: "成品甲".into(),
                planned_qty: fincore::Money::parse(qty).unwrap(),
                completed_qty: fincore::Money::ZERO,
                status: findb::scm::ProdStatus::InProgress,
                work_center: String::new(),
                so_id: 0,
                prepared_by: "boss".into(),
                memo: String::new(),
                order_kind: "inhouse".into(),
                supplier_code: String::new(),
                supplier_name: String::new(),
                plan_start: if plan_end.is_empty() {
                    String::new()
                } else {
                    "2026-01-10".into()
                },
                plan_end: plan_end.into(),
            };
            let id = findb::scm::prod_save(&db, &mut o).unwrap();
            if !plan_end.is_empty() {
                findb::scm::prod_schedule(
                    &db,
                    &[(id, "2026-01-10".to_string(), plan_end.to_string())],
                )
                .unwrap();
            }
        }
    }

    // 不带 date：总量口径，在途分成已排期/未排期
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(num(&v["on_hand"]), 100.0);
    assert_eq!(num(&v["incoming"]), 500.0);
    assert_eq!(num(&v["incoming_dated"]), 300.0, "排了期的才算有交期");
    assert_eq!(num(&v["incoming_undated"]), 200.0, "没排期的要单列");
    assert_eq!(v["earliest_ready"], serde_json::json!("2026-01-20"));

    // date=2026-01-15：1/20 完工的还不算数，只认现货
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501&date=2026-01-15", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(num(&v["incoming"]), 0.0, "1/20 完工的量在 1/15 不可承诺");
    assert_eq!(num(&v["atp"]), 100.0, "1/15 只能承诺现货 100");
    assert_eq!(v["as_of"], serde_json::json!("2026-01-15"));
    // 未排期的部分不能被这个口径吞掉，界面要能同时说「总量」和「有保证的量」
    assert_eq!(num(&v["incoming_undated"]), 200.0);

    // date=2026-01-20：300 算进来；未排期的 200 仍然不算
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501&date=2026-01-20", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(num(&v["incoming"]), 300.0);
    assert_eq!(num(&v["atp"]), 400.0, "现货 100 + 有日期的在途 300");
    assert!(
        v["date_formula"]
            .as_str()
            .unwrap_or("")
            .contains("未排期的在途不计入"),
        "响应要带日期口径说明：{v}"
    );

    // 日期格式错误要明确报错
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501&date=2026/01/20", &sid))
        .await
        .unwrap();
    assert!(r.status().is_client_error(), "日期格式不对必须被拒");
}

/// 排期接口必须拒掉自相矛盾的日期
///
/// 回归背景：`prod_schedule` 原来是**裸写** —— 连日期格式都不查，于是「完工日早于
/// 开工日」能直接进库。而 ATP 的日期承诺要拿 `plan_end` 当依据：排期错了，
/// 承诺日期就是错的，而且没有任何提示。
#[tokio::test]
async fn prod_schedule_rejects_end_before_start() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "10" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "建产单应成功：{b}");
    let id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/schedule",
            &sid,
            // 字段名是 start / end（`SchedItem`），不是 plan_start / plan_end
            serde_json::json!({ "items": [{ "id": id, "start": "2026-01-20", "end": "2026-01-10" }] }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert!(st.is_client_error(), "完工早于开工必须被拒（拿到 {st}）");
    assert!(b.contains("早于开工日"), "要说清原因：{b}");

    // 顺带钉住一个刚被校验暴露的入口缺陷：`SchedItem` 的 start/end 带
    // `#[serde(default)]`，所以**字段名写错就会静默变成空串**。以前这会把
    // 空日期直接写进库（排期看起来成功、实际什么都没排）；现在必须明确报错。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/schedule",
            &sid,
            serde_json::json!({ "items": [{ "id": id, "plan_start": "2026-01-10", "plan_end": "2026-01-20" }] }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert!(
        st.is_client_error(),
        "字段名写错导致日期为空，必须被拒（拿到 {st}）"
    );
    assert!(b.contains("格式"), "要说清是日期格式问题：{b}");
}

/// 备份目录全局共享，但 list/restore 必须按账套隔离，不能跨租户读取/覆盖
#[tokio::test]
async fn backups_isolated_per_book() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let zhang = provision_plain_user(&state, &admin_sid, "zhangy", "Zy12345678").await;
    let li = provision_plain_user(&state, &admin_sid, "liy", "Ly12345678").await;

    let (status, body) = create_book(&state, &zhang, "张记").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let zk = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &zhang, &zk).await, StatusCode::OK);

    let (status, body) = create_book(&state, &li, "李记").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let lk = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &li, &lk).await, StatusCode::OK);

    // 管理员进入张记套后备份（备份/恢复为管理员能力；按套隔离语义不变）
    assert_eq!(select_book(&state, &admin_sid, &zk).await, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/backups",
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "备份应成功");
    let v: serde_json::Value =
        serde_json::from_str(&body_string(resp).await).expect("备份响应应是 JSON");
    let zname = v["name"].as_str().expect("应返回备份文件名").to_string();

    // 李记套的备份列表里不能出现张记的备份（管理员切到李记套上下文查询）
    assert_eq!(select_book(&state, &admin_sid, &lk).await, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &admin_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_string(resp).await;
    assert!(!list.contains(&zname), "不应看到其他账套的备份：{list}");

    // 从李记套恢复张记备份必须被拒（不是 404，而是明确属于别的账套）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/restore",
            &admin_sid,
            serde_json::json!({ "file": zname }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "不应允许恢复其他账套的备份"
    );

    // 回到张记套：本套的备份仍可见（确认过滤没有把范围清空）
    assert_eq!(select_book(&state, &admin_sid, &zk).await, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backups", &admin_sid))
        .await
        .unwrap();
    let own = body_string(resp).await;
    assert!(own.contains(&zname), "本套的备份应可见：{own}");
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

/// 导入专项：各岗位基础资料（aux 重码/坏类型跳过、item 保质期+安全库存、account 类别推断、
/// opening_stock 数量+批次且不生成凭证）+ 模板下载 + 未知 kind 拒。
#[tokio::test]
async fn import_master_data() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_str = dash["current_period"].as_str().unwrap().to_string();

    // 模板下载：CSV + BOM；未知 kind 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/import/template?kind=aux", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "模板下载");
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/csv"), "CSV 类型：{ct}");
    let tpl = body_string(resp).await;
    assert!(tpl.starts_with('\u{feff}'), "模板带 BOM 供 Excel 打开");
    assert!(tpl.contains("类型,编码,名称,备注"), "aux 模板列头：{tpl}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/import/template?kind=bogus", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知模板 kind 拒");

    // aux 导入：2 行有效 + 1 行重码 + 1 行坏类型 → ok2/skipped2
    let aux_csv = "类型,编码,名称,备注\n客户,X01,新客户一,\n供应商,XS01,新供应商,\n客户,X01,重复编码,\n不明类型,BAD1,坏行,\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "aux", "template": "generic", "text": aux_csv }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "aux 导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 2, "有效 2 行：{r}");
    assert_eq!(r["skipped"], 2, "重码+坏类型各跳 1：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/aux?kind=customer", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r.as_array().unwrap().iter().any(|a| a["code"] == "X01"),
        "客户已建档：{r}"
    );
    // 幂等重导：全部跳过
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "aux", "template": "generic", "text": aux_csv }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 0, "重导幂等不重复：{r}");
    assert_eq!(r["skipped"], 4, "4 行全跳过：{r}");

    // item 导入：保质期入 props、安全库存入 item_plan
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "item", "template": "generic",
                "text": "编码,名称,保质期天,安全库存\nXI01,导入存货,30,50\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "item 导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 1, "{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/aux?kind=item", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let it = r
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["code"] == "XI01")
        .unwrap()
        .clone();
    assert_eq!(it["props"]["shelf_life_days"], "30", "保质期入 props：{it}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/item-plan?item=XI01", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["plan"]["safety_stock"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap(),
        50.0,
        "安全库存入 item_plan：{r}"
    );

    // account 导入：类别空按编码首位推（6→费用）；已存在跳过
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "account", "template": "generic",
                "text": "编码,名称,类别,方向,备注\n660099,导入测试费,,,\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "account 导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 1, "{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/accounts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let list = r
        .as_array()
        .cloned()
        .unwrap_or_else(|| r["rows"].as_array().cloned().unwrap_or_default());
    assert!(
        list.iter()
            .any(|a| a["code"] == "660099" && a["name"] == "导入测试费"),
        "科目已建档：{r}"
    );

    // 未知 kind → 400（per-kind 鉴权入口）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "bogus", "template": "generic", "text": "x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知 kind 拒");

    // opening_stock：数量流水 + 批次建档（保质期30天推失效日）；**不生成凭证**
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "opening_stock", "template": "generic",
                "text": "存货编码,仓库,数量,单价,批次号,生产日期,备注\nXI01,W09,40,1.5,OB001,2026-01-01,\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "期初库存导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 1, "{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches?item=XI01", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bt = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["batch_no"] == "OB001")
        .unwrap()
        .clone();
    assert_eq!(bt["warehouse"], "W09", "期初批次建档：{bt}");
    assert_eq!(
        bt["expiry_date"], "2026-01-31",
        "失效日=生产日+保质期30：{bt}"
    );
    assert_eq!(
        bt["balance"].as_str().unwrap().parse::<f64>().unwrap(),
        40.0,
        "期初数量入流水：{bt}"
    );
    // 期初库存不生成凭证（金额侧由科目期初负责）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers?period={}", cur_str.replace('-', "")),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vlist = r
        .as_array()
        .cloned()
        .unwrap_or_else(|| r["rows"].as_array().cloned().unwrap_or_default());
    assert!(vlist.is_empty(), "期初库存不应生成凭证：{r}");
}

/// C 选项 + 导入 v2：存货档案一站式聚合（档案+计划参数+现量+单位）
/// ＋ 往来期初按单据（导入→账龄覆盖→列表合计→删除）。
#[tokio::test]
async fn items_master_and_arap_opening() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // ---- 往来期初：导入 3 行（2 应收 + 1 应付）+ 1 坏类型；幂等重导 ----
    let csv = "类型,客商编码,单据号,单据日期,金额,客商名称,备注\n\
               应收,Q01,XSQ-9001,2025-12-01,5000,青云客户,期初一\n\
               应收,Q01,XSQ-9002,2025-12-15,3000,青云客户,期初二\n\
               应付,S91,CGQ-9001,2025-12-20,2000,远航供应商,\n\
               坏类型,X01,BAD-1,2025-12-01,100,,\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "arap_opening", "template": "generic", "text": csv }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "往来期初导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 3, "3 行有效：{r}");
    assert_eq!(r["skipped"], 1, "坏类型跳过：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "arap_opening", "template": "generic", "text": csv }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 0, "幂等重导：{r}");
    assert_eq!(r["skipped"], 4, "全跳过：{r}");

    // 列表：3 行 + 合计应收 8000 / 应付 2000
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/arap-opening", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(
        money_num(r["total_ar"].as_str().unwrap()),
        8000.0,
        "应收合计：{r}"
    );
    assert_eq!(
        money_num(r["total_ap"].as_str().unwrap()),
        2000.0,
        "应付合计：{r}"
    );
    let first_id = r["rows"][0]["id"].as_i64().unwrap();

    // 账龄覆盖：1122 账龄应含期初客商 Q01（影子行注入）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/aging?account=1122&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "账龄可用");
    let aging_body = body_string(resp).await;
    assert!(aging_body.contains("Q01"), "期初客商应进账龄：{aging_body}");

    // 删除一行 → 2 行
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(
            &format!("/api/arap-opening/{first_id}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除期初行");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/arap-opening", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 2, "删除后 2 行：{r}");

    // ---- 存货档案一站式：导入建档 + 计划参数 + 期初数量 ----
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "item", "template": "generic",
                "text": "编码,名称,保质期天,安全库存\nXM01,档案页存货,45,30\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "存货建档");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/item-plan",
            &sid,
            serde_json::json!({
                "item_code": "XM01", "safety_stock": "30",
                "lead_days": 3, "lot_size": "10"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计划参数保存");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "opening_stock", "template": "generic",
                "text": "存货编码,仓库,数量,单价\nXM01,W01,25,2\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "期初数量");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/items/master", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "存货档案端点");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let it = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["code"] == "XM01")
        .unwrap()
        .clone();
    assert_eq!(it["name"], "档案页存货", "{it}");
    assert_eq!(it["shelf_life"], "45", "保质期来自 props：{it}");
    assert_eq!(money_num(it["safety"].as_str().unwrap()), 30.0, "{it}");
    assert_eq!(it["lead_days"], 3, "{it}");
    assert_eq!(money_num(it["lot"].as_str().unwrap()), 10.0, "{it}");
    assert_eq!(
        money_num(it["qty"].as_str().unwrap()),
        25.0,
        "现量来自流水：{it}"
    );
    assert_eq!(it["disabled"], false, "{it}");
}

/// 链7：数字钻取（科目明细账：期初 + 逐笔 + 运行余额 + 贷余负值）与缺参校验。
#[tokio::test]
async fn report_drill() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 造两笔（借 1001 100 / 借 1001 200）+ 一笔贷方（1001 贷 30 → 余额转负验证方向）。
    // 口径：明细账与试算平衡同为「仅已记账」——每笔保存后立即记账（H-3 同款流程）。
    for (no, entries) in [
        (
            80,
            serde_json::json!([
                { "line": 1, "account_code": "1001", "summary": "drill-a", "debit": "100", "credit": "0" },
                { "line": 2, "account_code": "2001", "summary": "drill-a", "debit": "0", "credit": "100" }
            ]),
        ),
        (
            81,
            serde_json::json!([
                { "line": 1, "account_code": "1001", "summary": "drill-b", "debit": "200", "credit": "0" },
                { "line": 2, "account_code": "2001", "summary": "drill-b", "debit": "0", "credit": "200" }
            ]),
        ),
        (
            82,
            serde_json::json!([
                { "line": 1, "account_code": "1001", "summary": "drill-c", "debit": "0", "credit": "30" },
                { "line": 2, "account_code": "2001", "summary": "drill-c", "debit": "30", "credit": "0" }
            ]),
        ),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                &sid,
                serde_json::json!({
                    "id": 0, "period": cur_ymm, "date": d15.clone(), "word": "记",
                    "no": no, "attachments": 0, "memo": "", "entries": entries
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "造凭证 #{no}");
        let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            ["id"]
            .as_i64()
            .unwrap();
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "记账凭证 #{no}");
    }

    // 明细：本期 from=to → begin=0、3 行、借合计 300、贷合计 30、末行余额 270
    let next_ymm: i32 = {
        let y = cur_ymm / 100;
        let m = cur_ymm % 100;
        if m == 12 {
            (y + 1) * 100 + 1
        } else {
            y * 100 + m + 1
        }
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/account-detail?account=1001&from={cur_ymm}&to={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "明细账可用：{body}");
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 3, "3 行分录：{r}");
    assert_eq!(
        r["begin"].as_str().unwrap().parse::<f64>().unwrap(),
        0.0,
        "本期起无期初"
    );
    assert_eq!(
        r["total_debit"].as_str().unwrap().parse::<f64>().unwrap(),
        300.0,
        "借合计"
    );
    assert_eq!(
        r["total_credit"].as_str().unwrap().parse::<f64>().unwrap(),
        30.0,
        "贷合计"
    );
    let last = r["rows"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        last["balance"].as_str().unwrap().parse::<f64>().unwrap(),
        270.0,
        "运行余额 100+200-30：{last}"
    );
    assert_eq!(last["no"], "记82", "凭证号 word+no 拼接");
    assert_eq!(last["line_memo"], "drill-c", "分录摘要");

    // 期初：from=下期 → 无行、begin=本期累计 270
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/account-detail?account=1001&from={next_ymm}&to={next_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["rows"].as_array().unwrap().is_empty(), "下期无发生：{r}");
    assert_eq!(
        r["begin"].as_str().unwrap().parse::<f64>().unwrap(),
        270.0,
        "期初 = 本期累计：{r}"
    );

    // 缺 account → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/account-detail", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "缺 account 拒");
}

/// 链6：MPS（销售需求+净算+在制扣减+一键下达）→ 粗排（件/日顺排+按日负荷）→ 细排写回。
#[tokio::test]
async fn mps_schedule_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 需求：确认销售订单 140301 ×6（未发量 = 6）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "MPS 造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "6", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // ① MPS（从销售收集）：demand=6、wip=0、planned=6（无库存）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mps/run",
            &sid,
            serde_json::json!({ "from_sales": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "MPS 运行");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_code"] == "140301")
        .unwrap()
        .clone();
    assert_eq!(
        row["demand"].as_str().unwrap().parse::<f64>().unwrap(),
        6.0,
        "需求=未发量：{row}"
    );
    assert_eq!(
        row["wip"].as_str().unwrap().parse::<f64>().unwrap(),
        0.0,
        "初始无在制"
    );
    assert_eq!(
        row["planned"].as_str().unwrap().parse::<f64>().unwrap(),
        6.0,
        "计划=6-0-0：{row}"
    );

    // ② 在制扣减：建生产订单 ×4 → 再跑 MPS → wip=4、planned=2
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140301", "qty": "4", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建在制订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mps/run",
            &sid,
            serde_json::json!({ "from_sales": true }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row2 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_code"] == "140301")
        .unwrap()
        .clone();
    assert_eq!(
        row2["wip"].as_str().unwrap().parse::<f64>().unwrap(),
        4.0,
        "在制=4：{row2}"
    );
    assert_eq!(
        row2["planned"].as_str().unwrap().parse::<f64>().unwrap(),
        2.0,
        "计划扣在制=6-4：{row2}"
    );
    let mps_id = row2["id"].as_i64().unwrap();

    // ③ 一键下达 → 生成生产订单；重复下达 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mps/{mps_id}/convert"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "MPS 下达");
    let conv: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let mps_order_id = conv["order_id"].as_i64().unwrap();
    assert!(
        conv["order_no"].as_str().unwrap().starts_with("SC"),
        "生成生产订单单号"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mps/{mps_id}/convert"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复下达拒");

    // ④ 粗排：日产能 2 → 在制单（open 4 → 2 天）+ MPS 单（open 2 → 1 天）；负荷非空
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mps/rough",
            &sid,
            serde_json::json!({ "daily_qty": "2" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "粗排");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let orders = r["orders"].as_array().unwrap();
    assert!(orders.len() >= 2, "两单参与粗排：{r}");
    let mps_row = orders
        .iter()
        .find(|o| o["id"].as_i64() == Some(mps_order_id))
        .unwrap();
    assert_eq!(
        mps_row["need_days"].as_i64(),
        Some(1),
        "open2/日产能2 → 1 天：{mps_row}"
    );
    let wip_row = orders
        .iter()
        .find(|o| o["id"].as_i64() != Some(mps_order_id))
        .unwrap();
    assert_eq!(
        wip_row["need_days"].as_i64(),
        Some(2),
        "open4/日产能2 → 2 天：{wip_row}"
    );
    assert!(!r["load"].as_array().unwrap().is_empty(), "按日负荷非空");
    assert!(!mps_row["sug_start"].as_str().unwrap().is_empty());

    // ⑤ 细排写回：MPS 单排到 2026-02-01（跨期日期允许）；空清单 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/schedule",
            &sid,
            serde_json::json!({ "items": [{ "id": mps_order_id, "start": "2026-02-01", "end": "2026-02-01" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "细排写回");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["updated"], 1);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let scheduled = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(mps_order_id))
        .unwrap()
        .clone();
    assert_eq!(
        scheduled["plan_start"], "2026-02-01",
        "计划开工已写回：{scheduled}"
    );
    assert_eq!(scheduled["plan_end"], "2026-02-01");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/schedule",
            &sid,
            serde_json::json!({ "items": [] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "空清单拒");

    // ⑥ 空需求 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mps/run",
            &sid,
            serde_json::json!({ "from_sales": false, "demands": [] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "空需求拒");
}

/// 链5：批次盘点（批次账面快照/差异流水带批次/盘盈建档）→ 批次成本勾稽 → 批次调拨（双流水+主仓改写+FEFO）。
#[tokio::test]
async fn chain5_batch_stock() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 仓库主数据（v30）：调拨用的 W01/W02 先建档
    for (code, name) in [("W01", "一号仓"), ("W02", "二号仓")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/warehouses",
                &sid,
                serde_json::json!({ "code": code, "name": name }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建仓 {code}");
    }

    // 造批次：W01 入 10（BT500）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batch",
            &sid,
            serde_json::json!({
                "item": "RM10", "batch_no": "BT500", "production_date": "",
                "warehouse": "W01", "location": "", "qty": "10",
                "direction": "in", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "批次建档");

    // ---- 5a 批次盘点：行带 batch_no → 服务端按批次快照账面（W01=10）----
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/count",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "warehouse": "W01",
                "memo": "批次盘点造数",
                "lines": [{ "item": "RM10", "batch_no": "BT500", "count_qty": "8" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建盘点单");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_i64() == Some(cid))
        .unwrap()
        .clone();
    assert_eq!(doc["lines"][0]["batch_no"], "BT500", "行带批次号");
    assert_eq!(
        doc["lines"][0]["book_qty"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap(),
        10.0,
        "批次账面快照 = W01 分仓余额 10：{doc}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid}/apply"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "应用盘点");
    // 批次余额 = 8（差异流水带 batch_no）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches?item=RM10", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bt = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["batch_no"] == "BT500")
        .unwrap()
        .clone();
    assert_eq!(
        bt["balance"].as_str().unwrap().parse::<f64>().unwrap(),
        8.0,
        "盘点后批次余额 8：{bt}"
    );

    // ---- 5b 批次成本勾稽：明细 + 逐存货合计 ----
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batch-cost", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "批次成本勾稽可用");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["detail"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["item"] == "RM10" && d["batch_no"] == "BT500"),
        "明细含该批次：{r}"
    );
    let t = r["totals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["item"] == "RM10")
        .unwrap()
        .clone();
    assert_eq!(
        t["qty"].as_str().unwrap().parse::<f64>().unwrap(),
        8.0,
        "Σ批次数量 = 8：{t}"
    );
    assert!(
        t.get("book").is_some() && t.get("diff").is_some(),
        "账面与差异列"
    );

    // ---- 5c 批次调拨：W01→W02 双流水 + 主仓改写；超额 400；FEFO 自动选批 ----
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/transfer",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(),
                "item": "RM10", "batch_no": "BT500",
                "from_warehouse": "W01", "to_warehouse": "W02",
                "qty": "3", "memo": "库间调拨"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "调拨执行");
    let tr: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(tr["batch_no"], "BT500");
    assert!(
        tr["out_id"].as_i64().is_some() && tr["in_id"].as_i64().is_some(),
        "两条流水 id"
    );

    // 调拨报表：期间内 Transfer 行含 -3(W01) 与 +3(W02)
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/transfer", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    let out = rows.iter().find(|m| {
        m["warehouse"] == "W01"
            && m["batch_no"] == "BT500"
            && m["qty"].as_str().map(|q| q.starts_with('-')) == Some(true)
    });
    let inn = rows.iter().find(|m| {
        m["warehouse"] == "W02"
            && m["batch_no"] == "BT500"
            && m["qty"].as_str().map(|q| !q.starts_with('-')) == Some(true)
            && m["qty"].as_str() != Some("0")
    });
    assert!(out.is_some(), "调出行（W01 负数）应在报表：{r}");
    assert!(inn.is_some(), "调入行（W02 正数）应在报表：{r}");

    // 批次主仓标签改写 = W02，总量仍 8
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches?item=RM10", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bt = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["batch_no"] == "BT500")
        .unwrap()
        .clone();
    assert_eq!(bt["warehouse"], "W02", "主仓标签改写：{bt}");
    assert_eq!(
        bt["balance"].as_str().unwrap().parse::<f64>().unwrap(),
        8.0,
        "调拨不改变总量"
    );

    // 超额（分仓余额不足）→ 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/transfer",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(),
                "item": "RM10", "batch_no": "BT500",
                "from_warehouse": "W01", "to_warehouse": "W02",
                "qty": "100", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "W01 余额 5 < 100 应拒"
    );

    // FEFO 自动选批（batch_no 空）：W02→W01 ×2，返回选中批号
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/transfer",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(),
                "item": "RM10", "batch_no": "",
                "from_warehouse": "W02", "to_warehouse": "W01",
                "qty": "2", "memo": "FEFO 自动"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "FEFO 自动选批");
    let tr: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(tr["batch_no"], "BT500", "唯一可用批次被 FEFO 选中");
}

/// 发货通知 + 票款勾稽：未确认拒通知/超未发量拒 → 出库自动完成通知 → 收款与发票建边。
#[tokio::test]
async fn ship_notice_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 销售订单（6 件，未发量 6）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "通知造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "6", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未确认 → 拒通知
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/notice"),
            &sid,
            serde_json::json!({ "qty": "4", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未确认不能发通知");

    // 确认 → 超未发量拒 → 正常通知 4
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/notice"),
            &sid,
            serde_json::json!({ "qty": "9", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超未发量拒");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/notice"),
            &sid,
            serde_json::json!({ "qty": "4", "date": d15.clone(), "memo": "备货" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "通知发出");

    // 列表 pending 含本单
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/notices", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["so_id"].as_i64() == Some(so_id) && n["status"] == "pending"),
        "待发通知应含本单：{r}"
    );

    // 出库 → 通知自动完成（pending 不再含本单）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "出库");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/notices", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        !r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["so_id"].as_i64() == Some(so_id) && n["status"] == "pending"),
        "出库后通知应自动完成：{r}"
    );

    // 票款勾稽：下推销项发票 → 订单页收款 → 发票上游见收付款单
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下推销项发票");
    let inv_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["invoice_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/payment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "amount": "10", "memo": "部分收款" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单页收款");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=invoice&id={inv_id}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "receipt" && n["dir"] == "down"),
        "发票下游应见收付款单（票↔款勾稽）：{r}"
    );
}

/// 通知中心：结构/水位已读/可见性过滤 + 单据流程条数据源（instance-for）。
#[tokio::test]
async fn notices_endpoint() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 结构：todos/events/unread_events/now（now=YYYY-MM-DD HH:MM:SS）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/notices", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "通知端点可用");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["todos"].as_array().is_some(), "待办段");
    assert!(r["events"].as_array().is_some(), "动态段");
    let now = r["now"].as_str().unwrap().to_string();
    assert_eq!(now.len(), 19, "now 为 YYYY-MM-DD HH:MM:SS：{now}");

    // 水位推进 → 未读动态归零
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/notices?since={}", now.replace(' ', "%20")),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["unread_events"], 0, "水位后无新动态");

    // 造一条动态（保存工作流会写审计日志）→ 未读 ≥1
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows",
            &sid,
            serde_json::json!({
                "id": 0, "name": "通知造数流", "biz_type": "quotation",
                "nodes": [
                    { "id": "n1", "type": "start", "name": "开始" },
                    { "id": "n2", "type": "approve", "name": "初审" },
                    { "id": "n3", "type": "approve", "name": "复核" }
                ],
                "edges": [
                    { "id": "e1", "from": "n1", "to": "n2", "kind": "normal" },
                    { "id": "e2", "from": "n2", "to": "n3", "kind": "normal" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造流程（产生审计动态）");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/notices", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["unread_events"].as_i64().unwrap() >= 1,
        "应有新动态未读：{r}"
    );

    // instance-for：发布流程 → 建报价审批 → running 实例节点可查（流程条/行徽标数据源）
    let flow_id = /* 用返回 id */ {
        let resp2 = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/workflows",
                &sid,
                serde_json::json!({
                    "id": 0, "name": "通知流程条", "biz_type": "quotation",
                    "nodes": [
                        { "id": "n1", "type": "start", "name": "开始" },
                        { "id": "n2", "type": "approve", "name": "初审" },
                        { "id": "n3", "type": "approve", "name": "复核" }
                    ],
                    "edges": [
                        { "id": "e1", "from": "n1", "to": "n2", "kind": "normal" },
                        { "id": "e2", "from": "n2", "to": "n3", "kind": "normal" }
                    ]
                }),
            ))
            .await
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&body_string(resp2).await).unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/workflows/{flow_id}/publish"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发布流程");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20",
                "customer_code": "C09", "customer_name": "客户九",
                "item_code": "140301", "item_name": "原料", "qty": "3", "unit_price": "2",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let qid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "首次审批（入流）");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/workflows/instance-for?biz_type=quotation&id={qid}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["found"], serde_json::json!(true), "实例应存在：{r}");
    assert_eq!(r["status"], "running");
    assert_eq!(r["current_label"], "复核", "首节点推进后当前=复核：{r}");

    // 无实例的单据 → found=false
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/workflows/instance-for?biz_type=quotation&id=99999",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["found"], serde_json::json!(false));
    // 非法业务类型 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/workflows/instance-for?biz_type=bogus&id=1",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// 发票↔单据勾稽 + 进项认证：订单下推发票（doc_link 双向）→ 认证流转。
#[tokio::test]
async fn invoice_push_and_certify() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 采购链：带价 PO → 到货 → 下推进项发票（金额=整单 90，待认证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d15.clone(), "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-po",
            &sid,
            serde_json::json!({ "po_id": po_id }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let ib = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "PUSH_INV_ERR={ib}");
    let inv_id = serde_json::from_str::<serde_json::Value>(&ib).unwrap()["invoice_id"]
        .as_i64()
        .unwrap();

    // 发票内容：进项、供应商、金额90、待认证
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?kind=in", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let inv = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(inv_id))
        .unwrap();
    assert_eq!(inv["kind"], "in");
    assert_eq!(inv["seller"], "供应商甲");
    assert_eq!(money_num(inv["amount"].as_str().unwrap()), 90.0);
    assert_eq!(inv["status"], "pending", "下推即待认证");

    // 勾稽双向：PO 下游见发票；发票上游见 PO
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=po&id={po_id}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "invoice"
                && n["dir"] == "down"
                && n["id"].as_i64() == Some(inv_id)),
        "PO 下游应见发票：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=invoice&id={inv_id}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "po" && n["dir"] == "up" && n["id"].as_i64() == Some(po_id)),
        "发票上游应见 PO：{r}"
    );

    // 进项认证流转：pending → verified
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/invoices/{inv_id}/status"),
            &sid,
            serde_json::json!({ "status": "verified" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "认证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?kind=in", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let inv = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(inv_id))
        .unwrap();
    assert_eq!(inv["status"], "verified", "认证后状态");

    // 0 额订单 → 下推拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "1", "unit_price": "0", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let zero_po = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-po",
            &sid,
            serde_json::json!({ "po_id": zero_po }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "0额订单不能下推发票"
    );

    // 销售链：SO 确认发货 → 下推销项发票（整单金额 20）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "开票造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "4", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下推销售发票");
    let out_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["invoice_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?kind=out", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let inv = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(out_id))
        .unwrap();
    assert_eq!(inv["kind"], "out");
    assert_eq!(
        money_num(inv["amount"].as_str().unwrap()),
        20.0,
        "销项金额=订单整单"
    );
    assert_eq!(inv["buyer"], "客户甲");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=so&id={so_id}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "invoice" && n["dir"] == "down"),
        "SO 下游应见发票：{r}"
    );
}

/// 来料检验状态机（对标金蝶质检管理）：qc_required 存货到货入待检 →
/// 合格转正/不合格隔离（部分不合格拆分）→ 可用/待检/隔离三口径；非检验存货直通。
#[tokio::test]
async fn qc_state_machine() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 建检验存货并启用来料检验
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({ "id": 0, "kind": "item", "code": "RM50", "name": "待检料", "disabled": false, "memo": "", "props": { "qc_required": "1" } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建存货档案");

    let mk_po = |qty: String, price: String| {
        let st = state.clone();
        let sd = sid.clone();
        let dd = d15.clone();
        async move {
            let resp = handlers::router(st)
                .oneshot(authed_post(
                    "/api/procure/po",
                    &sd,
                    serde_json::json!({
                        "period": cur_ymm, "date": dd, "supplier_code": "S01",
                        "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                        "lines": [{ "item_code": "RM50", "qty_ordered": qty, "unit_price": price, "tax_rate": "0" }]
                    }),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "建采购订单");
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
                .as_i64()
                .unwrap()
        }
    };
    // PO1：到货10 → 待检（available=0, pending=10）
    let po1 = mk_po("10".to_string(), "5".to_string()).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po1, "period": cur_ymm, "date": d15.clone(), "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["qc_pending"],
        serde_json::json!(true),
        "检验存货到货应标记待检：{r}"
    );
    let get = |state: Arc<WebState>, sid: String, item: String| async move {
        let resp = handlers::router(state)
            .oneshot(authed_get(
                &format!("/api/inventory/warehouse-stock?item={item}"),
                &sid,
            ))
            .await
            .unwrap();
        body_string(resp).await
    };
    let b = get(state.clone(), sid.clone(), "RM50".to_string()).await;
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        money_num(v["rows"][0]["qty"].as_str().unwrap()),
        10.0,
        "结存10"
    );
    assert_eq!(
        money_num(v["rows"][0]["available"].as_str().unwrap()),
        0.0,
        "待检期可用为0"
    );
    assert_eq!(
        money_num(v["rows"][0]["pending"].as_str().unwrap()),
        10.0,
        "待检10"
    );

    // 质检部分不合格3 → 合格7转正 + 隔离3（拆分）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/qc",
            &sid,
            serde_json::json!({ "po_id": po1, "qty_insp": "10", "qty_fail": "3", "inspector": "质检员", "date": d15.clone(), "memo": "外观不良" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "质检保存");
    let b = get(state.clone(), sid.clone(), "RM50".to_string()).await;
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        money_num(v["rows"][0]["available"].as_str().unwrap()),
        7.0,
        "合格7转正"
    );
    assert_eq!(
        money_num(v["rows"][0]["quarantine"].as_str().unwrap()),
        3.0,
        "不合格3隔离"
    );
    assert_eq!(
        money_num(v["rows"][0]["pending"].as_str().unwrap()),
        0.0,
        "待检清零"
    );

    // PO2：到货5 → 全不合格 → 隔离+5、待检清零
    let po2 = mk_po("5".to_string(), "5".to_string()).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po2, "period": cur_ymm, "date": d15.clone(), "qty": "5", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/qc",
            &sid,
            serde_json::json!({ "po_id": po2, "qty_insp": "5", "qty_fail": "5", "date": d15.clone(), "memo": "整批不良" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "全不合格质检");
    let b = get(state.clone(), sid.clone(), "RM50".to_string()).await;
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        money_num(v["rows"][0]["quarantine"].as_str().unwrap()),
        8.0,
        "隔离累计8"
    );
    assert_eq!(
        money_num(v["rows"][0]["pending"].as_str().unwrap()),
        0.0,
        "无待检余量"
    );

    // 非检验存货（140301 未勾检验）→ 到货直通可用
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "6", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po3 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po3, "period": cur_ymm, "date": d15.clone(), "qty": "6", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["qc_pending"], serde_json::json!(false), "未勾检验直通");
    let b = get(state.clone(), sid.clone(), "140301".to_string()).await;
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        money_num(v["rows"][0]["available"].as_str().unwrap()),
        6.0,
        "直通可用=结存"
    );
}

/// 销售成本结转（Web 化，对标金蝶存货核算-凭证生成）：仅统计销售出库（kind=sale，
/// 领料/形态转换不进 6401）；无出库不生成凭证；同期间防重复结转。
#[tokio::test]
async fn sales_cost_cutover() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let d15 = format!("{cur_label}-15");
    let d12 = format!("{cur_label}-12");
    let d20 = format!("{cur_label}-20");

    // a) 采购入库 10×9（库存基础；此时无销售出库）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d12.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d12.clone(), "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购入库");

    // b) 尚无销售出库 → 结转返回 none（不生成凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/sales-cost",
            &sid,
            serde_json::json!({ "period": cur_ymm, "method": "moving_average", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["none"],
        serde_json::json!(true),
        "无销售出库不生成凭证：{r}"
    );

    // c) 销售订单确认 → 发货 4（kind=sale 出库）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "结转造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "4", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货");

    // d) 形态转换出 2 件（kind=other_out，验证不被结转进 6401）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/form-convert",
            &sid,
            serde_json::json!({ "from_item": "140301", "to_item": "140501", "qty": "2", "date": d15.clone(), "memo": "转产" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "形态转换");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140301",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let on_hand: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(on_hand, 4.0, "10-4-2=4（sale4 + other_out2 均已扣数量）");

    // e) 结转 → 6401 借 = 仅销售发出 4×9=36（不含形态转换的 2 件）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/sales-cost",
            &sid,
            serde_json::json!({ "period": cur_ymm, "method": "moving_average", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    let st2 = resp.status();
    let cbody = body_string(resp).await;
    assert_eq!(st2, StatusCode::OK, "CUTOVER_ERR={cbody}");
    let r: serde_json::Value = serde_json::from_str(&cbody).unwrap();
    let vid = r["voucher_id"].as_i64().unwrap_or(0);
    assert!(vid > 0, "应生成结转凭证：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let dr = entries
        .iter()
        .find(|e| e["account_code"] == "6401")
        .expect("借主营业务成本");
    assert_eq!(
        money_num(dr["debit"].as_str().unwrap()),
        36.0,
        "结转额=销售发出 4×9=36（口径排除领料/转换）"
    );
    let cr = entries
        .iter()
        .find(|e| e["account_code"] == "140501")
        .expect("贷库存商品");
    assert_eq!(money_num(cr["credit"].as_str().unwrap()), 36.0);

    // f) 同期间重复结转 → 拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/sales-cost",
            &sid,
            serde_json::json!({ "period": cur_ymm, "method": "moving_average", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复结转应拒绝");
}

/// 建套收归管理员 + 存量账套归属迁移（版本更新时执行、幂等）：
/// 普通账号名下的套 → 最早管理员名下，并把管理员补进该套的套内管理员成员行。
#[tokio::test]
async fn only_admin_creates_books_and_owners_migrate() {
    let (state, books_dir, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;

    // 1) 普通账号不能建套；2) 管理员能建套（owner=boss）
    let u = provision_plain_user(&state, &admin_sid, "fr1", "Fr12345678").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/books",
            &u,
            serde_json::json!({ "key": "", "company": "普通账号自建", "start_period": 202601 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "仅管理员可新建账套");
    let (st, body) = create_book(&state, &admin_sid, "存量改造套").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let k1 = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(state.realm.book_owner(&k1).unwrap().unwrap(), "boss");

    // 3) 造迁移前存量：普通账号 owner 的账套（realm 直插 + 真实文件 + 注册）
    let path = books_dir.join("legacy1.fbk");
    let _ = findb::Db::create_no_admin(&path, &BookOptions::default()).expect("建存量账套文件");
    state
        .realm
        .register_book("legacy1", path.to_str().unwrap(), "fr1", "旧账套")
        .expect("直插存量账套记录");
    state.books.register(&path, 16);

    // 4) 执行迁移：归属接管 + 套内补管理员成员行
    let n = state.migrate_book_owners_to_admin();
    assert!(n >= 1, "应接管至少 1 个存量账套（实际 {n}）");
    assert_eq!(
        state.realm.book_owner("legacy1").unwrap().unwrap(),
        "boss",
        "普通账号名下的套应归管理员"
    );
    assert_eq!(
        state.realm.book_owner(&k1).unwrap().unwrap(),
        "boss",
        "管理员名下的套不动"
    );
    let db = findb::Db::open(&path).expect("打开存量账套");
    let bu = findb::users::get(&db, "boss").expect("读套内成员");
    assert!(
        bu.map(|x| x.is_admin()).unwrap_or(false),
        "管理员应补进套内管理员成员行"
    );

    // 5) 幂等：二次执行无事可做
    let n2 = state.migrate_book_owners_to_admin();
    assert_eq!(n2, 0, "二次迁移应为 0");

    // 6) 管理员可进入迁移后的套
    assert_eq!(
        select_book(&state, &admin_sid, "legacy1").await,
        StatusCode::OK,
        "管理员应能进入迁移后的套"
    );
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

// ---------------------------------------------------------------------------
// 回归：Web 与桌面能力对齐（辅助核算/数量/期末处理）
// ---------------------------------------------------------------------------

/// Web 凭证支持辅助核算/数量/单价，且编辑时不丢原行未提交的要素。
#[tokio::test]
async fn web_voucher_aux_qty_roundtrip_and_preserve() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 140301 原材料（存货辅助 + 数量核算）100 = 5 × 20；贷 2001 短期借款 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                "no": 1, "attachments": 0, "memo": "带辅助数量的凭证",
                "entries": [
                    {
                        "line": 1, "account_code": "140301", "summary": "采购原料",
                        "debit": "100", "credit": "0",
                        "aux": { "item": "RM01" }, "qty": "5", "price": "20"
                    },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "带辅助/数量的凭证应可保存");
    let body = body_string(resp).await;
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .expect("应返回凭证 id");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let e0 = &v["entries"][0];
    assert_eq!(
        e0["aux"]["item"],
        serde_json::json!("RM01"),
        "存货辅助应落库"
    );
    assert_eq!(e0["qty"], serde_json::json!("5"), "数量应落库");
    assert_eq!(e0["price"], serde_json::json!("20"), "单价应落库");

    // 模拟旧版前端：更新时不提交 aux/qty/price，原行要素必须保留
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": id, "period": 202601, "date": "2026-01-15", "word": "记",
                "no": 1, "attachments": 0, "memo": "旧前端编辑",
                "entries": [
                    { "line": 1, "account_code": "140301", "summary": "采购原料", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "旧前端编辑应兼容");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["entries"][0]["aux"]["item"],
        serde_json::json!("RM01"),
        "未提交的辅助核算不能被清空"
    );
    assert_eq!(
        v["entries"][0]["qty"],
        serde_json::json!("5"),
        "未提交的数量不能被清空"
    );
}

/// Web 期末处理：结转损益 → 记账结转凭证 → 结账 → 反结账。
#[tokio::test]
async fn web_period_carry_close_unclose_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 6401 主营业务成本 100 / 贷 1001 库存现金 100（产生损益发生额）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20", "word": "记",
                "no": 1, "attachments": 0, "memo": "结转测试",
                "entries": [
                    { "line": 1, "account_code": "6401", "summary": "成本", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "付款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "业务凭证应能记账");

    // 结账前预检：应提示需要先结转损益（无未记账凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/periods/202601/precheck", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let chk: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        chk["pl_count"].as_i64().unwrap_or(0) > 0,
        "应有损益科目可结转"
    );

    // 结转损益 → 生成一张待记账的结转凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/carry-forward",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结转损益应成功");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结转凭证应能记账");

    // 结账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/close",
            &sid,
            serde_json::json!({ "require_carry": true }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "结转并记账后应能结账：{}",
        body_string(resp).await
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/periods", &sid))
        .await
        .unwrap();
    let periods: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        periods["closed_upto"],
        serde_json::json!("2026-01"),
        "结账线应推进"
    );

    // 反结账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/unclose",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "反结账应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/periods", &sid))
        .await
        .unwrap();
    let periods: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(periods["closed_upto"].is_null(), "反结账后不应有结账线");
}

/// 可选审核环节：启用后 未记账→已审核→已记账；未审核不能记账、已审核不能改/删。
#[tokio::test]
async fn web_optional_audit_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 启用审核环节（先读回 options 再改，避免覆盖其他字段）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    opts["enable_audit"] = serde_json::json!(true);
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "启用审核环节应成功");

    // 保存一张凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "审核流",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未审核不能记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未审核不应允许记账");

    // 审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核应成功");

    // 已审核不能直接修改
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": id, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "改一下",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "改", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "改", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已审核凭证不应允许修改"
    );

    // 审核后可以记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核后应能记账");

    // 已记账不能反审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已记账不应允许反审核"
    );

    // 反记账 → 反审核 → 可修改
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/unpost"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "反审核应成功");
}

/// Web 外币分录：币种/汇率/原币金额可存取（引擎校验 原币×汇率≈本位币）。
#[tokio::test]
async fn web_foreign_currency_entry_roundtrip() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 新建一个美元科目（8888，不能与内置科目冲突）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/accounts",
            &sid,
            serde_json::json!({
                "account": {
                    "code": "8888", "name": "美元户", "category": "asset", "dir": "debit",
                    "aux": 0, "unit": null, "currency": "USD", "has_qty": false,
                    "is_cash": false, "is_bank": false, "cash_flow_item": null,
                    "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
                },
                "aux_kinds": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增美元科目应成功");

    // 借 8888 原币 100 × 7.2 = 720；贷 2001 720
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-18", "word": "记",
                "no": 1, "attachments": 0, "memo": "外币收款",
                "entries": [
                    {
                        "line": 1, "account_code": "8888", "summary": "美元收款",
                        "debit": "720", "credit": "0",
                        "currency": "USD", "rate": "7.2", "amount_for": "100"
                    },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "720" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "外币凭证应可保存");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let e0 = &v["entries"][0];
    assert_eq!(e0["currency"], serde_json::json!("USD"), "币种应落库");
    assert_eq!(e0["rate"], serde_json::json!("7.2"), "汇率应落库");
    assert_eq!(
        e0["amount_for"],
        serde_json::json!("100.00"),
        "原币金额应落库"
    );

    // 原币 × 汇率与金额不符时应拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-19", "word": "记",
                "no": 2, "attachments": 0, "memo": "错汇率",
                "entries": [
                    {
                        "line": 1, "account_code": "8888", "summary": "美元收款",
                        "debit": "700", "credit": "0",
                        "currency": "USD", "rate": "7.2", "amount_for": "100"
                    },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "700" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "原币×汇率不符应被拒"
    );
}

/// Web 固定资产：建卡 → 计提折旧生成凭证 → 幂等 → 清理。
#[tokio::test]
async fn web_assets_depreciate_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets",
            &sid,
            serde_json::json!({
                "code": "GD0001", "name": "台式电脑", "category": "电子设备", "spec": "",
                "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
                "expense_account": "660201", "original_value": "12000",
                "residual_rate": "5", "life_months": 36, "method": "straight",
                "start_period": 202601, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增资产卡片应成功");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 计提 2026-01 折旧：12000 × 95% / 36 = 316.67
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/depreciate",
            &sid,
            serde_json::json!({ "ymm": 202601 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提折旧应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["already"], serde_json::json!(false));
    assert_eq!(r["total"], serde_json::json!("316.67"));
    let vid = r["voucher_id"].as_i64().expect("应返回折旧凭证 id");

    // 折旧凭证借贷平衡
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let sum = |k: &str| {
        entries
            .iter()
            .map(|e| e[k].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0))
            .sum::<f64>()
    };
    assert!(
        (sum("debit") - sum("credit")).abs() < 0.005,
        "折旧凭证应借贷平衡"
    );

    // 幂等：再次计提不生成新凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/depreciate",
            &sid,
            serde_json::json!({ "ymm": 202601 }),
        ))
        .await
        .unwrap();
    let r2: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r2["already"], serde_json::json!(true), "重复计提应幂等");
    assert_eq!(r2["voucher_id"].as_i64(), Some(vid), "幂等应返回原凭证");

    // 清理后再计提不再包含该资产
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/{id}/dispose"),
            &sid,
            serde_json::json!({ "ymm": 202601, "amount": "1000" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资产清理应成功");
}

/// Web 银行对账：导入对账单 → 自动勾对 → 余额调节表平衡。
#[tokio::test]
async fn web_bank_reconcile_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 账面：借 100201 1000 / 贷 2001 1000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "银行收款",
                "entries": [
                    { "line": 1, "account_code": "100201", "summary": "收款", "debit": "1000", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 导入对账单（余额与账面一致，便于验证调节表勾稽）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/import",
            &sid,
            serde_json::json!({
                "ymm": 202601, "account": "100201",
                "text": "日期,摘要,结算号,借方,贷方,余额\n2026-01-10,收款,SN001,1000.00,0.00,1000.00\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入对账单应成功");
    let imp: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(imp["imported"], serde_json::json!(1));

    // 自动勾对
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/auto-match",
            &sid,
            serde_json::json!({ "ymm": 202601, "account": "100201", "tolerance": 31 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let m: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        m["matched"].as_u64().unwrap_or(0) >= 1,
        "应至少勾对一对：{m}"
    );

    // 调节表：银行与账面调节后一致
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/bank?period=202601&account=100201", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        d["statements"][0]["entry_id"].as_i64(),
        Some(d["book"][0]["entry_id"].as_i64().unwrap()),
        "对账单应挂到账面分录"
    );
    assert_eq!(
        d["reconcile"]["balanced"],
        serde_json::json!(true),
        "调节表应平衡：{}",
        d["reconcile"]
    );
}

/// Web 往来核销：自动核销等额的一借一贷。
#[tokio::test]
async fn web_settle_auto_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 112201（客户 C01）1000 / 贷 1001 1000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let v1_body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "应收凭证应保存：{v1_body}");
    let v1 = serde_json::from_str::<serde_json::Value>(&v1_body).unwrap()["id"]
        .as_i64()
        .unwrap();
    // 收款：借 1001 / 贷 112201（客户 C01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20", "word": "记",
                "no": 2, "attachments": 0, "memo": "收款",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "1000", "credit": "0" },
                    { "line": 2, "account_code": "112201", "summary": "核销", "debit": "0", "credit": "1000", "aux": { "customer": "C01" } }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    for vid in [v1, v2] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "凭证 {vid} 应能记账");
    }

    // 核销前：两笔未核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        2,
        "核销前应有两笔未核销"
    );

    // 自动核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/auto",
            &sid,
            serde_json::json!({ "account": "112201", "ymm": 202601, "tolerance": "0.01" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["pairs"], serde_json::json!(1), "应核销一对：{r}");

    // 核销后：无未核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "核销后应无未核销"
    );
}

/// Web 总账/日记账接口与 CSV 导出。
#[tokio::test]
async fn web_ledger_tabs_and_export() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-12", "word": "记",
                "no": 1, "attachments": 0, "memo": "账簿测试",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收现", "debit": "300", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "300" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 总账
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/ledger/general?code=1001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let gl: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        gl.as_array().unwrap().len(),
        1,
        "总账应按期间汇总一行：{gl}"
    );
    assert_eq!(gl[0]["debit"], serde_json::json!("300.00"));

    // 日记账（对方科目）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/ledger/journal?code=1001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let jr: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(jr.as_array().unwrap().len(), 1, "日记账应一行");
    assert!(
        jr[0]["opposite_accounts"]
            .as_str()
            .unwrap_or("")
            .contains("短期借款"),
        "日记账应带对方科目：{jr}"
    );

    // 导出凭证 CSV
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/vouchers?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("text/csv"),
        "应返回 CSV"
    );
    let body = body_string(resp).await;
    assert!(
        body.contains("凭证号") && body.contains("库存现金"),
        "CSV 应含表头与科目名：{body}"
    );

    // 导出明细账 CSV（含数量列与否均可）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/export/ledger?code=1001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("日期") && body.contains("余额"),
        "明细账 CSV 应含表头：{body}"
    );
}

/// Web 凭证附件：multipart 上传 → 列表 → 下载 → 删除。
#[tokio::test]
async fn web_voucher_attachment_roundtrip() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-08", "word": "记",
                "no": 1, "attachments": 0, "memo": "附件测试",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "50", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "50" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // multipart 上传
    let boundary = "----finbooktest";
    let payload = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"receipt.txt\"\r\nContent-Type: text/plain\r\n\r\nhello attachment\r\n--{b}--\r\n",
        b = boundary
    );
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/vouchers/{id}/attachments"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::COOKIE, &sid)
        .body(Body::from(payload))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "上传附件应成功");
    let aid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 列表
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}/attachments"), &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "应有 1 个附件：{list}");
    assert_eq!(list[0]["name"], serde_json::json!("receipt.txt"));

    // 下载
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/attachments/{aid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "hello attachment");

    // 删除
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/attachments/{aid}"))
        .header(header::COOKIE, &sid)
        .body(Body::empty())
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除附件应成功");
}

/// Web 辅助账 / 数量金额账 / 自定义报表。
#[tokio::test]
async fn web_aux_qty_and_custom_reports() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收：借 112201 客户 C01 1000 / 贷 600101 1000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let v1 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 存货：借 140301 存货 RM01 5×20=100 / 贷 1001 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-07", "word": "记",
                "no": 2, "attachments": 0, "memo": "入库",
                "entries": [
                    { "line": 1, "account_code": "140301", "summary": "入库", "debit": "100", "credit": "0", "aux": { "item": "RM01" }, "qty": "5", "price": "20" },
                    { "line": 2, "account_code": "1001", "summary": "付款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    let v2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    for vid in [v1, v2] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // 辅助账（客户）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/aux-balance?kind=customer&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        d["rows"][0]["key"],
        serde_json::json!("C01"),
        "辅助账应有客户 C01：{d}"
    );
    assert_eq!(d["rows"][0]["debit"], serde_json::json!("1000.00"));

    // 数量金额账
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/qty-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let q: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = q["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account_code"] == serde_json::json!("140301"))
        .expect("数量金额账应含 140301");
    assert_eq!(
        row["qty_in"],
        serde_json::json!("5"),
        "入库数量应为 5：{row}"
    );
    assert_eq!(row["qty_end"], serde_json::json!("5"), "期末数量应为 5");

    // 自定义报表：QM("1001") 期末余额
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/custom-reports",
            &sid,
            serde_json::json!({
                "key": "", "name": "资金小表",
                "columns": ["期末"],
                "lines": [{ "name": "库存现金", "indent": 0, "formulas": ["QM(\"1001\")"], "bold": false }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存自定义报表应成功");
    let saved: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        saved["errors"].as_array().unwrap().is_empty(),
        "公式应无语法错误：{saved}"
    );
    let key = saved["key"].as_str().unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/custom-reports/{key}?period=202601"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(got["report"]["name"], serde_json::json!("资金小表"));
    let cell = got["values"][0][0].as_str().unwrap_or("");
    assert!(
        cell.contains("100.00"),
        "QM(\"1001\") 应算出 -100.00：{got}"
    );
}

// ---------------------------------------------------------------------------
// 凭证列表分页（GET /api/vouchers/page）
// ---------------------------------------------------------------------------
//
// 这些测试针对的是一个具体缺陷：老端点 `GET /api/vouchers` 默认 limit=200、
// 排序升序，于是**一个期间超过 200 张凭证后，后面的凭证在界面上完全看不到，
// 而且没有任何提示**。对月均两三百张的单子，这不是边角情况。

/// 灌 n 张草稿凭证（借 1001 / 贷 2001，金额 100），期间 202601。
async fn seed_draft_vouchers(state: &Arc<WebState>, sid: &str, n: i32) {
    for i in 1..=n {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                    "no": i, "attachments": 0, "memo": format!("批量 {i}"),
                    "entries": [
                        { "line": 1, "account_code": "1001", "summary": "批量", "debit": "100", "credit": "0" },
                        { "line": 2, "account_code": "2001", "summary": "批量", "debit": "0", "credit": "100" }
                    ]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "第 {i} 张凭证应保存成功");
    }
}

async fn get_voucher_page(state: &Arc<WebState>, sid: &str, qs: &str) -> serde_json::Value {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/page?{qs}"), sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "分页端点应 200：{qs}");
    serde_json::from_str(&body_string(resp).await).unwrap()
}

/// 回归：第 201 张及以后的凭证必须能被界面访问到。
///
/// 老端点 limit 默认 200，UI 用的是老端点，所以第 201 张开始就是死数据。
/// 这里用 205 张（略微超过 200）而不是刚好 200：边界值测不出来，
/// 差一张才说明分页真的接上了。
#[tokio::test]
async fn voucher_page_reaches_past_the_old_200_cap() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 205).await;

    // 最后一页（205 张，page_size=50 → 第 5 页）必须有内容
    let p5 = get_voucher_page(&state, &sid, "period=202601&page=5&page_size=50").await;
    assert_eq!(p5["total"], 205, "total 应是全量张数，不是本页数");
    let items = p5["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        5,
        "第 5 页应有 205-200=5 张，实际 {}",
        items.len()
    );

    // 老端点确实只给 200（证明这个测试对应的是真实缺陷，而不是我臆想的）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/vouchers?period=202601", &sid))
        .await
        .unwrap();
    let old = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap();
    assert_eq!(
        old.as_array().unwrap().len(),
        200,
        "老端点仍按 limit=200 截断"
    );
}

/// 翻完每一页，去重后的凭证号集合 == 全部凭证号，且不重不漏。
#[tokio::test]
async fn voucher_page_walks_every_row_exactly_once() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 23).await;

    let mut seen: Vec<i64> = Vec::new();
    for page in 1..=5 {
        let p = get_voucher_page(
            &state,
            &sid,
            &format!("period=202601&page={page}&page_size=5"),
        )
        .await;
        assert_eq!(p["total"], 23);
        for it in p["items"].as_array().unwrap() {
            seen.push(it["id"].as_i64().unwrap());
        }
    }
    let unique: std::collections::HashSet<i64> = seen.iter().copied().collect();
    assert_eq!(seen.len(), 23, "应恰好取到 23 张，实际 {}", seen.len());
    assert_eq!(
        unique.len(),
        23,
        "不应有重复：重复 {}/{}",
        seen.len(),
        unique.len()
    );
    // 越界页必须是空数组，而不是报错或回绕到第一页
    let p = get_voucher_page(&state, &sid, "period=202601&page=99&page_size=5").await;
    assert_eq!(p["items"].as_array().unwrap().len(), 0, "越界页应为空数组");
}

/// 合计必须是**全量**的，不能是本页的。
///
/// 会计核对时看的是「本期间借方合计 = 贷方合计」这个总账数字。
/// 按页汇总会让人以为账不平（翻到第 3 页合计突然变了）。
#[tokio::test]
async fn voucher_page_totals_cover_whole_set_not_the_page() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 9).await; // 每张借 100 / 贷 100

    let p1 = get_voucher_page(&state, &sid, "period=202601&page=1&page_size=4").await;
    assert_eq!(p1["items"].as_array().unwrap().len(), 4);
    assert_eq!(p1["total"], 9);
    assert_eq!(
        money_num(p1["debit_total"].as_str().unwrap()),
        900.0,
        "第 1 页也要显示全量合计 900"
    );
    assert_eq!(money_num(p1["credit_total"].as_str().unwrap()), 900.0);

    let p3 = get_voucher_page(&state, &sid, "period=202601&page=3&page_size=4").await;
    assert_eq!(p3["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        money_num(p3["debit_total"].as_str().unwrap()),
        900.0,
        "合计不应随页码变化"
    );

    // 过滤条件收窄时合计跟着变：证明合计与列表同源，不是写死的
    let nohit = get_voucher_page(&state, &sid, "period=202601&q=根本不存在的摘要").await;
    assert_eq!(nohit["total"], 0);
    assert_eq!(money_num(nohit["debit_total"].as_str().unwrap()), 0.0);
}

/// 作废凭证不进合计（与账簿口径一致），但仍可在列表里查到。
#[tokio::test]
async fn voucher_page_totals_exclude_void() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 3).await;
    // 作废其中一张
    let p = get_voucher_page(&state, &sid, "period=202601&page_size=10").await;
    let id = p["items"].as_array().unwrap()[0]["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/void"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let after = get_voucher_page(&state, &sid, "period=202601&page_size=10").await;
    assert_eq!(after["total"], 3, "作废的仍在列表里（能查到）");
    assert_eq!(
        money_num(after["debit_total"].as_str().unwrap()),
        200.0,
        "作废的 100 不应计入合计"
    );
}

/// 参数非法必须 400，不能静默当第 1 页。
///
/// 静默容错在浏览器端是优点，在 API 端是 bug：`page=abc` 返回首页的话，
/// 调用方会拿到「看起来正常但其实是首页」的数据，漏拉极难发现。
#[tokio::test]
async fn voucher_page_rejects_bad_params() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 2).await;

    for qs in [
        "period=202601&page=abc",
        "period=202601&page_size=xyz",
        "period=202601&page=0x",
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/vouchers/page?{qs}"), &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{qs} 应 400");
    }
    // page_size 超上限要被夹住而不是报错（缺省≠非法）
    let big = get_voucher_page(&state, &sid, "period=202601&page_size=99999").await;
    assert_eq!(big["page_size"], 500, "page_size 应夹到上限 500");
    // 缺省用默认值
    let def = get_voucher_page(&state, &sid, "period=202601").await;
    assert_eq!(def["page"], 1);
    assert_eq!(def["page_size"], 50);
}

/// 分页的 total 必须与数据范围一致：受限用户看到的张数更少。
#[tokio::test]
async fn voucher_page_total_respects_data_scope() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    // 两批各 6 张：一批借贷两侧都在 1xxx（落在范围 1001..1999 内），
    // 一批在 6xxx（越界）。
    // 两侧都必须在范围内才有意义 —— `can_see_voucher` 是「全部科目都在范围内
    // 才可见」，只把借方放进范围、贷方留在 2001 的话**连范围内那批也会被
    // 全部滤掉**（我第一版就是这么写的，测出来 total=0）。
    for (no_base, debit, credit) in [(1, "1001", "1901"), (101, "660201", "600101")] {
        for k in 0..6i32 {
            let resp = handlers::router(state.clone())
                .oneshot(authed_post(
                    "/api/vouchers",
                    &sid,
                    serde_json::json!({
                        "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                        "no": no_base + k, "attachments": 0,
                        "memo": format!("范围 {debit}"),
                        "entries": [
                            { "line": 1, "account_code": debit, "summary": debit, "debit": "100", "credit": "0" },
                            { "line": 2, "account_code": credit, "summary": credit, "debit": "0", "credit": "100" }
                        ]
                    }),
                ))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "{debit}/{credit} 第 {k} 张应保存成功"
            );
        }
    }

    // 管理员不限范围：12 张
    let all = get_voucher_page(&state, &sid, "period=202601&page_size=100").await;
    assert_eq!(all["total"], 12, "管理员应看到全部 12 张");

    // 建一个只允许看 1xxx 的普通账号。两层账号：先在平台层开通，再由 boss
    // 拉进 b1（直接 POST /api/users 会 400「该账号尚未开通」），最后收紧数据范围。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({
                "username": "scoped", "display_name": "受限", "password": "Scoped@123456"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "开通平台账号：{}",
        body_string(resp).await
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "scoped", "display_name": "受限", "password": "Scoped@123456",
                "role": "accountant", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "拉进 b1：{}",
        body_string(resp).await
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/scoped",
            &sid,
            serde_json::json!({ "data_scope": { "account_from": "1001", "account_to": "1999" } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "收紧数据范围应成功");

    // 平台层开通的账号默认 must_change_pwd，不改密就进不了账套（401）。
    let (st, sid0) = login(&state, "scoped", "Scoped@123456").await;
    assert_eq!(st, StatusCode::OK, "scoped 首登");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid0,
            serde_json::json!({ "old": "Scoped@123456", "new": "Scoped!23456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "首登改密");
    let (st, sid2) = login(&state, "scoped", "Scoped!23456").await;
    assert_eq!(st, StatusCode::OK, "scoped 改密后重登");
    assert_eq!(select_book(&state, &sid2, "b1").await, StatusCode::OK);
    let scoped = get_voucher_page(&state, &sid2, "period=202601&page_size=100").await;
    assert_eq!(
        scoped["total"], 6,
        "数据范围限定 1001..1999 时应只看到 6 张（借贷都在 1xxx 的那批）"
    );
    for it in scoped["items"].as_array().unwrap() {
        // item.summary 取的是**第一条分录**的摘要（Voucher::first_summary），
        // 所以这里用它来分辨批次：范围内 = 1001，越界那批 = 660201。
        assert_eq!(it["summary"], "1001", "越界的那批不该出现");
    }
    // 合计也跟着收窄（这是 total 必须与数据范围同源的直接证据）
    assert_eq!(money_num(scoped["debit_total"].as_str().unwrap()), 600.0);
}

// ---------------------------------------------------------------------------
// 报表勾稽 / 凭证流转 / 导出 / 发票 / 银行手工勾对 / 核销手工 / 打印端点
// ---------------------------------------------------------------------------

/// 断言「应当成功」，失败时把**响应体**一起打出来。
///
/// 本项目里绝大多数写接口的失败都是 400/403/409，而**拒绝原因只写在 body 里**：
/// 「该账套启用了审核环节，请先审核凭证再记账」/「涉及现金或银行科目，需出纳签字」/
/// 「试算不平衡」/「该期间已结账」。裸的 `assert_eq!(status, OK, "记账应成功")`
/// 只留下 `left: 400 right: 200`，读失败的人无法判断是哪道闸门，只能自己再改一遍
/// 加打印 —— 那是把成本推给下一个人。
async fn assert_ok(resp: axum::response::Response, what: &str) {
    let st = resp.status();
    let body = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "{what}：实际 {st}，响应体 {body}");
}

/// 建一张已记账凭证（返回 id）
async fn post_voucher(
    state: &Arc<WebState>,
    sid: &str,
    no: i32,
    entries: serde_json::Value,
) -> i64 {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                "no": no, "attachments": 0, "memo": "t",
                "entries": entries
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存凭证应成功");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    // 断言必须带上响应体：记账被拒全是 400，而 400 的**原因写在 body 里**
    // （「该账套启用了审核环节」/「涉及现金银行科目需出纳签字」/「试算不平衡」…）。
    // 只写「记账应成功」的话，一次失败只留下一句 left: 400 right: 200，
    // 排查得再跑一遍加打印 —— 那是把成本推给下一个读失败的人。
    assert_ok(resp, "记账应成功").await;
    id
}

/// H-3 定案：试算平衡默认只含已记账——草稿凭证不入账，记账后进入。
#[tokio::test]
async fn trial_balance_default_posted_only_h3() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 只保存、不记账（草稿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "草稿口径",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "草稿", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "草稿", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿凭证应可保存");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 草稿状态：本期发生额不含它
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let d = money_num(tb["totals"]["debit"].as_str().unwrap());
    assert!(
        (d - 0.0).abs() < 0.005,
        "H-3：草稿不应进试算平衡，本期借方 {d}：{tb}"
    );

    // 记账后进入
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(resp, "记账应成功").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let d = money_num(tb["totals"]["debit"].as_str().unwrap());
    assert!(
        (d - 100.0).abs() < 0.005,
        "H-3：记账后应进试算平衡，本期借方 {d}：{tb}"
    );
}

/// 工资三类凭证回链：计提 / 社保缴纳 / 发放状态可见，且同类型重复生成被拦。
#[tokio::test]
async fn payroll_voucher_status_tracking() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 录入工资行（应发5000，个人社保200+公积金200，单位各500）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202601",
            &sid,
            serde_json::json!({
                "employee": "E01", "dept": "D01", "gross": "5000",
                "social": "200", "housing": "200", "deduction": "0",
                "additional": "0", "social_co": "500", "housing_co": "500", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "录入工资行应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["id"].as_i64().unwrap() > 0);

    let get_rows = |sid: String| {
        let state = state.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_get("/api/payroll?period=202601", &sid))
                .await
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        }
    };

    // 计提凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/accrue?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "expense": "660201", "wage_payable": "221101",
                "social_payable": "221103", "housing_payable": "221104"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提凭证应成功");

    // 社保缴纳凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/social-pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "social_payable": "221103", "housing_payable": "221104",
                "personal_payable": "2241", "bank_account": "100201"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "社保缴纳凭证应成功");

    // 发放凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "payable_account": "221101", "bank_account": "100201",
                "tax_account": "222107", "social_account": "2241"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发放凭证应成功");

    // 三个回链都应写回工资行（响应为数组）
    let rows = get_rows(sid.clone()).await;
    let arr = rows.as_array().expect("工资列表应为数组");
    let row = &arr[0];
    assert!(!row["voucher_id"].is_null(), "计提凭证应回链：{rows}");
    assert!(
        !row["social_voucher_id"].is_null(),
        "社保凭证应回链：{rows}"
    );
    assert!(!row["paid_voucher_id"].is_null(), "发放凭证应回链：{rows}");

    // 同类型重复生成被拦（同一期幂等/唯一约束）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "payable_account": "221101", "bank_account": "100201",
                "tax_account": "222107", "social_account": "2241"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "重复生成发放凭证应被拦"
    );
}

/// 收付款单 API 流程：出凭证 + 自动核销 + 删除链（凭证先行）。
#[tokio::test]
async fn receipt_doc_api_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 先造一张应收挂账（客户辅助 C01）并记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 81, "attachments": 0, "memo": "挂账",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "挂账",
                      "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入",
                      "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "挂账凭证应可保存");
    let ar_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{ar_id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "挂账应记账");

    // 收款 600：草稿建单（不出凭证、不核销——审核流对标金蝶）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/receipts",
            &sid,
            serde_json::json!({
                "date": "2026-01-10", "kind": "receipt", "fund_account": "100201",
                "party": "C01", "amount": "600", "memo": "回款"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增收款单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc_id = r["id"].as_i64().unwrap();
    assert!(r["voucher_id"].is_null(), "草稿阶段不应生成凭证");
    // 审核 → 同事务生成凭证 + FIFO 自动核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["voucher_id"].as_i64().expect("审核应生成凭证");
    assert_eq!(r["settled"].as_i64().unwrap(), 1, "应自动核销 1 笔");
    // 重复审核拒绝（条件更新防并发）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复审核应拒绝");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "100201");
    assert_eq!(v["entries"][1]["account_code"], "112201");
    assert_eq!(v["entries"][1]["aux"]["customer"], "C01");

    // 列表可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/receipts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1);

    // 删除链：凭证存在 → 拒；删凭证 → 单据可删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "凭证存在时不能删单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿凭证应可删除");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "凭证删除后单据可删");

    // 撤审回路：再建一单 → 审核 → 撤审 → 凭证消失、单据回草稿、可直接删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/receipts",
            &sid,
            serde_json::json!({
                "date": "2026-01-11", "kind": "receipt", "fund_account": "100201",
                "party": "C02", "amount": "100", "memo": "二笔"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc2 = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc2}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let vid2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["voucher_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc2}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "撤审应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid2}"), &sid))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "撤审后凭证应已删除");
    // 单据回草稿（凭证号清空）→ 可直接删
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/receipts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == doc2)
        .unwrap();
    assert_eq!(row["status"], "draft", "撤审应回草稿");
    assert!(row["voucher_id"].is_null());
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc2}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿可直接删除");
}

/// 资金调拨：建单（草稿）→ 审核出凭证（借 转入 / 贷 转出，银行腿挂 B01）→
/// 撤审回草稿 → 删除链；守卫同户 / 非资金科目。
#[tokio::test]
async fn fund_transfer_api_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 守卫：转出 = 转入 拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/transfers",
            &sid,
            serde_json::json!({
                "date": "2026-01-05", "from_account": "100201", "to_account": "100201",
                "amount": "500", "memo": "同户"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "同户调拨应拒绝");

    // 守卫：转入非资金科目（6602）拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/transfers",
            &sid,
            serde_json::json!({
                "date": "2026-01-05", "from_account": "100201", "to_account": "6602",
                "amount": "500", "memo": "非资金科目"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非资金科目应拒绝");

    // 建单：提现备用（银行 → 现金），此时只落草稿、不出凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/transfers",
            &sid,
            serde_json::json!({
                "date": "2026-01-05", "from_account": "100201", "to_account": "1001",
                "amount": "500", "memo": "提现备用"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增调拨单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc_id = r["id"].as_i64().unwrap();
    assert_eq!(r["status"], "draft");
    assert!(r["voucher_id"].is_null(), "草稿阶段不应生成凭证");

    // 审核 → 生成凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核应成功");
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["voucher_id"]
        .as_i64()
        .expect("审核应生成凭证");

    // 重复审核拒绝（条件更新防并发重复出凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复审核应拒绝");

    // 凭证：借 转入 1001 / 贷 转出 100201（银行腿必须挂 bank=B01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "1001");
    assert_eq!(v["entries"][0]["debit"], serde_json::json!("500.00"));
    assert_eq!(v["entries"][1]["account_code"], "100201");
    assert_eq!(v["entries"][1]["credit"], serde_json::json!("500.00"));
    assert_eq!(v["entries"][1]["aux"]["bank"], "B01");

    // 列表可见且已审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/transfers", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "audited");

    // 删除链：凭证存在 → 拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "凭证存在时不能删单");

    // 撤审 → 凭证删除、单据回草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "撤审应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "撤审后凭证应已删除");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/transfers", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"][0]["status"], "draft", "撤审应回草稿");
    assert!(r["rows"][0]["voucher_id"].is_null());

    // 草稿可直接删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿可直接删除");
}

/// 报价单转订单 + 单据套打（字段白名单 / 批量紧凑分页 / A4）。
#[tokio::test]
async fn quote_to_order_and_doc_print() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 报价单 → 审批 → 转订单（草稿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05",
                "customer_code": "C01", "customer_name": "客户甲",
                "item_code": "140501", "item_name": "成品",
                "qty": "10", "unit_price": "20",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建报价单应成功");
    let qid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "报价审批应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/to-order"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "转订单应成功");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["so_id"]
        .as_i64()
        .unwrap();

    // 订单存在：金额200、草稿、带来源备注
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == so_id)
        .expect("转换生成的订单应存在");
    assert_eq!(row["status"], "Draft");
    assert_eq!(money_num(row["total_amount"].as_str().unwrap()), 200.0);
    assert!(
        row["memo"].as_str().unwrap().contains("由报价单"),
        "订单应带来源备注"
    );
    // 报价单状态 → converted；重复转换被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/quote?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let qrow = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == qid)
        .unwrap();
    assert_eq!(qrow["status"], "converted", "报价单应标记已转订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/to-order"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已转换不可再转");

    // 第二张订单（批量紧凑分页：两张小单应同页）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-06", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "5", "unit_price": "10", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    let so2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 订单套打：两张同页 + 默认全字段
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/sales/so/print-form?ids={so_id},{so2}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()[axum::http::header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .contains("text/html"));
    let html = body_string(resp).await;
    assert!(
        html.contains("销售订单") && html.contains("客户甲") && html.contains("税率"),
        "默认应全字段显示"
    );
    assert_eq!(
        html.matches("class=\"page\"").count(),
        1,
        "两张小单应紧凑同页"
    );

    // 字段收窄：fields 白名单生效
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/sales/so/print-form?ids={so_id}&fields=no,date,code,qty,amount"),
            &sid,
        ))
        .await
        .unwrap();
    let html = body_string(resp).await;
    assert!(
        !html.contains("税率") && html.contains("单号"),
        "fields 白名单应生效：{html}"
    );

    // ids 缺省 = 期间全部；空期间 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so/print-form?period=202601", &sid))
        .await
        .unwrap();
    let html = body_string(resp).await;
    assert!(
        html.matches("class=\"doc\"").count() >= 2,
        "缺省应打印期间内全部订单"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so/print-form?period=202501", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "无可打印订单应400");

    // 采购订单套打 smoke
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-07", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/procure/po/print-form?ids={pid}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;
    assert!(
        html.contains("采购订单") && html.contains("供应商"),
        "采购套打应含标题与供应商"
    );

    // 收付款单套打 smoke（收款 + 金额大写）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/receipts",
            &sid,
            serde_json::json!({
                "date": "2026-01-10", "kind": "receipt", "fund_account": "100201",
                "party": "C01", "amount": "1234.56", "memo": "回款"
            }),
        ))
        .await
        .unwrap();
    let rid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/funds/receipts/print-form?ids={rid}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;
    assert!(
        html.contains("收款单") && html.contains("壹仟贰佰叁拾肆元伍角陆分"),
        "收付款套打应含单名与金额大写"
    );
}

/// 暂估必须封顶在实收金额内 —— 端到端走 HTTP。
///
/// 回归背景：`po_estimate` 只有普通索引（非 UNIQUE），`est_amount` 全手填，
/// `po_estimate_add` 此前零校验。同一 PO 同一物料可反复登记暂估，每登一次
/// 就出一张「借 存货 / 贷 应付」凭证，应付与存货双双虚增，全程无报错。
/// 销售侧 `prod_from_so` 早有封顶，采购侧此前完全裸奔。
#[tokio::test]
async fn estimate_is_capped_by_receipt_over_http() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Confirmed", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "100", "unit_price": "10", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单应成功");
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // ① 没收货就登暂估：额度 0，必须被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "500" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "没收货就暂估应被拒");
    let msg = body_string(resp).await;
    assert!(msg.contains("超出可暂估额度"), "错误信息要说清原因：{msg}");

    // ② 收 100（税率 0，含税 1000），登满 1000 可以
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "date": "2026-01-08", "qty": "100" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购收货应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "1000" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "额度内暂估应成功");
    let est_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // ③ 再登一次（哪怕金额只有 1 元）必须被拒 —— 额度已用满，
    //    这正是原先能把应付凭空做虚的那条路
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "额度用满后重复登记暂估必须被拒（重复入账 = 应付虚增）"
    );

    // ④ 冲回后额度释放：发票到票、暂估冲回之后应能重新暂估全额
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/estimate/{est_id}/settle"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "冲回应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "1000" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "冲回后额度应释放，能重新暂估全额：{}",
        body_string(resp).await
    );
}

/// 采购暂估自动出凭证：登记（借存货/贷应付-供应商）+ 冲回反向 + 幂等。
#[tokio::test]
async fn estimate_auto_voucher() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建采购订单（S01，140301 300×9）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "300", "unit_price": "9", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pid = r["id"].as_i64().unwrap();

    // 先收货再暂估：暂估额度来自实收量（封顶在「价税合计 × 已收/订购」），
    // 货没到就登暂估会被守卫拒掉 —— 那正是守卫要挡的「凭空虚增应付」。
    // 收 300 → 额度 2700×1.13 = 3051，够登 800。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "date": "2026-01-08", "qty": "300" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购收货应成功");

    // 登记暂估 800 → 借140301 / 贷220201(S01)
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "800" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "登记暂估应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let est_id = r["id"].as_i64().unwrap();
    let vid = r["voucher_id"].as_i64().expect("登记暂估应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "140301");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 800.0);
    assert_eq!(v["entries"][1]["account_code"], "220201");
    assert_eq!(v["entries"][1]["aux"]["supplier"], "S01");

    // 冲回 → 反向凭证；幂等
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/estimate/{est_id}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-20" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "冲回应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rvid = r["voucher_id"].as_i64().expect("冲回应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{rvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "220201");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 800.0);
    assert_eq!(v["entries"][1]["account_code"], "140301");
    assert_eq!(
        money_num(v["entries"][1]["credit"].as_str().unwrap()),
        800.0
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/estimate/{est_id}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-21" }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "重复冲回应幂等：{r}");
}

/// 发货 → 收入确认凭证（比例法）+ 退货冲回 + 订单状态联动。
#[tokio::test]
async fn sales_ship_income_voucher() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 订单：1 行 10×10@13% → 不含税 100 / 税 13 / 价税 113
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "10", "unit_price": "10", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let id = r["id"].as_i64().unwrap();

    // 草稿订单不能发货（对标金蝶：执行前须确认）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": id, "period": 202601, "date": "2026-01-15", "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "草稿订单发货应被拒");

    // 确认订单
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认订单应成功");

    // 全量发货 → 收入凭证（借应收113 / 贷收入100 / 贷销项13）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": id, "period": 202601, "date": "2026-01-15", "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["voucher_id"].as_i64().expect("发货应确认收入凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "112201");
    assert_eq!(v["entries"][0]["aux"]["customer"], "C01");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 113.0);
    assert_eq!(v["entries"][1]["account_code"], "600101");
    assert_eq!(
        money_num(v["entries"][1]["credit"].as_str().unwrap()),
        100.0
    );
    assert_eq!(v["entries"][2]["account_code"], "22210102");
    assert_eq!(money_num(v["entries"][2]["credit"].as_str().unwrap()), 13.0);

    // 订单状态 → 已完成（全量发货）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap();
    assert_eq!(row["status"], "Completed", "全量发货后订单应为已完成");

    // 超发不再确认：再发 5 → 无新凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": id, "period": 202601, "date": "2026-01-16", "qty": "5", "memo": "超发" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "超发不应重复确认收入：{r}");
    // 超发时**库存也不能多出**：账实必须一致
    {
        let db = state.db_for("b1").unwrap();
        let shipped: f64 = db
            .conn()
            .query_row(
                "SELECT COALESCE(SUM(ABS(qty)),0) FROM stock_move WHERE item='140301' AND qty<0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let ordered: f64 = row["lines"][0]["qty_ordered"]
            .as_str()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        assert!(
            shipped <= ordered + 1e-9,
            "累计出库 {shipped} 超过订购量 {ordered} —— 账实不符（超发只挡了收入没挡库存）"
        );
    }

    // 退货 4 → 冲回凭证（收入40 / 销项5.2 / 应收45.2）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/return",
            &sid,
            serde_json::json!({ "so_id": id, "period": 202601, "date": "2026-01-17", "qty": "4", "memo": "部分退货" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "退货应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rvid = r["voucher_id"].as_i64().expect("退货应生成冲回凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{rvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "600101");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 40.0);
    assert_eq!(v["entries"][1]["account_code"], "22210102");
    assert_eq!(money_num(v["entries"][1]["debit"].as_str().unwrap()), 5.2);
    assert_eq!(v["entries"][2]["account_code"], "112201");
    assert_eq!(money_num(v["entries"][2]["credit"].as_str().unwrap()), 45.2);
}

/// 对标金蝶流程：订单 CRUD + 状态流转 + 行金额服务端计算 + 客户信用卡控。
#[tokio::test]
async fn order_crud_and_credit_guard() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建销售订单：2 行（140301 50×12@13% → 不含税 600 / 税 78）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "customer_name": "客户甲", "status": "Draft", "memo": "",
                "lines": [
                    { "item_code": "140301", "qty_ordered": "50", "unit_price": "12", "tax_rate": "0.13" },
                    { "item_code": "140501", "qty_ordered": "2", "unit_price": "100", "tax_rate": "0.13" }
                ]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let body = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建销售订单应成功：{body}");
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = r["id"].as_i64().unwrap();
    assert_eq!(
        money_num(r["total_amount"].as_str().unwrap()),
        800.0,
        "行金额服务端计算：50×12+2×100"
    );
    assert_eq!(
        money_num(r["total_tax"].as_str().unwrap()),
        104.0,
        "税额=金额×税率"
    );

    // 列表可回读（含明细）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .expect("列表应含新订单");
    assert_eq!(row["lines"].as_array().unwrap().len(), 2);

    // 草稿 → 已确认
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap();
    assert_eq!(row["status"], "Confirmed");

    // 客户信用额度：超限确认被拒，额度内可确认
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "customer", "code": "C99", "name": "信用客户",
                "parent_code": null, "disabled": false,
                "props": { "credit_limit": "100" }, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建客户档案应成功");

    // 超限订单：草稿可存，确认被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-06", "customer_code": "C99",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "100", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿不受信用限制");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let over_id = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{over_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超信用额度应被拒");
    let s = body_string(resp).await;
    assert!(s.contains("信用额度不足"), "应提示信用额度不足：{s}");

    // 额度内订单：50×2=100 ≤100 → 确认成功
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-07", "customer_code": "C99",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "50", "unit_price": "2", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ok_id = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{ok_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "额度内订单应可确认");
    // 两笔占用合计 500+100 > 100 —— 再来一笔新的应被拒（占用按全部有效订单累计）
    // 注：超限单仍是草稿不计占用，占用=已确认的100；此处再确认原超限单 → 100+500=600>100 拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{over_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "累计占用超限仍应被拒"
    );

    // 删除（草稿/已确认均可删，so_delete 无状态限制）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{ok_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除订单应成功");

    // 采购订单镜像：建单 + 确认
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-08", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "30", "unit_price": "9", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pid = r["id"].as_i64().unwrap();
    assert_eq!(money_num(r["total_amount"].as_str().unwrap()), 270.0);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{pid}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购订单确认应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{pid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 资金预算视图：当期现金/银行科目预算 vs 已记账实际（形态校验；口径见 findb 单测）
#[tokio::test]
async fn funds_budget_view_api() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/budget?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资金预算视图应 200");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["rows"].is_array(), "应返回 rows 数组：{r}");
}

/// 员工借支闭环：建单 → 支付出凭证 → 核销冲账出凭证；幂等与守卫。
#[tokio::test]
async fn advance_pay_settle_api_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建单（缺省编号自动生成）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/advances",
            &sid,
            serde_json::json!({
                "date": "2026-01-08", "employee": "张三", "purpose": "出差预借",
                "amount": "2000", "pay_account": "1001", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建单应成功");
    let aid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未支付不能核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/settle"),
            &sid,
            serde_json::json!({ "expense_account": "660201", "expense_amount": "500" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未支付不能核销");

    // 支付（默认今天）→ 凭证：借122105(员工) / 贷1001
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/pay"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "支付应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pvid = r["voucher_id"].as_i64().expect("支付应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{pvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "122105");
    assert!(v["entries"][0]["aux"]["employee"]
        .as_str()
        .unwrap()
        .contains("张三"));
    assert_eq!(v["entries"][1]["account_code"], "1001");
    // 重复支付幂等
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/pay"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "重复支付不应重复出凭证：{r}");

    // 核销（冲账1500，退回500）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/settle"),
            &sid,
            serde_json::json!({ "expense_account": "660201", "expense_amount": "1500", "date": "2026-01-20" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "核销应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let svid = r["voucher_id"].as_i64().expect("核销应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{svid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["entries"].as_array().unwrap().len(),
        3,
        "冲账+退回+贷员工，{v}"
    );
    assert_eq!(v["entries"][0]["account_code"], "660201");
    // 已核销不可删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已出凭证不可删除");
    // 员工/金额校验
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/advances",
            &sid,
            serde_json::json!({ "date": "2026-01-08", "employee": "", "amount": "100" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "借支人必填");
}

/// 出纳日清标记 + 支票登记簿：状态流转、查询与删除。
#[tokio::test]
async fn cashier_day_clear_and_checks() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 日清：标记 → 按期间查询 → 取消
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/day-clear",
            &sid,
            serde_json::json!({ "account_code": "1001", "date": "2026-01-10", "clear": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "标记日清应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/funds/day-clear?account=1001&from=2026-01-01&to=2026-01-31",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dates = r["dates"].as_array().unwrap();
    assert!(
        dates.iter().any(|d| d == "2026-01-10"),
        "应查到日清日期：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/day-clear",
            &sid,
            serde_json::json!({ "account_code": "1001", "date": "2026-01-10", "clear": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/funds/day-clear?account=1001&from=2026-01-01&to=2026-01-31",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["dates"].as_array().unwrap().is_empty(),
        "取消后应查不到：{r}"
    );

    // 支票：新增 → 列表 → 作废 → 恢复 → 删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/checks",
            &sid,
            serde_json::json!({
                "no": "ZP100", "kind": "transfer", "bank_account": "100201",
                "payee": "供应商乙", "amount": "1200", "issued_date": "2026-01-16", "memo": "货款"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "登记支票应成功");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/checks", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(money_num(rows[0]["amount"].as_str().unwrap()), 1200.0);
    assert_eq!(rows[0]["status"], "issued");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/checks/{cid}/status"),
            &sid,
            serde_json::json!({ "status": "void" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "作废应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/checks", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"][0]["status"], "void");
    // 非法状态被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/checks/{cid}/status"),
            &sid,
            serde_json::json!({ "status": "bad" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/checks/{cid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/checks", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["rows"].as_array().unwrap().is_empty());
}

/// 现金盘点：账面按资金日报（按日）口径快照 → 差异 → 盘盈盘亏凭证；幂等与守卫。
#[tokio::test]
async fn cash_count_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 现金 800 已记账（01-10）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 71, "attachments": 0, "memo": "盘点基数",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "800", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "800" }
                ]
            }),
        ))
        .await
        .unwrap();
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 盘点 01-12 实盘 850 → 账面 800、差异 +50
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/cash-counts",
            &sid,
            serde_json::json!({
                "date": "2026-01-12", "account_code": "1001", "counted": "850", "memo": "例行"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增盘点应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        money_num(r["book_amount"].as_str().unwrap()),
        800.0,
        "账面快照"
    );
    assert_eq!(money_num(r["diff"].as_str().unwrap()), 50.0, "差异 +50");
    let cid = r["id"].as_i64().unwrap();

    // 生成盘盈凭证：借1001 / 贷1901；幂等拒绝；挂凭证不可删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "盘盈凭证应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pvid = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{pvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "1001", "盘盈借现金");
    assert_eq!(v["entries"][1]["account_code"], "1901", "盘盈贷待处理损溢");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不可重复出凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已挂凭证不可删除");

    // 账实相符（实盘800）→ 差异0 → 凭证被拒、可删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/cash-counts",
            &sid,
            serde_json::json!({ "date": "2026-01-13", "account_code": "1001", "counted": "800" }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        money_num(r["diff"].as_str().unwrap()),
        0.0,
        "账实相符差异为0"
    );
    let cid2 = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid2}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "相符不应出凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid2}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "相符记录可删除");
}

/// 资金日报（按日）：上日结余/本日收支/日末结存，仅已记账（H-3）。
#[tokio::test]
async fn funds_daily_by_date_report() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 01-10 现金凭证并记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 61, "attachments": 0, "memo": "日报取数",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "800", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "800" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(resp, "记账应成功").await;

    let fetch = |sid: String, date: &'static str| {
        let state = state.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_get(
                    &format!("/api/funds/daily-by-date?date={date}"),
                    &sid,
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "日报应 200");
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        }
    };

    // 前一日：无发生
    let r = fetch(sid.clone(), "2026-01-09").await;
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["account_code"] == "1001")
        .expect("应有 1001 行");
    assert_eq!(money_num(row["income"].as_str().unwrap()), 0.0);
    assert_eq!(money_num(row["end"].as_str().unwrap()), 0.0);

    // 当日：收入 800、日末 800
    let r = fetch(sid.clone(), "2026-01-10").await;
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["account_code"] == "1001")
        .unwrap();
    assert_eq!(money_num(row["begin"].as_str().unwrap()), 0.0);
    assert_eq!(money_num(row["income"].as_str().unwrap()), 800.0);
    assert_eq!(money_num(row["end"].as_str().unwrap()), 800.0);

    // 次日：上日结余结转
    let r = fetch(sid.clone(), "2026-01-11").await;
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["account_code"] == "1001")
        .unwrap();
    assert_eq!(money_num(row["begin"].as_str().unwrap()), 800.0);
    assert_eq!(money_num(row["income"].as_str().unwrap()), 0.0);
    assert_eq!(money_num(row["end"].as_str().unwrap()), 800.0);
}

/// 台账-总账联动：票据流转自动生成台账凭证；融资到账/结清出凭证；幂等与删除守卫。
#[tokio::test]
async fn funds_ledger_voucher_linkage() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收票据 → 贴现：流转即自动生成凭证（借 100201 / 贷 112101）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/bills",
            &sid,
            serde_json::json!({ "kind": "receivable", "no": "FL001", "period": 202601,
                "issue_date": "2026-01-05", "due_date": "2026-03-05", "counterpart": "客户甲",
                "bank": "工行", "amount": "5000", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建票据应成功");
    let bid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/bills/{bid}/status"),
            &sid,
            serde_json::json!({ "status": "discounted", "date": "2026-01-15" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "贴现流转应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bvid = r["voucher_id"]
        .as_i64()
        .expect("资金流转应自动生成台账凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{bvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["status"], "draft", "台账凭证应为草稿（H-3 不入余额）");
    assert_eq!(v["entries"][0]["account_code"], "100201");
    assert_eq!(v["entries"][1]["account_code"], "112101");
    assert!(v["entries"][0]["debit"]
        .as_str()
        .unwrap()
        .starts_with("5000"));

    // 补出凭证 / 删除已挂凭证的票据 → 被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/bills/{bid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已出凭证应拒绝补出");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/bills/{bid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已挂凭证的票据不可删除"
    );

    // 融资借款：到账凭证 + 结清自动生成还本凭证（幂等）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/loans",
            &sid,
            serde_json::json!({ "kind": "borrow", "no": "LN009", "bank": "工行",
                "principal": "10000", "rate_pct": "4.5", "start_date": "2026-01-01",
                "end_date": "2026-06-30", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建融资应成功");
    let lid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到账凭证应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dvid = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{dvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "100201", "到账：借银行");
    assert_eq!(v["entries"][1]["account_code"], "2001", "到账：贷短期借款");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "到账凭证不可重复生成"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-20" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结清应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let svid = r["voucher_id"].as_i64().expect("结清应自动生成还本凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{svid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "2001", "还本：借短期借款");
    assert_eq!(v["entries"][1]["account_code"], "100201", "还本：贷银行");
    // 结清幂等：重复结清不重复出凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-21" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "重复结清不应重复出凭证：{r}");
}

/// 出纳签字：签字/取消/幂等 + 权限（CashierSign），只读账号应被拒。
#[tokio::test]
async fn cashier_sign_and_unsign() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造一张现金凭证（草稿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 90, "attachments": 0, "memo": "签字流",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "付", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿凭证应可保存");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 签字 → 凭证详情带签字人；重复签字幂等
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "签字应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重复签字应幂等");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["cashier"].as_str(),
        Some("boss"),
        "凭证详情应带签字人：{v}"
    );

    // 取消签字 → 清空
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/unsign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消签字应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(v["cashier"].is_null(), "取消后签字人应清空：{v}");

    // 只读账号（无 CashierSign）→ 403
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({
                "username": "nosign", "display_name": "nosign", "password": "Init@123456"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通 nosign 平台账号");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "nosign", "display_name": "nosign", "password": "",
                "role": "viewer", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请 nosign 为 viewer");
    let (st, vsid) = login(&state, "nosign", "Init@123456").await;
    assert_eq!(st, StatusCode::OK, "nosign 平台登录");
    // 平台账号默认首登强制改密；改密前只能访问改密/退出/登录（服务端拦截）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &vsid,
            serde_json::json!({ "old": "Init@123456", "new": "Pass123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "nosign 首登改密");
    assert_eq!(
        select_book(&state, &vsid, "b1").await,
        StatusCode::OK,
        "nosign 进入 b1"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/sign"),
            &vsid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "只读账号无出纳签字权限"
    );
}

/// require_cashier：开启后现金/银行凭证须签字才能记账；非资金凭证不受限；
/// 签字人出现在出纳日记账。
#[tokio::test]
async fn require_cashier_gates_post_and_scopes_to_funds() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 开启出纳签字前置（先读回再改，避免覆盖其他字段）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    opts["require_cashier"] = serde_json::json!(true);
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开启出纳签字前置应成功");

    // 现金凭证：未签字 → 记账被拒（提示含"出纳签字"）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-11", "word": "记",
                "no": 91, "attachments": 0, "memo": "现金待签字",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "付", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "未签字的现金凭证不应允许记账"
    );
    assert!(
        body_string(resp).await.contains("出纳签字"),
        "错误信息应提示出纳签字"
    );

    // 签字后可记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "签字应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "签字后应能记账");

    // 出纳日记账应带签字人
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/ledger/journal?code=1001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        rows[0]["cashier"].as_str(),
        Some("boss"),
        "出纳日记账应显示签字人：{rows}"
    );

    // 非资金凭证（1901 待处理财产损溢 / 2001 短期借款）不受签字限制
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-12", "word": "记",
                "no": 92, "attachments": 0, "memo": "非资金不签字",
                "entries": [
                    { "line": 1, "account_code": "1901", "summary": "盘亏", "debit": "50", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "转", "debit": "0", "credit": "50" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "非资金凭证应可保存");
    let nid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{nid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "非资金凭证不受出纳签字限制");
}

/// M-15 定案：借贷不平衡的凭证 Web 端必须 400 拒绝，错误信息说明借贷不平衡。
#[tokio::test]
async fn unbalanced_voucher_rejected_m15() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-11", "word": "记",
                "no": 1, "attachments": 0, "memo": "不平衡",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "99.99" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "借贷不平衡必须被拒");
    let body = body_string(resp).await;
    assert!(body.contains("借贷不平衡"), "{body}");
}

/// 报表/账簿的期间参数必须校验：敲错格式或区间倒置要 400，不能静默换一个区间。
///
/// 回归背景（真实生产事故级）：原来的写法是
/// `q.get("from").and_then(parse_period).unwrap_or(默认值)`。
/// `and_then` 在**解析失败**时也返回 `None`，于是「用户把期间敲错」和
/// 「用户没传期间」被混为一谈 —— 两者都静默回退到默认值。
///
/// 危险的地方在于：返回的报表**照样借贷平衡**。生产实测（10 万张已记账凭证、
/// 全年真实值 9,951,930）：
///
/// | from 传法 | 本期借方 | 是否正确 |
/// |---|---|---|
/// | `2026-01` / `202601` | 9,951,930 | ✓ |
/// | `2026-01-01`（日期格式） | 829,300 | ✗ 只算了一个月 |
/// | `garbage` | 829,300 | ✗ HTTP 200 |
/// | 不传 | 829,300 | ✗ HTTP 200 |
///
/// 也就是说财务数字错了，却**没有任何办法从结果上看出来** —— 「试算平衡表
/// 平不平」这个最自然的检查手段失效，因为它平。只能靠报错拦住。
///
/// 界面上起止期间是**无 `type` 的自由文本框**，敲错是常态而非例外。
#[tokio::test]
async fn report_period_params_reject_garbage_instead_of_silently_substituting() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造两个期间的数据，让「区间算错」在数字上真的看得出来
    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "收", "debit": "1000", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "1000" }
        ]),
    )
    .await;

    let debit_of =
        |v: &serde_json::Value| -> f64 { money_num(v["totals"]["end_debit"].as_str().unwrap()) };

    // 基线：合法格式（202601 期间式 / 2026-01 月份式）必须照常 200
    for q in ["from=202601&to=202601", "from=2026-01&to=2026-01", ""] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/reports/trial-balance?{q}"), &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "合法/缺省区间必须 200：{q}");
    }

    // 核心回归：这些以前全部返回 200 + 一份「合法外观的错误报表」
    for bad in [
        "from=2026-01-01&to=2026-12-31", // 日期格式：拼错成日期
        "from=garbage&to=garbage",       // 乱填
        "from=2026%2F01&to=2026%2F12",   // 斜杠分隔
        "from=202613&to=202613",         // 月份 13，不存在
        "from=202613&to=202601",         // 不存在的月份
        "to=2026-12-31",                 // 只错 to
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(
                &format!("/api/reports/trial-balance?{bad}"),
                &sid,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "期间参数非法时必须 400，不能静默换区间返回一份平衡但错误的报表：{bad}"
        );
        // 错误信息要能让人知道怎么改
        let body = body_string(resp).await;
        assert!(
            body.contains("期间"),
            "400 的提示里应说明是期间的问题（便于用户自查）：{bad} -> {body}"
        );
    }

    // 区间倒置：SQL 是 period BETWEEN ?1 AND ?2，倒置会**安静地**返回空表，
    // 用户看到「一张凭证都没有」而真实原因是他填反了 —— 同样要拦
    for bad in ["from=202612&to=202601", "from=2026-06&to=2026-01"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(
                &format!("/api/reports/trial-balance?{bad}"),
                &sid,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "区间倒置必须 400，不能静默返回空表：{bad}"
        );
    }

    // 反向证明：合法区间仍能取到数（确认上面的 400 不是「接口全挂」）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(debit_of(&tb) > 0.0, "合法区间必须能取到发生额：{tb}");

    // 同一套校验必须覆盖其它报表/账簿端点，否则只是把漏洞挪了个位置
    for ep in [
        "/api/reports/balance-sheet?to=2026-12-31",
        "/api/reports/income-statement?from=garbage",
        "/api/reports/cash-flow?to=2026-01-01",
        "/api/reports/equity?period=garbage",
        "/api/reports/summary-table?from=garbage",
        "/api/reports/multi-column?main=1001&cols=2001&from=garbage",
        "/api/ledger?code=1001&from=2026-01-01",
        "/api/ledger/print-form?code=1001&from=garbage",
        "/api/overview?period=2026-01-01",
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(ep, &sid))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "该端点也必须校验期间参数：{ep}"
        );
    }
}

/// 三大报表勾稽：试算平衡、资产=负债+权益、利润表净利。
#[tokio::test]
async fn web_statements_tie() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "收", "debit": "1000", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "1000" }
        ]),
    )
    .await;
    post_voucher(
        &state,
        &sid,
        2,
        serde_json::json!([
            { "line": 1, "account_code": "660201", "summary": "费", "debit": "300", "credit": "0" },
            { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "300" }
        ]),
    )
    .await;

    // 试算平衡
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ed = money_num(tb["totals"]["end_debit"].as_str().unwrap());
    let ec = money_num(tb["totals"]["end_credit"].as_str().unwrap());
    assert!((ed - ec).abs() < 0.005, "试算应平衡：{tb}");

    // 资产负债表：资产总计 == 负债和权益总计
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/balance-sheet?to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bs: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = bs["table"]["rows"].as_array().unwrap();
    let pick = |no: &str| {
        rows.iter()
            .find(|r| r["no"] == serde_json::json!(no))
            .and_then(|r| r["values"][0].as_str())
            .map(money_num)
            .unwrap_or(f64::NAN)
    };
    let asset = pick("17");
    let liab_eq = pick("36");
    assert!(
        !asset.is_nan() && !liab_eq.is_nan(),
        "应有资产/负债权益总计行：{bs}"
    );
    assert!(
        (asset - liab_eq).abs() < 0.005,
        "资产总计 {asset} 应等于负债和权益总计 {liab_eq}"
    );
    assert!((asset - 700.0).abs() < 0.005, "资产总计应为 700：{asset}");

    // 利润表：净亏损 -300
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/income-statement?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    let is: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = is["table"]["rows"].as_array().unwrap();
    let net = rows
        .iter()
        .find(|r| r["no"] == serde_json::json!("15"))
        .and_then(|r| r["values"][0].as_str())
        .map(money_num)
        .unwrap();
    assert!((net + 300.0).abs() < 0.005, "净利润应为 -300：{net}");

    // 现金流量表可生成（未标注时全部未分配，但结构完整）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/cash-flow?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 凭证流转：批量记账 / 红冲 / 删除 / 断号重排 / 取号。
#[tokio::test]
async fn web_voucher_state_ops() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 三张草稿
    let mut ids = Vec::new();
    for no in 1..=3 {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                &sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                    "no": no, "attachments": 0, "memo": "t",
                    "entries": [
                        { "line": 1, "account_code": "1001", "summary": "借", "debit": "10", "credit": "0" },
                        { "line": 2, "account_code": "2001", "summary": "贷", "debit": "0", "credit": "10" }
                    ]
                }),
            ))
            .await
            .unwrap();
        ids.push(
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
                .as_i64()
                .unwrap(),
        );
    }

    // 批量记账前两张
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers/batch-post",
            &sid,
            serde_json::json!({ "ids": [ids[0], ids[1]] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], serde_json::json!(2), "应批量记账 2 张：{r}");

    // 红冲一张已记账凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{}/reverse", ids[0]),
            &sid,
            serde_json::json!({ "period": 202601, "date": "2026-01-16" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "红冲应成功");
    let rid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_ne!(rid, ids[0], "红冲应生成新凭证");

    // 删除第三张草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{}/delete", ids[2]),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除草稿应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{}", ids[2]), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "删除后应查不到");

    // 断号重排与取号
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers/renumber",
            &sid,
            serde_json::json!({ "period": 202601, "word": "记" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重排断号应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/vouchers/next-no?period=202601&word=记",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let n: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(n["no"].as_i64().unwrap_or(0) >= 1, "取号应返回正整数：{n}");
}

/// 发票 CRUD 与工资/报销 CSV 导出。
#[tokio::test]
async fn web_invoice_crud_and_exports() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 发票
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "in", "code": "044001", "number": "10001",
                "date": "2026-01-10", "buyer": "本公司", "seller": "供应商A",
                "amount_tax": "1130", "amount": "1000", "tax": "130",
                "tax_rate": "13", "status": "pending", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增发票应成功");
    let iid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?period=202601", &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(list["rows"].as_array().unwrap().len(), 1);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/invoices/{iid}/status"),
            &sid,
            serde_json::json!({ "status": "verified" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发票认证应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices/summary", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发票汇总应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/invoices/{iid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除发票应成功");

    // 工资行 + 导出
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202601",
            &sid,
            serde_json::json!({
                "employee": "E001", "dept": "财务部", "gross": "10000",
                "social": "500", "housing": "300", "deduction": "0",
                "additional": "0", "social_co": "1000", "housing_co": "300", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存工资行应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/payroll?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("员工") && body.contains("E001"),
        "工资 CSV 应含员工：{body}"
    );

    // 报销单 + 导出
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claims",
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-20", "applicant": "张三",
                "dept": "财务部", "reason": "差旅", "amount": "200",
                "items": [{ "expense_account": "660201", "amount": "200", "memo": "" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增报销单应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/claims?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("单号") && body.contains("张三"),
        "报销 CSV 应含申请人：{body}"
    );
}

/// 银行手工勾对/取消/清空。
#[tokio::test]
async fn web_bank_manual_link_and_clear() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "100201", "summary": "收", "debit": "500", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "500" }
        ]),
    )
    .await;

    // 对账单日期远离凭证日期，自动勾对容差 0 不命中
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/import",
            &sid,
            serde_json::json!({
                "ymm": 202601, "account": "100201",
                "text": "2026-01-25,收款,SN9,400.00,0.00,400.00\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/auto-match",
            &sid,
            serde_json::json!({ "ymm": 202601, "account": "100201", "tolerance": 0 }),
        ))
        .await
        .unwrap();
    let m: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        m["matched"],
        serde_json::json!(0),
        "金额不同不应自动勾对：{m}"
    );

    // 手工勾对 → 取消 → 清空
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/bank?period=202601&account=100201", &sid))
        .await
        .unwrap();
    let d: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let stmt_id = d["statements"][0]["id"].as_i64().unwrap();
    let entry_id = d["book"][0]["entry_id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/link",
            &sid,
            serde_json::json!({ "stmt_id": stmt_id, "entry_id": entry_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "手工勾对应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/unlink",
            &sid,
            serde_json::json!({ "stmt_id": stmt_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消勾对应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/clear",
            &sid,
            serde_json::json!({ "ymm": 202601, "account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["removed"], serde_json::json!(1), "应清空 1 条对账单");
}

/// 往来手工核销 / 记录 / 账龄 / 取消核销。
#[tokio::test]
async fn web_settle_manual_records_aging() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(&state, &sid, 1, serde_json::json!([
        { "line": 1, "account_code": "112201", "summary": "销售", "debit": "500", "credit": "0", "aux": { "customer": "C01" } },
        { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "500" }
    ])).await;
    post_voucher(&state, &sid, 2, serde_json::json!([
        { "line": 1, "account_code": "1001", "summary": "收款", "debit": "200", "credit": "0" },
        { "line": 2, "account_code": "112201", "summary": "核销", "debit": "0", "credit": "200", "aux": { "customer": "C01" } }
    ])).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601&all=1",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    let from_entry = rows
        .iter()
        .find(|r| r["dir"] == serde_json::json!("借"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();
    let to_entry = rows
        .iter()
        .find(|r| r["dir"] == serde_json::json!("贷"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/run",
            &sid,
            serde_json::json!({ "from_entry": from_entry, "to_entry": to_entry, "amount": "200" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "手工核销应成功");
    let rec_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/records?account=112201", &sid))
        .await
        .unwrap();
    let recs: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        recs["rows"].as_array().unwrap().len(),
        1,
        "应有 1 条核销记录"
    );

    // 父级科目查询口径应与未核销/账龄一致（1122 含 112201 的记录）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/records?account=1122", &sid))
        .await
        .unwrap();
    let recs_parent: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        recs_parent["rows"].as_array().unwrap().len(),
        1,
        "父级科目应能查到下级核销记录"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/aging?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ag: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        !ag["buckets"].as_array().unwrap().is_empty(),
        "账龄应有分档"
    );
    assert!(!ag["rows"].as_array().unwrap().is_empty(), "账龄应有数据");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/unsettle",
            &sid,
            serde_json::json!({ "id": rec_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消核销应成功");
}

/// 预设岗位拆权：订单专员只动订单（动不了科目与凭证）；仓管员只动仓储（动不了订单与凭证）。
#[tokio::test]
async fn role_presets_order_clerk_and_keeper() {
    let (state, _bd, _dir) = test_state();
    let admin = boss_in_b1(&state).await;

    // 开平台账号 → 邀请进 b1（口令沿用平台）
    for (u, name, role) in [
        ("ord1", "订单专员", "order_clerk"),
        ("wh1", "仓管员", "keeper"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &admin,
                serde_json::json!({ "username": u, "display_name": name, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台账号 {u}");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &admin,
                serde_json::json!({
                    "username": u, "display_name": name, "password": "",
                    "role": role, "must_change_pwd": false,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {role} 应成功");
    }

    // 登录 + 平台首登强制改密 + 进账套
    let mut sids: Vec<String> = Vec::new();
    for u in ["ord1", "wh1"] {
        let (st, sid) = login(&state, u, "Test12345").await;
        assert_eq!(st, StatusCode::OK, "{u} 登录");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/change-password",
                &sid,
                serde_json::json!({ "old": "Test12345", "new": "Pass123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{u} 首登改密");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 进账套"
        );
        sids.push(sid);
    }
    let clerk = sids[0].clone();
    let keeper = sids[1].clone();

    // 订单专员：能建订单 / 报价（OrderOps）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &clerk,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "2", "unit_price": "10", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单专员应能建销售订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &clerk,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05",
                "customer_code": "C01", "customer_name": "客户甲",
                "item_code": "140501", "item_name": "成品",
                "qty": "1", "unit_price": "5",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单专员应能建报价单");

    // 订单专员：动不了科目（AccountEdit 分离的核心断言）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/accounts",
            &clerk,
            serde_json::json!({
                "account": {
                    "code": "88881", "name": "订单员不该建的科目", "category": "asset", "dir": "debit",
                    "aux": 0, "unit": null, "currency": null, "has_qty": false,
                    "is_cash": false, "is_bank": false, "cash_flow_item": null,
                    "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
                },
                "aux_kinds": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应能维护科目"
    );

    // 两张单都动不了凭证（无 VoucherNew）
    let voucher_payload = || {
        serde_json::json!({
            "id": 0, "period": 202601, "date": "2026-01-31", "word": "记",
            "no": 77, "attachments": 0, "memo": "",
            "entries": [
                { "line": 1, "account_code": "1001", "summary": "x", "debit": "10", "credit": "0" },
                { "line": 2, "account_code": "2001", "summary": "x", "debit": "0", "credit": "10" }
            ],
        })
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &clerk, voucher_payload()))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应能手工录凭证"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &keeper, voucher_payload()))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "仓管员不应能手工录凭证"
    );

    // 仓管员：能读仓储数据（Warehouse）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/unit?item=140301", &keeper))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "仓管员应能读多单位配置");
    // 存货盘点 = 仓储作业：仓管可见、订单专员不可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &keeper))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "仓管员应能进盘点");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "订单专员不应能进盘点");
    // 批次库位 = 仓储作业
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches", &keeper))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "仓管员应能看批次");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "订单专员不应能看批次");

    // 仓管员：动不了订单（反向隔离）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &keeper,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "1", "unit_price": "1", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "仓管员不应能建订单");

    // 订单专员：动不了仓储作业（反向隔离）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/assemble",
            &clerk,
            serde_json::json!({ "parent": "5001", "children": [["500101", "1"]], "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应能组装拆卸"
    );
}

/// 身兼多职（岗位并集）+ 多岗位下会计出纳互斥仍生效 + 价格字段权限（服务端裁剪/置零）。
#[tokio::test]
async fn multi_role_positions_and_price_field_perm() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;

    // 角色清单：12 个预设岗位（含新四岗）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/roles", &boss))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let roles: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let arr = roles.as_array().expect("roles 应是数组");
    assert!(arr.len() >= 12, "应有12个预设角色，实际 {}", arr.len());
    for code in [
        "receivables",
        "payables",
        "cost_accountant",
        "production",
        "order_clerk",
        "keeper",
    ] {
        assert!(arr.iter().any(|r| r["role"] == code), "缺少角色 {code}");
    }

    // 三个账号先开平台（邀请前必须存在同名平台账号）
    for u in ["mul1", "mix3", "prc1"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss,
                serde_json::json!({ "username": u, "display_name": u, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台 {u}");
    }
    // mul1：主岗位订单专员 + 兼任仓管员（身兼多职）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({
                "username": "mul1", "display_name": "多面手", "password": "",
                "role": "order_clerk", "roles": ["keeper"], "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "多岗位邀请应成功：{b}");
    // mix3：会计 + 兼任出纳 → 互斥（多岗位并集口径）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({
                "username": "mix3", "display_name": "混岗", "password": "",
                "role": "accountant", "roles": ["cashier"], "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "会计+出纳多岗位应被拒"
    );
    let s = body_string(resp).await;
    assert!(s.contains("会计与出纳"), "应提示互斥：{s}");
    // prc1：订单专员但被 deny 掉价格两权（字段级权限样本）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({
                "username": "prc1", "display_name": "录单员", "password": "",
                "role": "order_clerk", "deny_perms": ["price_view", "price_edit"],
                "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "deny 价格权邀请应成功");

    // mul1 / prc1 进账套
    let mut sids = std::collections::HashMap::new();
    for u in ["mul1", "prc1"] {
        let (st, sid) = login(&state, u, "Test12345").await;
        assert_eq!(st, StatusCode::OK, "{u} 登录");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/change-password",
                &sid,
                serde_json::json!({ "old": "Test12345", "new": "Pass123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{u} 首登改密");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 进账套"
        );
        sids.insert(u, sid);
    }
    let mul = sids["mul1"].clone();
    let prc = sids["prc1"].clone();

    // mul1 身兼多职：仓管域 + 订单域都通
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/unit?item=140301", &mul))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "兼岗（仓管）应能读仓储");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &mul,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "10", "unit_price": "5", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "兼岗（订单）应能建单");
    let r: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_get("/api/sales/so?period=202601", &mul))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let mul_row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["customer_code"] == "C01")
        .unwrap();
    assert_eq!(
        money_num(mul_row["total_amount"].as_str().unwrap()),
        50.0,
        "有价格权应看到真实金额"
    );

    // prc1 无价格权：建单价格被服务端置零、数量保留、套打无价格列
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &prc,
            serde_json::json!({
                "period": 202601, "date": "2026-01-06", "customer_code": "C02",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "prc1 建单应成功（数量域放行）"
    );
    let r: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_get("/api/sales/so?period=202601", &prc))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let prc_row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["customer_code"] == "C02")
        .unwrap();
    let prc_id = prc_row["id"].as_i64().unwrap();
    assert_eq!(
        money_num(prc_row["total_amount"].as_str().unwrap()),
        0.0,
        "无价格权金额应被置零"
    );
    assert_eq!(prc_row["lines"][0]["qty_ordered"], "10", "数量应保留");
    assert_eq!(
        money_num(prc_row["lines"][0]["unit_price"].as_str().unwrap()),
        0.0,
        "单价应被置零"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/sales/so/print-form?ids={prc_id}"),
            &prc,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;
    assert!(!html.contains("单价"), "无价格权套打不应出现单价列：{html}");

    // prc1 没有仓管权（兼岗是显式授予的，不是人人有）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/unit?item=140301", &prc))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "无仓管兼岗应403");
}

/// 批次库位流程：登记（自动批号/流水带批）→ 余额 → FEFO → 库位 CRUD。
#[tokio::test]
async fn stock_batch_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 入库：批号自动生成
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batch",
            &sid,
            serde_json::json!({
                "item": "RM10", "batch_no": "", "production_date": "2026-01-10",
                "qty": "50", "direction": "in"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "批次入库应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let auto_no = r["batch_no"].as_str().unwrap().to_string();
    assert!(
        auto_no.starts_with("BT"),
        "自动批号 BT+日期+序号：{auto_no}"
    );
    assert_eq!(money_num(r["balance"].as_str().unwrap()), 50.0);

    // 第二批（指定批号、生产日期更早）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batch",
            &sid,
            serde_json::json!({
                "item": "RM10", "batch_no": "B-001", "production_date": "2026-01-05",
                "warehouse": "WH1", "location": "L1", "qty": "30", "direction": "in"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 出库20 → 余额10
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batch",
            &sid,
            serde_json::json!({ "item": "RM10", "batch_no": "B-001", "qty": "20", "direction": "out" }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        money_num(r["balance"].as_str().unwrap()),
        10.0,
        "出库后余额10"
    );

    // 列表 + 库存流水同账（批次余额与普通库存一致）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches?item=RM10", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let ws = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/warehouse-stock?item=RM10", &sid))
        .await
        .unwrap();
    assert_eq!(
        ws.status(),
        StatusCode::OK,
        "仓库库存查询可用（批次=同一本账）"
    );

    // FEFO：生产日期早的先出（B-001 余额10 → 再吃自动批）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batches/fefo",
            &sid,
            serde_json::json!({ "item": "RM10", "qty": "60" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rec = r["rows"].as_array().unwrap();
    assert_eq!(rec[0]["batch_no"], "B-001", "近生产日期先出：{rec:?}");
    assert_eq!(money_num(rec[0]["take"].as_str().unwrap()), 10.0);
    assert_eq!(rec[1]["batch_no"], auto_no);
    assert_eq!(money_num(rec[1]["take"].as_str().unwrap()), 50.0);

    // 临期：未配保质期 → 无失效日期 → 空
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches/expiring?days=30", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 库位 CRUD + 校验
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/locations",
            &sid,
            serde_json::json!({ "code": "L01", "name": "A区货位", "kind": "storage", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增库位应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/locations", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let locs = r["rows"].as_array().unwrap();
    assert_eq!(locs.len(), 1);
    let lid = locs[0]["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/locations",
            &sid,
            serde_json::json!({ "code": "L02", "name": "坏类型", "kind": "bad" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非法库位类型应拒绝");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/locations/{lid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 销售出库闭环（与采购侧对称）：未确认拒发 → 发货写库存出库流水 → 超退防呆 → 退货回库。
#[tokio::test]
async fn so_shipment_stock_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let d12 = format!("{cur_label}-12");
    let d13 = format!("{cur_label}-13");
    let d14 = format!("{cur_label}-14");

    // 采购入库 10×9（库存基线 10）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d12.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d12.clone(), "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购入库");

    // 销售订单（C01，6 件 × 5）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d13.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "出库造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "6", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建销售订单");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未确认订单 → 发货拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d14.clone(), "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未确认不能发货");

    // 确认 → 发货 4：库存 10-4=6 + 收入凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d14.clone(), "qty": "4", "memo": "首批" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["voucher_id"].as_i64().unwrap_or(0) > 0,
        "发货应生成收入凭证：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140301",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let on_hand: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(on_hand, 6.0, "发货 4 → 库存 10-4=6（销售出库流水生效）");

    // 超退 8（净发货 4）→ 拒；退货 2 → 库存回 8
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/return",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d14.clone(), "qty": "8", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超退应拒");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/return",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d14.clone(), "qty": "2", "memo": "部分退回" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "退货");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140301",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let on_hand: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(on_hand, 8.0, "退货 2 → 库存回 8（销售流水回库）");
}

/// 存货核算↔总账对账：采购到货（有流水无凭证）差异 90 → 暂估凭证记账后对平。
#[tokio::test]
async fn gl_reconcile_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let d15 = format!("{cur_label}-15");
    let d10 = format!("{cur_label}-10");

    // 带价采购 9×10 → 到货（库存金额 90，尚无总账凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "对账造数",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d15.clone(), "qty": "10", "memo": "入库" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货入库");

    // 对账：140301 库存 90 / 总账 0 → 差异 90（未暂估）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/cost/gl-reconcile?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "对账端点可用");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item"] == "140301")
        .expect("对账表应含 140301");
    assert_eq!(
        money_num(row["stock_value"].as_str().unwrap()),
        90.0,
        "库存侧 90"
    );
    assert_eq!(
        money_num(row["gl_value"].as_str().unwrap()),
        0.0,
        "总账侧尚未有凭证"
    );
    assert_eq!(money_num(row["diff"].as_str().unwrap()), 90.0, "差异 90");
    assert_eq!(money_num(r["diff_total"].as_str().unwrap()), 90.0);

    // 手工暂估凭证（借 140301 带存货辅助 / 贷应付）→ 记账后入余额 → 对平
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d10.clone(), "word": "记", "no": 1,
                "attachments": 0, "memo": "暂估入库",
                "entries": [
                    { "line": 1, "account_code": "140301", "summary": "暂估入库",
                      "debit": "90", "credit": "0", "qty": "10", "price": "9",
                      "aux": { "item": "140301" } },
                    { "line": 2, "account_code": "220201", "summary": "暂估入库",
                      "debit": "0", "credit": "90", "aux": { "supplier": "S01" } }
                ]
            }),
        ))
        .await
        .unwrap();
    let vb = body_string(resp).await;
    let vid = serde_json::from_str::<serde_json::Value>(&vb).unwrap()["id"]
        .as_i64()
        .unwrap_or(0);
    assert!(vid > 0, "RECON_VB={vb}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账（余额口径=已记账）");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/cost/gl-reconcile?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item"] == "140301")
        .unwrap();
    assert_eq!(
        money_num(row["gl_value"].as_str().unwrap()),
        90.0,
        "总账侧 90"
    );
    assert_eq!(money_num(row["diff"].as_str().unwrap()), 0.0, "对平");
    assert_eq!(
        money_num(r["diff_total"].as_str().unwrap()),
        0.0,
        "总差异归零"
    );
}

/// 委外加工全链（委外=生产的变体）：建单(kind=outsourcing) → BOM+标准价 → 领料 → 开工 →
/// 加工费凭证（借500102/贷应付-供应商）→ 完工结转含加工费；非委外单确认加工费 400。
#[tokio::test]
async fn outsourcing_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 当前期间与日期
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let d15 = format!("{cur_label}-15");
    let d16 = format!("{cur_label}-16");
    let d20 = format!("{cur_label}-20");

    // 标准价 + BOM（1 成品 = 2 原料）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 建委外订单（10 件，供应商 S01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({
                "item_code": "140501", "qty": "10", "kind": "outsourcing",
                "supplier_code": "S01", "supplier_name": "供应商甲", "date": d15.clone()
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建委外订单");
    let body = body_string(resp).await;
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let po_id = created["id"].as_i64().unwrap();

    // 列表带出委外标识与供应商
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(po_id))
        .unwrap();
    assert_eq!(row["order_kind"], "outsourcing");
    assert_eq!(row["supplier_name"], "供应商甲");

    // 领料（20 件 × 10 = 200）→ 开工
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/issue"),
            &sid,
            serde_json::json!({ "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "委外发料");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        money_num(r["total"].as_str().unwrap()),
        200.0,
        "发料成本 200"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");

    // 加工费 300 → 凭证 借500102 / 贷应付(供应商辅助)
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/outsource-fee"),
            &sid,
            serde_json::json!({ "amount": "300", "date": d16.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认加工费");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let fee_vid = r["voucher_id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{fee_vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let dr = entries
        .iter()
        .find(|e| e["account_code"] == "500102")
        .expect("借 500102 直接人工");
    assert_eq!(money_num(dr["debit"].as_str().unwrap()), 300.0);
    let cr = entries
        .iter()
        .find(|e| money_num(e["credit"].as_str().unwrap()) == 300.0)
        .expect("贷应付 300");
    assert_eq!(cr["aux"]["supplier"], "S01", "应付带供应商辅助");

    // 非委外订单确认加工费 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "1", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    let plain: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let plain_id = plain["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{plain_id}/outsource-fee"),
            &sid,
            serde_json::json!({ "amount": "50", "date": d16.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "非委外订单不能确认加工费"
    );

    // 完工 10 件 → 结转 = 料 200 + 工 300 = 500（借 140501；贷 500101=200、500102=300）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/complete"),
            &sid,
            serde_json::json!({ "qty": "10", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "委外完工入库");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_str(&body_string(resp).await).unwrap();
    let comp = list
        .iter()
        .find(|x| {
            x["summary"]
                .as_str()
                .map(|s| s.contains("完工"))
                .unwrap_or(false)
        })
        .expect("应有完工凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers/{}", comp["id"].as_i64().unwrap()),
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let dr = entries
        .iter()
        .find(|e| e["account_code"] == "140501")
        .expect("借库存商品");
    assert_eq!(
        money_num(dr["debit"].as_str().unwrap()),
        500.0,
        "完工结转 = 料200+工300"
    );
    let l500101 = entries
        .iter()
        .find(|e| e["account_code"] == "500101")
        .expect("贷直接材料");
    assert_eq!(money_num(l500101["credit"].as_str().unwrap()), 200.0);
    let l500102 = entries
        .iter()
        .find(|e| e["account_code"] == "500102")
        .expect("贷直接人工（加工费）");
    assert_eq!(money_num(l500102["credit"].as_str().unwrap()), 300.0);
}

/// 最近价带出：采购订单保存自动沉淀价格历史，按日期倒序返回最新价。
#[tokio::test]
async fn price_history_suggest() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 初始无历史
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/price-history?item=RM77", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 0);

    // 建单（9 元）→ 自动沉淀
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-15", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "RM77", "qty_ordered": "3", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/price-history?item=RM77", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "保存订单应沉淀价格历史");
    assert_eq!(rows[0]["price"], "9");
    assert_eq!(rows[0]["supplier"], "S01");

    // 第二天 10 元 → 最新在前
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-16", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "RM77", "qty_ordered": "5", "unit_price": "10", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/price-history?item=RM77", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["price"], "10", "按日期倒序，最新价在前");

    // 缺 item → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/price-history", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// 库存作业：到货即入库（补价防呆）→ 质检不合格自动退货 → 超退防呆 → 形态转换 → 低库存端点。
#[tokio::test]
async fn inventory_qc_convert_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 带价采购订单（20 件 × 8 元）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-11", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "质检链造数",
                "lines": [{ "item_code": "140301", "qty_ordered": "20", "unit_price": "8", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单");
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 到货 20 → 采购入库流水（kind=purchase）库存 +20
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-12", "qty": "20", "memo": "首批" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货入库");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140301",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let on_hand: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(on_hand, 20.0, "到货后 140301 库存 20");

    // 质检：检验 20、不合格 5 → 自动退货 → 库存 15
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/qc",
            &sid,
            serde_json::json!({
                "po_id": po_id, "qty_insp": "20", "qty_fail": "5",
                "inspector": "质检员甲", "date": "2026-01-13", "memo": "外观不良"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "质检保存");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["qty_pass"], "15");
    assert_eq!(r["qty_fail"], "5");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140301",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let on_hand: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(on_hand, 15.0, "不合格 5 已退货 → 库存 15");

    // 质检防呆：不合格 > 检验 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/qc",
            &sid,
            serde_json::json!({ "po_id": po_id, "qty_insp": "5", "qty_fail": "9" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不合格超检验应拒");

    // 超退防呆：直接退货 20 > 累计净收货 15 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/return",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-14", "qty": "20", "memo": "超退测试" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超退应拒");

    // 形态转换 140301 → 140501 ×10：5 / 10
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/form-convert",
            &sid,
            serde_json::json!({ "from_item": "140301", "to_item": "140501", "qty": "10", "date": "2026-01-15", "memo": "转产" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "形态转换");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140301",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let q: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(q, 5.0, "源物料剩 5");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140501",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let q: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(q, 10.0, "目标物料 +10");

    // 同物料转换 → 400；低库存端点 200
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/form-convert",
            &sid,
            serde_json::json!({ "from_item": "140301", "to_item": "140301", "qty": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "同物料转换应拒");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/below-safety", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "低库存端点可用");
}

/// Ctrl+K 快速搜索：凭证/请购/报销按关键词分域返回 + 空词与无结果边界。
#[tokio::test]
async fn quick_search_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 空词 → 空结果
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/quick-search?q=", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 0, "空词不搜");

    // 造请购（品名关键词）→ 搜到 req 类
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/req",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-09",
                "item_code": "RM9", "item_name": "快搜物料", "qty": "5",
                "status": "draft", "requester": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/quick-search?q=%E5%BF%AB%E6%90%9C%E7%89%A9%E6%96%99",
            &sid,
        )) // 快搜物料
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "req" && x["view"] == "po-doc"),
        "应搜到请购：{r}"
    );

    // 造凭证（memo+摘要关键词）→ 搜到 voucher 类
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": format!("{cur_label}-06"),
                "word": "记", "no": 1, "attachments": 0, "memo": "快搜凭证摘录",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "快搜凭证摘录", "debit": "20", "credit": "0" },
                    { "line": 2, "account_code": "660201", "summary": "快搜凭证摘录", "debit": "0", "credit": "20" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/quick-search?q=%E5%BF%AB%E6%90%9C%E5%87%AD%E8%AF%81",
            &sid,
        )) // 快搜凭证
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "voucher" && x["view"] == "vouchers"),
        "应搜到凭证：{r}"
    );

    // 造报销（事由关键词）→ 搜到 claim 类（数据范围内）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claims",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "biz_date": format!("{cur_label}-07"),
                "applicant": "张三", "dept": "销售部", "reason": "快搜报销事由",
                "amount": "88",
                "items": [{ "expense_account": "660201", "amount": "88", "memo": "" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造报销");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/quick-search?q=%E5%BF%AB%E6%90%9C%E6%8A%A5%E9%94%80",
            &sid,
        )) // 快搜报销
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "claim" && x["view"] == "claims"),
        "应搜到报销：{r}"
    );

    // 无结果 → 200 空数组
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/quick-search?q=zzzz-not-exist", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 0, "无结果不报错");
}

/// Ctrl+K 搜档案：编码/名称命中 → kind=aux 且带 aux_kind 跳转字段
#[tokio::test]
async fn quick_search_aux() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建一条客户档案（名字带少见字，避免撞上预置档案）
    let ent = serde_json::json!({
        "id": 0, "kind": "customer", "code": "C077", "name": "快搜氧化客户",
        "parent_code": null, "disabled": false, "props": {}, "memo": ""
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/aux", &sid, ent))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建档案应成功");

    // 按名称搜到
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/quick-search?q=%E5%BF%AB%E6%90%9C%E6%B0%A7%E5%8C%96",
            &sid,
        )) // 快搜氧化
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "aux" && x["view"] == "aux" && x["aux_kind"] == "customer"),
        "应搜到档案并带跳转字段：{r}"
    );

    // 按编码搜到
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/quick-search?q=C077", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "aux"),
        "编码应搜到档案：{r}"
    );
}

/// 单据下推与追溯（对标金蝶源单→目标单）：审批 → 下推PO → 双向链 → 到货流水并入 → 拆单。
#[tokio::test]
async fn doc_push_chain() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建请购（draft）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/req",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-08",
                "item_code": "140301", "item_name": "原料", "qty": "30",
                "status": "draft", "requester": "张三", "memo": "下推链造数"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建请购");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/req?period=202601", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let req = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["memo"] == "下推链造数")
        .expect("请购应存在");
    let req_id = req["id"].as_i64().unwrap();
    assert_eq!(req["status"], "draft");

    // 未审批 → 下推拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/req/{req_id}/push-po"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未审批不能下推");

    // 审批（无工作流 → 默认流直接通过）→ 下推成功
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/req/{req_id}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审批请购");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/req/{req_id}/push-po"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下推应成功");
    let body = body_string(resp).await;
    let push: serde_json::Value = serde_json::from_str(&body).unwrap();
    let po_id = push["po_id"].as_i64().unwrap();
    assert!(
        push["po_no"].as_str().unwrap().starts_with("CG"),
        "订单号 CG 前缀：{body}"
    );

    // 请购 → ordered；链双向
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/req?period=202601", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let req = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"].as_i64() == Some(req_id))
        .unwrap();
    assert_eq!(req["status"], "ordered", "下推后请购应为已下推");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=po&id={po_id}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let links: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        links["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "req" && n["dir"] == "up" && n["id"].as_i64() == Some(req_id)),
        "PO 应见上游请购：{links}"
    );

    // 0 价防呆：未补价到货 → 400（0 价入库会污染移动平均）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-10", "qty": "30", "memo": "到货" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "0 价订单到货应被防呆拦截"
    );
    // 补价 + 补供应商（save_po 按 id 更新）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "id": po_id, "period": 202601, "date": "2026-01-08",
                "supplier_code": "S01", "supplier_name": "供应商甲",
                "status": "Draft", "memo": "源：请购",
                "lines": [{ "item_code": "140301", "qty_ordered": "30", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "补价应成功");
    // 到货执行 → 采购入库库存流水 + 流水并入链（下游 receipt）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-10", "qty": "30", "memo": "到货" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货登记（补价后）");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=po&id={po_id}"),
            &sid,
        ))
        .await
        .unwrap();
    let links: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        links["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["kind"] == "receipt" && n["dir"] == "down"),
        "PO 链应含到货流水：{links}"
    );

    // 拆单：再次下推 → 请购链两个下游 PO
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/req/{req_id}/push-po"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "拆单再下推");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=req&id={req_id}"),
            &sid,
        ))
        .await
        .unwrap();
    let links: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pos: Vec<_> = links["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["kind"] == "po" && n["dir"] == "down")
        .collect();
    assert_eq!(pos.len(), 2, "请购应见两张下游订单：{links}");

    // kind 非法 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/doc-links?kind=bogus&id=1", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// 我的工作台：权限分域矩阵 + 多期趋势长度 + 待办计数联动。
#[tokio::test]
async fn workbench_role_matrix() {
    let (state, _bd, _dir) = test_state();
    let admin = boss_in_b1(&state).await;

    // admin：全域 + 趋势期数 = 6
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workbench?periods=6", &admin))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "工作台端点应可用");
    let wb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let mut doms: Vec<&str> = wb["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["domain"].as_str().unwrap())
        .collect();
    doms.sort();
    doms.dedup();
    for expect in [
        "凭证", "报表", "资金", "销售", "采购", "仓管", "生产", "成本", "报销", "审批",
    ] {
        assert!(doms.contains(&expect), "admin 应含 {expect} 域：{doms:?}");
    }
    for t in wb["trends"].as_array().unwrap() {
        assert_eq!(
            t["periods"].as_array().unwrap().len(),
            6,
            "趋势期数应为 6：{}",
            t["key"]
        );
    }
    assert!(
        wb["todos"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["key"] == "voucher_unposted"),
        "admin 应有未记账凭证待办"
    );

    // 造一张未记账凭证 → 待办计数联动 ≥1
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &admin))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &admin,
            serde_json::json!({
                "id": 0, "period": cur_ymm,
                "date": format!("{cur_label}-05"),
                "word": "记", "no": 1, "attachments": 0, "memo": "工作台待办造数",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "备用金", "debit": "50", "credit": "0" },
                    { "line": 2, "account_code": "660201", "summary": "备用金", "debit": "0", "credit": "50" }
                ]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let vbody = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建未记账凭证：{vbody}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workbench?periods=6", &admin))
        .await
        .unwrap();
    let wb2: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let tu = wb2["todos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "voucher_unposted")
        .unwrap();
    assert!(tu["count"].as_i64().unwrap() >= 1, "未记账待办应 ≥1：{tu}");

    // 分域矩阵：keeper 只见 仓管/报销/审批；order_clerk 见 销售/采购
    for (u, name, role) in [
        ("wbk1", "仓管甲", "keeper"),
        ("wbc1", "订单甲", "order_clerk"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &admin,
                serde_json::json!({ "username": u, "display_name": name, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通 {u}");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &admin,
                serde_json::json!({ "username": u, "display_name": name, "password": "", "role": role, "must_change_pwd": false }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {role}");
    }
    let mut sids: Vec<String> = Vec::new();
    for u in ["wbk1", "wbc1"] {
        let (st, sid) = login(&state, u, "Test12345").await;
        assert_eq!(st, StatusCode::OK, "{u} 登录");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/change-password",
                &sid,
                serde_json::json!({ "old": "Test12345", "new": "Pass123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{u} 首登改密");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 进账套"
        );
        sids.push(sid);
    }
    let (keeper, clerk) = (sids[0].clone(), sids[1].clone());

    let doms_of = |wb: &serde_json::Value| -> Vec<String> {
        let mut d: Vec<String> = wb["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["domain"].as_str().unwrap().to_string())
            .collect();
        d.sort();
        d.dedup();
        d
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workbench?periods=6", &keeper))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "仓管可进工作台");
    let wk = doms_of(&serde_json::from_str(&body_string(resp).await).unwrap());
    assert!(wk.contains(&"仓管".to_string()), "仓管应含仓管域：{wk:?}");
    assert!(wk.contains(&"报销".to_string()));
    assert!(
        !wk.contains(&"凭证".to_string()),
        "仓管不应见凭证域：{wk:?}"
    );
    assert!(!wk.contains(&"资金".to_string()));
    assert!(!wk.contains(&"销售".to_string()));
    assert!(
        !wk.contains(&"报表".to_string()),
        "仓管无 FinReport → 无报表域"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workbench?periods=6", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单员可进工作台");
    let cl = doms_of(&serde_json::from_str(&body_string(resp).await).unwrap());
    assert!(
        cl.contains(&"销售".to_string()) && cl.contains(&"采购".to_string()),
        "订单员应含销售/采购：{cl:?}"
    );
    assert!(
        !cl.contains(&"仓管".to_string())
            && !cl.contains(&"凭证".to_string())
            && !cl.contains(&"资金".to_string())
    );
}

/// 工厂生产链（对标金蝶「业务单据同步凭证」）：标准价+BOM → 下达 →（未开工完工拒）
/// → 领料(借500101/贷140301) → 开工 → 完工(借140501/贷500101) 双凭证与状态链。
#[tokio::test]
async fn production_issue_complete_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 当前期间（生产订单与凭证期间随账套当前期间；日期取该期间内一天）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur = dash["current_period"].as_str().unwrap().to_string(); // 形如 2026-01
    let d15 = format!("{cur}-15");
    let d20 = format!("{cur}-20");
    let cur_ymm = cur.replace('-', ""); // 查询参数用 yyyymm

    // 1) 标准价（领料成本回退口径：无采购单价时按计价配置标准价）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "标准价配置应成功");

    // 2) BOM：1 件成品(140501) = 2 件原料(140301)
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存BOM应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/bom?parent=140501", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let brows = r["rows"].as_array().unwrap();
    assert_eq!(brows.len(), 1);
    assert_eq!(brows[0]["qty"], serde_json::json!("2"));

    // 3) 下达生产订单（计划量0拒绝）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "0" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "计划量0应拒绝");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "10", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下达应成功");
    let created: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po_id = created["id"].as_i64().unwrap();

    // 4) 状态链 Released →（未开工完工拒）→ 领料 → 开工 → InProgress → 完工 → Completed
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(po_id))
        .expect("列表应含新下达的订单");
    assert_eq!(po["status"], "Released", "下达后状态应为已下达");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/complete"),
            &sid,
            serde_json::json!({ "qty": "10", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "未开工不能完工入库");

    // 领料：BOM 2×10×(1+0) = 20 件 × 标准价10 = 200
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/issue"),
            &sid,
            serde_json::json!({ "date": d15.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let issue_body = body_string(resp).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "领料应成功：{issue_body}｜cur={cur} d15={d15}"
    );
    let r: serde_json::Value = serde_json::from_str(&issue_body).unwrap();
    assert_eq!(r["items"], 1, "一个子件一项领料");
    assert_eq!(
        money_num(r["total"].as_str().unwrap()),
        200.0,
        "领料成本 = 20×10"
    );

    // 按单限额领料：重复领料 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/issue"),
            &sid,
            serde_json::json!({ "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "重复领料应被限额拦截"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "重复开工应拒绝");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/complete"),
            &sid,
            serde_json::json!({ "qty": "10", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "完工入库应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(r["qty"].as_str().unwrap()), 10.0);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(po_id))
        .unwrap();
    assert_eq!(po["status"], "Completed", "完工后状态应为已完工");
    assert_eq!(money_num(po["completed_qty"].as_str().unwrap()), 10.0);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/complete"),
            &sid,
            serde_json::json!({ "qty": "1", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "已完工订单不能重复完工");

    // 5) 双凭证：列表（裸数组，summary 字段）定位 → 明细 entries（account_code）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_str(&body_string(resp).await).unwrap();
    let mat = list
        .iter()
        .find(|v| {
            v["summary"]
                .as_str()
                .map(|s| s.contains("领料"))
                .unwrap_or(false)
        })
        .expect("应有领料凭证");
    let comp = list
        .iter()
        .find(|v| {
            v["summary"]
                .as_str()
                .map(|s| s.contains("完工"))
                .unwrap_or(false)
        })
        .expect("应有完工凭证");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers/{}", mat["id"].as_i64().unwrap()),
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let d = entries
        .iter()
        .find(|e| e["account_code"] == "500101")
        .expect("领料借 500101 生产成本-直接材料");
    assert_eq!(money_num(d["debit"].as_str().unwrap()), 200.0);
    let c = entries
        .iter()
        .find(|e| e["account_code"] == "140301")
        .expect("领料贷 140301 原材料");
    assert_eq!(money_num(c["credit"].as_str().unwrap()), 200.0);

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers/{}", comp["id"].as_i64().unwrap()),
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let d = entries
        .iter()
        .find(|e| e["account_code"] == "140501")
        .expect("完工借 140501 库存商品");
    assert_eq!(money_num(d["debit"].as_str().unwrap()), 200.0);
    let c = entries
        .iter()
        .find(|e| e["account_code"] == "500101")
        .expect("完工贷 500101 生产成本-直接材料");
    assert_eq!(money_num(c["credit"].as_str().unwrap()), 200.0);
}

/// 可视化工作流（对标金蝶审批流）：设计 → 发布 → 单据审批自动入流逐节点推进 → 默认流回退。
#[tokio::test]
async fn workflow_visual_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建报价（流程发布前先批一张 → 默认流立即生效）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05",
                "customer_code": "C01", "customer_name": "客户甲",
                "item_code": "140501", "item_name": "成品",
                "qty": "1", "unit_price": "5",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let q1 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q1}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["pending"].is_null(),
        "无流程 → 默认流立即批准，无 pending：{r}"
    );

    // 设计两节点流（校验 + 保存 + 发布）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows",
            &sid,
            serde_json::json!({
                "id": 0, "name": "报价两级审批", "biz_type": "quotation",
                "nodes": [
                    { "id": "n1", "type": "start", "name": "开始" },
                    { "id": "n2", "type": "approve", "name": "初审" },
                    { "id": "n3", "type": "approve", "name": "复核" }
                ],
                "edges": [
                    { "id": "e1", "from": "n1", "to": "n2", "kind": "normal" },
                    { "id": "e2", "from": "n2", "to": "n3", "kind": "normal" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存流程应成功");
    let flow_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    // 无开始节点 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows",
            &sid,
            serde_json::json!({
                "name": "坏流程", "biz_type": "quotation",
                "nodes": [{ "id": "x1", "type": "approve", "name": "审批" }], "edges": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "缺开始节点应拒绝");
    // 发布
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/workflows/{flow_id}/publish"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发布应成功");

    // 流程内报价：第一节点 → pending（单据不动），第二节点 → 终态批准
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06",
                "customer_code": "C02", "customer_name": "客户乙",
                "item_code": "140501", "item_name": "成品",
                "qty": "2", "unit_price": "10",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let q2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q2}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["pending"],
        serde_json::json!("复核"),
        "首节点应返回下一节点：{r}"
    );
    // 单据此时仍未批准
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/quote?period=202601", &sid))
        .await
        .unwrap();
    let rows = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap();
    let row = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == q2)
        .unwrap();
    assert_eq!(row["status"], "draft", "pending 阶段单据不应被批准");
    // 第二节点 → 终态 → 原批准执行
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q2}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["pending"].is_null(), "终节点不应再有 pending：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/quote?period=202601", &sid))
        .await
        .unwrap();
    let rows = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap();
    let row = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == q2)
        .unwrap();
    assert_eq!(row["status"], "approved", "终态后单据应已批准");

    // 实例回放：q2 应产生 approved 实例
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workflows/instances", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let inst = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap();
    let arr = inst["rows"].as_array().unwrap();
    assert!(
        arr.iter().any(|i| {
            i["biz_type"] == "quotation"
                && i["biz_id"].as_i64() == Some(q2)
                && i["status"] == "approved"
        }),
        "应存在 q2 的已通过实例"
    );

    // 删除有运行中实例的流程 → 拒（再造第三张报价走首节点后删除）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-07",
                "customer_code": "C03", "customer_name": "客户丙",
                "item_code": "140501", "item_name": "成品",
                "qty": "3", "unit_price": "5",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let q3 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q3}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/workflows/{flow_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "有运行中实例不能删流程"
    );
}

/// 存货盘点流程：账面快照 → 应用（其他入库流水 + 盘盈盘亏凭证 1901）→ 守卫。
#[tokio::test]
async fn stock_count_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 配置标准价（金额 = 差异 × 标准价）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "配置标准价应成功");

    // 新建盘点：140301 账面0 → 实盘5（盘盈 +5 × 10 = 50）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/count",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-20", "warehouse": "", "memo": "一月盘点",
                "lines": [{ "item": "140301", "count_qty": "5", "memo": "" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新建盘点应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cid = r["id"].as_i64().unwrap();
    assert!(
        r["no"].as_str().unwrap().starts_with("PD"),
        "应生成盘点单号"
    );

    // 列表：账面快照为0、状态草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == cid)
        .unwrap();
    assert_eq!(row["status"], "draft");
    assert_eq!(
        money_num(row["lines"][0]["book_qty"].as_str().unwrap()),
        0.0,
        "账面应快照为0"
    );

    // 应用 → 盘盈盘亏凭证（借 140301 / 贷 1901）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid}/apply"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "应用应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["voucher_id"].as_i64().expect("有标准价应出凭证");
    assert_eq!(money_num(r["value"].as_str().unwrap()), 50.0, "价值 = 5×10");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "140301");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 50.0);
    assert_eq!(v["entries"][0]["aux"]["item"], "140301");
    assert_eq!(v["entries"][1]["account_code"], "1901");
    assert_eq!(money_num(v["entries"][1]["credit"].as_str().unwrap()), 50.0);

    // 已应用：状态、删除守卫、重复应用拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == cid)
        .unwrap();
    assert_eq!(row["status"], "applied");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已应用盘点单不可删");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid}/apply"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复应用应拒绝");

    // 草稿可删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/count",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-21", "warehouse": "",
                "lines": [{ "item": "140301", "count_qty": "3" }]
            }),
        ))
        .await
        .unwrap();
    let cid2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid2}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿盘点单应可删");
}

/// 报表分层（对标金蝶报表按角色授权）：业务岗只见业务报表；账簿/三大表/资金需 FinReport。
#[tokio::test]
async fn fin_report_vs_business_report() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;

    // 建订单专员 cl1（无 FinReport）与出纳 ca1（有 FinReport）
    for (u, role) in [("cl1", "order_clerk"), ("ca1", "cashier")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss,
                serde_json::json!({ "username": u, "display_name": u, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台 {u}");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &boss,
                serde_json::json!({ "username": u, "display_name": u, "password": "", "role": role, "must_change_pwd": false }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {role}");
    }
    let mut sids = std::collections::HashMap::new();
    for u in ["cl1", "ca1"] {
        let (st, sid) = login(&state, u, "Test12345").await;
        assert_eq!(st, StatusCode::OK, "{u} 登录");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/change-password",
                &sid,
                serde_json::json!({ "old": "Test12345", "new": "Pass123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{u} 改密");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 进账套"
        );
        sids.insert(u, sid);
    }
    let clerk = sids["cl1"].clone();
    let cashier = sids["ca1"].clone();

    // 业务岗：财务报表端点403（菜单隐藏 + 端点硬拦）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/balance-sheet?to=202601", &clerk))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应看到资产负债表"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/ledger?from=202601&to=202601", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "订单专员不应看到账簿");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/daily", &clerk))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应看到资金日报"
    );

    // 业务岗：业务报表仍可用（Report 保留）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/procure/quota?supplier=S01&item=140301",
            &clerk,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单专员应能查配额业务报表");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/order/change-log?type=po&id=1", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单专员应能查订单变更");

    // 出纳（FinReport）：资金日报可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/daily", &cashier))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "出纳应能看资金日报");

    // 管理员：财务报表照常
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/balance-sheet?to=202601", &boss))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员应能看资产负债表");
}

/// 凭证可见性默认放开（多岗位协作）：非管理员默认见全账套；按 data_scope 可按账号收紧。
#[tokio::test]
async fn own_voucher_scope_default_open() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;

    // boss 先录一张
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &boss,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 91, "attachments": 0, "memo": "老板录的",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "注", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "boss 录凭证");

    // 邀请会计 acct1 并进账套
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss,
            serde_json::json!({ "username": "acct1", "display_name": "会计甲", "password": "Test12345" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({ "username": "acct1", "display_name": "会计甲", "password": "", "role": "accountant", "must_change_pwd": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请会计");
    let (st, sid1) = login(&state, "acct1", "Test12345").await;
    assert_eq!(st, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid1,
            serde_json::json!({ "old": "Test12345", "new": "Pass123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(select_book(&state, &sid1, "b1").await, StatusCode::OK);

    // 会计自己再录一张
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid1,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06", "word": "记",
                "no": 92, "attachments": 0, "memo": "会计录的",
                "entries": [
                    { "line": 1, "account_code": "660201", "summary": "费", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "会计录凭证");

    // 默认放开：会计能看到全账套2张（含老板录的）
    async fn count_vouchers(state: &Arc<WebState>, sid: &str) -> usize {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get("/api/vouchers?from=202601&to=202601", sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let empty = Vec::<serde_json::Value>::new();
        let arr = v["rows"]
            .as_array()
            .or_else(|| v.as_array())
            .unwrap_or(&empty);
        arr.len()
    }
    let n = count_vouchers(&state, &sid1).await;
    assert!(
        n >= 2,
        "默认放开：会计应能看到全账套凭证（含他人录入），实际 {n}"
    );

    // 管理员按账号收紧 → 只剩自己的1张
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/acct1",
            &boss,
            serde_json::json!({
                "data_scope": {
                    "depts": [], "account_from": "", "account_to": "",
                    "own_voucher_only": true, "own_doc_only": false
                }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "按账号收紧应成功");
    let n2 = count_vouchers(&state, &sid1).await;
    assert!(n2 == 1, "收紧后应只剩本人1张，实际 {n2}");
}

/// 打印/导出端点冒烟（均返回 200 且内容类型正确）。
#[tokio::test]
async fn web_print_and_pdf_endpoints() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "100" }
        ]),
    )
    .await;

    let prints = [
        "/api/reports/balance-sheet/print?to=202601",
        "/api/reports/income-statement/print?from=202601&to=202601",
        "/api/reports/cash-flow/print?from=202601&to=202601",
        "/api/reports/equity/print?from=202601&to=202601",
        "/api/reports/trial-balance/print?from=202601&to=202601",
        "/api/vouchers/print-form?period=202601",
        "/api/ledger/print-form?code=1001&from=202601&to=202601&type=detail",
        "/api/ledger/print-form?code=1001&from=202601&to=202601&type=general",
    ];
    for uri in prints {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "打印端点应 200：{uri}");
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("text/html"), "应为 HTML：{uri} → {ct}");
    }

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance/export?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance/pdf?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("application/pdf"), "应返回 PDF：{ct}");
}

// ---------------------------------------------------------------------------
// 读接口冒烟（不允许 5xx）/ 写接口冒烟 / 安全边界 / 年末结转
// ---------------------------------------------------------------------------

/// 全部只读接口冒烟：返回 2xx/4xx 均可，但不得 5xx（5xx = 引擎错误或 panic）。
#[tokio::test]
async fn web_read_endpoints_no_5xx() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let uris = [
        "/api/me",
        "/api/roles",
        "/api/dashboard",
        "/api/overview?period=202601",
        "/api/periods",
        "/api/accounts",
        "/api/vouchers/next-no?period=202601&word=记",
        "/api/vouchers?period=202601",
        "/api/invoices?period=202601",
        "/api/invoices/summary",
        "/api/ledger?code=1001&from=202601&to=202601",
        "/api/ledger/general?code=1001&from=202601&to=202601",
        "/api/ledger/journal?code=1001&from=202601&to=202601",
        "/api/reports/trial-balance?from=202601&to=202601",
        "/api/reports/multi-column?main=1001&cols=1002&from=202601&to=202601",
        "/api/reports/summary-table?from=202601&to=202601",
        "/api/reports/ratios?period=202601",
        "/api/reports/balance-sheet?to=202601",
        "/api/reports/income-statement?from=202601&to=202601",
        "/api/reports/cash-flow?from=202601&to=202601",
        "/api/reports/equity?from=202601&to=202601",
        "/api/reports/compare?report_key=balance_sheet&from=202601&to=202601",
        "/api/reports/daily?code=1001&from=202601&to=202601",
        "/api/reports/reconcile?period=202601",
        "/api/reports/aux-balance?kind=customer&from=202601&to=202601",
        "/api/reports/qty-balance?from=202601&to=202601",
        "/api/reports/notes?report_key=balance-sheet",
        "/api/inventory/aging",
        "/api/inventory/abc",
        "/api/inventory/serial?item=RM01",
        "/api/inventory/unit?item=RM01",
        "/api/inventory/warehouse-stock?item=RM01",
        "/api/inventory/transfer?period=202601",
        "/api/procure/reconcile?period=202601",
        "/api/procure/quota?supplier=S01&item=140301",
        "/api/procure/price?item=140301",
        "/api/procure/track?po_id=1",
        "/api/procure/stats?period=202601",
        "/api/procure/req?period=202601",
        "/api/sales/reconcile?period=202601",
        "/api/sales/credit?customer=C01&period=202601",
        "/api/sales/track?so_id=1",
        "/api/sales/stats?period=202601",
        "/api/sales/quote?period=202601",
        "/api/order/change-log?period=202601",
        "/api/budget/alerts?period=202601",
        "/api/budget/versions",
        "/api/budget/analysis?period=202601",
        "/api/routing/140301",
        "/api/prod",
        "/api/mrp/latest",
        "/api/approvals",
        "/api/approvals/todo",
        "/api/archives?period=202601",
        "/api/funds/bills?period=202601",
        "/api/funds/loans",
        "/api/funds/daily?period=202601",
        "/api/funds/forecast?period=202601",
        "/api/cost/configs",
        "/api/logs?limit=10",
        "/api/templates",
        "/api/templates/due?period=202601",
        "/api/payroll?period=202601",
        "/api/payroll/ytd?employee=E001&period=202601",
        "/api/claims?period=202601",
        "/api/claims/next-no?period=202601",
        "/api/assets?period=202601",
        "/api/bank?period=202601&account=100201",
        "/api/settle/open?account=112201&upto=202601",
        "/api/settle/records?account=112201",
        "/api/settle/aging?account=112201&upto=202601",
        "/api/custom-reports",
    ];
    let mut bad = Vec::new();
    for uri in uris {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        let code = resp.status().as_u16();
        if code >= 500 {
            bad.push(format!("{uri} → {code}"));
        }
    }
    assert!(bad.is_empty(), "只读接口出现 5xx：{bad:#?}");
}

/// 写接口冒烟：状态码 <500，并抽查若干返回 200。
#[tokio::test]
async fn web_write_endpoints_smoke() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    // 前置：140301 带价入库（组装成本平移要求子件有成本价，0 价将被拒绝）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "5", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "前置采购单");
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-05", "qty": "5", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "前置到货");
    let post = |uri: &'static str, body: serde_json::Value| {
        let state = state.clone();
        let sid = sid.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_post(uri, &sid, body))
                .await
                .unwrap();
            (resp.status(), body_string(resp).await)
        }
    };

    // 逐项写入并断言 200（发现引擎错误/500 即为问题）
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("/api/period", serde_json::json!({ "ymm": 202601 })),
        ("/api/accounts/fill-defaults", serde_json::json!({})),
        (
            "/api/inventory/unit",
            serde_json::json!({ "item": "RM01", "base_unit": "个", "alt_unit": "箱", "factor": "12" }),
        ),
        (
            "/api/inventory/serial",
            serde_json::json!({ "item": "RM01", "serials": ["SN001", "SN002"], "batch_no": "B1", "date": "2026-01-10" }),
        ),
        (
            "/api/inventory/serial/out",
            serde_json::json!({ "serials": ["SN001"], "date": "2026-01-11" }),
        ),
        (
            "/api/inventory/adjust",
            serde_json::json!({ "period": 202601, "date": "2026-01-10", "item": "RM01", "delta": "5", "memo": "t" }),
        ),
        (
            "/api/inventory/assemble",
            serde_json::json!({ "parent": "140501", "children": [["140301", "1"]], "date": "2026-01-10", "memo": "t" }),
        ),
        (
            "/api/inventory/disassemble",
            serde_json::json!({ "parent": "140501", "children": [["140301", "1"]], "date": "2026-01-10", "memo": "t" }),
        ),
        (
            "/api/procure/req",
            serde_json::json!({ "period": 202601, "date": "2026-01-10", "item_code": "140301", "item_name": "原料", "qty": "1", "requester": "admin", "memo": "" }),
        ),
        (
            "/api/sales/quote",
            serde_json::json!({ "id": 0, "period": 202601, "date": "2026-01-10", "customer_code": "C01", "customer_name": "客户", "item_code": "140501", "item_name": "成品", "qty": "1", "unit_price": "10", "status": "draft", "memo": "" }),
        ),
        (
            "/api/routing/140301",
            serde_json::json!([{ "seq": 1, "op_code": "OP1", "op_name": "车", "work_center": "WC1", "std_hours": "1", "rate": "10" }]),
        ),
        (
            "/api/mrp/run",
            serde_json::json!({ "demands": [{ "item_code": "140501", "qty": "10", "source": "手工" }] }),
        ),
        (
            "/api/budget/versions",
            serde_json::json!({ "key": "V1", "name": "版本1", "is_current": false, "memo": "" }),
        ),
        (
            "/api/approvals",
            serde_json::json!({ "biz_kind": "test", "biz_id": 1, "title": "t", "approvers": ["boss"] }),
        ),
        (
            "/api/reports/notes",
            serde_json::json!({ "report_key": "balance-sheet", "period": 202601, "content": "附注" }),
        ),
        (
            "/api/archives",
            serde_json::json!({ "period": 202601, "kind": "voucher", "title": "t", "payload": "{}" }),
        ),
        (
            "/api/funds/bills",
            serde_json::json!({ "kind": "receivable", "no": "B001", "period": 202601, "issue_date": "2026-01-05", "due_date": "2026-03-05", "counterpart": "客户", "bank": "工行", "amount": "1000", "memo": "" }),
        ),
        (
            "/api/funds/loans",
            serde_json::json!({ "kind": "borrow", "no": "L001", "bank": "工行", "principal": "10000", "rate_pct": "4.5", "start_date": "2026-01-01", "end_date": "2026-12-31", "memo": "" }),
        ),
        (
            "/api/cost/configs",
            serde_json::json!({ "item": "140501", "method": "fifo", "standard_cost": "0" }),
        ),
        (
            "/api/templates",
            serde_json::json!({ "id": 0, "name": "月度模板", "memo": "", "entries": [] }),
        ),
    ];
    let mut bad = Vec::new();
    for (uri, body) in cases {
        let (st, text) = post(uri, body).await;
        if st.as_u16() >= 500 {
            bad.push(format!("{uri} → {st} {text}"));
        }
    }
    assert!(bad.is_empty(), "写接口出现 5xx：{bad:#?}");

    // 抽查关键写接口确实成功（避免上面只证明"没炸"）
    for (uri, body) in [
        (
            "/api/inventory/unit",
            serde_json::json!({ "item": "RM02", "base_unit": "个", "alt_unit": "箱", "factor": "6" }),
        ),
        (
            "/api/funds/bills",
            serde_json::json!({ "kind": "payable", "no": "B002", "period": 202601, "issue_date": "2026-01-05", "due_date": "2026-03-05", "counterpart": "供应商", "bank": "工行", "amount": "500", "memo": "" }),
        ),
        (
            "/api/cost/configs",
            serde_json::json!({ "item": "140502", "method": "moving_average", "standard_cost": "0" }),
        ),
        // 单号可省略（服务端自动生成）：UI 就是这么发的，缺 no 不能 422
        (
            "/api/procure/req",
            serde_json::json!({ "period": 202601, "date": "2026-01-10", "item_code": "140301", "item_name": "原料", "qty": "3", "status": "draft", "requester": "admin", "memo": "" }),
        ),
        (
            "/api/sales/quote",
            serde_json::json!({ "id": 0, "period": 202601, "date": "2026-01-10", "customer_code": "C01", "customer_name": "客户", "item_code": "140501", "item_name": "成品", "qty": "1", "unit_price": "10", "status": "draft", "prepared_by": "", "memo": "" }),
        ),
    ] {
        let (st, text) = post(uri, body).await;
        assert_eq!(st, StatusCode::OK, "{uri} 应成功：{text}");
    }

    // 列表接口的空筛选值应视为「全部」：UI 下拉默认值就是空串，
    // 若被当成具体类型去过滤（kind=''）会永远查不到已保存的数据。
    for uri in [
        "/api/funds/bills?kind=",
        "/api/funds/loans?kind=",
        "/api/archives?period=202601&kind=",
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri} 应成功");
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert!(
            !v["rows"].as_array().unwrap().is_empty(),
            "{uri} 空筛选应返回全部数据：{v}"
        );
    }
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

/// 年末结转：本年利润 → 未分配利润。
#[tokio::test]
async fn web_year_end_carry() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 6401 100 / 贷 1001 100，并结转损益（生成 4103 贷方 100）
    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "6401", "summary": "成本", "debit": "100", "credit": "0" },
            { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "100" }
        ]),
    )
    .await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/carry-forward",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结转损益应成功");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 年末结转：4103 余额 -100 → 转入未分配利润
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/year-end",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "年末结转应成功：{}",
        body_string(resp).await
    );
    // 幂等性：第二次调用 4103 已清零，应 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/year-end",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "余额为 0 时年末结转应被拒"
    );
}

/// Web 导入预检：报告缺失科目 → 带映射导入成功。
#[tokio::test]
async fn web_import_analyze_and_map() {
    let (state, _bd, _dir) = test_state();
    let (_, sid) = login(&state, "boss", "Admin!2026").await;
    let _ = select_book(&state, &sid, "b1").await;

    // 预检：9999 不在科目表中，2001 存在
    let csv = "9999,借,100\n2001,贷,100\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/analyze",
            &sid,
            serde_json::json!({ "kind": "begin", "template": "generic", "text": csv }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入预检应成功");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let missing = v["missing"].as_array().unwrap();
    assert_eq!(missing.len(), 1, "只应报告缺失科目 9999：{v}");
    assert_eq!(missing[0]["code"], serde_json::json!("9999"));
    assert_eq!(missing[0]["count"], serde_json::json!(1));

    // 映射 9999→1001 后导入：两行都应写入
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "begin", "template": "generic", "text": csv,
                "mapping": { "9999": "1001" },
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "带映射的导入应成功");
    let s = body_string(resp).await;
    assert!(s.contains("\"ok\":2"), "应导入 2 行：{s}");
    assert!(s.contains("\"skipped\":0"), "不应跳过：{s}");
}

/// 红字冲销：未传日期时取期间末日（历史期间冲销不会因"今天"不在期间内而失败）。
#[tokio::test]
async fn web_reverse_defaults_to_period_last_day() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let id = post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "冲销测试", "debit": "100", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "冲销测试", "debit": "0", "credit": "100" }
        ]),
    )
    .await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/reverse"),
            &sid,
            serde_json::json!({ "period": 202601 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "缺省日期红冲应成功");
    let rid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_ne!(rid, id, "红冲应生成新凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{rid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["date"],
        serde_json::json!("2026-01-31"),
        "应取期间末日：{v}"
    );
}

// ---------------------------------------------------------------------------
// 回归测试：HTTP 层缺陷修复批次
// ---------------------------------------------------------------------------

/// 期初整批导入必须原子：第 2 行金额非法时，第 1 行也不能落地。
///
/// 修复前 `balances::upsert_begin` 是裸 `conn.execute`，每行各成一次隐式提交，
/// 第 k 行失败会让前 k-1 行留在库里，客户端既无法判断落地了哪些行也无法安全重试。
#[tokio::test]
async fn begin_import_is_all_or_nothing() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([
                { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                // 第 2 行金额非法 → 整批必须回滚（含第 1 行）
                { "account_code": "2001", "dir": "debit", "yb": "不是数字", "ad": "0", "ac": "0", "qty": null },
                { "account_code": "2202", "dir": "credit", "yb": "3000", "ad": "0", "ac": "0", "qty": null }
            ]),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "第 2 行非法应 400：{b}");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        rows.as_array().unwrap().is_empty(),
        "整批应回滚，一条都不能留下：{rows}"
    );

    // 修正后同一批（去掉坏行）应正常整批落地
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([
                { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                { "account_code": "2001", "dir": "credit", "yb": "3000", "ad": "0", "ac": "0", "qty": null }
            ]),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "合法批次应成功：{b}");
    assert!(b.contains("\"count\":2"), "应写入 2 条：{b}");
}

/// 订单页付款/收款：付款流水写不进去时，绝不能留下可被审核生成凭证的孤立草稿收付款单。
///
/// 修复前三步各自分别提交且顺序是「先建单 → 再建边 → 再建流水」，流水一失败，
/// 已提交的草稿单没有付款流水配对，而 `POST /api/funds/receipts/:id/audit`
/// 会照样给它生成付款凭证。修复后依赖写入在前、单据最后建。
#[tokio::test]
async fn payment_registration_leaves_no_orphan_receipt() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d12 = format!("{}-12", dash["current_period"].as_str().unwrap());
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d12.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单");
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Confirmed", "memo": "收款造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建销售订单");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 让付款流水写不进去（触发器阻断 INSERT），模拟第 3 步失败
    block_inserts(&state, "b1", "po_payment");
    block_inserts(&state, "b1", "so_payment");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/payment",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d12.clone(), "amount": "10", "memo": "付" }),
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_server_error(),
        "付款流水写失败应报错（当前 {}）",
        resp.status()
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/payment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "amount": "10", "memo": "收" }),
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_server_error(),
        "收款流水写失败应报错（当前 {}）",
        resp.status()
    );

    // 关键断言：一条草稿收付款单都不该留下（它可被审核生成付款凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/receipts", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "收付款单列表应可用：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        r["rows"].as_array().unwrap().len(),
        0,
        "付款/收款流水失败时不得留下孤立草稿收付款单：{b}"
    );
}

/// 已提交的业务写入不能因为随后的审计日志失败而变成 500（否则客户端重试会重复入账）。
///
/// 手法：把 `audit_log` 表删掉，让所有 `db.log` 必然失败。修复前这些接口一律 500，
/// 客户端看到失败重试就会再录一张付款单/订单/凭证。
#[tokio::test]
async fn audit_log_failure_does_not_fail_committed_mutations() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let d12 = format!("{cur_label}-12");
    let d15 = format!("{cur_label}-15");
    let d20 = format!("{cur_label}-20");

    // 造数：采购入库 → 销售出库（给成本结转留销售出库）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d12.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d12.clone(), "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购入库");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Confirmed", "memo": "结转造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "4", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "销售出库");

    // 让所有审计日志写入必然失败
    block_inserts(&state, "b1", "audit_log");

    // ① 采购付款：业务写入已提交，日志失败不得改判 500
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/payment",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d12.clone(), "amount": "10", "memo": "付" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让付款变 500：{b}");
    let pay: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(pay["id"].as_i64().unwrap() > 0, "付款流水应已写入：{b}");

    // ② 销售收款
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/payment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "amount": "10", "memo": "收" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让收款变 500：{b}");

    // ③ 保存采购订单
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "日志坏了也要成",
                "lines": [{ "item_code": "140301", "qty_ordered": "2", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让保存订单变 500：{b}");

    // ④ 保存凭证（重试会多录一张，是最贵的重复写）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 0,
                "attachments": 0, "memo": "日志坏了也要成",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "s", "debit": "1", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "s", "debit": "0", "credit": "1" }
                ]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让保存凭证变 500：{b}");

    // ⑤ 销售成本结转：凭证已由 findb 提交，日志失败不得改判 500
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/sales-cost",
            &sid,
            serde_json::json!({ "period": cur_ymm, "method": "moving_average", "date": d20 }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让成本结转变 500：{b}");
    let cost: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(
        cost["voucher_id"].as_i64().unwrap_or(0) > 0,
        "结转凭证应已生成：{b}"
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

/// 完工入库数量不得超过订单未完工数量（防超完工虚增库存与摊薄单位成本）。
#[tokio::test]
async fn prod_complete_rejects_over_completion() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let d15 = format!("{cur_label}-15");
    let d20 = format!("{cur_label}-20");

    // 完工结转要求订单已归集到成本（无成本不许完工），先备齐标准价 + BOM 才能走到完工
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "配标准价");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "配 BOM");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "100", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "下达生产订单：{b}");
    let pid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");
    // 领料 2×100 归集材料成本（BOM 2 件 × 标准价 10）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/issue"),
            &sid,
            serde_json::json!({ "date": d15.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "领料归集成本：{b}");

    // 计划 100 件，未完工 100 件 → 报 999999 必须 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/complete"),
            &sid,
            serde_json::json!({ "qty": "999999", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "超量完工应 400：{b}");

    // completed_qty 必须原封不动
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let o = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(pid))
        .expect("列表应含该订单");
    assert_eq!(
        money_num(o["completed_qty"].as_str().unwrap()),
        0.0,
        "completed_qty 不应被推进：{o}"
    );
    assert_eq!(o["status"], "InProgress", "订单应仍在生产中：{o}");

    // 计划量之内仍然放行
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/complete"),
            &sid,
            serde_json::json!({ "qty": "100", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "足量完工应放行：{b}");
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

/// 订单状态机（由 findb 的 scm 流转表把关，HTTP 层透传 400）：草稿不得一步跳到已完成，
/// 草稿 → 已确认仍可用；已作废是终态。销售/采购两侧同口径。
#[tokio::test]
async fn order_transition_rejects_illegal_jump() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d13 = format!("{}-13", dash["current_period"].as_str().unwrap());

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d13.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建销售订单");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d13.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单");
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 草稿 → 已完成：两侧都必须拒绝
    for (uri, what) in [
        (format!("/api/sales/so/{so_id}/transition"), "销售订单"),
        (format!("/api/procure/po/{po_id}/transition"), "采购订单"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &uri,
                &sid,
                serde_json::json!({ "status": "Completed" }),
            ))
            .await
            .unwrap();
        let st = resp.status();
        let b = body_string(resp).await;
        assert_eq!(
            st,
            StatusCode::BAD_REQUEST,
            "{what} 草稿不得直接跳已完成：{b}"
        );
    }
    // 草稿 → 部分发货 同样非法
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "PartialShip" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "草稿不得直接跳部分发货：{b}");

    // 状态没被改掉
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/sales/so?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let o = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(so_id))
        .expect("列表应含该订单");
    assert_eq!(o["status"], "Draft", "非法流转不应改状态：{o}");

    // 草稿 → 已确认：仍可用
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "草稿 → 已确认应放行：{b}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "草稿 → 已确认应放行：{b}");

    // 已确认 → 已完成：findb 的状态机同样拒绝（发货进度只能由发货流水推进）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Completed" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "已确认不得手工跳已完成：{b}");

    // 作废是出口
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Cancelled" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "已确认 → 已作废应放行：{b}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "已作废是终态，不许复活：{b}");
}

/// 凭证：修改时凭证字不可变（保住号段一致），自选号码撞号回 400 而不是 500。
#[tokio::test]
async fn voucher_word_immutable_and_duplicate_no_is_400() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let mk = |word: &str, no: i32, memo: &str| {
        serde_json::json!({
            "id": 0, "period": 202601, "date": "2026-01-15", "word": word, "no": no,
            "attachments": 0, "memo": memo,
            "entries": [
                { "line": 1, "account_code": "1001", "summary": "s", "debit": "10", "credit": "0" },
                { "line": 2, "account_code": "2001", "summary": "s", "debit": "0", "credit": "10" }
            ]
        })
    };
    let entries = |memo: &str| {
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": memo, "debit": "10", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": memo, "debit": "0", "credit": "10" }
        ])
    };

    // 自动取号建一张 记-0001
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &sid, mk("记", 0, "原始")))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建凭证：{b}");
    let vid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 改凭证字 记 → 银：必须拒绝（否则 银-0001 会占掉银字号的号）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": vid, "period": 202601, "date": "2026-01-15", "word": "银", "no": 0,
                "attachments": 0, "memo": "改字", "entries": entries("改字")
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "凭证字不可改：{b}");

    // 凭证字不变、只改摘要：放行且号不变
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": vid, "period": 202601, "date": "2026-01-15", "word": "记", "no": 0,
                "attachments": 0, "memo": "只改摘要", "entries": entries("只改摘要")
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "同字修改应放行：{b}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["word"], "记", "凭证字应保持不变：{v}");
    assert_eq!(v["no"], 1, "号码应保持不变（号段不漂移）：{v}");
    assert_eq!(v["memo"], "只改摘要", "其它字段应已更新：{v}");

    // 自选号码撞上 记-0001：必须 400，不能是 500
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &sid, mk("记", 1, "撞号")))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "撞号应回 400 而不是 500：{b}");
    assert!(!b.contains("UNIQUE"), "不该把 SQLite 约束错误透给用户：{b}");

    // 换一个字就不撞，仍可自选
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &sid, mk("银", 1, "银字 1 号")))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "银字 1 号未被占用：{b}");
}

/// 往来期初合计：按金额格式（2 位小数）输出，且非 ar/ap 的 kind 不并进应付合计。
#[tokio::test]
async fn arap_opening_totals_money_and_no_misbucket() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let db = state.db_for("b1").expect("打开 b1");
    let mk = |kind: &str, doc: &str, party: &str, amt: &str| findb::settle::ArapOpening {
        id: 0,
        kind: kind.to_string(),
        party_code: party.to_string(),
        party_name: party.to_string(),
        doc_no: doc.to_string(),
        doc_date: "2025-12-31".to_string(),
        amount: fincore::Money::parse(amt).unwrap(),
        memo: String::new(),
        created_by: "boss".to_string(),
    };
    findb::settle::arap_opening_insert(&db, &mk("ar", "XSQ-1", "Q01", "5000"), "boss").unwrap();
    findb::settle::arap_opening_insert(&db, &mk("ap", "CGQ-1", "S01", "3000.5"), "boss").unwrap();
    // 脏数据：kind 写成大写 AR（导入笔误），绝不能被并进 total_ap
    findb::settle::arap_opening_insert(&db, &mk("AR", "XSQ-2", "Q02", "7000"), "boss").unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/arap-opening", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "往来期初列表：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 3, "三行都应返回：{b}");
    // 金额口径：2 位小数 + 千分位（fmt_qty 会印成 "5,000"）
    assert_eq!(r["total_ar"], "5,000.00", "应收合计按金额格式：{b}");
    assert_eq!(r["total_ap"], "3,000.50", "应付合计按金额格式：{b}");
    // 大写 AR 那 7000 既不进 ar 也不进 ap
    assert_eq!(r["unknown_kind"], 1, "未知 kind 应单独计数：{b}");
    assert_eq!(
        money_num(r["total_ar"].as_str().unwrap()) + money_num(r["total_ap"].as_str().unwrap()),
        8000.5,
        "合计不应把 7000 混进来：{b}"
    );
}

/// 资产卡片漏传残值率时按 0（无残值）落库，而不是默默按 5% 记（会低估折旧）。
#[tokio::test]
async fn asset_omitted_residual_rate_defaults_zero() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();

    // 完全不带 residual_rate 字段
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets",
            &sid,
            serde_json::json!({
                "id": 0, "code": "NR0001", "name": "漏传残值率", "category": "电子设备",
                "spec": "", "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
                "expense_account": "660201", "original_value": "12000",
                "life_months": 36, "method": "straight", "start_period": cur_ymm, "memo": ""
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建资产：{b}");
    let aid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 显式传 0 仍合法（0 是合法残值率）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets",
            &sid,
            serde_json::json!({
                "id": 0, "code": "NR0002", "name": "显式零残值", "category": "电子设备",
                "spec": "", "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
                "expense_account": "660201", "original_value": "12000", "residual_rate": "0",
                "life_months": 36, "method": "straight", "start_period": cur_ymm, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "显式 0 残值率应合法");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/assets", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "资产列表：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    let card = r["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_i64() == Some(aid))
        .expect("列表应含新资产");
    assert_eq!(
        money_num(card["residual_rate"].as_str().unwrap()),
        0.0,
        "漏传残值率应落 0 而不是 5：{card}"
    );
}

/// 删除融资记录必须留审计痕迹（原先是资金模块唯一不写日志的删除口）。
#[tokio::test]
async fn delete_loan_writes_audit_log() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/loans",
            &sid,
            serde_json::json!({ "kind": "borrow", "no": "LLOG1", "bank": "工行",
                "principal": "10000", "rate_pct": "4.5", "start_date": "2026-01-01",
                "end_date": "2026-12-31", "memo": "待删融资" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建融资：{b}");
    let lid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "删除融资：{b}");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/logs?limit=20", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "操作日志：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(
        r.as_array().unwrap().iter().any(|l| l["module"] == "资金"
            && l["action"] == "删除融资"
            && l["detail"] == format!("#{lid}")),
        "删除融资应留痕（资金/删除融资/#{lid}）：{b}"
    );
}

/// 凭证更正链（Cancel -> Amend）的端到端行为。
///
/// 回归背景：finbook 有反记账 + 取消审核，所以错凭证**能改** —— 但改完的两张凭证
/// 之间没有任何链接，事后审计只能靠时间和金额去猜哪张在冲哪张。
/// 「不能改」反而更安全：它逼人走更正流程；「能改」让人直接改，而直接改完的两张
/// 凭证互相不认识，才是真正查不出来的状态。
#[tokio::test]
async fn voucher_amend_links_old_and_new() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 建一张已记账的凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 91,
                "attachments": 0, "memo": "更正链测试", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "更正测试", "debit": "1000", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "更正测试", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账：{}", resp.status());

    // ① 原因必填 —— 没有原因的更正等于没更正
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/amend"),
            &sid,
            serde_json::json!({ "reason": "  " }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "空原因应被拒");
    // 被拒时原凭证必须还是已记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["status"], "posted", "校验失败不该动原凭证：{r}");

    // ② 正常更正
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/amend"),
            &sid,
            serde_json::json!({ "reason": "金额录错，应为 1200" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "更正失败");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let new_id = r["new_id"].as_i64().expect("应返回新凭证 id");
    assert!(new_id > 0 && new_id != vid);

    // ③ 原凭证：**保持已记账** + 标注「被谁红冲 + 谁更正重做」
    //    （准则形态：红字冲销 + 蓝字重做。作废原凭证会让总账看不出曾经记错过。）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["status"], "posted",
        "原凭证必须保持已记账，由红冲凭证抵消：{r}"
    );
    let red_id = r["amend"]["amended_by"].as_i64().expect("应标注被谁红冲");
    assert!(red_id > 0 && red_id != vid);
    assert_eq!(
        r["amend"]["amended_by_new"], new_id,
        "同批的更正凭证要单独标出：只列一张等于账上有一张凭证没交代来路：{r}"
    );
    assert_eq!(r["amend"]["reason"], "金额录错，应为 1200");
    assert!(
        r["amend"]["amended_by_no"]
            .as_str()
            .unwrap_or("")
            .starts_with("记-"),
        "要可读单号：{r}"
    );

    // ④ 红冲凭证：草稿 + 借贷互换 + 摘要带冲销标记
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{red_id}"), &sid))
        .await
        .unwrap();
    let red: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(red["status"], "draft", "红冲凭证应是草稿等会计审核：{red}");
    assert_eq!(
        red["amend"]["amends_id"], vid,
        "红冲凭证也要标注来自哪张：{red}"
    );
    let red_entries = red["entries"].as_array().unwrap();
    assert_eq!(red_entries.len(), 2);
    // 借贷互换：原凭证借 1001，红冲就贷 1001
    // （金额列以 TEXT 存、序列化带两位小数，所以用正则而不是精确等值）
    assert!(
        red_entries[0]["credit"]
            .as_str()
            .unwrap_or("")
            .starts_with("1000"),
        "红冲第一行应为贷方 1000（原凭证是借方）：{red}"
    );
    assert!(
        red_entries[1]["debit"]
            .as_str()
            .unwrap_or("")
            .starts_with("1000"),
        "红冲第二行应为借方 1000：{red}"
    );
    assert!(
        red_entries[0]["summary"]
            .as_str()
            .unwrap_or("")
            .contains("冲销"),
        "红冲凭证摘要要带冲销标记，否则账上看着像重复录入：{red}"
    );

    // ⑤ 更正凭证：草稿 + 标注「更正自谁」+ 分录原样复制
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{new_id}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["status"], "draft", "更正凭证应是草稿等会计审核：{r}");
    assert_eq!(r["amend"]["amends_id"], vid, "更正凭证要标注更正自谁：{r}");
    assert!(
        r["entries"][0]["debit"]
            .as_str()
            .unwrap_or("")
            .starts_with("1000"),
        "分录要原样复制：{r}"
    );

    // ⑥ 三张都记账后，任何一张都不能被单独反记账（撤掉一张净额就错）
    //    红冲与更正凭证是**新凭证**（草稿态），账套开着审核环节时须先审核。
    for id in [red_id, new_id] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{id}/audit"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "红冲/更正凭证应能审核");
    }
    for (id, what) in [(vid, "原凭证"), (red_id, "红冲凭证"), (new_id, "更正凭证")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{id}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{what} 应能记账");
    }
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{red_id}/unpost"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "红冲凭证不能被单独反记账（三张必须成套存在）"
    );

    // ⑦ 重复更正被拒，且错误信息要指向「已被更正过」而不是笼统的状态
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/amend"),
            &sid,
            serde_json::json!({ "reason": "再改一次" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复更正应被拒");
    let msg = body_string(resp).await;
    assert!(
        msg.contains("更正过"),
        "错误信息应说「已被更正过」，实际：{msg}"
    );

    // ⑥ 新凭证记账后，总账里 1001 只有一笔 1000（作废那张不进总账）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{new_id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "更正凭证记账");
}

/// 探针必须**能抓到故障** —— 否则它是虚假的安全感。
///
/// 启动时必须把所有账套迁到当前版本，并让 readiness 如实汇报。
///
/// 回归背景（生产真实发生）：迁移原本是**懒执行**的 —— 账套第一次被打开时
/// 才跑 `schema::init`。于是升级后：部署完成、readiness 全绿，而用户第一次
/// 点「收货」才报 `no such column: item_code`。故障点与原因隔了几小时，
/// 部署的人不会联想到 schema 版本。
///
/// 现在改成启动时后台批量迁移，并且 readiness 汇报三件事：
/// 迁完了没、有没有账套迁不动、还有没有账套根本没迁过。
#[tokio::test]
async fn startup_migrates_every_book_and_readiness_reports_it() {
    let (state, bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造一个「已注册但从未打开」的账套。
    //
    // 不能直接用刚建好的 b1：创建账套时已经打开过它，迁移早跑完了，pending 当然是空的。
    // `unregister` + `register` 正是**恢复账套**走的那条路（覆盖文件后重新注册），
    // 所以这里构造的就是恢复后「文件是旧版本、还没迁」的真实状态。
    let fbk = std::fs::read_dir(&bd)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "fbk").unwrap_or(false))
        .expect("应有一个账套文件");
    let key = fbk.file_stem().unwrap().to_string_lossy().to_string();
    state.books.unregister(&key);
    state.books.register(&fbk, 16);
    assert_eq!(
        state.books.unmigrated_books(),
        vec![key.clone()],
        "前置条件：重新注册后该账套应处于未迁移状态"
    );

    // ① 未迁移的账套必须被 readiness 点名（而不是报绿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "还有账套没迁过，readiness 不能报就绪"
    );
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let mig = &r["checks"]["migration"];
    assert_eq!(mig["ok"], false, "migration.ok 应为 false：{mig}");
    let pending = mig["pending"].as_array().expect("应报出 pending 列表");
    assert!(
        !pending.is_empty(),
        "必须指名道姓报出哪些账套没迁过，否则运维无从下手：{mig}"
    );
    // 顺带确认报的是当前 schema 版本，便于一眼看出「落后几个版本」
    assert!(
        mig["schema_version"].is_i64(),
        "应报出 schema_version：{mig}"
    );

    // ② 跑批量迁移后应全部成功、pending 清空、readiness 转绿
    let st = state.books.migrate_all();
    assert!(st.done(), "migrate_all 返回时不该还在进行中");
    assert!(
        st.failed.is_empty(),
        "批量迁移不该有失败（全新账套都是当前版本）：{:?}",
        st.failed
    );
    assert_eq!(st.ok, st.total, "所有账套都应迁移成功");
    assert!(st.total > 0, "夹具里应至少有一个账套，实际 {}", st.total);
    assert!(
        state.books.unmigrated_books().is_empty(),
        "迁移后不应还有未迁移账套：{:?}",
        state.books.unmigrated_books()
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "迁移完成后 readiness 应 200");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["checks"]["migration"]["ok"], true);
    assert_eq!(
        r["checks"]["migration"]["pending"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(r["ok"], true, "全部检查应通过：{r}");

    // ③ 幂等：再跑一次不报错、结果不变
    let again = state.books.migrate_all();
    assert!(again.failed.is_empty() && again.ok == again.total);

    // ④ liveness 全程 200 —— 迁移是后台做的，绝不能拖垮 liveness
    //    （liveness 一旦碰重活，编排器会判 unhealthy 并重启，放大抖动）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 账套迁不动时，readiness 必须报 503 并指名道姓，而不是继续报绿。
///
/// 与上一条互补：那条测「还没迁」，这条测「迁了但失败」。
/// 财务系统里一个账套打不开不是「可以先用着」，必须让人看见。
#[tokio::test]
async fn readiness_reports_503_when_a_book_cannot_be_migrated() {
    let (state, bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 把账套文件写坏，让 schema::init 失败
    let fbk = std::fs::read_dir(&bd)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "fbk").unwrap_or(false))
        .expect("应有一个账套文件");
    let key = fbk.file_stem().unwrap().to_string_lossy().to_string();
    std::fs::write(&fbk, b"not a sqlite database at all").unwrap();
    // 换一个干净连接重开（写坏前的连接可能还有缓存）
    state.books.unregister(&key);
    state.books.register(&fbk, 16);

    let st = state.books.migrate_all();
    assert!(st.done());
    assert!(
        st.has_failures(),
        "坏掉的账套必须被记为迁移失败，而不是悄悄跳过：{st:?}"
    );
    assert_eq!(st.ok, 0, "唯一那个账套应该失败：{st:?}");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "有账套迁不动，readiness 必须是 503"
    );
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let failed = r["checks"]["migration"]["failed"]
        .as_array()
        .expect("应报 failed 列表");
    assert_eq!(failed.len(), 1, "应指名报出那个账套：{r}");
    assert_eq!(failed[0]["book"].as_str().unwrap(), key.as_str());
    assert!(
        !failed[0]["error"].as_str().unwrap_or("").is_empty(),
        "必须带上错误原因，否则运维无从判断：{r}"
    );
}

/// 回归背景：原 `/api/health` 返回常量 `ok`（不碰任何依赖），于是磁盘写满、
/// 账套文件损坏、账号库打不开这些「系统已不可用」的状态，它一律报健康。
/// 监控拿着这样的答案会一直显示「一切正常」，人就不去查别的地方了。
///
/// 这里用「账套文件被截断成垃圾」模拟真实损坏：文件存在、能打开，
/// 但查询时才报错 —— 正是 `Connection::open` 惰性打开最容易漏过的那类故障。
#[tokio::test]
async fn readiness_probe_detects_a_corrupt_book_file() {
    let (state, bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // ① 正常状态：liveness 与 readiness 都 200
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "健康时 readiness 应 200");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], true, "健康时 ok 应为 true：{r}");
    assert_eq!(r["checks"]["realm"]["ok"], true);
    assert_eq!(r["checks"]["books_dir_writable"]["ok"], true);

    // ② 把账套文件写坏（截断成非 SQLite 内容）
    let fbk = std::fs::read_dir(&bd)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().map(|x| x == "fbk").unwrap_or(false))
        .expect("应有一个账套文件");
    std::fs::write(&fbk, b"this is definitely not a sqlite database").unwrap();

    // ③ liveness 仍 200（进程确实还活着）—— 这正是两者的分工
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "liveness 不该碰依赖：账套坏了不代表进程死了"
    );

    // ④ readiness 必须抓到：503，且明说是哪个账套坏了
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "账套损坏时 readiness 必须报 503，否则负载均衡会继续往坏实例打流量"
    );
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], false, "响应体也要明说不可用：{r}");
    assert_eq!(
        r["checks"]["book_sample"]["ok"], false,
        "要指出是抽样账套查询失败：{r}"
    );
    let detail = r["checks"]["book_sample"].to_string();
    assert!(
        detail.contains("error"),
        "必须给出错误原因，否则运维看到「不健康」却不知道修什么：{detail}"
    );
}

/// 探针不该在数据目录里留下垃圾，也不该把自己的临时文件当成账套。
#[tokio::test]
async fn readiness_probe_leaves_no_files_behind() {
    let (state, bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/health/ready", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let names: Vec<String> = std::fs::read_dir(&bd)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "探针跑完目录里应只剩账套本身，实际：{names:?}"
    );
    assert!(
        names[0].ends_with(".fbk"),
        "探针的临时文件不能以 .fbk 结尾，否则下次启动会被当成新账套注册：{names:?}"
    );
}

/// 发票列表必须限量返回（原先 limit=None，一次把全库发票读进内存并整包序列化）。
#[tokio::test]
async fn invoice_list_is_bounded() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let db = state.db_for("b1").expect("打开 b1");
    let mut sql = String::new();
    for i in 1..=501 {
        sql.push_str(&format!(
            "INSERT INTO invoice(kind,code,number,date,buyer,seller,amount_tax,amount,tax,tax_rate,\
             status,memo,attach_id,created_by,created_at,updated_at) VALUES(\
             'out','INV','{i:04}','2026-01-15','买方{i}','卖方{i}','113.00','100.00','13.00','13',\
             'pending','',0,'boss',datetime('now'),datetime('now'));\n"
        ));
    }
    let tx = db.write_tx().expect("开事务");
    tx.execute_batch(&sql).expect("批量造 501 张发票");
    tx.commit().expect("提交造数");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "发票列表：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(r["limit"], 500, "应声明上限：{b}");
    assert_eq!(
        r["rows"].as_array().unwrap().len(),
        500,
        "最多回 500 行：{b}"
    );
    assert_eq!(r["truncated"], true, "超限时须回报截断：{b}");

    // 收窄条件后不再截断
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?keyword=0001", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "带关键字的发票列表：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(
        r["rows"].as_array().unwrap().len() < 500,
        "收窄后不该再截断：{b}"
    );
    assert_eq!(r["truncated"], false, "收窄后不该报截断：{b}");
}

/// 造一个「往该表 INSERT 必失败」的 BEFORE INSERT 触发器。
///
/// 不能用 `DROP TABLE`：删表会让 `findb::schema::init` 的 `tables_complete` 快路径失效，
/// 下一次 `Db::open` 就重跑 DDL 把表建回来，注入的故障自己消失了。触发器不在
/// `tables_complete` 的检查范围内，能稳定撑住整个用例。
fn block_inserts(state: &Arc<WebState>, book: &str, table: &str) {
    let db = state.db_for(book).expect("打开账套");
    db.conn()
        .execute(
            &format!(
                "CREATE TRIGGER t_block_{table} BEFORE INSERT ON {table}
                 BEGIN SELECT RAISE(ABORT, '测试注入：该表写入被阻断'); END"
            ),
            [],
        )
        .unwrap_or_else(|e| panic!("装 {table} 阻断触发器失败: {e}"));
}

// ===========================================================================
// 回归测试：2026-09-29 全岗位流程走查发现的 8 个缺陷
//
// 这一批的共同点：**每个缺陷都是"操作成功但结果不对"**，界面上看不出异常，
// 只有拿数字对账才发现。所以每条测试都断言具体数值/具体文案，不断言 HTTP 200。
// ===========================================================================

/// ③ 期初写不存在的科目必须被拒 —— 修前钱从所有报表里静默消失。
///
/// 实测：写 `999999` 返回 `200 {"count":1,"ok":true}`，随后
///   · 试算平衡报表**没有这一行**，总额照样报「平衡」
///   · 资产负债表查不到这 1,000,000
/// 凭证录入那条路上有「非末级科目不能记账」「核算数量科目必须填数量」「借贷不平衡」
/// 一整套校验，唯独期初这条路原来是裸的。会计把科目编码打错一位就踩到。
#[tokio::test]
async fn begin_rejects_nonexistent_account() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 一批里混一个不存在的科目：整批必须被拒，且真实那行也不能落地
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!({
                "rows": [
                    { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                    { "account_code": "999999", "dir": "debit", "yb": "1000000", "ad": "0", "ac": "0", "qty": null }
                ],
                "delete_ids": []
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "不存在科目应 400：{b}");
    assert!(
        b.contains("999999"),
        "报错必须点名是哪个科目，否则会计不知道改哪一位：{b}"
    );

    // 拒绝时不能留半截数据
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        rows.as_array().unwrap().is_empty(),
        "校验失败必须整批不落地：{rows}"
    );

    // 对照：真实科目照常能存（别把校验写太死）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([{ "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null }]),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "真实科目应能存：{b}");
}

/// ④ 界面上的「移除」必须真删库里的行。
///
/// 实测：移除 140501 → 保存 → DB 纹丝不动 → 试算平衡从「1,544,100 vs 1,544,100 平衡」
/// 变成「1,544,100 vs 2,544,099 不平衡」，而界面上那行已经不见了。会计改个错字
/// 重新保存，期初金额就翻倍。
///
/// 根因：后端 `upsert_begin_on` 是 `ON CONFLICT DO UPDATE`，**没有任何删除语义**；
/// 前端把行从草稿删掉后只提交剩下的行，于是被删的那行永远留在库里。
#[tokio::test]
async fn begin_delete_ids_actually_remove_rows() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 先存 3 条
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!([
                { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                { "account_code": "2001", "dir": "credit", "yb": "3000", "ad": "0", "ac": "0", "qty": null },
                { "account_code": "2202", "dir": "credit", "yb": "2000", "ad": "0", "ac": "0", "qty": null }
            ]),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "先存 3 条");

    // 读回 id
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let id_of = |code: &str| -> i64 {
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["account_code"] == code)
            .unwrap_or_else(|| panic!("应有 {code}：{rows}"))["id"]
            .as_i64()
            .unwrap()
    };
    let (id1001, id2001, _) = (id_of("1001"), id_of("2001"), id_of("2202"));

    // 界面上移除 2001（3,000 贷方），另两行原样重发
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!({
                "rows": [
                    { "account_code": "1001", "dir": "debit", "yb": "5000", "ad": "0", "ac": "0", "qty": null },
                    { "account_code": "2202", "dir": "credit", "yb": "2000", "ad": "0", "ac": "0", "qty": null }
                ],
                "delete_ids": [id2001]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "带删除的保存应成功：{b}");
    assert!(b.contains("\"deleted\":1"), "响应应报告删了几条：{b}");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let codes: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["account_code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes.len(),
        2,
        "移除后应只剩 2 行，实际 {codes:?} —— 被移除的行还留在库里"
    );
    assert!(!codes.contains(&"2001"), "2001 必须消失，实际 {codes:?}");

    // 只 upsert 一条、**不带** delete_ids：另一条不能被误删
    // （这条钉住"按 id 精确删"而不是"整表替换"——后者会删掉别人维护的行）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/begin",
            &sid,
            serde_json::json!({
                "rows": [
                    { "account_code": "1001", "dir": "debit", "yb": "6000", "ad": "0", "ac": "0", "qty": null }
                ],
                "delete_ids": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "只改一行应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let arr = rows.as_array().unwrap();
    assert_eq!(arr.len(), 2, "不带 delete_ids 时未提及的行必须保留：{rows}");
    // Money 序列化成字符串（"6000.00"），不是 JSON number —— 前端 renderBegin 里
    // 也是按 String(r.year_begin) 处理的
    let yb1001: f64 = arr
        .iter()
        .find(|r| r["account_code"] == "1001")
        .expect("1001 行应在")["year_begin"]
        .as_str()
        .unwrap_or_else(|| panic!("year_begin 应是字符串金额：{rows}"))
        .parse()
        .unwrap();
    assert_eq!(
        yb1001, 6000.0,
        "重发的行应是覆盖而非新增（原来翻倍就是这么来的）：{rows}"
    );
    let _ = id1001;
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

/// ⑧ 重复点「结转损益」必须说清是"已经结过了"，而不是"损益发生额为零"。
///
/// 实测：第一次结转生成 658,000 的凭证，第二次点，提示是
///   「导入数据有误：本期没有需要结转的损益类科目（损益发生额均为零）」
/// 而账上明明有 658,000 的损益额。两处都误导：
///   · "导入数据有误" 是加在 `From<FinError>` 上的万能前缀，任何引擎错误都会被说成
///     导入错误（用户根本没在导入）
///   · "损益发生额均为零" 把「已结转过」说成「本期没做业务」
#[tokio::test]
async fn carry_forward_twice_explains_it_was_already_carried() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 本期发生一笔费用：借 6401 1000 / 贷 1001 1000
    post_voucher(&state, &sid, 1, serde_json::json!([
        { "line": 1, "account_code": "6401", "summary": "费用", "debit": "1000", "credit": "0" },
        { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "1000" }
    ]))
    .await;

    // 第一次结转：成功，生成 1,000 的结转凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/carry-forward",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "第一次结转应成功：{b}");

    // 第二次：应被拒（账结法幂等），但提示必须说清原因
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/carry-forward",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "重复结转应被拒：{b}");
    assert!(
        b.contains("已结转过"),
        "提示必须说明「已结转过」，否则用户以为账做错了：{b}"
    );
    assert!(
        !b.contains("导入数据有误"),
        "结转不是导入操作，不该带「导入数据有误」前缀：{b}"
    );
    assert!(
        !b.contains("损益发生额均为零"),
        "本期有 1,000 费用，说「损益发生额均为零」是错的：{b}"
    );
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

// ===========================================================================
// 回归测试：收付款单的「审核」判据
//
// 起因是补出纳日常 E2E 时实测撞出来的死路：出纳录单、会计想审，但
// `Role::Accountant` 按三权分离刻意不带 `VoucherAudit` —— 而
// `audit_receipt` 的 `Gate::NoFlow` 分支只认 `VoucherAudit`，
// 于是「只有会计+出纳、没有财务主管」的小微企业**永远把收付款单变不成凭证**。
//
// 特别注意：这条判据**与 `enable_audit` 无关**。所以在建账向导里选「不启用审核环节」
// 也救不了它 —— 向导的承诺在收付款这条路径上是破的。
//
// 修法：`require_fund_doc_gate` = VoucherAudit **或** VoucherPost。
// ===========================================================================

/// 建一个「出纳 + 会计」都在套里的账套，返回两个 sid 的取用方式。
/// 账号已存在时复用（`platform` 层一人一机，同一用户名不能登两次）。
async fn two_person(page: &std::sync::Arc<finweb::state::WebState>, tag: &str) -> (String, String) {
    let boss = boss_in_b1(page).await;
    for (u, pw, role) in [
        (format!("csh_{tag}"), "Cash@2026d", "cashier"),
        (format!("acc_{tag}"), "Acc@2026dd", "accountant"),
    ] {
        let r = handlers::router(page.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss,
                serde_json::json!({ "username": u, "display_name": u, "password": pw }),
            ))
            .await
            .unwrap();
        assert!(r.status().is_success(), "开通平台账号 {u}");
        let r = handlers::router(page.clone())
            .oneshot(authed_post(
                "/api/users",
                &boss,
                serde_json::json!({
                    "username": u, "display_name": u, "password": "",
                    "role": role, "must_change_pwd": false
                }),
            ))
            .await
            .unwrap();
        let st = r.status();
        let b = body_string(r).await;
        assert!(st.is_success(), "把 {u} 拉进账套：{b}");
    }
    // 财务主管（唯一有 VoucherAudit 的常规岗位）
    let r = handlers::router(page.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss,
            serde_json::json!({ "username": format!("sup_{tag}"), "display_name": "主管", "password": "Sup@2026dd" }),
        ))
        .await
        .unwrap();
    assert!(r.status().is_success(), "开通主管平台账号");
    let r = handlers::router(page.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({
                "username": format!("sup_{tag}"), "display_name": "主管", "password": "",
                "role": "supervisor", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert!(r.status().is_success(), "把主管拉进账套");

    // login 只到平台层，账套内端点还要先选账套（否则一律 401「请先选择账套」）
    let (_, csh) = login(page, &format!("csh_{tag}"), "Cash@2026d").await;
    let (_, acc) = login(page, &format!("acc_{tag}"), "Acc@2026dd").await;
    let (_, sup) = login(page, &format!("sup_{tag}"), "Sup@2026dd").await;
    for sid in [&csh, &acc, &sup] {
        assert_eq!(
            select_book(page, sid, "b1").await,
            StatusCode::OK,
            "选账套应成功"
        );
    }
    (csh, format!("{acc}|{sup}"))
}

#[tokio::test]
async fn receipt_audit_gates_on_audit_or_post_permission() {
    let (state, _bd, _dir) = test_state();
    let (csh, pair) = two_person(&state, "rcptgate").await;
    let (acc, sup) = pair.split_once('|').unwrap();

    // 出纳录一张收款单（VoucherNew 够用）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/receipts",
            &csh,
            serde_json::json!({
                "date": "2026-01-15", "kind": "receipt", "fund_account": "100201",
                "party": "C01", "amount": "100", "memo": "gate"
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "出纳应能录收付款单：{b}");
    let rid: i64 = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    // ① 出纳自己审自己的单 → 必须 403。
    //    这是这道闸门的核心：放开给 VoucherPost 之后，"谁录谁审"这条路不能也一起放开。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/audit"),
            &csh,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::FORBIDDEN,
        "出纳不该能审核自己提的收付款单"
    );

    // ② 会计（无 VoucherAudit、有 VoucherPost）→ 应当能审。
    //    修前这里是 403，两人公司就卡死在这一步。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/audit"),
            acc,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "会计（VoucherPost）应能审核收付款单：{b}"
    );
    let vid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["voucher_id"]
        .as_i64()
        .expect("审核后应生成凭证");
    assert!(vid > 0, "审核后应带出凭证 id：{b}");

    // ③ 撤审必须用**同一套**判据 —— 只放开"审"不放开"撤"，
    //    会出现「会计能审但撤不回来」，那比一开始就没有还糟。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/unaudit"),
            acc,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "会计应能撤销审核（与审核同一判据）：{b}"
    );

    // ④ 主管（有 VoucherAudit）当然也可以 —— 原有的路不能被改坏
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/audit"),
            sup,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "主管（VoucherAudit）应仍能审核：{b}");

    // ⑤ 出纳撤审同样 403
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/unaudit"),
            &csh,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "出纳不该能撤审");
}

/// Web 差异核销（抹零）：应收 1000、实收 998、差 2 记入差异科目，
/// 被核销方全额结清、差异落到损益（出凭证），账龄不挂尾。
#[tokio::test]
async fn web_settle_diff_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收 1000（借 112201 客户 C01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "应收凭证应保存");
    let v1 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    // 收款 998（贷 112201 客户 C01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20", "word": "记",
                "no": 2, "attachments": 0, "memo": "收款",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "998", "credit": "0" },
                    { "line": 2, "account_code": "112201", "summary": "收款", "debit": "0", "credit": "998", "aux": { "customer": "C01" } }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "收款凭证应保存");
    let v2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    for vid in [v1, v2] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "凭证 {vid} 应能记账");
    }

    // 取未核销的分录 id（借方 = 被核销方，贷方 = 核销方）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "应收/收款两笔未核销：{open}");
    let from = rows
        .iter()
        .find(|r| r["debit"] != serde_json::json!("0.00"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();
    let to = rows
        .iter()
        .find(|r| r["credit"] != serde_json::json!("0.00"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();

    // 差异核销：核销 998 + 差异 2 记入 660305
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/run",
            &sid,
            serde_json::json!({
                "from_entry": from, "to_entry": to,
                "amount": "998", "diff": "2", "diff_account": "660305"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "差异核销应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dv = r["diff_voucher_id"].as_i64().expect("应生成差异凭证");

    // 应收全额结清
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "差异核销后应收应全额结清：{open}"
    );

    // 差异凭证：借 660305 / 贷 112201，金额 2
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{dv}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let debit = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["debit"] != serde_json::json!("0.00"))
        .unwrap();
    assert_eq!(debit["account_code"], "660305");
    assert_eq!(debit["debit"], serde_json::json!("2.00"));
}

/// Web 往来对冲：同一公司既欠我们（应收）、我们又欠它（应付），对冲 600 后
/// 两端各自 FIFO 核销，应收剩 400、应付清 0。
#[tokio::test]
async fn web_arap_offset_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收 1000（借 112201 客户 C01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v1 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    // 应付 600（贷 220201 供应商 S01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06", "word": "记",
                "no": 2, "attachments": 0, "memo": "应付",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "付款", "debit": "600", "credit": "0" },
                    { "line": 2, "account_code": "220201", "summary": "采购", "debit": "0", "credit": "600", "aux": { "supplier": "S01" } }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    for vid in [v1, v2] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "凭证 {vid} 应能记账");
    }

    // 守卫：对冲额超过任一端未核销合计 → 400（这里应付只有 600）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/offsets",
            &sid,
            serde_json::json!({
                "date": "2026-01-20", "customer": "C01", "supplier": "S01",
                "amount": "800", "memo": "超量"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "建单只落草稿，超量在审核时拦"
    );
    let over_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{over_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "超过应付未核销应拒绝"
    );

    // 正常对冲 600
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/offsets",
            &sid,
            serde_json::json!({
                "date": "2026-01-20", "customer": "C01", "supplier": "S01",
                "amount": "600", "memo": "月末对冲"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增对冲单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc_id = r["id"].as_i64().unwrap();
    assert_eq!(r["status"], "draft");
    assert!(r["voucher_id"].is_null(), "草稿阶段不应生成凭证");

    // 审核 → 出凭证 + 两端核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["voucher_id"].as_i64().expect("审核应生成凭证");
    assert_eq!(r["settled"], serde_json::json!(2), "两端各核销一笔");

    // 凭证：借 220201 / 贷 112201，各 600
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "220201");
    assert_eq!(v["entries"][0]["debit"], serde_json::json!("600.00"));
    assert_eq!(v["entries"][1]["account_code"], "112201");
    assert_eq!(v["entries"][1]["credit"], serde_json::json!("600.00"));

    // 应收剩 400、应付清 0
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "应收应只剩一笔：{open}");
    assert_eq!(
        rows[0]["open"],
        serde_json::json!("400.00"),
        "应收对冲 600 后剩 400"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=220201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "应付应被全额冲销"
    );

    // 列表可见且已审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/offsets", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 2, "两单都在列表里");

    // 删除链：已审核 → 拒；撤审 → 回草稿且核销回退；再删 OK
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "凭证存在时不能删单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{doc_id}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "撤审应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "撤审后应可删除");
}

/// Web 通用往来互抵：A 打来的钱清 B 的货款（同科目跨往来单位）、
/// 客户应收抵供应商应付（跨科目三方抵账）——都自动出过渡凭证并各自核销。
#[tokio::test]
async fn web_settle_cross_flow() {
    async fn add_posted(
        state: &Arc<WebState>,
        sid: &str,
        date: &str,
        entries: serde_json::Value,
    ) -> i64 {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": date, "word": "记",
                    "no": 0, "attachments": 0, "memo": "互抵测试", "entries": entries
                }),
            ))
            .await
            .unwrap();
        let status = resp.status();
        let body = body_string(resp).await;
        assert_eq!(status, StatusCode::OK, "建凭证应成功：{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let vid = v["id"].as_i64().unwrap();
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "记账应成功");
        vid
    }

    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // B 的货款：借 112201 客户 B01 1000
    add_posted(
        &state,
        &sid,
        "2026-01-10",
        serde_json::json!([
            { "line": 1, "account_code": "112201", "summary": "B货款", "debit": "1000", "credit": "0", "aux": { "customer": "B01" } },
            { "line": 2, "account_code": "600101", "summary": "B货款", "debit": "0", "credit": "1000" }
        ]),
    )
    .await;
    // A 打来的钱记在 A 名下：贷 112201 客户 A01 1000
    add_posted(
        &state,
        &sid,
        "2026-01-15",
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "A代付", "debit": "1000", "credit": "0" },
            { "line": 2, "account_code": "112201", "summary": "A代付", "debit": "0", "credit": "1000", "aux": { "customer": "A01" } }
        ]),
    )
    .await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "B 的应收 + A 的收款：{open}");
    let debit_entry = rows
        .iter()
        .find(|r| r["debit"] != serde_json::json!("0.00"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();
    let credit_entry = rows
        .iter()
        .find(|r| r["credit"] != serde_json::json!("0.00"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();

    // 守卫：方向反了（拿贷方当借方）→ 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/cross",
            &sid,
            serde_json::json!({
                "debit_entry": credit_entry, "credit_entry": debit_entry, "amount": "1000"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "方向反了应拒绝");

    // 正常互抵 1000（同科目跨往来单位）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/cross",
            &sid,
            serde_json::json!({
                "debit_entry": debit_entry, "credit_entry": credit_entry,
                "amount": "1000", "date": "2026-01-31"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "跨往来单位互抵应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["bridge_voucher_id"].as_i64().expect("必须出过渡凭证");

    // 过渡凭证：借 112201（A 的收款）/ 贷 112201（B 的货款）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "112201");
    assert_eq!(v["entries"][0]["debit"], serde_json::json!("1000.00"));
    assert_eq!(v["entries"][1]["account_code"], "112201");
    assert_eq!(v["entries"][1]["credit"], serde_json::json!("1000.00"));

    // 两边同时清
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "互抵后应收应全清：{open}"
    );

    // ---- 跨科目三方抵账：客户 A02 欠我们 500，我们欠供应商 S02 300 ----
    add_posted(
        &state,
        &sid,
        "2026-01-12",
        serde_json::json!([
            { "line": 1, "account_code": "112201", "summary": "A02应收", "debit": "500", "credit": "0", "aux": { "customer": "A02" } },
            { "line": 2, "account_code": "600101", "summary": "A02应收", "debit": "0", "credit": "500" }
        ]),
    )
    .await;
    add_posted(
        &state,
        &sid,
        "2026-01-13",
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "S02应付", "debit": "300", "credit": "0" },
            { "line": 2, "account_code": "220201", "summary": "S02应付", "debit": "0", "credit": "300", "aux": { "supplier": "S02" } }
        ]),
    )
    .await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ar_entry = open["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["aux_key"] == serde_json::json!("customer=A02"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=220201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ap_entry = open["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["aux_key"] == serde_json::json!("supplier=S02"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/cross",
            &sid,
            serde_json::json!({
                "debit_entry": ar_entry, "credit_entry": ap_entry,
                "amount": "300", "date": "2026-01-31"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "三方抵账应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid2 = r["bridge_voucher_id"]
        .as_i64()
        .expect("跨科目必须出过渡凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid2}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["entries"][0]["account_code"], "220201",
        "过渡凭证借方应是应付"
    );
    assert_eq!(
        v["entries"][1]["account_code"], "112201",
        "过渡凭证贷方应是应收"
    );

    // 应收 A02 剩 200、应付 S02 清 0
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["open"],
        serde_json::json!("200.00"),
        "应收 A02 抵 300 后剩 200"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=220201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "应付 S02 应全额冲销"
    );
}
