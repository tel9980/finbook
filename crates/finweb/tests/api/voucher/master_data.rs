//! 由 `tests/api/voucher.rs`（5,128 行）按域拆分而来。只搬位置、不改一行用例。
//!
//! 本册：主数据导入、预算、台账、跑单与备份。

use super::super::*;

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
