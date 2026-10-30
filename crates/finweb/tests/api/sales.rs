//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

/// P1：供应链补链——报价→订单 doc_link + 到货推进采购订单状态（部分入库/已完成，退货回落）。
#[tokio::test]
async fn quote_link_and_po_status() {
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

    // 报价 → 审批 → 转订单 → 单据链上游含报价
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d13, "customer_code": "C01",
                "customer_name": "客户甲", "item_code": "140301", "item_name": "原料",
                "qty": "3", "unit_price": "20", "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建报价");
    let qid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审批报价");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/to-order"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "报价转订单");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["so_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=so&id={so_id}"),
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
            .any(|n| n["kind"] == "quote" && n["dir"] == "up"),
        "订单链上游应见报价：{r}"
    );

    // 采购订单 10 @9：到货 4 → 部分入库；到货 6 → 已完成；退货 2 → 部分入库
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d13, "supplier_code": "S01", "supplier_name": "供应商甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    async fn po_status_of(state: &Arc<WebState>, sid: &str, cur_ymm: i32, po_id: i64) -> String {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(
                &format!("/api/procure/po?period={cur_ymm}"),
                sid,
            ))
            .await
            .unwrap();
        let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["id"] == po_id)
            .map(|x| x["status"].as_str().unwrap().to_string())
            .unwrap_or_default()
    }
    assert_eq!(po_status_of(&state, &sid, cur_ymm, po_id).await, "Draft");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货 4");
    assert_eq!(
        po_status_of(&state, &sid, cur_ymm, po_id).await,
        "PartialIn",
        "部分到货应为部分入库"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "6", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货 6");
    assert_eq!(
        po_status_of(&state, &sid, cur_ymm, po_id).await,
        "Completed",
        "全量到货应为已完成"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/return",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d13, "qty": "2", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "退货 2");
    assert_eq!(
        po_status_of(&state, &sid, cur_ymm, po_id).await,
        "PartialIn",
        "退货后应回落到部分入库"
    );
}

/// P2：单据编码规则自定义 + 工作流消息节点（到达即通知并自动继续）。
#[tokio::test]
async fn doc_prefix_and_wf_message() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 1) 单据前缀：采购改 CGP，销售留默认
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    opts["doc_prefixes"] = serde_json::json!({ "po": "CGP" });
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存前缀");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "supplier_code": "S01", "supplier_name": "供应商甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "1", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/procure/po?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po_no = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(po_id))
        .unwrap()["no"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        po_no.starts_with("CGP202601"),
        "采购单号应带自定义前缀：{po_no}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "1", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let so_no = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(so_id))
        .unwrap()["no"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        so_no.starts_with("XS202601"),
        "销售单号保持默认前缀：{so_no}"
    );

    // 2) 工作流消息节点：start → 初审 → 消息 → 复核；消息节点自动跳过并留痕
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows",
            &sid,
            serde_json::json!({
                "id": 0, "name": "报价带消息节点", "biz_type": "quotation",
                "nodes": [
                    { "id": "n1", "type": "start", "name": "开始" },
                    { "id": "n2", "type": "approve", "name": "初审" },
                    { "id": "n3", "type": "message", "name": "通知业务员" },
                    { "id": "n4", "type": "approve", "name": "复核" }
                ],
                "edges": [
                    { "id": "e1", "from": "n1", "to": "n2", "kind": "normal" },
                    { "id": "e2", "from": "n2", "to": "n3", "kind": "normal" },
                    { "id": "e3", "from": "n3", "to": "n4", "kind": "normal" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存流程");
    let flow_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/workflows/{flow_id}/publish"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发布流程");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06", "customer_code": "C03", "customer_name": "客户丙",
                "item_code": "140501", "item_name": "成品", "qty": "1", "unit_price": "5",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let q = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["pending"], "复核", "消息节点应自动跳过、停在复核：{r}");
    // 消息留痕（审计 → 通知中心动态）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/logs?limit=20", &sid))
        .await
        .unwrap();
    let logs = body_string(resp).await;
    assert!(
        logs.contains("工作流") && logs.contains("消息"),
        "消息节点应留痕：{logs}"
    );
    // 复核通过 → 终态批准
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["ok"] == true, "终态批准：{r}");
}

/// 税务申报表端点：进项只认已认证、参数校验、导出带 BOM
#[tokio::test]
async fn tax_vat_form_endpoints() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造两张销项（含 pending）+ 一张已认证进项 + 一张未认证进项
    async fn add(
        state: &Arc<WebState>,
        sid: &str,
        kind: &str,
        number: &str,
        date: &str,
        net: &str,
        tax: &str,
        status: &str,
    ) {
        let net = fincore::Money::parse(net).unwrap();
        let tax = fincore::Money::parse(tax).unwrap();
        let r = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/invoices",
                sid,
                serde_json::json!({
                    "id": 0,
                    "kind": kind, "code": "044001900111", "number": number, "date": date,
                    "buyer": if kind == "out" { "客户甲" } else { "本企业" },
                    "seller": if kind == "out" { "本企业" } else { "供应商甲" },
                    "amount_tax": format!("{}", net + tax), "amount": format!("{net}"),
                    "tax": format!("{tax}"), "tax_rate": "0.13", "status": status,
                    "memo": ""
                }),
            ))
            .await
            .unwrap();
        let st = r.status();
        let b = body_string(r).await;
        assert_eq!(st, StatusCode::OK, "建发票应成功：{b}");
    }
    add(
        &state,
        &sid,
        "out",
        "FP000001",
        "2026-01-05",
        "1000",
        "130",
        "pending",
    )
    .await;
    add(
        &state,
        &sid,
        "in",
        "FP000002",
        "2026-01-06",
        "500",
        "65",
        "verified",
    )
    .await;
    add(
        &state,
        &sid,
        "in",
        "FP000003",
        "2026-01-07",
        "300",
        "39",
        "pending",
    )
    .await;

    // 取申报表
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/tax/vat?period=2026-01", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let f = &v["form"];
    assert_eq!(f["output_tax"], serde_json::json!("130.00"), "销项 {f}");
    assert_eq!(
        f["input_tax"],
        serde_json::json!("65.00"),
        "进项只应计已认证的 65：{f}"
    );
    assert_eq!(f["payable"], serde_json::json!("65.00"));
    // 未认证进项必须在提示里说出来
    let warns: Vec<&str> = f["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        warns.iter().any(|w| w.contains("尚未认证")),
        "未认证进项必须提示人工处理（否则会计以为数算全了）：{warns:?}"
    );

    // 构成明细：13% 销项档
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/tax/vat/detail?period=2026-01&kind=out&rate=13",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(d["count"], serde_json::json!(1), "{d}");
    // 缺参数 / 非法 kind 要 400
    for uri in [
        "/api/tax/vat/detail?period=2026-01&kind=out",
        "/api/tax/vat/detail?period=2026-01&kind=xx&rate=13",
    ] {
        let r = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{uri} 应 400");
    }

    // 导出：带 BOM（Excel 双击不乱码）+ 口径说明随表走
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/tax/vat/export?period=2026-01", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(
        r.headers().get(header::CONTENT_DISPOSITION).is_some(),
        "导出应带 attachment 头"
    );
    let bytes = axum::body::to_bytes(r.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(&bytes[0..3], &[0xEF, 0xBB, 0xBF], "CSV 应带 UTF-8 BOM");
    let text = String::from_utf8_lossy(&bytes).to_string();
    for want in [
        "销项税额",
        "130.00",
        "城市维护建设税",
        "进项税额明细",
        "口径说明",
    ] {
        assert!(text.contains(want), "导出表应含「{want}」");
    }
}

