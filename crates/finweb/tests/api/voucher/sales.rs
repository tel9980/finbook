//! 由 `tests/api/voucher.rs`（5,128 行）按域拆分而来。只搬位置、不改一行用例。
//!
//! 本册：销售出货与成本、委外加工。

use super::super::*;

/// 发货/退货的收入凭证与库存流水必须**同一事务**。
/// 回归：早先 `add_so_shipment` 先调 `so_income_voucher`（自行提交）再调
/// `so_shipment_with_stock`（另一个事务）。第二步失败时收入与应收已入账提交，
/// 却没有发货/出库记录 → 孤儿凭证。退货同理：超退报错但收入冲回已提交。
#[tokio::test]
async fn sales_shipment_and_return_are_atomic() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let dash: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_get("/api/dashboard", &sid))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let cur_ymm: i32 = dash["current_period"]
        .as_str()
        .unwrap()
        .replace('-', "")
        .parse()
        .unwrap();
    let d13 = format!("{}-13", dash["current_period"].as_str().unwrap());

    // 建一张已确认的销售订单，返回 (id, 单号)
    async fn mk_so(
        state: &Arc<WebState>,
        sid: &str,
        cur_ymm: i32,
        d13: &str,
        qty: &str,
    ) -> (i64, String) {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/sales/so",
                sid,
                serde_json::json!({
                    "id": 0, "period": cur_ymm, "date": d13,
                    "customer_code": "C01", "customer_name": "客户甲",
                    "status": "Confirmed", "memo": "原子性造数",
                    "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": qty, "unit_price": "25", "tax_rate": "0" }]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建销售订单");
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let id = v["id"].as_i64().unwrap();
        // save_so 的响应里没有 `no` 字段，从列表接口取单号
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/sales/so?period={cur_ymm}"), sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "列销售订单");
        let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let no = list["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("订单列表(rows)不是数组：{list}"))
            .iter()
            .find(|x| x["id"].as_i64() == Some(id))
            .and_then(|x| x["no"].as_str())
            .unwrap_or_else(|| panic!("列表里找不到 id={id} 的订单：{list}"))
            .to_string();
        (id, no)
    }

    // 统计摘要里提到该订单号的凭证数（收入确认凭证的摘要形如「发货确认 SO-xxx」）
    async fn count_vouchers_for(
        state: &Arc<WebState>,
        sid: &str,
        cur_ymm: i32,
        so_no: &str,
    ) -> usize {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(
                &format!("/api/vouchers?period={cur_ymm}&limit=500"),
                sid,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "列凭证");
        let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        list.as_array()
            .unwrap()
            .iter()
            .filter(|v| v["summary"].as_str().unwrap_or("").contains(so_no))
            .count()
    }

    // ── 场景 1：发货时仓库不存在 → 400，且不得留下任何收入凭证 ──
    let (so1, no1) = mk_so(&state, &sid, cur_ymm, &d13, "10").await;
    let before = count_vouchers_for(&state, &sid, cur_ymm, &no1).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so1, "period": cur_ymm, "date": d13, "qty": "4", "memo": "", "warehouse": "NOPE" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非法仓库应 400");
    assert_eq!(
        before,
        count_vouchers_for(&state, &sid, cur_ymm, &no1).await,
        "发货失败不得留下收入凭证（原子性）"
    );

    // ── 场景 2：正常发货成功，凭证 + 出库都在 ──
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so1, "period": cur_ymm, "date": d13, "qty": "4", "memo": "", "warehouse": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货应成功");
    let ok_body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        ok_body["voucher_id"].is_i64(),
        "发货应生成收入凭证：{ok_body}"
    );
    assert!(
        count_vouchers_for(&state, &sid, cur_ymm, &no1).await > before,
        "发货后凭证数应增加"
    );

    // ── 场景 3：退货超量 → 400，且不得留下收入冲回凭证 ──
    let (so2, no2) = mk_so(&state, &sid, cur_ymm, &d13, "10").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so2, "period": cur_ymm, "date": d13, "qty": "4", "memo": "", "warehouse": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "先发货 4");
    let before_ret = count_vouchers_for(&state, &sid, cur_ymm, &no2).await;
    // 净发货只有 4，退 50 必须被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/return",
            &sid,
            serde_json::json!({ "so_id": so2, "period": cur_ymm, "date": d13, "qty": "50", "memo": "", "warehouse": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "超退应 400");
    assert_eq!(
        before_ret,
        count_vouchers_for(&state, &sid, cur_ymm, &no2).await,
        "超退失败不得留下收入冲回凭证（原子性）"
    );
}

