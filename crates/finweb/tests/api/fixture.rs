//! 挂具台账 API：购置 → 领用 → 归还 → 报废 → 结存/寿命预警

use super::*;

fn item_body(code: &str, life: i64) -> serde_json::Value {
    serde_json::json!({
        "code": code,
        "name": format!("{code} 号挂具"),
        "tool_kind": "挂具",
        "spec": "600×400",
        "unit": "套",
        "life_times": life,
    })
}

fn move_body(code: &str, kind: &str, qty: &str, date: &str) -> serde_json::Value {
    serde_json::json!({
        "tool_code": code,
        "kind": kind,
        "date": date,
        "qty": qty,
    })
}

fn num(v: &serde_json::Value) -> f64 {
    v.as_str()
        .map(|s| s.replace(',', "").parse::<f64>().unwrap_or(f64::NAN))
        .unwrap_or_else(|| v.as_f64().unwrap_or(f64::NAN))
}

#[tokio::test]
async fn tool_ledger_flow_blocks_bad_moves() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建档案：额定 2 次
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/tools/items", &sid, item_body("GJ01", 2)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建挂具档案");

    // 购置 10 套
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/tools/moves",
            &sid,
            move_body("GJ01", "buy", "10", "2026-01-05"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "购置");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["move"]["no"].as_str().unwrap(),
        "GJ-G-202601-001",
        "单号按类型+期间"
    );
    assert_eq!(num(&v["balance"]["in_stock"]), 10.0, "在库 10");

    // 领用 8 套 → 在库 2、在用 8
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/tools/moves",
            &sid,
            move_body("GJ01", "issue", "8", "2026-01-06"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "领用 8");

    // 再领 5 套：超出在库 2 → 400，正文要说清在库多少
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/tools/moves",
            &sid,
            move_body("GJ01", "issue", "5", "2026-01-07"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "在库不足应被拦");
    let msg = body_string(resp).await;
    assert!(msg.contains("在库 2"), "报错要说清在库多少：{msg}");

    // 归还 8 套（默认按套数记 8 次使用 → 超过额定 2 次）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/tools/moves",
            &sid,
            move_body("GJ01", "back", "8", "2026-01-08"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "归还 8");

    // 报废 3 套：在库 10 → 7
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/tools/moves",
            &sid,
            move_body("GJ01", "scrap", "3", "2026-01-09"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "报废 3");

    // 结存：在库 7、在用 0、报废 3、使用 8 次、需要换新
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/tools", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["tool_code"] == "GJ01")
        .expect("结存里应有 GJ01")
        .clone();
    assert_eq!(num(&row["in_stock"]), 7.0, "在库 7");
    assert_eq!(num(&row["in_use"]), 0.0, "在用 0");
    assert_eq!(num(&row["scraped"]), 3.0, "报废 3");
    assert_eq!(row["used_times"].as_i64().unwrap(), 8, "归还折合 8 次");

    // 寿命用完的清单里有它
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/tools/need-replace", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["rows"].as_array().unwrap().len(),
        1,
        "额定 2 次用了 8 次，应进换新清单"
    );

    // 有流水的档案不能删
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/tools/items/GJ01", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "有流水不能删档案");

    // 不在档案里的挂具录流水 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/tools/moves",
            &sid,
            move_body("GJ99", "buy", "1", "2026-01-10"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "档案不存在应被拦");
    let msg = body_string(resp).await;
    assert!(msg.contains("不在档案里"), "{msg}");
}
