//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

/// P1：MRP 采购建议下推请购——仅采购行可推、幂等、生产行拒绝。
#[tokio::test]
async fn mrp_purchase_push() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 无 BOM 物料 → 采购建议
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mrp/run",
            &sid,
            serde_json::json!({ "demands": [{ "item_code": "140301", "qty": "6", "source": "手工" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "MRP 运行");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_code"] == "140301")
        .expect("140301 行");
    assert_eq!(row["action"], "purchase", "无 BOM 应为采购建议：{r}");
    let rid = row["id"].as_i64().unwrap();

    // 下推请购 → 200 + 单号
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mrp/{rid}/to-req"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "下推请购：{b}");
    let pr: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(pr["req_id"].as_i64().unwrap() > 0, "应返回请购单 id：{pr}");
    let no = pr["no"].as_str().unwrap().to_string();
    assert!(!no.is_empty(), "应返回请购单号");

    // 重复下推 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mrp/{rid}/to-req"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复下推应 400");

    // 未知 id → 404
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mrp/999999/to-req",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "未知 MRP 行应 404");

    // 建 BOM 后：140501=生产建议，生产行下推请购 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建 BOM");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mrp/run",
            &sid,
            serde_json::json!({ "demands": [{ "item_code": "140501", "qty": "2", "source": "手工" }] }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let prod = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_code"] == "140501")
        .expect("140501 行");
    assert_eq!(prod["action"], "produce", "有 BOM 应为生产建议：{r}");
    let pid = prod["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mrp/{pid}/to-req"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "生产建议不可下推请购"
    );
}

/// P1：仓库主数据——默认仓、CRUD 守卫、出入库带仓/非法仓拒绝、默认仓兜底。
#[tokio::test]
async fn warehouse_master_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d13 = format!("{}-13", dash["current_period"].as_str().unwrap());

    // 默认仓已种（01 主仓）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/warehouses", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let def = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["code"] == "01")
        .expect("应有默认仓 01");
    assert_eq!(def["is_default"], true);
    assert_eq!(def["name"], "主仓");

    // 新增 02 成品仓 → upsert 改名
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/warehouses",
            &sid,
            serde_json::json!({ "code": "02", "name": "成品仓" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增仓库 02");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/warehouses",
            &sid,
            serde_json::json!({ "code": "02", "name": "成品仓A", "is_default": false, "disabled": false, "memo": "m" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改名 upsert");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/warehouses", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["code"] == "02" && x["name"] == "成品仓A"),
        "名称应已更新：{r}"
    );

    // 采购 5@9：到货指定 02 → 分仓库存可见 02
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d13, "supplier_code": "S01", "supplier_name": "供应商甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "5", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "5", "warehouse": "02", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货指定仓库 02");
    let wh_qty = |state: Arc<WebState>, sid: String| async move {
        let resp = handlers::router(state)
            .oneshot(authed_get(
                "/api/inventory/warehouse-stock?item=140301",
                &sid,
            ))
            .await
            .unwrap();
        let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                (
                    x["warehouse"].as_str().unwrap().to_string(),
                    x["qty"]
                        .as_str()
                        .unwrap()
                        .replace(',', "")
                        .parse::<f64>()
                        .unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };
    let rows = wh_qty(state.clone(), sid.clone()).await;
    assert!(
        rows.iter()
            .any(|(w, q)| w == "02" && (*q - 5.0).abs() < 0.005),
        "02 仓应有 5：{rows:?}"
    );

    // 不填仓库 → 默认仓 01 兜底
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "1", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "默认仓到货");
    let rows = wh_qty(state.clone(), sid.clone()).await;
    assert!(
        rows.iter()
            .any(|(w, q)| w == "01" && (*q - 1.0).abs() < 0.005),
        "默认仓 01 应有 1：{rows:?}"
    );

    // 非法仓 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "1", "warehouse": "ZZ" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不存在仓库应 400");

    // 停用后写入 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/warehouses",
            &sid,
            serde_json::json!({ "code": "02", "name": "成品仓A", "disabled": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "停用 02");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "1", "warehouse": "02" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "停用仓应 400");

    // 删除守卫：默认仓 400；被引用仓 400；未引用仓 200
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/warehouses/01", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "默认仓不可删");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/warehouses/02", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "被引用仓不可删");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/warehouses",
            &sid,
            serde_json::json!({ "code": "03", "name": "临时仓" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增 03");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/warehouses/03", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "未引用仓可删");
}

/// P2：制造成本 Web 化——WIP / 差异 / 预测 / 制造费用分摊（试算与应用）。
#[tokio::test]
async fn cost_web_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 标准价 + BOM + 订单 → 开工 → 领料（材料成本 5×2×10 = 100）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "标准价");
    // 存货档案参考成本（BOM 成本汇总口径：props.ref_cost）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "item", "code": "140301", "name": "原料",
                "parent_code": null, "disabled": false, "props": { "ref_cost": "10" }, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "存货参考成本");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "BOM");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "5", "date": d15 }),
        ))
        .await
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/issue"),
            &sid,
            serde_json::json!({ "date": d15 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "领料");

    // WIP：材料 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/cost/wip?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let w = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["po_id"].as_i64() == Some(pid))
        .expect("WIP 应含该订单");
    assert!(
        (w["material"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 100.0)
            .abs()
            < 0.005,
        "WIP 材料 100：{w}"
    );

    // 差异 + 预测
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/cost/variance?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["po_id"].as_i64() == Some(pid)),
        "差异应含该订单：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/cost/forecast?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let f = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["po_id"].as_i64() == Some(pid))
        .expect("预测应含该订单");
    assert!(
        (f["forecast"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 100.0)
            .abs()
            < 0.005,
        "预测料本 100：{f}"
    );

    // 制造费用分摊：试算 50（按成本占比，唯一订单全额）→ 应用 → WIP overhead 50
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/overhead",
            &sid,
            serde_json::json!({ "period": cur_ymm, "amount": "50", "base": "cost", "apply": false }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["applied"], false);
    assert_eq!(r["rows"].as_array().unwrap().len(), 1, "试算 1 单：{r}");
    assert!(
        (r["rows"][0]["amount"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 50.0)
            .abs()
            < 0.005,
        "试算 50：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/overhead",
            &sid,
            serde_json::json!({ "period": cur_ymm, "amount": "50", "base": "cost", "apply": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "应用分摊");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/cost/wip?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let w = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["po_id"].as_i64() == Some(pid))
        .unwrap();
    assert!(
        (w["overhead"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 50.0)
            .abs()
            < 0.005,
        "应用后 WIP 制造费用 50：{w}"
    );
    // 金额 0 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/overhead",
            &sid,
            serde_json::json!({ "period": cur_ymm, "amount": "0", "base": "cost", "apply": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "分摊金额 0 应 400");
}

/// 标准成本差异端点：价格差异可正可负，且必须能追到构成流水
#[tokio::test]
async fn cost_variance_endpoint() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 配一个标准成本存货：入库价 9，标准 7
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "standard", "standard_cost": "7" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "配置标准成本应成功");

    // 录一张采购入库 100 × 9。
    // 出入库流水是**凭证/采购入库驱动**的（没有直接的 HTTP 库存入库端点），
    // 所以这里用引擎造数，端点只负责读+算——这正是它要验的东西。
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("100").unwrap();
        let p = fincore::Money::parse("9").unwrap();
        findb::business::stock_insert(
            &db,
            &findb::business::StockMove {
                id: 0,
                period: fincore::Period::new(2026, 1).unwrap(),
                biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: String::new(),
                batch_no: String::new(),
                qty: q,
                price: p,
                amount: (q * p).round2(),
                voucher_id: None,
                memo: "买贵了".into(),
            },
        )
        .unwrap();
    }

    // 出差异表：买贵 2 元 × 100 = 200
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/cost/material-variance?period=2026-01",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let row = v["rows"]
        .as_array()
        .expect("rows 应是数组")
        .iter()
        .find(|x| x["item"] == "140301")
        .expect("应有 140301 的差异行");
    assert_eq!(row["actual_unit"], serde_json::json!("9.00"));
    assert_eq!(
        row["price_variance"],
        serde_json::json!("200.00"),
        "买贵 2×100=200"
    );
    assert_eq!(v["total_price"], serde_json::json!("200.00"));
    // 差异必须能追到单据，否则「超支 200」不是可行动的信息
    let ids = row["move_ids"].as_array().expect("应带 move_ids");
    assert!(!ids.is_empty(), "差异行必须带构成流水 id");
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/inventory/move/{}", ids[0].as_i64().unwrap()),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let m: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(m["item"], serde_json::json!("140301"));
    assert_eq!(m["kind_label"], serde_json::json!("采购入库"));

    // 响应里必须声明「算了但没算的」差异项
    let missing: Vec<&str> = v["missing"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x.as_str())
        .collect();
    for want in ["用量差异", "效率差异", "固定制造费用差异"] {
        assert!(
            missing.iter().any(|x| x.contains(want)),
            "必须点名没算的差异：{want}；实际 {missing:?}"
        );
    }
}

/// 链5：批次盘点（批次账面快照/差异流水带批次/盘盈建档）→ 批次成本勾稽 → 批次调拨（双流水+主仓改写+FEFO）。
#[tokio::test]
async fn chain5_batch_stock() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 仓库主数据（v30）：调拨用的 W01/W02 先建档
    for (code, name) in [("W01", "一号仓"), ("W02", "二号仓")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/warehouses",
                &sid,
                serde_json::json!({ "code": code, "name": name }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建仓 {code}");
    }

    // 造批次：W01 入 10（BT500）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batch",
            &sid,
            serde_json::json!({
                "item": "RM10", "batch_no": "BT500", "production_date": "",
                "warehouse": "W01", "location": "", "qty": "10",
                "direction": "in", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "批次建档");

    // ---- 5a 批次盘点：行带 batch_no → 服务端按批次快照账面（W01=10）----
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/count",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "warehouse": "W01",
                "memo": "批次盘点造数",
                "lines": [{ "item": "RM10", "batch_no": "BT500", "count_qty": "8" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建盘点单");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_i64() == Some(cid))
        .unwrap()
        .clone();
    assert_eq!(doc["lines"][0]["batch_no"], "BT500", "行带批次号");
    assert_eq!(
        doc["lines"][0]["book_qty"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap(),
        10.0,
        "批次账面快照 = W01 分仓余额 10：{doc}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid}/apply"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "应用盘点");
    // 批次余额 = 8（差异流水带 batch_no）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches?item=RM10", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bt = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["batch_no"] == "BT500")
        .unwrap()
        .clone();
    assert_eq!(
        bt["balance"].as_str().unwrap().parse::<f64>().unwrap(),
        8.0,
        "盘点后批次余额 8：{bt}"
    );

    // ---- 5b 批次成本勾稽：明细 + 逐存货合计 ----
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batch-cost", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "批次成本勾稽可用");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["detail"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["item"] == "RM10" && d["batch_no"] == "BT500"),
        "明细含该批次：{r}"
    );
    let t = r["totals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["item"] == "RM10")
        .unwrap()
        .clone();
    assert_eq!(
        t["qty"].as_str().unwrap().parse::<f64>().unwrap(),
        8.0,
        "Σ批次数量 = 8：{t}"
    );
    assert!(
        t.get("book").is_some() && t.get("diff").is_some(),
        "账面与差异列"
    );

    // ---- 5c 批次调拨：W01→W02 双流水 + 主仓改写；超额 400；FEFO 自动选批 ----
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/transfer",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(),
                "item": "RM10", "batch_no": "BT500",
                "from_warehouse": "W01", "to_warehouse": "W02",
                "qty": "3", "memo": "库间调拨"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "调拨执行");
    let tr: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(tr["batch_no"], "BT500");
    assert!(
        tr["out_id"].as_i64().is_some() && tr["in_id"].as_i64().is_some(),
        "两条流水 id"
    );

    // 调拨报表：期间内 Transfer 行含 -3(W01) 与 +3(W02)
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/transfer", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    let out = rows.iter().find(|m| {
        m["warehouse"] == "W01"
            && m["batch_no"] == "BT500"
            && m["qty"].as_str().map(|q| q.starts_with('-')) == Some(true)
    });
    let inn = rows.iter().find(|m| {
        m["warehouse"] == "W02"
            && m["batch_no"] == "BT500"
            && m["qty"].as_str().map(|q| !q.starts_with('-')) == Some(true)
            && m["qty"].as_str() != Some("0")
    });
    assert!(out.is_some(), "调出行（W01 负数）应在报表：{r}");
    assert!(inn.is_some(), "调入行（W02 正数）应在报表：{r}");

    // 批次主仓标签改写 = W02，总量仍 8
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches?item=RM10", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bt = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["batch_no"] == "BT500")
        .unwrap()
        .clone();
    assert_eq!(bt["warehouse"], "W02", "主仓标签改写：{bt}");
    assert_eq!(
        bt["balance"].as_str().unwrap().parse::<f64>().unwrap(),
        8.0,
        "调拨不改变总量"
    );

    // 超额（分仓余额不足）→ 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/transfer",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(),
                "item": "RM10", "batch_no": "BT500",
                "from_warehouse": "W01", "to_warehouse": "W02",
                "qty": "100", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "W01 余额 5 < 100 应拒"
    );

    // FEFO 自动选批（batch_no 空）：W02→W01 ×2，返回选中批号
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/transfer",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(),
                "item": "RM10", "batch_no": "",
                "from_warehouse": "W02", "to_warehouse": "W01",
                "qty": "2", "memo": "FEFO 自动"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "FEFO 自动选批");
    let tr: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(tr["batch_no"], "BT500", "唯一可用批次被 FEFO 选中");
}

/// 来料检验状态机（对标金蝶质检管理）：qc_required 存货到货入待检 →
/// 合格转正/不合格隔离（部分不合格拆分）→ 可用/待检/隔离三口径；非检验存货直通。
#[tokio::test]
async fn qc_state_machine() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

    // 建检验存货并启用来料检验
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({ "id": 0, "kind": "item", "code": "RM50", "name": "待检料", "disabled": false, "memo": "", "props": { "qc_required": "1" } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建存货档案");

    let mk_po = |qty: String, price: String| {
        let st = state.clone();
        let sd = sid.clone();
        let dd = d15.clone();
        async move {
            let resp = handlers::router(st)
                .oneshot(authed_post(
                    "/api/procure/po",
                    &sd,
                    serde_json::json!({
                        "period": cur_ymm, "date": dd, "supplier_code": "S01",
                        "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                        "lines": [{ "item_code": "RM50", "qty_ordered": qty, "unit_price": price, "tax_rate": "0" }]
                    }),
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "建采购订单");
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
                .as_i64()
                .unwrap()
        }
    };
    // PO1：到货10 → 待检（available=0, pending=10）
    let po1 = mk_po("10".to_string(), "5".to_string()).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po1, "period": cur_ymm, "date": d15.clone(), "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["qc_pending"],
        serde_json::json!(true),
        "检验存货到货应标记待检：{r}"
    );
    let get = |state: Arc<WebState>, sid: String, item: String| async move {
        let resp = handlers::router(state)
            .oneshot(authed_get(
                &format!("/api/inventory/warehouse-stock?item={item}"),
                &sid,
            ))
            .await
            .unwrap();
        body_string(resp).await
    };
    let b = get(state.clone(), sid.clone(), "RM50".to_string()).await;
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        money_num(v["rows"][0]["qty"].as_str().unwrap()),
        10.0,
        "结存10"
    );
    assert_eq!(
        money_num(v["rows"][0]["available"].as_str().unwrap()),
        0.0,
        "待检期可用为0"
    );
    assert_eq!(
        money_num(v["rows"][0]["pending"].as_str().unwrap()),
        10.0,
        "待检10"
    );

    // 质检部分不合格3 → 合格7转正 + 隔离3（拆分）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/qc",
            &sid,
            serde_json::json!({ "po_id": po1, "qty_insp": "10", "qty_fail": "3", "inspector": "质检员", "date": d15.clone(), "memo": "外观不良" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "质检保存");
    let b = get(state.clone(), sid.clone(), "RM50".to_string()).await;
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        money_num(v["rows"][0]["available"].as_str().unwrap()),
        7.0,
        "合格7转正"
    );
    assert_eq!(
        money_num(v["rows"][0]["quarantine"].as_str().unwrap()),
        3.0,
        "不合格3隔离"
    );
    assert_eq!(
        money_num(v["rows"][0]["pending"].as_str().unwrap()),
        0.0,
        "待检清零"
    );

    // PO2：到货5 → 全不合格 → 隔离+5、待检清零
    let po2 = mk_po("5".to_string(), "5".to_string()).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po2, "period": cur_ymm, "date": d15.clone(), "qty": "5", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/qc",
            &sid,
            serde_json::json!({ "po_id": po2, "qty_insp": "5", "qty_fail": "5", "date": d15.clone(), "memo": "整批不良" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "全不合格质检");
    let b = get(state.clone(), sid.clone(), "RM50".to_string()).await;
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        money_num(v["rows"][0]["quarantine"].as_str().unwrap()),
        8.0,
        "隔离累计8"
    );
    assert_eq!(
        money_num(v["rows"][0]["pending"].as_str().unwrap()),
        0.0,
        "无待检余量"
    );

    // 非检验存货（140301 未勾检验）→ 到货直通可用
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "6", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po3 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po3, "period": cur_ymm, "date": d15.clone(), "qty": "6", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["qc_pending"], serde_json::json!(false), "未勾检验直通");
    let b = get(state.clone(), sid.clone(), "140301".to_string()).await;
    let v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        money_num(v["rows"][0]["available"].as_str().unwrap()),
        6.0,
        "直通可用=结存"
    );
}

