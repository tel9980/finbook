//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

/// P0：固定资产三件套——变更历史 + 总账对账 + 处置清理凭证。
#[tokio::test]
async fn asset_p0_flow() {
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
    let num = |v: &serde_json::Value| -> f64 {
        v.as_str().unwrap().replace(',', "").parse::<f64>().unwrap()
    };

    // 建卡：160101 / 1602 / 660201，原值 12000，36 期，残值 5%
    let card = serde_json::json!({
        "id": 0, "code": "P0A01", "name": "测试设备", "category": "电子设备", "spec": "",
        "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
        "expense_account": "660201", "original_value": "12000", "residual_rate": "5",
        "life_months": 36, "method": "straight", "start_period": cur_ymm, "memo": ""
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/assets", &sid, card.clone()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建卡");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 资本化入账：借 160101 / 贷 1002（对账基准）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 91,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "160101", "summary": "购入资产", "debit": "12000", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "购入资产", "debit": "0", "credit": "12000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "资本化凭证：{body}");
    let cap_vid = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cap_vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资本化凭证记账");

    // 变更历史：改名 + 换部门 → 至少 2 条字段级记录
    let mut edited = card.clone();
    edited["id"] = serde_json::json!(id);
    edited["name"] = serde_json::json!("测试设备-改");
    edited["dept"] = serde_json::json!("生产部");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(&format!("/api/assets/{id}"), &sid, edited))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "改卡");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/assets/{id}/changes"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let fields: Vec<String> = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["field"].as_str().unwrap().to_string())
        .collect();
    assert!(
        fields.contains(&"名称".to_string()),
        "应记录名称变更：{fields:?}"
    );
    assert!(
        fields.contains(&"使用部门".to_string()),
        "应记录部门变更：{fields:?}"
    );

    // 计提折旧（草稿）→ 记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/depreciate",
            &sid,
            serde_json::json!({ "ymm": cur_ymm }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提折旧");
    let dep: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dep_vid = dep["voucher_id"].as_i64().expect("折旧凭证 id");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{dep_vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "折旧凭证记账");

    // 对账（清理前）：原值 12000/12000、累计折旧 316.67/316.67，双向对平
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/assets/gl-reconcile?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        (num(&r["cost_asset"]) - 12000.0).abs() < 0.005,
        "原值资产侧：{r}"
    );
    assert!(
        (num(&r["cost_gl"]) - 12000.0).abs() < 0.005,
        "原值总账侧：{r}"
    );
    assert!(num(&r["cost_diff"]).abs() < 0.005, "原值应平：{r}");
    assert!(
        (num(&r["dep_asset"]) - 316.67).abs() < 0.005,
        "累计折旧资产侧：{r}"
    );
    assert!(
        (num(&r["dep_gl"]) - 316.67).abs() < 0.005,
        "累计折旧总账侧：{r}"
    );
    assert!(num(&r["dep_diff"]).abs() < 0.005, "折旧应平：{r}");

    // 清理 → 转销凭证（借 1602 + 借 1606 净值 / 贷 160101）→ 记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/{id}/dispose"),
            &sid,
            serde_json::json!({ "ymm": cur_ymm, "amount": "1000" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "清理");
    let dis: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dis_vid = dis["voucher_id"].as_i64().expect("清理转销凭证 id");
    assert!(dis_vid > 0);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{dis_vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "转销凭证记账");

    // 对账（清理后）：资产侧与总账侧同时归零
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/assets/gl-reconcile?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        num(&r["cost_asset"]).abs() < 0.005,
        "清理后原值资产侧=0：{r}"
    );
    assert!(num(&r["cost_gl"]).abs() < 0.005, "清理后原值总账侧=0：{r}");
    assert!(
        num(&r["dep_asset"]).abs() < 0.005,
        "清理后折旧资产侧=0：{r}"
    );
    assert!(num(&r["dep_gl"]).abs() < 0.005, "清理后折旧总账侧=0：{r}");

    // 重复清理 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/{id}/dispose"),
            &sid,
            serde_json::json!({ "ymm": cur_ymm, "amount": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复清理应 400");
}

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

/// P1：合并报表（跨账套汇总）——两套独立取数按科目合计；非管理员 403；未知账套 400。
#[tokio::test]
async fn consolidate_books() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // b1：借 1001 100 / 贷 2001 100 → 记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记", "no": 95,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "合并造数", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "合并造数", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "b1 造数");
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
    assert_eq!(resp.status(), StatusCode::OK, "b1 记账");

    // 建 b2 并进入：借 1001 200 / 贷 2001 200 → 记账
    let (st, body) = create_book(&state, &sid, "第二公司").await;
    assert_eq!(st, StatusCode::OK, "建第二账套");
    let b2 = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        select_book(&state, &sid, &b2).await,
        StatusCode::OK,
        "进入 b2"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记", "no": 95,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "合并造数", "debit": "200", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "合并造数", "debit": "0", "credit": "200" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "b2 造数");
    let vid2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    // b2 走生产建账路径 → **两道闸门默认都开**（审核 + 出纳签字），与 b1 夹具不同。
    // 夹具在 test_state() 里显式关掉了两道闸门，所以 b1 那张不需要这两步 ——
    // 这也正是夹具必须显式声明、不能靠继承默认值的原因。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid2}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "b2 审核");
    // 出纳签字：b2 的凭证借记 1001 库存现金，生产默认开着出纳闸门。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid2}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(resp, "b2 出纳签字").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid2}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(resp, "b2 记账").await;

    // 合并汇总：1001 合计 300、2001 合计 -300（借正贷负）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/consolidate/preview?period=202601&books=b1,{b2}"),
            &sid,
        ))
        .await
        .unwrap();
    let st = resp.status();
    let body = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "合并汇总：{body}");
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["books"].as_array().unwrap().len(), 2, "两套：{r}");
    let row = |code: &str| {
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["account_code"] == code)
            .cloned()
            .unwrap()
    };
    let n = |v: &serde_json::Value| v.as_str().unwrap().replace(',', "").parse::<f64>().unwrap();
    assert!(
        (n(&row("1001")["total"]) - 300.0).abs() < 0.005,
        "1001 合计 300：{r}"
    );
    assert!(
        (n(&row("2001")["total"]) + 300.0).abs() < 0.005,
        "2001 合计 -300：{r}"
    );

    // 未知账套 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/consolidate/preview?period=202601&books=b1,nope",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知账套应 400");

    // 非管理员 → 403（先完成首登改密）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({ "username": "cu1", "display_name": "合并测试", "password": "Cu1@pass99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开号 cu1");
    let (_, cu_sid) = login(&state, "cu1", "Cu1@pass99").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &cu_sid,
            serde_json::json!({ "old": "Cu1@pass99", "new": "Cu1@pass99x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "cu1 改密");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/consolidate/preview?period=202601&books=b1",
            &cu_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "非管理员应 403");
}

/// P2：补口子——凭证作废/恢复、资产减值、资产盘点接线。
#[tokio::test]
async fn p2_gap_fill_flow() {
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

    // 1) 凭证作废 / 恢复（引擎口径：未记账可作废；已记账请先反记账）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 96,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "作废测试", "debit": "10", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "作废测试", "debit": "0", "credit": "10" }
                ]
            }),
        ))
        .await
        .unwrap();
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/void"),
            &sid,
            serde_json::json!({ "void": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "未记账作废");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["status"], "void", "作废后状态：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/void"),
            &sid,
            serde_json::json!({ "void": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "恢复作废");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["status"], "draft", "恢复后回到草稿：{r}");
    // 已记账 → 作废被引擎拒绝（先反记账）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/void"),
            &sid,
            serde_json::json!({ "void": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已记账应先反记账");

    // 2) 资产减值
    let card = serde_json::json!({
        "id": 0, "code": "P2A01", "name": "减值测试资产", "category": "电子设备", "spec": "",
        "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
        "expense_account": "660201", "original_value": "12000", "residual_rate": "5",
        "life_months": 36, "method": "straight", "start_period": cur_ymm, "memo": ""
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/assets", &sid, card))
        .await
        .unwrap();
    let aid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/{aid}/impair"),
            &sid,
            serde_json::json!({ "amount": "500", "period": cur_ymm, "memo": "测试减值" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资产减值");

    // 3) 资产盘点：盘亏 → 过账置停用
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/counts",
            &sid,
            serde_json::json!({ "period": cur_ymm, "date": d15, "memo": "测试盘点", "lines": [{ "asset_id": aid, "found": false }] }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建盘点单：{b}");
    let cid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/counts/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let pr: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(pr["lost"], 1, "盘亏 1 张：{pr}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/assets?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let a = r["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(aid))
        .unwrap();
    assert_eq!(a["status"], "idle", "盘亏后置停用：{a}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/assets/counts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"].as_i64() == Some(cid)),
        "盘点单列表应含：{r}"
    );
    // 未知资产 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/counts",
            &sid,
            serde_json::json!({ "period": cur_ymm, "lines": [{ "asset_id": 999999 }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知资产应 400");
}

/// P2：预算硬控制——预算行编制 + 凭证保存按 warn/strong 校验（执行口径=已记账）。
#[tokio::test]
async fn budget_control_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 编制预算行：660201 202601 预算 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/budget/rows",
            &sid,
            serde_json::json!({ "period": 202601, "account_code": "660201", "dept": "", "amount": "100", "memo": "测试" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "编制预算行");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/budget/rows?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1, "预算行列表：{r}");

    // 账套参数：预算控制 = warn
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    opts["budget_control"] = serde_json::json!("warn");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts.clone()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存参数（warn）");

    let save_voucher = |amount: &'static str| {
        let state = state.clone();
        let sid = sid.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_post(
                    "/api/vouchers",
                    &sid,
                    serde_json::json!({
                        "id": 0, "period": 202601, "date": "2026-01-15", "word": "记", "no": 0,
                        "attachments": 0, "memo": "", "entries": [
                            { "line": 1, "account_code": "660201", "summary": "预算测试", "debit": amount, "credit": "0" },
                            { "line": 2, "account_code": "1001", "summary": "预算测试", "debit": "0", "credit": amount }
                        ]
                    }),
                ))
                .await
                .unwrap();
            resp.status()
        }
    };

    // warn：超预算放行（120 > 100）
    assert_eq!(save_voucher("120").await, StatusCode::OK, "warn 超预算放行");
    // warn 下未超（50）也放行
    assert_eq!(save_voucher("50").await, StatusCode::OK, "warn 未超放行");

    // 切 strong：超预算拒绝（120），未超放行（50）
    opts["budget_control"] = serde_json::json!("strong");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存参数（strong）");
    assert_eq!(
        save_voucher("120").await,
        StatusCode::BAD_REQUEST,
        "strong 超预算拒绝"
    );
    assert_eq!(save_voucher("50").await, StatusCode::OK, "strong 未超放行");
}

/// 默认配置下审核是硬闸门：未审核不能记账；审核后可以；且错误信息要说清下一步。
#[tokio::test]
async fn audit_gate_blocks_post_until_audited() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    let (status, body) = create_book(&state, &admin_sid, "审核闸门").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let key = serde_json::from_str::<serde_json::Value>(&body).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(select_book(&state, &admin_sid, &key).await, StatusCode::OK);

    // 录一张草稿凭证（对方科目用 1001 库存现金，无需辅助核算）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &admin_sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记", "no": 1,
                "attachments": 0, "memo": "审核闸门用例",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "应收", "debit": "1000", "credit": "0",
                      "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "1001", "summary": "收现", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(r).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 草稿直接记账 → 400，且提示要先审核
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "未审核不应能记账");
    let msg = body_string(r).await;
    assert!(
        msg.contains("审核"),
        "错误信息应指向「先审核」这个下一步动作，而不是只说失败：{msg}"
    );

    // 审核通过后再记账 → 200
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/audit"),
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "审核应成功");
    // 本用例自建账套，走的是**生产默认**（两道闸门都开）。凭证贷记 1001 库存现金，
    // 所以审核过了还要过出纳签字那道闸 —— 补上这一步，用例才真的走完「记账」这条路。
    //
    // 顺带钉住一件事：出纳闸门**不是**审核的替代品。反过来（没审核就签字能不能记）
    // 由前面的 400 断言已经覆盖。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/sign"),
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(r, "出纳签字应成功").await;
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &admin_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(r, "审核 + 签字后应能记账").await;
}

