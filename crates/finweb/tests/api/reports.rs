//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

/// P0：数据范围全量接入——科目区间过滤账簿/报表/期初，范围外直接 403。
#[tokio::test]
async fn data_scope_ledgers() {
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

    // 造凭证：借 1001 100 / 贷 2001 100 → 记账
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": cur_ymm, "date": d15, "word": "记", "no": 92,
                "attachments": 0, "memo": "", "entries": [
                    { "line": 1, "account_code": "1001", "summary": "范围测试", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "范围测试", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "造范围测试凭证");
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

    // sc1：平台开号 → 首登改密 → 入套为会计 → 科目范围 1001..1001
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &sid,
            serde_json::json!({ "username": "sc1", "display_name": "范围会计", "password": "Sc1@pass99" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "平台开号 sc1");
    let (_, sc1_sid) = login(&state, "sc1", "Sc1@pass99").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/change-password",
            &sc1_sid,
            serde_json::json!({ "old": "Sc1@pass99", "new": "Sc1@pass99x" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "sc1 首登改密");
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/users",
            &sid,
            serde_json::json!({
                "username": "sc1", "display_name": "范围会计", "password": "",
                "role": "accountant", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "邀请 sc1 入套");
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/users/sc1",
            &sid,
            serde_json::json!({ "data_scope": { "account_from": "1001", "account_to": "1001" } }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "设置科目范围");
    assert_eq!(select_book(&state, &sc1_sid, "b1").await, StatusCode::OK);

    // 试算平衡：只含 1001，不含 2001
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/trial-balance?from={cur_ymm}&to={cur_ymm}"),
            &sc1_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let codes: Vec<String> = r["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["account_code"].as_str().unwrap().to_string())
        .collect();
    assert!(
        codes.iter().all(|c| c.starts_with("1001")),
        "范围外科目不应出现：{codes:?}"
    );
    assert!(
        !codes.iter().any(|c| c == "2001"),
        "2001 应不可见：{codes:?}"
    );

    // 明细账：1001 可看；2001 → 403
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/ledger/journal?code=1001&from={cur_ymm}&to={cur_ymm}"),
            &sc1_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "范围内科目可查");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/ledger/journal?code=2001&from={cur_ymm}&to={cur_ymm}"),
            &sc1_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "范围外科目应 403");

    // 数字钻取：2001 → 403
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/account-detail?account=2001&from={cur_ymm}&to={cur_ymm}"),
            &sc1_sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "范围外钻取应 403");

    // 期初列表：范围外不可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/begin", &sc1_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        rows.iter()
            .all(|x| x["account_code"].as_str().unwrap_or("").starts_with("1001")),
        "期初列表不应含范围外科目"
    );

    // 管理员视角：2001 正常可见
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/trial-balance?from={cur_ymm}&to={cur_ymm}"),
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
            .any(|x| x["account_code"] == "2001"),
        "管理员应可见 2001"
    );
}

