//! Web 端「批量粘贴」的 HTTP 层集成测试。
//!
//! 为什么解析的单元测试在 `fincore/src/batch/`（42 条），这里还要再写：
//! 那 42 条验的是**解析**对不对，这里验的是**端点**有没有把解析结果正确地
//! 送到前端 / 落进库。中间这段最容易出「解析对但没送到」的低级错 ——
//! 比如某个字段忘了加进 json!()，解析全对，前端表格却少一列。
//!
//! 另外三条是这个端点特有的约定，单元测试管不到：
//!   1. **导入只认前端回传的行**，不接受「重新解析一遍文本」
//!      （用户改过文本的话，重新解析等于「确认了 A、写进去 B」）
//!   2. **科目方向不接受前端传**，每次从科目表现取
//!      （前端传方向 = 让前端决定借贷，那正是期初不平的头号原因）
//!   3. 越权科目要么不出现在预览里，要么直接拒，不能静默写入

use axum::http::StatusCode;
use std::sync::Arc;
use tower::ServiceExt;

use finweb::handlers;
use finweb::state::WebState;

mod common;
use common::{authed_get, authed_post, authed_put, body_string, boss_in_b1, test_state};

async fn post(
    state: &Arc<WebState>,
    sid: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(uri, sid, body))
        .await
        .unwrap();
    let st = resp.status();
    let s = body_string(resp).await;
    let v = serde_json::from_str(&s).unwrap_or(serde_json::json!({"raw": s}));
    (st, v)
}

async fn put(
    state: &Arc<WebState>,
    sid: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_put(uri, sid, body))
        .await
        .unwrap();
    let st = resp.status();
    let s = body_string(resp).await;
    let v = serde_json::from_str(&s).unwrap_or(serde_json::json!({"raw": s}));
    (st, v)
}

async fn get(state: &Arc<WebState>, sid: &str, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(uri, sid))
        .await
        .unwrap();
    let st = resp.status();
    let s = body_string(resp).await;
    let v = serde_json::from_str(&s).unwrap_or(serde_json::json!({"raw": s}));
    (st, v)
}

// ---------------- 辅助档案 ----------------

#[tokio::test]
async fn 辅助档案预览不写库() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;

    let (st, v) = post(
        &state,
        &sid,
        "/api/aux-batch",
        serde_json::json!({
            "kind": "customer",
            "text": "C01,杭州明远\nC02,宁波华信"
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "预览应 200：{v}");
    assert_eq!(v["total"], 2);
    assert_eq!(v["importable"], 2);
    assert_eq!(v["empty"], false);

    // 每个字段都要送到前端 —— 少一个就是「解析对但表格少一列」
    let r0 = &v["rows"][0];
    assert_eq!(r0["code"], "C01");
    assert_eq!(r0["name"], "杭州明远");
    assert!(r0["skip"].is_null(), "可导入的行不该有跳过原因");

    // 关键：预览**没有写库**
    let (_, list) = get(&state, &sid, "/api/aux?kind=customer").await;
    let rows = list.as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 0, "预览就写库的话，用户还没确认就进去了");
}

#[tokio::test]
async fn 辅助档案只导入前端确认过的行() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;

    // 前端预览后**只勾了 C01**（模拟用户取消勾选 C02）
    let (st, v) = put(
        &state,
        &sid,
        "/api/aux-batch",
        serde_json::json!({
            "kind": "customer",
            "rows": [{"code": "C01", "name": "杭州明远"}]
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "导入应 200：{v}");
    assert_eq!(v["inserted"], 1);

    let (_, list) = get(&state, &sid, "/api/aux?kind=customer").await;
    let rows = list.as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 1, "只该导入确认过的那一条");
    assert_eq!(rows[0]["code"], "C01");
}

