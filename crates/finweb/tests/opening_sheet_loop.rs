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
    fill_core(src, &[], "87000")
}

/// 同上，但可以额外塞几行应收、并改实收资本（用来造「编码笔误」这类不平也要平的场景）。
fn fill_core(src: &[u8], extra_ar: &[(&str, &str, &str)], paid_in: &str) -> Vec<u8> {
    let sheets = findb::imports::read_xlsx_sheets(src).expect("模板应能被解析回来");
    let mut wb = Workbook::new();
    let f = Format::new();
    let mut filled = 0usize;

    // 往某张纸里塞一行：已有这一行就填金额，没有就**追加**。
    //
    // 为什么必须能追加：模板的 ①② 两张纸是按账套里的客户/供应商预填的，账套还没有
    // 档案时它们只有表头 —— 只在已有行上填数，测试就变成「什么都没填也全绿」。
    let set_row = |rows: &mut Vec<Vec<String>>, key: &str, mk: &dyn Fn() -> Vec<String>| {
        if let Some(r) = rows.iter_mut().find(|r| r.first().map(|c| c.as_str()) == Some(key)) {
            let fresh = mk();
            for (i, v) in fresh.iter().enumerate() {
                if i < r.len() {
                    r[i] = v.clone();
                } else {
                    r.push(v.clone());
                }
            }
        } else {
            rows.push(mk());
        }
    };

    for (name, rows) in &sheets {
        let mut rows = rows.clone();
        if name.contains("客户应收") {
            set_row(&mut rows, "C001", &|| {
                vec!["C001".into(), "华达五金".into(), "50000".into(), "XS-001".into(), String::new()]
            });
            set_row(&mut rows, "C002", &|| {
                vec!["C002".into(), "鑫盛电器".into(), "45000".into(), String::new(), String::new()]
            });
            for (code, name, amt) in extra_ar {
                set_row(&mut rows, code, &|| {
                    vec![
                        (*code).to_string(),
                        (*name).to_string(),
                        (*amt).to_string(),
                        String::new(),
                        String::new(),
                    ]
                });
            }
            filled += 1;
        }
        if name.contains("供应商应付") {
            set_row(&mut rows, "S001", &|| {
                vec!["S001".into(), "宏源氧化".into(), "28000".into(), "CG-001".into(), String::new()]
            });
            filled += 1;
        }
        if name.contains("账户余额") {
            set_row(&mut rows, "1001", &|| {
                vec!["1001".into(), "库存现金".into(), "20000".into()]
            });
            filled += 1;
        }
        // 借方 = 现金 20000 + 应收 95000；贷方 = 应付 28000 + 实收资本 87000，两边都是 115000。
        if name.contains("基本资料") {
            set_row(&mut rows, "实收资本", &|| {
                vec!["实收资本".into(), paid_in.to_string()]
            });
            filled += 1;
        }

        let ws = wb.add_worksheet();
        ws.set_name(name.as_str()).unwrap();
        for (r, row) in rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if cell.is_empty() {
                    continue;
                }
                // 金额列写成数字（Excel 里手填也是这样），其余当文本
                match cell.parse::<f64>() {
                    Ok(n) if c > 0 => {
                        ws.write_number(r as u32, c as u16, n, &f).unwrap();
                    }
                    _ => {
                        ws.write_string(r as u32, c as u16, cell, &f).unwrap();
                    }
                }
            }
        }
    }
    assert_eq!(filled, 4, "四张纸都该被填上");
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

// ── 导入之后的对账视图 ───────────────────────────────────────────────
//
// 上面三条证明「表能导进来」；下面三条证明「导进来之后账上真的变成了表里那样」。
// 两件事的失败方式完全不同：前者会报错，后者**一声不响**。

