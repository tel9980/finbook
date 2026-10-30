//! 导入导出闭环的 HTTP 级回归。
//!
//! `roundtrip.rs` 证明的是「导出的字节喂回 imports 能还原数据」——
//! 那是**文件格式**层面的证据。这里补的是另一层：**路由是否真的挂着、
//! 权限是否真的生效、附件头是否正确**。两层缺一层都会出现「代码写得对，
//! 用户点下去 404」。

mod common;
use common::*;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

use finweb::handlers;
use finweb::state::WebState;

const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

/// 拉一个 xlsx 导出端点，断言状态码 / MIME / zip 魔数，返回字节
async fn fetch_xlsx(state: &Arc<WebState>, sid: &str, uri: &str) -> Vec<u8> {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(uri, sid))
        .await
        .unwrap();
    let status = resp.status();
    let ctype = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let disp = resp
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    assert_eq!(status, StatusCode::OK, "{uri} 应 200：{ctype}");
    assert_eq!(ctype, XLSX_MIME, "{uri} 的 Content-Type 应是 xlsx");
    assert!(
        disp.contains("attachment"),
        "{uri} 应带 Content-Disposition（附件下载），实际「{disp}」"
    );
    // xlsx 就是 zip：PK\x03\x04。比 assert 非空更硬 —— 空 Vec 也能过非空断言。
    assert!(
        bytes.starts_with(b"PK\x03\x04"),
        "{uri} 返回的不是 zip/xlsx（前 8 字节 {:?}）",
        &bytes[..8.min(bytes.len())]
    );
    bytes
}

/// 附件名里的中文不能被静默丢掉
///
/// `HeaderValue::from_str` 遇到非 ASCII 直接失败。旧代码写的是
/// `if let Ok(v) = ... { insert }` —— 失败就**什么都不加**，
/// 浏览器于是把 xlsx 当成网页在标签页里打开，用户以为「导出没反应」。
#[test]
fn attachment_filename_survives_non_ascii() {
    let h = finweb::handlers::attachment_header("资产负债表_202610.xlsx")
        .expect("应能构造头部值")
        .to_str()
        .unwrap()
        .to_string();
    assert!(h.is_ascii(), "头部值必须能塞进 HeaderValue：{h}");
    // RFC 5987：非 ASCII 走 filename*=UTF-8''，百分号编码
    assert!(h.contains("filename*=UTF-8''"), "应带 filename*：{h}");
    assert!(h.contains('%'), "中文应被百分号编码：{h}");
    // ASCII 兜底名不能为空，否则老浏览器存出个没扩展名的文件
    assert!(h.contains(".xlsx"), "兜底名应保留扩展名：{h}");
}

#[tokio::test]
async fn aux_and_master_data_export_as_xlsx() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    // 先造点基础资料，避免导出的是空表（空表也能过，但测不出「有没有内容」）
    // 走的是「粘贴一批」那条真实入口（tab 分隔），不是直接写库 ——
    // 这样导出的内容经过一次真实录入路径，不是测试自己造的。
    let resp = handlers::router(state.clone())
        .oneshot(authed_post(
            "/api/aux-batch",
            &sid,
            serde_json::json!({
                "kind": "customer",
                "text": "C001\t杭州明远贸易有限公司\nC002\t宁波鑫盛五金厂\n"
            }),
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "粘贴一批客户档案应成功：{}",
        body_string(resp).await
    );

    for uri in [
        "/api/export/aux",
        "/api/export/aux?kind=customer",
        "/api/export/items",
        "/api/export/accounts",
        "/api/export/begin",
    ] {
        fetch_xlsx(&state, &sid, uri).await;
    }
}

#[tokio::test]
async fn four_statements_export_as_xlsx() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    for uri in [
        "/api/reports/trial-balance/xlsx",
        "/api/reports/balance-sheet/xlsx",
        "/api/reports/income-statement/xlsx",
        "/api/reports/cash-flow/xlsx",
        "/api/reports/equity/xlsx",
    ] {
        fetch_xlsx(&state, &sid, uri).await;
    }
}

