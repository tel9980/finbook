//! 取消核销的两种形态：整条 / 部分。
//!
//! 逻辑本身（部分撤销怎么减、已结账为什么拒绝）由
//! `findb::settle` 的单测覆盖，这里只守 HTTP 这一层最容易出错的两处：
//!
//! 1. `amount` 是**可选**字段 —— 老前端只传 `{id}` 也要照旧能用；
//! 2. `amount` 传了个乱七八糟的字符串时，必须在碰数据库之前就 400，
//!    并把原值回显出来（「撤销金额「5万」不是合法金额」比 500 有用得多）。

mod common;

use axum::body::Body;
use axum::http::StatusCode;
use common::*;
use std::sync::Arc;
use tower::ServiceExt;

async fn post_unsettle(
    state: &Arc<finweb::state::WebState>,
    sid: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_post("/api/settle/unsettle", sid, body))
        .await
        .unwrap();
    let code = resp.status();
    (code, body_string(resp).await)
}

/// 只传 id：老前端的行为不变 —— 记录不存在时报「找不到核销记录」，而不是别的东西。
#[tokio::test]
async fn full_unsettle_without_amount_still_works() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let (code, body) = post_unsettle(&state, &sid, serde_json::json!({ "id": 999999 })).await;
    assert!(
        code == StatusCode::NOT_FOUND || code == StatusCode::BAD_REQUEST,
        "不存在的记录应当是 404/400，实际 {code} {body}"
    );
}

/// 金额写成「5万」这种：400 + 原值回显，不能静默当成 0 或整条撤销。
#[tokio::test]
async fn partial_unsettle_rejects_a_bad_amount_before_touching_the_db() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let (code, body) = post_unsettle(
        &state,
        &sid,
        serde_json::json!({ "id": 999999, "amount": "5万" }),
    )
    .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "实际 {code} {body}");
    assert!(body.contains("不是合法金额"), "body={body}");
    assert!(body.contains("5万"), "错误信息里要带上用户填的原值，body={body}");
}

/// 空串等价于「没填」——整条撤销。
#[tokio::test]
async fn blank_amount_means_full_unsettle() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let (code, body) = post_unsettle(
        &state,
        &sid,
        serde_json::json!({ "id": 999999, "amount": "   " }),
    )
    .await;
    // 空串应当走「整条撤销」分支：它照样会因为记录不存在而报错，但**不是**
    // 「不是合法金额」——两者的区别是真实存在的：一个说「你填错了」，一个说「没这条」。
    assert!(
        !body.contains("不是合法金额"),
        "空串不该被当成非法金额，body={body}"
    );
    assert!(body.contains("核销记录"), "应当落到「记录不存在」，实际 {code} {body}");
}