/// 凭证列表分页不重不漏，且不泄露 v1 之外的数据
#[tokio::test]
async fn v1_vouchers_pagination() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(select_book(&state, &admin_sid, "b1").await, StatusCode::OK);

    // 录 7 张草稿。用「银行存款 → 库存现金」这类不需要辅助核算的科目对：
    // 分页用例的主题是分页，不该被辅助核算档案的初始化状态左右。
    for i in 1..=7 {
        let r = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                &admin_sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": format!("2026-01-{:02}", i),
                    "word": "记", "no": i, "attachments": 0, "memo": format!("分页用例{i}"),
                    "entries": [
                        { "line": 1, "account_code": "100201", "summary": "转存", "debit": "100", "credit": "0" },
                        { "line": 2, "account_code": "1001", "summary": "收现", "debit": "0", "credit": "100" }
                    ]
                }),
            ))
            .await
            .unwrap();
        let st = r.status();
        let body = body_string(r).await;
        assert_eq!(st, StatusCode::OK, "第 {i} 张应录入成功：{body}");
    }

    let key = issue_ro_key(&state, "b1").await;
    let mut seen: Vec<i64> = Vec::new();
    for page in 1..=3 {
        let (st, body) = v1_get(
            &state,
            &format!("/api/v1/vouchers?page={page}&page_size=3"),
            &key,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let v = serde_json::from_str::<serde_json::Value>(&body).unwrap();
        let items = v["items"].as_array().expect("items 应是数组");
        for it in items {
            seen.push(it["id"].as_i64().expect("凭证应有 id"));
        }
        if page < 3 {
            assert_eq!(items.len(), 3, "第 {page} 页应满页：{body}");
            assert_eq!(v["has_more"], serde_json::json!(true), "{body}");
        }
    }
    let uniq: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(seen.len(), 7, "三页共应 7 条，实际 {}", seen.len());
    assert_eq!(uniq.len(), 7, "分页结果不应重复：{seen:?}");

    // page_size 上限：page_size=99999 不该真的返回 99999 行
    let (st, body) = v1_get(&state, "/api/v1/vouchers?page_size=99999", &key).await;
    assert_eq!(st, StatusCode::OK);
    let v = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(
        v["page_size"],
        serde_json::json!(500),
        "page_size 应被夹到 500"
    );

    // 单张取回含分录；不存在 → 404
    // 这条同时是**参数段路由**的回归守卫：axum 0.7 的 matchit 只认 `:id`，
    // 写 `{id}` 会被当字面量，`/api/v1/vouchers/1` 匹配不上 → 掉进
    // spa_fallback → 未登录态下返回 401「未登录或会话已失效」。
    // 症状是「单张凭证查不到」，根因却在一个字符，且没有编译错、没有警告。
    let (st, body) = v1_get(&state, &format!("/api/v1/vouchers/{}", seen[0]), &key).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "单张凭证应查得到（参数段路由是否接上？）：{body}"
    );
    let v = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert_eq!(v["entries"].as_array().map(|a| a.len()), Some(2), "{body}");

    let (st, body) = v1_get(&state, "/api/v1/vouchers/999999", &key).await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "不存在的凭证应 404 而不是别的：{body}"
    );
}

/// 导入专项：各岗位基础资料（aux 重码/坏类型跳过、item 保质期+安全库存、account 类别推断、
/// opening_stock 数量+批次且不生成凭证）+ 模板下载 + 未知 kind 拒。
#[tokio::test]
async fn import_master_data() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_str = dash["current_period"].as_str().unwrap().to_string();

    // 模板下载：CSV + BOM；未知 kind 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/import/template?kind=aux", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "模板下载");
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("text/csv"), "CSV 类型：{ct}");
    let tpl = body_string(resp).await;
    assert!(tpl.starts_with('\u{feff}'), "模板带 BOM 供 Excel 打开");
    assert!(tpl.contains("类型,编码,名称,备注"), "aux 模板列头：{tpl}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/import/template?kind=bogus", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知模板 kind 拒");

    // aux 导入：2 行有效 + 1 行重码 + 1 行坏类型 → ok2/skipped2
    let aux_csv = "类型,编码,名称,备注\n客户,X01,新客户一,\n供应商,XS01,新供应商,\n客户,X01,重复编码,\n不明类型,BAD1,坏行,\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "aux", "template": "generic", "text": aux_csv }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "aux 导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 2, "有效 2 行：{r}");
    assert_eq!(r["skipped"], 2, "重码+坏类型各跳 1：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/aux?kind=customer", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r.as_array().unwrap().iter().any(|a| a["code"] == "X01"),
        "客户已建档：{r}"
    );
    // 幂等重导：全部跳过
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "aux", "template": "generic", "text": aux_csv }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 0, "重导幂等不重复：{r}");
    assert_eq!(r["skipped"], 4, "4 行全跳过：{r}");

    // item 导入：保质期入 props、安全库存入 item_plan
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "item", "template": "generic",
                "text": "编码,名称,保质期天,安全库存\nXI01,导入存货,30,50\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "item 导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 1, "{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/aux?kind=item", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let it = r
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["code"] == "XI01")
        .unwrap()
        .clone();
    assert_eq!(it["props"]["shelf_life_days"], "30", "保质期入 props：{it}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/item-plan?item=XI01", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["plan"]["safety_stock"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap(),
        50.0,
        "安全库存入 item_plan：{r}"
    );

    // account 导入：类别空按编码首位推（6→费用）；已存在跳过
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "account", "template": "generic",
                "text": "编码,名称,类别,方向,备注\n660099,导入测试费,,,\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "account 导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 1, "{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/accounts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let list = r
        .as_array()
        .cloned()
        .unwrap_or_else(|| r["rows"].as_array().cloned().unwrap_or_default());
    assert!(
        list.iter()
            .any(|a| a["code"] == "660099" && a["name"] == "导入测试费"),
        "科目已建档：{r}"
    );

    // 未知 kind → 400（per-kind 鉴权入口）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "bogus", "template": "generic", "text": "x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未知 kind 拒");

    // opening_stock：数量流水 + 批次建档（保质期30天推失效日）；**不生成凭证**
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "opening_stock", "template": "generic",
                "text": "存货编码,仓库,数量,单价,批次号,生产日期,备注\nXI01,W09,40,1.5,OB001,2026-01-01,\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "期初库存导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 1, "{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches?item=XI01", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bt = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["batch_no"] == "OB001")
        .unwrap()
        .clone();
    assert_eq!(bt["warehouse"], "W09", "期初批次建档：{bt}");
    assert_eq!(
        bt["expiry_date"], "2026-01-31",
        "失效日=生产日+保质期30：{bt}"
    );
    assert_eq!(
        bt["balance"].as_str().unwrap().parse::<f64>().unwrap(),
        40.0,
        "期初数量入流水：{bt}"
    );
    // 期初库存不生成凭证（金额侧由科目期初负责）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers?period={}", cur_str.replace('-', "")),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vlist = r
        .as_array()
        .cloned()
        .unwrap_or_else(|| r["rows"].as_array().cloned().unwrap_or_default());
    assert!(vlist.is_empty(), "期初库存不应生成凭证：{r}");
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

/// Web 凭证支持辅助核算/数量/单价，且编辑时不丢原行未提交的要素。
#[tokio::test]
async fn web_voucher_aux_qty_roundtrip_and_preserve() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 140301 原材料（存货辅助 + 数量核算）100 = 5 × 20；贷 2001 短期借款 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                "no": 1, "attachments": 0, "memo": "带辅助数量的凭证",
                "entries": [
                    {
                        "line": 1, "account_code": "140301", "summary": "采购原料",
                        "debit": "100", "credit": "0",
                        "aux": { "item": "RM01" }, "qty": "5", "price": "20"
                    },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "带辅助/数量的凭证应可保存");
    let body = body_string(resp).await;
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .expect("应返回凭证 id");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let e0 = &v["entries"][0];
    assert_eq!(
        e0["aux"]["item"],
        serde_json::json!("RM01"),
        "存货辅助应落库"
    );
    assert_eq!(e0["qty"], serde_json::json!("5"), "数量应落库");
    assert_eq!(e0["price"], serde_json::json!("20"), "单价应落库");

    // 模拟旧版前端：更新时不提交 aux/qty/price，原行要素必须保留
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": id, "period": 202601, "date": "2026-01-15", "word": "记",
                "no": 1, "attachments": 0, "memo": "旧前端编辑",
                "entries": [
                    { "line": 1, "account_code": "140301", "summary": "采购原料", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "旧前端编辑应兼容");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["entries"][0]["aux"]["item"],
        serde_json::json!("RM01"),
        "未提交的辅助核算不能被清空"
    );
    assert_eq!(
        v["entries"][0]["qty"],
        serde_json::json!("5"),
        "未提交的数量不能被清空"
    );
}

