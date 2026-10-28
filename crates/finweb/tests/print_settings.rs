//! 打印预览页「点一下、拖一下就能存」的后端。
//!
//! 这条链路的三个关键点，各一条测试：
//!
//! 1. 局部更新 —— 预览页不认识的字段（凭证/账簿纸型）不能被冲掉；
//! 2. 越界值要报错并带上原值 —— 滑块给不出 20pt，但接口不假设调用方守规矩；
//! 3. 未登录一律拒绝 —— 版式是账套级的，改一次所有人都变。

mod common;

use axum::body::Body;
use axum::http::{header, StatusCode};
use common::*;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

async fn post_patch(
    state: &Arc<finweb::state::WebState>,
    sid: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_post("/api/reports/print-settings", sid, body))
        .await
        .unwrap();
    let code = resp.status();
    (code, body_string(resp).await)
}

async fn get_options(state: &Arc<finweb::state::WebState>, sid: &str) -> serde_json::Value {
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_get("/api/options", sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    serde_json::from_str(&body_string(resp).await).unwrap()
}

/// 拖一下字号、关掉页码：两样都存住，而且**别的设置一个都没动**。
#[tokio::test]
async fn a_single_drag_is_saved_without_touching_the_rest() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    // 先把三个「预览页不认识的」字段改成非默认值，后面看它们有没有被冲掉
    let mut opts = get_options(&state, &sid).await;
    opts["print"]["voucher_paper"] = json!("241x140");
    opts["print"]["ledger_paper"] = json!("A5");
    opts["print"]["offset_x_mm"] = json!(-3);
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{}", body_string(resp).await);

    // 预览页上拖字号 + 关页码
    let (code, body) = post_patch(
        &state,
        &sid,
        json!({ "font_pt": 13, "show_page_no": false }),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{body}");

    let after = get_options(&state, &sid).await;
    assert_eq!(after["print"]["font_pt"], 13, "字号没存住");
    assert_eq!(after["print"]["show_page_no"], false, "页码开关没存住");
    assert_eq!(after["print"]["voucher_paper"], "241x140", "凭证纸型被冲掉了");
    assert_eq!(after["print"]["ledger_paper"], "A5", "账簿纸型被冲掉了");
    assert_eq!(after["print"]["offset_x_mm"], -3, "套打偏移被冲掉了");
}

/// 越界的字号：400 + 原值，而不是悄悄夹成 14。
#[tokio::test]
async fn out_of_range_value_is_rejected_with_the_number() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let (code, body) = post_patch(&state, &sid, json!({ "font_pt": 20 })).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("20"), "错误信息要带上原值，body={body}");
    // 被拒之后库里不能变
    let after = get_options(&state, &sid).await;
    assert_eq!(after["print"]["font_pt"], 11, "拒绝之后不该留下半个改动");
}

/// 未登录不能改版式。
#[tokio::test]
async fn print_settings_require_login() {
    let (state, _dir, _tmp) = test_state();
    let resp = finweb::handlers::router(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/reports/print-settings")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"font_pt":12}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
