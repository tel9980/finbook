//! 加工费规则 API：按面积计价 + 最低收费 + 加急费 + 规则校验

use super::*;

#[tokio::test]
async fn price_rule_quote_area_floor_and_rush() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 氧化厂典型规则：18 元/㎡，起步 30 元，加急 20 元 + 10%
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({
                "name": "氧化-普通",
                "unit": "㎡",
                "unit_price": "18",
                "min_charge": "30",
                "rush_fee": "20",
                "rush_rate": "10",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建规则");

    // 12.5 ㎡ × 18 = 225，不触起步价
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?qty=12.5", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["base"].as_str().unwrap()), 225.00);
    assert_eq!(money_num(v["quote"]["floor_extra"].as_str().unwrap()), 0.00);
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 225.00);

    // 0.5 ㎡ × 18 = 9 → 补到起步价 30
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?qty=0.5", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["base"].as_str().unwrap()), 9.00);
    assert_eq!(
        money_num(v["quote"]["floor_extra"].as_str().unwrap()),
        21.00
    );
    assert_eq!(
        v["quote"]["total"].as_str().unwrap(),
        "30.00",
        "最低收费兜底"
    );

    // 加急：225 + 22.5(10%) + 20 = 267.5
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?qty=12.5&rush=1", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["rush_extra"].as_str().unwrap()), 42.50);
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 267.50);

    // 数量为 0 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules/quote?qty=0", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 加急率超过 100% → 400（不是静默按 100 算）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            serde_json::json!({ "name": "乱填", "unit": "件", "unit_price": "1", "rush_rate": "120" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("0-100"));

    // 规则列表带出刚建的那条
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-rules", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["rules"].as_array().unwrap().len(), 1);
    assert_eq!(v["rules"][0]["name"].as_str().unwrap(), "氧化-普通");
}
