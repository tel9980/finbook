//! 客户视图 HTTP 层集成测试。
//!
//! 分工：findb 层的 `customers::tests` 验**口径**（余额怎么算），
//! 这层验**接口与权限**。唯一交叉点是 `customer_detail_balance_matches_list`
//! —— 若两层各自算一遍余额，界面上会「列表说欠 1000、点进去明细加起来 900」。

use axum::http::StatusCode;
use std::sync::Arc;
use tower::ServiceExt;

use finweb::handlers;
use finweb::state::WebState;

mod common;
use common::{
    authed_get, authed_post, body_string, boss_in_b1, login, money_num, select_book, test_state,
};

/// 建一个客户档案（同步：内部只有一次 oneshot().await，故须 async；
/// 这里包一层是因为路由调用本身就是异步的）
async fn mk_customer(state: &Arc<WebState>, sid: &str, code: &str, name: &str) {
    let req = authed_post(
        "/api/aux",
        sid,
        serde_json::json!({
            // id: 0 —— AuxEntity 是整结构反序列化，每个字段都必填
            // （fincore/src/auxiliary.rs:44，没有 serde(default)）
            "id": 0,
            "kind": "customer", "code": code, "name": name,
            "parent_code": null, "disabled": false, "props": {}, "memo": ""
        }),
    );
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "建客户档案 {code} 应成功：{}",
        body_string(resp).await
    );
}

/// 记一笔**已记账**的应收（借 112201 / 贷 1001，辅助核算挂客户）
///
/// 三步（审核 → 出纳签字 → 记账）不能省：`test_state()` 的夹具**显式**关掉了
/// 那两道闸门，所以本层其实只需 audit + post。但这里仍走三步 ——
/// 将来若有人把夹具改回生产默认值（那才是该做的），本测试不会突然红。
async fn post_ar(state: &Arc<WebState>, sid: &str, date: &str, cust: &str, amount: &str) {
    // period 由日期推导，不写死 202601 —— 写死的话，任何跨期间的用例都会
    // 撞上「凭证日期不在所选期间」的 400，而报错完全指不到真正的原因
    // （我写账龄用例时就这么撞了一次）。
    let ym = date.replace('-', "");
    let period: i32 = ym[..6].parse().expect("日期应能推出 YYYYMM");
    let req = authed_post(
        "/api/vouchers",
        sid,
        serde_json::json!({
            "id": 0, "period": period, "date": date, "word": "记", "no": 0,
            "attachments": 0, "memo": "",
            "entries": [
                { "line": 1, "account_code": "112201", "summary": "应收",
                  "debit": amount, "credit": "0", "aux": { "customer": cust } },
                { "line": 2, "account_code": "1001", "summary": "收",
                  "debit": "0", "credit": amount }
            ]
        }),
    );
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "录凭证：{}",
        body_string(resp).await
    );
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let vid = v["id"].as_i64().expect("应返回凭证 id");
    for step in ["audit", "sign", "post"] {
        let req = authed_post(
            &format!("/api/vouchers/{vid}/{step}"),
            sid,
            serde_json::json!({}),
        );
        let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "{step} 应成功：{}",
            body_string(resp).await
        );
    }
}