/// 税务参数：税率越界 / 非法枚举要 400，且必须能从接口写入
#[tokio::test]
async fn tax_options_validation() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    for (body, why) in [
        (
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "city", "edu_rate": "0.5", "local_edu_rate": "0.02" }),
            "教育费附加 50%",
        ),
        (
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "city", "edu_rate": "-0.03", "local_edu_rate": "0.02" }),
            "负税率",
        ),
        (
            serde_json::json!({ "taxpayer": "alien", "city_tax_zone": "city", "edu_rate": "0.03", "local_edu_rate": "0.02" }),
            "非法纳税人身份",
        ),
        (
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "mars", "edu_rate": "0.03", "local_edu_rate": "0.02" }),
            "非法城建区",
        ),
        (
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "city", "edu_rate": "abc", "local_edu_rate": "0.02" }),
            "非数字税率",
        ),
    ] {
        let r = handlers::router(state.clone())
            .oneshot(authed_post("/api/tax/options", &sid, body))
            .await
            .unwrap();
        let st = r.status();
        let b = body_string(r).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{why} 应 400：{b}");
    }

    // 合法值可写入，且**真的影响附加税费计算**
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/tax/options",
            &sid,
            serde_json::json!({ "taxpayer": "general", "city_tax_zone": "county", "edu_rate": "0.03", "local_edu_rate": "0.02" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices",
            &sid,
            serde_json::json!({
                "id": 0,
                "kind": "out", "code": "044001900111", "number": "FP900001",
                "date": "2026-01-05", "buyer": "客户甲", "seller": "本企业",
                "amount_tax": "1130", "amount": "1000", "tax": "130",
                "tax_rate": "0.13", "status": "pending", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "{b}");

    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/tax/vat?period=2026-01", &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    // 县城档 5%：130 × 5% = 6.50（默认市区是 9.10，能看出参数确实生效）
    assert_eq!(
        v["form"]["surcharge"]["city_tax"],
        serde_json::json!("6.50"),
        "改了适用地区就该改税率：{v}"
    );
}

/// 超发必须被封顶：超发不重复确认收入，但**发货流水也不能多出库存**
///
/// 回归背景：`so_income_voucher_in` 明确「超发不重复确认」（`delta_qty.min(remaining)`），
/// 这只挡住了**收入**。如果发货流水按原始 qty 落库，就会出现「收入没多确认、
/// 库存却多发了」——账实不符，而且没有任何提示。
#[tokio::test]
async fn over_shipment_is_capped_in_stock_too() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Confirmed",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140301", "item_name": "成品甲",
                    "qty_ordered": "10", "unit_price": "100", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(r).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 发 6（合法）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": 202601, "date": "2026-01-10", "qty": "6" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    // 再发 6（超发：只剩 4 未发）。接受还是拒绝都不重要，
    // 重要的是**累计发货不能超过订购量**。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": 202601, "date": "2026-01-11", "qty": "6" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    let db = state.db_for("b1").unwrap();
    let shipped_qty: f64 = db
        .conn()
        .query_row(
            "SELECT COALESCE(SUM(ABS(qty)),0) FROM stock_move WHERE item='140301' AND qty<0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        shipped_qty <= 10.0 + 1e-9,
        "累计发货 {shipped_qty} 超过订购量 10 —— 超发只挡了收入没挡库存，账实不符（请求返回 {st}：{b}）"
    );
}

/// 销售订单下推生产订单：产销两单在数据上连起来，且下推量封顶
///
/// 回归背景：`production_order` 以前**没有 so_id**，产出的货与谁要它没有任何
/// 记录对应，「这批货是哪张订单要的」只能靠人肉记忆，缺货时更无法反查
/// 「哪些订单正等着这批料」。
#[tokio::test]
async fn sales_order_pushes_to_production_order() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Confirmed",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140501", "item_name": "成品甲",
                    "qty_ordered": "100", "unit_price": "100", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "建销售订单应成功：{b}");
    let so_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 下推前 ATP：无现货、无在途、已占用 100 → 负数（承诺不了）
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140501", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let a: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };
    assert_eq!(num(&a["committed"]), 100.0, "已确认订单应占用 100");
    assert_eq!(num(&a["atp"]), -100.0, "没现货没在途时承诺不了 100 件");

    // 下推生产订单（不填 qty = 认领全部余量）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id, "date": "2026-01-06" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "下推生产订单应成功：{b}");
    let pushed: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        pushed["so_id"],
        serde_json::json!(so_id),
        "必须记住来源销售订单"
    );
    assert_eq!(num(&pushed["planned_qty"]), 100.0);
    // 返回里带上 ATP：在途 +100 正好抵掉占用
    assert_eq!(num(&pushed["atp"]["atp"]), 0.0, "在途抵掉占用后才够承诺");

    // 生产订单列表里能看到来源单号
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod?period=2026-01", &sid))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let row = list["orders"]
        .as_array()
        .expect("产单列表应返回数组")
        .iter()
        .find(|x| x["so_id"] == so_id)
        .expect("列表里应能看到这张下推来的产单");
    assert!(
        !row["so_no"].as_str().unwrap_or("").is_empty(),
        "应回显来源销售订单号，否则产销还是对不上：{row}"
    );

    // 全部认领后再推必须被拒（否则产销对不上）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id, "date": "2026-01-07" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert!(
        st.is_client_error(),
        "已全部下推后不该还能再推（拿到 {st}：{b}）"
    );
}