/// Web 期末处理：结转损益 → 记账结转凭证 → 结账 → 反结账。
#[tokio::test]
async fn web_period_carry_close_unclose_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 6401 主营业务成本 100 / 贷 1001 库存现金 100（产生损益发生额）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20", "word": "记",
                "no": 1, "attachments": 0, "memo": "结转测试",
                "entries": [
                    { "line": 1, "account_code": "6401", "summary": "成本", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "付款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "业务凭证应能记账");

    // 结账前预检：应提示需要先结转损益（无未记账凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/periods/202601/precheck", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let chk: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        chk["pl_count"].as_i64().unwrap_or(0) > 0,
        "应有损益科目可结转"
    );

    // 结转损益 → 生成一张待记账的结转凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/carry-forward",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结转损益应成功");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结转凭证应能记账");

    // 结账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/close",
            &sid,
            serde_json::json!({ "require_carry": true }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "结转并记账后应能结账：{}",
        body_string(resp).await
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/periods", &sid))
        .await
        .unwrap();
    let periods: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        periods["closed_upto"],
        serde_json::json!("2026-01"),
        "结账线应推进"
    );

    // 反结账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/unclose",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "反结账应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/periods", &sid))
        .await
        .unwrap();
    let periods: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(periods["closed_upto"].is_null(), "反结账后不应有结账线");
}

/// 可选审核环节：启用后 未记账→已审核→已记账；未审核不能记账、已审核不能改/删。
#[tokio::test]
async fn web_optional_audit_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 启用审核环节（先读回 options 再改，避免覆盖其他字段）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    opts["enable_audit"] = serde_json::json!(true);
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "启用审核环节应成功");

    // 保存一张凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "审核流",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未审核不能记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未审核不应允许记账");

    // 审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核应成功");

    // 已审核不能直接修改
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": id, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "改一下",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "改", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "改", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已审核凭证不应允许修改"
    );

    // 审核后可以记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核后应能记账");

    // 已记账不能反审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已记账不应允许反审核"
    );

    // 反记账 → 反审核 → 可修改
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/unpost"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "反审核应成功");
}

/// Web 外币分录：币种/汇率/原币金额可存取（引擎校验 原币×汇率≈本位币）。
#[tokio::test]
async fn web_foreign_currency_entry_roundtrip() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 新建一个美元科目（8888，不能与内置科目冲突）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/accounts",
            &sid,
            serde_json::json!({
                "account": {
                    "code": "8888", "name": "美元户", "category": "asset", "dir": "debit",
                    "aux": 0, "unit": null, "currency": "USD", "has_qty": false,
                    "is_cash": false, "is_bank": false, "cash_flow_item": null,
                    "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
                },
                "aux_kinds": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增美元科目应成功");

    // 借 8888 原币 100 × 7.2 = 720；贷 2001 720
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-18", "word": "记",
                "no": 1, "attachments": 0, "memo": "外币收款",
                "entries": [
                    {
                        "line": 1, "account_code": "8888", "summary": "美元收款",
                        "debit": "720", "credit": "0",
                        "currency": "USD", "rate": "7.2", "amount_for": "100"
                    },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "720" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "外币凭证应可保存");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let e0 = &v["entries"][0];
    assert_eq!(e0["currency"], serde_json::json!("USD"), "币种应落库");
    assert_eq!(e0["rate"], serde_json::json!("7.2"), "汇率应落库");
    assert_eq!(
        e0["amount_for"],
        serde_json::json!("100.00"),
        "原币金额应落库"
    );

    // 原币 × 汇率与金额不符时应拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-19", "word": "记",
                "no": 2, "attachments": 0, "memo": "错汇率",
                "entries": [
                    {
                        "line": 1, "account_code": "8888", "summary": "美元收款",
                        "debit": "700", "credit": "0",
                        "currency": "USD", "rate": "7.2", "amount_for": "100"
                    },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "700" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "原币×汇率不符应被拒"
    );
}

/// Web 固定资产：建卡 → 计提折旧生成凭证 → 幂等 → 清理。
#[tokio::test]
async fn web_assets_depreciate_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets",
            &sid,
            serde_json::json!({
                "code": "GD0001", "name": "台式电脑", "category": "电子设备", "spec": "",
                "dept": "财务部", "asset_account": "160101", "dep_account": "1602",
                "expense_account": "660201", "original_value": "12000",
                "residual_rate": "5", "life_months": 36, "method": "straight",
                "start_period": 202601, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增资产卡片应成功");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 计提 2026-01 折旧：12000 × 95% / 36 = 316.67
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/depreciate",
            &sid,
            serde_json::json!({ "ymm": 202601 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提折旧应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["already"], serde_json::json!(false));
    assert_eq!(r["total"], serde_json::json!("316.67"));
    let vid = r["voucher_id"].as_i64().expect("应返回折旧凭证 id");

    // 折旧凭证借贷平衡
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let sum = |k: &str| {
        entries
            .iter()
            .map(|e| e[k].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0))
            .sum::<f64>()
    };
    assert!(
        (sum("debit") - sum("credit")).abs() < 0.005,
        "折旧凭证应借贷平衡"
    );

    // 幂等：再次计提不生成新凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/assets/depreciate",
            &sid,
            serde_json::json!({ "ymm": 202601 }),
        ))
        .await
        .unwrap();
    let r2: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r2["already"], serde_json::json!(true), "重复计提应幂等");
    assert_eq!(r2["voucher_id"].as_i64(), Some(vid), "幂等应返回原凭证");

    // 清理后再计提不再包含该资产
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/assets/{id}/dispose"),
            &sid,
            serde_json::json!({ "ymm": 202601, "amount": "1000" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资产清理应成功");
}

/// Web 总账/日记账接口与 CSV 导出。
#[tokio::test]
async fn web_ledger_tabs_and_export() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-12", "word": "记",
                "no": 1, "attachments": 0, "memo": "账簿测试",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收现", "debit": "300", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "300" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 总账
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/ledger/general?code=1001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let gl: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        gl.as_array().unwrap().len(),
        1,
        "总账应按期间汇总一行：{gl}"
    );
    assert_eq!(gl[0]["debit"], serde_json::json!("300.00"));

    // 日记账（对方科目）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/ledger/journal?code=1001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let jr: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(jr.as_array().unwrap().len(), 1, "日记账应一行");
    assert!(
        jr[0]["opposite_accounts"]
            .as_str()
            .unwrap_or("")
            .contains("短期借款"),
        "日记账应带对方科目：{jr}"
    );

    // 导出凭证 CSV
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/export/vouchers?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("text/csv"),
        "应返回 CSV"
    );
    let body = body_string(resp).await;
    assert!(
        body.contains("凭证号") && body.contains("库存现金"),
        "CSV 应含表头与科目名：{body}"
    );

    // 导出明细账 CSV（含数量列与否均可）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/export/ledger?code=1001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("日期") && body.contains("余额"),
        "明细账 CSV 应含表头：{body}"
    );
}

/// Web 凭证附件：multipart 上传 → 列表 → 下载 → 删除。
#[tokio::test]
async fn web_voucher_attachment_roundtrip() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-08", "word": "记",
                "no": 1, "attachments": 0, "memo": "附件测试",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "50", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "50" }
                ]
            }),
        ))
        .await
        .unwrap();
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // multipart 上传
    let boundary = "----finbooktest";
    let payload = format!(
        "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"receipt.txt\"\r\nContent-Type: text/plain\r\n\r\nhello attachment\r\n--{b}--\r\n",
        b = boundary
    );
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/vouchers/{id}/attachments"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::COOKIE, &sid)
        .body(Body::from(payload))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "上传附件应成功");
    let aid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 列表
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}/attachments"), &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "应有 1 个附件：{list}");
    assert_eq!(list[0]["name"], serde_json::json!("receipt.txt"));

    // 下载
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/attachments/{aid}"), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "hello attachment");

    // 删除
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/attachments/{aid}"))
        .header(header::COOKIE, &sid)
        .body(Body::empty())
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除附件应成功");
}

/// 回归：第 201 张及以后的凭证必须能被界面访问到。
///
/// 老端点 limit 默认 200，UI 用的是老端点，所以第 201 张开始就是死数据。
/// 这里用 205 张（略微超过 200）而不是刚好 200：边界值测不出来，
/// 差一张才说明分页真的接上了。
#[tokio::test]
async fn voucher_page_reaches_past_the_old_200_cap() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 205).await;

    // 最后一页（205 张，page_size=50 → 第 5 页）必须有内容
    let p5 = get_voucher_page(&state, &sid, "period=202601&page=5&page_size=50").await;
    assert_eq!(p5["total"], 205, "total 应是全量张数，不是本页数");
    let items = p5["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        5,
        "第 5 页应有 205-200=5 张，实际 {}",
        items.len()
    );

    // 老端点确实只给 200（证明这个测试对应的是真实缺陷，而不是我臆想的）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/vouchers?period=202601", &sid))
        .await
        .unwrap();
    let old = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap();
    assert_eq!(
        old.as_array().unwrap().len(),
        200,
        "老端点仍按 limit=200 截断"
    );
}