/// ① 列表能出数，且汇总字段与 rows 自洽
#[tokio::test]
async fn customers_list_returns_rows_and_consistent_totals() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_customer(&state, &sid, "C01", "客户甲").await;
    mk_customer(&state, &sid, "C02", "客户乙").await;
    post_ar(&state, &sid, "2026-01-05", "C01", "1000").await;
    post_ar(&state, &sid, "2026-01-06", "C02", "250").await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();

    let rows = v["rows"].as_array().expect("应返回 rows");
    assert_eq!(rows.len(), 2, "两个客户都该出现：{v}");
    assert_eq!(v["total"].as_u64(), Some(2), "total 要与 rows 长度一致");
    assert_eq!(v["open_count"].as_u64(), Some(2), "两人都未核销");
    assert_eq!(
        money_num(v["open_sum"].as_str().unwrap()),
        1250.0,
        "open_sum = 1000 + 250：{v}"
    );
    // 按未核销倒序：催款时先看到最该催的
    assert_eq!(rows[0]["code"].as_str(), Some("C01"), "欠款多的排前面");
    assert_eq!(
        money_num(rows[0]["balance"].as_str().unwrap()),
        1000.0,
        "余额取已记账分录"
    );
    assert_eq!(rows[0]["open_count"].as_u64(), Some(1), "未核销笔数");

    // only_open 过滤
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers?only_open=1", &sid))
        .await
        .unwrap();
    let v2: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(v2["rows"].as_array().unwrap().len(), 2);

    // 搜索过滤（按名称）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers?q=%E4%B9%99", &sid)) // URL 编码的「乙」
        .await
        .unwrap();
    let v3: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let rows3 = v3["rows"].as_array().unwrap();
    assert_eq!(rows3.len(), 1, "按名称搜「乙」应只有一条");
    assert_eq!(rows3[0]["code"].as_str(), Some("C02"));
}

/// ② **跨接口口径一致**：详情余额 == 列表余额 == 明细逐行相加
///
/// 这是 findb 层与 web 层之间唯一的交叉点，也是「余额只有一处算法」
/// 这条约束的真正考验。
#[tokio::test]
async fn customer_detail_balance_matches_list() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_customer(&state, &sid, "C01", "客户甲").await;
    post_ar(&state, &sid, "2026-01-05", "C01", "1200").await;
    post_ar(&state, &sid, "2026-01-09", "C01", "300").await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers", &sid))
        .await
        .unwrap();
    let list: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let from_list = money_num(list["rows"][0]["balance"].as_str().unwrap());

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers/C01", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let det: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    // 注意路径是**平的**：CustomerDetail 上有 #[serde(flatten)] summary，
    // 所以 balance 在 customer.balance，不在 customer.summary.balance。
    let from_detail = money_num(det["customer"]["balance"].as_str().unwrap());

    assert_eq!(
        from_list, from_detail,
        "列表与详情的余额必须一致（同一处算法）：列表 {from_list} vs 详情 {from_detail}"
    );

    let lines = det["customer"]["lines"].as_array().unwrap();
    let sum: f64 = lines
        .iter()
        .map(|l| money_num(l["amount"].as_str().unwrap()))
        .sum();
    assert_eq!(
        sum, from_detail,
        "明细逐行相加应等于汇总（否则界面上「对不上账」）：{det}"
    );
    assert_eq!(lines.len(), 2, "2 张凭证 = 2 行明细，实际 {}", lines.len());
    for l in lines {
        assert!(
            l["voucher_id"].as_i64().unwrap_or(0) > 0,
            "明细行应有 voucher_id：{l}"
        );
        assert!(!l["doc_no"].as_str().unwrap().is_empty(), "应有单据号：{l}");
    }
    assert_eq!(
        money_num(det["customer"]["opening_balance"].as_str().unwrap()),
        0.0,
        "没有期初挂账时该字段应为 0 而不是缺省（且路径是平的，见上）"
    );
}

/// ③ 权限分界：出纳能看，不能改档案
///
/// 设计 §4：出纳要看（催款）、要能核销，但**不该能改客户档案**。
/// 把「挂 Report 而不是 AuxEdit」这个决定钉住 ——
/// 将来若有人改成 AuxEdit（因为「客户管理页嘛」），这条会红。
#[tokio::test]
async fn cashier_can_read_customers_but_not_edit() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;

    // 账套内子账号必须**先**在平台层开通同名账号 —— 直接 POST /api/users
    // 会报 400「该账号尚未开通」。helpers.js 的 addBookUser 注释记着这件事，
    // 我写测试时忘了。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/platform/users",
            &boss,
            serde_json::json!({
                "username": "csh", "display_name": "出纳",
                "password": "Csh@2026x", "is_admin": false,
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
            &boss,
            serde_json::json!({
                "username": "csh", "display_name": "出纳", "password": "",
                "role": "cashier", "must_change_pwd": false
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "拉进出纳：{}",
        body_string(resp).await
    );

    let (_st, sid) = login(&state, "csh", "Csh@2026x").await;
    assert_eq!(select_book(&state, &sid, "b1").await, StatusCode::OK);

    // 能读
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "出纳应能看客户列表（催款要用）：{}",
        body_string(resp).await
    );

    // 不能改档案
    // payload 必须**完整**：缺 `id` 会被反序列化挡下（422），
    // 根本走不到权限检查 —— 那时这条测试红的原因就不是它想验的那件事。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux",
            &sid,
            serde_json::json!({
                "id": 0,
                "kind": "customer", "code": "CX", "name": "越权新建",
                "parent_code": null, "disabled": false, "props": {}, "memo": ""
            }),
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "出纳不该能新建客户档案 —— 若这条红了，说明 /api/aux 放宽了（payload 完整，故 403 来自权限而非反序列化）"
    );
}