/// 往来账龄也能导出成 Excel。
///
/// 账龄表以前只能截图：老板发给客户、存档给会计，截图既不能改也不能算。
/// 这条测试只证明「接口能出一份结构正确的 xlsx」，数字对不对由
/// `finweb` 的 `/api/settle/aging` 与 `findb::settle::aging` 的测试负责——
/// 两边走的是同一条取数路径。
#[tokio::test]
async fn settle_aging_export_as_xlsx() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;
    fetch_xlsx(&state, &sid, "/api/settle/aging/xlsx?account=1122").await;
    // 按年分档（与天数分档走不同的桶）也要能出
    fetch_xlsx(
        &state,
        &sid,
        "/api/settle/aging/xlsx?account=1122&scheme=year",
    )
    .await;

    // 应付也要能导：文件名与表头都得跟着科目走。
    // 只测 1122 的话，"写死 1122"这个错法照样绿灯 —— 而它会让会计把应付账龄
    // 当应收发给客户。
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/settle/aging/xlsx?account=2202", &sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let disp = resp
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(
        disp.contains("2202") || disp.contains("%E5%BA%94%E4%BB%98"),
        "导出文件名要带上科目/往来类型，否则两个文件分不清：{disp}"
    );

    // 导出是数据外带，必须留痕：谁把多少家的往来余额带走了，事后查得到。
    let logs = handlers::router(state.clone())
        .oneshot(authed_get("/api/logs?limit=20", &sid))
        .await
        .unwrap();
    assert_eq!(logs.status(), StatusCode::OK);
    let text = String::from_utf8_lossy(
        &axum::body::to_bytes(logs.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .to_string();
    assert!(
        text.contains("导出往来账龄"),
        "账龄导出必须写操作日志（应收会计也有导出权了）：{text}"
    );
}

/// 未登录一律 401，不能因为「反正是只读导出」就放行
#[tokio::test]
async fn exports_require_login() {
    let (state, _dir, _tmp) = test_state();
    for uri in ["/api/export/aux", "/api/reports/balance-sheet/xlsx"] {
        let resp = handlers::router(state.clone())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} 未登录应 401"
        );
    }
}

/// 上传一个真实账套：能注册、能出现在列表、能打开取数
#[tokio::test]
async fn upload_fbk_registers_a_usable_book() {
    let (state, books_dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    // 造一个「外部」账套：跟测试栈没关系，就在临时目录里
    let external = books_dir.join("desktop_export.fbk");
    let opts = fincore::BookOptions {
        start_period: fincore::Period::new(2026, 1).unwrap(),
        company: "外部搬来的厂".into(),
        enable_audit: false,
        require_cashier: false,
        ..Default::default()
    };
    {
        let db = findb::Db::create_no_admin(&external, &opts).expect("建外部账套失败");
        // 关键：桌面版建的账套里，同名 admin 那行是「首次登录必须改密」状态。
        // Web 端登录走平台口令（这边的平台 boss 是 not-must-change），两边一比
        // 就卡在账套层的改密闸门上 —— 每个账套内接口 401，前端就是「点账套又
        // 弹回选择页」。所以这一行必须照真实形态造出来，否则这条测试守不住它。
        findb::users::create_admin(&db, "boss", "Desktop!2026", "管理员")
            .expect("造桌面版账套内的 admin 失败");
        let mut u = findb::users::get(&db, "boss").unwrap().unwrap();
        u.must_change_pwd = true;
        findb::users::update(&db, &u).unwrap();
    }
    let bytes = std::fs::read(&external).expect("读外部账套失败");
    std::fs::remove_file(&external).unwrap();

    let boundary = "----finbookimport";
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(
        format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\n搬来的厂\r\n",
            b = boundary
        )
        .as_bytes(),
    );
    payload.extend_from_slice(
        format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"desktop_export.fbk\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n",
            b = boundary
        )
        .as_bytes(),
    );
    payload.extend_from_slice(&bytes);
    payload.extend_from_slice(format!("\r\n--{b}--\r\n", b = boundary).as_bytes());

    let req = Request::builder()
        .method("POST")
        .uri("/api/books/import")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::COOKIE, &sid)
        .body(Body::from(payload))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert_eq!(status, StatusCode::OK, "导入账套应成功：{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = v["key"].as_str().expect("应返回账套 key").to_string();
    assert_eq!(
        v["company"],
        serde_json::json!("外部搬来的厂"),
        "公司名应从账套里读出来"
    );

    // 落盘了、没留下临时文件
    assert!(
        books_dir.join(format!("{key}.fbk")).is_file(),
        "账套文件应落盘"
    );
    let leftovers: Vec<String> = std::fs::read_dir(&books_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(".import_"))
        .collect();
    assert!(leftovers.is_empty(), "不该留下临时文件：{leftovers:?}");

    // 出现在账套列表里
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/books", &sid))
        .await
        .unwrap();
    let list = body_string(resp).await;
    assert!(list.contains(&key), "新账套应出现在列表里：{list}");

    // 真的能进去取数（不只是文件躺在磁盘上）
    assert_eq!(
        select_book(&state, &sid, &key).await,
        StatusCode::OK,
        "导入的账套应能选中进入"
    );
    // `/api/me` 是前端进账套后的第一枪（`enterBook`：select → /me → 建界面）。
    // 它 401 就等于「导入成功但进不去」，而这正是用户报的那个问题。
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &sid))
        .await
        .unwrap();
    let status = resp.status();
    let me = body_string(resp).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "进账套后 /api/me 必须能过（账套内那行的改密标记不该拦住平台已认证的身份）：{me}"
    );
    assert!(me.contains("boss"), "应返回账套内身份：{me}");

    // 导入后账套内那一行应与平台身份对齐。这不是内部整洁问题：用户搬完家
    // 回到桌面端打开同一个账套，若那行还是 `must_change_pwd = 1`，会先被要求
    // 「首次登录必须改密」—— 刚迁移完就给一道密码闸门，很容易被当成迁移出错。
    {
        let db = findb::Db::open(&books_dir.join(format!("{key}.fbk"))).expect("重新打开账套");
        let u = findb::users::get(&db, "boss").unwrap().unwrap();
        assert!(
            !u.must_change_pwd,
            "导入后账套内同名行应与平台身份对齐（改密标记），否则桌面端会莫名要求改密"
        );
    }

    // 再验一层：万一账套里那行的改密标记事后又被置回 true（桌面端重置口令、
    // 或者这个账套是修复之前导入的），也不该把平台已认证的身份拦在门外 ——
    // 平台层 `RealmUser` 已经按真身拦过一次了，账套这份影子不参与鉴权。
    {
        let db = findb::Db::open(&books_dir.join(format!("{key}.fbk"))).expect("重新打开账套");
        let mut u = findb::users::get(&db, "boss").unwrap().unwrap();
        u.must_change_pwd = true;
        findb::users::update(&db, &u).unwrap();
    }
    let resp = handlers::router(state.clone())
        .oneshot(authed_get("/api/me", &sid))
        .await
        .unwrap();
    let status = resp.status();
    let me = body_string(resp).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "账套层的改密标记只是平台标记的影子，不该单独拦人：{me}"
    );

    fetch_xlsx(&state, &sid, "/api/reports/balance-sheet/xlsx").await;
}

