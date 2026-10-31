//! finweb 关键 API 集成测试（多租户模型）
//!
//! 覆盖：平台身份库引导、平台登录、普通用户自建账套、归属隔离、
//! 成员协作（邀请账套内成员）、管理员跨账套查看（不留痕迹）、
//! 平台账号管理权限边界、「一人一机」设备绑定与重置、导入等核心业务链路。
//! 这是「多租户 + 归属隔离 + 设备绑定」部署级特性的回归防线。

// 集成测试共用助手（test_state / boss_in_b1 / authed_* / body_string / money_num /
// create_book / login / select_book）。2026-10-01 从本文件提取，供 customers.rs 复用。
#[path = "../common/mod.rs"]
pub mod common;
pub use common::*;

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

// ---------------------------------------------------------------------------
// 基础
// ---------------------------------------------------------------------------

// ===== 共享夹具（原 api.rs 中被用例夹着的辅助函数，v1.8.59 收进本文件）=====

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

/// 显式指定 key 建套（管理员），返回实际 key。
async fn create_book_with_key(state: &Arc<WebState>, sid: &str, key: &str) -> String {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/books",
            sid,
            serde_json::json!({ "key": key, "company": key, "start_period": 202601 }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "建套 {key} 应成功：{body}");
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string()
}

// ===== 用例按域拆到 `tests/api/`（v1.8.59）；本文件只留共享夹具 =====
mod assets;
mod auth;
mod books;
mod consign;
mod export;
mod funds;
mod hr;
mod inventory;
mod misc;
mod piecepay;
mod platform;
mod processfee;
mod prod;
mod reports;
mod sales;
mod settle;
mod voucher;