/// 批次库位流程：登记（自动批号/流水带批）→ 余额 → FEFO → 库位 CRUD。
#[tokio::test]
async fn stock_batch_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 入库：批号自动生成
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batch",
            &sid,
            serde_json::json!({
                "item": "RM10", "batch_no": "", "production_date": "2026-01-10",
                "qty": "50", "direction": "in"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "批次入库应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let auto_no = r["batch_no"].as_str().unwrap().to_string();
    assert!(
        auto_no.starts_with("BT"),
        "自动批号 BT+日期+序号：{auto_no}"
    );
    assert_eq!(money_num(r["balance"].as_str().unwrap()), 50.0);

    // 第二批（指定批号、生产日期更早）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batch",
            &sid,
            serde_json::json!({
                "item": "RM10", "batch_no": "B-001", "production_date": "2026-01-05",
                "warehouse": "WH1", "location": "L1", "qty": "30", "direction": "in"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // 出库20 → 余额10
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batch",
            &sid,
            serde_json::json!({ "item": "RM10", "batch_no": "B-001", "qty": "20", "direction": "out" }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        money_num(r["balance"].as_str().unwrap()),
        10.0,
        "出库后余额10"
    );

    // 列表 + 库存流水同账（批次余额与普通库存一致）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches?item=RM10", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let ws = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/warehouse-stock?item=RM10", &sid))
        .await
        .unwrap();
    assert_eq!(
        ws.status(),
        StatusCode::OK,
        "仓库库存查询可用（批次=同一本账）"
    );

    // FEFO：生产日期早的先出（B-001 余额10 → 再吃自动批）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/batches/fefo",
            &sid,
            serde_json::json!({ "item": "RM10", "qty": "60" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rec = r["rows"].as_array().unwrap();
    assert_eq!(rec[0]["batch_no"], "B-001", "近生产日期先出：{rec:?}");
    assert_eq!(money_num(rec[0]["take"].as_str().unwrap()), 10.0);
    assert_eq!(rec[1]["batch_no"], auto_no);
    assert_eq!(money_num(rec[1]["take"].as_str().unwrap()), 50.0);

    // 临期：未配保质期 → 无失效日期 → 空
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches/expiring?days=30", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 库位 CRUD + 校验
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/locations",
            &sid,
            serde_json::json!({ "code": "L01", "name": "A区货位", "kind": "storage", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增库位应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/locations", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let locs = r["rows"].as_array().unwrap();
    assert_eq!(locs.len(), 1);
    let lid = locs[0]["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/locations",
            &sid,
            serde_json::json!({ "code": "L02", "name": "坏类型", "kind": "bad" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非法库位类型应拒绝");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/locations/{lid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 库存作业：到货即入库（补价防呆）→ 质检不合格自动退货 → 超退防呆 → 形态转换 → 低库存端点。
#[tokio::test]
async fn inventory_qc_convert_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 带价采购订单（20 件 × 8 元）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-11", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "质检链造数",
                "lines": [{ "item_code": "140301", "qty_ordered": "20", "unit_price": "8", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单");
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 到货 20 → 采购入库流水（kind=purchase）库存 +20
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-12", "qty": "20", "memo": "首批" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货入库");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140301",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let on_hand: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(on_hand, 20.0, "到货后 140301 库存 20");

    // 质检：检验 20、不合格 5 → 自动退货 → 库存 15
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/qc",
            &sid,
            serde_json::json!({
                "po_id": po_id, "qty_insp": "20", "qty_fail": "5",
                "inspector": "质检员甲", "date": "2026-01-13", "memo": "外观不良"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "质检保存");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["qty_pass"], "15");
    assert_eq!(r["qty_fail"], "5");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140301",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let on_hand: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(on_hand, 15.0, "不合格 5 已退货 → 库存 15");

    // 质检防呆：不合格 > 检验 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/qc",
            &sid,
            serde_json::json!({ "po_id": po_id, "qty_insp": "5", "qty_fail": "9" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不合格超检验应拒");

    // 超退防呆：直接退货 20 > 累计净收货 15 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/return",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-14", "qty": "20", "memo": "超退测试" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超退应拒");

    // 形态转换 140301 → 140501 ×10：5 / 10
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/form-convert",
            &sid,
            serde_json::json!({ "from_item": "140301", "to_item": "140501", "qty": "10", "date": "2026-01-15", "memo": "转产" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "形态转换");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140301",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let q: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(q, 5.0, "源物料剩 5");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/inventory/warehouse-stock?item=140501",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let q: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(q, 10.0, "目标物料 +10");

    // 同物料转换 → 400；低库存端点 200
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/form-convert",
            &sid,
            serde_json::json!({ "from_item": "140301", "to_item": "140301", "qty": "1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "同物料转换应拒");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/below-safety", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "低库存端点可用");
}

/// 完工入库数量不得超过订单未完工数量（防超完工虚增库存与摊薄单位成本）。
#[tokio::test]
async fn prod_complete_rejects_over_completion() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let d15 = format!("{cur_label}-15");
    let d20 = format!("{cur_label}-20");

    // 完工结转要求订单已归集到成本（无成本不许完工），先备齐标准价 + BOM 才能走到完工
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "配标准价");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "配 BOM");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "100", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "下达生产订单：{b}");
    let pid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");
    // 领料 2×100 归集材料成本（BOM 2 件 × 标准价 10）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/issue"),
            &sid,
            serde_json::json!({ "date": d15.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "领料归集成本：{b}");

    // 计划 100 件，未完工 100 件 → 报 999999 必须 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/complete"),
            &sid,
            serde_json::json!({ "qty": "999999", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "超量完工应 400：{b}");

    // completed_qty 必须原封不动
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let o = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(pid))
        .expect("列表应含该订单");
    assert_eq!(
        money_num(o["completed_qty"].as_str().unwrap()),
        0.0,
        "completed_qty 不应被推进：{o}"
    );
    assert_eq!(o["status"], "InProgress", "订单应仍在生产中：{o}");

    // 计划量之内仍然放行
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{pid}/complete"),
            &sid,
            serde_json::json!({ "qty": "100", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "足量完工应放行：{b}");
}
