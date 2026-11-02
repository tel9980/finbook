//! 由 `tests/api/voucher.rs`（5,128 行）按域拆分而来。只搬位置、不改一行用例。
//!
//! 本册：固定资产、总账对账。

use super::super::*;

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
