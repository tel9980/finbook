//! 客供料台账 API：来料→交货→超交拦截→结存→对账单

use super::*;

/// 金额在 JSON 里可能是 "1,000.00" 这种带千分位的字符串，统一转数字再比
fn num(v: &serde_json::Value) -> f64 {
    if let Some(n) = v.as_f64() {
        return n;
    }
    v.as_str()
        .unwrap_or("0")
        .replace(',', "")
        .parse::<f64>()
        .unwrap_or(0.0)
}

fn doc_body(
    customer: &str,
    kind: &str,
    date: &str,
    item: &str,
    qty: &str,
    batch: &str,
) -> serde_json::Value {
    serde_json::json!({
        "customer": customer,
        "kind": kind,
        "date": date,
        "lines": [{
            "item": item,
            "item_name": "客户来料",
            "unit": "件",
            "qty": qty,
            "batch_no": batch,
        }]
    })
}

#[tokio::test]
async fn consign_flow_blocks_over_delivery() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 来料 100 件
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/consign/docs",
            &sid,
            doc_body("C001", "in", "2026-01-05", "KH-MB01", "100", "B1"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "登记来料");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["doc"]["no"].as_str().unwrap(),
        "LR-202601-001",
        "单号按类型+期间"
    );

    // 交货 60：允许
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/consign/docs",
            &sid,
            doc_body("C001", "out", "2026-01-08", "KH-MB01", "60", "B1"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "交货 60");

    // 再交 50：超出（只剩 40）→ 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/consign/docs",
            &sid,
            doc_body("C001", "out", "2026-01-09", "KH-MB01", "50", "B1"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超交要拦下");
    let msg = body_string(resp).await;
    assert!(msg.contains("超出可核销数量"), "实际：{msg}");

    // 结存 40
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/consign/balances?customer=C001", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "一个物料一个批次一行：{rows:?}");
    assert_eq!(money_num(rows[0]["qty"].as_str().unwrap()), 40.0);
    assert_eq!(rows[0]["batch_no"].as_str().unwrap(), "B1");

    // 对账单：来料 100 / 交货 60 / 期末 40
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/consign/statement?customer=C001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let r = &v["rows"][0];
    assert_eq!(money_num(r["inbound"].as_str().unwrap()), 100.0);
    assert_eq!(money_num(r["delivered"].as_str().unwrap()), 60.0);
    assert_eq!(money_num(r["end"].as_str().unwrap()), 40.0);

    // 对账单必须指定客户
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/consign/statement?from=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// 交货单挂销售订单：单号回填、进度表按「订购 − 已交」算未交
#[tokio::test]
async fn consign_delivery_links_sales_order_progress() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 一张销售订单：C001 订 KH-MB01 100 件
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "date": "2026-01-02",
                "customer_code": "C001",
                "customer_name": "客户甲",
                "status": "Confirmed",
                "lines": [{
                    "item_code": "KH-MB01",
                    "item_name": "客户来料",
                    "qty_ordered": "100",
                    "unit_price": "10",
                    "tax_rate": "13",
                }]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let txt = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建销售订单：{txt}");
    let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
    let so_id = v["id"].as_i64().unwrap();

    // 来料 200 → 交货 60（挂订单）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/consign/docs",
            &sid,
            doc_body("C001", "in", "2026-01-05", "KH-MB01", "200", "B1"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "登记来料");

    let mut body = doc_body("C001", "out", "2026-01-08", "KH-MB01", "60", "B1");
    body["so_id"] = serde_json::json!(so_id);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/consign/docs", &sid, body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "交货单挂订单");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let so_no = v["doc"]["so_no"].as_str().unwrap().to_string();
    assert!(!so_no.is_empty(), "交货单要回填订单号快照");

    // 进度：订 100、已交 60、未交 40
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/consign/so-progress?customer=C001", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["so_no"].as_str().unwrap(), so_no);
    assert_eq!(num(&rows[0]["ordered"]), 100.0);
    assert_eq!(num(&rows[0]["delivered"]), 60.0);
    assert_eq!(num(&rows[0]["remain"]), 40.0);

    // 挂别的客户的订单要拦
    let mut bad = doc_body("C001", "out", "2026-01-09", "KH-MB01", "10", "B1");
    bad["so_id"] = serde_json::json!(9999);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/consign/docs", &sid, bad))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "订单不存在要拦");
    let txt = body_string(resp).await;
    assert!(txt.contains("不存在"), "实际：{txt}");
}
