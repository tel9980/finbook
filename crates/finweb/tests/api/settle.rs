//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

/// 往来核销列表的「含未记账」只读开关（对标金蝶单据驱动模型）。
///
/// 核销/账龄/催款只认已记账分录（全仓 H-3 口径）；勾上「含未记账」能把待记账的
/// 往来一并列出来**查看**，但它们不可被核销——服务端 `settle` 会拒绝未记账的
/// 被核销方。回归：早先 `open_entries` 写 `status != 'void'`，草稿直接进了核销列表。
#[tokio::test]
async fn settle_open_include_draft_is_readonly() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 录一张应收凭证（112201 核算客户，必须带辅助核算），可选是否记账
    async fn mk(state: &Arc<WebState>, sid: &str, no: i32, post_it: bool) -> i64 {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                    "no": no, "attachments": 0, "memo": "含未记账开关造数",
                    "entries": [
                        { "line": 1, "account_code": "112201", "summary": "应收", "debit": "100", "credit": "0",
                          "aux": { "customer": "C01" } },
                        { "line": 2, "account_code": "600101", "summary": "应收", "debit": "0", "credit": "100" }
                    ]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "录应收凭证");
        let id: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            ["id"]
            .as_i64()
            .unwrap();
        if post_it {
            let r = handlers::router(state.clone())
                .oneshot(authed_post(
                    &format!("/api/vouchers/{id}/post"),
                    sid,
                    serde_json::json!({}),
                ))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK, "记账");
        }
        id
    }
    let posted_vid = mk(&state, &sid, 1, true).await;
    let draft_vid = mk(&state, &sid, 2, false).await;

    // 抓草稿凭证在 112201 上的分录 id
    let draft_eid: i64 = {
        let r = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/vouchers/{draft_vid}"), &sid))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["account_code"] == "112201")
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    // 已记账那张的分录 id
    let posted_eid: i64 = {
        let r = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/vouchers/{posted_vid}"), &sid))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["account_code"] == "112201")
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    };

    // 默认：只有已记账那笔
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(
        d["rows"].as_array().unwrap().len(),
        1,
        "默认不应列出未记账行"
    );
    assert_eq!(d["draft_count"], 0);
    assert_eq!(d["rows"][0]["posted"], true);

    // 开关打开：两笔都在，草稿那笔 posted=false
    let r = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601&draft=1",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(d["include_draft"], true);
    assert_eq!(
        d["rows"].as_array().unwrap().len(),
        2,
        "开关打开应列出未记账行"
    );
    assert_eq!(d["draft_count"], 1);
    let draft_row = d["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["posted"] == false)
        .expect("应有 posted=false 的行");
    assert_eq!(draft_row["entry_id"].as_i64(), Some(draft_eid));

    // 关键：草稿那笔不能当「原单」被核销（手工核销 entry id 由前端传入，
    // 绕过了列表过滤，只能靠 settle 自己兜）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/run",
            &sid,
            serde_json::json!({ "from_entry": draft_eid, "to_entry": posted_eid, "amount": "10" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "未记账债权不得被核销");
    let b: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert!(
        b["error"].as_str().unwrap_or("").contains("已记账"),
        "应说明需先记账：{b}"
    );

    // 核销记录不得留下
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/records?account=112201", &sid))
        .await
        .unwrap();
    let d: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let recs = d["rows"].as_array().cloned().unwrap_or_default();
    assert!(recs.is_empty(), "被拒的核销不得留下记录：{d}");
}

/// 往来对账单（对标金蝶「客户对账单」）：按期间出账单 → 发出 → 客户回签确认。
///
/// 与催款单不同：对账单走**完整流水**（含已核销），期初 + 本期增 - 减 = 期末，
/// 且创建时快照，之后改凭证不影响已发出的单子。
#[tokio::test]
async fn statement_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    /// 造一张 112201 应收凭证（带客户辅助核算），对方科目固定 1001 库存现金（无辅助核算）。
    /// `dr`/`cr` 是 112201 侧的借贷方，对方科目自动取相反方向保证借贷必平。
    async fn ar_voucher(
        state: &Arc<WebState>,
        sid: &str,
        date: &str,
        no: i32,
        dr: &str,
        cr: &str,
        post_it: bool,
    ) -> i64 {
        let opp_dr = if dr == "0" { cr } else { "0" };
        let opp_cr = if cr == "0" { dr } else { "0" };
        // 期间必须与日期一致（否则被「日期与所属期间不一致」校验挡下）：YYYYMM
        let ymm: i32 = format!("{}{}", &date[0..4], &date[5..7]).parse().unwrap();
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                sid,
                serde_json::json!({
                    "id": 0, "period": ymm, "date": date, "word": "记",
                    "no": no, "attachments": 0, "memo": "对账单造数",
                    "entries": [
                        { "line": 1, "account_code": "112201", "summary": "应收", "debit": dr, "credit": cr,
                          "aux": { "customer": "C01" } },
                        { "line": 2, "account_code": "1001", "summary": "对方", "debit": opp_dr, "credit": opp_cr }
                    ]
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "录应收凭证：{date}");
        let id: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            ["id"]
            .as_i64()
            .unwrap();
        if post_it {
            let r = handlers::router(state.clone())
                .oneshot(authed_post(
                    &format!("/api/vouchers/{id}/post"),
                    sid,
                    serde_json::json!({}),
                ))
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK, "记账");
        }
        id
    }

    // 1 月形成 1000 欠款；1 月收 400；2 月草稿 900（不进对账单）
    ar_voucher(&state, &sid, "2026-01-10", 1, "1000", "0", true).await;
    ar_voucher(&state, &sid, "2026-01-20", 2, "0", "400", true).await;
    ar_voucher(&state, &sid, "2026-02-10", 3, "900", "0", false).await;

    // 无往来的客商 → 400
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/statements",
            &sid,
            serde_json::json!({ "kind": "ar", "account": "112201", "party_code": "NOBODY",
                               "party_name": "", "from": 202601, "to": 202601, "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "无往来不应出对账单");

    // 正常出账：期初 0 / 增 1000 / 减 400 / 期末 600
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/statements",
            &sid,
            serde_json::json!({ "kind": "ar", "account": "112201", "party_code": "C01",
                               "party_name": "客户甲", "from": 202601, "to": 202601, "memo": "1月对账" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "生成对账单");
    let s: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let sid_id = s["id"].as_i64().unwrap();
    assert!(s["no"].as_str().unwrap().starts_with("DB202601"));
    assert_eq!(s["status"], "draft");
    assert_eq!(s["begin_balance"], "0.00");
    assert_eq!(s["period_increase"], "1,000.00");
    assert_eq!(s["period_decrease"], "400.00");
    assert_eq!(s["end_balance"], "600.00");
    // 草稿那笔不得进对账单
    let lines = s["lines"].as_array().unwrap();
    assert!(
        !lines
            .iter()
            .any(|l| l["increase"].as_str() == Some("900.00")),
        "草稿不得入账：{lines:?}"
    );
    // 末行滚动余额 == 期末
    assert_eq!(
        lines.last().unwrap()["balance"].as_str(),
        Some("600.00"),
        "逐行滚动余额必须与期末一致"
    );

    // 状态机：草稿 → 已发出
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "sent" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "发出对账单");
    // 重复发出 → 400
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "sent" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "已发出不应重复发出");

    // 回签确认必须填确认人
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "confirmed", "confirmed_by": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "回签必须填确认人");

    // 正常回签
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "confirmed", "confirmed_by": "客户王五" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "客户回签");
    let s: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(s["status"], "confirmed");
    assert_eq!(s["confirmed_by"], "客户王五");
    // 已确认是终态：不能作废
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/statements/{sid_id}/status"),
            &sid,
            serde_json::json!({ "status": "cancelled" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST, "已确认对账单不得作废");

    // 快照语义：出账后再补一笔已记账，已发出的对账单数字不变
    ar_voucher(&state, &sid, "2026-01-25", 4, "500", "0", true).await;
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/statements?kind=ar", &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let got = list["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"].as_i64() == Some(sid_id))
        .unwrap();
    assert_eq!(
        got["end_balance"], "600.00",
        "已发出的对账单不得被后续业务改动"
    );
}

/// P1：催款单/对账函——按客商快照未核销、状态流转、无欠款拒绝。
#[tokio::test]
async fn dunning_flow() {
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

    // 销售链造应收：SO C01 4×25 → 确认 → 发货（自动生成应收凭证）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d13,
                "customer_code": "C01", "customer_name": "客户甲",
                "status": "Draft", "memo": "催款造数",
                "lines": [{ "item_code": "140301", "item_name": "原料", "qty_ordered": "4", "unit_price": "25", "tax_rate": "0" }]
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
            serde_json::json!({ "so_id": so_id, "period": cur_ymm, "date": d13, "qty": "4", "memo": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发货");

    // 发货生成的应收凭证是草稿，往来核销/催款单只认**已记账**分录（H-3 口径），
    // 所以必须先记账，否则催款快照查不到任何未核销单据。
    // 回归：open_entries 早先写 `status != 'void'`，草稿也被算成欠款 ——
    // 那意味着可以对一张还没入账、随时可能删掉的凭证发催款函。
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/vouchers?period={cur_ymm}&status=draft"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "列草稿凭证");
    let drafts: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ar_vid = drafts
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["summary"].as_str().unwrap_or("").contains("发货确认"))
        .and_then(|v| v["id"].as_i64())
        .expect("应存在发货确认的应收凭证草稿");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{ar_vid}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "记账应收凭证");

    // 生成催款单：快照未核销应收 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/dunnings",
            &sid,
            serde_json::json!({ "kind": "ar", "account": "1122", "party_code": "C01", "party_name": "客户甲", "date": "", "memo": "首次催收" }),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let b = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "生成催款单：{b}");
    let r: serde_json::Value = serde_json::from_str(&b).unwrap();
    let d = &r["dunning"];
    let did = d["id"].as_i64().unwrap();
    let no = d["no"].as_str().unwrap().to_string();
    assert!(no.starts_with("CK"), "单号前缀：{no}");
    assert_eq!(d["status"], "draft");
    assert!(
        (d["amount"]
            .as_str()
            .unwrap()
            .replace(',', "")
            .parse::<f64>()
            .unwrap()
            - 100.0)
            .abs()
            < 0.005,
        "应收快照 100：{d}"
    );
    assert!(d["item_count"].as_i64().unwrap() >= 1, "明细至少一笔");
    assert!(!d["detail"].as_array().unwrap().is_empty(), "明细快照");

    // 无欠款客商 → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/dunnings",
            &sid,
            serde_json::json!({ "kind": "ar", "account": "1122", "party_code": "C99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "无欠款应 400");

    // 状态流转：draft → sent → settled；重复/越级 → 400
    for target in ["sent", "settled"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/settle/dunnings/{did}/status"),
                &sid,
                serde_json::json!({ "status": target }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "流转到 {target}");
    }
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/dunnings/{did}/status"),
            &sid,
            serde_json::json!({ "status": "settled" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "重复结清应 400");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/dunnings/{did}/status"),
            &sid,
            serde_json::json!({ "status": "cancelled" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已结清不可作废");

    // 列表包含该单
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/dunnings?kind=ar", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        r["rows"].as_array().unwrap().iter().any(|x| x["no"] == no),
        "列表应含 {no}"
    );

    // 未知 id → 404
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/dunnings/999999/status",
            &sid,
            serde_json::json!({ "status": "sent" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "未知催款单应 404");
}

/// 订单驱动全链条勾稽：销售订单 → 发货 → 开票 → 收款 → 应收对账
///
/// 这条用例的主题不是「每个环节能不能跑」，而是**贯穿全程的数字能不能对上**。
/// 单环节测试全绿但勾稽对不上，是 ERP 最常见也最难自查的一类错：
///   销售订单 100 件 / 价税合计 113000
///     → 发货 60 件应确认 60% 收入与应收
///     → 开票应与发货金额一致
///     → 收款应冲减应收
///     → 期末应收余额 = 113000 − 已收，未发货部分仍是应收
/// 任何一环口径错了，这条会红，而单环节测试不会。
#[tokio::test]
async fn order_driven_chain_ties_out() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 1) 销售订单：100 件 × 1000（13% 税）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/so",
            &sid,
            serde_json::json!({
                "period": 202601, "date": "2026-01-05", "status": "Draft",
                "customer_code": "C01", "customer_name": "客户甲",
                "lines": [{
                    "item_code": "140301", "item_name": "成品甲",
                    "qty_ordered": "100", "unit_price": "1000", "tax_rate": "0.13"
                }]
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(st, StatusCode::OK, "建销售订单应成功：{v}");
    let so_id = v["id"].as_i64().expect("应返回订单 id");

    // 2) 确认订单（此时不该有任何收入/应收）
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/sales/so/{so_id}/transition"),
            &sid,
            serde_json::json!({ "status": "Confirmed" }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "确认订单应成功：{b}");

    // 3) 发货 60 件 → 按比例确认收入与应收
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/shipment",
            &sid,
            serde_json::json!({
                "so_id": so_id, "period": 202601, "date": "2026-01-10", "qty": "60"
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "发货应成功：{b}");

    // 订单执行状态与执行进度应一致推进。
    //
    // 数值比较而不是字符串比较：之前这里写的是 `assert_ne!(shipped, "0")`，
    // 而实际值是 "0.00" —— **字符串不等，断言通过，bug 却还在**。这类
    // 「能通过但证明不了结论」的断言比没有断言更坏。
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/sales/so", &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let so = list["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == so_id)
        .expect("应能查到该订单")
        .clone();
    let shipped: f64 = so["shipped_amount"]
        .as_str()
        .unwrap_or("0")
        .replace(",", "")
        .parse()
        .unwrap_or(-1.0);
    assert!(
        shipped > 0.0,
        "发货后累计发货额不该是 0（状态已 PartialShip，两者是矛盾的）：{so}"
    );
    assert_eq!(
        so["status"],
        serde_json::json!("PartialShip"),
        "发 60/100 应是部分发货：{so}"
    );
    // 行级已发数量也要推进（订单执行进度落到行上）
    let line_shipped: f64 = so["lines"][0]["qty_shipped"]
        .as_str()
        .unwrap_or("0")
        .parse()
        .unwrap_or(-1.0);
    assert!(line_shipped > 0.0, "行级已发数量也不该是 0：{so}");

    // 4) 开票：从销售订单下推，金额应与订单一致
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/invoices/from-so",
            &sid,
            serde_json::json!({ "so_id": so_id }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "从销售订单下推发票应成功：{b}");
    let inv_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["invoice_id"]
        .as_i64()
        .expect("应返回发票 id");
    let r = handlers::router(state.clone())
        .oneshot(authed_get("/api/invoices", &sid))
        .await
        .unwrap();
    let invs: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    let inv = invs["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == inv_id)
        .expect("应能查到该发票")
        .clone();
    assert_eq!(
        inv["kind"],
        serde_json::json!("out"),
        "从销售订单下推的必须是销项发票：{inv}"
    );
    assert_eq!(
        inv["seller"],
        serde_json::json!("测试公司"),
        "销项发票的卖方应是本企业（账套公司名）：{inv}"
    );
    // 销项税额必须与订单税额一致（税率没被算歪）。
    // 用数值比而不是字符串比：订单列表与发票列表的千分位格式不同
    // （"13000.00" vs "13,000.00"），字符串比会给出假失败。
    let so_tax: f64 = so["total_tax"]
        .as_str()
        .unwrap_or("0")
        .replace(",", "")
        .parse()
        .unwrap_or(-1.0);
    let inv_tax: f64 = inv["tax"]
        .as_str()
        .unwrap_or("0")
        .replace(",", "")
        .parse()
        .unwrap_or(-2.0);
    assert!(
        (so_tax - inv_tax).abs() < 0.01,
        "下推发票的税额应等于订单税额：订单 {so_tax} vs 发票 {inv_tax}"
    );

    // 5) 收款 30000 → 应收应等额减少
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/sales/payment",
            &sid,
            serde_json::json!({
                "so_id": so_id, "period": 202601, "date": "2026-01-20", "amount": "30000"
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "收款应成功：{b}");
    let doc_id = serde_json::from_str::<serde_json::Value>(&b).unwrap()["doc_id"]
        .as_i64()
        .expect("收款应返回收付款单 id");

    // 5b) 审核收付款单 —— 凭证在这一步才生成（设计如此：审核后生成凭证 + 自动核销）。
    //     漏掉这一步会看到「应收没有减少」，那是流程没走完，不是链路断了。
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/funds/receipts/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "审核收付款单应成功：{b}");

    // 6) 把本链路产生的凭证全部记账。
    //    必须记：对账单只统计**已记账**凭证。链路上生成的收入/应收凭证、以及
    //    审核收付款单生成的收款凭证，此刻都还是草稿。
    //    这也正是真实流程：业务单据驱动出凭证 → 财务审核 → 记账。
    {
        let db = state.db_for("b1").unwrap();
        let ids: Vec<i64> = db
            .conn()
            .prepare("SELECT id FROM voucher WHERE period=202601 AND status='draft' ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!ids.is_empty(), "订单链应至少产生收入/应收凭证");
        for vid in ids {
            let r = handlers::router(state.clone())
                .oneshot(authed_post(
                    &format!("/api/vouchers/{vid}/post"),
                    &sid,
                    serde_json::json!({}),
                ))
                .await
                .unwrap();
            let st = r.status();
            let b = body_string(r).await;
            assert_eq!(st, StatusCode::OK, "凭证 {vid} 记账应成功：{b}");
        }
    }

    // 7) 应收对账：能生成，且期末余额 = 订单价税合计 − 已收
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/statements",
            &sid,
            serde_json::json!({
                "kind": "ar", "account": "112201",
                "party_code": "C01", "party_name": "客户甲",
                "from": 202601, "to": 202601
            }),
        ))
        .await
        .unwrap();
    let st = r.status();
    let b = body_string(r).await;
    assert_eq!(st, StatusCode::OK, "生成应收对账单应成功：{b}");
    let sm: serde_json::Value = serde_json::from_str(&b).unwrap();
    // 字段是 end_balance（不是 end_amount）；订单价税合计 = 不含税 + 税额
    let num = |x: &serde_json::Value| -> f64 {
        x.as_str()
            .unwrap_or("0")
            .replace(",", "")
            .parse()
            .unwrap_or(-1.0)
    };
    let gross_num: f64 = num(&so["total_amount"]) + num(&so["total_tax"]);
    let end_num: f64 = num(&sm["end_balance"]);
    // 应收期末 = **已发货确认的应收** − 已收
    //
    // 关键业务规则：**按发货比例确认收入/应收**，未发货的 40% 不形成应收。
    // 所以这里不能用订单全额算 —— 用全额断言会把「按发货确认」这个正确的行为
    // 判成 bug，或者反过来：哪天有人改成订单全额确认应收，这条也会红。
    // 两条都写死，才能锁住口径本身而不只是锁住数字。
    let shipped_qty: f64 = num(&so["lines"][0]["qty_shipped"]);
    let ordered_qty: f64 = num(&so["lines"][0]["qty_ordered"]);
    assert!(ordered_qty > 0.0, "应能读到订购数量：{so}");
    let shipped_ratio = shipped_qty / ordered_qty;
    let ar_confirmed = gross_num * shipped_ratio;
    let expect = ar_confirmed - 30000.0;
    assert!(
        (end_num - expect).abs() < 0.01,
        "应收期末应为 {expect}（已发货 {}% × 价税合计 {gross_num} = {ar_confirmed} − 已收 30000），\
         实际 {end_num} —— 订单链某处口径断了",
        (shipped_ratio * 100.0).round()
    );
    // 反向守卫：未发货部分不该形成应收（若哪天改成订单全额确认，这条会红）
    assert!(
        end_num < gross_num - 30000.0,
        "未发货部分形成了应收：期末 {end_num} ≥ 订单全额减已收 —— 收入确认口径变了"
    );
}

/// C 选项 + 导入 v2：存货档案一站式聚合（档案+计划参数+现量+单位）
/// ＋ 往来期初按单据（导入→账龄覆盖→列表合计→删除）。
#[tokio::test]
async fn items_master_and_arap_opening() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // ---- 往来期初：导入 3 行（2 应收 + 1 应付）+ 1 坏类型；幂等重导 ----
    let csv = "类型,客商编码,单据号,单据日期,金额,客商名称,备注\n\
               应收,Q01,XSQ-9001,2025-12-01,5000,青云客户,期初一\n\
               应收,Q01,XSQ-9002,2025-12-15,3000,青云客户,期初二\n\
               应付,S91,CGQ-9001,2025-12-20,2000,远航供应商,\n\
               坏类型,X01,BAD-1,2025-12-01,100,,\n";
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "arap_opening", "template": "generic", "text": csv }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "往来期初导入");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 3, "3 行有效：{r}");
    assert_eq!(r["skipped"], 1, "坏类型跳过：{r}");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({ "kind": "arap_opening", "template": "generic", "text": csv }),
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["ok"], 0, "幂等重导：{r}");
    assert_eq!(r["skipped"], 4, "全跳过：{r}");

    // 列表：3 行 + 合计应收 8000 / 应付 2000
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/arap-opening", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(
        money_num(r["total_ar"].as_str().unwrap()),
        8000.0,
        "应收合计：{r}"
    );
    assert_eq!(
        money_num(r["total_ap"].as_str().unwrap()),
        2000.0,
        "应付合计：{r}"
    );
    let first_id = r["rows"][0]["id"].as_i64().unwrap();

    // 账龄覆盖：1122 账龄应含期初客商 Q01（影子行注入）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/aging?account=1122&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "账龄可用");
    let aging_body = body_string(resp).await;
    assert!(aging_body.contains("Q01"), "期初客商应进账龄：{aging_body}");

    // 删除一行 → 2 行
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(
            &format!("/api/arap-opening/{first_id}"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "删除期初行");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/arap-opening", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 2, "删除后 2 行：{r}");

    // ---- 存货档案一站式：导入建档 + 计划参数 + 期初数量 ----
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "item", "template": "generic",
                "text": "编码,名称,保质期天,安全库存\nXM01,档案页存货,45,30\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "存货建档");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/item-plan",
            &sid,
            serde_json::json!({
                "item_code": "XM01", "safety_stock": "30",
                "lead_days": 3, "lot_size": "10"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计划参数保存");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/import/run",
            &sid,
            serde_json::json!({
                "kind": "opening_stock", "template": "generic",
                "text": "存货编码,仓库,数量,单价\nXM01,W01,25,2\n"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "期初数量");

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/items/master", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "存货档案端点");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let it = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["code"] == "XM01")
        .unwrap()
        .clone();
    assert_eq!(it["name"], "档案页存货", "{it}");
    assert_eq!(it["shelf_life"], "45", "保质期来自 props：{it}");
    assert_eq!(money_num(it["safety"].as_str().unwrap()), 30.0, "{it}");
    assert_eq!(it["lead_days"], 3, "{it}");
    assert_eq!(money_num(it["lot"].as_str().unwrap()), 10.0, "{it}");
    assert_eq!(
        money_num(it["qty"].as_str().unwrap()),
        25.0,
        "现量来自流水：{it}"
    );
    assert_eq!(it["disabled"], false, "{it}");
}

/// Web 往来核销：自动核销等额的一借一贷。
#[tokio::test]
async fn web_settle_auto_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 借 112201（客户 C01）1000 / 贷 1001 1000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let v1_body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "应收凭证应保存：{v1_body}");
    let v1 = serde_json::from_str::<serde_json::Value>(&v1_body).unwrap()["id"]
        .as_i64()
        .unwrap();
    // 收款：借 1001 / 贷 112201（客户 C01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20", "word": "记",
                "no": 2, "attachments": 0, "memo": "收款",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "1000", "credit": "0" },
                    { "line": 2, "account_code": "112201", "summary": "核销", "debit": "0", "credit": "1000", "aux": { "customer": "C01" } }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    for vid in [v1, v2] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "凭证 {vid} 应能记账");
    }

    // 核销前：两笔未核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        2,
        "核销前应有两笔未核销"
    );

    // 自动核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/auto",
            &sid,
            serde_json::json!({ "account": "112201", "ymm": 202601, "tolerance": "0.01" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["pairs"], serde_json::json!(1), "应核销一对：{r}");

    // 核销后：无未核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "核销后应无未核销"
    );
}

