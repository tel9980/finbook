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

/// 套打偏移能存能读。针式打印机走纸偏 2–3mm 是常态，以前只能改驱动或忍着；
/// 这两个字段丢了，退回去就又是「每次都对不齐」。
#[tokio::test]
async fn paper_offset_survives_save_and_reload() {
    let (state, sid, _g) = ready().await;
    let mut o = get_options(&state, &sid).await;
    assert_eq!(o["print"]["offset_x_mm"], 0, "默认不该有偏移：{o}");
    assert_eq!(o["print"]["offset_y_mm"], 0, "默认不该有偏移：{o}");

    o["print"]["offset_x_mm"] = json!(5);
    o["print"]["offset_y_mm"] = json!(-3);
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::OK, "合法偏移应保存成功：{body}");

    let back = get_options(&state, &sid).await;
    assert_eq!(back["print"]["offset_x_mm"], 5, "横向偏移没存住：{back}");
    assert_eq!(back["print"]["offset_y_mm"], -3, "纵向偏移没存住：{back}");
}

#[tokio::test]
async fn out_of_range_paper_offset_is_rejected() {
    let (state, sid, _g) = ready().await;
    let mut o = get_options(&state, &sid).await;
    // ±20mm 是「对位」的量级；填 80 说明是想改版式，不是对纸，得拦住问一句
    o["print"]["offset_x_mm"] = json!(80);
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "越界偏移必须被拦：{body}");
    assert!(body.contains("偏移"), "要讲清是偏移的问题：{body}");
    assert!(body.contains("80"), "要把填错的值带出来：{body}");
}

/// 套打纸型：三种纸各有各的档，存住且能拦住认不出来的值。
///
/// 为什么必须分三档：小厂里凭证常是 A5 或 241×140 连续纸、账簿是 A4、
/// 收付款单是 A5 或三等分联 —— 一个全局纸型必然有一头打歪。
#[tokio::test]
async fn paper_sizes_survive_save_and_reload() {
    let (state, sid, _g) = ready().await;
    let mut o = get_options(&state, &sid).await;
    assert_eq!(o["print"]["voucher_paper"], "A4", "老账套默认凭证 A4：{o}");
    assert_eq!(o["print"]["doc_paper"], "", "单据默认跟随模板：{o}");

    o["print"]["voucher_paper"] = json!("A5");
    o["print"]["ledger_paper"] = json!("A4");
    o["print"]["doc_paper"] = json!("third");
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::OK, "合法纸型应保存成功：{body}");

    let back = get_options(&state, &sid).await;
    assert_eq!(back["print"]["voucher_paper"], "A5", "凭证纸型没存住：{back}");
    assert_eq!(back["print"]["doc_paper"], "third", "单据纸型没存住：{back}");
}

#[tokio::test]
async fn unknown_paper_size_is_rejected() {
    let (state, sid, _g) = ready().await;
    let mut o = get_options(&state, &sid).await;
    // B5 不是支持的纸型：悄悄兜底成 A4 会让用户以为设置生效了，实际还在打歪
    o["print"]["voucher_paper"] = json!("B5");
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "认不出的纸型必须被拦：{body}");
    assert!(body.contains("纸型"), "要讲清是纸型的问题：{body}");
    assert!(body.contains("B5"), "要把填错的值带出来：{body}");
}

/// 试打一张：`?po_ox=` 覆盖**本次**渲染、不写库。没有这条，用户只能
/// 「改 → 存 → 去报表 → 打印 → 发现偏 → 回来再改」。
#[tokio::test]
async fn preview_query_can_shift_paper_without_saving() {
    let (state, sid, _g) = ready().await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance/print?from=202601&to=202601&po_ox=5",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;
    // 默认页边距 12mm：向右 5 → 左 17、右 7
    assert!(
        html.contains("margin:12mm 7mm 12mm 17mm"),
        "试打参数没有进页边距：{html}"
    );

    // 库里那份不能被动过
    let back = get_options(&state, &sid).await;
    assert_eq!(back["print"]["offset_x_mm"], 0, "试打不该改账套参数");
}

/// 单据统计前缀写错键名 → 永远用默认值，界面上看不出来。
#[tokio::test]
async fn unknown_doc_prefix_key_is_rejected() {
    let (state, sid, _g) = ready().await;
    let mut o = get_options(&state, &sid).await;
    o["doc_prefixes"] = json!({ "purchase": "CG" });
    let (st, body) = put_options(&state, &sid, &o).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "拼错的键名必须被拦：{body}");
    assert!(body.contains("purchase"), "要把拼错的键名带出来：{body}");
}