/// 草稿销售订单不能下推生产
#[tokio::test]
async fn draft_sales_order_cannot_push_production() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Draft",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140501", "item_name": "成品甲",
                    "qty_ordered": "10", "unit_price": "100", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(r).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert!(
        st.is_client_error(),
        "草稿订单不该能下推生产（拿到 {st}：{b}）"
    );
}

/// ATP 端点：待检库存不算可用，草稿订单不占用
///
/// 这两条是「宁可少承诺也不能虚承诺」的底线：待检货领不走，草稿还不是承诺。
#[tokio::test]
async fn atp_endpoint_excludes_qc_pending_and_draft() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造待检入库 80 件
    {
        let db = state.db_for("b1").unwrap();
        let q = fincore::Money::parse("80").unwrap();
        let p = fincore::Money::parse("5").unwrap();
        let id = findb::business::stock_insert(
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
                memo: String::new(),
            },
        )
        .unwrap();
        db.conn()
            .execute(
                "UPDATE stock_move SET qc_status='pending' WHERE id=?1",
                [id],
            )
            .unwrap();
    }
    // 一张草稿订单要 30 件（不占用）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Draft",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140301", "item_name": "材料甲",
                    "qty_ordered": "30", "unit_price": "5", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/atp?item=140301", &sid))
        .await
        .unwrap();
    let a: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };
    assert_eq!(
        num(&a["on_hand"]),
        0.0,
        "待检库存不可领用，不能算进可承诺量：{a}"
    );
    assert_eq!(num(&a["committed"]), 0.0, "草稿订单还不是承诺：{a}");
    assert_eq!(num(&a["atp"]), 0.0);
    assert!(
        a["formula"].as_str().unwrap_or("").contains("在途"),
        "响应要带上公式，否则数字没法核对：{a}"
    );
}