/// 销售成本结转（Web 化，对标金蝶存货核算-凭证生成）：仅统计销售出库（kind=sale，
/// 领料/形态转换不进 6401）；无出库不生成凭证；同期间防重复结转。
#[tokio::test]
async fn sales_cost_cutover() {
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
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let d15 = format!("{cur_label}-15");
    let d12 = format!("{cur_label}-12");
    let d20 = format!("{cur_label}-20");

    // a) 采购入库 10×9（库存基础；此时无销售出库）
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

    // b) 尚无销售出库 → 结转返回 none（不生成凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/sales-cost",
            &sid,
            serde_json::json!({ "period": cur_ymm, "method": "moving_average", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["none"],
        serde_json::json!(true),
        "无销售出库不生成凭证：{r}"
    );

    // c) 销售订单确认 → 发货 4（kind=sale 出库）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "结转造数",
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

    // d) 形态转换出 2 件（kind=other_out，验证不被结转进 6401）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/form-convert",
            &sid,
            serde_json::json!({ "from_item": "140301", "to_item": "140501", "qty": "2", "date": d15.clone(), "memo": "转产" }),
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
    let on_hand: f64 = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x["qty"].as_str().unwrap()))
        .sum();
    assert_eq!(on_hand, 4.0, "10-4-2=4（sale4 + other_out2 均已扣数量）");

    // e) 结转 → 6401 借 = 仅销售发出 4×9=36（不含形态转换的 2 件）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/sales-cost",
            &sid,
            serde_json::json!({ "period": cur_ymm, "method": "moving_average", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    let st2 = resp.status();
    let cbody = body_string(resp).await;
    assert_eq!(st2, StatusCode::OK, "CUTOVER_ERR={cbody}");
    let r: serde_json::Value = serde_json::from_str(&cbody).unwrap();
    let vid = r["voucher_id"].as_i64().unwrap_or(0);
    assert!(vid > 0, "应生成结转凭证：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let dr = entries
        .iter()
        .find(|e| e["account_code"] == "6401")
        .expect("借主营业务成本");
    assert_eq!(
        money_num(dr["debit"].as_str().unwrap()),
        36.0,
        "结转额=销售发出 4×9=36（口径排除领料/转换）"
    );
    let cr = entries
        .iter()
        .find(|e| e["account_code"] == "140501")
        .expect("贷库存商品");
    assert_eq!(money_num(cr["credit"].as_str().unwrap()), 36.0);

    // f) 同期间重复结转 → 拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/sales-cost",
            &sid,
            serde_json::json!({ "period": cur_ymm, "method": "moving_average", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复结转应拒绝");
}

/// 采购暂估自动出凭证：登记（借存货/贷应付-供应商）+ 冲回反向 + 幂等。
#[tokio::test]
async fn estimate_auto_voucher() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建采购订单（S01，140301 300×9）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "300", "unit_price": "9", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pid = r["id"].as_i64().unwrap();

    // 先收货再暂估：暂估额度来自实收量（封顶在「价税合计 × 已收/订购」），
    // 货没到就登暂估会被守卫拒掉 —— 那正是守卫要挡的「凭空虚增应付」。
    // 收 300 → 额度 2700×1.13 = 3051，够登 800。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "date": "2026-01-08", "qty": "300" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "采购收货应成功");

    // 登记暂估 800 → 借140301 / 贷220201(S01)
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/estimate",
            &sid,
            serde_json::json!({ "po_id": pid, "period": 202601, "item": "140301", "est_amount": "800" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "登记暂估应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let est_id = r["id"].as_i64().unwrap();
    let vid = r["voucher_id"].as_i64().expect("登记暂估应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "140301");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 800.0);
    assert_eq!(v["entries"][1]["account_code"], "220201");
    assert_eq!(v["entries"][1]["aux"]["supplier"], "S01");

    // 冲回 → 反向凭证；幂等
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/estimate/{est_id}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-20" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "冲回应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rvid = r["voucher_id"].as_i64().expect("冲回应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{rvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "220201");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 800.0);
    assert_eq!(v["entries"][1]["account_code"], "140301");
    assert_eq!(
        money_num(v["entries"][1]["credit"].as_str().unwrap()),
        800.0
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/procure/estimate/{est_id}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-21" }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "重复冲回应幂等：{r}");
}

/// 发货 → 收入确认凭证（比例法）+ 退货冲回 + 订单状态联动。
#[tokio::test]
async fn sales_ship_income_voucher() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 订单：1 行 10×10@13% → 不含税 100 / 税 13 / 价税 113
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "10", "unit_price": "10", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let id = r["id"].as_i64().unwrap();

    // 草稿订单不能发货（对标金蝶：执行前须确认）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": id, "period": 202601, "date": "2026-01-15", "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "草稿订单发货应被拒");

    // 确认订单
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认订单应成功");

    // 全量发货 → 收入凭证（借应收113 / 贷收入100 / 贷销项13）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": id, "period": 202601, "date": "2026-01-15", "qty": "10", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["voucher_id"].as_i64().expect("发货应确认收入凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "112201");
    assert_eq!(v["entries"][0]["aux"]["customer"], "C01");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 113.0);
    assert_eq!(v["entries"][1]["account_code"], "600101");
    assert_eq!(
        money_num(v["entries"][1]["credit"].as_str().unwrap()),
        100.0
    );
    assert_eq!(v["entries"][2]["account_code"], "22210102");
    assert_eq!(money_num(v["entries"][2]["credit"].as_str().unwrap()), 13.0);

    // 订单状态 → 已完成（全量发货）
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
    assert_eq!(row["status"], "Completed", "全量发货后订单应为已完成");

    // 超发不再确认：再发 5 → 无新凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": id, "period": 202601, "date": "2026-01-16", "qty": "5", "memo": "超发" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "超发不应重复确认收入：{r}");
    // 超发时**库存也不能多出**：账实必须一致
    {
        let db = state.db_for("b1").unwrap();
        let shipped: f64 = db
            .conn()
            .query_row(
                "SELECT COALESCE(SUM(ABS(qty)),0) FROM stock_move WHERE item='140301' AND qty<0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let ordered: f64 = row["lines"][0]["qty_ordered"]
            .as_str()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        assert!(
            shipped <= ordered + 1e-9,
            "累计出库 {shipped} 超过订购量 {ordered} —— 账实不符（超发只挡了收入没挡库存）"
        );
    }

    // 退货 4 → 冲回凭证（收入40 / 销项5.2 / 应收45.2）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/return",
            &sid,
            serde_json::json!({ "so_id": id, "period": 202601, "date": "2026-01-17", "qty": "4", "memo": "部分退货" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "退货应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rvid = r["voucher_id"].as_i64().expect("退货应生成冲回凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{rvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "600101");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 40.0);
    assert_eq!(v["entries"][1]["account_code"], "22210102");
    assert_eq!(money_num(v["entries"][1]["debit"].as_str().unwrap()), 5.2);
    assert_eq!(v["entries"][2]["account_code"], "112201");
    assert_eq!(money_num(v["entries"][2]["credit"].as_str().unwrap()), 45.2);
}

/// 委外加工全链（委外=生产的变体）：建单(kind=outsourcing) → BOM+标准价 → 领料 → 开工 →
/// 加工费凭证（借500102/贷应付-供应商）→ 完工结转含加工费；非委外单确认加工费 400。
#[tokio::test]
async fn outsourcing_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 当前期间与日期
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let d15 = format!("{cur_label}-15");
    let d16 = format!("{cur_label}-16");
    let d20 = format!("{cur_label}-20");

    // 标准价 + BOM（1 成品 = 2 原料）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 建委外订单（10 件，供应商 S01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({
                "item_code": "140501", "qty": "10", "kind": "outsourcing",
                "supplier_code": "S01", "supplier_name": "供应商甲", "date": d15.clone()
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建委外订单");
    let body = body_string(resp).await;
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let po_id = created["id"].as_i64().unwrap();

    // 列表带出委外标识与供应商
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(po_id))
        .unwrap();
    assert_eq!(row["order_kind"], "outsourcing");
    assert_eq!(row["supplier_name"], "供应商甲");

    // 领料（20 件 × 10 = 200）→ 开工
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/issue"),
            &sid,
            serde_json::json!({ "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "委外发料");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        money_num(r["total"].as_str().unwrap()),
        200.0,
        "发料成本 200"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工");

    // 加工费 300 → 凭证 借500102 / 贷应付(供应商辅助)
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/outsource-fee"),
            &sid,
            serde_json::json!({ "amount": "300", "date": d16.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "确认加工费");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let fee_vid = r["voucher_id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{fee_vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let dr = entries
        .iter()
        .find(|e| e["account_code"] == "500102")
        .expect("借 500102 直接人工");
    assert_eq!(money_num(dr["debit"].as_str().unwrap()), 300.0);
    let cr = entries
        .iter()
        .find(|e| money_num(e["credit"].as_str().unwrap()) == 300.0)
        .expect("贷应付 300");
    assert_eq!(cr["aux"]["supplier"], "S01", "应付带供应商辅助");

    // 非委外订单确认加工费 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "1", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    let plain: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let plain_id = plain["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{plain_id}/outsource-fee"),
            &sid,
            serde_json::json!({ "amount": "50", "date": d16.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "非委外订单不能确认加工费"
    );

    // 完工 10 件 → 结转 = 料 200 + 工 300 = 500（借 140501；贷 500101=200、500102=300）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/complete"),
            &sid,
            serde_json::json!({ "qty": "10", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "委外完工入库");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_str(&body_string(resp).await).unwrap();
    let comp = list
        .iter()
        .find(|x| {
            x["summary"]
                .as_str()
                .map(|s| s.contains("完工"))
                .unwrap_or(false)
        })
        .expect("应有完工凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers/{}", comp["id"].as_i64().unwrap()),
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let dr = entries
        .iter()
        .find(|e| e["account_code"] == "140501")
        .expect("借库存商品");
    assert_eq!(
        money_num(dr["debit"].as_str().unwrap()),
        500.0,
        "完工结转 = 料200+工300"
    );
    let l500101 = entries
        .iter()
        .find(|e| e["account_code"] == "500101")
        .expect("贷直接材料");
    assert_eq!(money_num(l500101["credit"].as_str().unwrap()), 200.0);
    let l500102 = entries
        .iter()
        .find(|e| e["account_code"] == "500102")
        .expect("贷直接人工（加工费）");
    assert_eq!(money_num(l500102["credit"].as_str().unwrap()), 300.0);
}
