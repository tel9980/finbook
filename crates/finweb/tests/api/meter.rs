//! 电费台账 API：充值 → 抄表登记 → 结转出凭证。
//!
//! 守的是「充值不是费用」这条线：充值只动 112301，抄表只登记，
//! 结转才进 510103 并按税率拆进项；结转后台账行不许再删。

use super::*;

fn num(v: &serde_json::Value) -> f64 {
    v.as_str()
        .map(|s| s.replace(',', "").parse::<f64>().unwrap_or(f64::NAN))
        .unwrap_or_else(|| v.as_f64().unwrap_or(f64::NAN))
}

async fn get_json(state: &Arc<WebState>, sid: &str, path: &str) -> serde_json::Value {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(path, sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET {path}");
    serde_json::from_str(&body_string(resp).await).unwrap()
}

#[tokio::test]
async fn meter_book_flow_recharge_read_close() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    // 当期：形如 "2026-01"
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let month = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = month.replace('-', "").parse().unwrap();
    let d20 = format!("{month}-20");
    let d28 = format!("{month}-28");

    // 建「112301 预付电费」明细（默认科目表只有 1123 预付账款，非末级不许直接记账）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/accounts",
            &sid,
            serde_json::json!({
                "account": {
                    "code": "112301", "name": "预付电费", "category": "asset", "dir": "debit",
                    "aux": 0, "unit": null, "currency": null, "has_qty": false,
                    "is_cash": false, "is_bank": false, "cash_flow_item": null,
                    "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
                },
                "aux_kinds": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建预付电费明细");

    // 充值 20,000：借 112301 / 贷 1001（充值只动预付卡，不进费用）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d20, "word": "记",
                "no": 120, "attachments": 0, "memo": "电卡充值", "entries": [
                    { "line": 1, "account_code": "112301", "summary": "电卡充值", "debit": "20000", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "电卡充值", "debit": "0", "credit": "20000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "充值凭证");
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "充值凭证记账");

    // 抄表两行：电 15,000 度 × 1.2（13%）+ 水 10 吨 × 100（9%）
    for (kind, no, qty, price, rate) in [
        ("electric", "DB-01", "15000", "1.2", "0.13"),
        ("water", "SB-01", "10", "100", "0.09"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/meter",
                &sid,
                serde_json::json!({
                    "kind": kind, "meter_no": no, "date": d28,
                    "qty": qty, "unit_price": price, "rate": rate, "period": month
                }),
            ))
            .await
            .unwrap();
        let status = resp.status();
        let body = body_string(resp).await;
        assert_eq!(status, StatusCode::OK, "登记抄表 {no}: {body}");
    }

    // 概览：充值 20,000、已结转 0、卡里剩 20,000、待结转 19,000
    let d = get_json(&state, &sid, &format!("/api/meter?period={month}")).await;
    assert_eq!(num(&d["summary"]["recharged"]), 20000.0);
    assert_eq!(num(&d["summary"]["consumed"]), 0.0);
    assert_eq!(num(&d["summary"]["balance"]), 20000.0);
    assert_eq!(num(&d["summary"]["unclosed"]), 19000.0);
    assert_eq!(d["summary"]["unclosed_count"].as_i64().unwrap(), 2);
    assert_eq!(
        num(&d["summary"]["unclosed_tax"]),
        2153.37,
        "2070.80 + 82.57"
    );

    // 结转：借 510103 16,846.63 / 借 22210101 2,153.37 / 贷 112301 19,000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/meter/close",
            &sid,
            serde_json::json!({ "period": month, "date": d28 }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "结转: {body}");
    let close: serde_json::Value = serde_json::from_str(&body).unwrap();
    let cvid = close["voucher_id"].as_i64().unwrap();
    assert!(cvid > 0, "应当生成了结转凭证");

    // 结转后：余额 1,000、待结转 0
    let d = get_json(&state, &sid, &format!("/api/meter?period={month}")).await;
    assert_eq!(num(&d["summary"]["consumed"]), 19000.0);
    assert_eq!(num(&d["summary"]["balance"]), 1000.0);
    assert_eq!(num(&d["summary"]["unclosed"]), 0.0);
    assert_eq!(d["summary"]["unclosed_count"].as_i64().unwrap(), 0);
    assert!(
        d["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["closed"] == true),
        "两行都该标记为已结转"
    );

    // 重复结转不该再造凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/meter/close",
            &sid,
            serde_json::json!({ "period": month, "date": d28 }),
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    let again: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        again["voucher_id"].as_i64().unwrap(),
        0,
        "没有待结转行：{body}"
    );

    // 已结转的行不许删
    let rid = d["items"][0]["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/meter/{rid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已结转的行不能删");
}
