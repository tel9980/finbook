//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

/// 资产卡片漏传残值率时按 0（无残值）落库，而不是默默按 5% 记（会低估折旧）。
#[tokio::test]
async fn asset_omitted_residual_rate_defaults_zero() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();

    // 完全不带 residual_rate 字段
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets",
            &sid,
            serde_json::json!({
                "id": 0, "code": "NR0001", "name": "漏传残值率", "category": "电子设备",
                "spec": "", "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
                "expense_account": "660201", "original_value": "12000",
                "life_months": 36, "method": "straight", "start_period": cur_ymm, "memo": ""
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建资产：{b}");
    let aid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 显式传 0 仍合法（0 是合法残值率）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets",
            &sid,
            serde_json::json!({
                "id": 0, "code": "NR0002", "name": "显式零残值", "category": "电子设备",
                "spec": "", "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
                "expense_account": "660201", "original_value": "12000", "residual_rate": "0",
                "life_months": 36, "method": "straight", "start_period": cur_ymm, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "显式 0 残值率应合法");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/assets", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "资产列表：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    let card = r["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_i64() == Some(aid))
        .expect("列表应含新资产");
    assert_eq!(
        money_num(card["residual_rate"].as_str().unwrap()),
        0.0,
        "漏传残值率应落 0 而不是 5：{card}"
    );
}