/// 往来手工核销 / 记录 / 账龄 / 取消核销。
#[tokio::test]
async fn web_settle_manual_records_aging() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(&state, &sid, 1, serde_json::json!([
        { "line": 1, "account_code": "112201", "summary": "销售", "debit": "500", "credit": "0", "aux": { "customer": "C01" } },
        { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "500" }
    ])).await;
    post_voucher(&state, &sid, 2, serde_json::json!([
        { "line": 1, "account_code": "1001", "summary": "收款", "debit": "200", "credit": "0" },
        { "line": 2, "account_code": "112201", "summary": "核销", "debit": "0", "credit": "200", "aux": { "customer": "C01" } }
    ])).await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601&all=1",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    let from_entry = rows
        .iter()
        .find(|r| r["dir"] == serde_json::json!("借"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();
    let to_entry = rows
        .iter()
        .find(|r| r["dir"] == serde_json::json!("贷"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/run",
            &sid,
            serde_json::json!({ "from_entry": from_entry, "to_entry": to_entry, "amount": "200" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "手工核销应成功");
    let rec_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/records?account=112201", &sid))
        .await
        .unwrap();
    let recs: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        recs["rows"].as_array().unwrap().len(),
        1,
        "应有 1 条核销记录"
    );

    // 父级科目查询口径应与未核销/账龄一致（1122 含 112201 的记录）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/records?account=1122", &sid))
        .await
        .unwrap();
    let recs_parent: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        recs_parent["rows"].as_array().unwrap().len(),
        1,
        "父级科目应能查到下级核销记录"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/aging?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ag: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        !ag["buckets"].as_array().unwrap().is_empty(),
        "账龄应有分档"
    );
    assert!(!ag["rows"].as_array().unwrap().is_empty(), "账龄应有数据");

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/unsettle",
            &sid,
            serde_json::json!({ "id": rec_id }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "取消核销应成功");
}

/// 全部只读接口冒烟：返回 2xx/4xx 均可，但不得 5xx（5xx = 引擎错误或 panic）。
#[tokio::test]
async fn web_read_endpoints_no_5xx() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    let uris = [
        "/api/me",
        "/api/roles",
        "/api/dashboard",
        "/api/overview?period=202601",
        "/api/periods",
        "/api/accounts",
        "/api/vouchers/next-no?period=202601&word=记",
        "/api/vouchers?period=202601",
        "/api/invoices?period=202601",
        "/api/invoices/summary",
        "/api/ledger?code=1001&from=202601&to=202601",
        "/api/ledger/general?code=1001&from=202601&to=202601",
        "/api/ledger/journal?code=1001&from=202601&to=202601",
        "/api/reports/trial-balance?from=202601&to=202601",
        "/api/reports/multi-column?main=1001&cols=1002&from=202601&to=202601",
        "/api/reports/summary-table?from=202601&to=202601",
        "/api/reports/ratios?period=202601",
        "/api/reports/balance-sheet?to=202601",
        "/api/reports/income-statement?from=202601&to=202601",
        "/api/reports/cash-flow?from=202601&to=202601",
        "/api/reports/equity?from=202601&to=202601",
        "/api/reports/compare?report_key=balance_sheet&from=202601&to=202601",
        "/api/reports/daily?code=1001&from=202601&to=202601",
        "/api/reports/reconcile?period=202601",
        "/api/reports/aux-balance?kind=customer&from=202601&to=202601",
        "/api/reports/qty-balance?from=202601&to=202601",
        "/api/reports/notes?report_key=balance-sheet",
        "/api/inventory/aging",
        "/api/inventory/abc",
        "/api/inventory/serial?item=RM01",
        "/api/inventory/unit?item=RM01",
        "/api/inventory/warehouse-stock?item=RM01",
        "/api/inventory/transfer?period=202601",
        "/api/procure/reconcile?period=202601",
        "/api/procure/quota?supplier=S01&item=140301",
        "/api/procure/price?item=140301",
        "/api/procure/track?po_id=1",
        "/api/procure/stats?period=202601",
        "/api/procure/req?period=202601",
        "/api/sales/reconcile?period=202601",
        "/api/sales/credit?customer=C01&period=202601",
        "/api/sales/track?so_id=1",
        "/api/sales/stats?period=202601",
        "/api/sales/quote?period=202601",
        "/api/order/change-log?period=202601",
        "/api/budget/alerts?period=202601",
        "/api/budget/versions",
        "/api/budget/analysis?period=202601",
        "/api/routing/140301",
        "/api/prod",
        "/api/mrp/latest",
        "/api/approvals",
        "/api/approvals/todo",
        "/api/archives?period=202601",
        "/api/funds/bills?period=202601",
        "/api/funds/loans",
        "/api/funds/daily?period=202601",
        "/api/funds/forecast?period=202601",
        "/api/cost/configs",
        "/api/logs?limit=10",
        "/api/templates",
        "/api/templates/due?period=202601",
        "/api/payroll?period=202601",
        "/api/payroll/ytd?employee=E001&period=202601",
        "/api/claims?period=202601",
        "/api/claims/next-no?period=202601",
        "/api/assets?period=202601",
        "/api/bank?period=202601&account=100201",
        "/api/settle/open?account=112201&upto=202601",
        "/api/settle/records?account=112201",
        "/api/settle/aging?account=112201&upto=202601",
        "/api/custom-reports",
    ];
    let mut bad = Vec::new();
    for uri in uris {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        let code = resp.status().as_u16();
        if code >= 500 {
            bad.push(format!("{uri} → {code}"));
        }
    }
    assert!(bad.is_empty(), "只读接口出现 5xx：{bad:#?}");
}

/// Web 差异核销（抹零）：应收 1000、实收 998、差 2 记入差异科目，
/// 被核销方全额结清、差异落到损益（出凭证），账龄不挂尾。
#[tokio::test]
async fn web_settle_diff_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收 1000（借 112201 客户 C01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "应收凭证应保存");
    let v1 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    // 收款 998（贷 112201 客户 C01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-20", "word": "记",
                "no": 2, "attachments": 0, "memo": "收款",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "收款", "debit": "998", "credit": "0" },
                    { "line": 2, "account_code": "112201", "summary": "收款", "debit": "0", "credit": "998", "aux": { "customer": "C01" } }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "收款凭证应保存");
    let v2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    for vid in [v1, v2] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "凭证 {vid} 应能记账");
    }

    // 取未核销的分录 id（借方 = 被核销方，贷方 = 核销方）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "应收/收款两笔未核销：{open}");
    let from = rows
        .iter()
        .find(|r| r["debit"] != serde_json::json!("0.00"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();
    let to = rows
        .iter()
        .find(|r| r["credit"] != serde_json::json!("0.00"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();

    // 差异核销：核销 998 + 差异 2 记入 660305
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/run",
            &sid,
            serde_json::json!({
                "from_entry": from, "to_entry": to,
                "amount": "998", "diff": "2", "diff_account": "660305"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "差异核销应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let dv = r["diff_voucher_id"].as_i64().expect("应生成差异凭证");

    // 应收全额结清
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "差异核销后应收应全额结清：{open}"
    );

    // 差异凭证：借 660305 / 贷 112201，金额 2
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{dv}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let debit = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["debit"] != serde_json::json!("0.00"))
        .unwrap();
    assert_eq!(debit["account_code"], "660305");
    assert_eq!(debit["debit"], serde_json::json!("2.00"));
}

/// Web 往来对冲：同一公司既欠我们（应收）、我们又欠它（应付），对冲 600 后
/// 两端各自 FIFO 核销，应收剩 400、应付清 0。
#[tokio::test]
async fn web_arap_offset_flow() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收 1000（借 112201 客户 C01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-05", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v1 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    // 应付 600（贷 220201 供应商 S01）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06", "word": "记",
                "no": 2, "attachments": 0, "memo": "应付",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "付款", "debit": "600", "credit": "0" },
                    { "line": 2, "account_code": "220201", "summary": "采购", "debit": "0", "credit": "600", "aux": { "supplier": "S01" } }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v2 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    for vid in [v1, v2] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                &sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "凭证 {vid} 应能记账");
    }

    // 守卫：对冲额超过任一端未核销合计 → 400（这里应付只有 600）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/offsets",
            &sid,
            serde_json::json!({
                "date": "2026-01-20", "customer": "C01", "supplier": "S01",
                "amount": "800", "memo": "超量"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "建单只落草稿，超量在审核时拦"
    );
    let over_id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{over_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "超过应付未核销应拒绝"
    );

    // 正常对冲 600
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/offsets",
            &sid,
            serde_json::json!({
                "date": "2026-01-20", "customer": "C01", "supplier": "S01",
                "amount": "600", "memo": "月末对冲"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "新增对冲单应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let doc_id = r["id"].as_i64().unwrap();
    assert_eq!(r["status"], "draft");
    assert!(r["voucher_id"].is_null(), "草稿阶段不应生成凭证");

    // 审核 → 出凭证 + 两端核销
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{doc_id}/audit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "审核应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["voucher_id"].as_i64().expect("审核应生成凭证");
    assert_eq!(r["settled"], serde_json::json!(2), "两端各核销一笔");

    // 凭证：借 220201 / 贷 112201，各 600
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "220201");
    assert_eq!(v["entries"][0]["debit"], serde_json::json!("600.00"));
    assert_eq!(v["entries"][1]["account_code"], "112201");
    assert_eq!(v["entries"][1]["credit"], serde_json::json!("600.00"));

    // 应收剩 400、应付清 0
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "应收应只剩一笔：{open}");
    assert_eq!(
        rows[0]["open"],
        serde_json::json!("400.00"),
        "应收对冲 600 后剩 400"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=220201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "应付应被全额冲销"
    );

    // 列表可见且已审核
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/offsets", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 2, "两单都在列表里");

    // 删除链：已审核 → 拒；撤审 → 回草稿且核销回退；再删 OK
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "凭证存在时不能删单");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{doc_id}/unaudit"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "撤审应成功");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/settle/offsets/{doc_id}/delete"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "撤审后应可删除");
}