/// 越权回归：只读（Viewer）与出纳不得写入这些端点。
///
/// 历史上预算版本 / 审批 / 报表附注 / 档案的写路由只用只读权限 Perm::Report 把关，
/// 而 Report 是每个角色（含 Viewer）都自带的最低权限，等于对只读账号开放了写入；
/// 模板 / 工资 / 报销的 DELETE 又误用了 Perm::VoucherNew，使无 VoucherDelete
/// 的出纳也能删。
#[tokio::test]
async fn readonly_roles_cannot_write() {
    let (state, _bd, _dir) = test_state();
    let (_, boss_sid) = login(&state, "boss", "Admin!2026").await;

    // 平台账号（首登强制改密）
    for u in ["view1", "cash1"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/platform/users",
                &boss_sid,
                serde_json::json!({ "username": u, "display_name": u, "password": "Init@123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "开通平台账号 {u}");
    }
    // boss 进入名下账套 b1，把两人拉成 Viewer / 出纳
    assert_eq!(
        select_book(&state, &boss_sid, "b1").await,
        StatusCode::OK,
        "boss 应能进入 b1"
    );
    for (u, role) in [("view1", "viewer"), ("cash1", "cashier")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/users",
                &boss_sid,
                serde_json::json!({
                    "username": u, "display_name": u, "password": "Init@123456",
                    "role": role, "must_change_pwd": false,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "邀请 {u} 为 {role}");
    }

    let mut sids = Vec::new();
    for u in ["view1", "cash1"] {
        let (st, sid) = login(&state, u, "Init@123456").await;
        assert_eq!(st, StatusCode::OK, "{u} 平台登录");
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/change-password",
                &sid,
                serde_json::json!({ "old": "Init@123456", "new": "Pass123456" }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{u} 首登改密");
        let (st, sid) = login(&state, u, "Pass123456").await;
        assert_eq!(st, StatusCode::OK, "{u} 改密后重登");
        assert_eq!(
            select_book(&state, &sid, "b1").await,
            StatusCode::OK,
            "{u} 应能进入 b1"
        );
        sids.push(sid);
    }
    let view_sid = sids[0].clone();
    let cash_sid = sids[1].clone();

    // Viewer 读权限仍在
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/budget/versions", &view_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "Viewer 应能读预算版本");

    // 但所有写入口一律 403
    for (uri, body) in [
        (
            "/api/budget/versions",
            serde_json::json!({ "key": "v1", "name": "回归" }),
        ),
        (
            "/api/approvals",
            serde_json::json!({ "biz_kind": "purchase", "biz_id": 1, "title": "t", "approvers": ["boss"] }),
        ),
        (
            "/api/reports/notes",
            serde_json::json!({ "report_key": "balance-sheet", "period": 202601, "content": "x" }),
        ),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(uri, &view_sid, body))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "Viewer 写 {uri} 应被拒"
        );
    }

    // 出纳无 VoucherDelete，不能删凭证模板
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete("/api/templates/1", &cash_sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "出纳删凭证模板应被拒");
}

/// 链7：数字钻取（科目明细账：期初 + 逐笔 + 运行余额 + 贷余负值）与缺参校验。
#[tokio::test]
async fn report_drill() {
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

    // 造两笔（借 1001 100 / 借 1001 200）+ 一笔贷方（1001 贷 30 → 余额转负验证方向）。
    // 口径：明细账与试算平衡同为「仅已记账」——每笔保存后立即记账（H-3 同款流程）。
    for (no, entries) in [
        (
            80,
            serde_json::json!([
                { "line": 1, "account_code": "1001", "summary": "drill-a", "debit": "100", "credit": "0" },
                { "line": 2, "account_code": "2001", "summary": "drill-a", "debit": "0", "credit": "100" }
            ]),
        ),
        (
            81,
            serde_json::json!([
                { "line": 1, "account_code": "1001", "summary": "drill-b", "debit": "200", "credit": "0" },
                { "line": 2, "account_code": "2001", "summary": "drill-b", "debit": "0", "credit": "200" }
            ]),
        ),
        (
            82,
            serde_json::json!([
                { "line": 1, "account_code": "1001", "summary": "drill-c", "debit": "0", "credit": "30" },
                { "line": 2, "account_code": "2001", "summary": "drill-c", "debit": "30", "credit": "0" }
            ]),
        ),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/vouchers",
                &sid,
                serde_json::json!({
                    "id": 0, "period": cur_ymm, "date": d15.clone(), "word": "记",
                    "no": no, "attachments": 0, "memo": "", "entries": entries
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "造凭证 #{no}");
        let vid = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
            ["id"]
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
        assert_eq!(resp.status(), StatusCode::OK, "记账凭证 #{no}");
    }

    // 明细：本期 from=to → begin=0、3 行、借合计 300、贷合计 30、末行余额 270
    let next_ymm: i32 = {
        let y = cur_ymm / 100;
        let m = cur_ymm % 100;
        if m == 12 {
            (y + 1) * 100 + 1
        } else {
            y * 100 + m + 1
        }
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/account-detail?account=1001&from={cur_ymm}&to={cur_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "明细账可用：{body}");
    let r: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 3, "3 行分录：{r}");
    assert_eq!(
        r["begin"].as_str().unwrap().parse::<f64>().unwrap(),
        0.0,
        "本期起无期初"
    );
    assert_eq!(
        r["total_debit"].as_str().unwrap().parse::<f64>().unwrap(),
        300.0,
        "借合计"
    );
    assert_eq!(
        r["total_credit"].as_str().unwrap().parse::<f64>().unwrap(),
        30.0,
        "贷合计"
    );
    let last = r["rows"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        last["balance"].as_str().unwrap().parse::<f64>().unwrap(),
        270.0,
        "运行余额 100+200-30：{last}"
    );
    assert_eq!(last["no"], "记82", "凭证号 word+no 拼接");
    assert_eq!(last["line_memo"], "drill-c", "分录摘要");

    // 期初：from=下期 → 无行、begin=本期累计 270
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/reports/account-detail?account=1001&from={next_ymm}&to={next_ymm}"),
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["rows"].as_array().unwrap().is_empty(), "下期无发生：{r}");
    assert_eq!(
        r["begin"].as_str().unwrap().parse::<f64>().unwrap(),
        270.0,
        "期初 = 本期累计：{r}"
    );

    // 缺 account → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/account-detail", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "缺 account 拒");
}

/// Web 辅助账 / 数量金额账 / 自定义报表。
#[tokio::test]
async fn web_aux_qty_and_custom_reports() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 应收：借 112201 客户 C01 1000 / 贷 600101 1000
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-06", "word": "记",
                "no": 1, "attachments": 0, "memo": "应收",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "销售", "debit": "1000", "credit": "0", "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "收入", "debit": "0", "credit": "1000" }
                ]
            }),
        ))
        .await
        .unwrap();
    let v1 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 存货：借 140301 存货 RM01 5×20=100 / 贷 1001 100
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-07", "word": "记",
                "no": 2, "attachments": 0, "memo": "入库",
                "entries": [
                    { "line": 1, "account_code": "140301", "summary": "入库", "debit": "100", "credit": "0", "aux": { "item": "RM01" }, "qty": "5", "price": "20" },
                    { "line": 2, "account_code": "1001", "summary": "付款", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
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
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // 辅助账（客户）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/aux-balance?kind=customer&from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let d: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        d["rows"][0]["key"],
        serde_json::json!("C01"),
        "辅助账应有客户 C01：{d}"
    );
    assert_eq!(d["rows"][0]["debit"], serde_json::json!("1000.00"));

    // 数量金额账
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/qty-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let q: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = q["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["account_code"] == serde_json::json!("140301"))
        .expect("数量金额账应含 140301");
    assert_eq!(
        row["qty_in"],
        serde_json::json!("5"),
        "入库数量应为 5：{row}"
    );
    assert_eq!(row["qty_end"], serde_json::json!("5"), "期末数量应为 5");

    // 自定义报表：QM("1001") 期末余额
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/custom-reports",
            &sid,
            serde_json::json!({
                "key": "", "name": "资金小表",
                "columns": ["期末"],
                "lines": [{ "name": "库存现金", "indent": 0, "formulas": ["QM(\"1001\")"], "bold": false }]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "保存自定义报表应成功");
    let saved: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(
        saved["errors"].as_array().unwrap().is_empty(),
        "公式应无语法错误：{saved}"
    );
    let key = saved["key"].as_str().unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            &format!("/api/custom-reports/{key}?period=202601"),
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(got["report"]["name"], serde_json::json!("资金小表"));
    let cell = got["values"][0][0].as_str().unwrap_or("");
    assert!(
        cell.contains("100.00"),
        "QM(\"1001\") 应算出 -100.00：{got}"
    );
}

/// H-3 定案：试算平衡默认只含已记账——草稿凭证不入账，记账后进入。
#[tokio::test]
async fn trial_balance_default_posted_only_h3() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 只保存、不记账（草稿）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            &sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": 1, "attachments": 0, "memo": "草稿口径",
                "entries": [
                    { "line": 1, "account_code": "1001", "summary": "草稿", "debit": "100", "credit": "0" },
                    { "line": 2, "account_code": "2001", "summary": "草稿", "debit": "0", "credit": "100" }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿凭证应可保存");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 草稿状态：本期发生额不含它
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let d = money_num(tb["totals"]["debit"].as_str().unwrap());
    assert!(
        (d - 0.0).abs() < 0.005,
        "H-3：草稿不应进试算平衡，本期借方 {d}：{tb}"
    );

    // 记账后进入
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    assert_ok(resp, "记账应成功").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let d = money_num(tb["totals"]["debit"].as_str().unwrap());
    assert!(
        (d - 100.0).abs() < 0.005,
        "H-3：记账后应进试算平衡，本期借方 {d}：{tb}"
    );
}

/// 报表/账簿的期间参数必须校验：敲错格式或区间倒置要 400，不能静默换一个区间。
///
/// 回归背景（真实生产事故级）：原来的写法是
/// `q.get("from").and_then(parse_period).unwrap_or(默认值)`。
/// `and_then` 在**解析失败**时也返回 `None`，于是「用户把期间敲错」和
/// 「用户没传期间」被混为一谈 —— 两者都静默回退到默认值。
///
/// 危险的地方在于：返回的报表**照样借贷平衡**。生产实测（10 万张已记账凭证、
/// 全年真实值 9,951,930）：
///
/// | from 传法 | 本期借方 | 是否正确 |
/// |---|---|---|
/// | `2026-01` / `202601` | 9,951,930 | ✓ |
/// | `2026-01-01`（日期格式） | 829,300 | ✗ 只算了一个月 |
/// | `garbage` | 829,300 | ✗ HTTP 200 |
/// | 不传 | 829,300 | ✗ HTTP 200 |
///
/// 也就是说财务数字错了，却**没有任何办法从结果上看出来** —— 「试算平衡表
/// 平不平」这个最自然的检查手段失效，因为它平。只能靠报错拦住。
///
/// 界面上起止期间是**无 `type` 的自由文本框**，敲错是常态而非例外。
#[tokio::test]
async fn report_period_params_reject_garbage_instead_of_silently_substituting() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造两个期间的数据，让「区间算错」在数字上真的看得出来
    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "收", "debit": "1000", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "1000" }
        ]),
    )
    .await;

    let debit_of =
        |v: &serde_json::Value| -> f64 { money_num(v["totals"]["end_debit"].as_str().unwrap()) };

    // 基线：合法格式（202601 期间式 / 2026-01 月份式）必须照常 200
    for q in ["from=202601&to=202601", "from=2026-01&to=2026-01", ""] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(&format!("/api/reports/trial-balance?{q}"), &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "合法/缺省区间必须 200：{q}");
    }

    // 核心回归：这些以前全部返回 200 + 一份「合法外观的错误报表」
    for bad in [
        "from=2026-01-01&to=2026-12-31", // 日期格式：拼错成日期
        "from=garbage&to=garbage",       // 乱填
        "from=2026%2F01&to=2026%2F12",   // 斜杠分隔
        "from=202613&to=202613",         // 月份 13，不存在
        "from=202613&to=202601",         // 不存在的月份
        "to=2026-12-31",                 // 只错 to
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(
                &format!("/api/reports/trial-balance?{bad}"),
                &sid,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "期间参数非法时必须 400，不能静默换区间返回一份平衡但错误的报表：{bad}"
        );
        // 错误信息要能让人知道怎么改
        let body = body_string(resp).await;
        assert!(
            body.contains("期间"),
            "400 的提示里应说明是期间的问题（便于用户自查）：{bad} -> {body}"
        );
    }

    // 区间倒置：SQL 是 period BETWEEN ?1 AND ?2，倒置会**安静地**返回空表，
    // 用户看到「一张凭证都没有」而真实原因是他填反了 —— 同样要拦
    for bad in ["from=202612&to=202601", "from=2026-06&to=2026-01"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(
                &format!("/api/reports/trial-balance?{bad}"),
                &sid,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "区间倒置必须 400，不能静默返回空表：{bad}"
        );
    }

    // 反向证明：合法区间仍能取到数（确认上面的 400 不是「接口全挂」）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(debit_of(&tb) > 0.0, "合法区间必须能取到发生额：{tb}");

    // 同一套校验必须覆盖其它报表/账簿端点，否则只是把漏洞挪了个位置
    for ep in [
        "/api/reports/balance-sheet?to=2026-12-31",
        "/api/reports/income-statement?from=garbage",
        "/api/reports/cash-flow?to=2026-01-01",
        "/api/reports/equity?period=garbage",
        "/api/reports/summary-table?from=garbage",
        "/api/reports/multi-column?main=1001&cols=2001&from=garbage",
        "/api/ledger?code=1001&from=2026-01-01",
        "/api/ledger/print-form?code=1001&from=garbage",
        "/api/overview?period=2026-01-01",
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(ep, &sid))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "该端点也必须校验期间参数：{ep}"
        );
    }
}

