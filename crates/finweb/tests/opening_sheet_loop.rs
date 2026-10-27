//! 期初收集表的**完整闭环**：下载模板 → 填金额 → 先预检 → 再导入。
//!
//! 这条链以前只测得到「喂一份手搓的 xlsx 能不能导进去」（`findb/tests/opening_book.rs`）。
//! 但工厂真正走的路是「从系统里下载模板，填完再传回来」——中间只要有一处列名、
//! 表名、纸的顺序对不上，用户拿到的就是一份**导不回去的模板**，而他自己看不出为什么。
//!
//! 所以这里从 HTTP 下载模板开始，把导出的字节真的填上金额再传回导入接口。

mod common;

use axum::body::Body;
use axum::http::{header, StatusCode};
use common::*;
use rust_xlsxwriter::{Format, Workbook};
use std::sync::Arc;
use tower::ServiceExt;

/// multipart 上传一个 xlsx 到指定接口。
async fn post_sheet(
    state: &Arc<finweb::state::WebState>,
    sid: &str,
    uri: &str,
    bytes: &[u8],
) -> (StatusCode, String) {
    let b = "----finbookopening";
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(
        format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"期初收集表.xlsx\"\r\n\
             Content-Type: application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\r\n\r\n"
        )
        .as_bytes(),
    );
    payload.extend_from_slice(bytes);
    payload.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={b}"),
        )
        .header(header::COOKIE, sid)
        .body(Body::from(payload))
        .unwrap();
    let resp = finweb::handlers::router(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let code = resp.status();
    (code, body_string(resp).await)
}

async fn get_xlsx(state: &Arc<finweb::state::WebState>, sid: &str, uri: &str) -> Vec<u8> {
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_get(uri, sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "下载模板应成功");
    axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec()
}

/// 把导出的模板读回来 → 改两个数字 → 重新打包成 xlsx。
///
/// 这正是用户在 Excel 里做的事：不改结构，只填金额。
fn fill_and_repack(src: &[u8]) -> Vec<u8> {
    let sheets = findb::imports::read_xlsx_sheets(src).expect("模板应能被解析回来");
    let mut wb = Workbook::new();
    let f = Format::new();
    let mut filled = false;
    for (name, rows) in &sheets {
        let ws = wb.add_worksheet();
        ws.set_name(name.as_str()).unwrap();
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if cell.is_empty() {
                    continue;
                }
                ws.write_string(r as u32, c as u16, cell, &f).unwrap();
            }
        }
        // ③ 账户余额：给「1001 库存现金」填 50000
        if name.contains("账户余额") {
            for (r, row) in rows.iter().enumerate() {
                if row.first().map(|s| s.as_str()) == Some("1001") {
                    ws.write_number(r as u32, 2, 50000.0, &f).unwrap();
                    filled = true;
                }
            }
        }
        // ④ 基本资料：实收资本 50000（与 1001 对冲，正好平）
        if name.contains("基本资料") {
            for (r, row) in rows.iter().enumerate() {
                if row.first().map(|s| s.contains("实收资本")).unwrap_or(false) {
                    ws.write_number(r as u32, 1, 50000.0, &f).unwrap();
                    filled = true;
                }
            }
        }
    }
    assert!(filled, "模板里应当能定位到 1001 与「实收资本」两行");
    wb.save_to_buffer().unwrap()
}

