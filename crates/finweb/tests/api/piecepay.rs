//! 计件工资 API：建工艺路线 → 下单 → 派工 → 报工 → 计件汇总 → 回填工资表

use super::*;

fn today_period() -> (i32, String) {
    let p = fincore::Period::from_date(chrono::Local::now().date_naive());
    (p.ymm(), format!("{:04}-{:02}", p.year(), p.month()))
}

#[tokio::test]
async fn piece_work_fills_payroll() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let (ymm, plabel) = today_period();

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