/// ④ 不存在的客户 → 404 且报错点名是哪个客户
#[tokio::test]
async fn customer_missing_is_404_named() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers/NOPE", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let t = body_string(resp).await;
    assert!(
        t.contains("NOPE"),
        "报错要点名是哪个客户，否则用户分不清是编码打错还是没建档：{t}"
    );
}

/// ⑤ 账龄 Tab：**跨接口口径一致** —— 客户页这一行 == 账龄页这一行
///
/// 这是「桶定义与算法只有一处」（`settle::aging`）的真正考验。
/// 客户页若自己分一次桶，就会「客户页说 60 天以上 3000、账龄页说 0」，
/// 而两个数都是「按账龄算的」，没人会去怀疑口径不同。
#[tokio::test]
async fn customer_aging_matches_settle_aging_row() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_customer(&state, &sid, "C01", "客户甲").await;
    mk_customer(&state, &sid, "C02", "客户乙").await;
    // 两笔不同日期：必须落在**不同的桶**里，否则比对不出「桶错位」
    post_ar(&state, &sid, "2025-01-10", "C01", "3000").await;
    post_ar(&state, &sid, "2026-01-05", "C01", "500").await;
    post_ar(&state, &sid, "2026-01-06", "C02", "700").await;

    // 客户页：as_of 与账龄页取同一天，否则天数不同、桶也不同（那是有意的口径差异）
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/customers/C01/aging?as_of=2026-01-31",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{}", body_string(resp).await);
    let mine: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let a = &mine["aging"];
    assert_eq!(
        a["empty"],
        serde_json::json!(false),
        "有未核销余额，不该是 empty：{mine}"
    );
    let buckets = a["buckets"].as_array().expect("应有桶标签");
    let amounts_json = a["amounts"].as_array().expect("应有各档金额");
    assert_eq!(
        buckets.len(),
        amounts_json.len(),
        "桶标签与金额必须一一对应：{mine}"
    );

    // 账龄页：同一科目、同一天，取 C01 那一行
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(
            "/api/settle/aging?account=1122&upto=202601&as_of=2026-01-31",
            &sid,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let all: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let row = all["rows"]
        .as_array()
        .expect("账龄页应返回 rows")
        .iter()
        .find(|r| r["key"] == "C01")
        .unwrap_or_else(|| panic!("账龄页应有 C01 一行：{all}"))
        .clone();

    // 先比**数值**再比**字符串**，顺序不能反。
    //
    // 契约是「同一套桶、同一套算法 → 同一个数」，不是「同一个字符串」。
    // 我第一版直接比字符串，结果第一处失败是 `"0"` vs `"0.00"` —— 那是
    // 格式化差异（`Money` 直接序列化 vs `fmt_money`），不是算错。
    // 只比字符串会让「数值对但格式不同」被误判成口径不一致（假失败），
    // 只比数值又会让「同一个数长得不一样」悄悄过去（真问题被放过）。
    // 所以两者都断言，且各自说清自己在守什么。
    let mine_amounts: Vec<f64> = amounts_json
        .iter()
        .map(|x| money_num(x.as_str().unwrap()))
        .collect();
    let page_amounts: Vec<f64> = row["amounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| money_num(x.as_str().unwrap()))
        .collect();
    assert_eq!(
        mine_amounts, page_amounts,
        "各档金额必须逐档相等（同一套桶、同一套算法）：客户页 {mine} vs 账龄页 {all}"
    );
    assert_eq!(
        money_num(a["total"].as_str().unwrap()),
        money_num(row["total"].as_str().unwrap()),
        "合计应相等：客户页 {mine} vs 账龄页 {all}"
    );
    assert_eq!(
        buckets,
        all["buckets"].as_array().unwrap(),
        "桶标签本身也必须一致（桶定义只有一处）"
    );
    assert_eq!(
        amounts_json,
        row["amounts"].as_array().unwrap(),
        "两个页面的金额**字符串**也必须一模一样 —— 用户会把客户页与账龄页对着看，\
         「3,000.00」对「3000.00」会被当成算错：客户页 {mine} vs 账龄页 {all}"
    );
    // 3000 那笔已超 365 天，500 那笔不到 30 天 —— 两笔必须落在不同桶，
    // 否则这条测试比对的是一个「全落同一桶」的退化场景。
    let amounts: Vec<f64> = amounts_json
        .iter()
        .map(|x| money_num(x.as_str().unwrap()))
        .collect();
    let nonzero = amounts.iter().filter(|x| **x > 0.0).count();
    assert_eq!(
        nonzero, 2,
        "两笔应落在两个不同的桶里（桶 {buckets:?}，金额 {amounts:?}）"
    );
}

/// ⑥ 账龄 Tab 的两个边界：客户不存在 → 404；有档案但无余额 → empty
#[tokio::test]
async fn customer_aging_missing_customer_404_and_no_balance_is_empty() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;

    // 不存在的客户：与详情页一样 404，且点名。
    // 若返回 200 + 全 0，界面上会同时出现「无未核销余额」和「点开是 404」。
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers/NOPE/aging", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(
        body_string(resp).await.contains("NOPE"),
        "报错要点名客户编码"
    );

    // 有档案、无余额：200 + empty=true（而不是 404，也不是「算过了是 0」）
    mk_customer(&state, &sid, "C01", "客户甲").await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers/C01/aging", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{}", body_string(resp).await);
    let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        v["aging"]["empty"],
        serde_json::json!(true),
        "有档案但无未核销余额 → empty=true，界面显示「无未核销余额」：{v}"
    );
    assert_eq!(
        money_num(v["aging"]["total"].as_str().unwrap()),
        0.0,
        "total 应显式是 0.00 而不是缺字段：{v}"
    );
}

