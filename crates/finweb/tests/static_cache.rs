//! 静态资源的缓存策略：带版本号的可长缓存，首页必须回源。
//!
//! 为什么值得单开一个用例：这两条头一旦写错，症状是「升级后用户还在跑旧脚本」——
//! 前端与新接口对不上，报的却是一堆莫名其妙的错，而服务器日志里一切正常。
//! 反过来写太宽松（首页也可长缓存）同样致命，且**同样**只在升级那一刻暴露。

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

use common::test_state_with_static;
use finweb::handlers;

/// 造一个最小静态目录：首页（含 `{{V}}` 占位符）+ 一个脚本资产
fn make_static_dir() -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("建临时静态目录失败");
    std::fs::write(
        d.path().join("index.html"),
        "<html><script src=\"/app.js?v={{V}}\"></script></html>",
    )
    .unwrap();
    std::fs::write(d.path().join("app.js"), "// body\n").unwrap();
    d
}

fn cache_control(h: &header::HeaderMap) -> String {
    h.get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

async fn get(state: std::sync::Arc<finweb::state::WebState>, uri: &str) -> axum::response::Response {
    handlers::router(state)
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn versioned_asset_is_cacheable_for_a_year() {
    let d = make_static_dir();
    let (state, _books, _guard) = test_state_with_static(d.path().to_path_buf());
    let resp = get(state, "/app.js?v=abc123").await;
    assert_eq!(resp.status(), StatusCode::OK, "带 ?v= 的资产应能取到");
    let cc = cache_control(resp.headers());
    assert!(
        cc.contains("immutable"),
        "带 ?v= 的资产应可长缓存（内容一变 URL 就变），实际：{cc:?}"
    );
}

#[tokio::test]
async fn unversioned_asset_must_revalidate() {
    let d = make_static_dir();
    let (state, _books, _guard) = test_state_with_static(d.path().to_path_buf());
    let resp = get(state, "/app.js").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let cc = cache_control(resp.headers());
    assert!(
        cc.contains("no-cache") && !cc.contains("immutable"),
        "没带版本号的直达请求必须回源验证，否则升级后用户拿旧脚本打新接口，实际：{cc:?}"
    );
}

#[tokio::test]
async fn index_html_is_never_cached() {
    let d = make_static_dir();
    let (state, _books, _guard) = test_state_with_static(d.path().to_path_buf());
    let resp = get(state, "/").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        cache_control(resp.headers()).contains("no-cache"),
        "首页带 {{{{V}}}} 替换，缓存住就等于把旧资源版本号钉在用户浏览器里"
    );
    let body = axum::body::to_bytes(resp.into_body(), 65536).await.unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(!html.contains("{{V}}"), "占位符必须被替换掉，实际：{html}");
}
