//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

/// P0：出纳交接班——交班快照（现金结存/在库票据/未日清）+ 接班确认（不能自确认）。
#[tokio::test]
async fn cash_shift_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    // 当期与测试日期
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

    // 造一笔现金收款（借 1001 500 / 贷 2001 500）并记账 → 快照应有 500
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记",
                "no": 90, "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "shift-in", "debit": "500", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "shift-in", "debit": "0", "credit": "500" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造现金凭证");
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
    assert_eq!(resp.status(), StatusCode::OK, "记账");

    // 交班单 A：快照 + 自己不能确认 → 取消
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/shifts",
            &sid,
            serde_json::json!({ "date": d15, "to_user": "sh1", "memo": "晚班交接" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "创建交班单 A");
    let a: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let aid = a["id"].as_i64().unwrap();
    assert_eq!(
        a["shift"]["cash_balance"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap(),
        500.0,
        "现金结存快照：{a}"
    );
    assert_eq!(a["shift"]["status"], "open");
    assert!(
        a["shift"]["uncleared"].as_i64().unwrap() >= 1,
        "未日清账户数应 ≥1：{a}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/shifts/{aid}/confirm"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "交班人不能自确认");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/shifts/{aid}/cancel"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消交班单 A");

    // 出纳 sh1（平台开号 → 首登改密 → 加入账套 cashier）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({ "username": "sh1", "display_name": "出纳一", "password": "Sh1@pass99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "平台开号 sh1");
    let (_, sh1_sid) = login(&state, "sh1", "Sh1@pass99").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sh1_sid,
            serde_json::json!({ "old": "Sh1@pass99", "new": "Sh1@pass99x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "sh1 首登改密");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "sh1", "display_name": "出纳一", "password": "",
                "role": "cashier", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请 sh1 入套为出纳");
    assert_eq!(select_book(&state, &sh1_sid, "b1").await, StatusCode::OK);

    // 交班单 B：boss 交班 → sh1 确认 → 重复确认 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/shifts",
            &sid,
            serde_json::json!({ "date": d15, "to_user": "sh1" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "创建交班单 B");
    let b: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bid = b["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/shifts/{bid}/confirm"),
            &sh1_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "接班人确认");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/shifts/{bid}/confirm"),
            &sh1_sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复确认应 400");

    // 列表：B 已确认（确认人 sh1），A 已取消
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/shifts", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    let br = rows
        .iter()
        .find(|x| x["id"] == bid)
        .expect("列表应含交班单 B");
    assert_eq!(br["status"], "confirmed");
    assert_eq!(br["confirmed_by"], "sh1");
    let ar = rows
        .iter()
        .find(|x| x["id"] == aid)
        .expect("列表应含交班单 A");
    assert_eq!(ar["status"], "cancelled");
}

/// P2：滚动资金预测——票据到期 + 融资起止按期间展开。
#[tokio::test]
async fn funds_rolling_forecast() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收票据 1000 到期 202602；借款 500 起 202602 止 202603
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/bills",
            &sid,
            serde_json::json!({
                "kind": "receivable", "no": "B1", "period": 202601,
                "issue_date": "2026-01-05", "due_date": "2026-02-20",
                "counterpart": "客户甲", "bank": "工行", "amount": "1000", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建票据");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/loans",
            &sid,
            serde_json::json!({
                "kind": "borrow", "no": "L1", "bank": "工行", "principal": "500",
                "rate_pct": "4.5", "start_date": "2026-02-01", "end_date": "2026-03-31", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建融资");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/funds/forecast-rolling?from=202601&periods=3",
            &sid,
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "滚动预测：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let n = |v: &serde_json::Value| v.as_str().unwrap().replace(',', "").parse::<f64>().unwrap();
    // 202602：票据收 1000 + 融资到账 500 → 净 1500
    let feb = &rows[1];
    assert!(
        (n(&feb["bill_in"]) - 1000.0).abs() < 0.005,
        "2 月票据到期：{feb}"
    );
    assert!(
        (n(&feb["loan_in"]) - 500.0).abs() < 0.005,
        "2 月融资到账：{feb}"
    );
    assert!(
        (n(&feb["net"]) - 1500.0).abs() < 0.005,
        "2 月净流 1500：{feb}"
    );
    assert!(
        (n(&feb["balance"]) - 1500.0).abs() < 0.005,
        "2 月结存 1500：{feb}"
    );
    // 202603：融资偿还 500 → 净 -500、结存 1000
    let mar = &rows[2];
    assert!(
        (n(&mar["loan_out"]) - 500.0).abs() < 0.005,
        "3 月偿还：{mar}"
    );
    assert!(
        (n(&mar["net"]) + 500.0).abs() < 0.005,
        "3 月净流 -500：{mar}"
    );
    assert!(
        (n(&mar["balance"]) - 1000.0).abs() < 0.005,
        "3 月结存 1000：{mar}"
    );
}

/// Web 银行对账：导入对账单 → 自动勾对 → 余额调节表平衡。
#[tokio::test]
async fn web_bank_reconcile_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 账面：借 100201 1000 / 贷 2001 1000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "银行收款",
                "entries": [
                    { "line": 1, "account_code": "100201", "summary": "收款", "debit": "1000", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "1000" }
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

    // 导入对账单（余额与账面一致，便于验证调节表勾稽）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/import",
            &sid,
            serde_json::json!({
                "ymm": 202601, "account": "100201",
                "text": "日期,摘要,结算号,借方,贷方,余额\n2026-01-10,收款,SN001,1000.00,0.00,1000.00\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "导入对账单应成功");
    let imp: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(imp["imported"], serde_json::json!(1));

    // 自动勾对
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/auto-match",
            &sid,
            serde_json::json!({ "ymm": 202601, "account": "100201", "tolerance": 31 }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let m: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        m["matched"].as_u64().unwrap_or(0) >= 1,
        "应至少勾对一对：{m}"
    );

    // 调节表：银行与账面调节后一致
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/bank?period=202601&account=100201", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        d["statements"][0]["entry_id"].as_i64(),
        Some(d["book"][0]["entry_id"].as_i64().unwrap()),
        "对账单应挂到账面分录"
    );
    assert_eq!(
        d["reconcile"]["balanced"],
        serde_json::json!(true),
        "调节表应平衡：{}",
        d["reconcile"]
    );
}

/// 收付款单 API 流程：出凭证 + 自动核销 + 删除链（凭证先行）。
#[tokio::test]
async fn receipt_doc_api_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 先造一张应收挂账（客户辅助 C01）并记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 81, "attachments": 0, "memo": "挂账",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "挂账",
                      "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入",
                      "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "挂账凭证应可保存");
    let ar_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{ar_id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "挂账应记账");

    // 收款 600：草稿建单（不出凭证、不核销——审核流对标金蝶）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/receipts",
            &sid,
            serde_json::json!({
                "date": "2026-01-10", "kind": "receipt", "fund_account": "100201",
                "party": "C01", "amount": "600", "memo": "回款"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增收款单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc_id = r["id"].as_i64().unwrap();
    assert!(r["voucher_id"].is_null(), "草稿阶段不应生成凭证");
    // 审核 → 同事务生成凭证 + FIFO 自动核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["voucher_id"].as_i64().expect("审核应生成凭证");
    assert_eq!(r["settled"].as_i64().unwrap(), 1, "应自动核销 1 笔");
    // 重复审核拒绝（条件更新防并发）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复审核应拒绝");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "100201");
    assert_eq!(v["entries"][1]["account_code"], "112201");
    assert_eq!(v["entries"][1]["aux"]["customer"], "C01");

    // 列表可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/receipts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 1);

    // 删除链：凭证存在 → 拒；删凭证 → 单据可删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "凭证存在时不能删单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{vid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿凭证应可删除");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "凭证删除后单据可删");

    // 撤审回路：再建一单 → 审核 → 撤审 → 凭证消失、单据回草稿、可直接删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/receipts",
            &sid,
            serde_json::json!({
                "date": "2026-01-11", "kind": "receipt", "fund_account": "100201",
                "party": "C02", "amount": "100", "memo": "二笔"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc2 = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc2}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let vid2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["voucher_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc2}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "撤审应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid2}"), &sid))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "撤审后凭证应已删除");
    // 单据回草稿（凭证号清空）→ 可直接删
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/receipts", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == doc2)
        .unwrap();
    assert_eq!(row["status"], "draft", "撤审应回草稿");
    assert!(row["voucher_id"].is_null());
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc2}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿可直接删除");
}

/// 资金调拨：建单（草稿）→ 审核出凭证（借 转入 / 贷 转出，银行腿挂 B01）→
/// 撤审回草稿 → 删除链；守卫同户 / 非资金科目。
#[tokio::test]
async fn fund_transfer_api_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 守卫：转出 = 转入 拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/transfers",
            &sid,
            serde_json::json!({
                "date": "2026-01-05", "from_account": "100201", "to_account": "100201",
                "amount": "500", "memo": "同户"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "同户调拨应拒绝");

    // 守卫：转入非资金科目（6602）拒绝
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/transfers",
            &sid,
            serde_json::json!({
                "date": "2026-01-05", "from_account": "100201", "to_account": "6602",
                "amount": "500", "memo": "非资金科目"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "非资金科目应拒绝");

    // 建单：提现备用（银行 → 现金），此时只落草稿、不出凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/transfers",
            &sid,
            serde_json::json!({
                "date": "2026-01-05", "from_account": "100201", "to_account": "1001",
                "amount": "500", "memo": "提现备用"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增调拨单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc_id = r["id"].as_i64().unwrap();
    assert_eq!(r["status"], "draft");
    assert!(r["voucher_id"].is_null(), "草稿阶段不应生成凭证");

    // 审核 → 生成凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核应成功");
    let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["voucher_id"]
        .as_i64()
        .expect("审核应生成凭证");

    // 重复审核拒绝（条件更新防并发重复出凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复审核应拒绝");

    // 凭证：借 转入 1001 / 贷 转出 100201（银行腿必须挂 bank=B01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "1001");
    assert_eq!(v["entries"][0]["debit"], serde_json::json!("500.00"));
    assert_eq!(v["entries"][1]["account_code"], "100201");
    assert_eq!(v["entries"][1]["credit"], serde_json::json!("500.00"));
    assert_eq!(v["entries"][1]["aux"]["bank"], "B01");

    // 列表可见且已审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/transfers", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["status"], "audited");

    // 删除链：凭证存在 → 拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "凭证存在时不能删单");

    // 撤审 → 凭证删除、单据回草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "撤审应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::OK, "撤审后凭证应已删除");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/transfers", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"][0]["status"], "draft", "撤审应回草稿");
    assert!(r["rows"][0]["voucher_id"].is_null());

    // 草稿可直接删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/transfers/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿可直接删除");
}

/// 报价单转订单 + 单据套打（字段白名单 / 批量紧凑分页 / A4）。
#[tokio::test]
async fn quote_to_order_and_doc_print() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 报价单 → 审批 → 转订单（草稿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/quote",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05",
                "customer_code": "C01", "customer_name": "客户甲",
                "item_code": "140501", "item_name": "成品",
                "qty": "10", "unit_price": "20",
                "status": "draft", "prepared_by": "", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建报价单应成功");
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
    assert_eq!(resp.status(), StatusCode::OK, "报价审批应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/to-order"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "转订单应成功");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["so_id"]
        .as_i64()
        .unwrap();

    // 订单存在：金额200、草稿、带来源备注
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == so_id)
        .expect("转换生成的订单应存在");
    assert_eq!(row["status"], "Draft");
    assert_eq!(money_num(row["total_amount"].as_str().unwrap()), 200.0);
    assert!(
        row["memo"].as_str().unwrap().contains("由报价单"),
        "订单应带来源备注"
    );
    // 报价单状态 → converted；重复转换被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/quote?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let qrow = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == qid)
        .unwrap();
    assert_eq!(qrow["status"], "converted", "报价单应标记已转订单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/quote/{qid}/to-order"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已转换不可再转");

    // 第二张订单（批量紧凑分页：两张小单应同页）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-06", "customer_code": "C01",
                "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140501", "qty_ordered": "5", "unit_price": "10", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    let so2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 订单套打：两张同页 + 默认全字段
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/sales/so/print-form?ids={so_id},{so2}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()[axum::http::header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .contains("text/html"));
    let html = body_string(resp).await;
    assert!(
        html.contains("销售订单") && html.contains("客户甲") && html.contains("税率"),
        "默认应全字段显示"
    );
    assert_eq!(
        html.matches("class=\"page\"").count(),
        1,
        "两张小单应紧凑同页"
    );

    // 字段收窄：fields 白名单生效
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/sales/so/print-form?ids={so_id}&fields=no,date,code,qty,amount"),
            &sid,
        ))
        .await
        .unwrap();
    let html = body_string(resp).await;
    assert!(
        !html.contains("税率") && html.contains("单号"),
        "fields 白名单应生效：{html}"
    );

    // ids 缺省 = 期间全部；空期间 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so/print-form?period=202601", &sid))
        .await
        .unwrap();
    let html = body_string(resp).await;
    assert!(
        html.matches("class=\"doc\"").count() >= 2,
        "缺省应打印期间内全部订单"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so/print-form?period=202501", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "无可打印订单应400");

    // 采购订单套打 smoke
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-07", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0.13" }]
            }),
        ))
        .await
        .unwrap();
    let pid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/procure/po/print-form?ids={pid}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;
    assert!(
        html.contains("采购订单") && html.contains("供应商"),
        "采购套打应含标题与供应商"
    );

    // 收付款单套打 smoke（收款 + 金额大写）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/receipts",
            &sid,
            serde_json::json!({
                "date": "2026-01-10", "kind": "receipt", "fund_account": "100201",
                "party": "C01", "amount": "1234.56", "memo": "回款"
            }),
        ))
        .await
        .unwrap();
    let rid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/funds/receipts/print-form?ids={rid}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;
    assert!(
        html.contains("收款单") && html.contains("壹仟贰佰叁拾肆元伍角陆分"),
        "收付款套打应含单名与金额大写"
    );
}