/// 链6：MPS（销售需求+净算+在制扣减+一键下达）→ 粗排（件/日顺排+按日负荷）→ 细排写回。
#[tokio::test]
async fn mps_schedule_flow() {
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

    // 需求：确认销售订单 140301 ×6（未发量 = 6）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "MPS 造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "6", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // ① MPS（从销售收集）：demand=6、wip=0、planned=6（无库存）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mps/run",
            &sid,
            serde_json::json!({ "from_sales": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "MPS 运行");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_code"] == "140301")
        .unwrap()
        .clone();
    assert_eq!(
        row["demand"].as_str().unwrap().parse::<f64>().unwrap(),
        6.0,
        "需求=未发量：{row}"
    );
    assert_eq!(
        row["wip"].as_str().unwrap().parse::<f64>().unwrap(),
        0.0,
        "初始无在制"
    );
    assert_eq!(
        row["planned"].as_str().unwrap().parse::<f64>().unwrap(),
        6.0,
        "计划=6-0-0：{row}"
    );

    // ② 在制扣减：建生产订单 ×4 → 再跑 MPS → wip=4、planned=2
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140301", "qty": "4", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建在制订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mps/run",
            &sid,
            serde_json::json!({ "from_sales": true }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row2 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_code"] == "140301")
        .unwrap()
        .clone();
    assert_eq!(
        row2["wip"].as_str().unwrap().parse::<f64>().unwrap(),
        4.0,
        "在制=4：{row2}"
    );
    assert_eq!(
        row2["planned"].as_str().unwrap().parse::<f64>().unwrap(),
        2.0,
        "计划扣在制=6-4：{row2}"
    );
    let mps_id = row2["id"].as_i64().unwrap();

    // ③ 一键下达 → 生成生产订单；重复下达 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mps/{mps_id}/convert"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "MPS 下达");
    let conv: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let mps_order_id = conv["order_id"].as_i64().unwrap();
    assert!(
        conv["order_no"].as_str().unwrap().starts_with("SC"),
        "生成生产订单单号"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/mps/{mps_id}/convert"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复下达拒");

    // ④ 粗排：日产能 2 → 在制单（open 4 → 2 天）+ MPS 单（open 2 → 1 天）；负荷非空
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mps/rough",
            &sid,
            serde_json::json!({ "daily_qty": "2" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "粗排");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let orders = r["orders"].as_array().unwrap();
    assert!(orders.len() >= 2, "两单参与粗排：{r}");
    let mps_row = orders
        .iter()
        .find(|o| o["id"].as_i64() == Some(mps_order_id))
        .unwrap();
    assert_eq!(
        mps_row["need_days"].as_i64(),
        Some(1),
        "open2/日产能2 → 1 天：{mps_row}"
    );
    let wip_row = orders
        .iter()
        .find(|o| o["id"].as_i64() != Some(mps_order_id))
        .unwrap();
    assert_eq!(
        wip_row["need_days"].as_i64(),
        Some(2),
        "open4/日产能2 → 2 天：{wip_row}"
    );
    assert!(!r["load"].as_array().unwrap().is_empty(), "按日负荷非空");
    assert!(!mps_row["sug_start"].as_str().unwrap().is_empty());

    // ⑤ 细排写回：MPS 单排到 2026-02-01（跨期日期允许）；空清单 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/schedule",
            &sid,
            serde_json::json!({ "items": [{ "id": mps_order_id, "start": "2026-02-01", "end": "2026-02-01" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "细排写回");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["updated"], 1);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/prod?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let scheduled = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(mps_order_id))
        .unwrap()
        .clone();
    assert_eq!(
        scheduled["plan_start"], "2026-02-01",
        "计划开工已写回：{scheduled}"
    );
    assert_eq!(scheduled["plan_end"], "2026-02-01");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod/schedule",
            &sid,
            serde_json::json!({ "items": [] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "空清单拒");

    // ⑥ 空需求 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/mps/run",
            &sid,
            serde_json::json!({ "from_sales": false, "demands": [] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "空需求拒");
}

/// 发货通知 + 票款勾稽：未确认拒通知/超未发量拒 → 出库自动完成通知 → 收款与发票建边。
#[tokio::test]
async fn ship_notice_flow() {
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

    // 销售订单（6 件，未发量 6）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "通知造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "6", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未确认 → 拒通知
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/notice"),
            &sid,
            serde_json::json!({ "qty": "4", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未确认不能发通知");

    // 确认 → 超未发量拒 → 正常通知 4
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/notice"),
            &sid,
            serde_json::json!({ "qty": "9", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超未发量拒");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/notice"),
            &sid,
            serde_json::json!({ "qty": "4", "date": d15.clone(), "memo": "备货" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "通知发出");

    // 列表 pending 含本单
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/notices", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["so_id"].as_i64() == Some(so_id) && n["status"] == "pending"),
        "待发通知应含本单：{r}"
    );

    // 出库 → 通知自动完成（pending 不再含本单）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "出库");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/notices", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        !r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["so_id"].as_i64() == Some(so_id) && n["status"] == "pending"),
        "出库后通知应自动完成：{r}"
    );

    // 票款勾稽：下推销项发票 → 订单页收款 → 发票上游见收付款单
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下推销项发票");
    let inv_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["invoice_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/payment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "amount": "10", "memo": "部分收款" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单页收款");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=invoice&id={inv_id}"),
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
            .any(|n| n["kind"] == "receipt" && n["dir"] == "down"),
        "发票下游应见收付款单（票↔款勾稽）：{r}"
    );
}

/// 通知中心：结构/水位已读/可见性过滤 + 单据流程条数据源（instance-for）。
#[tokio::test]
async fn notices_endpoint() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 结构：todos/events/unread_events/now（now=YYYY-MM-DD HH:MM:SS）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/notices", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "通知端点可用");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["todos"].as_array().is_some(), "待办段");
    assert!(r["events"].as_array().is_some(), "动态段");
    let now = r["now"].as_str().unwrap().to_string();
    assert_eq!(now.len(), 19, "now 为 YYYY-MM-DD HH:MM:SS：{now}");

    // 水位推进 → 未读动态归零
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/notices?since={}", now.replace(' ', "%20")),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["unread_events"], 0, "水位后无新动态");

    // 造一条动态（保存工作流会写审计日志）→ 未读 ≥1
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows",
            &sid,
            serde_json::json!({
                "id": 0, "name": "通知造数流", "biz_type": "quotation",
                "nodes": [
                    { "id": "n1", "type": "start", "name": "开始" },
                    { "id": "n2", "type": "approve", "name": "初审" },
                    { "id": "n3", "type": "approve", "name": "复核" }
                ],
                "edges": [
                    { "id": "e1", "from": "n1", "to": "n2", "kind": "normal" },
                    { "id": "e2", "from": "n2", "to": "n3", "kind": "normal" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造流程（产生审计动态）");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/notices", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["unread_events"].as_i64().unwrap() >= 1,
        "应有新动态未读：{r}"
    );

    // instance-for：发布流程 → 建报价审批 → running 实例节点可查（流程条/行徽标数据源）
    let flow_id = /* 用返回 id */ {
        let resp2 = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/workflows",
                &sid,
                serde_json::json!({
                    "id": 0, "name": "通知流程条", "biz_type": "quotation",
                    "nodes": [
                        { "id": "n1", "type": "start", "name": "开始" },
                        { "id": "n2", "type": "approve", "name": "初审" },
                        { "id": "n3", "type": "approve", "name": "复核" }
                    ],
                    "edges": [
                        { "id": "e1", "from": "n1", "to": "n2", "kind": "normal" },
                        { "id": "e2", "from": "n2", "to": "n3", "kind": "normal" }
                    ]
                }),
            ))
            .await
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&body_string(resp2).await).unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/workflows/{flow_id}/publish"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发布流程");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20",
                "customer_code": "C09", "customer_name": "客户九",
                "item_code": "140301", "item_name": "原料", "qty": "3", "unit_price": "2",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let qid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "首次审批（入流）");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/workflows/instance-for?biz_type=quotation&id={qid}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["found"], serde_json::json!(true), "实例应存在：{r}");
    assert_eq!(r["status"], "running");
    assert_eq!(r["current_label"], "复核", "首节点推进后当前=复核：{r}");

    // 无实例的单据 → found=false
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/workflows/instance-for?biz_type=quotation&id=99999",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["found"], serde_json::json!(false));
    // 非法业务类型 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/workflows/instance-for?biz_type=bogus&id=1",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// 发票↔单据勾稽 + 进项认证：订单下推发票（doc_link 双向）→ 认证流转。
#[tokio::test]
async fn invoice_push_and_certify() {
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

    // 采购链：带价 PO → 到货 → 下推进项发票（金额=整单 90，待认证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
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
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d15.clone(), "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-po",
            &sid,
            serde_json::json!({ "po_id": po_id }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let ib = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "PUSH_INV_ERR={ib}");
    let inv_id = serde_json::from_str::<serde_json::Value>(&ib).unwrap()["invoice_id"]
        .as_i64()
        .unwrap();

    // 发票内容：进项、供应商、金额90、待认证
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?kind=in", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let inv = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(inv_id))
        .unwrap();
    assert_eq!(inv["kind"], "in");
    assert_eq!(inv["seller"], "供应商甲");
    assert_eq!(money_num(inv["amount"].as_str().unwrap()), 90.0);
    assert_eq!(inv["status"], "pending", "下推即待认证");

    // 勾稽双向：PO 下游见发票；发票上游见 PO
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=po&id={po_id}"),
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
            .any(|n| n["kind"] == "invoice"
                && n["dir"] == "down"
                && n["id"].as_i64() == Some(inv_id)),
        "PO 下游应见发票：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=invoice&id={inv_id}"),
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
            .any(|n| n["kind"] == "po" && n["dir"] == "up" && n["id"].as_i64() == Some(po_id)),
        "发票上游应见 PO：{r}"
    );

    // 进项认证流转：pending → verified
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/invoices/{inv_id}/status"),
            &sid,
            serde_json::json!({ "status": "verified" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "认证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?kind=in", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let inv = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(inv_id))
        .unwrap();
    assert_eq!(inv["status"], "verified", "认证后状态");

    // 0 额订单 → 下推拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "1", "unit_price": "0", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let zero_po = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-po",
            &sid,
            serde_json::json!({ "po_id": zero_po }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "0额订单不能下推发票"
    );

    // 销售链：SO 确认发货 → 下推销项发票（整单金额 20）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "开票造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "4", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下推销售发票");
    let out_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["invoice_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?kind=out", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let inv = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(out_id))
        .unwrap();
    assert_eq!(inv["kind"], "out");
    assert_eq!(
        money_num(inv["amount"].as_str().unwrap()),
        20.0,
        "销项金额=订单整单"
    );
    assert_eq!(inv["buyer"], "客户甲");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/doc-links?kind=so&id={so_id}"),
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
            .any(|n| n["kind"] == "invoice" && n["dir"] == "down"),
        "SO 下游应见发票：{r}"
    );
}

/// 对标金蝶流程：订单 CRUD + 状态流转 + 行金额服务端计算 + 客户信用卡控。
#[tokio::test]
async fn order_crud_and_credit_guard() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建销售订单：2 行（140301 50×12@13% → 不含税 600 / 税 78）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "customer_name": "客户甲", "status": "Draft", "memo": "",
                "lines": [
                    { "item_code": "140301", "qty_ordered": "50", "unit_price": "12", "tax_rate": "0.13" },
                    { "item_code": "140501", "qty_ordered": "2", "unit_price": "100", "tax_rate": "0.13" }
                ]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let body = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建销售订单应成功：{body}");
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = r["id"].as_i64().unwrap();
    assert_eq!(
        money_num(r["total_amount"].as_str().unwrap()),
        800.0,
        "行金额服务端计算：50×12+2×100"
    );
    assert_eq!(
        money_num(r["total_tax"].as_str().unwrap()),
        104.0,
        "税额=金额×税率"
    );

    // 列表可回读（含明细）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .expect("列表应含新订单");
    assert_eq!(row["lines"].as_array().unwrap().len(), 2);

    // 草稿 → 已确认
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .unwrap();
    assert_eq!(row["status"], "Confirmed");

    // 客户信用额度：超限确认被拒，额度内可确认
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "customer", "code": "C99", "name": "信用客户",
                "parent_code": null, "disabled": false,
                "props": { "credit_limit": "100" }, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建客户档案应成功");

    // 超限订单：草稿可存，确认被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-06", "customer_code": "C99",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "100", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿不受信用限制");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let over_id = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{over_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超信用额度应被拒");
    let s = body_string(resp).await;
    assert!(s.contains("信用额度不足"), "应提示信用额度不足：{s}");

    // 额度内订单：50×2=100 ≤100 → 确认成功
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-07", "customer_code": "C99",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "50", "unit_price": "2", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ok_id = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{ok_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "额度内订单应可确认");
    // 两笔占用合计 500+100 > 100 —— 再来一笔新的应被拒（占用按全部有效订单累计）
    // 注：超限单仍是草稿不计占用，占用=已确认的100；此处再确认原超限单 → 100+500=600>100 拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{over_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "累计占用超限仍应被拒"
    );

    // 删除（草稿/已确认均可删，so_delete 无状态限制）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{ok_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除订单应成功");

    // 采购订单镜像：建单 + 确认
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-08", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "30", "unit_price": "9", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pid = r["id"].as_i64().unwrap();
    assert_eq!(money_num(r["total_amount"].as_str().unwrap()), 270.0);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{pid}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购订单确认应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{pid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 发票 CRUD 与工资/报销 CSV 导出。
#[tokio::test]
async fn web_invoice_crud_and_exports() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 发票
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "in", "code": "044001", "number": "10001",
                "date": "2026-01-10", "buyer": "本公司", "seller": "供应商A",
                "amount_tax": "1130", "amount": "1000", "tax": "130",
                "tax_rate": "13", "status": "pending", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增发票应成功");
    let iid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?period=202601", &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(list["rows"].as_array().unwrap().len(), 1);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/invoices/{iid}/status"),
            &sid,
            serde_json::json!({ "status": "verified" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发票认证应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices/summary", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发票汇总应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/invoices/{iid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除发票应成功");

    // 工资行 + 导出
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202601",
            &sid,
            serde_json::json!({
                "employee": "E001", "dept": "财务部", "gross": "10000",
                "social": "500", "housing": "300", "deduction": "0",
                "additional": "0", "social_co": "1000", "housing_co": "300", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存工资行应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/payroll?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("员工") && body.contains("E001"),
        "工资 CSV 应含员工：{body}"
    );

    // 报销单 + 导出
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claims",
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-20", "applicant": "张三",
                "dept": "财务部", "reason": "差旅", "amount": "200",
                "items": [{ "expense_account": "660201", "amount": "200", "memo": "" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增报销单应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/claims?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("单号") && body.contains("张三"),
        "报销 CSV 应含申请人：{body}"
    );
}

/// 身兼多职（岗位并集）+ 多岗位下会计出纳互斥仍生效 + 价格字段权限（服务端裁剪/置零）。
#[tokio::test]
async fn multi_role_positions_and_price_field_perm() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;

    // 角色清单：12 个预设岗位（含新四岗）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/roles", &boss))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let roles: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let arr = roles.as_array().expect("roles 应是数组");
    assert!(arr.len() >= 12, "应有12个预设角色，实际 {}", arr.len());
    for code in [
        "receivables",
        "payables",
        "cost_accountant",
        "production",
        "order_clerk",
        "keeper",
    ] {
        assert!(arr.iter().any(|r| r["role"] == code), "缺少角色 {code}");
    }

    // 三个账号先开平台（邀请前必须存在同名平台账号）
    for u in ["mul1", "mix3", "prc1"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss,
                serde_json::json!({ "username": u, "display_name": u, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台 {u}");
    }
    // mul1：主岗位订单专员 + 兼任仓管员（身兼多职）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({
                "username": "mul1", "display_name": "多面手", "password": "",
                "role": "order_clerk", "roles": ["keeper"], "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "多岗位邀请应成功：{b}");
    // mix3：会计 + 兼任出纳 → 互斥（多岗位并集口径）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({
                "username": "mix3", "display_name": "混岗", "password": "",
                "role": "accountant", "roles": ["cashier"], "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "会计+出纳多岗位应被拒"
    );
    let s = body_string(resp).await;
    assert!(s.contains("会计与出纳"), "应提示互斥：{s}");
    // prc1：订单专员但被 deny 掉价格两权（字段级权限样本）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({
                "username": "prc1", "display_name": "录单员", "password": "",
                "role": "order_clerk", "deny_perms": ["price_view", "price_edit"],
                "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "deny 价格权邀请应成功");

    // mul1 / prc1 进账套
    let mut sids = std::collections::HashMap::new();
    for u in ["mul1", "prc1"] {
        let (st, sid) = login(&state, u, "Test12345").await;
        assert_eq!(st, StatusCode::OK, "{u} 登录");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/change-password",
                &sid,
                serde_json::json!({ "old": "Test12345", "new": "Pass123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{u} 首登改密");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 进账套"
        );
        sids.insert(u, sid);
    }
    let mul = sids["mul1"].clone();
    let prc = sids["prc1"].clone();

    // mul1 身兼多职：仓管域 + 订单域都通
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/unit?item=140301", &mul))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "兼岗（仓管）应能读仓储");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &mul,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "10", "unit_price": "5", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "兼岗（订单）应能建单");
    let r: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_get("/api/sales/so?period=202601", &mul))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let mul_row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["customer_code"] == "C01")
        .unwrap();
    assert_eq!(
        money_num(mul_row["total_amount"].as_str().unwrap()),
        50.0,
        "有价格权应看到真实金额"
    );

    // prc1 无价格权：建单价格被服务端置零、数量保留、套打无价格列
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &prc,
            serde_json::json!({
                "period": 202601, "date": "2026-01-06", "customer_code": "C02",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "prc1 建单应成功（数量域放行）"
    );
    let r: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_get("/api/sales/so?period=202601", &prc))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let prc_row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["customer_code"] == "C02")
        .unwrap();
    let prc_id = prc_row["id"].as_i64().unwrap();
    assert_eq!(
        money_num(prc_row["total_amount"].as_str().unwrap()),
        0.0,
        "无价格权金额应被置零"
    );
    assert_eq!(prc_row["lines"][0]["qty_ordered"], "10", "数量应保留");
    assert_eq!(
        money_num(prc_row["lines"][0]["unit_price"].as_str().unwrap()),
        0.0,
        "单价应被置零"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/sales/so/print-form?ids={prc_id}"),
            &prc,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;
    assert!(!html.contains("单价"), "无价格权套打不应出现单价列：{html}");

    // prc1 没有仓管权（兼岗是显式授予的，不是人人有）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/unit?item=140301", &prc))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "无仓管兼岗应403");
}

/// 销售出库闭环（与采购侧对称）：未确认拒发 → 发货写库存出库流水 → 超退防呆 → 退货回库。
#[tokio::test]
async fn so_shipment_stock_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let d12 = format!("{cur_label}-12");
    let d13 = format!("{cur_label}-13");
    let d14 = format!("{cur_label}-14");

    // 采购入库 10×9（库存基线 10）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d12.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
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
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d12.clone(), "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购入库");

    // 销售订单（C01，6 件 × 5）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d13.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "出库造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "6", "unit_price": "5", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建销售订单");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未确认订单 → 发货拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d14.clone(), "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未确认不能发货");

    // 确认 → 发货 4：库存 10-4=6 + 收入凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d14.clone(), "qty": "4", "memo": "首批" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["voucher_id"].as_i64().unwrap_or(0) > 0,
        "发货应生成收入凭证：{r}"
    );
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
    assert_eq!(on_hand, 6.0, "发货 4 → 库存 10-4=6（销售出库流水生效）");

    // 超退 8（净发货 4）→ 拒；退货 2 → 库存回 8
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/return",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d14.clone(), "qty": "8", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超退应拒");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/return",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d14.clone(), "qty": "2", "memo": "部分退回" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "退货");
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
    assert_eq!(on_hand, 8.0, "退货 2 → 库存回 8（销售流水回库）");
}