/// 作废凭证不进合计（与账簿口径一致），但仍可在列表里查到。
#[tokio::test]
async fn voucher_page_totals_exclude_void() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 3).await;
    // 作废其中一张
    let p = get_voucher_page(&state, &sid, "period=202601&page_size=10").await;
    let id = p["items"].as_array().unwrap()[0]["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/void"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let after = get_voucher_page(&state, &sid, "period=202601&page_size=10").await;
    assert_eq!(after["total"], 3, "作废的仍在列表里（能查到）");
    assert_eq!(
        money_num(after["debit_total"].as_str().unwrap()),
        200.0,
        "作废的 100 不应计入合计"
    );
}

/// 参数非法必须 400，不能静默当第 1 页。
///
/// 静默容错在浏览器端是优点，在 API 端是 bug：`page=abc` 返回首页的话，
/// 调用方会拿到「看起来正常但其实是首页」的数据，漏拉极难发现。
#[tokio::test]
async fn voucher_page_rejects_bad_params() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_draft_vouchers(&state, &sid, 2).await;

    for qs in [
        "period=202601&page=abc",
        "period=202601&page_size=xyz",
        "period=202601&page=0x",
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/vouchers/page?{qs}"), &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{qs} 应 400");
    }
    // page_size 超上限要被夹住而不是报错（缺省≠非法）
    let big = get_voucher_page(&state, &sid, "period=202601&page_size=99999").await;
    assert_eq!(big["page_size"], 500, "page_size 应夹到上限 500");
    // 缺省用默认值
    let def = get_voucher_page(&state, &sid, "period=202601").await;
    assert_eq!(def["page"], 1);
    assert_eq!(def["page_size"], 50);
}

