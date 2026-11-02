//! 收款认领 API：一笔钱认给两个客户。
//!
//! 守的是「收款不一一对应」这条线：C01 名下的一笔 45,000 里，15,000 是替 C02 付的。
//! 认领后 C02 的单要清掉、过渡凭证要出、并且**这笔收款必须认完**；
//! 而 C01 那张 45,000 的单只清了 30,000 —— 剩下 15,000 是真没收到，不能凭空消掉。

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

/// 记一张已记账凭证（借贷两行），返回凭证 id
async fn post_voucher(
    state: &Arc<WebState>,
    sid: &str,
    ymm: i32,
    date: &str,
    no: i32,
    lines: serde_json::Value,
) -> i64 {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            sid,
            serde_json::json!({
                "id": 0, "period": ymm, "date": date, "word": "记",
                "no": no, "attachments": 0, "memo": "认领测试", "entries": lines
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建凭证 {no}");
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账 {no}");
    vid
}

#[tokio::test]
async fn claim_one_receipt_across_two_customers() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let dash = get_json(&state, &sid, "/api/dashboard").await;
    let month = dash["current_period"].as_str().unwrap().to_string();
    let ymm: i32 = month.replace('-', "").parse().unwrap();
    let d05 = format!("{month}-05");
    let d06 = format!("{month}-06");
    let d20 = format!("{month}-20");

    // 两张应收：C01 45,000、C02 15,000（收入直接进 600101，简化）
    post_voucher(
        &state,
        &sid,
        ymm,
        &d05,
        101,
        serde_json::json!([
            { "line": 1, "account_code": "112201", "summary": "加工费", "debit": "45000", "credit": "0", "aux": { "customer": "C01" } },
            { "line": 2, "account_code": "600101", "summary": "加工费", "debit": "0", "credit": "45000" }
        ]),
    )
    .await;
    post_voucher(
        &state,
        &sid,
        ymm,
        &d06,
        102,
        serde_json::json!([
            { "line": 1, "account_code": "112201", "summary": "加工费", "debit": "15000", "credit": "0", "aux": { "customer": "C02" } },
            { "line": 2, "account_code": "600101", "summary": "加工费", "debit": "0", "credit": "15000" }
        ]),
    )
    .await;
    // 收款 45,000 全记在 C01 名下（现实里会计只知道钱到账了）
    post_voucher(
        &state,
        &sid,
        ymm,
        &d20,
        103,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "收款", "debit": "45000", "credit": "0" },
            { "line": 2, "account_code": "112201", "summary": "收款", "debit": "0", "credit": "45000", "aux": { "customer": "C01" } }
        ]),
    )
    .await;

    // 认领工作台：一笔待认收款 + 两张挂着未收的单
    let d = get_json(
        &state,
        &sid,
        &format!("/api/claim?account=112201&period={ymm}"),
    )
    .await;
    let inflows = d["inflows"].as_array().unwrap();
    assert_eq!(inflows.len(), 1, "只有一笔待认领的收款：{d}");
    assert_eq!(inflows[0]["party"], "C01");
    let inflow = inflows[0]["entry"].as_i64().unwrap();
    assert_eq!(num(&inflows[0]["open"]), 45000.0);
    let cands = d["candidates"].as_array().unwrap();
    assert_eq!(cands.len(), 2, "两张单都还挂着");
    let c01 = cands.iter().find(|c| c["party"] == "C01").unwrap()["entry"]
        .as_i64()
        .unwrap();
    let c02 = cands.iter().find(|c| c["party"] == "C02").unwrap()["entry"]
        .as_i64()
        .unwrap();
    // 建议先清同一客户：C01 整笔 45,000
    let picks = d["suggestions"][0]["picks"].as_array().unwrap();
    assert_eq!(picks.len(), 1);
    assert_eq!(num(&picks[0]["amount"]), 45000.0);

    // 会计说：其中 15,000 是 C02 的 → 30,000 认 C01、15,000 认 C02
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claim",
            &sid,
            serde_json::json!({
                "account": "112201", "inflow": inflow,
                "picks": [ { "entry": c01, "amount": "30000" }, { "entry": c02, "amount": "15000" } ],
                "date": d20
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "认领");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(num(&r["amount"]), 45000.0);
    assert_eq!(
        r["bridges"].as_array().unwrap().len(),
        1,
        "跨客户那条要出过渡凭证"
    );
    assert_eq!(num(&r["leftover"]), 0.0, "这笔收款认完了");

    // 认领后再看：收款没了；C02 清干净；C01 还挂着 15,000（真没收到）
    let d2 = get_json(
        &state,
        &sid,
        &format!("/api/claim?account=112201&period={ymm}"),
    )
    .await;
    assert!(d2["inflows"].as_array().unwrap().is_empty(), "收款认完了");
    let left = d2["candidates"].as_array().unwrap();
    assert_eq!(left.len(), 1, "只剩 C01：{d2}");
    assert_eq!(left[0]["party"], "C01");
    assert_eq!(num(&left[0]["open"]), 15000.0);

    // 超额认领会给 400，且要说清超了多少
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claim",
            &sid,
            serde_json::json!({
                "account": "112201", "inflow": inflow,
                "picks": [ { "entry": c01, "amount": "1000" } ], "date": d20
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "这笔收款已认完，不能再认"
    );
    let txt = body_string(resp).await;
    assert!(
        txt.contains("认完") || txt.contains("超过"),
        "要说清为什么认不了：{txt}"
    );

    // 数据范围：看不见的科目按「没这个科目」挡掉，不返回空清单
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claim?account=999999&period=202601", &sid))
        .await
        .unwrap();
    assert!(
        resp.status() == StatusCode::FORBIDDEN || resp.status() == StatusCode::BAD_REQUEST,
        "看不见的科目不该回空清单，实际 {}",
        resp.status()
    );
}
