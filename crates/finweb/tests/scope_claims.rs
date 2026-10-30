//! 数据范围的第三、四个出口：**部门范围**（报销/工资）与**写侧**越权。
//!
//! v1.8.54 守的是首页与往来（"不是报表页"的读）。这一版守剩下两类：
//!
//! 1. **部门范围**：`DataScope.depts`。工资那边早就有 `scope_payroll` 统一收口，
//!    报销却是 `list_claims` / `export_claims` / 四个写接口各抄一句 —— 抄漏了部门。
//! 2. **写侧**：`own_voucher_only` 的人看不见别人的凭证，但"拿着 id 能不能改"是另一件事。
//!    看不见却能改，比看不见更糟：没人会发现。
//!
//! 两条都从"越权者"视角写，并各带一条"自己范围内照常可用"的反向断言 ——
//! 加闸门最容易顺手把正常路径也堵掉。

mod common;

use axum::http::StatusCode;
use common::*;
use finweb::handlers;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

async fn call(
    state: &Arc<finweb::state::WebState>,
    method: &str,
    uri: &str,
    sid: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = match method {
        "GET" => authed_get(uri, sid),
        "POST" => authed_post(uri, sid, body.unwrap_or(json!({}))),
        _ => authed_put(uri, sid, body.unwrap_or(json!({}))),
    };
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    let code = resp.status();
    let text = body_string(resp).await;
    (code, serde_json::from_str(&text).unwrap_or(json!(text)))
}

