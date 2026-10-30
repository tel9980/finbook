//! 期初收集表上传：整本四张纸一次导完，不平整批拒绝。
//!
//! 单开文件盯它，是因为它同时踩了三个容易分叉的地方：
//!
//! 1. **台账与科目余额** —— 往来期初（`arap_opening`）只是台账，资产负债表的应收数
//!    来自 `112201`。只导台账不导科目，就是「账龄表上有欠款、报表上一分没有」。
//! 2. **平衡** —— 期初不平，试算平衡开局就是不平的，后面每个月的报表都歪。
//! 3. **拒绝时要干净** —— 报错但已经写进半个账套，比不导更糟。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::{body_string, login, select_book, test_state};
use finweb::handlers;
use rust_xlsxwriter::{Format, Workbook};
use std::sync::Arc;
use tower::ServiceExt;

use finweb::state::WebState;

async fn ready() -> (Arc<WebState>, String, tempfile::TempDir) {
    let (state, _books, guard) = test_state();
    let (st, sid) = login(&state, "boss", "Admin!2026").await;
    assert_eq!(st, StatusCode::OK, "管理员应能登录");
    assert_eq!(
        select_book(&state, &sid, "b1").await,
        StatusCode::OK,
        "应能进入预置账套"
    );
    (state, sid, guard)
}

fn money_cell(s: &mut rust_xlsxwriter::Worksheet, r: u32, c: u16, v: f64, f: &Format) {
    s.write_number(r, c, v, f).ok();
}

/// 造一本《期初收集表》（结构照 `crates/findb/examples/opening_sheet.rs`）。
fn collection_book(
    ar: &[(&str, &str, f64)],
    ap: &[(&str, &str, f64)],
    bal: &[(&str, f64)],
    paid_in: f64,
) -> Vec<u8> {
    let mut wb = Workbook::new();
    let f = Format::new();

    let s = wb.add_worksheet();
    s.set_name("① 客户应收").unwrap();
    s.write_string(0, 0, "客户欠款（应收）· 截止 2026-10-31", &f)
        .unwrap();
    for (c, h) in ["客户编码", "客户名称", "欠款金额", "单据号（可空）", "备注"]
        .iter()
        .enumerate()
    {
        s.write_string(1, c as u16, *h, &f).unwrap();
    }
    for (i, (code, name, amt)) in ar.iter().enumerate() {
        let r = 2 + i as u32;
        s.write_string(r, 0, *code, &f).unwrap();
        s.write_string(r, 1, *name, &f).unwrap();
        money_cell(s, r, 2, *amt, &f);
    }
    s.write_string(2 + ar.len() as u32, 1, "应收合计", &f)
        .unwrap();

    let s = wb.add_worksheet();
    s.set_name("② 供应商应付").unwrap();
    s.write_string(0, 0, "欠供应商的钱（应付）· 截止 2026-10-31", &f)
        .unwrap();
    for (c, h) in [
        "供应商编码",
        "供应商名称",
        "品类",
        "欠款金额",
        "单据号（可空）",
        "备注",
    ]
    .iter()
    .enumerate()
    {
        s.write_string(1, c as u16, *h, &f).unwrap();
    }
    for (i, (code, name, amt)) in ap.iter().enumerate() {
        let r = 2 + i as u32;
        s.write_string(r, 0, *code, &f).unwrap();
        s.write_string(r, 1, *name, &f).unwrap();
        money_cell(s, r, 3, *amt, &f);
    }

    let s = wb.add_worksheet();
    s.set_name("③ 账户余额").unwrap();
    s.write_string(0, 0, "各账户余额 · 截止 2026-10-31", &f)
        .unwrap();
    for (c, h) in ["科目编码", "账户", "余额", "说明"].iter().enumerate() {
        s.write_string(1, c as u16, *h, &f).unwrap();
    }
    for (i, (code, amt)) in bal.iter().enumerate() {
        let r = 2 + i as u32;
        s.write_string(r, 0, *code, &f).unwrap();
        s.write_string(r, 1, "账户", &f).unwrap();
        money_cell(s, r, 2, *amt, &f);
    }

    // ④ 基本资料：只填实收资本，本年利润靠轧差（收集表本身就是这么设计的）
    let s = wb.add_worksheet();
    s.set_name("④ 基本资料").unwrap();
    s.write_string(0, 0, "基本资料与平衡检查", &f).unwrap();
    for (c, h) in ["项目", "金额", "说明"].iter().enumerate() {
        s.write_string(1, c as u16, *h, &f).unwrap();
    }
    s.write_string(2, 0, "实收资本 4001", &f).unwrap();
    s.write_number(2, 1, paid_in, &f).ok();
    s.write_string(3, 0, "余额合计", &f).unwrap();
    s.write_number(3, 1, 0.0, &f).ok();
    wb.save_to_buffer().unwrap()
}