/// 可视化工作流（对标金蝶审批流）：设计 → 发布 → 单据审批自动入流逐节点推进 → 默认流回退。
#[tokio::test]
async fn workflow_visual_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建报价（流程发布前先批一张 → 默认流立即生效）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05",
                "customer_code": "C01", "customer_name": "客户甲",
                "item_code": "140501", "item_name": "成品",
                "qty": "1", "unit_price": "5",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let q1 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q1}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["pending"].is_null(),
        "无流程 → 默认流立即批准，无 pending：{r}"
    );

    // 设计两节点流（校验 + 保存 + 发布）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows",
            &sid,
            serde_json::json!({
                "id": 0, "name": "报价两级审批", "biz_type": "quotation",
                "nodes": [
                    { "id": "n1", "type": "start", "name": "开始" },
                    { "id": "n2", "type": "approve", "name": "初审" },
                    { "id": "n3", "type": "approve", "name": "复核" }
                ],
                "edges": [
                    { "id": "e1", "from": "n1", "to": "n2", "kind": "normal" },
                    { "id": "e2", "from": "n2", "to": "n3", "kind": "normal" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存流程应成功");
    let flow_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    // 无开始节点 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/workflows",
            &sid,
            serde_json::json!({
                "name": "坏流程", "biz_type": "quotation",
                "nodes": [{ "id": "x1", "type": "approve", "name": "审批" }], "edges": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "缺开始节点应拒绝");
    // 发布
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/workflows/{flow_id}/publish"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发布应成功");

    // 流程内报价：第一节点 → pending（单据不动），第二节点 → 终态批准
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06",
                "customer_code": "C02", "customer_name": "客户乙",
                "item_code": "140501", "item_name": "成品",
                "qty": "2", "unit_price": "10",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let q2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q2}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["pending"],
        serde_json::json!("复核"),
        "首节点应返回下一节点：{r}"
    );
    // 单据此时仍未批准
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/quote?period=202601", &sid))
        .await
        .unwrap();
    let rows = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap();
    let row = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == q2)
        .unwrap();
    assert_eq!(row["status"], "draft", "pending 阶段单据不应被批准");
    // 第二节点 → 终态 → 原批准执行
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q2}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["pending"].is_null(), "终节点不应再有 pending：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/quote?period=202601", &sid))
        .await
        .unwrap();
    let rows = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap();
    let row = rows["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == q2)
        .unwrap();
    assert_eq!(row["status"], "approved", "终态后单据应已批准");

    // 实例回放：q2 应产生 approved 实例
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workflows/instances", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let inst = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap();
    let arr = inst["rows"].as_array().unwrap();
    assert!(
        arr.iter().any(|i| {
            i["biz_type"] == "quotation"
                && i["biz_id"].as_i64() == Some(q2)
                && i["status"] == "approved"
        }),
        "应存在 q2 的已通过实例"
    );

    // 删除有运行中实例的流程 → 拒（再造第三张报价走首节点后删除）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-07",
                "customer_code": "C03", "customer_name": "客户丙",
                "item_code": "140501", "item_name": "成品",
                "qty": "3", "unit_price": "5",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    let q3 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let _ = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{q3}/approve"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/workflows/{flow_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "有运行中实例不能删流程"
    );
}

