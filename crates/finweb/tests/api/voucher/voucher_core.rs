//! 由 `tests/api/voucher.rs`（5,128 行）按域拆分而来。只搬位置、不改一行用例。
//!
//! 本册：凭证本身：分页、明细、附件、外币、改单、冲销。

use super::super::*;

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