/// 建一张报销单（部门 + 申请人可控）
async fn make_claim(
    state: &Arc<finweb::state::WebState>,
    sid: &str,
    dept: &str,
    applicant: &str,
    amount: &str,
) -> i64 {
    let (code, v) = call(
        state,
        "POST",
        "/api/claims",
        sid,
        Some(json!({
            "applicant": applicant,
            "dept": dept,
            "biz_date": "",
            "reason": "范围测试",
            "amount": amount,
            "items": [ { "expense_account": "660101", "amount": amount, "memo": "范围测试" } ]
        })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "建报销单失败：{v}");
    v["id"].as_i64().expect("报销单 id")
}

/// 平台开号 → 首登改密 → 入套 → 设数据范围，返回其会话
async fn scoped_user(
    state: &Arc<finweb::state::WebState>,
    boss_sid: &str,
    username: &str,
    scope: Value,
    role: &str,
) -> String {
    let (code, v) = call(
        state,
        "POST",
        "/api/platform/users",
        boss_sid,
        Some(json!({ "username": username, "display_name": "范围会计", "password": "Sc2@pass99" })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "平台开号：{v}");
    let (_, sid0) = login(state, username, "Sc2@pass99").await;
    let (code, v) = call(
        state,
        "POST",
        "/api/change-password",
        &sid0,
        Some(json!({ "old": "Sc2@pass99", "new": "Sc2@pass99x" })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "首登改密：{v}");
    let (code, v) = call(
        state,
        "POST",
        "/api/users",
        boss_sid,
        Some(
            json!({ "username": username, "display_name": "范围会计", "password": "",
                     "role": role, "must_change_pwd": false }),
        ),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "入套：{v}");
    let (code, v) = call(
        state,
        "PUT",
        &format!("/api/users/{username}"),
        boss_sid,
        Some(json!({ "data_scope": scope })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "设数据范围：{v}");
    let (_, sid) = login(state, username, "Sc2@pass99x").await;
    assert_eq!(select_book(state, &sid, "b1").await, StatusCode::OK);
    sid
}

/// 部门范围：报销单列表与导出都只看本部门
#[tokio::test]
async fn claim_list_and_export_respect_dept_scope() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;
    let prod = make_claim(&state, &boss, "生产部", "张三", "120.00").await;
    let sales = make_claim(&state, &boss, "销售部", "李四", "340.00").await;

    // 老板不受限：两张都在
    let (code, all) = call(&state, "GET", "/api/claims", &boss, None).await;
    assert_eq!(code, StatusCode::OK);
    let n = all.as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(n, 2, "老板应看到两张：{all}");

    // 导出要 Export 权限（会计角色没有），所以这一个人给「应收会计」：
    // 它同时有 Export 与 VoucherNew，能覆盖列表和导出两段代码
    let scoped = scoped_user(
        &state,
        &boss,
        "scp1",
        json!({ "depts": ["生产部"], "account_from": "", "account_to": "",
                           "own_voucher_only": false, "own_doc_only": false }),
        "receivables",
    )
    .await;

    let (code, mine) = call(&state, "GET", "/api/claims", &scoped, None).await;
    assert_eq!(code, StatusCode::OK);
    let rows = mine.as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 1, "只该看到生产部那张：{mine}");
    assert_eq!(rows[0]["id"].as_i64(), Some(prod), "看到的必须是生产部那张");
    assert!(
        rows.iter().all(|r| r["id"].as_i64() != Some(sales)),
        "销售部那张不能出现：{mine}"
    );

    // 导出走的是另一段代码，必须同口径
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/claims", &scoped))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导出报销单");
    let csv = body_string(resp).await;
    assert!(csv.contains("120.00"), "本部门的金额应在导出里：{csv}");
    assert!(
        !csv.contains("340.00"),
        "范围外部门的金额不能出现在导出里：{csv}"
    );
}

/// 部门范围也管**写侧**：看不见的单据不能审批/改/生成凭证
#[tokio::test]
async fn claim_write_endpoints_refuse_other_dept() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;
    let sales = make_claim(&state, &boss, "销售部", "李四", "340.00").await;
    let prod = make_claim(&state, &boss, "生产部", "张三", "120.00").await;

    let scoped = scoped_user(
        &state,
        &boss,
        "scp2",
        json!({ "depts": ["生产部"], "account_from": "", "account_to": "",
                           "own_voucher_only": false, "own_doc_only": false }),
        "accountant",
    )
    .await;

    // 范围外的单子：审批 / 改 / 删 / 生成凭证 全部拒绝
    for (method, uri, body) in [
        (
            "POST",
            format!("/api/claims/{sales}/transition"),
            json!({ "status": "approved" }),
        ),
        (
            "PUT",
            format!("/api/claims/{sales}"),
            json!({ "applicant": "李四", "dept": "销售部", "biz_date": "", "reason": "改一下", "amount": "340.00",
                    "items": [ { "expense_account": "660101", "amount": "340.00", "memo": "改" } ] }),
        ),
        (
            "POST",
            format!("/api/claims/{sales}/voucher"),
            json!({ "pay": false, "pay_account": "" }),
        ),
    ] {
        let (code, v) = call(&state, method, &uri, &scoped, Some(body)).await;
        assert!(
            code == StatusCode::FORBIDDEN || code == StatusCode::NOT_FOUND,
            "{method} {uri} 对范围外部门应当拒绝，实际 {code} {v}"
        );
    }

    // 自己部门的单子照常可用：草稿 → 提交（同一个人、同一个接口，只差部门）
    let (code, v) = call(
        &state,
        "POST",
        &format!("/api/claims/{prod}/transition"),
        &scoped,
        Some(json!({ "status": "submitted" })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "本部门提交应当成功：{v}");
}

/// 写侧越权：`own_voucher_only` 的人不能操作别人的凭证
#[tokio::test]
async fn own_voucher_only_cannot_touch_others_voucher() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;

    // 老板自己填一张并记账
    let (code, dash) = call(&state, "GET", "/api/dashboard", &boss, None).await;
    assert_eq!(code, StatusCode::OK);
    let cur = dash["current_period"].as_str().unwrap().to_string();
    let ymm: i32 = cur.replace('-', "").parse().unwrap();
    let (code, v) = call(
        &state,
        "POST",
        "/api/vouchers",
        &boss,
        Some(json!({
            "id": 0, "period": ymm, "date": format!("{cur}-10"), "word": "记", "no": 1,
            "attachments": 0, "memo": "", "entries": [
                { "line": 1, "account_code": "1001", "summary": "老板填的", "debit": "10", "credit": "0" },
                { "line": 2, "account_code": "600101", "summary": "老板填的", "debit": "0", "credit": "10" }
            ]
        })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "老板建凭证：{v}");
    let vid = v["id"].as_i64().unwrap();

    let scoped = scoped_user(
        &state,
        &boss,
        "scp3",
        json!({ "depts": [], "account_from": "", "account_to": "",
                 "own_voucher_only": true, "own_doc_only": false }),
        "accountant",
    )
    .await;

    // 看不见
    let (code, v) = call(
        &state,
        "GET",
        &format!("/api/vouchers/{vid}"),
        &scoped,
        None,
    )
    .await;
    assert!(
        code == StatusCode::NOT_FOUND || code == StatusCode::FORBIDDEN,
        "别人的凭证不该查得到，实际 {code} {v}"
    );

    // 更重要的：不能改、不能记账、不能作废
    for (method, uri) in [
        ("POST", format!("/api/vouchers/{vid}/post")),
        ("POST", format!("/api/vouchers/{vid}/void")),
        ("POST", format!("/api/vouchers/{vid}/audit")),
    ] {
        let (code, v) = call(&state, method, &uri, &scoped, Some(json!({}))).await;
        assert!(
            code == StatusCode::NOT_FOUND || code == StatusCode::FORBIDDEN,
            "{uri} 对别人的凭证应当拒绝，实际 {code} {v}"
        );
    }

    // 老板自己还能正常作废（闸门不能把主人也拦住）
    let (code, v) = call(
        &state,
        "POST",
        &format!("/api/vouchers/{vid}/void"),
        &boss,
        Some(json!({ "reason": "范围测试" })),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "老板作废自己的凭证：{v}");
}
