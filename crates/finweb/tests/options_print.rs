//! 账套参数（系统设置）的保存口径：打印设置能存能读，脏值必须被拦。
//!
//! 为什么单开一个文件盯这两件事：
//!
//! 1. `PUT /api/options` 收的是**整个** `BookOptions` 对象并整体覆盖。少带一个字段，
//!    `serde(default)` 会把它悄悄重置成默认值 —— 症状是「刚调好的打印版式，一保存
//!    又回原样」，而接口返回 200，日志里什么也看不出来。
//! 2. 校验如果只写在界面上，就会「哪条路径忘了调，哪条路径能写进脏数据」。这里从
//!    接口层打，才证明桌面端和网页端共用的是同一道闸。

mod common;

use common::{authed_get, authed_put, body_string, login, select_book, test_state};
use axum::http::StatusCode;
use finweb::handlers;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

use finweb::state::WebState;

/// 登录并进入预置账套 b1，返回 (state 保持存活, 会话 cookie)
async fn ready() -> (Arc<WebState>, String, tempfile::TempDir) {
    let (state, _books, guard) = test_state();
    let (st, sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(st, StatusCode::OK, "管理员应能登录");
    assert_eq!(
        select_book(&state, &sid, "b1").await,
        StatusCode::OK,
        "应能进入预置账套"
    );
    (state, sid, guard)
}

async fn get_options(state: &Arc<WebState>, sid: &str) -> Value {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    serde_json::from_str(&body_string(resp).await).expect("参数应是合法 JSON")
}

async fn put_options(state: &Arc<WebState>, sid: &str, body: &Value) -> (StatusCode, String) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", sid, body.clone()))
        .await
        .unwrap();
    let st = resp.status();
    (st, body_string(resp).await)
}

#[tokio::test]
async fn print_settings_survive_save_and_reload() {
    let (state, sid, _g) = ready().await;
    let mut o = get_options(&state, &sid).await;

    // 默认值必须是「自动 + 11pt + 12mm + 全显示」，否则存量账套的打印行为会莫名变化
    assert_eq!(o["print"]["orientation"], "auto", "默认方向应为自动：{o}");
    assert_eq!(o["print"]["font_pt"], 11);
    assert_eq!(o["print"]["margin_mm"], 12);

    o["print"]["orientation"] = json!("landscape");
    o["print"]["font_pt"] = json!(9);
    o["print"]["margin_mm"] = json!(20);
    o["print"]["show_page_no"] = json!(false);
    o["print"]["sign_line"] = json!("制表：老李");
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::OK, "合法参数应保存成功：{body}");

    let back = get_options(&state, &sid).await;
    assert_eq!(back["print"]["orientation"], "landscape", "方向没存住：{back}");
    assert_eq!(back["print"]["font_pt"], 9, "字号没存住：{back}");
    assert_eq!(back["print"]["margin_mm"], 20);
    assert_eq!(back["print"]["show_page_no"], false);
    assert_eq!(back["print"]["sign_line"], "制表：老李");
}

#[tokio::test]
async fn out_of_range_print_settings_are_rejected_with_the_value() {
    let (state, sid, _g) = ready().await;
    let mut o = get_options(&state, &sid).await;
    o["print"]["font_pt"] = json!(30);
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "越界字号必须被拦：{body}");
    assert!(body.contains("字号"), "要讲清是字号的问题：{body}");
    assert!(body.contains("30"), "要把填错的值带出来：{body}");

    // 拦下来之后，库里那份不能被动过
    let back = get_options(&state, &sid).await;
    assert_eq!(back["print"]["font_pt"], 11, "被拒的保存不该改动原值");
}

#[tokio::test]
async fn biz_account_must_be_a_leaf_account() {
    let (state, sid, _g) = ready().await;
    let mut o = get_options(&state, &sid).await;
    // 1122 是一级科目（下有 112201），拿它当应收科目不会有任何提示，
    // 直到某天点「发货」才报「不能直接记账」——那时单据已经开出去了
    o["biz_accounts"]["ar"] = json!("1122");
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "一级科目必须被拦：{body}");
    assert!(body.contains("末级"), "要讲清是「不是末级」：{body}");
}

#[tokio::test]
async fn unknown_biz_account_is_rejected() {
    let (state, sid, _g) = ready().await;
    let mut o = get_options(&state, &sid).await;
    o["biz_accounts"]["income"] = json!("999999");
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "不存在的科目必须被拦：{body}");
    assert!(body.contains("不存在"), "要讲清是科目不存在：{body}");
}

/// 全量 GET → PUT 原样回存必须成功（幂等），否则任何一页「保存」都会踩雷
#[tokio::test]
async fn options_roundtrip_is_idempotent() {
    let (state, sid, _g) = ready().await;
    let o = get_options(&state, &sid).await;
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::OK, "原样回存应成功：{body}");
}