fn upload(state: &Arc<WebState>, sid: &str, bytes: Vec<u8>, name: &str) -> Request<Body> {
    let boundary = "----finbookopening";
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(
        format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{n}\"\r\n\
             Content-Type: application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\r\n\r\n",
            b = boundary,
            n = name
        )
        .as_bytes(),
    );
    payload.extend_from_slice(&bytes);
    payload.extend_from_slice(format!("\r\n--{b}--\r\n", b = boundary).as_bytes());
    let _ = state;
    Request::builder()
        .method("POST")
        .uri("/api/books/opening-sheet")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::COOKIE, sid)
        .body(Body::from(payload))
        .unwrap()
}

#[tokio::test]
async fn collection_sheet_imports_ledger_and_opening_balances() {
    let (state, sid, _g) = ready().await;
    // 预置账套 b1 已有 C001/C002/S001 档案；期初：应收 70000、应付 30000、银行 20000、实收 50000
    let bytes = collection_book(
        &[("C001", "华达", 50000.0), ("C002", "鑫盛", 20000.0)],
        &[("S001", "宏源", 30000.0)],
        &[("100201", 20000.0)],
        50000.0,
    );
    let resp = handlers::router(state.clone())
        .oneshot(upload(&state, &sid, bytes, "期初收集表.xlsx"))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "导入应成功：{body}");

    let v: serde_json::Value = serde_json::from_str(&body).expect("应是 JSON");
    assert_eq!(v["ar"], 2, "应收两笔：{body}");
    assert_eq!(v["ap"], 1, "应付一笔：{body}");
    assert_eq!(v["debit_total"], "90000.00", "借方合计：{body}");
    assert_eq!(v["credit_total"], "90000.00", "贷方合计：{body}");

    // 科目期初真的写进去了（不只是台账）
    let resp = handlers::router(state.clone())
        .oneshot(common::authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let begin = body_string(resp).await;
    assert!(begin.contains("112201"), "112201 期初应落库：{begin}");
    assert!(begin.contains("70000"), "应收科目期初应是 70000：{begin}");
    assert!(begin.contains("220201"), "220201 期初应落库：{begin}");
}

#[tokio::test]
async fn unbalanced_sheet_is_refused_at_the_api_and_writes_nothing() {
    let (state, sid, _g) = ready().await;
    // 应收 70000 + 银行 20000 = 借 90000；应付 30000 + 实收 0 = 贷 30000 → 差 60000
    // 实收资本填 0，轧差块不会触发（paid_in 为 0），于是必须报不平。
    let bytes = collection_book(
        &[("C001", "华达", 50000.0), ("C002", "鑫盛", 20000.0)],
        &[("S001", "宏源", 30000.0)],
        &[("100201", 20000.0)],
        0.0,
    );
    let resp = handlers::router(state.clone())
        .oneshot(upload(&state, &sid, bytes, "期初收集表.xlsx"))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    // 实收资本 0 → 不平，接口必须拒绝并且把差额讲清楚
    assert_eq!(status, StatusCode::BAD_REQUEST, "不平应 400：{body}");
    assert!(
        body.contains("不平") || body.contains("差额"),
        "要说清哪里不平：{body}"
    );

    let resp = handlers::router(state.clone())
        .oneshot(common::authed_get("/api/begin", &sid))
        .await
        .unwrap();
    let begin = body_string(resp).await;
    assert!(
        !begin.contains("112201"),
        "拒绝时不能留下半个账套（112201 不该有期初）：{begin}"
    );
}

#[tokio::test]
async fn a_plain_spreadsheet_is_refused_with_a_readable_reason() {
    let (state, sid, _g) = ready().await;
    let mut wb = Workbook::new();
    let f = Format::new();
    let s = wb.add_worksheet();
    s.set_name("Sheet1").unwrap();
    s.write_string(0, 0, "随便什么", &f).unwrap();
    let bytes = wb.save_to_buffer().unwrap();

    let resp = handlers::router(state.clone())
        .oneshot(upload(&state, &sid, bytes, "随便.xlsx"))
        .await
        .unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "不是收集表应 400：{body}");
    assert!(
        body.contains("期初收集表"),
        "错误里要点名期初收集表：{body}"
    );
}
