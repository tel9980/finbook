//! 核销「未核销清单」的分页与总数（v1.8.38）
//!
//! 为什么要这两条断言：这个接口原来一次返回全部未核销分录，v1.8.36 实测单科目
//! 1980 张凭证时是 **222.9 KB**，6000 张会到近 700 KB——手机上看一眼核销页要下
//! 半兆。改成分页之后有两个必须守住的性质：
//!
//! 1. **总数不能跟着页走**。第一页说「共 792 笔」、翻到第二页说「共 200 笔」，
//!    会计会以为钱变少了。
//! 2. **截断了必须明说**。悄悄少给几行，表现就是「这单明明还欠着，核销页里
//!    找不到」——比慢十倍更坏。
//!
//! `limit=0` 保留全量语义，是给导出/脚本留的后路。

mod common;
use common::*;

use axum::http::StatusCode;
use finweb::handlers;
use std::sync::Arc;
use tower::ServiceExt;

use finweb::state::WebState;

/// 录一张已记账的应收凭证（112201 核算客户，必须带 aux.customer）
async fn mk_ar(state: &Arc<WebState>, sid: &str, no: i32, amount: &str) -> i64 {
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/vouchers",
            sid,
            serde_json::json!({
                "id": 0, "period": 202601, "date": "2026-01-10", "word": "记",
                "no": no, "attachments": 0, "memo": "分页造数",
                "entries": [
                    { "line": 1, "account_code": "112201", "summary": "应收", "debit": amount, "credit": "0",
                      "aux": { "customer": "C01" } },
                    { "line": 2, "account_code": "600101", "summary": "应收", "debit": "0", "credit": amount }
                ]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "录应收凭证");
    let id = serde_json::from_str::<serde_json::Value>(&body_string(resp).await).unwrap()["id"]
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

async fn get_json(state: &Arc<WebState>, sid: &str, uri: &str) -> serde_json::Value {
    // `boss_in_b1` 返回的已经是完整的 cookie 串（`finbook_sid=…`），
    // 别再自己拼一次 `format!("finbook_sid={sid}")`——那样会变成
    // `finbook_sid=finbook_sid=…`，接口一律 401。
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(uri, sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    serde_json::from_str(&body_string(resp).await).unwrap()
}

#[tokio::test]
async fn settle_open_pages_without_losing_the_total() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    for (i, amt) in ["100", "200", "300"].iter().enumerate() {
        mk_ar(&state, &sid, i as i32 + 1, amt).await;
    }

    // 默认不截断（3 笔 < 200），全部给出
    let all = get_json(&state, &sid, "/api/settle/open?account=112201&upto=202601").await;
    assert_eq!(all["total"], 3, "三张应收单都应未核销");
    assert_eq!(all["returned"], 3);
    assert_eq!(all["truncated"], false, "没截断就不要说自己截断了");
    assert_eq!(
        all["sum_debit"], "600.00",
        "合计按未核销金额算：100+200+300"
    );

    // 显式 limit=2：给两行，但总数仍是 3
    let p1 = get_json(
        &state,
        &sid,
        "/api/settle/open?account=112201&upto=202601&limit=2",
    )
    .await;
    assert_eq!(p1["returned"], 2);
    assert_eq!(p1["total"], 3, "总数不能跟着页走 —— 翻页时钱不能变少");
    assert_eq!(p1["truncated"], true);
    assert_eq!(p1["sum_debit"], "600.00", "合计是全部未核销的，不是本页的");

    // offset 翻页：第二页只剩 1 行，总数还是 3
    let p2 = get_json(
        &state,
        &sid,
        "/api/settle/open?account=112201&upto=202601&limit=2&offset=2",
    )
    .await;
    assert_eq!(p2["returned"], 1);
    assert_eq!(p2["total"], 3);
    let ids1: Vec<i64> = p1["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["entry_id"].as_i64().unwrap())
        .collect();
    let ids2: Vec<i64> = p2["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["entry_id"].as_i64().unwrap())
        .collect();
    assert!(ids2.iter().all(|i| !ids1.contains(i)), "两页不能出现同一行");

    // limit=0 = 全量（导出与脚本的后路），不能被上限悄悄砍掉
    let full = get_json(
        &state,
        &sid,
        "/api/settle/open?account=112201&upto=202601&limit=0",
    )
    .await;
    assert_eq!(full["returned"], 3);
    assert_eq!(full["truncated"], false);
}

/// 上限 2000 要真的夹住：写 `limit=999999` 不能把 700KB 的包又拉回来。
#[tokio::test]
async fn settle_open_limit_is_capped() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_ar(&state, &sid, 1, "100").await;
    let d = get_json(
        &state,
        &sid,
        "/api/settle/open?account=112201&upto=202601&limit=999999",
    )
    .await;
    assert_eq!(d["limit"], 2000, "超过上限要按上限回复，而不是照抄请求值");
}

/// 关键字过滤要能在截断之前生效：会计记得「0031 号单」，就不该逼他翻页。
#[tokio::test]
async fn settle_open_keyword_filters_before_paging() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_ar(&state, &sid, 1, "100").await;
    mk_ar(&state, &sid, 2, "200").await;

    let hit = get_json(
        &state,
        &sid,
        "/api/settle/open?account=112201&upto=202601&q=%E8%AE%B0-2",
    )
    .await;
    assert_eq!(hit["total"], 1, "按凭证号「记-2」筛出 1 张");
    assert_eq!(hit["rows"][0]["debit"], "200.00");

    let miss = get_json(
        &state,
        &sid,
        "/api/settle/open?account=112201&upto=202601&q=%E6%B2%A1%E6%9C%89%E8%BF%99%E4%B8%AA",
    )
    .await;
    assert_eq!(miss["total"], 0, "查不到就是 0，不能退化成「全给你」");
}

/// 账龄下钻：按往来对象过滤 + 每张单「挂了几天」。
///
/// 账龄表只回答「最老 148 天」，答不了「这 5 万是哪几张单、各挂多久」——
/// 而这正是催款第一句要问的。天数必须按期间末算，与账龄的 `max_days` 同基准。
#[tokio::test]
async fn settle_open_drills_into_one_party_with_age_in_days() {
    let (state, _bd, _dir) = test_state();
    let sid = boss_in_b1(&state).await;
    mk_ar(&state, &sid, 1, "100").await;
    mk_ar(&state, &sid, 2, "200").await;

    // 2026-01 期末是 1-31，凭证日期 2026-01-10 → 挂账 21 天
    let d = get_json(
        &state,
        &sid,
        "/api/settle/open?account=112201&upto=202601&party=customer%3DC01",
    )
    .await;
    assert_eq!(d["total"], 2, "按往来对象过滤后仍是这两张");
    assert_eq!(d["rows"][0]["days"], 21, "天数要按期间末算：{d}");
    assert_eq!(
        d["rows"][0]["voucher_no"], "记-0001",
        "下钻要能直接报出凭证号"
    );

    // 换一个对象就是 0 笔——不能把别的客户的钱也算进来
    let other = get_json(
        &state,
        &sid,
        "/api/settle/open?account=112201&upto=202601&party=customer%3DC99",
    )
    .await;
    assert_eq!(other["total"], 0);
}