/// 订单状态机（由 findb 的 scm 流转表把关，HTTP 层透传 400）：草稿不得一步跳到已完成，
/// 草稿 → 已确认仍可用；已作废是终态。销售/采购两侧同口径。
#[tokio::test]
async fn order_transition_rejects_illegal_jump() {
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

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d13.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建销售订单");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d13.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单");
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 草稿 → 已完成：两侧都必须拒绝
    for (uri, what) in [
        (format!("/api/sales/so/{so_id}/transition"), "销售订单"),
        (format!("/api/procure/po/{po_id}/transition"), "采购订单"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &uri,
                &sid,
                serde_json::json!({ "status": "Completed" }),
            ))
            .await
            .unwrap();
        let st = resp.status();
        let b = body_string(resp).await;
        assert_eq!(
            st,
            StatusCode::BAD_REQUEST,
            "{what} 草稿不得直接跳已完成：{b}"
        );
    }
    // 草稿 → 部分发货 同样非法
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "PartialShip" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "草稿不得直接跳部分发货：{b}");

    // 状态没被改掉
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/sales/so?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let o = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(so_id))
        .expect("列表应含该订单");
    assert_eq!(o["status"], "Draft", "非法流转不应改状态：{o}");

    // 草稿 → 已确认：仍可用
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "草稿 → 已确认应放行：{b}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "草稿 → 已确认应放行：{b}");

    // 已确认 → 已完成：findb 的状态机同样拒绝（发货进度只能由发货流水推进）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Completed" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "已确认不得手工跳已完成：{b}");

    // 作废是出口
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Cancelled" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "已确认 → 已作废应放行：{b}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/po/{po_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "已作废是终态，不许复活：{b}");
}