/// 分页的 total 必须与数据范围一致：受限用户看到的张数更少。
#[tokio::test]
async fn voucher_page_total_respects_data_scope() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    // 两批各 6 张：一批借贷两侧都在 1xxx（落在范围 1001..1999 内），
    // 一批在 6xxx（越界）。
    // 两侧都必须在范围内才有意义 —— `can_see_voucher` 是「全部科目都在范围内
    // 才可见」，只把借方放进范围、贷方留在 2001 的话**连范围内那批也会被
    // 全部滤掉**（我第一版就是这么写的，测出来 total=0）。
    for (no_base, debit, credit) in [(1, "1001", "1901"), (101, "660201", "600101")] {
        for k in 0..6i32 {
            let resp = handlers::router(state.clone())
                .oneshot(authed_post(
                    "/api/vouchers",
                    &sid,
                    serde_json::json!({
                        "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                        "no": no_base + k, "attachments": 0,
                        "memo": format!("范围 {debit}"),
                        "entries": [
                            { "line": 1, "account_code": debit, "summary": debit, "debit": "100", "credit": "0" },
                            { "line": 2, "account_code": credit, "summary": credit, "debit": "0", "credit": "100" }
                        ]
                    }),
                ))
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "{debit}/{credit} 第 {k} 张应保存成功"
            );
        }
    }

    // 管理员不限范围：12 张
    let all = get_voucher_page(&state, &sid, "period=202601&page_size=100").await;
    assert_eq!(all["total"], 12, "管理员应看到全部 12 张");

    // 建一个只允许看 1xxx 的普通账号。两层账号：先在平台层开通，再由 boss
    // 拉进 b1（直接 POST /api/users 会 400「该账号尚未开通」），最后收紧数据范围。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({
                "username": "scoped", "display_name": "受限", "password": "Scoped@123456"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "开通平台账号：{}",
        body_string(resp).await
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "scoped", "display_name": "受限", "password": "Scoped@123456",
                "role": "accountant", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "拉进 b1：{}",
        body_string(resp).await
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/scoped",
            &sid,
            serde_json::json!({ "data_scope": { "account_from": "1001", "account_to": "1999" } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "收紧数据范围应成功");

    // 平台层开通的账号默认 must_change_pwd，不改密就进不了账套（401）。
    let (st, sid0) = login(&state, "scoped", "Scoped@123456").await;
    assert_eq!(st, StatusCode::OK, "scoped 首登");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid0,
            serde_json::json!({ "old": "Scoped@123456", "new": "Scoped!23456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "首登改密");
    let (st, sid2) = login(&state, "scoped", "Scoped!23456").await;
    assert_eq!(st, StatusCode::OK, "scoped 改密后重登");
    assert_eq!(select_book(&state, &sid2, "b1").await, StatusCode::OK);
    let scoped = get_voucher_page(&state, &sid2, "period=202601&page_size=100").await;
    assert_eq!(
        scoped["total"], 6,
        "数据范围限定 1001..1999 时应只看到 6 张（借贷都在 1xxx 的那批）"
    );
    for it in scoped["items"].as_array().unwrap() {
        // item.summary 取的是**第一条分录**的摘要（Voucher::first_summary），
        // 所以这里用它来分辨批次：范围内 = 1001，越界那批 = 660201。
        assert_eq!(it["summary"], "1001", "越界的那批不该出现");
    }
    // 合计也跟着收窄（这是 total 必须与数据范围同源的直接证据）
    assert_eq!(money_num(scoped["debit_total"].as_str().unwrap()), 600.0);
}

/// 部门范围同样要下推到 SQL：`total` 必须与 `can_see_voucher` 同口径，否则
/// 「共 6 张」却只翻得出 3 张（`list_vouchers_page` 的 debug_assert 会直接炸）。
///
/// 关键语义：**没打部门标签的凭证仍然可见**。本厂大部分凭证不填部门，
/// 若把这类凭证判成越界，部门会计会凭空少掉一半账。
#[tokio::test]
async fn voucher_page_total_respects_dept_scope() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 三批各 3 张，借贷两侧打同一个部门标签：D01 / D02 / 不打标签。
    for (no_base, dept) in [(1, Some("D01")), (11, Some("D02")), (21, None)] {
        for k in 0..3i32 {
            let label = dept.unwrap_or("未打标签");
            let mut e1 = serde_json::json!({
                "line": 1, "account_code": "660201", "summary": label,
                "debit": "100", "credit": "0"
            });
            let mut e2 = serde_json::json!({
                "line": 2, "account_code": "1001", "summary": "现金",
                "debit": "0", "credit": "100"
            });
            if let Some(d) = dept {
                e1["aux"] = serde_json::json!({ "dept": d });
                e2["aux"] = serde_json::json!({ "dept": d });
            }
            let resp = handlers::router(state.clone())
                .oneshot(authed_post(
                    "/api/vouchers",
                    &sid,
                    serde_json::json!({
                        "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                        "no": no_base + k, "attachments": 0,
                        "memo": format!("部门 {} 第 {k} 张", label),
                        "entries": [e1, e2]
                    }),
                ))
                .await
                .unwrap();
            let status = resp.status();
            let body = body_string(resp).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{label} 第 {k} 张应保存成功：{body}"
            );
            let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
                .as_i64()
                .expect("保存应返回凭证 id");
            // 明细账/余额表默认只算已记账凭证（posted_only=true），不记账查到的是空表
            let resp = handlers::router(state.clone())
                .oneshot(authed_post(
                    &format!("/api/vouchers/{id}/post"),
                    &sid,
                    serde_json::json!({}),
                ))
                .await
                .unwrap();
            assert_ok(resp, &format!("{label} 第 {k} 张应记账成功")).await;
        }
    }

    // 管理员不限部门：9 张
    let all = get_voucher_page(&state, &sid, "period=202601&page_size=100").await;
    assert_eq!(all["total"], 9, "管理员应看到全部 9 张");

    // 建一个只允许看 D01 的会计（两层账号：先平台开通，再拉进 b1，最后设范围）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({
                "username": "deptscoped", "display_name": "部门会计", "password": "Dept@123456"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "开通平台账号：{}",
        body_string(resp).await
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "deptscoped", "display_name": "部门会计", "password": "Dept@123456",
                "role": "accountant", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "拉进 b1：{}",
        body_string(resp).await
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/deptscoped",
            &sid,
            serde_json::json!({ "data_scope": { "depts": ["D01"] } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "设部门范围应成功");

    // 平台层开通的账号默认 must_change_pwd，不改密就进不了账套（401）。
    let (st, sid0) = login(&state, "deptscoped", "Dept@123456").await;
    assert_eq!(st, StatusCode::OK, "deptscoped 首登");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid0,
            serde_json::json!({ "old": "Dept@123456", "new": "Dept!23456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "首登改密");
    let (st, sid2) = login(&state, "deptscoped", "Dept!23456").await;
    assert_eq!(st, StatusCode::OK, "改密后重登");
    assert_eq!(select_book(&state, &sid2, "b1").await, StatusCode::OK);

    let scoped = get_voucher_page(&state, &sid2, "period=202601&page_size=100").await;
    assert_eq!(
        scoped["total"], 6,
        "部门范围 D01 时应看到 6 张（D01 那批 + 没打标签那批）"
    );
    for it in scoped["items"].as_array().unwrap() {
        assert_ne!(it["summary"], "D02", "别的部门那批不该出现");
    }
    assert_eq!(money_num(scoped["debit_total"].as_str().unwrap()), 600.0);

    // ── 账簿口径 vs 汇总口径（v1.8.62 定案）──────────────────────────
    // 明细账/总账是「逐笔」类入口，和凭证列表同粒度，必须一起收窄；
    // 科目余额表/试算是公司级控制表，一直全公司——按部门收窄会把一张平表算成不平：
    // 真实凭证的对方分录（银行/往来）不挂部门，只有费用侧挂。
    // /api/ledger 直接返回数组
    let ledger = get_json(
        &state,
        &sid2,
        "/api/ledger?code=660201&from=202601&to=202601",
    )
    .await;
    let rows = ledger.as_array().expect("明细账应返回数组").clone();
    // 本部门 3 笔 + 没打标签 3 笔（无标签按「全公司」放行）= 6，D02 那批必须消失
    assert_eq!(
        rows.len(),
        6,
        "部门会计在 660201 的明细账应收窄为本部门 + 无标签：{ledger}"
    );
    let d01 = rows
        .iter()
        .filter(|r| r["aux"]["dept"].as_str() == Some("D01"))
        .count();
    assert_eq!(d01, 3, "本部门那 3 笔必须在：{ledger}");
    for r in &rows {
        assert_ne!(r["summary"], "D02", "别的部门那批不该出现在明细账：{r}");
    }
    let admin_ledger = get_json(
        &state,
        &sid,
        "/api/ledger?code=660201&from=202601&to=202601",
    )
    .await;
    assert_eq!(
        admin_ledger.as_array().unwrap().len(),
        9,
        "管理员明细账应含全部 9 笔"
    );

    let tb = get_json(
        &state,
        &sid2,
        "/api/reports/trial-balance?from=202601&to=202601",
    )
    .await;
    assert_eq!(
        tb["totals"]["debit"], tb["totals"]["credit"],
        "余额表/试算必须保持公司口径且借贷平衡：{}",
        tb["totals"]
    );
    assert_eq!(
        tb["totals"]["debit"], "900.00",
        "余额表不该按部门收窄（公司口径 9 张 × 100）：{}",
        tb["totals"]
    );
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

/// 出纳签字：签字/取消/幂等 + 权限（CashierSign），只读账号应被拒。
#[tokio::test]
async fn cashier_sign_and_unsign() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造一张现金凭证（草稿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 90, "attachments": 0, "memo": "签字流",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "付", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿凭证应可保存");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 签字 → 凭证详情带签字人；重复签字幂等
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "签字应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重复签字应幂等");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["cashier"].as_str(),
        Some("boss"),
        "凭证详情应带签字人：{v}"
    );

    // 取消签字 → 清空
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/unsign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消签字应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{id}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(v["cashier"].is_null(), "取消后签字人应清空：{v}");

    // 只读账号（无 CashierSign）→ 403
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({
                "username": "nosign", "display_name": "nosign", "password": "Init@123456"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开通 nosign 平台账号");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "nosign", "display_name": "nosign", "password": "",
                "role": "viewer", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请 nosign 为 viewer");
    let (st, vsid) = login(&state, "nosign", "Init@123456").await;
    assert_eq!(st, StatusCode::OK, "nosign 平台登录");
    // 平台账号默认首登强制改密；改密前只能访问改密/退出/登录（服务端拦截）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &vsid,
            serde_json::json!({ "old": "Init@123456", "new": "Pass123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "nosign 首登改密");
    assert_eq!(
        select_book(&state, &vsid, "b1").await,
        StatusCode::OK,
        "nosign 进入 b1"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/sign"),
            &vsid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "只读账号无出纳签字权限"
    );
}

/// require_cashier：开启后现金/银行凭证须签字才能记账；非资金凭证不受限；
/// 签字人出现在出纳日记账。
#[tokio::test]
async fn require_cashier_gates_post_and_scopes_to_funds() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 开启出纳签字前置（先读回再改，避免覆盖其他字段）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/options", &sid))
        .await
        .unwrap();
    let mut opts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    opts["require_cashier"] = serde_json::json!(true);
    let resp = handlers::router(state.clone())
        .oneshot(authed_put("/api/options", &sid, opts))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开启出纳签字前置应成功");

    // 现金凭证：未签字 → 记账被拒（提示含"出纳签字"）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-11", "word": "记",
                "no": 91, "attachments": 0, "memo": "现金待签字",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "付", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "未签字的现金凭证不应允许记账"
    );
    assert!(
        body_string(resp).await.contains("出纳签字"),
        "错误信息应提示出纳签字"
    );

    // 签字后可记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/sign"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "签字应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "签字后应能记账");

    // 出纳日记账应带签字人
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/ledger/journal?code=1001&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let rows: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        rows[0]["cashier"].as_str(),
        Some("boss"),
        "出纳日记账应显示签字人：{rows}"
    );

    // 非资金凭证（1901 待处理财产损溢 / 2001 短期借款）不受签字限制
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-12", "word": "记",
                "no": 92, "attachments": 0, "memo": "非资金不签字",
                "entries": [
                    { "line": 1, "account_code": "1901", "summary": "盘亏", "debit": "50", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "转", "debit": "0", "credit": "50" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "非资金凭证应可保存");
    let nid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{nid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "非资金凭证不受出纳签字限制");
}

/// M-15 定案：借贷不平衡的凭证 Web 端必须 400 拒绝，错误信息说明借贷不平衡。
#[tokio::test]
async fn unbalanced_voucher_rejected_m15() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-11", "word": "记",
                "no": 1, "attachments": 0, "memo": "不平衡",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "99.99" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "借贷不平衡必须被拒");
    let body = body_string(resp).await;
    assert!(body.contains("借贷不平衡"), "{body}");
}

/// 凭证流转：批量记账 / 红冲 / 删除 / 断号重排 / 取号。
#[tokio::test]
async fn web_voucher_state_ops() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 三张草稿
    let mut ids = Vec::new();
    for no in 1..=3 {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                &sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                    "no": no, "attachments": 0, "memo": "t",
                    "entries": [
                        { "line": 1, "account_code": "1001", "summary": "借", "debit": "10", "credit": "0" },
                        { "line": 2, "account_code": "2001", "summary": "贷", "debit": "0", "credit": "10" }
                    ]
                }),
            ))
            .await
            .unwrap();
        ids.push(
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
                .as_i64()
                .unwrap(),
        );
    }

    // 批量记账前两张
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers/batch-post",
            &sid,
            serde_json::json!({ "ids": [ids[0], ids[1]] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], serde_json::json!(2), "应批量记账 2 张：{r}");

    // 红冲一张已记账凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{}/reverse", ids[0]),
            &sid,
            serde_json::json!({ "period": 202601, "date": "2026-01-16" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "红冲应成功");
    let rid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_ne!(rid, ids[0], "红冲应生成新凭证");

    // 删除第三张草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{}/delete", ids[2]),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除草稿应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{}", ids[2]), &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "删除后应查不到");

    // 断号重排与取号
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers/renumber",
            &sid,
            serde_json::json!({ "period": 202601, "word": "记" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重排断号应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/vouchers/next-no?period=202601&word=记",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let n: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(n["no"].as_i64().unwrap_or(0) >= 1, "取号应返回正整数：{n}");
}

/// 预设岗位拆权：订单专员只动订单（动不了科目与凭证）；仓管员只动仓储（动不了订单与凭证）。
#[tokio::test]
async fn role_presets_order_clerk_and_keeper() {
    let (state, _bd, _dir) = test_state();
    let admin = boss_in_b1(&state).await;

    // 开平台账号 → 邀请进 b1（口令沿用平台）
    for (u, name, role) in [
        ("ord1", "订单专员", "order_clerk"),
        ("wh1", "仓管员", "keeper"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &admin,
                serde_json::json!({ "username": u, "display_name": name, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台账号 {u}");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &admin,
                serde_json::json!({
                    "username": u, "display_name": name, "password": "",
                    "role": role, "must_change_pwd": false,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {role} 应成功");
    }

    // 登录 + 平台首登强制改密 + 进账套
    let mut sids: Vec<String> = Vec::new();
    for u in ["ord1", "wh1"] {
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
        sids.push(sid);
    }
    let clerk = sids[0].clone();
    let keeper = sids[1].clone();

    // 订单专员：能建订单 / 报价（OrderOps）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &clerk,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "2", "unit_price": "10", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单专员应能建销售订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &clerk,
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
    assert_eq!(resp.status(), StatusCode::OK, "订单专员应能建报价单");

    // 订单专员：动不了科目（AccountEdit 分离的核心断言）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/accounts",
            &clerk,
            serde_json::json!({
                "account": {
                    "code": "88881", "name": "订单员不该建的科目", "category": "asset", "dir": "debit",
                    "aux": 0, "unit": null, "currency": null, "has_qty": false,
                    "is_cash": false, "is_bank": false, "cash_flow_item": null,
                    "bs_item": null, "pl_item": null, "disabled": false, "memo": ""
                },
                "aux_kinds": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应能维护科目"
    );

    // 两张单都动不了凭证（无 VoucherNew）
    let voucher_payload = || {
        serde_json::json!({
            "id": 0, "period": 202601, "date": "2026-01-31", "word": "记",
            "no": 77, "attachments": 0, "memo": "",
            "entries": [
                { "line": 1, "account_code": "1001", "summary": "x", "debit": "10", "credit": "0" },
                { "line": 2, "account_code": "2001", "summary": "x", "debit": "0", "credit": "10" }
            ],
        })
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &clerk, voucher_payload()))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应能手工录凭证"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &keeper, voucher_payload()))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "仓管员不应能手工录凭证"
    );

    // 仓管员：能读仓储数据（Warehouse）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/unit?item=140301", &keeper))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "仓管员应能读多单位配置");
    // 存货盘点 = 仓储作业：仓管可见、订单专员不可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &keeper))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "仓管员应能进盘点");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "订单专员不应能进盘点");
    // 批次库位 = 仓储作业
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches", &keeper))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "仓管员应能看批次");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/batches", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "订单专员不应能看批次");

    // 仓管员：动不了订单（反向隔离）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &keeper,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "1", "unit_price": "1", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "仓管员不应能建订单");

    // 订单专员：动不了仓储作业（反向隔离）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/assemble",
            &clerk,
            serde_json::json!({ "parent": "5001", "children": [["500101", "1"]], "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应能组装拆卸"
    );
}

/// 存货核算↔总账对账：采购到货（有流水无凭证）差异 90 → 暂估凭证记账后对平。
#[tokio::test]
async fn gl_reconcile_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let d15 = format!("{cur_label}-15");
    let d10 = format!("{cur_label}-10");

    // 带价采购 9×10 → 到货（库存金额 90，尚无总账凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "对账造数",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d15.clone(), "qty": "10", "memo": "入库" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到货入库");

    // 对账：140301 库存 90 / 总账 0 → 差异 90（未暂估）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/cost/gl-reconcile?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "对账端点可用");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item"] == "140301")
        .expect("对账表应含 140301");
    assert_eq!(
        money_num(row["stock_value"].as_str().unwrap()),
        90.0,
        "库存侧 90"
    );
    assert_eq!(
        money_num(row["gl_value"].as_str().unwrap()),
        0.0,
        "总账侧尚未有凭证"
    );
    assert_eq!(money_num(row["diff"].as_str().unwrap()), 90.0, "差异 90");
    assert_eq!(money_num(r["diff_total"].as_str().unwrap()), 90.0);

    // 手工暂估凭证（借 140301 带存货辅助 / 贷应付）→ 记账后入余额 → 对平
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d10.clone(), "word": "记", "no": 1,
                "attachments": 0, "memo": "暂估入库",
                "entries": [
                    { "line": 1, "account_code": "140301", "summary": "暂估入库",
                      "debit": "90", "credit": "0", "qty": "10", "price": "9",
                      "aux": { "item": "140301" } },
                    { "line": 2, "account_code": "220201", "summary": "暂估入库",
                      "debit": "0", "credit": "90", "aux": { "supplier": "S01" } }
                ]
            }),
        ))
        .await
        .unwrap();
    let vb = body_string(resp).await;
    let vid = serde_json::from_str::<serde_json::Value>(&vb).unwrap()["id"]
        .as_i64()
        .unwrap_or(0);
    assert!(vid > 0, "RECON_VB={vb}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账（余额口径=已记账）");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/cost/gl-reconcile?period={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item"] == "140301")
        .unwrap();
    assert_eq!(
        money_num(row["gl_value"].as_str().unwrap()),
        90.0,
        "总账侧 90"
    );
    assert_eq!(money_num(row["diff"].as_str().unwrap()), 0.0, "对平");
    assert_eq!(
        money_num(r["diff_total"].as_str().unwrap()),
        0.0,
        "总差异归零"
    );
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

/// Ctrl+K 快速搜索：凭证/请购/报销按关键词分域返回 + 空词与无结果边界。
#[tokio::test]
async fn quick_search_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 空词 → 空结果
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/quick-search?q=", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 0, "空词不搜");

    // 造请购（品名关键词）→ 搜到 req 类
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/req",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-09",
                "item_code": "RM9", "item_name": "快搜物料", "qty": "5",
                "status": "draft", "requester": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/quick-search?q=%E5%BF%AB%E6%90%9C%E7%89%A9%E6%96%99",
            &sid,
        )) // 快搜物料
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "req" && x["view"] == "po-doc"),
        "应搜到请购：{r}"
    );

    // 造凭证（memo+摘要关键词）→ 搜到 voucher 类
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": format!("{cur_label}-06"),
                "word": "记", "no": 1, "attachments": 0, "memo": "快搜凭证摘录",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "快搜凭证摘录", "debit": "20", "credit": "0" },
                    { "line": 2, "account_code": "660201", "summary": "快搜凭证摘录", "debit": "0", "credit": "20" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/quick-search?q=%E5%BF%AB%E6%90%9C%E5%87%AD%E8%AF%81",
            &sid,
        )) // 快搜凭证
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "voucher" && x["view"] == "vouchers"),
        "应搜到凭证：{r}"
    );

    // 造报销（事由关键词）→ 搜到 claim 类（数据范围内）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claims",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "biz_date": format!("{cur_label}-07"),
                "applicant": "张三", "dept": "销售部", "reason": "快搜报销事由",
                "amount": "88",
                "items": [{ "expense_account": "660201", "amount": "88", "memo": "" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造报销");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/quick-search?q=%E5%BF%AB%E6%90%9C%E6%8A%A5%E9%94%80",
            &sid,
        )) // 快搜报销
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["kind"] == "claim" && x["view"] == "claims"),
        "应搜到报销：{r}"
    );

    // 无结果 → 200 空数组
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/quick-search?q=zzzz-not-exist", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 0, "无结果不报错");
}

