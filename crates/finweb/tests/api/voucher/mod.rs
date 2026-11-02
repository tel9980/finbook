//! 凭证域的 Web API 端到端用例（原 `tests/api/voucher.rs`，5,128 行 / 42 个用例）。
//!
//! 为什么拆：一个文件 5 千行，改一处要滚十几屏；出问题看 backtrace 只知道行号，
//! 不知道是哪个域。按域分册后，红的是哪一册，问题就在哪一册。
//!
//! 分册：`assets` 固定资产与总账对账 / `sales` 销售成本与委外 / `voucher_core` 凭证本身 /
//! `audit_perm` 审核闸门与数据范围 / `period` 期间与年末结转 / `master_data` 主数据与跑单。

use super::*;

mod assets;
mod audit_perm;
mod master_data;
mod period;
mod sales;
mod voucher_core;

/// GET 一个端点并解析 JSON（失败即报错，带响应体）。
async fn get_json(state: &Arc<WebState>, sid: &str, path: &str) -> serde_json::Value {
    let resp = handlers::router(state.clone())
        .oneshot(authed_get(path, sid))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET {path} 应 200");
    serde_json::from_str(&body_string(resp).await).unwrap()
}
