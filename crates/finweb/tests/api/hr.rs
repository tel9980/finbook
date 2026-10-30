//! 由 `tests/api.rs` 按域拆分而来（v1.8.59）。只搬位置、不改一行用例。

use super::*;

#[tokio::test]
async fn payroll_lifecycle_with_tax_and_vouchers() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 前置：建职员档案
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0, "kind": "employee", "code": "E001", "name": "张三",
                "parent_code": null, "disabled": false, "props": {}, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 录工资（后端算税）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202601",
            &sid,
            serde_json::json!({
                "employee": "E001", "dept": "销售部", "gross": "10000",
                "social": "800", "housing": "500", "deduction": "0",
                "additional": "1000", "social_co": "2000", "housing_co": "500", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "录工资应成功");
    let s = body_string(resp).await;
    let row: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert!(
        row["tax_base"].as_str().unwrap().parse::<f64>().unwrap() > 0.0,
        "计税基数应为正：{s}"
    );
    let net: f64 = row["net"].as_str().unwrap().parse().unwrap();
    assert!(net > 0.0 && net < 10000.0, "实发应在 0 与应发之间：{net}");

    // 列表回读
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll?period=202601", &sid))
        .await
        .unwrap();
    assert!(body_string(resp).await.contains("E001"), "工资表应含 E001");

    // 累计（202602 的 ytd 应含 202601 的 10000）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/payroll/ytd?period=202602&employee=E001",
            &sid,
        ))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("\"income\":\"10000"), "累计收入应为 10000：{s}");

    // 计提凭证 → 工资行挂上凭证 → 删除被拦截
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/accrue?period=202601",
            &sid,
            serde_json::json!({
                "date": "2026-01-31", "expense": "660201", "wage_payable": "221101",
                "social_payable": "221103", "housing_payable": "221104"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提凭证应成功");
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    assert!(vid > 0, "应返回凭证 id");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll?period=202601", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains(&vid.to_string()), "工资行应挂上凭证 id：{s}");
    let pid: i64 = {
        let rows: serde_json::Value = serde_json::from_str(&s).unwrap();
        rows[0]["id"].as_i64().unwrap()
    };
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/payroll/{pid}"), &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已挂凭证的工资行不可删除"
    );

    // 发放凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "2026-01-31", "payable_account": "221101", "bank_account": "100201",
                "tax_account": "222103", "social_account": "2241"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发放凭证应成功");

    // 社保缴纳凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/social-pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "2026-01-31", "social_payable": "221103", "housing_payable": "221104",
                "personal_payable": "2241", "bank_account": "100201"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "社保缴纳凭证应成功");
}