/// 传个普通 SQLite / 随便什么文件上来，不能被注册成账套
#[tokio::test]
async fn upload_rejects_non_finbook_file() {
    let (state, _books_dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    // 一个合法 SQLite 库，但没有 FinBook 的表
    let junk = _tmp.path().join("junk.fbk");
    {
        let c = rusqlite::Connection::open(&junk).unwrap();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1);")
            .unwrap();
    }
    let bytes = std::fs::read(&junk).unwrap();

    let boundary = "----finbookjunk";
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(
        format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"junk.fbk\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n",
            b = boundary
        )
        .as_bytes(),
    );
    payload.extend_from_slice(&bytes);
    payload.extend_from_slice(format!("\r\n--{b}--\r\n", b = boundary).as_bytes());

    let req = Request::builder()
        .method("POST")
        .uri("/api/books/import")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::COOKIE, &sid)
        .body(Body::from(payload))
        .unwrap();
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let body = body_string(resp).await;
    assert!(
        status.is_client_error(),
        "不是 FinBook 账套应被拒绝，实际 {status}：{body}"
    );
    assert!(
        body.contains("账套"),
        "拒绝理由要说人话（提到「账套」）：{body}"
    );
}

/// 大响应要 gzip，xlsx 不要 gzip。
///
/// 前端 `app.js` 是 780KB、科目表 JSON 几十 KB，手机上走公网时这一层直接决定
/// 「打开要几秒」。而 xlsx 本身是 zip，再压一遍只是白烧 CPU、还会把
/// `Content-Length` 变成未知（进度条没了）。
///
/// 两个方向都要守：**该压的压了**、**不该压的没压**。只断言前者的话，
/// 哪天有人把整条链路改成「全部压缩」，这个测试照样绿。
#[tokio::test]
async fn big_responses_are_gzipped_but_xlsx_is_not() {
    let (state, _dir, _tmp) = test_state();
    let sid = boss_in_b1(&state).await;

    // ① 科目表 JSON（约 199 条）→ 应 gzip
    let mut req = authed_get("/api/accounts", &sid);
    req.headers_mut()
        .insert(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_ENCODING)
            .map(|v| v.to_str().unwrap()),
        Some("gzip"),
        "大 JSON 响应应当 gzip"
    );
    assert_eq!(
        resp.headers()
            .get(header::VARY)
            .map(|v| v.to_str().unwrap()),
        Some("accept-encoding"),
        "压缩层要声明 Vary: accept-encoding，否则缓存会把压缩体发给不支持的客户端"
    );
    let _ = body_string(resp).await;

    // ② xlsx 导出 → 不压
    let mut req = authed_get("/api/export/accounts", &sid);
    req.headers_mut()
        .insert(header::ACCEPT_ENCODING, "gzip".parse().unwrap());
    let resp = handlers::router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get(header::CONTENT_ENCODING).is_none(),
        "xlsx 本身已经是 zip 了，不应再压一层"
    );
    let _ = body_string(resp).await;
}
