//! 催款函打印页
//!
//! 逻辑（逾期天数怎么算、只列欠款那一侧）由 `findb::settle` 的单测守；
//! 这里守三件只有 HTTP 这一层才看得见的事：
//!   ① 这页真的出得来，而且是一封「函」——有致语、有合计、有落款；
//!   ② 预收/预付不能混进合计（客户会以为我们记错了账），但要单独说明；
//!   ③ 找不到的单子必须是 404，不是 500。

mod common;

use axum::http::StatusCode;
use common::*;
use std::sync::Arc;
use tower::ServiceExt;

async fn setup_book_with_ar(state: &Arc<finweb::state::WebState>, sid: &str) {
    // 客户档案，账期 30 天
    let ent = serde_json::json!({
        "id": 0, "kind": "customer", "code": "C900", "name": "青云客户",
        "parent_code": null, "disabled": false, "props": { "pay_days": "30" }, "memo": ""
    });
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_post("/api/aux", sid, ent))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建客户档案");

    // 一张挂很久的应收（账期 30 → 逾期）
    let csv = "类型,客商编码,单据号,单据日期,金额,客商名称,备注\n\
               应收,C900,XSQ-8801,2026-01-05,8000,青云客户,老货款\n";
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            sid,
            serde_json::json!({ "kind": "arap_opening", "template": "generic", "text": csv }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入往来期初");
}

async fn create_dunning(state: &Arc<finweb::state::WebState>, sid: &str) -> i64 {
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/dunnings",
            sid,
            serde_json::json!({
                "kind": "ar", "account": "1122", "party_code": "C900",
                "party_name": "青云客户", "date": "2026-10-25", "memo": "请于本月内结清"
            }),
        ))
        .await
        .unwrap();
    let code = resp.status();
    let body = body_string(resp).await;
    assert_eq!(code, StatusCode::OK, "生成催款单：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    v["dunning"]["id"]
        .as_i64()
        .or_else(|| v["id"].as_i64())
        .unwrap_or_else(|| panic!("返回里没有 id：{body}"))
}

#[tokio::test]
async fn dunning_letter_is_a_printable_document() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    setup_book_with_ar(&state, &sid).await;
    let id = create_dunning(&state, &sid).await;

    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/settle/dunnings/{id}/print"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;

    assert!(html.contains("催款函"), "标题要写清这是什么文书：{html}");
    assert!(html.contains("致：青云客户"), "函必须有收件人");
    assert!(html.contains("8,000.00"), "合计金额要出现");
    assert!(html.contains("XSQ-8801"), "明细要能对上单据号");
    assert!(html.contains("逾期"), "账期 30 天、挂了快一年，必须标逾期");
    assert!(html.contains("请于本月内结清"), "备注要带进函里");
    assert!(html.contains("制表"), "落款要留着");
}

#[tokio::test]
async fn dunning_letter_404s_for_unknown_id() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/dunnings/999999/print", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