/// 资金预算视图：当期现金/银行科目预算 vs 已记账实际（形态校验；口径见 findb 单测）
#[tokio::test]
async fn funds_budget_view_api() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/budget?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "资金预算视图应 200");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["rows"].is_array(), "应返回 rows 数组：{r}");
}

/// 员工借支闭环：建单 → 支付出凭证 → 核销冲账出凭证；幂等与守卫。
#[tokio::test]
async fn advance_pay_settle_api_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 建单（缺省编号自动生成）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/advances",
            &sid,
            serde_json::json!({
                "date": "2026-01-08", "employee": "张三", "purpose": "出差预借",
                "amount": "2000", "pay_account": "1001", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建单应成功");
    let aid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 未支付不能核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/settle"),
            &sid,
            serde_json::json!({ "expense_account": "660201", "expense_amount": "500" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "未支付不能核销");

    // 支付（默认今天）→ 凭证：借122105(员工) / 贷1001
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/pay"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "支付应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pvid = r["voucher_id"].as_i64().expect("支付应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{pvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "122105");
    assert!(v["entries"][0]["aux"]["employee"]
        .as_str()
        .unwrap()
        .contains("张三"));
    assert_eq!(v["entries"][1]["account_code"], "1001");
    // 重复支付幂等
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/pay"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "重复支付不应重复出凭证：{r}");

    // 核销（冲账1500，退回500）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/settle"),
            &sid,
            serde_json::json!({ "expense_account": "660201", "expense_amount": "1500", "date": "2026-01-20" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "核销应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let svid = r["voucher_id"].as_i64().expect("核销应生成凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{svid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["entries"].as_array().unwrap().len(),
        3,
        "冲账+退回+贷员工，{v}"
    );
    assert_eq!(v["entries"][0]["account_code"], "660201");
    // 已核销不可删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/advances/{aid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已出凭证不可删除");
    // 员工/金额校验
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/advances",
            &sid,
            serde_json::json!({ "date": "2026-01-08", "employee": "", "amount": "100" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "借支人必填");
}

/// 出纳日清标记 + 支票登记簿：状态流转、查询与删除。
#[tokio::test]
async fn cashier_day_clear_and_checks() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 日清：标记 → 按期间查询 → 取消
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/day-clear",
            &sid,
            serde_json::json!({ "account_code": "1001", "date": "2026-01-10", "clear": true }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "标记日清应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/funds/day-clear?account=1001&from=2026-01-01&to=2026-01-31",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dates = r["dates"].as_array().unwrap();
    assert!(
        dates.iter().any(|d| d == "2026-01-10"),
        "应查到日清日期：{r}"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/day-clear",
            &sid,
            serde_json::json!({ "account_code": "1001", "date": "2026-01-10", "clear": false }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/funds/day-clear?account=1001&from=2026-01-01&to=2026-01-31",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["dates"].as_array().unwrap().is_empty(),
        "取消后应查不到：{r}"
    );

    // 支票：新增 → 列表 → 作废 → 恢复 → 删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/checks",
            &sid,
            serde_json::json!({
                "no": "ZP100", "kind": "transfer", "bank_account": "100201",
                "payee": "供应商乙", "amount": "1200", "issued_date": "2026-01-16", "memo": "货款"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "登记支票应成功");
    let cid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/checks", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = r["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(money_num(rows[0]["amount"].as_str().unwrap()), 1200.0);
    assert_eq!(rows[0]["status"], "issued");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/checks/{cid}/status"),
            &sid,
            serde_json::json!({ "status": "void" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "作废应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/checks", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"][0]["status"], "void");
    // 非法状态被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/checks/{cid}/status"),
            &sid,
            serde_json::json!({ "status": "bad" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/checks/{cid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/checks", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["rows"].as_array().unwrap().is_empty());
}

/// 现金盘点：账面按资金日报（按日）口径快照 → 差异 → 盘盈盘亏凭证；幂等与守卫。
#[tokio::test]
async fn cash_count_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 现金 800 已记账（01-10）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 71, "attachments": 0, "memo": "盘点基数",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "800", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "800" }
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
    assert_eq!(resp.status(), StatusCode::OK);

    // 盘点 01-12 实盘 850 → 账面 800、差异 +50
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/cash-counts",
            &sid,
            serde_json::json!({
                "date": "2026-01-12", "account_code": "1001", "counted": "850", "memo": "例行"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增盘点应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        money_num(r["book_amount"].as_str().unwrap()),
        800.0,
        "账面快照"
    );
    assert_eq!(money_num(r["diff"].as_str().unwrap()), 50.0, "差异 +50");
    let cid = r["id"].as_i64().unwrap();

    // 生成盘盈凭证：借1001 / 贷1901；幂等拒绝；挂凭证不可删
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "盘盈凭证应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let pvid = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{pvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "1001", "盘盈借现金");
    assert_eq!(v["entries"][1]["account_code"], "1901", "盘盈贷待处理损溢");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "不可重复出凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已挂凭证不可删除");

    // 账实相符（实盘800）→ 差异0 → 凭证被拒、可删除
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/cash-counts",
            &sid,
            serde_json::json!({ "date": "2026-01-13", "account_code": "1001", "counted": "800" }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        money_num(r["diff"].as_str().unwrap()),
        0.0,
        "账实相符差异为0"
    );
    let cid2 = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid2}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "相符不应出凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/cash-counts/{cid2}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "相符记录可删除");
}

/// 资金日报（按日）：上日结余/本日收支/日末结存，仅已记账（H-3）。
#[tokio::test]
async fn funds_daily_by_date_report() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 01-10 现金凭证并记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 61, "attachments": 0, "memo": "日报取数",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "800", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "借款", "debit": "0", "credit": "800" }
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
    assert_ok(resp, "记账应成功").await;

    let fetch = |sid: String, date: &'static str| {
        let state = state.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_get(
                    &format!("/api/funds/daily-by-date?date={date}"),
                    &sid,
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "日报应 200");
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        }
    };

    // 前一日：无发生
    let r = fetch(sid.clone(), "2026-01-09").await;
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["account_code"] == "1001")
        .expect("应有 1001 行");
    assert_eq!(money_num(row["income"].as_str().unwrap()), 0.0);
    assert_eq!(money_num(row["end"].as_str().unwrap()), 0.0);

    // 当日：收入 800、日末 800
    let r = fetch(sid.clone(), "2026-01-10").await;
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["account_code"] == "1001")
        .unwrap();
    assert_eq!(money_num(row["begin"].as_str().unwrap()), 0.0);
    assert_eq!(money_num(row["income"].as_str().unwrap()), 800.0);
    assert_eq!(money_num(row["end"].as_str().unwrap()), 800.0);

    // 次日：上日结余结转
    let r = fetch(sid.clone(), "2026-01-11").await;
    let row = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["account_code"] == "1001")
        .unwrap();
    assert_eq!(money_num(row["begin"].as_str().unwrap()), 800.0);
    assert_eq!(money_num(row["income"].as_str().unwrap()), 0.0);
    assert_eq!(money_num(row["end"].as_str().unwrap()), 800.0);
}

/// 台账-总账联动：票据流转自动生成台账凭证；融资到账/结清出凭证；幂等与删除守卫。
#[tokio::test]
async fn funds_ledger_voucher_linkage() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收票据 → 贴现：流转即自动生成凭证（借 100201 / 贷 112101）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/bills",
            &sid,
            serde_json::json!({ "kind": "receivable", "no": "FL001", "period": 202601,
                "issue_date": "2026-01-05", "due_date": "2026-03-05", "counterpart": "客户甲",
                "bank": "工行", "amount": "5000", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建票据应成功");
    let bid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/bills/{bid}/status"),
            &sid,
            serde_json::json!({ "status": "discounted", "date": "2026-01-15" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "贴现流转应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let bvid = r["voucher_id"]
        .as_i64()
        .expect("资金流转应自动生成台账凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{bvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["status"], "draft", "台账凭证应为草稿（H-3 不入余额）");
    assert_eq!(v["entries"][0]["account_code"], "100201");
    assert_eq!(v["entries"][1]["account_code"], "112101");
    assert!(v["entries"][0]["debit"]
        .as_str()
        .unwrap()
        .starts_with("5000"));

    // 补出凭证 / 删除已挂凭证的票据 → 被拒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/bills/{bid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已出凭证应拒绝补出");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/bills/{bid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已挂凭证的票据不可删除"
    );

    // 融资借款：到账凭证 + 结清自动生成还本凭证（幂等）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/loans",
            &sid,
            serde_json::json!({ "kind": "borrow", "no": "LN009", "bank": "工行",
                "principal": "10000", "rate_pct": "4.5", "start_date": "2026-01-01",
                "end_date": "2026-06-30", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建融资应成功");
    let lid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "到账凭证应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dvid = r["id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{dvid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "100201", "到账：借银行");
    assert_eq!(v["entries"][1]["account_code"], "2001", "到账：贷短期借款");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/voucher"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "到账凭证不可重复生成"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-20" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "结清应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let svid = r["voucher_id"].as_i64().expect("结清应自动生成还本凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{svid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "2001", "还本：借短期借款");
    assert_eq!(v["entries"][1]["account_code"], "100201", "还本：贷银行");
    // 结清幂等：重复结清不重复出凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/settle"),
            &sid,
            serde_json::json!({ "date": "2026-01-21" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["voucher_id"].is_null(), "重复结清不应重复出凭证：{r}");
}

/// 银行手工勾对/取消/清空。
#[tokio::test]
async fn web_bank_manual_link_and_clear() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "100201", "summary": "收", "debit": "500", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "500" }
        ]),
    )
    .await;

    // 对账单日期远离凭证日期，自动勾对容差 0 不命中
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/import",
            &sid,
            serde_json::json!({
                "ymm": 202601, "account": "100201",
                "text": "2026-01-25,收款,SN9,400.00,0.00,400.00\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/auto-match",
            &sid,
            serde_json::json!({ "ymm": 202601, "account": "100201", "tolerance": 0 }),
        ))
        .await
        .unwrap();
    let m: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        m["matched"],
        serde_json::json!(0),
        "金额不同不应自动勾对：{m}"
    );

    // 手工勾对 → 取消 → 清空
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/bank?period=202601&account=100201", &sid))
        .await
        .unwrap();
    let d: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let stmt_id = d["statements"][0]["id"].as_i64().unwrap();
    let entry_id = d["book"][0]["entry_id"].as_i64().unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/link",
            &sid,
            serde_json::json!({ "stmt_id": stmt_id, "entry_id": entry_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "手工勾对应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/unlink",
            &sid,
            serde_json::json!({ "stmt_id": stmt_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消勾对应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/bank/clear",
            &sid,
            serde_json::json!({ "ymm": 202601, "account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["removed"], serde_json::json!(1), "应清空 1 条对账单");
}

/// 报表分层（对标金蝶报表按角色授权）：业务岗只见业务报表；账簿/三大表/资金需 FinReport。
#[tokio::test]
async fn fin_report_vs_business_report() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;

    // 建订单专员 cl1（无 FinReport）与出纳 ca1（有 FinReport）
    for (u, role) in [("cl1", "order_clerk"), ("ca1", "cashier")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss,
                serde_json::json!({ "username": u, "display_name": u, "password": "Test12345" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台 {u}");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &boss,
                serde_json::json!({ "username": u, "display_name": u, "password": "", "role": role, "must_change_pwd": false }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {role}");
    }
    let mut sids = std::collections::HashMap::new();
    for u in ["cl1", "ca1"] {
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
        assert_eq!(resp.status(), StatusCode::OK, "{u} 改密");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 进账套"
        );
        sids.insert(u, sid);
    }
    let clerk = sids["cl1"].clone();
    let cashier = sids["ca1"].clone();

    // 业务岗：财务报表端点403（菜单隐藏 + 端点硬拦）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/balance-sheet?to=202601", &clerk))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应看到资产负债表"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/ledger?from=202601&to=202601", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "订单专员不应看到账簿");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/daily", &clerk))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "订单专员不应看到资金日报"
    );

    // 业务岗：业务报表仍可用（Report 保留）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/procure/quota?supplier=S01&item=140301",
            &clerk,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单专员应能查配额业务报表");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/order/change-log?type=po&id=1", &clerk))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "订单专员应能查订单变更");

    // 出纳（FinReport）：资金日报可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/daily", &cashier))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "出纳应能看资金日报");

    // 管理员：财务报表照常
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/balance-sheet?to=202601", &boss))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "管理员应能看资产负债表");
}

/// 写接口冒烟：状态码 <500，并抽查若干返回 200。
#[tokio::test]
async fn web_write_endpoints_smoke() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    // 前置：140301 带价入库（组装成本平移要求子件有成本价，0 价将被拒绝）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/po",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "supplier_code": "S01",
                "supplier_name": "供应商甲", "status": "Draft", "memo": "",
                "lines": [{ "item_code": "140301", "qty_ordered": "5", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "前置采购单");
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/receipt",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": 202601, "date": "2026-01-05", "qty": "5", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "前置到货");
    let post = |uri: &'static str, body: serde_json::Value| {
        let state = state.clone();
        let sid = sid.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_post(uri, &sid, body))
                .await
                .unwrap();
            (resp.status(), body_string(resp).await)
        }
    };

    // 逐项写入并断言 200（发现引擎错误/500 即为问题）
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("/api/period", serde_json::json!({ "ymm": 202601 })),
        ("/api/accounts/fill-defaults", serde_json::json!({})),
        (
            "/api/inventory/unit",
            serde_json::json!({ "item": "RM01", "base_unit": "个", "alt_unit": "箱", "factor": "12" }),
        ),
        (
            "/api/inventory/serial",
            serde_json::json!({ "item": "RM01", "serials": ["SN001", "SN002"], "batch_no": "B1", "date": "2026-01-10" }),
        ),
        (
            "/api/inventory/serial/out",
            serde_json::json!({ "serials": ["SN001"], "date": "2026-01-11" }),
        ),
        (
            "/api/inventory/adjust",
            serde_json::json!({ "period": 202601, "date": "2026-01-10", "item": "RM01", "delta": "5", "memo": "t" }),
        ),
        (
            "/api/inventory/assemble",
            serde_json::json!({ "parent": "140501", "children": [["140301", "1"]], "date": "2026-01-10", "memo": "t" }),
        ),
        (
            "/api/inventory/disassemble",
            serde_json::json!({ "parent": "140501", "children": [["140301", "1"]], "date": "2026-01-10", "memo": "t" }),
        ),
        (
            "/api/procure/req",
            serde_json::json!({ "period": 202601, "date": "2026-01-10", "item_code": "140301", "item_name": "原料", "qty": "1", "requester": "admin", "memo": "" }),
        ),
        (
            "/api/sales/quote",
            serde_json::json!({ "id": 0, "period": 202601, "date": "2026-01-10", "customer_code": "C01", "customer_name": "客户", "item_code": "140501", "item_name": "成品", "qty": "1", "unit_price": "10", "status": "draft", "memo": "" }),
        ),
        (
            "/api/routing/140301",
            serde_json::json!([{ "seq": 1, "op_code": "OP1", "op_name": "车", "work_center": "WC1", "std_hours": "1", "rate": "10" }]),
        ),
        (
            "/api/mrp/run",
            serde_json::json!({ "demands": [{ "item_code": "140501", "qty": "10", "source": "手工" }] }),
        ),
        (
            "/api/budget/versions",
            serde_json::json!({ "key": "V1", "name": "版本1", "is_current": false, "memo": "" }),
        ),
        (
            "/api/approvals",
            serde_json::json!({ "biz_kind": "test", "biz_id": 1, "title": "t", "approvers": ["boss"] }),
        ),
        (
            "/api/reports/notes",
            serde_json::json!({ "report_key": "balance-sheet", "period": 202601, "content": "附注" }),
        ),
        (
            "/api/archives",
            serde_json::json!({ "period": 202601, "kind": "voucher", "title": "t", "payload": "{}" }),
        ),
        (
            "/api/funds/bills",
            serde_json::json!({ "kind": "receivable", "no": "B001", "period": 202601, "issue_date": "2026-01-05", "due_date": "2026-03-05", "counterpart": "客户", "bank": "工行", "amount": "1000", "memo": "" }),
        ),
        (
            "/api/funds/loans",
            serde_json::json!({ "kind": "borrow", "no": "L001", "bank": "工行", "principal": "10000", "rate_pct": "4.5", "start_date": "2026-01-01", "end_date": "2026-12-31", "memo": "" }),
        ),
        (
            "/api/cost/configs",
            serde_json::json!({ "item": "140501", "method": "fifo", "standard_cost": "0" }),
        ),
        (
            "/api/templates",
            serde_json::json!({ "id": 0, "name": "月度模板", "memo": "", "entries": [] }),
        ),
    ];
    let mut bad = Vec::new();
    for (uri, body) in cases {
        let (st, text) = post(uri, body).await;
        if st.as_u16() >= 500 {
            bad.push(format!("{uri} → {st} {text}"));
        }
    }
    assert!(bad.is_empty(), "写接口出现 5xx：{bad:#?}");

    // 抽查关键写接口确实成功（避免上面只证明"没炸"）
    for (uri, body) in [
        (
            "/api/inventory/unit",
            serde_json::json!({ "item": "RM02", "base_unit": "个", "alt_unit": "箱", "factor": "6" }),
        ),
        (
            "/api/funds/bills",
            serde_json::json!({ "kind": "payable", "no": "B002", "period": 202601, "issue_date": "2026-01-05", "due_date": "2026-03-05", "counterpart": "供应商", "bank": "工行", "amount": "500", "memo": "" }),
        ),
        (
            "/api/cost/configs",
            serde_json::json!({ "item": "140502", "method": "moving_average", "standard_cost": "0" }),
        ),
        // 单号可省略（服务端自动生成）：UI 就是这么发的，缺 no 不能 422
        (
            "/api/procure/req",
            serde_json::json!({ "period": 202601, "date": "2026-01-10", "item_code": "140301", "item_name": "原料", "qty": "3", "status": "draft", "requester": "admin", "memo": "" }),
        ),
        (
            "/api/sales/quote",
            serde_json::json!({ "id": 0, "period": 202601, "date": "2026-01-10", "customer_code": "C01", "customer_name": "客户", "item_code": "140501", "item_name": "成品", "qty": "1", "unit_price": "10", "status": "draft", "prepared_by": "", "memo": "" }),
        ),
    ] {
        let (st, text) = post(uri, body).await;
        assert_eq!(st, StatusCode::OK, "{uri} 应成功：{text}");
    }

    // 列表接口的空筛选值应视为「全部」：UI 下拉默认值就是空串，
    // 若被当成具体类型去过滤（kind=''）会永远查不到已保存的数据。
    for uri in [
        "/api/funds/bills?kind=",
        "/api/funds/loans?kind=",
        "/api/archives?period=202601&kind=",
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri} 应成功");
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert!(
            !v["rows"].as_array().unwrap().is_empty(),
            "{uri} 空筛选应返回全部数据：{v}"
        );
    }
}

