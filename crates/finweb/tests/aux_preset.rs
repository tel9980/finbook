//! 辅助核算「预置常用档案」的 HTTP 层集成测试。
//!
//! 存在的理由是一次真实事故：`GET /api/aux-presets?kind=dept` 返回 **422**，
//! 因为 `KindQuery.kind` 没写 `#[serde(default)]`。axum 对查询串反序列化失败
//! 直接 422，**不会**交给处理函数里那行 `unwrap_or(Dept)`。结果前端
//! `loadAuxPreset` catch 到错误直接 return —— 按钮保持 hidden、提示区空白，
//! 整条预置链路静默消失。
//!
//! 为什么编译期和单元测试都发现不了：Rust 侧那只 handler 逻辑是对的，
//! 错的是「框架在进 handler 之前就拒了」。这类 bug 只有真的发一个
//! **带查询串的 HTTP 请求**才暴露 —— 所以这条测试的价值全在那个 `?kind=`。
//!
//! 顺带钉住的是产品约定：只预置部门/银行账户，客户/供应商/存货一条不加；
//! 且重复预置**不覆盖**已有编码。

use axum::http::StatusCode;
use std::sync::Arc;
use tower::ServiceExt;

use finweb::handlers;
use finweb::state::WebState;

mod common;
use common::{authed_get, authed_post, body_string, boss_in_b1, test_state};

async fn get(state: &Arc<WebState>, sid: &str, uri: &str) -> (StatusCode, String) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(uri, sid))
        .await
        .unwrap();
    let s = resp.status();
    (s, body_string(resp).await)
}

async fn post(
    state: &Arc<WebState>,
    sid: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, String) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(uri, sid, body))
        .await
        .unwrap();
    let s = resp.status();
    (s, body_string(resp).await)
}

/// 回归：带 `?kind=` 的 GET 必须 200。
///
/// 这一条就是本次事故的钉子 —— 少了 `#[serde(default)]` 它立刻变 422。
#[tokio::test]
async fn 预置清单的查询串能正常解析() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    let (st, body) = get(&state, &sid, "/api/aux-presets?kind=dept").await;
    assert_eq!(st, StatusCode::OK, "?kind=dept 应 200，实际 {st}：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["available"], true);
    assert_eq!(v["items"].as_array().unwrap().len(), 3, "部门应预置 3 条");

    // 没有 kind 参数也不能 422 —— 前端任何一次拼错都会走到这里
    let (st, body) = get(&state, &sid, "/api/aux-presets").await;
    assert_eq!(
        st,
        StatusCode::OK,
        "缺 kind 应回落到默认值，不该 422：{body}"
    );
    // 类别不认识也不能 422（回落成部门而不是报错）
    let (st, body) = get(&state, &sid, "/api/aux-presets?kind=not-a-kind").await;
    assert_eq!(st, StatusCode::OK, "未知类别应回落，不该 422：{body}");
}

/// 只预置有通用值的类别
#[tokio::test]
async fn 只预置部门与银行账户() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    let (_, body) = post(
        &state,
        &sid,
        "/api/aux-presets",
        serde_json::json!({"kind": "dept"}),
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["inserted"], 3);

    let (_, body) = post(
        &state,
        &sid,
        "/api/aux-presets",
        serde_json::json!({"kind": "bank"}),
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["inserted"], 2);

    // 客户/供应商/存货：预置端点必须报告「什么都没加」，而不是偷偷造几条
    for k in ["customer", "supplier", "item"] {
        let (_, body) = post(
            &state,
            &sid,
            "/api/aux-presets",
            serde_json::json!({"kind": k}),
        )
        .await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["inserted"], 0, "{k} 不该被预置");
        assert_eq!(v["skipped"].as_array().unwrap().len(), 0);

        // 清单接口也必须说 available=false，前端据此不显示按钮
        let (_, body) = get(&state, &sid, &format!("/api/aux-presets?kind={k}")).await;
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["available"], false, "{k} 不该有预置清单");
    }
}

/// 核心约定：重复预置**不覆盖**已有档案
#[tokio::test]
async fn 重复预置不覆盖已有档案() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    // 用户先自己建了一个 D01
    let (st, body) = post(
        &state,
        &sid,
        "/api/aux",
        serde_json::json!({
            "id": 0, "kind": "dept", "code": "D01", "name": "我的部门",
            "parent_code": null, "disabled": false, "props": {}, "memo": "我自己建的"
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "建自有部门应成功：{body}");

    let (_, body) = post(
        &state,
        &sid,
        "/api/aux-presets",
        serde_json::json!({"kind": "dept"}),
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["inserted"], 2, "只该新增 D02/D03");
    let skipped = v["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0], "D01", "D01 必须报告为跳过");

    // D01 必须还是「我的部门」—— 若用 INSERT OR REPLACE 会被悄悄改掉
    let (st, body) = get(&state, &sid, "/api/aux?kind=dept").await;
    assert_eq!(st, StatusCode::OK);
    // list_aux 直接返回数组（没有 {rows:[…]} 包装）
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    let d01 = rows
        .iter()
        .find(|r| r["code"] == "D01")
        .expect("D01 应还在");
    assert_eq!(d01["name"], "我的部门", "已有档案被覆盖了");
    assert_eq!(d01["memo"], "我自己建的", "已有档案的备注被覆盖了");
    assert_eq!(rows.len(), 3, "应是 我的部门 + D02 + D03");
}

/// 预置的银行账户不带编造账号 —— 编造的账号会被打进对账单和报表
#[tokio::test]
async fn 预置的银行账户不带账号() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    post(
        &state,
        &sid,
        "/api/aux-presets",
        serde_json::json!({"kind": "bank"}),
    )
    .await;
    let (st, body) = get(&state, &sid, "/api/aux?kind=bank").await;
    assert_eq!(st, StatusCode::OK);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(rows.len(), 2);
    for r in rows {
        let acct = r["props"].get("bank_account");
        assert!(
            acct.is_none() || acct == Some(&serde_json::json!("")),
            "{} 不该带编造账号：{acct:?}",
            r["code"]
        );
        assert!(
            !r["memo"].as_str().unwrap_or("").is_empty(),
            "{} 预置条目必须带说明，提醒用户这是通用占位",
            r["code"]
        );
    }
}
