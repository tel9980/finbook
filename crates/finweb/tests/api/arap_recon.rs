//! 应收应付对账批次：先录对方报来的数，对好了才落账。
//!
//! 这一组守的是「对账过程中账上一分钱都不动」这条底线：
//! 建批次、按账面列名单、改金额、标争议……全程不生成任何凭证；
//! 只有「确认」那一刻，且只有差额，才生成一张**草稿**凭证。

use super::*;

/// 造一张已记账的应收凭证（112201 核算客户 C01）
async fn mk_ar(state: &Arc<WebState>, sid: &str, no: i32, amt: &str) -> i64 {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": no, "attachments": 0, "memo": "对账造数",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "应收", "debit": amt, "credit": "0",
                      "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "应收", "debit": "0", "credit": amt }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "录应收凭证");
    let id: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "记账");
    id
}

async fn get_json(state: &Arc<WebState>, sid: &str, path: &str) -> serde_json::Value {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(path, sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET {path}");
    serde_json::from_str(&body_string(resp).await).unwrap()
}

/// 全流程：建批次 → 按账面列名单 → 改对方金额 → 确认 → 差额变成一张草稿凭证。
#[tokio::test]
async fn recon_batch_confirm_turns_diff_into_draft_voucher() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_ar(&state, &sid, 1, "1000").await;

    // ① 建批次：默认挂 1122 / 112201，状态是对账中
    let v: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_post(
                    "/api/arap-recon/batches",
                    &sid,
                    serde_json::json!({ "kind": "ar", "ymm": 202601 }),
                ))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["batch"]["account"], "1122");
    assert_eq!(v["batch"]["post_account"], "112201");
    assert_eq!(v["batch"]["status"], "open");
    assert_eq!(v["batch"]["title"], "2026年01期 应收对账", "没起名字就自动取一个");

    // ② 按账面列名单：C01 的 1000 已经摆好，差额 0
    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/arap-recon/batches/{id}/seed"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let d = get_json(&state, &sid, &format!("/api/arap-recon/batches/{id}")).await;
    assert_eq!(d["summary"]["party_count"], 1);
    assert_eq!(d["summary"]["agreed"], 1, "账面和自己当然对得上");
    assert_eq!(money_num(d["lines"][0]["claim"].as_str().unwrap()), 1000.0);
    assert_eq!(money_num(d["summary"]["diff_total"].as_str().unwrap()), 0.0);
    assert!(d["book_only"].as_array().unwrap().is_empty());

    // ③ 对方报来 1200：改金额，差额自己冒出来，账上仍然一分钱没动
    let lid = d["lines"][0]["id"].as_i64().unwrap();
    let r = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/arap-recon/lines/{lid}"),
            &sid,
            serde_json::json!({ "claim": "1200" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let d = get_json(&state, &sid, &format!("/api/arap-recon/batches/{id}")).await;
    assert_eq!(money_num(d["lines"][0]["diff"].as_str().unwrap()), 200.0);
    assert_eq!(d["summary"]["need_adjust"], 1);
    assert_eq!(money_num(d["summary"]["diff_total"].as_str().unwrap()), 200.0);
    let before = get_json(&state, &sid, "/api/vouchers?period=202601").await;
    let n_before = before.as_array().map(|a| a.len()).unwrap_or(0);

    // ④ 确认：差额 200 生成**一张草稿**调整凭证，不自动记账
    let adj = "660305"; // 财务费用-其他：账套自带、末级、不核算辅助维度
    let v: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_post(
                    &format!("/api/arap-recon/batches/{id}/confirm"),
                    &sid,
                    serde_json::json!({ "adjust_account": adj }),
                ))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let vid = v["voucher_id"].as_i64().expect("差额该生成凭证");
    assert_eq!(money_num(v["diff_total"].as_str().unwrap()), 200.0);

    let d = get_json(&state, &sid, &format!("/api/arap-recon/batches/{id}")).await;
    assert_eq!(d["batch"]["status"], "confirmed");
    assert_eq!(d["batch"]["voucher_id"].as_i64().unwrap(), vid);
    let vou = get_json(&state, &sid, &format!("/api/vouchers/{vid}")).await;
    assert_eq!(vou["status"], "draft", "系统生成的凭证一律草稿");
    let after = get_json(&state, &sid, "/api/vouchers?period=202601").await;
    assert_eq!(
        after.as_array().map(|a| a.len()).unwrap_or(0),
        n_before + 1,
        "只该多这一张调整凭证"
    );

    // ⑤ 已确认的批次再改数：拒绝（否则账面和凭证就对不上了）
    let r = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/arap-recon/lines/{lid}"),
            &sid,
            serde_json::json!({ "claim": "1300" }),
        ))
        .await
        .unwrap();
    assert!(!r.status().is_success(), "已确认的批次不许改数");
}

/// 全部对上时：确认不生成凭证，账上什么都不多。
#[tokio::test]
async fn recon_batch_all_agreed_confirms_without_voucher() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_ar(&state, &sid, 1, "800").await;

    let v: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_post(
                    "/api/arap-recon/batches",
                    &sid,
                    serde_json::json!({ "kind": "ar", "ymm": 202601 }),
                ))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let id = v["id"].as_i64().unwrap();
    handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/arap-recon/batches/{id}/seed"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/arap-recon/batches/{id}/confirm"),
            &sid,
            serde_json::json!({ "adjust_account": "" }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert!(v["voucher_id"].is_null(), "对得上就不该凭空造凭证");

    let d = get_json(&state, &sid, &format!("/api/arap-recon/batches/{id}")).await;
    assert_eq!(d["batch"]["status"], "confirmed");
    assert_eq!(d["summary"]["agreed"], 1);
}

/// 粘贴导入：跳表头、跳坏行，同一户覆盖不重复。
#[tokio::test]
async fn recon_paste_import_skips_header_and_bad_rows() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_ar(&state, &sid, 1, "1000").await;

    let v: serde_json::Value = serde_json::from_str(
        &body_string(
            handlers::router(state.clone())
                .oneshot(authed_post(
                    "/api/arap-recon/batches",
                    &sid,
                    serde_json::json!({ "kind": "ar", "ymm": 202601 }),
                ))
                .await
                .unwrap(),
        )
        .await,
    )
    .unwrap();
    let id = v["id"].as_i64().unwrap();

    let r = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/arap-recon/batches/{id}/import"),
            &sid,
            serde_json::json!({
                "text": "客商编码,对账金额,备注\nC01,1200,11月加工费\nC01,1250\n坏行,abc\nC09,300"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(v["imported"], 3, "三行都认（C01 两次，后一次覆盖）");
    assert_eq!(v["skipped"], 1, "「坏行,abc」金额不是数，跳过");

    let d = get_json(&state, &sid, &format!("/api/arap-recon/batches/{id}")).await;
    let lines = d["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2, "同一户不重复");
    let c1 = lines
        .iter()
        .find(|l| l["party_code"] == "C01")
        .expect("C01");
    assert_eq!(money_num(c1["claim"].as_str().unwrap()), 1250.0, "后一次覆盖前一次");
    assert_eq!(money_num(c1["diff"].as_str().unwrap()), 250.0);
}
