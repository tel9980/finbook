//! 由 `tests/api/voucher.rs`（5,128 行）按域拆分而来。只搬位置、不改一行用例。
//!
//! 本册：审核闸门、出纳、角色与数据范围。

use super::super::*;
use super::get_json;

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
