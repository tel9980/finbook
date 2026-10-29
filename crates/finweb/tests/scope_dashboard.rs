//! 数据范围（DataScope）在「有金额的出口」上到底有没有生效。
//!
//! 已有的 `api.rs::data_scope_ledgers` 守的是试算平衡表；这一组守另外两条最容易漏的：
//!
//! 1. **首页**那三块数字（本期经营结果 / 钱在哪 / 本年累计）；
//! 2. **往来**入口（账龄、未核销清单）。
//!
//! 为什么单拎出来：首页和往来都不是"报表页"，权限检查很容易只加在报表上。
//! 而它们显示的是同一批数 —— 报表裁了、首页不裁，就会出现「首页看到全公司、
//! 点进利润表只剩自己的」，两边都"看起来对"，最难查。
//!
//! 两条断言都从"违规侧"出发：查不到权限外的科目，且**不是**返回空清单。

mod common;

use axum::http::StatusCode;
use common::*;
use finweb::handlers;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

async fn get_json(
    state: &Arc<finweb::state::WebState>,
    sid: &str,
    uri: &str,
) -> (StatusCode, serde_json::Value) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(uri, sid))
        .await
        .unwrap();
    let code = resp.status();
    let body = body_string(resp).await;
    let v = serde_json::from_str(&body).unwrap_or(json!(body));
    (code, v)
}

/// 造一张 借 1001 100 / 贷 6001 100 的已记账凭证，并返回当期 ymm。
async fn seed_cash_and_income(state: &Arc<finweb::state::WebState>, sid: &str) -> i32 {
    let (_, dash) = get_json(state, sid, "/api/dashboard").await;
    let cur = dash["current_period"].as_str().expect("dashboard 有当前期间").to_string();
    let cur_ymm: i32 = cur.replace('-', "").parse().unwrap();
    let date = format!("{cur}-15");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            sid,
            json!({
                "id": 0, "period": cur_ymm, "date": date, "word": "记", "no": 1,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "范围测试收款", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "600101", "summary": "范围测试收入", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    let code = resp.status();
    let body = body_string(resp).await;
    assert_eq!(code, StatusCode::OK, "造范围测试凭证：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let vid = v["id"].as_i64().expect("凭证 id");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(&format!("/api/vouchers/{vid}/post"), sid, json!({})))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账");
    cur_ymm
}

/// 平台开号 → 首登改密 → 入套为会计 → 设科目范围，返回其会话
async fn scoped_accountant(
    state: &Arc<finweb::state::WebState>,
    boss_sid: &str,
    username: &str,
    from: &str,
    to: &str,
) -> String {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            boss_sid,
            json!({ "username": username, "display_name": "范围会计", "password": "Sc1@pass99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "平台开号");
    let (_, s) = login(state, username, "Sc1@pass99").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &s,
            json!({ "old": "Sc1@pass99", "new": "Sc1@pass99x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "首登改密");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            boss_sid,
            json!({ "username": username, "display_name": "范围会计", "password": "",
                    "role": "accountant", "must_change_pwd": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "入套");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/users/{username}"),
            boss_sid,
            json!({ "data_scope": { "account_from": from, "account_to": to } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "设科目范围");
    let (_, sid2) = login(state, username, "Sc1@pass99x").await;
    assert_eq!(select_book(state, &sid2, "b1").await, StatusCode::OK);
    sid2
}

/// 首页「钱在哪」跟着科目范围走：范围只有收入科目时，手头现金不能显示出来。
#[tokio::test]
async fn dashboard_money_position_respects_data_scope() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;
    let cur = seed_cash_and_income(&state, &boss).await;

    let (code, dash) = get_json(&state, &boss, "/api/dashboard").await;
    assert_eq!(code, StatusCode::OK);
    let boss_cash = dash["brief"]["money"]["cash"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        boss_cash, "100.00",
        "老板应看到 100 现金，实际 {boss_cash}；dashboard={dash}"
    );

    // 只允许 6001（收入），1001（库存现金）在范围外
    let scoped = scoped_accountant(&state, &boss, "scd1", "600101", "600101").await;
    let (code, dash2) = get_json(&state, &scoped, &format!("/api/dashboard?period={cur}")).await;
    assert_eq!(code, StatusCode::OK);
    let scoped_cash = dash2["brief"]["money"]["cash"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_ne!(
        scoped_cash, "100.00",
        "范围外的 1001 现金不该出现在首页：{dash2}"
    );
    assert!(
        scoped_cash.is_empty() || scoped_cash == "0.00",
        "范围外现金应当为空或 0，实际 {scoped_cash}"
    );
}

/// 往来入口（账龄 / 未核销）对范围外科目必须 403，而不是给一个空清单。
#[tokio::test]
async fn settle_endpoints_refuse_out_of_scope_accounts() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;
    let _ = seed_cash_and_income(&state, &boss).await;
    // 范围只含 1001，112201（应收账款）在范围外
    let scoped = scoped_accountant(&state, &boss, "scd2", "1001", "1001").await;

    for uri in [
        "/api/settle/aging?account=112201",
        "/api/settle/open?account=112201",
    ] {
        let (code, body) = get_json(&state, &scoped, uri).await;
        assert_eq!(
            code,
            StatusCode::FORBIDDEN,
            "{uri} 应当 403，实际 {code} {body}"
        );
        let msg = body.to_string();
        assert!(
            msg.contains("无权查看该科目"),
            "{uri} 的错误信息要说明是权限问题：{msg}"
        );
    }

    // 范围内的科目照常可用（不能因为加了闸门把正常路径也堵掉）
    let (code, _) = get_json(&state, &scoped, "/api/settle/aging?account=1001").await;
    assert_eq!(code, StatusCode::OK, "范围内科目应当可用");
}