/// 我的工作台：权限分域矩阵 + 多期趋势长度 + 待办计数联动。
#[tokio::test]
async fn workbench_role_matrix() {
    let (state, _bd, _dir) = test_state();
    let admin = boss_in_b1(&state).await;

    // admin：全域 + 趋势期数 = 6
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workbench?periods=6", &admin))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "工作台端点应可用");
    let wb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let mut doms: Vec<&str> = wb["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["domain"].as_str().unwrap())
        .collect();
    doms.sort();
    doms.dedup();
    for expect in [
        "凭证", "报表", "资金", "销售", "采购", "仓管", "生产", "成本", "报销", "审批",
    ] {
        assert!(doms.contains(&expect), "admin 应含 {expect} 域：{doms:?}");
    }
    for t in wb["trends"].as_array().unwrap() {
        assert_eq!(
            t["periods"].as_array().unwrap().len(),
            6,
            "趋势期数应为 6：{}",
            t["key"]
        );
    }
    assert!(
        wb["todos"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["key"] == "voucher_unposted"),
        "admin 应有未记账凭证待办"
    );

    // 造一张未记账凭证 → 待办计数联动 ≥1
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &admin))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur_label = dash["current_period"].as_str().unwrap().to_string();
    let cur_ymm: i32 = cur_label.replace('-', "").parse().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &admin,
            serde_json::json!({
                "id": 0, "period": cur_ymm,
                "date": format!("{cur_label}-05"),
                "word": "记", "no": 1, "attachments": 0, "memo": "工作台待办造数",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "备用金", "debit": "50", "credit": "0" },
                    { "line": 2, "account_code": "660201", "summary": "备用金", "debit": "0", "credit": "50" }
                ]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let vbody = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建未记账凭证：{vbody}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workbench?periods=6", &admin))
        .await
        .unwrap();
    let wb2: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let tu = wb2["todos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["key"] == "voucher_unposted")
        .unwrap();
    assert!(tu["count"].as_i64().unwrap() >= 1, "未记账待办应 ≥1：{tu}");

    // 分域矩阵：keeper 只见 仓管/报销/审批；order_clerk 见 销售/采购
    for (u, name, role) in [
        ("wbk1", "仓管甲", "keeper"),
        ("wbc1", "订单甲", "order_clerk"),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &admin,
                serde_json::json!({ "username": u, "display_name": name, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通 {u}");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &admin,
                serde_json::json!({ "username": u, "display_name": name, "password": "", "role": role, "must_change_pwd": false }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {role}");
    }
    let mut sids: Vec<String> = Vec::new();
    for u in ["wbk1", "wbc1"] {
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
        sids.push(sid);
    }
    let (keeper, clerk) = (sids[0].clone(), sids[1].clone());

    let doms_of = |wb: &serde_json::Value| -> Vec<String> {
        let mut d: Vec<String> = wb["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["domain"].as_str().unwrap().to_string())
            .collect();
        d.sort();
        d.dedup();
        d
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workbench?periods=6", &keeper))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "仓管可进工作台");
    let wk = doms_of(&serde_json::from_str(&body_string(resp).await).unwrap());
    assert!(wk.contains(&"仓管".to_string()), "仓管应含仓管域：{wk:?}");
    assert!(wk.contains(&"报销".to_string()));
    assert!(
        !wk.contains(&"凭证".to_string()),
        "仓管不应见凭证域：{wk:?}"
    );
    assert!(!wk.contains(&"资金".to_string()));
    assert!(!wk.contains(&"销售".to_string()));
    assert!(
        !wk.contains(&"报表".to_string()),
        "仓管无 FinReport → 无报表域"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/workbench?periods=6", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单员可进工作台");
    let cl = doms_of(&serde_json::from_str(&body_string(resp).await).unwrap());
    assert!(
        cl.contains(&"销售".to_string()) && cl.contains(&"采购".to_string()),
        "订单员应含销售/采购：{cl:?}"
    );
    assert!(
        !cl.contains(&"仓管".to_string())
            && !cl.contains(&"凭证".to_string())
            && !cl.contains(&"资金".to_string())
    );
}

/// 工厂生产链（对标金蝶「业务单据同步凭证」）：标准价+BOM → 下达 →（未开工完工拒）
/// → 领料(借500101/贷140301) → 开工 → 完工(借140501/贷500101) 双凭证与状态链。
#[tokio::test]
async fn production_issue_complete_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 当前期间（生产订单与凭证期间随账套当前期间；日期取该期间内一天）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let dash: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cur = dash["current_period"].as_str().unwrap().to_string(); // 形如 2026-01
    let d15 = format!("{cur}-15");
    let d20 = format!("{cur}-20");
    let cur_ymm = cur.replace('-', ""); // 查询参数用 yyyymm

    // 1) 标准价（领料成本回退口径：无采购单价时按计价配置标准价）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "标准价配置应成功");

    // 2) BOM：1 件成品(140501) = 2 件原料(140301)
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bom",
            &sid,
            serde_json::json!({ "parent": "140501", "children": [{ "child": "140301", "qty": "2", "loss": "0" }] }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存BOM应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/bom?parent=140501", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let brows = r["rows"].as_array().unwrap();
    assert_eq!(brows.len(), 1);
    assert_eq!(brows[0]["qty"], serde_json::json!("2"));

    // 3) 下达生产订单（计划量0拒绝）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "0" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "计划量0应拒绝");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/prod",
            &sid,
            serde_json::json!({ "item_code": "140501", "qty": "10", "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下达应成功");
    let created: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po_id = created["id"].as_i64().unwrap();

    // 4) 状态链 Released →（未开工完工拒）→ 领料 → 开工 → InProgress → 完工 → Completed
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(po_id))
        .expect("列表应含新下达的订单");
    assert_eq!(po["status"], "Released", "下达后状态应为已下达");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/complete"),
            &sid,
            serde_json::json!({ "qty": "10", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "未开工不能完工入库");

    // 领料：BOM 2×10×(1+0) = 20 件 × 标准价10 = 200
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/issue"),
            &sid,
            serde_json::json!({ "date": d15.clone() }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let issue_body = body_string(resp).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "领料应成功：{issue_body}｜cur={cur} d15={d15}"
    );
    let r: serde_json::Value = serde_json::from_str(&issue_body).unwrap();
    assert_eq!(r["items"], 1, "一个子件一项领料");
    assert_eq!(
        money_num(r["total"].as_str().unwrap()),
        200.0,
        "领料成本 = 20×10"
    );

    // 按单限额领料：重复领料 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/issue"),
            &sid,
            serde_json::json!({ "date": d15.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "重复领料应被限额拦截"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "开工应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/start"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "重复开工应拒绝");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/complete"),
            &sid,
            serde_json::json!({ "qty": "10", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "完工入库应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(money_num(r["qty"].as_str().unwrap()), 10.0);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/prod", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let po = r["orders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"].as_i64() == Some(po_id))
        .unwrap();
    assert_eq!(po["status"], "Completed", "完工后状态应为已完工");
    assert_eq!(money_num(po["completed_qty"].as_str().unwrap()), 10.0);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/prod/{po_id}/complete"),
            &sid,
            serde_json::json!({ "qty": "1", "date": d20.clone() }),
        ))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "已完工订单不能重复完工");

    // 5) 双凭证：列表（裸数组，summary 字段）定位 → 明细 entries（account_code）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers?period={cur_ymm}"), &sid))
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_str(&body_string(resp).await).unwrap();
    let mat = list
        .iter()
        .find(|v| {
            v["summary"]
                .as_str()
                .map(|s| s.contains("领料"))
                .unwrap_or(false)
        })
        .expect("应有领料凭证");
    let comp = list
        .iter()
        .find(|v| {
            v["summary"]
                .as_str()
                .map(|s| s.contains("完工"))
                .unwrap_or(false)
        })
        .expect("应有完工凭证");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers/{}", mat["id"].as_i64().unwrap()),
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let d = entries
        .iter()
        .find(|e| e["account_code"] == "500101")
        .expect("领料借 500101 生产成本-直接材料");
    assert_eq!(money_num(d["debit"].as_str().unwrap()), 200.0);
    let c = entries
        .iter()
        .find(|e| e["account_code"] == "140301")
        .expect("领料贷 140301 原材料");
    assert_eq!(money_num(c["credit"].as_str().unwrap()), 200.0);

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers/{}", comp["id"].as_i64().unwrap()),
            &sid,
        ))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let d = entries
        .iter()
        .find(|e| e["account_code"] == "140501")
        .expect("完工借 140501 库存商品");
    assert_eq!(money_num(d["debit"].as_str().unwrap()), 200.0);
    let c = entries
        .iter()
        .find(|e| e["account_code"] == "500101")
        .expect("完工贷 500101 生产成本-直接材料");
    assert_eq!(money_num(c["credit"].as_str().unwrap()), 200.0);
}

/// 存货盘点流程：账面快照 → 应用（其他入库流水 + 盘盈盘亏凭证 1901）→ 守卫。
#[tokio::test]
async fn stock_count_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 配置标准价（金额 = 差异 × 标准价）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/configs",
            &sid,
            serde_json::json!({ "item": "140301", "method": "moving_average", "standard_cost": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "配置标准价应成功");

    // 新建盘点：140301 账面0 → 实盘5（盘盈 +5 × 10 = 50）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/count",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-20", "warehouse": "", "memo": "一月盘点",
                "lines": [{ "item": "140301", "count_qty": "5", "memo": "" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新建盘点应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let cid = r["id"].as_i64().unwrap();
    assert!(
        r["no"].as_str().unwrap().starts_with("PD"),
        "应生成盘点单号"
    );

    // 列表：账面快照为0、状态草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == cid)
        .unwrap();
    assert_eq!(row["status"], "draft");
    assert_eq!(
        money_num(row["lines"][0]["book_qty"].as_str().unwrap()),
        0.0,
        "账面应快照为0"
    );

    // 应用 → 盘盈盘亏凭证（借 140301 / 贷 1901）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid}/apply"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "应用应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["voucher_id"].as_i64().expect("有标准价应出凭证");
    assert_eq!(money_num(r["value"].as_str().unwrap()), 50.0, "价值 = 5×10");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "140301");
    assert_eq!(money_num(v["entries"][0]["debit"].as_str().unwrap()), 50.0);
    assert_eq!(v["entries"][0]["aux"]["item"], "140301");
    assert_eq!(v["entries"][1]["account_code"], "1901");
    assert_eq!(money_num(v["entries"][1]["credit"].as_str().unwrap()), 50.0);

    // 已应用：状态、删除守卫、重复应用拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/inventory/counts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == cid)
        .unwrap();
    assert_eq!(row["status"], "applied");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已应用盘点单不可删");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid}/apply"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复应用应拒绝");

    // 草稿可删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/inventory/count",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-21", "warehouse": "",
                "lines": [{ "item": "140301", "count_qty": "3" }]
            }),
        ))
        .await
        .unwrap();
    let cid2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/inventory/count/{cid2}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿盘点单应可删");
}

