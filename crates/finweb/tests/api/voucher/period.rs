//! 由 `tests/api/voucher.rs`（5,128 行）按域拆分而来。只搬位置、不改一行用例。
//!
//! 本册：期间结转、年末结转、合并与反记账。

use super::super::*;

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