/// POST /api/begin 保存（`{rows, delete_ids}` 包起来的那种）。
/// 建好往来档案。
///
/// 为什么必须有：期初收集表的 ①② 两张纸是**按当前账套的客户/供应商清单预填**的 ——
/// 账套里一个档案都没有时，那两张纸上不会出现编码，`opening_book` 的
/// 「往来单位编码都在档案里」那条校验立刻就会红。
///
/// 走的是前端真实的两步：POST 只**解析**（不写库），PUT 才带 `rows` 落库。
/// 直接用 POST 会「接口 200、档案一条没有」——这一版的测试就这么被骗过一次。
async fn seed_aux(state: &Arc<finweb::state::WebState>, sid: &str) {
    for (kind, text) in [
        ("customer", "C001\t华达五金\nC002\t鑫盛电器\n"),
        ("supplier", "S001\t宏源氧化\n"),
    ] {
        let resp = finweb::handlers::router(state.clone())
            .oneshot(authed_post(
                "/api/aux-batch",
                sid,
                serde_json::json!({ "kind": kind, "text": text }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "解析档案应当成功（{kind}）");
        let body: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        let rows = body["rows"].clone();
        assert!(
            rows.as_array().map(|a| !a.is_empty()).unwrap_or(false),
            "解析结果里应当有行：{body}"
        );

        let resp = finweb::handlers::router(state.clone())
            .oneshot(authed_put(
                "/api/aux-batch",
                sid,
                serde_json::json!({ "kind": kind, "rows": rows }),
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "导入档案应当成功（{kind}）：{}",
            body_string(resp).await
        );
    }
}

async fn post_begin(
    state: &Arc<finweb::state::WebState>,
    sid: &str,
    rows: serde_json::Value,
    delete_ids: Vec<i64>,
) -> (StatusCode, String) {
    let body = serde_json::json!({ "rows": rows, "delete_ids": delete_ids });
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_post("/api/begin", sid, body))
        .await
        .unwrap();
    let code = resp.status();
    (code, body_string(resp).await)
}

async fn get_json(state: &Arc<finweb::state::WebState>, sid: &str, uri: &str) -> serde_json::Value {
    let resp = finweb::handlers::router(state.clone())
        .oneshot(authed_get(uri, sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{uri} 应返回 200");
    let raw = axum::body::to_bytes(resp.into_body(), 4 << 20)
        .await
        .unwrap()
        .to_vec();
    serde_json::from_slice(&raw).unwrap_or_else(|e| {
        panic!(
            "{uri} 不是 JSON：{e}\n{}",
            String::from_utf8_lossy(&raw)
        )
    })
}

/// 正常导入之后，对账应当一片绿：四张纸都对得上、借贷相等、资产=负债+权益。
#[tokio::test]
async fn reconcile_after_import_is_all_green() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_aux(&state, &sid).await;
    let tpl = get_xlsx(&state, &sid, "/api/books/opening-sheet/template").await;
    let filled = fill_and_repack(&tpl);
    let (code, body) = post_sheet(&state, &sid, "/api/books/opening-sheet", &filled).await;
    assert_eq!(code, StatusCode::OK, "导入应成功：{body}");

    let r = get_json(&state, &sid, "/api/books/opening-sheet/reconcile").await;
    assert_eq!(r["ok"], serde_json::json!(true), "对账应当全绿：{r}");
    assert_eq!(r["empty"], serde_json::json!(false));
    assert_eq!(r["balanced"], serde_json::json!(true));
    let sheets = r["sheets"].as_array().expect("sheets");
    assert_eq!(sheets.len(), 4, "四张纸都要在：{r}");
    assert_eq!(sheets[0]["label"], serde_json::json!("① 客户应收"));
    let checks = r["checks"].as_array().expect("checks");
    assert!(checks.len() >= 5, "至少五条交叉校验：{r}");
    for c in checks {
        assert_eq!(c["ok"], serde_json::json!(true), "这一条没过：{c}");
    }
    assert_eq!(r["issues"].as_array().unwrap().len(), 0, "{r}");
}

/// 把 112201 的期初删掉（模拟「只导了台账没导科目」）：对账必须当场指出是哪两处对不上。
#[tokio::test]
async fn reconcile_catches_ledger_without_booked_accounts() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_aux(&state, &sid).await;
    let tpl = get_xlsx(&state, &sid, "/api/books/opening-sheet/template").await;
    let filled = fill_and_repack(&tpl);
    let (code0, body0) = post_sheet(&state, &sid, "/api/books/opening-sheet", &filled).await;
    assert_eq!(code0, StatusCode::OK, "导入应成功：{body0}");

    // 找到 112201 那行的 id 并删掉
    let begins = get_json(&state, &sid, "/api/begin").await;
    let ar_id = begins
        .as_array()
        .expect("begin 数组")
        .iter()
        .find(|r| r["account_code"] == serde_json::json!("112201"))
        .and_then(|r| r["id"].as_i64())
        .unwrap_or_else(|| panic!("导入后应当有 112201 的期初行，实际 begins={begins}"));
    let (code, body) = post_begin(&state, &sid, serde_json::json!([]), vec![ar_id]).await;
    assert_eq!(code, StatusCode::OK, "删期初应成功：{body}");

    let r = get_json(&state, &sid, "/api/books/opening-sheet/reconcile").await;
    assert_eq!(r["ok"], serde_json::json!(false), "删掉科目期初后不该全绿：{r}");
    let issues = serde_json::to_string(&r["issues"]).unwrap();
    assert!(issues.contains("应收科目余额"), "要点名是哪条校验：{r}");
    // 台账那一边仍然是 95000 —— 这正是「两处都对、合起来不对」的样子
    let sheets = r["sheets"].as_array().unwrap();
    assert_eq!(sheets[0]["rows"], serde_json::json!(2));
}

/// 台账上写了个档案里没有的客户编码：对账要**点名是哪一个**。
///
/// 导入时只校验科目存在，不会拦往来编码 —— 编码打错一位（C001 写成 C999）时
/// 期初照样能导进去、借贷照样平，只有这条交叉校验抓得住。
#[tokio::test]
async fn reconcile_flags_a_party_code_missing_from_the_archive() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    seed_aux(&state, &sid).await;

    let tpl = get_xlsx(&state, &sid, "/api/books/opening-sheet/template").await;
    // 多一行 C999（档案里没有），实收资本跟着加 1000 让借贷仍然相等
    let filled = fill_core(&tpl, &[("C999", "查无此户", "1000")], "88000");
    let (code, body) = post_sheet(&state, &sid, "/api/books/opening-sheet", &filled).await;
    assert_eq!(code, StatusCode::OK, "导入应成功：{body}");

    let r = get_json(&state, &sid, "/api/books/opening-sheet/reconcile").await;
    assert_eq!(r["ok"], serde_json::json!(false), "编码对不上不该全绿：{r}");
    let c = r["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"].as_str().unwrap_or("").contains("编码都在档案里"))
        .expect("要有编码校验那条");
    assert_eq!(c["ok"], serde_json::json!(false), "{r}");
    assert!(
        c["detail"].as_str().unwrap_or("").contains("C999"),
        "要点名是哪个编码：{r}"
    );
}

