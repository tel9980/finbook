//! 返工件成本单 API：建单挂原单 → 三段成本 → 占比报表 → 结案后不许改

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
async fn rework_order_flow_blocks_over_qty_and_closed_edits() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 一张生产订单，计划 10 件
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "10", "date": "2026-01-05" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建生产订单");
    let po: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po_id = po["id"].as_i64().unwrap();
    let po_no = po["no"].as_str().unwrap().to_string();

    // 建返工单：数量 3（不超过原单 10），界面乱填的编号/物料要被来源订单覆盖
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/rework",
            &sid,
            serde_json::json!({
                "po_id": po_id,
                "date": "2026-01-20",
                "qty": "3",
                "reason": "挂具印痕",
                "po_no": "乱填",
                "item_code": "XXXX",
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建返工单");
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rw_id = v["id"].as_i64().unwrap();
    assert_eq!(v["no"].as_str().unwrap(), "RG-202601-001", "单号按期间");
    assert_eq!(
        v["po_no"].as_str().unwrap(),
        po_no,
        "来源订单编号由系统带出"
    );
    assert_eq!(
        v["item_code"].as_str().unwrap(),
        "140501",
        "物料由来源订单带出"
    );

    // 超过原单计划量 → 400，正文带两个数量
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/rework",
            &sid,
            serde_json::json!({ "po_id": po_id, "date": "2026-01-20", "qty": "11" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超原单量");
    let msg = body_string(resp).await;
    assert!(msg.contains("超过原单计划量"), "实际：{msg}");

    // 没挂来源订单 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/rework",
            &sid,
            serde_json::json!({ "po_id": 0, "qty": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "没挂来源订单");

    // 三段成本：材料 100 / 人工 50 / 制造费用 30
    for (kind, amount) in [("material", "100"), ("labor", "50"), ("overhead", "30")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/rework/{rw_id}/costs"),
                &sid,
                serde_json::json!({ "cost_type": kind, "amount": amount }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "加成本 {kind}");
    }

    // 明细：三段汇总 180
    let d = get_json(&state, &sid, &format!("/api/rework/{rw_id}")).await;
    assert_eq!(num(&d["material"]), 100.0);
    assert_eq!(num(&d["labor"]), 50.0);
    assert_eq!(num(&d["overhead"]), 30.0);
    assert_eq!(num(&d["total"]), 180.0, "三段之和");
    assert_eq!(d["costs"].as_array().unwrap().len(), 3);

    // 成本类型不认识 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/rework/{rw_id}/costs"),
            &sid,
            serde_json::json!({ "cost_type": "travel", "amount": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "成本类型不认识");

    // 列表 + 报表：原单没有归集成本时比例是空串，不炸
    let list = get_json(&state, &sid, "/api/rework?period=202601").await;
    assert_eq!(list["orders"].as_array().unwrap().len(), 1);
    assert!(
        list["kinds"].as_array().unwrap().len() >= 3,
        "三类成本口径给前端"
    );
    let rep = get_json(&state, &sid, "/api/rework/report?period=202601").await;
    let rows = rep["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(num(&rows[0]["total"]), 180.0);
    assert_eq!(
        rows[0]["ratio"].as_str().unwrap(),
        "",
        "原单没成本时比例为空白"
    );
    assert_eq!(num(&rep["total"]), 180.0, "报表合计");

    // 结案之后不许再追加成本
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/rework",
            &sid,
            serde_json::json!({ "id": rw_id, "po_id": po_id, "date": "2026-01-20", "qty": "3", "status": "closed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结案");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/rework/{rw_id}/costs"),
            &sid,
            serde_json::json!({ "cost_type": "labor", "amount": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "结案后加成本");
    let msg = body_string(resp).await;
    assert!(msg.contains("已结案"), "实际：{msg}");

    // 结案的单也不许删
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/rework/{rw_id}"))
                .header(header::COOKIE, &sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "结案后删单");

    // 删掉一条成本，合计跟着减
    let cost_id = d["costs"].as_array().unwrap()[0]["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/rework/costs/{cost_id}"))
                .header(header::COOKIE, &sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // 已结案的连成本也不许删
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "结案后删成本");
}

/// 原因分类 + 责任部门：字典把自由文本规范成分类，汇总表按两者分组且不丢未填的行
#[tokio::test]
async fn rework_analysis_groups_by_reason_and_dept() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 原因分类字典：槽液 SLUG、挂具 GJ
    for (code, name) in [("slug", "槽液"), ("GJ", "挂具")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/rework/reasons",
                &sid,
                serde_json::json!({ "code": code, "name": name }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建原因分类 {code}");
    }
    let dict = get_json(&state, &sid, "/api/rework/reasons").await;
    let codes: Vec<&str> = dict["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["GJ", "SLUG"], "编码统一大写，按停用/编码排序");

    // 一张生产订单，建三张返工单：槽液 300 / 挂具 100 / 谁都没填 50
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "20", "date": "2026-01-05" }),
        ))
        .await
        .unwrap();
    let po: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po_id = po["id"].as_i64().unwrap();

    let mk = |qty: &str, code: &str, dept: &str| {
        serde_json::json!({
            "po_id": po_id,
            "date": "2026-01-20",
            "qty": qty,
            "reason": "细节写在备注里",
            "reason_code": code,
            "dept": dept,
        })
    };
    let mut ids = Vec::new();
    for (body, cost) in [
        (mk("3", "SLUG", "氧化一班"), "300"),
        (mk("2", "GJ", "氧化二班"), "100"),
        (mk("1", "", ""), "50"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post("/api/rework", &sid, body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建返工单");
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        ids.push(v["id"].as_i64().unwrap());
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                format!("/api/rework/{}/costs", v["id"].as_i64().unwrap()).as_str(),
                &sid,
                serde_json::json!({ "cost_type": "labor", "amount": cost }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "追加人工成本");
    }

    let a = get_json(&state, &sid, "/api/rework/analysis?period=202601").await;
    assert_eq!(a["count"].as_i64().unwrap(), 3);
    assert_eq!(num(&a["total"]), 450.0, "300+100+50");

    let by_reason = a["by_reason"].as_array().unwrap();
    assert_eq!(by_reason.len(), 3, "未分类也要占一行");
    assert_eq!(by_reason[0]["label"].as_str().unwrap(), "槽液");
    assert_eq!(num(&by_reason[0]["total"]), 300.0);
    assert_eq!(
        by_reason[0]["share"].as_str().unwrap(),
        "66.67",
        "占比分母是本期合计，不是行内金额"
    );
    let none = by_reason
        .iter()
        .find(|b| b["label"].as_str().unwrap() == "未分类")
        .expect("谁都没填的单据不能被丢掉");
    assert_eq!(num(&none["total"]), 50.0);

    let by_dept = a["by_dept"].as_array().unwrap();
    let sum: f64 = by_dept.iter().map(|b| num(&b["total"])).sum();
    assert!((sum - 450.0).abs() < 0.001, "各分组合计 == 总合计");
    assert!(by_dept
        .iter()
        .any(|b| b["label"].as_str().unwrap() == "未指定"));

    // 报表行带中文分类名
    let rep = get_json(&state, &sid, "/api/rework/report?period=202601").await;
    let labels: Vec<&str> = rep["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reason_label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"槽液"));
    assert!(labels.contains(&"未分类"));

    // 分类被引用就不许删；停用可以，新单不再选它
    let slug_id = dict["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["code"].as_str().unwrap() == "SLUG")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/rework/reasons/{slug_id}"))
                .header(header::COOKIE, &sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "被引用的分类不许删");
    let body = body_string(resp).await;
    assert!(body.contains("先改单"), "要提示怎么办：{body}");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/rework/reasons",
            &sid,
            serde_json::json!({ "id": slug_id, "code": "SLUG", "name": "槽液", "disabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改成停用");
    let dict = get_json(&state, &sid, "/api/rework/reasons").await;
    let slug = dict["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["code"].as_str().unwrap() == "SLUG")
        .unwrap();
    assert_eq!(slug["disabled"].as_bool().unwrap(), true);
}
