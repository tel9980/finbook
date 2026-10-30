//! 月结向导接口：清单形状、阻断逻辑、已结账期间
//!
//! 判定本身在 `findb::month_close` 有单测（含「必须与 precheck 一致」那条），
//! 这里只守 HTTP 这一层最容易走样的两点：
//!   ① 步骤清单必须完整下发（少了哪一步，界面就不会提醒）；
//!   ② `can_close` 必须是「所有 blocking 步骤都 ok」的结果，不能写死。

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

async fn get_json(
    state: &Arc<finweb::state::WebState>,
    sid: &str,
    uri: &str,
) -> (StatusCode, Value) {
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_get(uri, sid))
        .await
        .unwrap();
    let code = resp.status();
    let body = body_string(resp).await;
    (code, serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn step<'a>(wiz: &'a Value, code: &str) -> &'a Value {
    wiz["steps"]
        .as_array()
        .expect("steps 必须是数组")
        .iter()
        .find(|s| s["code"] == code)
        .unwrap_or_else(|| panic!("清单里缺少步骤 {code}"))
}

#[tokio::test]
async fn wizard_lists_every_step_in_order() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let (code, wiz) = get_json(&state, &sid, "/api/periods/202601/wizard").await;
    assert_eq!(code, StatusCode::OK, "{wiz}");
    let codes: Vec<&str> = wiz["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        vec![
            "sequence",
            "unposted",
            "trial",
            "carry",
            "cost",
            "negative_cash"
        ],
        "步骤清单变了就要同步改界面与文档"
    );
    for c in &codes {
        let s = step(&wiz, c);
        assert!(s["label"].is_string(), "{c} 缺 label");
        assert!(s["detail"].is_string(), "{c} 缺 detail");
        assert!(s["hint"].is_string(), "{c} 缺 hint");
        assert!(s["ok"].is_boolean(), "{c} 缺 ok");
        assert!(s["blocking"].is_boolean(), "{c} 缺 blocking");
    }
    assert_eq!(wiz["can_close"], true, "空账套第一期应当可以结账：{wiz}");
    assert_eq!(wiz["closed"], false);
    assert_eq!(
        wiz["period"], "2026-01",
        "period 用 YYYY-MM（与 precheck 一致）"
    );
}

/// 存一张草稿凭证 → 「凭证全部记账」这一步必须变为未完成，且 can_close 变 false
#[tokio::test]
async fn draft_voucher_blocks_closing() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let body = serde_json::json!({
        "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
        "no": 1, "attachments": 0, "memo": "向导测试",
        "entries": [
            { "line": 1, "account_code": "1001", "summary": "收款", "debit": "100", "credit": "0" },
            { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "100" }
        ],
    });
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &sid, body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{}", body_string(resp).await);

    let (code, wiz) = get_json(&state, &sid, "/api/periods/202601/wizard").await;
    assert_eq!(code, StatusCode::OK);
    let unposted = step(&wiz, "unposted");
    assert_eq!(
        unposted["ok"], false,
        "草稿凭证没记账，这一步不该打勾：{unposted}"
    );
    assert_eq!(unposted["blocking"], true);
    assert_eq!(wiz["can_close"], false, "有未记账凭证就不能结账：{wiz}");
}

/// 已结账的期间：向导只报状态，不能再结一次
#[tokio::test]
async fn closed_period_reports_state_and_refuses() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/close",
            &sid,
            serde_json::json!({ "require_carry": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{}", body_string(resp).await);

    let (code, wiz) = get_json(&state, &sid, "/api/periods/202601/wizard").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(wiz["closed"], true, "{wiz}");
    assert_eq!(wiz["can_close"], false, "已结账不该再说「可以结账」：{wiz}");
    assert_eq!(wiz["steps"][0]["code"], "sequence");
    assert_eq!(wiz["steps"][0]["ok"], true, "已结账本身不算顺序错误");
}