/// ⑦ 账龄 Tab 也挂 `Perm::Report`（出纳能看）
///
/// 挂 Report 而不是更严的权限：催款时出纳要能看账龄 —— 这是设计 §4 的取舍。
#[tokio::test]
async fn customer_aging_readable_by_cashier() {
    let (state, _bd, _dir) = test_state();
    let boss = boss_in_b1(&state).await;
    for (path, payload) in [
        (
            "/api/platform/users",
            serde_json::json!({
                "username": "csh2", "display_name": "出纳",
                "password": "Csh@2026y", "is_admin": false,
            }),
        ),
        (
            "/api/users",
            serde_json::json!({
                "username": "csh2", "display_name": "出纳", "password": "",
                "role": "cashier", "must_change_pwd": false
            }),
        ),
    ] {
        let resp = handlers::router(state.clone())
            .oneshot(authed_post(path, &boss, payload))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "{path}：{}",
            body_string(resp).await
        );
    }
    let (_st, sid) = login(&state, "csh2", "Csh@2026y").await;
    assert_eq!(select_book(&state, &sid, "b1").await, StatusCode::OK);
    mk_customer(&state, &boss, "C01", "客户甲").await;

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/customers/C01/aging", &sid))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "出纳应能看账龄（催款要看账龄，不是只有列表能看）：{}",
        body_string(resp).await
    );
}