#[tokio::test]
async fn claim_lifecycle_to_voucher() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 新增草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/claims",
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-15", "applicant": "张三",
                "dept": "销售部", "reason": "差旅费", "amount": "500",
                // 明细备注留空：生成凭证时应回退用事由做摘要，不能因摘要为空被拒
                "items": [ { "expense_account": "660201", "amount": "500", "memo": "" } ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "建报销单应成功");
    let s = body_string(resp).await;
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let cid = v["id"].as_i64().unwrap();
    assert_eq!(v["no"], "BX202601-001", "单据号应自增：{s}");

    // 草稿期可改
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/claims/{cid}"),
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-16", "applicant": "张三",
                "dept": "销售部", "reason": "差旅费(改)", "amount": "600",
                "items": [ { "expense_account": "660201", "amount": "600", "memo": "机票" } ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "草稿应可修改");

    // 提交 → 审批 → 支付
    for st in ["submitted", "approved", "paid"] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                &format!("/api/claims/{cid}/transition"),
                &sid,
                serde_json::json!({ "status": st }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "流转到 {st} 应成功");
    }

    // 支付即自动落账：已付列表应带凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claims?period=202601&status=paid", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(
        !s.contains("\"voucher_id\":null"),
        "支付应自动生成付款凭证：{s}"
    );

    // 支付后不可再改内容
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            &format!("/api/claims/{cid}"),
            &sid,
            serde_json::json!({
                "period": 202601, "biz_date": "2026-01-16", "applicant": "张三",
                "dept": "销售部", "reason": "再改", "amount": "600", "items": []
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "已支付单据不可修改");

    // 生成凭证（明细合计与金额一致）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/claims/{cid}/voucher"),
            &sid,
            serde_json::json!({ "pay_account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "生成凭证应成功");
    let vid: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    assert!(vid > 0);

    // 幂等：重复请求返回同一张凭证（支付时已自动出账，手动请求不再新增）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/claims/{cid}/voucher"),
            &sid,
            serde_json::json!({ "pay_account": "100201" }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "重复请求应幂等返回");
    let vid2: i64 = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        ["id"]
        .as_i64()
        .unwrap();
    assert_eq!(vid2, vid, "幂等返回同一张凭证");

    // 已生成凭证 → 删除被拦截
    let resp = handlers::router(state.clone())
        .oneshot(authed_delete(&format!("/api/claims/{cid}"), &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "已挂凭证的报销单不可删除"
    );

    // 列表状态过滤
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claims?period=202601&status=paid", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("BX202601-001"), "已付列表应含该单：{s}");

    // 空状态 = 全部（UI 下拉默认空串），不能退化成只看草稿
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/claims?period=202601&status=", &sid))
        .await
        .unwrap();
    let s = body_string(resp).await;
    assert!(s.contains("BX202601-001"), "空状态应返回全部单据：{s}");
}

/// P2：银行代发文件 + 工资条 + 个税申报表。
#[tokio::test]
async fn payroll_bank_slip_tax() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 员工档案：E001 带银行账号，E002 无
    for (code, props) in [
        (
            "E001",
            serde_json::json!({ "bank_account": "6222001", "bank_name": "张三" }),
        ),
        ("E002", serde_json::json!({})),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/aux",
                &sid,
                serde_json::json!({
                    "id": 0, "kind": "employee", "code": code, "name": code,
                    "parent_code": null, "disabled": false, "props": props, "memo": ""
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "员工档案 {code}");
    }
    // 工资两条
    for (emp, gross) in [("E001", "10000"), ("E002", "8000")] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/payroll?period=202601",
                &sid,
                serde_json::json!({
                    "employee": emp, "dept": "财务部", "gross": gross,
                    "social": "500", "housing": "300", "deduction": "0",
                    "additional": "1000", "social_co": "800", "housing_co": "300", "memo": ""
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "工资 {emp}");
    }

    // 银行代发：CSV 含账号与户名；无账号员工跳过
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll/bank-file?period=202601", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let csv = body_string(resp).await;
    assert!(csv.contains("6222001"), "代发应含账号：{csv}");
    assert!(csv.contains("张三"), "户名取档案 bank_name：{csv}");
    assert!(!csv.contains("E002"), "无账号员工应跳过：{csv}");

    // 工资条：先补 202602 一条，验证 YTD 口径 = 截至上月（2 月时累计 1 个月）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202602",
            &sid,
            serde_json::json!({
                "employee": "E001", "dept": "财务部", "gross": "10000",
                "social": "500", "housing": "300", "deduction": "0",
                "additional": "1000", "social_co": "800", "housing_co": "300", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "2 月工资");
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/payroll/slip?period=202602&employee=E001",
            &sid,
        ))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["payroll"]["net"].as_str().is_some(), "工资条实发：{r}");
    assert_eq!(r["ytd"]["months"], 1, "2 月时本年累计 1 个月：{r}");
    assert!(
        r["ytd"]["income"].as_str().unwrap().contains("10,000"),
        "累计收入=1 月应发：{r}"
    );
    // 缺 employee → 400
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll/slip?period=202602", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "缺 employee 应 400");

    // 个税申报表：2 人
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/payroll/tax-report?period=202601", &sid))
        .await
        .unwrap();
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 2, "申报表 2 人：{r}");
}

/// 工资三类凭证回链：计提 / 社保缴纳 / 发放状态可见，且同类型重复生成被拦。
#[tokio::test]
async fn payroll_voucher_status_tracking() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 录入工资行（应发5000，个人社保200+公积金200，单位各500）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll?period=202601",
            &sid,
            serde_json::json!({
                "employee": "E01", "dept": "D01", "gross": "5000",
                "social": "200", "housing": "200", "deduction": "0",
                "additional": "0", "social_co": "500", "housing_co": "500", "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "录入工资行应成功");
    let r: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(r["id"].as_i64().unwrap() > 0);

    let get_rows = |sid: String| {
        let state = state.clone();
        async move {
            let resp = handlers::router(state)
                .oneshot(authed_get("/api/payroll?period=202601", &sid))
                .await
                .unwrap();
            serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()
        }
    };

    // 计提凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/accrue?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "expense": "660201", "wage_payable": "221101",
                "social_payable": "221103", "housing_payable": "221104"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "计提凭证应成功");

    // 社保缴纳凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/social-pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "social_payable": "221103", "housing_payable": "221104",
                "personal_payable": "2241", "bank_account": "100201"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "社保缴纳凭证应成功");

    // 发放凭证
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "payable_account": "221101", "bank_account": "100201",
                "tax_account": "222107", "social_account": "2241"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "发放凭证应成功");

    // 三个回链都应写回工资行（响应为数组）
    let rows = get_rows(sid.clone()).await;
    let arr = rows.as_array().expect("工资列表应为数组");
    let row = &arr[0];
    assert!(!row["voucher_id"].is_null(), "计提凭证应回链：{rows}");
    assert!(
        !row["social_voucher_id"].is_null(),
        "社保凭证应回链：{rows}"
    );
    assert!(!row["paid_voucher_id"].is_null(), "发放凭证应回链：{rows}");

    // 同类型重复生成被拦（同一期幂等/唯一约束）
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/payroll/pay?period=202601",
            &sid,
            serde_json::json!({
                "date": "", "payable_account": "221101", "bank_account": "100201",
                "tax_account": "222107", "social_account": "2241"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "重复生成发放凭证应被拦"
    );
}