/// 本年利润是**借方**（亏损）时，权益要变小而不是变大。
///
/// 这是本模块自己踩过的坑：负债/权益一律按「贷方正」写，遇到借方余额的 4103
/// 会把亏损当成收益加进权益，于是「资产 = 负债 + 权益」这条校验**照样通过**，
/// 而权益数是错的。所以这里直接摆一个亏损场景钉住它。
#[tokio::test]
async fn reconcile_treats_debit_profit_as_a_loss() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    // 现金 20000（借）/ 实收资本 50000（贷）/ 本年利润 30000（借 = 亏损）
    // 注意 `/api/begin` 的入参是**界面形状**（方向 `dir` + 正数金额 `yb`），
    // 不是数据库形状（带符号的 `year_begin`）—— 两者只在前端拆合的这一步不同。
    let rows = serde_json::json!([
        { "account_code": "1001", "dir": "debit", "yb": "20000.00" },
        { "account_code": "4001", "dir": "credit", "yb": "50000.00" },
        { "account_code": "4103", "dir": "debit", "yb": "30000.00" },
    ]);
    let (code, body) = post_begin(&state, &sid, rows, vec![]).await;
    assert_eq!(code, StatusCode::OK, "保存期初应成功：{body}");

    let r = get_json(&state, &sid, "/api/books/opening-sheet/reconcile").await;
    assert_eq!(r["asset_total"], serde_json::json!("20000.00"), "{r}");
    // 50000 - 30000 = 20000：权益被亏损冲减
    assert_eq!(r["equity_total"], serde_json::json!("20000.00"), "亏损要从权益里减掉：{r}");
    let balance = r["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"].as_str().unwrap_or("").contains("资产 = 负债 + 权益"))
        .expect("要有资产负债结构那条校验");
    assert_eq!(balance["ok"], serde_json::json!(true), "{r}");
}