/// 三大报表勾稽：试算平衡、资产=负债+权益、利润表净利。
#[tokio::test]
async fn web_statements_tie() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "收", "debit": "1000", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "1000" }
        ]),
    )
    .await;
    post_voucher(
        &state,
        &sid,
        2,
        serde_json::json!([
            { "line": 1, "account_code": "660201", "summary": "费", "debit": "300", "credit": "0" },
            { "line": 2, "account_code": "1001", "summary": "付", "debit": "0", "credit": "300" }
        ]),
    )
    .await;

    // 试算平衡
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let tb: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let ed = money_num(tb["totals"]["end_debit"].as_str().unwrap());
    let ec = money_num(tb["totals"]["end_credit"].as_str().unwrap());
    assert!((ed - ec).abs() < 0.005, "试算应平衡：{tb}");

    // 资产负债表：资产总计 == 负债和权益总计
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/reports/balance-sheet?to=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bs: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = bs["table"]["rows"].as_array().unwrap();
    let pick = |no: &str| {
        rows.iter()
            .find(|r| r["no"] == serde_json::json!(no))
            .and_then(|r| r["values"][0].as_str())
            .map(money_num)
            .unwrap_or(f64::NAN)
    };
    let asset = pick("17");
    let liab_eq = pick("36");
    assert!(
        !asset.is_nan() && !liab_eq.is_nan(),
        "应有资产/负债权益总计行：{bs}"
    );
    assert!(
        (asset - liab_eq).abs() < 0.005,
        "资产总计 {asset} 应等于负债和权益总计 {liab_eq}"
    );
    assert!((asset - 700.0).abs() < 0.005, "资产总计应为 700：{asset}");

    // 利润表：净亏损 -300
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/income-statement?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    let is: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows = is["table"]["rows"].as_array().unwrap();
    let net = rows
        .iter()
        .find(|r| r["no"] == serde_json::json!("15"))
        .and_then(|r| r["values"][0].as_str())
        .map(money_num)
        .unwrap();
    assert!((net + 300.0).abs() < 0.005, "净利润应为 -300：{net}");

    // 现金流量表可生成（未标注时全部未分配，但结构完整）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/cash-flow?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// 打印/导出端点冒烟（均返回 200 且内容类型正确）。
