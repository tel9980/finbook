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