/// Web 通用往来互抵：A 打来的钱清 B 的货款（同科目跨往来单位）、
/// 客户应收抵供应商应付（跨科目三方抵账）——都自动出过渡凭证并各自核销。
#[tokio::test]
async fn web_settle_cross_flow() {
    async fn add_posted(
        state: &Arc<WebState>,
        sid: &str,
        date: &str,
        entries: serde_json::Value,
    ) -> i64 {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                sid,
                serde_json::json!({
                    "id": 0, "period": 202601, "date": date, "word": "记",
                    "no": 0, "attachments": 0, "memo": "互抵测试", "entries": entries
                }),
            ))
            .await
            .unwrap();
        let status = resp.status();
        let body = body_string(resp).await;
        assert_eq!(status, StatusCode::OK, "建凭证应成功：{body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let vid = v["id"].as_i64().unwrap();
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/vouchers/{vid}/post"),
                sid,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "记账应成功");
        vid
    }

    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // B 的货款：借 112201 客户 B01 1000
    add_posted(
        &state,
        &sid,
        "2026-01-10",
        serde_json::json!([
            { "line": 1, "account_code": "112201", "summary": "B货款", "debit": "1000", "credit": "0", "aux": { "customer": "B01" } },
            { "line": 2, "account_code": "600101", "summary": "B货款", "debit": "0", "credit": "1000" }
        ]),
    )
    .await;
    // A 打来的钱记在 A 名下：贷 112201 客户 A01 1000
    add_posted(
        &state,
        &sid,
        "2026-01-15",
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "A代付", "debit": "1000", "credit": "0" },
            { "line": 2, "account_code": "112201", "summary": "A代付", "debit": "0", "credit": "1000", "aux": { "customer": "A01" } }
        ]),
    )
    .await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "B 的应收 + A 的收款：{open}");
    let debit_entry = rows
        .iter()
        .find(|r| r["debit"] != serde_json::json!("0.00"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();
    let credit_entry = rows
        .iter()
        .find(|r| r["credit"] != serde_json::json!("0.00"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();

    // 守卫：方向反了（拿贷方当借方）→ 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/cross",
            &sid,
            serde_json::json!({
                "debit_entry": credit_entry, "credit_entry": debit_entry, "amount": "1000"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "方向反了应拒绝");

    // 正常互抵 1000（同科目跨往来单位）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/cross",
            &sid,
            serde_json::json!({
                "debit_entry": debit_entry, "credit_entry": credit_entry,
                "amount": "1000", "date": "2026-01-31"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "跨往来单位互抵应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = r["bridge_voucher_id"].as_i64().expect("必须出过渡凭证");

    // 过渡凭证：借 112201（A 的收款）/ 贷 112201（B 的货款）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v["entries"][0]["account_code"], "112201");
    assert_eq!(v["entries"][0]["debit"], serde_json::json!("1000.00"));
    assert_eq!(v["entries"][1]["account_code"], "112201");
    assert_eq!(v["entries"][1]["credit"], serde_json::json!("1000.00"));

    // 两边同时清
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "互抵后应收应全清：{open}"
    );

    // ---- 跨科目三方抵账：客户 A02 欠我们 500，我们欠供应商 S02 300 ----
    add_posted(
        &state,
        &sid,
        "2026-01-12",
        serde_json::json!([
            { "line": 1, "account_code": "112201", "summary": "A02应收", "debit": "500", "credit": "0", "aux": { "customer": "A02" } },
            { "line": 2, "account_code": "600101", "summary": "A02应收", "debit": "0", "credit": "500" }
        ]),
    )
    .await;
    add_posted(
        &state,
        &sid,
        "2026-01-13",
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "S02应付", "debit": "300", "credit": "0" },
            { "line": 2, "account_code": "220201", "summary": "S02应付", "debit": "0", "credit": "300", "aux": { "supplier": "S02" } }
        ]),
    )
    .await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ar_entry = open["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["aux_key"] == serde_json::json!("customer=A02"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=220201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ap_entry = open["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["aux_key"] == serde_json::json!("supplier=S02"))
        .unwrap()["entry_id"]
        .as_i64()
        .unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/settle/cross",
            &sid,
            serde_json::json!({
                "debit_entry": ar_entry, "credit_entry": ap_entry,
                "amount": "300", "date": "2026-01-31"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "三方抵账应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid2 = r["bridge_voucher_id"]
        .as_i64()
        .expect("跨科目必须出过渡凭证");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(&format!("/api/vouchers/{vid2}"), &sid))
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["entries"][0]["account_code"], "220201",
        "过渡凭证借方应是应付"
    );
    assert_eq!(
        v["entries"][1]["account_code"], "112201",
        "过渡凭证贷方应是应收"
    );

    // 应收 A02 剩 200、应付 S02 清 0
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=112201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = open["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["open"],
        serde_json::json!("200.00"),
        "应收 A02 抵 300 后剩 200"
    );
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/open?account=220201&upto=202601",
            &sid,
        ))
        .await
        .unwrap();
    let open: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        open["rows"].as_array().unwrap().len(),
        0,
        "应付 S02 应全额冲销"
    );
}
