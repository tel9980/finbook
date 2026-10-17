//! 集成测试共用助手。
//
// 从 api.rs 原样提取（2026-10-01），让 customers.rs 等测试文件共用同一套。
// 提取而不是复制：复制一份就会分叉 —— 改一处忘另一处，两边行为不一致时
// 你会怀疑「是产品的问题还是测试的问题」，而那是最难查的一类。
//
// 本文件里的函数必须保持**纯函数**：不引用 api.rs 的任何私有项。
// 若某个助手需要 api.rs 的私有状态，说明该测试不该独立成文件。

/// 建临时平台身份库 + 预置账套 + 完整 WebState，返回 (state, books_dir, dir_guard)
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use fincore::user::PasswordPolicy;
use fincore::BookOptions;
use tower::ServiceExt;

use finweb::handlers;
use finweb::realm::RealmDb;
use finweb::state::{BookRegistry, SessionStore, WebState};

pub fn test_state() -> (Arc<WebState>, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    // 平台身份库：引导管理员 boss（不强制改密，便于测试）
    let realm = RealmDb::open(dir.path().join("realm.db")).expect("建 realm 失败");
    realm
        .ensure_bootstrap("boss", "Admin!2026", false, &PasswordPolicy::default())
        .expect("引导管理员失败");

    // 预置一个归属 boss 的账套 b1
    let books_dir = dir.path().join("books");
    std::fs::create_dir_all(&books_dir).expect("建账套目录失败");
    let opts = BookOptions {
        start_period: fincore::Period::new(2026, 1).unwrap(),
        // 公司名非空：下推发票会用它填「本企业」（销项发票的卖方 / 进项的买方），
        // 以及税务申报表的表头。留空会让这类断言看着像产品 bug。
        company: "测试公司".into(),
        tax_no: "91110000TEST000001".into(),
        // 测试夹具**显式**关掉两道闸门：绝大多数用例的主题是银行对账 / 核销 / 账龄 /
        // 报表 / 结账，不是审核闸门也不是出纳签字，给它们统一加上「审核 + 签字 + 记账」
        // 三步只会淹没真正的主题。
        //
        // 生产默认值两道都是**开**（三权分离 + 现金/银行须出纳签字，见
        // fincore::account::BookOptions 文档），由 `audit_default_on_for_new_books`、
        // `require_cashier_gates_post_and_scopes_to_funds`、
        // `audit_gate_blocks_post_until_audited` 三个用例专门盯住默认值与闸门语义 ——
        // 夹具关掉不等于默认关掉。
        //
        // ⚠️ 这两行**必须显式写出来**，不能靠 `..Default::default()` 继承。
        // 实测过：require_cashier 的默认值从 false 改成 true 的那天，186 条用例里
        // 28 条集体变红，报错全是干巴巴的「记账应成功 left: 400 right: 200」，
        // 指向的是记账逻辑，实际是夹具静默继承了新默认值。
        // check-ci.js 现在会检查本夹具是否把两个开关都显式列出。
        enable_audit: false,
        require_cashier: false,
        ..Default::default()
    };
    let book_path = books_dir.join("b1.fbk");
    let db = findb::Db::create_no_admin(&book_path, &opts).expect("建账失败");
    // 身份对账：把 boss 种子为账套内管理员（复用生产逻辑）
    let ru = realm.get_user("boss").unwrap().expect("boss 应存在");
    finweb::realm::ensure_book_admin(&db, &ru).expect("种子账套管理员失败");
    drop(db);
    realm
        .register_book("b1", &book_path.to_string_lossy(), "boss", "预置公司")
        .unwrap();

    let reg = BookRegistry::new();
    reg.register(&book_path, 4);
    let state = WebState::new(
        reg,
        SessionStore::new(),
        realm,
        books_dir.clone(),
        "test".to_string(),
        opts.start_period.ymm(),
        std::path::PathBuf::new(),
        "test".to_string(),
    );
    (state, books_dir, dir)
}
pub async fn login_with_device(
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

pub async fn login(state: &Arc<WebState>, username: &str, password: &str) -> (StatusCode, String) {
    login_with_device(state, username, password, "dev-test-0001").await
}

/// 选择当前账套（进入某套账）
pub async fn select_book(state: &Arc<WebState>, sid: &str, key: &str) -> StatusCode {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/books/{}/select", key),
            sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    resp.status()
}

/// 普通用户自助建账套（返回 (status, body)）
// 跨 test target 共享的 helper：某些 target 用不到，dead_code 是误报，别删。
#[allow(dead_code)]
pub async fn create_book(state: &Arc<WebState>, sid: &str, company: &str) -> (StatusCode, String) {
    // 治理收归（账号模型二元化）：建套仅管理员——helper 内部改用管理员建套，并把原
    // 调用者（会话反查 username）邀请进套为会计，既有测试的「进入/协作」语义不变；
    // 调用者本身是管理员（boss）时直接用其会话建套、不重复邀请。
    let token = sid.split('=').nth(1).unwrap_or(sid); // Cookie 形如 "sid=xxx"，会话表用裸 token
    let caller = state.sessions.get(token, 0).map(|s| s.username);
    let admin_sid = match &caller {
        Some(u) if u == "boss" => sid.to_string(),
        _ => login(state, "boss", "Admin!2026").await.1,
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/books",
            &admin_sid,
            serde_json::json!({ "key": "", "company": company, "start_period": 202601 }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let s = body_string(resp).await;
    if status == StatusCode::OK {
        if let Some(u) = caller.filter(|u| u != "boss") {
            let key = serde_json::from_str::<serde_json::Value>(&s)
                .ok()
                .and_then(|v| v["key"].as_str().map(String::from));
            if let Some(key) = key {
                assert_eq!(
                    select_book(state, &admin_sid, &key).await,
                    StatusCode::OK,
                    "helper: 管理员应能进入新建账套"
                );
                let inv = handlers::router(state.clone())
                    .oneshot(authed_post(
                        "/api/users",
                        &admin_sid,
                        serde_json::json!({
                            "username": u, "display_name": u, "password": "",
                            "role": "accountant", "must_change_pwd": false
                        }),
                    ))
                    .await
                    .unwrap();
                assert_eq!(inv.status(), StatusCode::OK, "helper: 应把调用者邀请进套");
            }
        }
    }
    (status, s)
}

/// 带 sid 的请求
pub fn authed_get(uri: &str, sid: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, sid)
        .body(Body::empty())
        .unwrap()
}

/// 带 sid 的 POST JSON 请求
pub fn authed_post(uri: &str, sid: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, sid)
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// 带会话 cookie 的 PUT（批量导入用 PUT 与解析区分：解析不写库，导入写库）
// 跨 test target 共享：batch_paste.rs 在用，本 target 不用是误报，别删。
#[allow(dead_code)]
pub fn authed_put(uri: &str, sid: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, sid)
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub async fn body_string(resp: axum::http::Response<Body>) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .expect("读取响应体失败");
    String::from_utf8_lossy(&bytes).to_string()
}

/// boss 登录并进入预置账套 b1
pub async fn boss_in_b1(state: &Arc<WebState>) -> String {
    let (_, sid) = login(state, "boss", "Admin!2026").await;
    assert_eq!(select_book(state, &sid, "b1").await, StatusCode::OK);
    sid
}

// 跨 test target 共享（customers.rs 在用），本 target 不用是误报，别删。
#[allow(dead_code)]
pub fn money_num(s: &str) -> f64 {
    s.replace(',', "").parse::<f64>().unwrap_or(0.0)
}