/// 订单页付款/收款：付款流水写不进去时，绝不能留下可被审核生成凭证的孤立草稿收付款单。
///
/// 修复前三步各自分别提交且顺序是「先建单 → 再建边 → 再建流水」，流水一失败，
/// 已提交的草稿单没有付款流水配对，而 `POST /api/funds/receipts/:id/audit`
/// 会照样给它生成付款凭证。修复后依赖写入在前、单据最后建。
#[tokio::test]
async fn payment_registration_leaves_no_orphan_receipt() {
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
    let d12 = format!("{}-12", dash["current_period"].as_str().unwrap());
    let d15 = format!("{}-15", dash["current_period"].as_str().unwrap());

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
    assert_eq!(resp.status(), StatusCode::OK, "建采购订单");
    let po_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15.clone(),
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Confirmed", "memo": "收款造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "10", "unit_price": "9", "tax_rate": "0" }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建销售订单");
    let so_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 让付款流水写不进去（触发器阻断 INSERT），模拟第 3 步失败
    block_inserts(&state, "b1", "po_payment");
    block_inserts(&state, "b1", "so_payment");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/procure/payment",
            &sid,
            serde_json::json!({ "po_id": po_id, "period": cur_ymm, "date": d12.clone(), "amount": "10", "memo": "付" }),
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_server_error(),
        "付款流水写失败应报错（当前 {}）",
        resp.status()
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/payment",
            &sid,
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d15.clone(), "amount": "10", "memo": "收" }),
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_server_error(),
        "收款流水写失败应报错（当前 {}）",
        resp.status()
    );

    // 关键断言：一条草稿收付款单都不该留下（它可被审核生成付款凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/funds/receipts", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "收付款单列表应可用：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(
        r["rows"].as_array().unwrap().len(),
        0,
        "付款/收款流水失败时不得留下孤立草稿收付款单：{b}"
    );
}

