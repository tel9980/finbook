//! 首页首屏（本期经营结果 + 钱在哪）的 HTTP 级回归。
//!
//! 这两块是老板打开系统第一眼要看的东西，而它们的取数全都跨模块
//! （利润走 `financial_totals`、钱在哪走 `money_position`、没记账走
//! `status_summary`）。任何一处口径跑偏，首页就会**自信地显示一个错数字** ——
//! 比报错更糟。
//!
//! 这里从「录凭证」开始，一路断言到 `/api/dashboard` 的 JSON，确保
//! 数字是**凭证推出来的**，不是代码里算出来的一厢情愿。

mod common;
use common::*;

use std::sync::Arc;

use axum::http::StatusCode;
use tower::ServiceExt;

use finweb::handlers;
use finweb::state::WebState;

/// 造一张凭证，返回 id
async fn voucher(
    state: &Arc<WebState>,
    sid: &str,
    ymm: i32,
    date: &str,
    no: i32,
    entries: serde_json::Value,
) -> i64 {
    let body = serde_json::json!({
        "id": 0, "period": ymm, "date": date, "word": "记", "no": no,
        "attachments": 0, "memo": "", "entries": entries,
    });
    let resp = handlers::router(state.clone())
        .oneshot(authed_post("/api/vouchers", sid, body))
        .await
        .unwrap();
    let status = resp.status();
    let txt = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "建凭证应成功：{txt}");
    serde_json::from_str::<serde_json::Value>(&txt).unwrap()["id"]
        .as_i64()
        .expect("应返回 id")
}

async fn post_voucher(state: &Arc<WebState>, sid: &str, id: i64) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            &format!("/api/vouchers/{id}/post"),
            sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let txt = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "记账应成功：{txt}");
}

/// 粘一批档案（tab 分隔），走与前端「批量粘贴」完全相同的两步
///
/// ⚠️ 只发 POST（解析预览）档案是**不会进库**的 —— 解析器故意不写库，前端要拿
/// 预览给用户确认后再 PUT。第一版这里只发了 POST，结果测试里客户名字全变成
/// 编码（`C001` 而不是「奇园江门」）、两家同名的 `both` 也是 false —— 看着像
/// 产品 bug，实际是夹具少发了一次请求。
async fn paste_aux(state: &Arc<WebState>, sid: &str, kind: &str, text: &str) {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux-batch",
            sid,
            serde_json::json!({ "kind": kind, "text": text }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let txt = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "粘档案预览应成功：{txt}");
    let preview: serde_json::Value = serde_json::from_str(&txt).unwrap();
    let rows = preview["rows"].clone();
    assert!(
        rows.as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "预览应有行：{txt}"
    );

    let resp = handlers::router(state.clone())
        .oneshot(authed_put(
            "/api/aux-batch",
            sid,
            serde_json::json!({ "kind": kind, "rows": rows }),
        ))
        .await
        .unwrap();
    let status = resp.status();
    let txt = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "导入档案应成功：{txt}");
    let done: serde_json::Value = serde_json::from_str(&txt).unwrap();
    assert!(
        done["inserted"].as_i64().unwrap_or(0) >= 1,
        "至少要进库一条，否则后面的名字断言会被编码糊过去：{txt}"
    );
}

async fn brief(state: &Arc<WebState>, sid: &str) -> serde_json::Value {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", sid))
        .await
        .unwrap();
    let status = resp.status();
    let txt = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "首页应 200：{txt}");
    let d: serde_json::Value = serde_json::from_str(&txt).unwrap();
    d["brief"].clone()
}