#[tokio::test]
async fn 辅助档案导入时跳过已存在() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;

    let ents: Vec<serde_json::Value> = ["C01,我的客户", "C02,我的供应商"]
        .iter()
        .map(|s| {
            let (c, n) = s.split_once(',').unwrap();
            serde_json::json!({
                "id": 0, "kind": "customer", "code": c, "name": n,
                "parent_code": null, "disabled": false, "props": {}, "memo": ""
            })
        })
        .collect();
    for e in ents {
        let (st, body) = post(&state, &sid, "/api/aux", e).await;
        assert_eq!(st, StatusCode::OK, "{body}");
    }

    // 再导入 C01：已存在，必须跳过且**不覆盖名字**
    let (st, v) = put(
        &state,
        &sid,
        "/api/aux-batch",
        serde_json::json!({
            "kind": "customer",
            "rows": [{"code": "C01", "name": "别的人改过的名字"}]
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["inserted"], 0, "已存在的不该被覆盖");
    assert!(v["skipped"].as_i64().unwrap_or(0) >= 1);

    let (_, list) = get(&state, &sid, "/api/aux?kind=customer").await;
    let rows = list.as_array().cloned().unwrap_or_default();
    let c01 = rows.iter().find(|r| r["code"] == "C01").unwrap();
    assert_eq!(c01["name"], "我的客户", "已有档案被覆盖了");
}

#[tokio::test]
async fn 辅助档案预览标出库中已有的() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;
    post(
        &state,
        &sid,
        "/api/aux",
        serde_json::json!({
            "id": 0, "kind": "supplier", "code": "S01", "name": "老供应商",
            "parent_code": null, "disabled": false, "props": {}, "memo": ""
        }),
    )
    .await;

    let (_, v) = post(
        &state,
        &sid,
        "/api/aux-batch",
        serde_json::json!({"kind": "supplier", "text": "S01,新名字\nS02,新供应商"}),
    )
    .await;
    assert_eq!(v["total"], 2);
    assert_eq!(v["importable"], 1);
    assert_eq!(v["skipped"], 1);
    // 跳过原因必须是**中文文案**（两端共用 fincore 的 label()）
    let skip = v["rows"][0]["skip"].as_str().unwrap_or("");
    assert!(
        skip.contains("已存在"),
        "跳过原因应说清为什么，实际：{skip}"
    );
    assert!(v["rows"][1]["skip"].is_null());
}

