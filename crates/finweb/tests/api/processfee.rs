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

/// 折算系数 + 单价矩阵 + 按口径报价（v49）
///
/// 背景是用户的原话：氧化加工有按件/条/只算的，有按米长、按米重算的，
/// 还有按平方算的 —— 一个物料同时要挂在四五个口径上。
#[tokio::test]
async fn calc_units_drive_matrix_and_conversion() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 300 只折合 7.02 ㎡；1 米型材折合 1.85 kg
    for body in [
        serde_json::json!({ "item_code": "MB01", "calc_unit": "㎡", "factor": "0.0234", "scale": 2 }),
        serde_json::json!({ "item_code": "MB01", "calc_unit": "kg", "factor": "1.85", "scale": 2 }),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post("/api/calc-units", &sid, body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建折算系数");
    }

    // 口径列出来了
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/calc-units?item=MB01", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["units"].as_array().unwrap().len(), 2);

    // 在矩阵里给「MB01 按㎡」填价：12 元/㎡，起步 30
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/price-matrix",
            &sid,
            serde_json::json!({ "item_code": "MB01", "calc_unit": "㎡", "unit_price": "12", "min_charge": "30" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "矩阵改价");

    // 矩阵：一行两列，系数与单价都在
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-matrix", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mx: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = mx["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["item_code"] == "MB01")
        .expect("矩阵里要有 MB01 这一行");
    let area = row["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["unit"] == "㎡")
        .unwrap();
    assert_eq!(money_num(area["unit_price"].as_str().unwrap()), 12.0);
    assert_eq!(money_num(area["factor"].as_str().unwrap()), 0.0234);
    let mass = row["cells"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["unit"] == "kg")
        .unwrap();
    assert_eq!(money_num(mass["factor"].as_str().unwrap()), 1.85);
    assert_eq!(
        mass["rule_id"].as_i64().unwrap(),
        0,
        "没定价的格子 rule_id=0"
    );

    // 300 只 → 7.02 ㎡ × 12 = 84.24（不触起步价）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=300&calc_unit=%E3%8E%A1",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["conv"]["calc_qty"].as_str().unwrap()), 7.02);
    assert_eq!(money_num(v["conv"]["factor"].as_str().unwrap()), 0.0234);
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 84.24);

    // 按 kg 报没定价 → 400，并且说清这一格是空的（不是「没有规则」这种含糊话）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=300&calc_unit=kg",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("还是空的"));

    // 没定价的口径不能拿别的口径的价格顶上（12 元/㎡ 乘 300 kg 是几百倍错账）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=300&calc_unit=kg",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // 老接口（直接给计价数量）照旧能用
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&qty=7.02",
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 84.24);

    // 删掉系数后，按㎡ 报价要说「还没设折算系数」
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/calc-units?item=MB01", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let area_id = v["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["calc_unit"] == "㎡")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/calc-units/{area_id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删系数");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=300&calc_unit=%E3%8E%A1",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("折算系数"));
}

/// 提前录下一版价：生效日没到之前不能顶掉现在的价，日期写错要当场拦
#[tokio::test]
async fn matrix_cell_carries_effective_date() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/calc-units",
            &sid,
            serde_json::json!({ "item_code": "MB01", "calc_unit": "㎡", "factor": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 现在的价 10，3 月起的新价 12（提前录）
    for (price, from) in [("10", "2020-01-01"), ("12", "2999-03-01")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_put(
                "/api/price-matrix",
                &sid,
                serde_json::json!({
                    "item_code": "MB01", "calc_unit": "㎡",
                    "unit_price": price, "effective_from": from,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "改价 {price}");
    }

    // 矩阵里显示的是「现在生效的那条」，未来价单独标出来
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/price-matrix", &sid))
        .await
        .unwrap();
    let mx: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cell = &mx["rows"].as_array().unwrap()[0]["cells"]
        .as_array()
        .unwrap()[0];
    assert_eq!(money_num(cell["unit_price"].as_str().unwrap()), 10.0);

    // 报价也用现在这条
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/price-rules/quote?item=MB01&stock_qty=1&calc_unit=%E3%8E%A1",
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(v["quote"]["total"].as_str().unwrap()), 10.0);
    assert_eq!(v["quote"]["future"].as_bool().unwrap(), false);
    assert_eq!(v["quote"]["effective_from"].as_str().unwrap(), "2020-01-01");

    // 日期写错（2026-3-1）→ 400，理由说的是格式
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/price-matrix",
            &sid,
            serde_json::json!({
                "item_code": "MB01", "calc_unit": "㎡",
                "unit_price": "13", "effective_from": "2026-3-1",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(body_string(resp).await.contains("2026-03-01"));
}