/// 首页首屏的数字必须是凭证推出来的
#[tokio::test]
async fn home_brief_is_derived_from_vouchers() {
    let (state, _books, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    paste_aux(&state, &sid, "customer", "C001\t奇园江门\n").await;
    paste_aux(&state, &sid, "supplier", "S001\t深圳奥科\n").await;

    // ① 卖货挂应收 100,000
    let v1 = voucher(
        &state,
        &sid,
        202601,
        "2026-01-10",
        1,
        serde_json::json!([
            {"line":0,"summary":"卖货","account_code":"112201","debit":"100000","credit":"0","aux":{"customer":"C001"}},
            {"line":1,"summary":"卖货","account_code":"600101","debit":"0","credit":"100000"}
        ]),
    )
    .await;
    post_voucher(&state, &sid, v1).await;

    // ② 房租水电记制造费用 15,000（走银行）
    let v2 = voucher(
        &state,
        &sid,
        202601,
        "2026-01-15",
        2,
        serde_json::json!([
            {"line":0,"summary":"房租水电","account_code":"510105","debit":"15000","credit":"0"},
            {"line":1,"summary":"房租水电","account_code":"100201","debit":"0","credit":"15000","aux":{"bank":"B01"}}
        ]),
    )
    .await;
    post_voucher(&state, &sid, v2).await;

    // ③ 外发加工费挂应付 8,000
    let v3 = voucher(
        &state,
        &sid,
        202601,
        "2026-01-20",
        3,
        serde_json::json!([
            {"line":0,"summary":"外发加工","account_code":"510105","debit":"8000","credit":"0"},
            {"line":1,"summary":"外发加工","account_code":"220201","debit":"0","credit":"8000","aux":{"supplier":"S001"}}
        ]),
    )
    .await;
    post_voucher(&state, &sid, v3).await;

    let b = brief(&state, &sid).await;
    assert_eq!(b["revenue"], "100,000.00", "营业收入应来自 600101：{b}");
    // 成本口径含**未结转的制造费用**（510105），这是 v1.8.12 修的那个口径：
    // 只算 6401+6402 会让房租水电整块消失在利润表外，净利润虚高。
    assert_eq!(b["cost"], "23,000.00", "营业成本应含 510105 制造费用：{b}");
    assert_eq!(
        b["net_profit"], "77,000.00",
        "净利润 = 100,000 − 23,000：{b}"
    );
    assert_eq!(b["margin"], 77.0, "利润率应 77.0：{b}");
    assert_eq!(b["unposted"], 0, "全部已记账，不该有警告：{b}");

    // 钱在哪：别人欠我 100,000（C001），我欠别人 8,000（S001）
    let m = &b["money"];
    assert_eq!(m["receivable"], "100,000.00", "别人欠我应 100,000：{m}");
    assert_eq!(m["payable"], "8,000.00", "我欠别人应 8,000：{m}");
    assert_eq!(m["net"], "92,000.00", "净头寸 = 100,000 − 8,000：{m}");
    assert_eq!(m["net_is_receivable"], true, "净头寸方向应是别人欠我：{m}");
    let top = &m["top_customers"][0];
    assert_eq!(top["name"], "奇园江门", "欠我最多的客户应带名字：{m}");
    assert_eq!(top["amount"], "100,000.00", "{m}");
    assert!(
        top["max_days"].as_i64().unwrap() >= 0,
        "账龄应是个非负数：{m}"
    );
}

/// 收入为零时利润率是「没法算」，不是 0%
///
/// 0% 会让人以为"这月白干"，实际是"这月还没开张"——两件事完全不同。
#[tokio::test]
async fn margin_is_null_when_revenue_is_zero() {
    let (state, _books, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let b = brief(&state, &sid).await;
    assert_eq!(b["revenue"], "0.00", "{b}");
    assert!(
        b["margin"].is_null(),
        "收入为 0 时利润率必须是 null（没法算），不能是 0：{b}"
    );
    assert_eq!(b["net_profit"], "0.00", "{b}");
    assert_eq!(b["net_profit_diff"], "0.00", "{b}");
}

/// 草稿凭证要出现在警告里，但不能算进利润
///
/// 这是首页最容易骗人的一处：老板录完凭证忘了记账，然后看着"这月只赚这点"
/// 百思不得其解。数字对（确实还没入账）+ 一句话说清楚，才是诚实的做法。
#[tokio::test]
async fn draft_vouchers_warn_but_do_not_count() {
    let (state, _books, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    paste_aux(&state, &sid, "customer", "C001\t奇园江门\n").await;

    // 只建不记账
    voucher(
        &state,
        &sid,
        202601,
        "2026-01-10",
        1,
        serde_json::json!([
            {"line":0,"summary":"卖货","account_code":"112201","debit":"50000","credit":"0","aux":{"customer":"C001"}},
            {"line":1,"summary":"卖货","account_code":"600101","debit":"0","credit":"50000"}
        ]),
    )
    .await;

    let b = brief(&state, &sid).await;
    assert_eq!(b["unposted"], 1, "应有 1 张没记账的凭证：{b}");
    assert_eq!(b["revenue"], "0.00", "草稿不该算进收入：{b}");
    // 首屏那行字也得说这件事。只守 `unposted` 的话，把这个数藏起来
    // （或者只拿去填概览、不喂给一句话结论）测试照样绿。
    assert_eq!(b["verdict"]["kind"], "unposted", "首页该先提醒去记账：{b}");
    assert_eq!(b["verdict"]["urgent"], true, "没记账属于急事：{b}");
    assert_eq!(
        b["verdict"]["view"], "vouchers",
        "「去处理」该跳到凭证查询：{b}"
    );
}

/// 同一家既是客户又是供应商（宏源那种）：两行都要能轧出净额
#[tokio::test]
async fn party_with_two_identities_shows_net() {
    let (state, _books, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    // 同一个名字，两个身份
    paste_aux(&state, &sid, "customer", "C009\t宏源\n").await;
    paste_aux(&state, &sid, "supplier", "S009\t宏源\n").await;

    // 他欠我 15,000
    let v1 = voucher(
        &state,
        &sid,
        202601,
        "2026-01-10",
        1,
        serde_json::json!([
            {"line":0,"summary":"拿货来加工","account_code":"112201","debit":"15000","credit":"0","aux":{"customer":"C009"}},
            {"line":1,"summary":"拿货来加工","account_code":"600101","debit":"0","credit":"15000"}
        ]),
    )
    .await;
    post_voucher(&state, &sid, v1).await;
    // 我欠他 28,000
    let v2 = voucher(
        &state,
        &sid,
        202601,
        "2026-01-12",
        2,
        serde_json::json!([
            {"line":0,"summary":"外发加工","account_code":"510105","debit":"28000","credit":"0"},
            {"line":1,"summary":"外发加工","account_code":"220201","debit":"0","credit":"28000","aux":{"supplier":"S009"}}
        ]),
    )
    .await;
    post_voucher(&state, &sid, v2).await;

    let b = brief(&state, &sid).await;
    let cust = &b["money"]["top_customers"][0];
    let supp = &b["money"]["top_suppliers"][0];
    for p in [cust, supp] {
        assert_eq!(p["both"], true, "两行都应标出「同一家两种身份」：{b}");
        assert_eq!(p["net"], "13,000.00", "净额应是 28,000 − 15,000：{b}");
    }
    // 方向：客户行「他欠我 15,000」少于「我欠他 28,000」→ 净我欠他
    assert_eq!(
        cust["net_is_receivable"], false,
        "客户行净额方向应翻成我欠他：{b}"
    );
    assert_eq!(supp["net_is_receivable"], false, "供应商行仍是我欠他：{b}");
}

/// 进入账套时的「当前期间」必须是**当前期间**，不是建账首月。
///
/// 曾经的实现是 `select_book` 里取 `db.options().start_period`，后果：
/// 账套结到 2026-09 之后，网页一登录仍停在 2026-01，首页「本期经营结果」
/// 「钱在哪」整块显示的是建账首月的数，而且不会自己动。桌面版一直是
/// `periods::current_period`（最后一个已结账期间的下一期），网页版没跟上。
#[tokio::test]
async fn selecting_a_book_lands_on_current_period_not_start_period() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    // 2026-01 录一张并记账、结账 → 当前期间应推进到 2026-02
    let id = voucher(
        &state,
        &sid,
        202601,
        "2026-01-15",
        1,
        serde_json::json!([
            {"line":0,"summary":"挂账","account_code":"112201","debit":"100","credit":"0","aux":{"customer":"C01"}},
            {"line":1,"summary":"收入","account_code":"600101","debit":"0","credit":"100"}
        ]),
    )
    .await;
    post_voucher(&state, &sid, id).await;
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/periods/202601/close",
            &sid,
            serde_json::json!({}),
        ))
        .await
        .unwrap();
    let st = resp.status();
    let txt = body_string(resp).await;
    assert_eq!(st, StatusCode::OK, "结账应成功：{txt}");

    // 重新进一次账套（就是登录后点账套那一步）
    let st = select_book(&state, &sid, "b1").await;
    assert_eq!(st, StatusCode::OK);

    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/dashboard", &sid))
        .await
        .unwrap();
    let txt = body_string(resp).await;
    let d: serde_json::Value = serde_json::from_str(&txt).expect("dashboard 应是 JSON");
    assert_eq!(
        d["closed_upto"], "2026-01",
        "前置条件：应已结账到 2026-01：{txt}"
    );
    assert_eq!(
        d["current_period"], "2026-02",
        "进账套后当前期间应停在「最后一个已结账期间的下一期」，不是建账首月 2026-01：{txt}"
    );
    // 首页那几个数字也得是 2026-02 的（这个月没录凭证 → 0），
    // 而不是 2026-01 那 100
    assert_eq!(
        d["brief"]["revenue"], "0.00",
        "首页应显示当前期间（2026-02）的数，不是建账首月的：{txt}"
    );
}

/// 「这几天有价要生效」：提前录的下一版价，到点了得有人提醒
///
/// 录完就忘是这张表最常见的坑：生效那天没人说，客户按新价结账、系统里还是旧价，
/// 一个月后对账才发现少收钱。首页只报窗口内（7 天）的，远处的别来占地方。
#[tokio::test]
async fn dashboard_warns_about_prices_taking_effect() {
    let (state, _books, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    paste_aux(&state, &sid, "item", "MB01\t氧化件\n").await;

    // ① 一条都没有时不占地方
    let b = brief(&state, &sid).await;
    assert_eq!(
        b["price_soon"].as_array().map(|a| a.len()),
        Some(0),
        "没有要生效的价就不该有这一块：{b}"
    );
    assert_eq!(b["price_soon_total"], 0, "{b}");

    let day = |n: i64| {
        (chrono::Local::now().date_naive() + chrono::Duration::days(n))
            .format("%Y-%m-%d")
            .to_string()
    };
    let mk = |from: String, price: &str, name: &str| {
        serde_json::json!({
            "name": name,
            "item_code": "MB01",
            "customer_code": "",
            "unit": "㎡",
            "unit_price": price,
            "min_charge": "0",
            "rush_fee": "0",
            "rush_rate": "0",
            "disabled": false,
            "memo": "",
            "effective_from": from,
            "process": "",
        })
    };

    // ② 三天后生效 → 进提醒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            mk(day(3), "0.15", "下月价"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let b = brief(&state, &sid).await;
    assert_eq!(b["price_soon_total"], 1, "三天后生效的要提醒：{b}");
    let row = &b["price_soon"][0];
    assert_eq!(row["date"], day(3));
    assert_eq!(row["item_code"], "MB01");
    assert_eq!(row["unit"], "㎡");
    assert_eq!(row["price"], "0.15");
    assert_eq!(row["today"], false, "三天后生效的不能说成今天生效：{b}");

    // ③ 下个月才生效 → 窗口外，不提醒
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/price-rules",
            &sid,
            mk(day(30), "0.12", "更远的价"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let b = brief(&state, &sid).await;
    assert_eq!(
        b["price_soon_total"], 1,
        "只报 7 天内的，远处的别来占首页：{b}"
    );
}

/// 一句话结论 + 「账有没有记错」：这是「打开软件第一眼」的两块
///
/// 判定只有一份（`findb::verdict`），桌面端首屏读的也是它。这里从 HTTP 层
/// 断言三件事：① 空账套说的是「还没开始录」而不是「一切正常」；
/// ② 有凭证没记账时这句话**压过**其他所有情况（那是最紧急的，因为此时利润偏小）；
/// ③ 试算结论由后端算，前端不自己猜。
#[tokio::test]
async fn dashboard_carries_verdict_and_trial_conclusion() {
    let (state, _books, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    paste_aux(&state, &sid, "customer", "C001\t奇园江门\n").await;

    // ① 空账套：让先去记第一笔；账本身是平的
    let b = brief(&state, &sid).await;
    assert_eq!(
        b["verdict"]["kind"], "no_revenue",
        "空账套该说「本月还没有收入凭证」：{b}"
    );
    assert_eq!(b["verdict"]["view"], "vouchers", "点这句话该去凭证页：{b}");
    assert_eq!(
        b["verdict"]["urgent"], false,
        "「还没开始录」不是紧急事项，不该用警示色：{b}"
    );
    assert!(
        b["verdict"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("本月还没有收入凭证"),
        "文案要直说没收入：{b}"
    );
    assert_eq!(b["trial"]["balanced"], true, "空账套借贷是平的：{b}");
    assert_eq!(b["trial"]["diff"], "0.00", "平的时候差额是 0.00：{b}");

    // ② 有凭证没记账 → 这句话必须排第一（此时利润偏小，拿去做决定会错）
    let v1 = voucher(
        &state,
        &sid,
        202601,
        "2026-01-10",
        1,
        serde_json::json!([
            {"line":0,"summary":"卖货","account_code":"112201","debit":"100000","credit":"0","aux":{"customer":"C001"}},
            {"line":1,"summary":"卖货","account_code":"600101","debit":"0","credit":"100000"}
        ]),
    )
    .await;
    let b = brief(&state, &sid).await;
    assert_eq!(b["verdict"]["kind"], "unposted", "先记账排最前：{b}");
    assert_eq!(b["verdict"]["urgent"], true, "这是要马上处理的：{b}");
    assert_eq!(b["verdict"]["view"], "vouchers", "{b}");
    assert!(
        b["verdict"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("还有 1 张凭证没记账"),
        "要说清有几张：{b}"
    );

    // ③ 记账之后：本月有收入、没人超期 → 兜底那句（不用跳转）
    post_voucher(&state, &sid, v1).await;
    let b = brief(&state, &sid).await;
    assert_eq!(b["verdict"]["kind"], "ok", "一切正常：{b}");
    assert_eq!(b["verdict"]["view"], "", "兜底那句没有地方可去：{b}");
    assert!(
        b["verdict"]["text"]
            .as_str()
            .unwrap_or("")
            .contains("收支正常"),
        "{b}"
    );
    assert_eq!(b["trial"]["balanced"], true, "记完账还是平的：{b}");
}