/// 模板必须带着当前账套的科目清单（否则工厂还得手抄编码）。
#[tokio::test]
async fn template_carries_the_current_books_account_codes() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let bytes = get_xlsx(&state, &sid, "/api/books/opening-sheet/template").await;

    let sheets = findb::imports::read_xlsx_sheets(&bytes).expect("应能被我们的解析器读回来");
    let names: Vec<&str> = sheets.iter().map(|(n, _)| n.as_str()).collect();
    for want in ["客户应收", "供应商应付", "账户余额", "基本资料"] {
        assert!(
            names.iter().any(|n| n.contains(want)),
            "模板里缺「{want}」那张纸，实际有 {names:?}"
        );
    }
    let bal = sheets
        .iter()
        .find(|(n, _)| n.contains("账户余额"))
        .expect("账户余额纸");
    assert!(
        bal.1.iter().any(|r| r.first().map(|s| s.as_str()) == Some("1001")),
        "账户余额纸里应当预填 1001 库存现金，实际前几行：{:?}",
        &bal.1.iter().take(5).collect::<Vec<_>>()
    );
    // 1122/2202 的余额不能出现在模板里：它们由往来两张纸生成，填了就是双计
    assert!(
        !bal.1.iter().any(|r| r
            .first()
            .map(|s| s.starts_with("1122") || s.starts_with("2202"))
            .unwrap_or(false)),
        "账户余额纸不该列 1122/2202"
    );
}

/// 预检不落库：同一份表先预检、再导入，期初科目数必须从 0 变成有数。
#[tokio::test]
async fn preview_does_not_write_then_import_does() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let tpl = get_xlsx(&state, &sid, "/api/books/opening-sheet/template").await;
    let filled = fill_and_repack(&tpl);

    let (code, body) = post_sheet(&state, &sid, "/api/books/opening-sheet/preview", &filled).await;
    assert_eq!(code, StatusCode::OK, "预检应通过：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["dry_run"], serde_json::json!(true));
    assert_eq!(v["debit_total"], v["credit_total"], "预检应算出平衡：{body}");

    // 预检之后库里必须还是空的
    let begin = String::from_utf8(
        axum::body::to_bytes(
            finweb::handlers::router(state.clone())
                .oneshot(authed_get("/api/begin", &sid))
                .await
                .unwrap()
                .into_body(),
            1 << 20,
        )
        .await
        .unwrap()
        .to_vec(),
    )
    .unwrap();
    assert_eq!(begin.trim(), "[]", "预检不许落库，实际 /api/begin = {begin}");

    // 正式导入
    let (code, body) = post_sheet(&state, &sid, "/api/books/opening-sheet", &filled).await;
    assert_eq!(code, StatusCode::OK, "导入应成功：{body}");
    let begin = String::from_utf8(
        axum::body::to_bytes(
            finweb::handlers::router(state.clone())
                .oneshot(authed_get("/api/begin", &sid))
                .await
                .unwrap()
                .into_body(),
            1 << 20,
        )
        .await
        .unwrap()
        .to_vec(),
    )
    .unwrap();
    assert!(begin.contains("1001"), "导入后科目期初应有 1001：{begin}");
}

/// 不平衡时预检也要明确报错（而不是「通过」），否则预检就没有意义。
#[tokio::test]
async fn preview_rejects_an_unbalanced_sheet() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    let tpl = get_xlsx(&state, &sid, "/api/books/opening-sheet/template").await;
    // 只填资产、不填权益 → 必然不平
    let sheets = findb::imports::read_xlsx_sheets(&tpl).unwrap();
    let mut wb = Workbook::new();
    let f = Format::new();
    for (name, rows) in &sheets {
        let ws = wb.add_worksheet();
        ws.set_name(name.as_str()).unwrap();
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if !cell.is_empty() {
                    ws.write_string(r as u32, c as u16, cell, &f).unwrap();
                }
            }
        }
        if name.contains("账户余额") {
            for (r, row) in rows.iter().enumerate() {
                if row.first().map(|s| s.as_str()) == Some("1001") {
                    ws.write_number(r as u32, 2, 50000.0, &f).unwrap();
                }
            }
        }
    }
    let bytes = wb.save_to_buffer().unwrap();
    let (code, body) = post_sheet(&state, &sid, "/api/books/opening-sheet/preview", &bytes).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "不平应当 400，实际 {code} {body}");
    assert!(body.contains("不平"), "错误里要说清是不平：{body}");
}