/// 凭证可见性默认放开（多岗位协作）：非管理员默认见全账套；按 data_scope 可按账号收紧。
#[tokio::test]
async fn own_voucher_scope_default_open() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;

    // boss 先录一张
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &boss,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 91, "attachments": 0, "memo": "老板录的",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "注", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "boss 录凭证");

    // 邀请会计 acct1 并进账套
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss,
            serde_json::json!({ "username": "acct1", "display_name": "会计甲", "password": "Test12345" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &boss,
            serde_json::json!({ "username": "acct1", "display_name": "会计甲", "password": "", "role": "accountant", "must_change_pwd": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请会计");
    let (st, sid1) = login(&state, "acct1", "Test12345").await;
    assert_eq!(st, StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sid1,
            serde_json::json!({ "old": "Test12345", "new": "Pass123456" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(select_book(&state, &sid1, "b1").await, StatusCode::OK);

    // 会计自己再录一张
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid1,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06", "word": "记",
                "no": 92, "attachments": 0, "memo": "会计录的",
                "entries": [
                    { "line": 1, "account_code": "660201", "summary": "费", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "会计录凭证");

    // 默认放开：会计能看到全账套2张（含老板录的）
    async fn count_vouchers(state: &Arc<WebState>, sid: &str) -> usize {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get("/api/vouchers?from=202601&to=202601", sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let empty = Vec::<serde_json::Value>::new();
        let arr = v["rows"]
            .as_array()
            .or_else(|| v.as_array())
            .unwrap_or(&empty);
        arr.len()
    }
    let n = count_vouchers(&state, &sid1).await;
    assert!(
        n >= 2,
        "默认放开：会计应能看到全账套凭证（含他人录入），实际 {n}"
    );

    // 管理员按账号收紧 → 只剩自己的1张
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/acct1",
            &boss,
            serde_json::json!({
                "data_scope": {
                    "depts": [], "account_from": "", "account_to": "",
                    "own_voucher_only": true, "own_doc_only": false
                }
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "按账号收紧应成功");
    let n2 = count_vouchers(&state, &sid1).await;
    assert!(n2 == 1, "收紧后应只剩本人1张，实际 {n2}");
}

/// 年末结转：本年利润 → 未分配利润。
#[tokio::test]
async fn web_year_end_carry() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 6401 100 / 贷 1001 100，并结转损益（生成 4103 贷方 100）
    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "6401", "summary": "成本", "debit": "100", "credit": "0" },
            { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "100" }
        ]),
    )
    .await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/carry-forward",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结转损益应成功");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{cid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 年末结转：4103 余额 -100 → 转入未分配利润
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/year-end",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "年末结转应成功：{}",
        body_string(resp).await
    );
    // 幂等性：第二次调用 4103 已清零，应 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/year-end",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "余额为 0 时年末结转应被拒"
    );
}

/// 红字冲销：未传日期时取期间末日（历史期间冲销不会因"今天"不在期间内而失败）。
#[tokio::test]
async fn web_reverse_defaults_to_period_last_day() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let id = post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "冲销测试", "debit": "100", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "冲销测试", "debit": "0", "credit": "100" }
        ]),
    )
    .await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/reverse"),
            &sid,
            serde_json::json!({ "period": 202601 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "缺省日期红冲应成功");
    let rid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_ne!(rid, id, "红冲应生成新凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{rid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["date"],
        serde_json::json!("2026-01-31"),
        "应取期间末日：{v}"
    );
}

/// 已提交的业务写入不能因为随后的审计日志失败而变成 500（否则客户端重试会重复入账）。
///
/// 手法：把 `audit_log` 表删掉，让所有 `db.log` 必然失败。修复前这些接口一律 500，
/// 客户端看到失败重试就会再录一张付款单/订单/凭证。
#[tokio::test]
async fn audit_log_failure_does_not_fail_committed_mutations() {
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
    let d12 = format!("{cur_label}-12");
    let d15 = format!("{cur_label}-15");
    let d20 = format!("{cur_label}-20");

    // 造数：采购入库 → 销售出库（给成本结转留销售出库）
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
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Confirmed", "memo": "结转造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "4", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "销售出库");

    // 让所有审计日志写入必然失败
    block_inserts(&state, "b1", "audit_log");

    // ① 采购付款：业务写入已提交，日志失败不得改判 500
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/payment",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d12.clone(), "amount": "10", "memo": "付" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让付款变 500：{b}");
    let pay: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(pay["id"].as_i64().unwrap() > 0, "付款流水应已写入：{b}");

    // ② 销售收款
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/payment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "amount": "10", "memo": "收" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让收款变 500：{b}");

    // ③ 保存采购订单
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": cur_ymm, "date": d15.clone(), "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "日志坏了也要成",
                "lines": [{ "item_code": "140301", "qty_ordered": "2", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让保存订单变 500：{b}");

    // ④ 保存凭证（重试会多录一张，是最贵的重复写）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 0,
                "attachments": 0, "memo": "日志坏了也要成",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "s", "debit": "1", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "s", "debit": "0", "credit": "1" }
                ]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让保存凭证变 500：{b}");

    // ⑤ 销售成本结转：凭证已由 findb 提交，日志失败不得改判 500
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/cost/sales-cost",
            &sid,
            serde_json::json!({ "period": cur_ymm, "method": "moving_average", "date": d20 }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "日志表坏了也不能让成本结转变 500：{b}");
    let cost: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(
        cost["voucher_id"].as_i64().unwrap_or(0) > 0,
        "结转凭证应已生成：{b}"
    );
}

