//! 账套参数的三件事：导出成文件 / 导入回来 / 恢复默认
//!
//! 逻辑在 `fincore::BookOptions` 的 `reset_preferences` / `adopt_config` 有单测，
//! 这里守 HTTP 层最容易走样的三点：
//!   ① 导出必须把整个对象写出来（少一个字段，导入后会静默回到默认值）
//!   ② 结构项不同必须整体拒绝，且**一个字段都不改**
//!   ③ 恢复默认不能顺手把企业名称、启用期间、默认科目清掉

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

async fn get_json(state: &Arc<finweb::state::WebState>, sid: &str, uri: &str) -> (StatusCode, Value) {
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_get(uri, sid))
        .await
        .unwrap();
    let code = resp.status();
    let body = body_string(resp).await;
    (code, serde_json::from_str(&body).unwrap_or(Value::Null))
}

async fn post_json(
    state: &Arc<finweb::state::WebState>,
    sid: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_post(uri, sid, body))
        .await
        .unwrap();
    let code = resp.status();
    let body = body_string(resp).await;
    (code, serde_json::from_str(&body).unwrap_or(Value::Null))
}

#[tokio::test]
async fn exported_config_covers_every_field() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let (code, opts) = get_json(&state, &sid, "/api/options").await;
    assert_eq!(code, StatusCode::OK);
    for k in [
        "code_scheme",
        "start_period",
        "base_currency",
        "company",
        "tax_no",
        "enable_qty",
        "enable_foreign",
        "enable_production",
        "enable_audit",
        "require_cashier",
        "require_audit",
        "biz_accounts",
        "budget_control",
        "doc_prefixes",
        "print",
        "voucher_words",
    ] {
        assert!(!opts[k].is_null(), "导出的参数缺字段 {k}：{opts}");
    }
    // print 里也得是全的 —— 版式字段漏一个，导入后就回默认，用户看不出为什么
    for k in [
        "orientation",
        "font_pt",
        "margin_mm",
        "offset_x_mm",
        "offset_y_mm",
        "show_company",
        "show_printed_at",
        "show_page_no",
        "show_signatures",
        "sign_line",
    ] {
        assert!(!opts["print"][k].is_null(), "print 缺字段 {k}");
    }
}

/// 导入：改打印版式与预算控制能生效；编码级长不同必须整体拒绝且不改任何字段
#[tokio::test]
async fn import_moves_preferences_but_refuses_structural_mismatch() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let (_, mut incoming) = get_json(&state, &sid, "/api/options").await;
    incoming["print"]["font_pt"] = Value::from(13);
    incoming["print"]["orientation"] = Value::from("landscape");
    incoming["budget_control"] = Value::from("strong");

    let (code, body) = post_json(&state, &sid, "/api/options/import", incoming.clone()).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let (_, now) = get_json(&state, &sid, "/api/options").await;
    assert_eq!(now["print"]["font_pt"], 13, "版式要真的存进去");
    assert_eq!(now["budget_control"], "strong");

    // 结构项不同：拒绝
    let company_before = now["company"].clone();
    let mut bad = now.clone();
    // 必须与测试账套真的不同：b1 本身就是 4-2-2-2-2
    bad["code_scheme"] = Value::from(vec![3, 3, 3]);
    bad["company"] = Value::from("不该生效");
    let (code, body) = post_json(&state, &sid, "/api/options/import", bad).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("编码级长"),
        "错误要说清是哪一项：{body}"
    );
    let (_, after) = get_json(&state, &sid, "/api/options").await;
    assert_eq!(after["company"], company_before, "拒绝时不能部分生效");
}

/// 恢复默认：版式/预算/单据前缀回默认，企业名称、启用期间、默认科目不动
#[tokio::test]
async fn reset_only_touches_preferences() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let (_, mut incoming) = get_json(&state, &sid, "/api/options").await;
    incoming["print"]["font_pt"] = Value::from(14);
    incoming["print"]["offset_x_mm"] = Value::from(5);
    incoming["budget_control"] = Value::from("warn");
    incoming["enable_production"] = Value::from(true);
    let (code, body) = post_json(&state, &sid, "/api/options/import", incoming).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let (_, before) = get_json(&state, &sid, "/api/options").await;

    let (code, body) = post_json(&state, &sid, "/api/options/reset", Value::Null).await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let (_, after) = get_json(&state, &sid, "/api/options").await;

    assert_eq!(after["print"]["font_pt"], 11, "字号回默认");
    assert_eq!(after["print"]["offset_x_mm"], 0);
    assert_eq!(after["budget_control"], "");
    assert_eq!(after["enable_production"], false);

    for k in ["company", "start_period", "code_scheme", "biz_accounts", "voucher_words"] {
        assert_eq!(after[k], before[k], "{k} 与已记账数据绑定，恢复默认不能动它");
    }
}