/// 回归：`inserted + skipped` 必须等于请求的行数。
///
/// 第一版把「本批已处理过」（用来防重复）和「库里已有的」放在同一个数组里，
/// 导入成功后把前者也当 skipped 报出去 —— 用户看到「已导入 2 条，跳过 2 条」，
/// 2 + 2 ≠ 2。数字对不上时用户第一反应是「这软件出 bug 了」，而真正的错只在
/// 一个返回字段里，肉眼极难发现。所以这条断言盯的是**恒等式**，不是某个具体值。
#[tokio::test]
async fn 导入与跳过之和等于请求行数() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;
    post(
        &state,
        &sid,
        "/api/aux",
        serde_json::json!({
            "id": 0, "kind": "employee", "code": "E01", "name": "老员工",
            "parent_code": null, "disabled": false, "props": {}, "memo": ""
        }),
    )
    .await;

    // 4 行：1 行库里已有、1 行批次内重复、2 行新
    let rows: Vec<serde_json::Value> = vec![
        serde_json::json!({"code": "E01", "name": "改名了"}),
        serde_json::json!({"code": "E02", "name": "新员工A"}),
        serde_json::json!({"code": "E02", "name": "新员工A重复"}),
        serde_json::json!({"code": "E03", "name": "新员工B"}),
    ];
    let (st, v) = put(
        &state,
        &sid,
        "/api/aux-batch",
        serde_json::json!({"kind": "employee", "rows": rows}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let ins = v["inserted"].as_i64().unwrap_or(-1);
    let skip = v["skipped"].as_i64().unwrap_or(-1);
    assert_eq!(
        ins + skip,
        4,
        "插入 {ins} + 跳过 {skip} ≠ 请求的 4 行 —— 用户会以为数字对不上、软件坏了"
    );
    assert_eq!(ins, 2, "两条新的应进");
    assert_eq!(skip, 2, "一条库里已有 + 一条批内重复");
}

// ---------------- 期初余额 ----------------

#[tokio::test]
async fn 期初预览给出试算() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;

    // 1001 是资产（借）、2001 是负债（贷）—— 方向由科目表决定，前端不传
    let (st, v) = post(
        &state,
        &sid,
        "/api/begin-batch",
        serde_json::json!({"text": "1001,50000\n2001,50000"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "期初预览应 200：{v}");
    assert_eq!(v["total"], 2);
    assert_eq!(v["importable"], 2);
    assert_eq!(v["rows"][0]["dir"], "debit", "1001 资产应归位到借方");
    assert_eq!(v["rows"][1]["dir"], "credit", "2001 负债应归位到贷方");
    // Web 端没有草稿区，粘贴后立刻提交，所以**必须**给试算反馈
    assert_eq!(v["trial"]["debit"], "50,000.00");
    assert_eq!(v["trial"]["credit"], "50,000.00");
    assert_eq!(v["trial"]["balanced"], true, "借贷相等应报平");
}

#[tokio::test]
async fn 期初预览标出未知科目并给建议() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;

    let (_, v) = post(
        &state,
        &sid,
        "/api/begin-batch",
        serde_json::json!({"text": "1001,5000\n10011,3000"}),
    )
    .await;
    assert_eq!(v["total"], 2);
    assert_eq!(v["importable"], 1);
    let skip = v["rows"][1]["skip"].as_str().unwrap_or("");
    assert!(skip.contains("无此科目"), "实际：{skip}");
    assert!(skip.contains("1001"), "该给出最接近的科目，实际：{skip}");
    // 坏行不进试算 —— 否则用户会以为「差一点」
    assert_eq!(v["trial"]["debit"], "5,000.00");
}

#[tokio::test]
async fn 期初导入落库并可再读回() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;

    let (st, v) = put(
        &state,
        &sid,
        "/api/begin-batch",
        serde_json::json!({
            "rows": [
                {"code": "1001", "dir": "debit",  "amount": "50000.00"},
                {"code": "2001", "dir": "credit", "amount": "50000.00"}
            ],
            "existing": []
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "期初导入应 200：{v}");
    assert_eq!(v["inserted"], 2);

    let (st, list) = get(&state, &sid, "/api/begin").await;
    assert_eq!(st, StatusCode::OK);
    let rows = list.as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 2);
    // 负债行的 year_begin 必须是**负数**（正=借、负=贷是 fincore 全引擎约定）
    //
    // 注意 Money 序列化成的是**字符串**（"50000.00"），不是 JSON 数字。
    // 用 as_f64() 取会静默拿到 0 —— 我第一版就是这么写的，测试报
    // 「贷方余额应存成负数，实际 0」，看起来像产品 bug，其实是取值方式错了。
    let r2001 = rows.iter().find(|r| r["account_code"] == "2001").unwrap();
    let amt = r2001["year_begin"].as_str().unwrap_or("0");
    assert!(amt.starts_with('-'), "贷方余额应存成负数，实际 {amt}");
    assert_eq!(amt, "-50000.00");
}

#[tokio::test]
async fn 期初导入跳过前端说已有的() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;

    let (st, v) = put(
        &state,
        &sid,
        "/api/begin-batch",
        serde_json::json!({
            "rows": [{"code": "1001", "dir": "debit", "amount": "5000"}],
            "existing": ["1001"]
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["inserted"], 0, "前端说草稿里有的，不该再写一遍");
    assert_eq!(v["skipped"], 1);

    let (_, list) = get(&state, &sid, "/api/begin").await;
    assert_eq!(list.as_array().map(|a| a.len()).unwrap_or(0), 0);
}

#[tokio::test]
async fn 期初导入跳过未知科目() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;

    let (st, v) = put(
        &state,
        &sid,
        "/api/begin-batch",
        serde_json::json!({
            "rows": [
                {"code": "1001", "dir": "debit", "amount": "5000"},
                {"code": "999999", "dir": "debit", "amount": "1"}
            ],
            "existing": []
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["inserted"], 1, "未知科目不该写进去");
    assert_eq!(v["skipped"], 1);
}

#[tokio::test]
async fn 期初导入批次内重复只进一条() {
    let (state, _d, _t) = test_state();
    let sid = boss_in_b1(&state).await;

    let (st, v) = put(
        &state,
        &sid,
        "/api/begin-batch",
        serde_json::json!({
            "rows": [
                {"code": "1001", "dir": "debit", "amount": "5000"},
                {"code": "1001", "dir": "debit", "amount": "9999"}
            ],
            "existing": []
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["inserted"], 1, "同一科目在一批里只能有一条");

    let (_, list) = get(&state, &sid, "/api/begin").await;
    let rows = list.as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 1);
    // Money 是字符串（见上面那条注释）
    assert_eq!(
        rows[0]["year_begin"].as_str(),
        Some("5000.00"),
        "应保留第一条的金额"
    );
}
