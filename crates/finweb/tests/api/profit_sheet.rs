//! 月份利润分析表：照抄厂里那张表的行，按行取数。

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
async fn profit_sheet_rows_then_report_by_account() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // ① 默认行照抄那张表：收入行挂好收入科目，支出行空着
    let v = get_json(&state, &sid, "/api/profit-sheet").await;
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 16, "收入 1 行 + 支出 15 行");
    assert_eq!(rows[0]["label"], "收入：月总产量");
    assert_eq!(rows[0]["kind"], "revenue");
    assert_eq!(rows[0]["accounts"], "6001,6051");
    let rent = rows
        .iter()
        .find(|r| r["label"] == "厂租")
        .expect("厂租这一行");
    assert_eq!(rent["accounts"], "", "支出行默认空着，等按表生成科目");
    assert_eq!(rent["hint"], "含宿租金及房东杂费");

    // ② 一键给没挂科目的行建同名末级科目
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/profit-sheet/create-accounts",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["created"], 15);
    assert_eq!(v["touched"], 15);

    // 回读：每行都挂上了科目
    let v = get_json(&state, &sid, "/api/profit-sheet").await;
    let rows = v["rows"].as_array().unwrap();
    let code_of = |label: &str| {
        rows.iter().find(|r| r["label"] == label).unwrap()["accounts"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let rent_code = code_of("厂租");
    let power_code = code_of("电费");
    assert!(!rent_code.is_empty());
    assert_ne!(rent_code, power_code, "每行一个独立科目，不能共用");

    // ③ 按这些科目记两笔账，看表出数
    let period: i32 = 202601;
    let date = "2026-01-15";
    let vouchers_in: Vec<(&str, serde_json::Value)> = vec![
        (
            "收入",
            serde_json::json!([
                { "line": 1, "account_code": "1001", "summary": "收款", "debit": "10000", "credit": "0" },
                { "line": 2, "account_code": "600101", "summary": "加工费收入", "debit": "0", "credit": "10000" }
            ]),
        ),
        (
            "厂租",
            serde_json::json!([
                { "line": 1, "account_code": rent_code.clone(), "summary": "厂租", "debit": "3000", "credit": "0" },
                { "line": 2, "account_code": "1001", "summary": "厂租", "debit": "0", "credit": "3000" }
            ]),
        ),
        (
            "电费",
            serde_json::json!([
                { "line": 1, "account_code": power_code.clone(), "summary": "电费", "debit": "800", "credit": "0" },
                { "line": 2, "account_code": "1001", "summary": "电费", "debit": "0", "credit": "800" }
            ]),
        ),
    ];
    for (idx, (name, lines)) in vouchers_in.into_iter().enumerate() {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                &sid,
                serde_json::json!({
                    "id": 0, "period": period, "date": date, "word": "记", "no": 11 + idx as i64,
                    "attachments": 0, "memo": "", "entries": lines
                }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "{name} 凭证：{}",
            body_string(resp).await
        );
        let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            ["id"]
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
        assert_eq!(resp.status(), StatusCode::OK, "{name} 记账");
    }

    let v = get_json(&state, &sid, "/api/profit-sheet/report?period=202601").await;
    assert_eq!(num(&v["revenue"]), 10000.0);
    assert_eq!(num(&v["expense"]), 3800.0);
    assert_eq!(num(&v["profit"]), 6200.0);
    assert_eq!(v["unmapped"], 0, "都挂上科目了，不该再有未挂的");
    let line = |label: &str| {
        v["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["label"] == label)
            .unwrap_or_else(|| panic!("没有 {label} 这一行"))
            .clone()
    };
    assert_eq!(num(&line("厂租")["amount"]), 3000.0);
    assert_eq!(num(&line("电费")["amount"]), 800.0);
    assert_eq!(
        num(&line("水费")["amount"]),
        0.0,
        "没记账的行是 0，不是报错"
    );
    // 收入行带月总产量（数量列，不是金额）
    assert!(line("收入：月总产量")["qty"].is_string());

    // ④ 同一科目挂两行要被拦下：合计会翻倍
    let water_id = rows.iter().find(|r| r["label"] == "水费").unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/profit-sheet",
            &sid,
            serde_json::json!({
                "id": water_id, "seq": 5, "label": "水费", "hint": "",
                "kind": "expense", "accounts": rent_code, "disabled": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let msg = body_string(resp).await;
    assert!(msg.contains("合计会翻倍"), "错误要说清后果：{msg}");
}

#[tokio::test]
async fn profit_sheet_auto_map_previews_then_applies() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // ① 先按表建好科目（每行一个同名末级科目）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/profit-sheet/create-accounts",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // ② 把「电费」这一行的科目清掉（模拟：厂里原来把电费记在别的科目上，现在想挂回自己那个）
    let v = get_json(&state, &sid, "/api/profit-sheet").await;
    let rows = v["rows"].as_array().unwrap().clone();
    let elec = rows.iter().find(|r| r["label"] == "电费").unwrap().clone();
    let elec_id = elec["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/profit-sheet",
            &sid,
            serde_json::json!({
                "id": elec_id, "seq": 4, "label": "电费", "hint": "",
                "kind": "expense", "accounts": "", "disabled": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // ③ 预览：只算不写
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/profit-sheet/auto-map",
            &sid,
            serde_json::json!({ "apply": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["applied"], false);
    assert_eq!(v["touched"], 0);
    let auto = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "电费")
        .unwrap()
        .clone();
    let added = auto["added"].as_array().unwrap();
    // 标准科目表里本来就有「510103 水电费」，加上刚建的「电费」，两个都该挂上
    assert!(!added.is_empty(), "电费这一行应该能找到科目：{added:?}");
    let names: Vec<String> = added
        .iter()
        .map(|a| a[1].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        names.iter().any(|n| n.contains("电费")),
        "要挂上名字里带电费的那个：{names:?}"
    );
    let elec_code = added[0][0].as_str().unwrap().to_string();
    assert!(elec_code.starts_with("5101"), "{elec_code}");

    // 预览没写库
    let v = get_json(&state, &sid, "/api/profit-sheet").await;
    let still = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "电费")
        .unwrap()
        .clone();
    assert_eq!(still["accounts"], "", "预览不许改数据");

    // ④ 落地
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/profit-sheet/auto-map",
            &sid,
            serde_json::json!({ "apply": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["applied"], true);
    assert!(v["touched"].as_u64().unwrap() >= 1);

    let v = get_json(&state, &sid, "/api/profit-sheet").await;
    let done = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "电费")
        .unwrap()
        .clone();
    let done_codes: Vec<&str> = done["accounts"].as_str().unwrap().split(',').collect();
    assert!(
        done_codes.contains(&elec_code.as_str()),
        "落地后这一行要挂着 {elec_code}：{done_codes:?}"
    );

    // ⑤ 再跑一次不再改动（幂等）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/profit-sheet/auto-map",
            &sid,
            serde_json::json!({ "apply": true }),
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["touched"], 0, "挂好的行不该再动");
}