#[tokio::test]
async fn web_print_and_pdf_endpoints() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    post_voucher(
        &state,
        &sid,
        1,
        serde_json::json!([
            { "line": 1, "account_code": "1001", "summary": "收", "debit": "100", "credit": "0" },
            { "line": 2, "account_code": "2001", "summary": "借", "debit": "0", "credit": "100" }
        ]),
    )
    .await;

    let prints = [
        "/api/reports/balance-sheet/print?to=202601",
        "/api/reports/income-statement/print?from=202601&to=202601",
        "/api/reports/cash-flow/print?from=202601&to=202601",
        "/api/reports/equity/print?from=202601&to=202601",
        "/api/reports/trial-balance/print?from=202601&to=202601",
        "/api/vouchers/print-form?period=202601",
        "/api/ledger/print-form?code=1001&from=202601&to=202601&type=detail",
        "/api/ledger/print-form?code=1001&from=202601&to=202601&type=general",
    ];
    for uri in prints {
        let resp = handlers::router(state.clone())
            .oneshot(authed_get(uri, &sid))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "打印端点应 200：{uri}");
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("text/html"), "应为 HTML：{uri} → {ct}");
    }

    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance/export?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/reports/trial-balance/pdf?from=202601&to=202601",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("application/pdf"), "应返回 PDF：{ct}");
}