/// 删除融资记录必须留审计痕迹（原先是资金模块唯一不写日志的删除口）。
#[tokio::test]
async fn delete_loan_writes_audit_log() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/loans",
            &sid,
            serde_json::json!({ "kind": "borrow", "no": "LLOG1", "bank": "工行",
                "principal": "10000", "rate_pct": "4.5", "start_date": "2026-01-01",
                "end_date": "2026-12-31", "memo": "待删融资" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "建融资：{b}");
    let lid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/loans/{lid}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "删除融资：{b}");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/logs?limit=20", &sid))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "操作日志：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert!(
        r.as_array().unwrap().iter().any(|l| l["module"] == "资金"
            && l["action"] == "删除融资"
            && l["detail"] == format!("#{lid}")),
        "删除融资应留痕（资金/删除融资/#{lid}）：{b}"
    );
}

#[tokio::test]
async fn receipt_audit_gates_on_audit_or_post_permission() {
    let (state, _bd, _dir) = test_state();
    let (csh, pair) = two_person(&state, "rcptgate").await;
    let (acc, sup) = pair.split_once('|').unwrap();

    // 出纳录一张收款单（VoucherNew 够用）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/funds/receipts",
            &csh,
            serde_json::json!({
                "date": "2026-01-15", "kind": "receipt", "fund_account": "100201",
                "party": "C01", "amount": "100", "memo": "gate"
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "出纳应能录收付款单：{b}");
    let rid: i64 = serde_json::from_str::<serde_json::Value>(&b).unwrap()["id"]
        .as_i64()
        .unwrap();

    // ① 出纳自己审自己的单 → 必须 403。
    //    这是这道闸门的核心：放开给 VoucherPost 之后，"谁录谁审"这条路不能也一起放开。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/audit"),
            &csh,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::FORBIDDEN,
        "出纳不该能审核自己提的收付款单"
    );

    // ② 会计（无 VoucherAudit、有 VoucherPost）→ 应当能审。
    //    修前这里是 403，两人公司就卡死在这一步。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/audit"),
            acc,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "会计（VoucherPost）应能审核收付款单：{b}"
    );
    let vid = serde_json::from_str::<serde_json::Value>(&b).unwrap()["voucher_id"]
        .as_i64()
        .expect("审核后应生成凭证");
    assert!(vid > 0, "审核后应带出凭证 id：{b}");

    // ③ 撤审必须用**同一套**判据 —— 只放开"审"不放开"撤"，
    //    会出现「会计能审但撤不回来」，那比一开始就没有还糟。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/unaudit"),
            acc,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "会计应能撤销审核（与审核同一判据）：{b}"
    );

    // ④ 主管（有 VoucherAudit）当然也可以 —— 原有的路不能被改坏
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/audit"),
            sup,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "主管（VoucherAudit）应仍能审核：{b}");

    // ⑤ 出纳撤审同样 403
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{rid}/unaudit"),
            &csh,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN, "出纳不该能撤审");
}