/// 发票列表必须限量返回（原先 limit=None，一次把全库发票读进内存并整包序列化）。
#[tokio::test]
async fn invoice_list_is_bounded() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let db = state.db_for("b1").expect("打开 b1");
    let mut sql = String::new();
    for i in 1..=501 {
        sql.push_str(&format!(
            "INSERT INTO invoice(kind,code,number,date,buyer,seller,amount_tax,amount,tax,tax_rate,\
             status,memo,attach_id,created_by,created_at,updated_at) VALUES(\
             'out','INV','{i:04}','2026-01-15','买方{i}','卖方{i}','113.00','100.00','13.00','13',\
             'pending','',0,'boss',datetime('now'),datetime('now'));\n"
        ));
    }
    let tx = db.write_tx().expect("开事务");
    tx.execute_batch(&sql).expect("批量造 501 张发票");
    tx.commit().expect("提交造数");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "发票列表：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(r["limit"], 500, "应声明上限：{b}");
    assert_eq!(
        r["rows"].as_array().unwrap().len(),
        500,
        "最多回 500 行：{b}"
    );
    assert_eq!(r["truncated"], true, "超限时须回报截断：{b}");

    // 收窄条件后不再截断
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices?keyword=0001", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "带关键字的发票列表：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(
        r["rows"].as_array().unwrap().len() < 500,
        "收窄后不该再截断：{b}"
    );
    assert_eq!(r["truncated"], false, "收窄后不该报截断：{b}");
}
