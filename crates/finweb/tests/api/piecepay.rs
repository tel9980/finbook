//! 计件工资 API：建工艺路线 → 下单 → 派工 → 报工 → 计件汇总 → 回填工资表

use super::*;

/// 账套的当前期间（报工流水跟订单期间走，不再是"今天所在期间"）
async fn book_period(state: &Arc<WebState>, sid: &str) -> (i32, String) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pstr = v["period"].as_str().unwrap().to_string(); // "2026-01"
    let y: i32 = pstr[..4].parse().unwrap();
    let m: i32 = pstr[5..7].parse().unwrap();
    (y * 100 + m, pstr)
}

#[tokio::test]
async fn piece_work_fills_payroll() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let (ymm, plabel) = book_period(&state, &sid).await;

    // 工艺路线：140501 一道「氧化」工序
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/routing/140501",
            &sid,
            serde_json::json!([{
                "seq": 1,
                "op_code": "OX",
                "op_name": "氧化",
                "work_center": "",
                "std_hours": "0.5",
                "rate": "0",
                "qc_required": false,
            }]),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建工艺路线");

    // 下达生产订单（按工艺路线生成工序）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "100", "date": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下达生产订单");
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 工序 id
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod/{pid}/ops"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let op_id = v["ops"][0]["id"].as_i64().unwrap();

    // 派工给张三
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/op/dispatch",
            &sid,
            serde_json::json!({ "op_id": op_id, "worker": "张三" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "派工");

    // 计件单价：140501 的氧化工序 1.5 元/件
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/piece-rates",
            &sid,
            serde_json::json!({ "item_code": "140501", "op_name": "氧化", "rate": "1.5" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建计件单价");

    // 报工 100 件
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/op/report",
            &sid,
            serde_json::json!({ "op_id": op_id, "qty": "100", "hours": "2" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "报工");

    // 计件汇总：张三 100 件 × 1.5 = 150
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/payroll/piece?period={plabel}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "一个人一行：{rows:?}");
    assert_eq!(rows[0]["employee"].as_str().unwrap(), "张三");
    assert_eq!(money_num(rows[0]["qty"].as_str().unwrap()), 100.0);
    assert_eq!(money_num(rows[0]["amount"].as_str().unwrap()), 150.0);
    assert_eq!(money_num(v["total"].as_str().unwrap()), 150.0);
    assert_eq!(rows[0]["lines"][0]["matched"].as_bool().unwrap(), true);

    // 回填工资表
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/payroll/piece-fill?period={plabel}"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "回填");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["filled"].as_array().unwrap().len(), 1);
    assert_eq!(money_num(v["filled"][0]["after"].as_str().unwrap()), 150.0);

    // 工资表里应发已经提上来了
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/payroll?period={plabel}"), &sid))
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["employee"] == "张三")
        .expect("工资表里应有张三");
    assert_eq!(money_num(row["gross"].as_str().unwrap()), 150.0);

    // 再回填一次：应发已不低于计件额 → 跳过，不重复加钱
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/payroll/piece-fill?period={plabel}"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["filled"].as_array().unwrap().len(), 0, "幂等：不重复累加");
    assert!(
        body_string(
            handlers::router(state.clone())
                .oneshot(authed_get(&format!("/api/payroll?period={plabel}"), &sid))
                .await
                .unwrap()
        )
        .await
        .contains("\"150"),
        "应发仍是 150"
    );
    let _ = ymm;
}

/// 工艺改版以后，单价要跟着版本走：同一道工序在两个版本上是两个价。
#[tokio::test]
async fn piece_rate_matches_routing_version() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let (ymm, plabel) = book_period(&state, &sid).await;
    let _ = ymm;

    // 工艺路线：140501 的 V2 版，一道「氧化」工序（没有默认版本）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/routing/140501",
            &sid,
            serde_json::json!([{
                "seq": 1,
                "version": "V2",
                "op_code": "OX",
                "op_name": "氧化",
                "work_center": "",
                "std_hours": "0.5",
                "rate": "0",
                "qc_required": false,
            }]),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建 V2 版工艺路线");

    // 下达生产订单：没有默认版本时退到第一个版本，仍要生成工序
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "100", "date": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下达生产订单");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["ops"].as_i64().unwrap_or(0),
        1,
        "按 V2 版工艺生成 1 道工序"
    );
    let pid = v["id"].as_i64().unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod/{pid}/ops"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let op_id = v["ops"][0]["id"].as_i64().unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/op/dispatch",
            &sid,
            serde_json::json!({ "op_id": op_id, "worker": "张三" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "派工");

    // 两条单价：V1 是 1 元、V2 是 2 元
    for (ver, rate) in [("V1", "1"), ("V2", "2")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/piece-rates",
                &sid,
                serde_json::json!({
                    "version": ver, "item_code": "140501", "op_name": "氧化", "rate": rate
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建 {ver} 版计价单价");
    }
    // 列表里能看见版本
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/piece-rates", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let versions: Vec<String> = v["rates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(versions.contains(&"V1".to_string()) && versions.contains(&"V2".to_string()));

    // 报工 100 件：这道工序是 V2 版的，按 2 元算 = 200
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/op/report",
            &sid,
            serde_json::json!({ "op_id": op_id, "qty": "100", "hours": "2" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "报工");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/payroll/piece?period={plabel}"),
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = v["rows"].as_array().unwrap();
    assert_eq!(money_num(rows[0]["amount"].as_str().unwrap()), 200.0, "{v}");
    assert_eq!(rows[0]["lines"][0]["version"].as_str().unwrap(), "V2");
    assert_eq!(
        money_num(rows[0]["lines"][0]["rate"].as_str().unwrap()),
        2.0
    );
}

/// 下单时选了哪一版工艺，订单就要记下来：工序按这一版生成、报工按这一版算钱。
///
/// 这条用例挡的是「订单不记版本」这个退化：工序仍能生成（默认版本或第一个版本），
/// 但订单上没版本，事后重生成工序会退回默认版本，报工记的版本就与当初排产的对不上，
/// 计件工资按错价算而界面上看不出任何异常。
#[tokio::test]
async fn prod_order_records_routing_version() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let (_ymm, plabel) = book_period(&state, &sid).await;

    // 两版工艺：V1 / V2 各一道「氧化」
    for ver in ["V1", "V2"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/routing/140501",
                &sid,
                serde_json::json!([{
                    "seq": 1,
                    "version": ver,
                    "op_code": "OX",
                    "op_name": "氧化",
                    "work_center": "",
                    "std_hours": "0.5",
                    "rate": "0",
                    "qc_required": false,
                }]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建 {ver} 版工艺路线");
    }

    // 版本列表（下拉数据源）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/routing/140501/versions", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let versions: Vec<String> = v["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(versions, vec!["V1".to_string(), "V2".to_string()], "{v}");

    // 按指定版本取工序也要能取（界面里切版本看工序用）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/routing/140501?version=V2", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["ops"].as_array().unwrap().len(), 1, "按 V2 取工序：{v}");

    // 下单并指定按 V2 排产（注意 V1 也存在：不记版本就会退回第一个版本 V1，
    // 后面计件金额会变成 100 而不是 200，这个测试才守得住）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "100", "date": "", "version": "V2" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下达生产订单");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["version"].as_str().unwrap_or(""),
        "V2",
        "响应带回版本：{v}"
    );
    assert_eq!(v["ops"].as_i64().unwrap_or(0), 1, "按 V2 生成 1 道工序");
    let pid = v["id"].as_i64().unwrap();

    // 订单列表里也记着版本（重生成工序要按它来）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let order = v["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(pid))
        .expect("订单在列表里")
        .clone();
    assert_eq!(
        order["routing_version"].as_str().unwrap_or(""),
        "V2",
        "订单必须记下按哪一版排产：{v}"
    );

    // 报工 100 件：工序是 V1 版的，按 V1 的 1 元算 = 100（不是 V2 的 2 元）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod/{pid}/ops"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let op_id = v["ops"][0]["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/op/dispatch",
            &sid,
            serde_json::json!({ "op_id": op_id, "worker": "李四" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "派工");
    for (ver, rate) in [("V1", "1"), ("V2", "2")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/piece-rates",
                &sid,
                serde_json::json!({
                    "version": ver, "item_code": "140501", "op_name": "氧化", "rate": rate
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建 {ver} 版计价单价");
    }
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/op/report",
            &sid,
            serde_json::json!({ "op_id": op_id, "qty": "100", "hours": "2" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "报工");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/payroll/piece?period={plabel}"),
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["employee"].as_str() == Some("李四"))
        .expect("李四的计件行")
        .clone();
    assert_eq!(
        money_num(row["amount"].as_str().unwrap()),
        200.0,
        "按 V2 的 2 元算，不是 V1 的 1 元：{v}"
    );
    assert_eq!(row["lines"][0]["version"].as_str().unwrap_or(""), "V2");
}