/// 凭证：修改时凭证字不可变（保住号段一致），自选号码撞号回 400 而不是 500。
#[tokio::test]
async fn voucher_word_immutable_and_duplicate_no_is_400() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let mk = |word: &str, no: i32, memo: &str| {
        serde_json::json!({
            "id": 0, "period": 202601, "date": "2026-01-15", "word": word, "no": no,
            "attachments": 0, "memo": memo,
            "entries": [
                { "line": 1, "account_code": "1001", "summary": "s", "debit": "10", "credit": "0" },
                { "line": 2, "account_code": "2001", "summary": "s", "debit": "0", "credit": "10" }
            ]
        })
    };
    let entries = |memo: &str| {
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": memo, "debit": "10", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": memo, "debit": "0", "credit": "10" }
        ])
    };

    // 自动取号建一张 记-0001
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &sid, mk("记", 0, "原始")))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建凭证：{b}");
    let vid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 改凭证字 记 → 银：必须拒绝（否则 银-0001 会占掉银字号的号）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": vid, "period": 202601, "date": "2026-01-15", "word": "银", "no": 0,
                "attachments": 0, "memo": "改字", "entries": entries("改字")
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "凭证字不可改：{b}");

    // 凭证字不变、只改摘要：放行且号不变
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": vid, "period": 202601, "date": "2026-01-15", "word": "记", "no": 0,
                "attachments": 0, "memo": "只改摘要", "entries": entries("只改摘要")
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "同字修改应放行：{b}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["word"], "记", "凭证字应保持不变：{v}");
    assert_eq!(v["no"], 1, "号码应保持不变（号段不漂移）：{v}");
    assert_eq!(v["memo"], "只改摘要", "其它字段应已更新：{v}");

    // 自选号码撞上 记-0001：必须 400，不能是 500
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &sid, mk("记", 1, "撞号")))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "撞号应回 400 而不是 500：{b}");
    assert!(!b.contains("UNIQUE"), "不该把 SQLite 约束错误透给用户：{b}");

    // 换一个字就不撞，仍可自选
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", &sid, mk("银", 1, "银字 1 号")))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "银字 1 号未被占用：{b}");
}

/// 凭证更正链（Cancel -> Amend）的端到端行为。
///
/// 回归背景：finbook 有反记账 + 取消审核，所以错凭证**能改** —— 但改完的两张凭证
/// 之间没有任何链接，事后审计只能靠时间和金额去猜哪张在冲哪张。
/// 「不能改」反而更安全：它逼人走更正流程；「能改」让人直接改，而直接改完的两张
/// 凭证互相不认识，才是真正查不出来的状态。
#[tokio::test]
async fn voucher_amend_links_old_and_new() {
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

    // 建一张已记账的凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 91,
                "attachments": 0, "memo": "更正链测试", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "更正测试", "debit": "1000", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "更正测试", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
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
    assert_eq!(resp.status(), StatusCode::OK, "记账：{}", resp.status());

    // ① 原因必填 —— 没有原因的更正等于没更正
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/amend"),
            &sid,
            serde_json::json!({ "reason": "  " }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "空原因应被拒");
    // 被拒时原凭证必须还是已记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["status"], "posted", "校验失败不该动原凭证：{r}");

    // ② 正常更正
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/amend"),
            &sid,
            serde_json::json!({ "reason": "金额录错，应为 1200" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "更正失败");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let new_id = r["new_id"].as_i64().expect("应返回新凭证 id");
    assert!(new_id > 0 && new_id != vid);

    // ③ 原凭证：**保持已记账** + 标注「被谁红冲 + 谁更正重做」
    //    （准则形态：红字冲销 + 蓝字重做。作废原凭证会让总账看不出曾经记错过。）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        r["status"], "posted",
        "原凭证必须保持已记账，由红冲凭证抵消：{r}"
    );
    let red_id = r["amend"]["amended_by"].as_i64().expect("应标注被谁红冲");
    assert!(red_id > 0 && red_id != vid);
    assert_eq!(
        r["amend"]["amended_by_new"], new_id,
        "同批的更正凭证要单独标出：只列一张等于账上有一张凭证没交代来路：{r}"
    );
    assert_eq!(r["amend"]["reason"], "金额录错，应为 1200");
    assert!(
        r["amend"]["amended_by_no"]
            .as_str()
            .unwrap_or("")
            .starts_with("记-"),
        "要可读单号：{r}"
    );

    // ④ 红冲凭证：草稿 + 借贷互换 + 摘要带冲销标记
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{red_id}"), &sid))
        .await
        .unwrap();
    let red: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(red["status"], "draft", "红冲凭证应是草稿等会计审核：{red}");
    assert_eq!(
        red["amend"]["amends_id"], vid,
        "红冲凭证也要标注来自哪张：{red}"
    );
    let red_entries = red["entries"].as_array().unwrap();
    assert_eq!(red_entries.len(), 2);
    // 借贷互换：原凭证借 1001，红冲就贷 1001
    // （金额列以 TEXT 存、序列化带两位小数，所以用正则而不是精确等值）
    assert!(
        red_entries[0]["credit"]
            .as_str()
            .unwrap_or("")
            .starts_with("1000"),
        "红冲第一行应为贷方 1000（原凭证是借方）：{red}"
    );
    assert!(
        red_entries[1]["debit"]
            .as_str()
            .unwrap_or("")
            .starts_with("1000"),
        "红冲第二行应为借方 1000：{red}"
    );
    assert!(
        red_entries[0]["summary"]
            .as_str()
            .unwrap_or("")
            .contains("冲销"),
        "红冲凭证摘要要带冲销标记，否则账上看着像重复录入：{red}"
    );

    // ⑤ 更正凭证：草稿 + 标注「更正自谁」+ 分录原样复制
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{new_id}"), &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["status"], "draft", "更正凭证应是草稿等会计审核：{r}");
    assert_eq!(r["amend"]["amends_id"], vid, "更正凭证要标注更正自谁：{r}");
    assert!(
        r["entries"][0]["debit"]
            .as_str()
            .unwrap_or("")
            .starts_with("1000"),
        "分录要原样复制：{r}"
    );

    // ⑥ 三张都记账后，任何一张都不能被单独反记账（撤掉一张净额就错）
    //    红冲与更正凭证是**新凭证**（草稿态），账套开着审核环节时须先审核。
    for id in [red_id, new_id] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{id}/audit"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "红冲/更正凭证应能审核");
    }
    for (id, what) in [(vid, "原凭证"), (red_id, "红冲凭证"), (new_id, "更正凭证")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{id}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{what} 应能记账");
    }
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{red_id}/unpost"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "红冲凭证不能被单独反记账（三张必须成套存在）"
    );

    // ⑦ 重复更正被拒，且错误信息要指向「已被更正过」而不是笼统的状态
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/amend"),
            &sid,
            serde_json::json!({ "reason": "再改一次" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复更正应被拒");
    let msg = body_string(resp).await;
    assert!(
        msg.contains("更正过"),
        "错误信息应说「已被更正过」，实际：{msg}"
    );

    // ⑥ 新凭证记账后，总账里 1001 只有一笔 1000（作废那张不进总账）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{new_id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "更正凭证记账");
}

/// 引擎校验错误不能被套上「导入数据有误」的万能前缀。
///
/// `state.rs` 的 `From<FinError>` 曾把**任何**引擎错误都改写成
/// 「导入数据有误：<原文>」——而记账、结转、结账都不是导入操作。用户看到
/// 「导入数据有误」会去检查导入的 Excel，其实那一行都没导入过。
#[tokio::test]
async fn engine_validation_errors_are_not_labelled_as_import_errors() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 1000 / 贷 900：借贷不平，必被引擎拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-15", "word": "记",
                "no": 1, "attachments": 0, "memo": "不平",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收", "debit": "1000", "credit": "0" },
                    { "line": 2, "account_code": "6401", "summary": "费", "debit": "0", "credit": "900" }
                ]
            }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "借贷不平必须被拒：{b}");
    assert!(
        !b.contains("导入数据有误"),
        "记账不是导入操作，不该带「导入数据有误」前缀：{b}"
    );
}

/// 备份下载只打包当前账套引用的大附件，不能把共享 `.attachments/` 整目录拖走。
#[tokio::test]
async fn backup_download_excludes_other_books_attachments() {
    let (state, _bd, _dir) = test_state();
    let (_, admin_sid) = login(&state, "boss", "Admin!2026").await;

    let a_key = create_book_with_key(&state, &admin_sid, "attach_a").await;
    let b_key = create_book_with_key(&state, &admin_sid, "attach_b").await;

    // 两个账套各传一个 >256KB 的附件（小附件是 inline，不进 .attachments/）。
    let a_data = format!("A{}", "a".repeat(300 * 1024));
    let b_data = format!("B{}", "b".repeat(300 * 1024));

    for (key, data, file) in [
        (&a_key, a_data.as_str(), "a.txt"),
        (&b_key, b_data.as_str(), "b.txt"),
    ] {
        assert_eq!(select_book(&state, &admin_sid, key).await, StatusCode::OK);
        // 先建一张平衡凭证，附件挂在凭证上
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                &admin_sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": "2026-01-08", "word": "记",
                    "no": 1, "attachments": 0, "memo": "附件隔离测试",
                    "entries": [
                        { "line": 1, "account_code": "1001", "summary": "收", "debit": "50", "credit": "0" },
                        { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "50" }
                    ]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "建凭证应成功");
        let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            ["id"]
            .as_i64()
            .unwrap();

        let boundary = "----finbookatt";
        let mut payload = Vec::new();
        payload.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\nContent-Type: text/plain\r\n\r\n"
            )
            .as_bytes(),
        );
        payload.extend_from_slice(data.as_bytes());
        payload.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/vouchers/{vid}/attachments"))
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header(header::COOKIE, &admin_sid)
            .body(Body::from(payload))
            .unwrap();
        let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "上传大附件应成功");
    }

    // 回到 A 下载打包，只应看到 A 的附件
    assert_eq!(
        select_book(&state, &admin_sid, &a_key).await,
        StatusCode::OK
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/backup/download", &admin_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 32 * 1024 * 1024)
        .await
        .unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut attachments: Vec<Vec<u8>> = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        if f.name().starts_with(".attachments/") {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut f, &mut buf).unwrap();
            attachments.push(buf);
        }
    }
    assert_eq!(
        attachments.len(),
        1,
        "只能打包当前账套的 1 个附件，实际 {} 个",
        attachments.len()
    );
    assert_eq!(attachments[0], a_data.as_bytes());
    assert_ne!(attachments[0], b_data.as_bytes());
}

/// GET 一个端点并解析 JSON（失败即报错，带响应体）。
async fn get_json(state: &Arc<WebState>, sid: &str, path: &str) -> serde_json::Value {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(path, sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET {path} 应 200");
    serde_json::from_str(&body_string(resp).await).unwrap()
}
