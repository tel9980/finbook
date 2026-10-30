//! HTTP 请求处理函数

pub(crate) use std::collections::HashMap;
pub(crate) use std::fs::File;
pub(crate) use std::io;
pub(crate) use std::sync::Arc;

pub(crate) use axum::body::Body;
pub(crate) use axum::extract::{FromRequestParts, Path, Query, State};
pub(crate) use axum::http::{header, HeaderMap, StatusCode};
pub(crate) use axum::response::{IntoResponse, Response};
pub(crate) use axum::routing::{delete, get, post, put};
pub(crate) use axum::{Json, Router};
pub(crate) use chrono::{Datelike, NaiveDate};
pub(crate) use fincore::user::Perm;
pub(crate) use fincore::{
    Account, AuxEntity, AuxKind, AuxMask, AuxQuery, AuxRef, BookOptions, Direction, Entry, Money,
    Period, Role, User, Voucher, VoucherSource, VoucherStatus,
};
pub(crate) use findb::accounts;
pub(crate) use findb::advanced;
pub(crate) use findb::balances::{self, BalanceQuery, BalanceSnapshot, BeginRow, LedgerQuery};
pub(crate) use findb::periods;
pub(crate) use zip::{CompressionMethod, ZipWriter};
pub(crate) use zip::write::{FileOptions, FileOptionExtension};
pub(crate) use findb::security;
pub(crate) use findb::users;
pub(crate) use findb::vouchers::{self, VoucherQuery};
pub(crate) use findb::Db;
pub(crate) use findb::{auxs, business, template};
pub(crate) use serde::Deserialize;
pub(crate) use serde_json::json;
pub(crate) use tracing::error;

pub(crate) use crate::dto::*;
pub(crate) use crate::realm::{RealmBook, RealmDb};
pub(crate) use tower_http::compression::predicate::{NotForContentType, Predicate, SizeAbove};
pub(crate) use tower_http::compression::CompressionLayer;
pub(crate) use crate::state::{
    clear_cookie_header, parse_money_checked, parse_period, period_param, period_range_param,
    period_to_str, AppError, CurrentUser, RealmUser, WebState,
};


// ---- 按域拆出去的子模块（v1.8.57）----
pub(crate) mod assets;
pub(crate) mod auth;
pub(crate) mod books;
pub(crate) mod export;
pub(crate) mod funds;
pub(crate) mod hr;
pub(crate) mod inventory;
pub(crate) mod misc;
pub(crate) mod platform;
pub(crate) mod procure;
pub(crate) mod prod;
pub(crate) mod reports;
pub(crate) mod sales;
pub(crate) mod settle;
pub(crate) mod voucher;

use self::assets::*;
use self::auth::*;
use self::books::*;
use self::export::*;
use self::funds::*;
use self::hr::*;
use self::inventory::*;
use self::misc::*;
use self::platform::*;
use self::procure::*;
use self::prod::*;
use self::reports::*;
use self::sales::*;
use self::settle::*;
use self::voucher::*;

const SESSION_SECS: i64 = 60 * 60 * 24 * 7;
/// 单账号最多可自建账套数（防无限建账占满磁盘）
const MAX_BOOKS_PER_USER: i64 = 10;

/// M-9 会话门禁：在路由匹配前把未认证的 /api/* 统一拦成 401——
/// 否则未登录探测者可用 401（真实接口）/ 404（不存在）/ 405（方法未注册）
/// 的差异枚举出全部接口清单。公开接口放行；只做令牌校验且文案与
/// RealmUser/CurrentUser 提取器共用 session_of 逐字一致；完整身份对账
/// 仍由各接口自己的提取器完成，不改变任何既有授权语义。
async fn api_auth_gate(
    State(state): State<Arc<WebState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<Response, AppError> {
    let path = req.uri().path();
    // 开放 API（/api/v1/*）不走这条会话门禁：它的鉴权是 Bearer 密钥，
    // 由各 v1 handler 里的 ApiCaller 自己完成——且**必须在**自己手里完成，
    // 因为 API 密钥没有 cookie 也没有 CSRF 语义，套 session_of 只会把所有
    // 合法的外部调用一律 401。豁免前缀而非逐个路径：将来加 v1 端点时
    // 忘记在这里登记的概率，远大于新增路径被漏审的风险。
    if path.starts_with("/api/")
        && !path.starts_with("/api/v1/")
        && path != "/api/health"
        // 深度探针同样免登录：它的调用方是监控/负载均衡，不是浏览器用户。
        // 它只回「依赖是否可用」，不回任何业务数据。
        && path != "/api/health/ready"
        && path != "/api/login"
        && path != "/api/logout"
        && path != "/api/setup/status"
    {
        crate::state::session_of(req.headers(), &state)?;
    }
    Ok(next.run(req).await)
}


/// 统一 fallback：/api/* 未匹配 → 已登录回 404、未登录回 401（M-9）；
/// 其余路径走静态资源（SPA 资产；"/" 已由 serve_index 路由处理）。
/// 静态服务从 main::build_app 移入此处，正是为了让 /api/* 不再落到
/// 静态 404——门禁与本 fallback 双重覆盖，对 axum 的 layer/fallback
/// 包裹顺序不敏感。
async fn spa_fallback(State(state): State<Arc<WebState>>, req: axum::extract::Request) -> Response {
    if req.uri().path().starts_with("/api/") {
        let (mut parts, _body) = req.into_parts();
        return match CurrentUser::from_request_parts(&mut parts, &state).await {
            Ok(_) => AppError::not_found("接口不存在").into_response(),
            Err(e) => e.into_response(),
        };
    }
    use tower::ServiceExt;
    // 带 `?v=` 的资源可以长期缓存：`index.html` 里的 `{{V}}` 会替换成静态文件
    // mtime 推出的版本号，**内容一变 URL 就变**，所以让浏览器一年内不再回源是安全的。
    // 不带 `?v=` 的直接访问（手输 /app.js、favicon 之类）必须回源验证，
    // 否则升级后用户会拿着旧脚本撞新接口。
    let versioned = req
        .uri()
        .query()
        .map(|q| q.split('&').any(|kv| kv.starts_with("v=")))
        .unwrap_or(false);
    tower_http::services::ServeDir::new(state.static_dir.clone())
        .oneshot(req)
        .await
        // tower-http 的响应体类型映射为 axum Body
        .map(|resp| {
            let (mut parts, body) = resp.map(axum::body::Body::new).into_parts();
            if parts.status.is_success() {
                parts.headers.insert(
                    header::CACHE_CONTROL,
                    if versioned {
                        axum::http::HeaderValue::from_static("public, max-age=31536000, immutable")
                    } else {
                        axum::http::HeaderValue::from_static("no-cache")
                    },
                );
            }
            Response::from_parts(parts, body)
        })
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}


/// 组装路由
pub fn router(state: Arc<WebState>) -> Router {
    Router::new()
        // 开放 API（/api/v1/*）：Bearer 密钥鉴权，与浏览器那套 session+CSRF 完全隔离。
        // merge 在同一层而非 nest：这些路径要绕开 api_auth_gate 的 session 门禁
        // （见 api_auth_gate 里的前缀豁免），也绕开 csrf_guard（Bearer 不会由浏览器
        // 自动携带，跨站表单也拿不到它，CSRF 对这种凭据不适用）。
        .merge(crate::openapi::routes())
        .route("/api/setup/status", get(get_setup_status))
        // 平台账套目录：列表（按归属过滤）+ 自建账套 + 选择当前账套
        .route("/api/books", get(list_books).post(create_book))
        .route("/api/consolidate/preview", get(consolidate_preview))
        // 平台经营总览：跨账套 × 各业务域可视化（对标金蝶星空云「多组织集团管控视图」）
        .route("/api/platform/overview", get(platform_overview))
        // 开放 API 密钥管理（仅平台管理员）。密钥本身属于**平台**资源而非账套资源，
        // 所以放在账号库（realm）而不是某个账套的 SQLite 里——跨账套的统一管控。
        .route(
            "/api/api-keys",
            get(crate::openapi::list_api_keys).post(crate::openapi::create_api_key),
        )
        .route(
            "/api/api-keys/:id",
            post(crate::openapi::toggle_api_key).delete(crate::openapi::delete_api_key),
        )
        .route("/api/books/:key/select", post(select_book))
        .route("/api/books/:key", delete(delete_book))
        // 上传外部账套（.fbk 或从别的服务器导出的 .zip 备份）。
        // 单独放宽 body 上限：全局 12MB，账套含附件时可能上百 MB。
        .route(
            "/api/books/import",
            post(import_book).layer(axum::extract::DefaultBodyLimit::max(MAX_IMPORT_BOOK_BYTES)),
        )
        // 导入整本《期初收集表》（四张纸一次导完，先算平再落库）
        .route(
            "/api/books/opening-sheet",
            post(import_opening_sheet)
                .layer(axum::extract::DefaultBodyLimit::max(MAX_IMPORT_BOOK_BYTES)),
        )
        // 同上但只体检不落库：期初是边整理边填的，能随时先看一眼平不平
        .route(
            "/api/books/opening-sheet/preview",
            post(preview_opening_sheet)
                .layer(axum::extract::DefaultBodyLimit::max(MAX_IMPORT_BOOK_BYTES)),
        )
        // 空模板（已预填当前账套的科目/客户/供应商清单）
        .route("/api/books/opening-sheet/template", get(opening_sheet_template))
        // 导入**之后**的对账：四张纸落库情况 + 五条交叉校验
        .route(
            "/api/books/opening-sheet/reconcile",
            get(opening_sheet_reconcile),
        )
        .route("/api/login", post(post_login))
        .route("/api/logout", post(post_logout))
        // 平台安全中心（仅管理员）：口令策略 / 登录审计 / 锁定账号
        .route(
            "/api/security/policy",
            get(get_security_policy).post(set_security_policy),
        )
        .route("/api/security/login-attempts", get(list_security_attempts))
        .route(
            "/api/security/locked-users",
            get(list_locked_security_users),
        )
        .route("/api/security/unlock", post(unlock_security_user))
        .route("/api/me", get(get_me))
        .route("/api/change-password", post(post_change_password))
        // 账号管理（仅管理员，作用于全局身份库）
        .route(
            "/api/platform/users",
            get(list_platform_users).post(create_platform_user),
        )
        .route(
            "/api/platform/users/:username",
            put(update_platform_user).delete(delete_platform_user),
        )
        .route(
            "/api/platform/users/:username/reset-password",
            post(reset_platform_password),
        )
        .route(
            "/api/platform/users/:username/reset-device",
            post(reset_platform_device),
        )
        // 用户管理
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/:username", put(update_user).delete(delete_user))
        .route(
            "/api/users/:username/reset-password",
            post(reset_user_password),
        )
        .route("/api/users/:username/reset-device", post(reset_user_device))
        .route("/api/users/:username/unlock", post(unlock_user))
        .route("/api/roles", get(list_roles))
        // 账套参数 / 仪表盘 / 期间
        .route("/api/options", get(get_options).put(put_options))
        .route("/api/options/export", get(export_options))
        .route("/api/options/import", post(import_options))
        .route("/api/options/reset", post(reset_options))
        // 打印预览页上的「点一下 / 拖一下」直接落库（局部更新，不覆盖纸型等它不认识的字段）
        .route("/api/reports/print-settings", post(post_print_settings))
        .route("/api/dashboard", get(get_dashboard))
        .route("/api/workbench", get(get_workbench))
        .route("/api/notices", get(get_notices))
        .route("/api/workflows/instance-for", get(get_wf_instance_for))
        .route("/api/quick-search", get(quick_search))
        .route("/api/overview", get(get_overview))
        .route("/api/periods", get(get_periods))
        .route("/api/period", post(post_period))
        // 期末处理（与桌面端对齐）：预检 / 结转损益 / 年末结转 / 结账 / 反结账
        .route("/api/periods/:ymm/precheck", get(period_precheck))
        // 月结向导：与 precheck 同一批判定，但拆成带 ok/blocking 的步骤清单，
        // 前端才能画成「打勾清单」而不是一串错误文字
        .route("/api/periods/:ymm/wizard", get(period_wizard))
        .route(
            "/api/periods/:ymm/carry-forward",
            post(period_carry_forward),
        )
        .route("/api/periods/:ymm/year-end", post(period_year_end))
        .route("/api/periods/:ymm/close", post(period_close))
        .route("/api/periods/:ymm/unclose", post(period_unclose))
        // 科目 / 凭证
        .route("/api/accounts", get(list_accounts))
        .route("/api/accounts/fill-defaults", post(fill_default_accounts))
        .route("/api/vouchers/next-no", get(next_voucher_no))
        .route("/api/vouchers", get(list_vouchers).post(save_voucher))
        // 分页列表：老端点默认 limit=200 且无「共 N 张」，一个期间超 200 张后
        // 后面的凭证在界面上完全不可见。放在 /api/vouchers/:id 之前注册，
        // 否则 "page" 会被当成 id 解析。
        .route("/api/vouchers/page", get(list_vouchers_page))
        .route("/api/vouchers/batch-post", post(voucher_batch_post))
        .route("/api/vouchers/:id", get(get_voucher))
        .route("/api/vouchers/:id/post", post(voucher_post))
        .route("/api/vouchers/:id/unpost", post(voucher_unpost))
        .route("/api/vouchers/:id/void", post(voucher_void))
        .route("/api/vouchers/:id/amend", post(voucher_amend))
        .route("/api/vouchers/:id/audit", post(voucher_audit))
        .route("/api/vouchers/:id/unaudit", post(voucher_unaudit))
        .route("/api/vouchers/:id/sign", post(voucher_sign))
        .route("/api/vouchers/:id/unsign", post(voucher_unsign))
        .route("/api/vouchers/:id/reverse", post(voucher_reverse))
        .route("/api/vouchers/:id/delete", post(voucher_delete))
        .route("/api/vouchers/renumber", post(voucher_renumber))
        // 凭证附件（上传/下载/删除）
        .route(
            "/api/vouchers/:id/attachments",
            get(list_voucher_attachments).post(upload_voucher_attachment),
        )
        .route(
            "/api/attachments/:id",
            get(download_attachment).delete(delete_attachment),
        )
        // 发票管理
        .route("/api/invoices", get(list_invoices).post(create_invoice))
        .route("/api/invoices/summary", get(invoice_summary))
        // 税务申报表（增值税一般纳税人）：只取数+算表+导出手工填报，不连网网报
        .route("/api/tax/vat", get(get_tax_vat))
        .route("/api/tax/vat/export", get(export_tax_vat))
        .route("/api/tax/vat/detail", get(get_tax_vat_detail))
        .route(
            "/api/tax/options",
            get(get_tax_options).post(save_tax_options),
        )
        // 存货级成本差异（采购价格差异 / 出库超支差异）。
        // 路径与订单级的 /api/cost/variance 刻意分开：两者口径不同（一个按存货
        // 收发存、一个按生产订单实际成本 vs BOM），同名会让界面上出现两个
        // "差异" 且数字对不上。
        .route("/api/cost/material-variance", get(get_cost_variance))
        .route("/api/invoices/from-po", post(invoice_from_po))
        .route("/api/invoices/from-so", post(invoice_from_so))
        .route(
            "/api/invoices/:id",
            put(update_invoice).delete(delete_invoice),
        )
        .route("/api/invoices/:id/status", post(invoice_set_status))
        // 数据导入（其他软件 / CSV）
        .route("/api/import/analyze", post(import_analyze))
        .route("/api/import/template", get(import_template))
        .route("/api/arap-opening", get(list_arap_opening))
        .route("/api/arap-opening/:id", delete(delete_arap_opening))
        // 客户视图（只读；余额口径由 findb::customers 保证走 open_entries）
        .route("/api/customers", get(list_customers))
        .route("/api/customers/:code", get(get_customer))
        // 账龄 Tab。必须与 `/:code` 分开注册：axum 0.8 里 `:code` 只匹配一段，
        // `/C01/aging` 会落到另一个 handler，写在一起容易看错。
        .route("/api/customers/:code/aging", get(get_customer_aging))
        // 交易历史 / 催款记录。同样各自一段路径，不当 `:code` 的子参数解析。
        .route("/api/customers/:code/trade", get(get_customer_trade))
        .route("/api/customers/:code/dunning", get(get_customer_dunning))
        .route("/api/items/master", get(items_master))
        .route("/api/import/run", post(import_run))
        // 账簿 / 报表
        .route("/api/ledger", get(get_ledger))
        .route("/api/ledger/general", get(get_general_ledger))
        .route("/api/ledger/journal", get(get_journal))
        .route("/api/ledger/print-form", get(print_ledger_form))
        .route("/api/vouchers/print-form", get(print_voucher_form))
        .route("/api/reports/trial-balance", get(get_trial_balance))
        .route("/api/reports/account-detail", get(account_detail_ep))
        .route("/api/reports/trial-balance/print", get(print_trial_balance))
        .route("/api/reports/trial-balance/xlsx", get(export_trial_balance_xlsx))
        .route(
            "/api/reports/trial-balance/export",
            get(export_trial_balance),
        )
        .route(
            "/api/reports/trial-balance/pdf",
            get(export_trial_balance_pdf),
        )
        // 数据导出（CSV，需 Export 权限）
        .route("/api/export/vouchers", get(export_vouchers))
        .route("/api/export/ledger", get(export_ledger))
        .route("/api/export/payroll", get(export_payroll))
        .route("/api/export/claims", get(export_claims))
        .route("/api/export/dunning", get(export_dunning))
        .route("/api/export/dunning/xlsx", get(export_dunning_xlsx))
        .route(
            "/api/export/schedules",
            get(list_export_schedules).post(save_export_schedule),
        )
        .route(
            "/api/export/schedules/:id/delete",
            post(delete_export_schedule),
        )
        .route("/api/export/schedules/:id/run", post(run_export_schedule))
        // xlsx 导出（需 Export 权限）。与上面的 CSV 并存而不是替换：
        // 那批是凭证/明细账/工资等老接口，前端和外部脚本在用。
        // 这批专给「要拿去继续算的表」和三大报表 —— 金额是数字，能求和能透视。
        .route("/api/export/aux", get(export_aux_xlsx))
        .route("/api/export/items", get(export_items_xlsx))
        .route("/api/export/accounts", get(export_accounts_xlsx))
        .route("/api/export/begin", get(export_begin_xlsx))
        // 高级功能：多栏账 / 摘要汇总表 / 财务指标
        .route("/api/reports/multi-column", get(get_multi_column))
        .route("/api/reports/summary-table", get(get_summary_table))
        .route("/api/reports/ratios", get(get_fin_ratios))
        // 财务核心：三大报表（资产负债表/利润表/现金流量表）
        .route("/api/reports/balance-sheet", get(get_balance_sheet))
        .route("/api/reports/balance-sheet/print", get(print_balance_sheet))
        .route("/api/reports/balance-sheet/xlsx", get(export_balance_sheet_xlsx))
        .route("/api/reports/income-statement", get(get_income_statement))
        .route(
            "/api/reports/income-statement/print",
            get(print_income_statement),
        )
        .route(
            "/api/reports/income-statement/xlsx",
            get(export_income_statement_xlsx),
        )
        .route("/api/reports/cash-flow", get(get_cash_flow))
        .route("/api/reports/cash-flow/print", get(print_cash_flow))
        .route("/api/reports/cash-flow/xlsx", get(export_cash_flow_xlsx))
        // 财务核心：所有者权益变动表 / 报表对比 / 科目日报表 / 期末对账
        .route("/api/reports/equity", get(get_equity_statement))
        .route("/api/reports/equity/print", get(print_equity_statement))
        .route("/api/reports/equity/xlsx", get(export_equity_xlsx))
        .route("/api/reports/compare", get(get_report_compare))
        .route("/api/reports/daily", get(get_account_daily))
        .route("/api/reports/reconcile", get(get_period_reconcile))
        // 辅助账 / 数量金额账（与桌面端对齐）
        .route("/api/reports/aux-balance", get(get_aux_balance))
        .route("/api/reports/qty-balance", get(get_qty_balance))
        // 自定义报表（UFO 公式，与桌面端对齐）
        .route(
            "/api/custom-reports",
            get(list_custom_reports).post(save_custom_report),
        )
        .route("/api/custom-reports/:key", get(get_custom_report))
        .route(
            "/api/custom-reports/:key/delete",
            post(delete_custom_report),
        )
        // 存货核算：成本调整
        .route("/api/inventory/adjust", post(stock_adjust_endpoint))
        // 库存深度：序列号 / 多单位 / 账龄 / ABC / 组装拆卸 / 分仓库
        .route(
            "/api/inventory/serial",
            get(list_serial).post(serial_in_endpoint),
        )
        .route("/api/inventory/serial/out", post(serial_out_endpoint))
        .route("/api/inventory/unit", get(get_unit).post(set_unit))
        .route("/api/inventory/aging", get(get_inv_aging))
        .route("/api/inventory/abc", get(get_abc))
        .route("/api/inventory/assemble", post(assemble_endpoint))
        .route("/api/inventory/disassemble", post(disassemble_endpoint))
        .route("/api/inventory/warehouse-stock", get(get_warehouse_stock))
        // 单条出入库流水（成本差异的「构成流水」点开明细用）
        .route("/api/inventory/move/:id", get(get_stock_move))
        // 仓库主数据（v30）
        .route("/api/warehouses", get(list_warehouses).post(save_warehouse))
        .route("/api/warehouses/:code", delete(delete_warehouse))
        .route(
            "/api/inventory/transfer",
            get(get_transfer_report).post(do_transfer),
        )
        .route("/api/inventory/batch-cost", get(batch_cost))
        // 采购/销售深度：暂估 / 对账 / 配额 / 订单变更
        .route(
            "/api/procure/estimate",
            get(list_estimates).post(add_estimate),
        )
        .route("/api/procure/estimate/status", get(estimate_status))
        .route("/api/procure/estimate/:id/settle", post(settle_estimate))
        .route("/api/procure/reconcile", get(get_po_reconcile))
        .route("/api/procure/quota", get(get_quota).post(set_quota))
        .route("/api/sales/reconcile", get(get_so_reconcile))
        .route("/api/order/change-log", get(get_change_log))
        // 采购/销售全生命周期：请购 / 报价 / 到货 / 发货 / 付款 / 收款 / 退货 / 信用
        .route(
            "/api/procure/req",
            get(list_purchase_req).post(save_purchase_req),
        )
        .route("/api/procure/req/:id/approve", post(approve_purchase_req))
        .route("/api/procure/req/:id/push-po", post(push_req_po))
        .route("/api/procure/price-history", get(price_history_ep))
        .route("/api/procure/receipt", post(add_po_receipt))
        .route("/api/procure/payment", post(add_po_payment))
        .route("/api/procure/return", post(add_po_return))
        .route("/api/procure/price", get(get_price_history))
        .route("/api/procure/track", get(get_po_track))
        .route("/api/procure/stats", get(get_purchase_stats))
        .route("/api/procure/po", get(list_po).post(save_po))
        .route("/api/procure/po/:id/transition", post(transition_po))
        .route("/api/procure/po/:id/delete", post(delete_po))
        .route("/api/procure/po/print-form", get(print_po_form))
        .route("/api/sales/quote", get(list_quotation).post(save_quotation))
        .route("/api/sales/quote/:id/approve", post(approve_quotation))
        .route("/api/sales/quote/:id/to-order", post(convert_quotation))
        .route("/api/sales/shipment", post(add_so_shipment))
        .route("/api/sales/payment", post(add_so_payment))
        .route("/api/sales/return", post(add_so_return))
        .route("/api/sales/credit", get(get_credit_check))
        .route("/api/sales/track", get(get_so_track))
        .route("/api/sales/stats", get(get_sales_stats))
        .route("/api/sales/so", get(list_so).post(save_so))
        .route("/api/sales/so/:id/transition", post(transition_so))
        .route("/api/sales/so/:id/notice", post(create_notice))
        .route("/api/sales/notices", get(list_notices))
        .route("/api/sales/so/:id/delete", post(delete_so))
        .route("/api/sales/so/print-form", get(print_so_form))
        // 预算预警
        .route("/api/budget/alerts", get(get_budget_alerts))
        // 坏账准备计提
        .route(
            "/api/settle/bad-debt/provision",
            post(bad_debt_provision_endpoint),
        )
        // 工艺路线 / 报工 / MRP
        .route("/api/routing/:item", get(get_routing).post(post_routing))
        .route("/api/routing/:item/delete", post(delete_routing))
        .route("/api/prod", get(list_prod_orders).post(create_prod_ep))
        // 销售订单下推生产订单：产销在数据上连起来
        .route("/api/prod/from-so", post(prod_from_so_ep))
        // 可承诺量（ATP）：现货 + 在途 − 已占用
        .route("/api/atp", get(get_atp))
        .route("/api/prod/:id", put(update_prod_ep))
        .route("/api/prod/:id/cancel", post(cancel_prod_ep))
        .route("/api/prod/:id/changes", get(list_prod_changes))
        .route("/api/prod/:id/qc", get(list_prod_qc).post(prod_qc_ep))
        .route("/api/prod/:id/ops", get(get_prod_ops))
        .route("/api/prod/op/report", post(report_prod_op))
        .route("/api/prod/op/finish", post(finish_prod_op))
        .route("/api/prod/:id/start", post(prod_start_ep))
        // 下达（草稿 → 已下达）：生产订单的审批动作，审批流挂在这里
        .route("/api/prod/:id/release", post(prod_release_ep))
        .route("/api/prod/:id/issue", post(prod_issue_ep))
        .route("/api/prod/:id/complete", post(prod_complete_ep))
        .route("/api/prod/:id/outsource-fee", post(outsource_fee_ep))
        .route("/api/bom", get(get_bom_ep).post(save_bom_ep))
        .route("/api/mrp/latest", get(get_mrp_latest))
        .route("/api/mrp/run", post(run_mrp))
        .route("/api/mrp/:id/to-req", post(mrp_to_req_ep))
        .route("/api/mps/run", post(run_mps))
        .route("/api/mps/latest", get(get_mps_latest))
        .route("/api/mps/:id/convert", post(convert_mps))
        .route("/api/mps/rough", post(rough_mps))
        .route("/api/prod/schedule", post(schedule_prod))
        // 预算版本
        .route(
            "/api/budget/versions",
            get(list_budget_versions).post(save_budget_version),
        )
        .route(
            "/api/budget/versions/:key/delete",
            post(delete_budget_version),
        )
        .route(
            "/api/budget/versions/:key/activate",
            post(activate_budget_version),
        )
        .route("/api/budget/versions/copy", post(copy_budget_version))
        .route(
            "/api/budget/rows",
            get(list_budget_rows).post(save_budget_row),
        )
        .route("/api/budget/rows/:id/delete", post(delete_budget_row))
        // 审批流
        .route("/api/approvals", get(list_approvals).post(start_approval))
        .route("/api/approvals/todo", get(list_approval_todo))
        .route("/api/approvals/:id", get(get_approval))
        .route("/api/approvals/:id/act", post(act_approval))
        .route("/api/approvals/:id/cancel", post(cancel_approval))
        // 报表附注
        .route("/api/reports/notes", get(list_notes).post(save_note))
        .route("/api/reports/notes/:id/delete", post(delete_note))
        // 会计电子档案
        .route("/api/archives", get(list_archives).post(create_archive))
        .route("/api/archives/:id", get(get_archive))
        .route("/api/archives/:id/verify", get(verify_archive))
        // 存活探针：**故意不碰任何依赖**。它只回答「进程还在收请求吗」，
        // 所以不能加 DB 查询 —— Docker HEALTHCHECK 每 30s 调一次，
        // 一旦让它碰 DB，数据库慢就会把容器判成 unhealthy 并被编排器重启，
        // 变成「数据库一慢就把应用重启」的自伤式故障放大。
        // 需要验证数据层请用 /api/health/ready（深度探针）。
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/health/ready", get(health_ready))
        // 资金：票据 / 融资 / 资金日报 / 资金预测（台账与总账联动：流转/结清自动生成凭证）
        .route("/api/funds/bills", get(list_bills).post(save_bill))
        .route("/api/funds/bills/:id/status", post(bill_transition))
        .route("/api/funds/bills/:id/delete", post(delete_bill))
        .route("/api/funds/bills/:id/voucher", post(bill_voucher))
        .route("/api/funds/loans", get(list_loans).post(save_loan))
        .route("/api/funds/loans/:id/settle", post(loan_settle))
        .route("/api/funds/loans/:id/delete", post(delete_loan))
        .route("/api/funds/loans/:id/voucher", post(loan_voucher))
        .route("/api/funds/daily", get(get_funds_daily))
        .route("/api/funds/daily-by-date", get(get_funds_daily_date))
        .route("/api/funds/budget", get(get_funds_budget))
        .route(
            "/api/funds/cash-counts",
            get(list_cash_counts).post(save_cash_count),
        )
        .route("/api/funds/cash-counts/:id/delete", post(delete_cash_count))
        .route(
            "/api/funds/cash-counts/:id/voucher",
            post(cash_count_voucher),
        )
        .route(
            "/api/funds/day-clear",
            get(list_day_clear).post(set_day_clear),
        )
        .route("/api/funds/checks", get(list_checks).post(save_check))
        .route("/api/funds/checks/:id/status", post(check_status))
        .route("/api/funds/checks/:id/delete", post(delete_check))
        .route("/api/funds/advances", get(list_advances).post(save_advance))
        .route("/api/funds/advances/:id/pay", post(pay_advance))
        .route("/api/funds/advances/:id/settle", post(settle_advance))
        .route("/api/funds/advances/:id/delete", post(delete_advance))
        // 出纳交接班：交班快照 + 接班确认 + 取消
        .route(
            "/api/funds/shifts",
            get(list_cash_shifts).post(create_cash_shift),
        )
        .route("/api/funds/shifts/:id/confirm", post(confirm_cash_shift))
        .route("/api/funds/shifts/:id/cancel", post(cancel_cash_shift))
        .route(
            "/api/funds/receipts",
            get(list_receipts).post(create_receipt),
        )
        .route("/api/funds/receipts/:id/delete", post(delete_receipt))
        .route("/api/funds/receipts/print-form", get(print_receipt_form))
        .route("/api/funds/receipts/:id/audit", post(audit_receipt))
        .route("/api/funds/receipts/:id/unaudit", post(unaudit_receipt))
        // 资金调拨（账户间转账）：与收付款单同构的建单/审核/撤审/删除链
        .route(
            "/api/funds/transfers",
            get(list_transfers).post(create_transfer),
        )
        .route("/api/funds/transfers/:id/audit", post(audit_transfer))
        .route("/api/funds/transfers/:id/unaudit", post(unaudit_transfer))
        .route("/api/funds/transfers/:id/delete", post(delete_transfer))
        .route("/api/inventory/counts", get(list_counts))
        .route("/api/inventory/count", post(create_count))
        .route("/api/inventory/count/:id/apply", post(apply_count))
        .route("/api/inventory/count/:id/delete", post(delete_count))
        .route("/api/inventory/batch", post(register_batch))
        .route("/api/inventory/batches", get(list_batches))
        .route("/api/inventory/batches/fefo", post(fefo_batches))
        .route("/api/inventory/batches/expiring", get(expiring_batches_ep))
        .route(
            "/api/inventory/locations",
            get(list_locations).post(save_location),
        )
        .route("/api/inventory/locations/:id/delete", post(delete_location))
        .route("/api/inventory/form-convert", post(form_convert_ep))
        .route("/api/inventory/qc", post(qc_order_ep))
        .route("/api/inventory/below-safety", get(below_safety_ep))
        .route("/api/funds/forecast", get(get_funds_forecast))
        .route(
            "/api/funds/forecast-rolling",
            get(funds_forecast_rolling_ep),
        )
        // 预算分析
        .route("/api/budget/analysis", get(get_budget_analysis))
        // 成本：计价配置 + 期末结价
        .route(
            "/api/cost/configs",
            get(list_cost_configs).post(save_cost_method),
        )
        // 到岸成本：把运费/关税摊进存货成本
        .route(
            "/api/landed-cost",
            get(list_landed_cost).post(save_landed_cost),
        )
        .route("/api/landed-cost/:id/post", post(post_landed_cost))
        .route("/api/landed-cost/from-po", get(landed_cost_from_po))
        .route("/api/cost/configs/:item/delete", post(clear_cost_method))
        .route("/api/cost/gl-reconcile", get(gl_reconcile_ep))
        .route("/api/cost/sales-cost", post(sales_cost_ep))
        .route(
            "/api/cost/period-end",
            get(run_period_end_cost).post(run_period_end_cost),
        )
        .route("/api/cost/wip", get(cost_wip_ep))
        .route("/api/cost/variance", get(cost_variance_ep))
        .route("/api/cost/forecast", get(cost_forecast_ep))
        .route("/api/cost/overhead", post(cost_overhead_ep))
        // 固定资产（与桌面端对齐）：卡片 / 折旧计划 / 计提 / 清理
        .route("/api/assets", get(list_assets).post(create_asset))
        .route("/api/assets/:id", put(update_asset).delete(delete_asset))
        .route("/api/assets/:id/depreciations", get(list_asset_deps))
        .route("/api/assets/:id/changes", get(list_asset_changes))
        .route("/api/assets/gl-reconcile", get(asset_gl_reconcile))
        .route("/api/assets/:id/impair", post(impair_asset))
        .route(
            "/api/assets/counts",
            get(list_asset_counts).post(save_asset_count),
        )
        .route("/api/assets/counts/:id/post", post(post_asset_count))
        .route("/api/assets/:id/dispose", post(dispose_asset))
        .route("/api/assets/depreciate", post(depreciate_assets))
        .route(
            "/api/assets/depreciations/delete-period",
            post(delete_asset_deps_period),
        )
        // 银行对账（与桌面端对齐）
        .route("/api/bank", get(get_bank))
        .route("/api/bank/import", post(import_bank))
        .route("/api/bank/auto-match", post(auto_match_bank))
        .route("/api/bank/gen-vouchers", post(gen_bank_vouchers))
        .route("/api/bank/link", post(link_bank))
        .route("/api/bank/unlink", post(unlink_bank))
        .route("/api/bank/clear", post(clear_bank))
        // 往来核销（与桌面端对齐）
        .route("/api/settle/open", get(get_settle_open))
        .route("/api/settle/auto", post(auto_settle_endpoint))
        .route("/api/settle/run", post(manual_settle_endpoint))
        // 通用往来互抵：跨往来单位 / 跨科目（代收代付、三方抵账），自动出过渡凭证
        .route("/api/settle/cross", post(cross_settle_endpoint))
        .route("/api/settle/records", get(list_settle_records))
        .route("/api/settle/unsettle", post(unsettle_endpoint))
        .route("/api/settle/aging", get(get_settle_aging))
        .route("/api/settle/aging/xlsx", get(export_settle_aging_xlsx))
        // 往来对冲单（应收 ↔ 应付相互抵销：借 应付 / 贷 应收，两端各自 FIFO 核销）
        .route("/api/settle/offsets", get(list_offsets).post(create_offset))
        .route("/api/settle/offsets/:id/audit", post(audit_offset))
        .route("/api/settle/offsets/:id/unaudit", post(unaudit_offset))
        .route("/api/settle/offsets/:id/delete", post(delete_offset))
        .route(
            "/api/settle/dunnings",
            get(list_dunnings).post(create_dunning),
        )
        .route("/api/settle/dunnings/:id/status", post(dunning_status_ep))
        .route("/api/settle/dunnings/:id/print", get(print_dunning))
        // ---- 往来对账单（按期间对账 + 客户回签确认）----
        .route(
            "/api/settle/statements",
            get(list_statements).post(create_statement),
        )
        .route(
            "/api/settle/statements/:id/status",
            post(statement_status_ep),
        )
        // ---- 账套内基础资料与系统功能（对齐桌面端 finui 补齐）----
        .route("/api/accounts", post(create_account).put(update_account))
        .route("/api/accounts/:code", delete(delete_account))
        .route("/api/begin", get(list_begin).post(save_begin))
        .route(
            "/api/begin-batch",
            post(parse_begin_batch).put(import_begin_batch),
        )
        .route("/api/logs", get(list_logs))
        .route("/api/backups", get(list_backups).post(create_backup))
        .route("/api/restore", post(restore_backup))
        .route("/api/backup/download", get(download_backup))
        .route("/api/templates", get(list_templates).post(create_template))
        .route("/api/templates/due", get(due_templates))
        .route(
            "/api/templates/:id",
            put(update_template).delete(delete_template),
        )
        .route("/api/templates/:id/generate", post(generate_template))
        .route("/api/aux", get(list_aux).post(create_aux))
        .route(
            "/api/aux-batch",
            post(parse_aux_batch).put(import_aux_batch),
        )
        // 路径刻意用 `aux-presets` 而不是 `aux/presets`：上面那条
        // `/api/aux/:id` 会把 `presets` 当成 id 吃掉。
        .route("/api/aux-presets", get(get_aux_presets).post(preset_aux))
        .route("/api/item-plan", get(get_item_plan).post(save_item_plan))
        .route("/api/aux/:id", put(update_aux).delete(delete_aux))
        .route("/api/payroll", get(list_payroll).post(save_payroll))
        .route("/api/payroll/generate", post(generate_payroll))
        .route("/api/payroll/ytd", get(payroll_ytd))
        .route("/api/payroll/accrue", post(payroll_accrue))
        .route("/api/payroll/social-pay", post(payroll_social_pay))
        .route("/api/payroll/pay", post(payroll_pay))
        .route("/api/payroll/:id", delete(delete_payroll))
        .route("/api/payroll/bank-file", get(export_payroll_bank_file))
        .route("/api/payroll/slip", get(get_payroll_slip))
        .route("/api/payroll/tax-report", get(get_payroll_tax_report))
        .route("/api/claims", get(list_claims).post(create_claim))
        .route("/api/claims/next-no", get(next_claim_no))
        .route("/api/claims/:id", put(update_claim).delete(delete_claim))
        .route("/api/claims/:id/transition", post(claim_transition))
        .route("/api/claims/:id/voucher", post(claim_voucher))
        .route("/api/workflows", get(list_workflows).post(save_workflow))
        .route("/api/workflows/templates", get(list_wf_templates))
        .route("/api/workflows/cond-fields", get(get_wf_cond_fields))
        .route("/api/workflows/templates/apply", post(apply_wf_template))
        .route("/api/workflows/instances", get(list_wf_instances))
        .route("/api/workflows/:id/publish", post(publish_workflow))
        .route("/api/workflows/:id/unpublish", post(unpublish_workflow))
        .route("/api/workflows/:id/delete", post(delete_workflow))
        .route("/api/doc-links", get(get_doc_links))
        // SPA 首页：动态注入资源版本号，避免浏览器长期缓存旧版 JS/CSS
        .route("/", get(serve_index))
        .layer(axum::middleware::from_fn(csrf_guard))
        // 附件上传最大 10MB（引擎限制），请求体留一点余量
        .layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024))
        // M-9 会话门禁：路由匹配前统一 401 未认证的 /api/*（公开接口除外）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api_auth_gate,
        ))
        // M-9 统一 fallback：/api/* 未匹配 → 已登录 404 / 未登录 401；其余走静态资源
        .fallback(spa_fallback)
        // 压缩。放在最外层（最后 .layer）= 连静态资源和 fallback 一起压：
        // app.js 800KB → gzip 后 207KB，手机 4G 上首屏少下 0.6MB；
        // /api/* 的 JSON 同理（凭证列表、报表都是几倍冗余的文本）。
        //
        // 已经压过的类型不再压：xlsx / zip 内部就是 deflate，再 gzip 一遍
        // 体积不变、CPU 白烧，大导出还会明显变慢。
        .layer(CompressionLayer::new().compress_when(
            SizeAbove::new(1024)
                .and(NotForContentType::new("application/zip"))
                .and(NotForContentType::new(
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                )),
        ))
        .with_state(state)
}


/// CSRF 纵深防御：对状态变更请求校验 Sec-Fetch-Site / Origin / Referer。
///
/// 主防线仍是 SameSite=Lax + JSON Content-Type（跨站表单拿不到 Cookie 也发不出 JSON），
/// 这里额外拦"同站被攻破的其他源/旧浏览器/代理改写"等场景。非浏览器客户端
/// （curl/脚本）不带这些头，不拦截——它们本来也不依赖 Cookie 自动携带。
async fn csrf_guard(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    use axum::http::Method;
    let method = req.method().clone();
    if matches!(
        method,
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    ) {
        let reject = || {
            (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "跨站请求被拒绝" })),
            )
                .into_response()
        };
        if let Some(site) = req
            .headers()
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
        {
            if site != "same-origin" && site != "none" {
                return reject();
            }
        }
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_ascii_lowercase());
        if let Some(origin) = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
        {
            if !origin_host_matches(origin, host.as_deref()) {
                return reject();
            }
        } else if let Some(referer) = req
            .headers()
            .get(header::REFERER)
            .and_then(|v| v.to_str().ok())
        {
            if !origin_host_matches(referer, host.as_deref()) {
                return reject();
            }
        }
    }
    next.run(req).await
}


/// `scheme://host[:port]/...` 的主机是否与 Host 头一致（无 Host 头时无法比较，放行）
fn origin_host_matches(url: &str, host: Option<&str>) -> bool {
    let Some(host) = host else { return true };
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let origin_host = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    !origin_host.is_empty() && origin_host == host
}


/// 返回 SPA 首页，并把 `{{V}}` 占位符替换为当前资源版本号。
/// 版本号由静态文件 mtime 计算，任何前端改动都会使 URL 变化 → 浏览器缓存自动失效。
async fn serve_index(State(state): State<Arc<WebState>>) -> Response {
    use axum::response::Html;
    let path = state.static_dir.join("index.html");
    let html = match std::fs::read_to_string(&path) {
        Ok(h) => h,
        Err(e) => {
            // 路径只进服务端日志，避免未登录即可探测文件系统布局
            error!(path = %path.display(), error = %e, "读取 index.html 失败");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "服务器内部错误，请稍后重试" })),
            )
                .into_response();
        }
    };
    // 首页**绝不能被缓存**：它带 `{{V}}` 占位符替换，缓存住就等于把「升级前的
    // 资源版本号」钉在用户浏览器里，升级后拿旧脚本打新接口。措辞用 `no-cache`
    // 而不是 `no-store`：允许存，但每次必须回源验证（304 很便宜）。
    let mut r = Html(html.replace("{{V}}", &state.assets_ver)).into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    r
}


/// 账套可见列表：管理员全部可见；其余按授权三层（管理员 / 归属者 / 账套内成员）取并集。
///
/// `RealmDb::list_books_for` 只按 `owner_username` 过滤，realm 层不便改动，故在此补齐
/// 「被邀请成员」这一支——`create_user` 会为非归属的平台账号建账套成员行，而
/// `select_book` / `CurrentUser` 的授权判定都认这条成员行，两边口径必须一致，
/// 否则被邀请成员登录后看不到自己被邀请进的账套（能进却列不出来）。
///
/// 逐个账套查成员行的取数方式与 `select_book` 逐字一致；账套打不开（文件损坏等）
/// 同样透出 500 而不是吞成"不可见"，避免用一个假象掩盖真实故障。
fn visible_books(
    state: &WebState,
    username: &str,
    is_admin: bool,
) -> Result<Vec<RealmBook>, AppError> {
    let mut books = state.realm.list_books_for(username, is_admin)?;
    if is_admin {
        return Ok(books);
    }
    for b in state.realm.list_books_for(username, true)? {
        if books.iter().any(|x| x.key == b.key) {
            continue;
        }
        let is_member = {
            let db = state.db_for(&b.key)?;
            users::get(&db, username).ok().flatten().is_some()
        };
        if is_member {
            books.push(b);
        }
    }
    books.sort_by_key(|b| b.id);
    Ok(books)
}


/// 取来源 IP：优先反代写入的 X-Forwarded-For（首个地址）/ X-Real-IP。
/// 直连（未配反代）时取不到，返回 None，仅跳过 IP 维度限流，账号维度仍生效。
fn client_ip(headers: &HeaderMap) -> Option<String> {
    let pick = |v: &str| -> Option<String> {
        let s = v.split(',').next().unwrap_or("").trim().to_string();
        (!s.is_empty() && s.len() <= 64).then_some(s)
    };
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(pick)
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(pick)
        })
}


#[derive(Deserialize)]
struct LoginAttemptsQuery {
    username: Option<String>,
    limit: Option<u32>,
}


#[derive(Deserialize)]
struct UnlockSecurityReq {
    username: String,
}


#[derive(serde::Deserialize)]
struct ProdFromSoReq {
    so_id: i64,
    /// 计划数量；留空 = 下推该订单尚未认领的全部余量
    #[serde(default)]
    qty: String,
    #[serde(default)]
    date: String,
}


// ═══════════════════════════════════════════════════════════════
// 到岸成本（Landed Cost）
//
// 权限口径：写 = 采购订单操作权（与采购入库同级），因为它直接改变存货成本；
// 读 = 报表权。**刻意不给「总账」权限就能改存货成本** —— 那是绕过职责分离。
// ═══════════════════════════════════════════════════════════════

#[derive(serde::Deserialize)]
struct LcLineReq {
    po_id: i64,
    #[serde(default)]
    item_code: String,
    #[serde(default)]
    item_name: String,
    #[serde(default)]
    qty: String,
    #[serde(default)]
    weight: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    manual_amount: String,
    #[serde(default)]
    memo: String,
}


#[derive(serde::Deserialize)]
struct LcSaveReq {
    #[serde(default)]
    id: i64,
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    supplier_code: String,
    #[serde(default)]
    supplier_name: String,
    #[serde(default)]
    basis: String,
    #[serde(default)]
    freight: String,
    #[serde(default)]
    duty: String,
    #[serde(default)]
    insurance: String,
    #[serde(default)]
    clearing: String,
    #[serde(default)]
    other: String,
    #[serde(default)]
    dr_account: String,
    #[serde(default)]
    cr_account: String,
    /// 付款账户（贷方核算银行账户时必填）
    #[serde(default)]
    bank_account: String,
    #[serde(default)]
    expense_account: String,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    lines: Vec<LcLineReq>,
}


fn lc_line_json(l: &findb::landedcost::LcLine) -> serde_json::Value {
    json!({
        "id": l.id,
        "po_id": l.po_id,
        "po_no": l.po_no,
        "item_code": l.item_code,
        "item_name": l.item_name,
        "qty": l.qty.fmt_qty(),
        "weight": l.weight.fmt_qty(),
        "amount": l.amount.fmt_money(),
        "manual_amount": l.manual_amount.fmt_money(),
        "allocated": l.allocated.fmt_money(),
        "to_stock": l.to_stock.fmt_money(),
        "to_expense": l.to_expense.fmt_money(),
        "on_hand_qty": l.on_hand_qty.fmt_qty(),
        "memo": l.memo,
    })
}


fn lc_json(lc: &findb::landedcost::LandedCost) -> serde_json::Value {
    // 刻意把明细拆到独立函数：这个 json! 宏展开到 25 个字段时已经撞上
    // `recursion limit reached while expanding json_internal!`，加字段就会编译失败。
    // 明细单独一个宏，宏的规模才可控。
    json!({
        "id": lc.id,
        "no": lc.no,
        "period": lc.period,
        "date": lc.date,
        "supplier_code": lc.supplier_code,
        "supplier_name": lc.supplier_name,
        "basis": lc.basis,
        "basis_label": findb::landedcost::AllocBasis::parse(&lc.basis).label(),
        "freight": lc.freight.fmt_money(),
        "duty": lc.duty.fmt_money(),
        "insurance": lc.insurance.fmt_money(),
        "clearing": lc.clearing.fmt_money(),
        "other": lc.other.fmt_money(),
        "total_charges": lc.total_charges().fmt_money(),
        "dr_account": lc.dr_account,
        "cr_account": lc.cr_account,
        "bank_account": lc.bank_account,
        "expense_account": lc.expense_account,
        "status": lc.status,
        "status_label": findb::landedcost::status_label(&lc.status),
        "voucher_id": lc.voucher_id,
        "memo": lc.memo,
        "prepared_by": lc.prepared_by,
        "total_allocated": lc.total_allocated().fmt_money(),
        "total_to_stock": lc.total_to_stock().fmt_money(),
        "total_to_expense": lc.total_to_expense().fmt_money(),
        "lines": lc.lines.iter().map(lc_line_json).collect::<Vec<_>>(),
    })
}


#[derive(Deserialize)]
struct OverheadReq {
    period: i32,
    amount: String,
    /// cost（默认，按已归集成本）/ labor（按直接人工）/ qty（按计划产量）
    #[serde(default)]
    base: String,
    /// true = 写入归集；false = 仅试算
    #[serde(default)]
    apply: bool,
}


// ---- 预算编制（当前激活版本的预算行，Web 入口） ----

#[derive(Deserialize)]
struct BudgetRowReq {
    /// 客户端编辑时会回传，但 upsert 按「期间+科目+部门+版本」定位，
    /// 真正的行 id 由 budget_upsert_version 决定，故这里只收不用。
    #[serde(default)]
    #[allow(dead_code)]
    id: i64,
    period: i32,
    account_code: String,
    #[serde(default)]
    dept: String,
    amount: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct ExportScheduleReq {
    #[serde(default)]
    id: i64,
    kind: String,
    #[serde(default)]
    period_mode: String,
    #[serde(default)]
    at_time: String,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    memo: String,
}


/// 生成唯一账套 key（文件名，不含扩展名）
fn make_book_key(raw: &str, owner: &str, realm: &RealmDb) -> Result<String, AppError> {
    let now = chrono::Local::now();
    let base = if raw.trim().is_empty() {
        format!("{}_{:04}{:02}", owner, now.year(), now.month())
    } else {
        let cleaned: String = raw
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let trimmed = cleaned.trim_matches('_').to_string();
        if trimmed.is_empty() {
            format!("{}_{:04}{:02}", owner, now.year(), now.month())
        } else {
            trimmed
        }
    };
    // 唯一化：若冲突则在末尾追加序号
    let mut key = base.clone();
    let mut n = 1;
    while realm.get_book(&key)?.is_some() {
        n += 1;
        key = format!("{base}_{n}");
    }
    Ok(key)
}


/// 凡是要把口令写进存储的入口，都必须先过这道校验。
///
/// 单密码统一后所有口令入口都走应用级 PasswordPolicy（默认 8 位且需含字母+数字）：
/// Web 层先由本函数做 400 校验，realm 层再以 policy 参数复核，双层一致。
fn check_password(state: &WebState, pwd: &str) -> Result<(), AppError> {
    state.policy().check(pwd).map_err(AppError::bad_request)
}


fn current_period(state: &WebState, user: &CurrentUser) -> Period {
    let ymm = user
        .token
        .is_empty()
        .then_some(state.default_period)
        .or_else(|| state.sessions.period(&user.token))
        .unwrap_or(state.default_period);
    // 会话/配置中的期间也做校验：脏数据不能让后续 first_day/last_day panic
    Period::from_ymm_checked(ymm)
        .or_else(|_| Period::from_ymm_checked(state.default_period))
        .unwrap_or_else(|_| Period::default())
}


/// 校验用户传入的期间（YYYYMM）：非法值返回 400。
/// 直接用 `Period::from_ymm` 会让非法期间一路流到 `first_day()/last_day()` 处 panic，
/// 在 `panic = "abort"` 的 release 下等于把整个服务打挂。
fn period_checked(ymm: i32) -> Result<Period, AppError> {
    Period::from_ymm_checked(ymm).map_err(|e| AppError::bad_request(e.to_string()))
}


/// 首页首屏两块：本期赚没赚（利润口径）+ 钱在哪（往来口径）
///
/// 取数全部复用报表那条路径（`findb::advanced::financial_totals` /
/// `financial_analysis` / `money_position`），**不另算一套**。首页自己算一遍
/// 的下场是「首页说赚 10 万，点进利润表是 8.5 万」，而两边都"看起来对"。
fn home_brief(
    db: &findb::Db,
    cur: Period,
    user: &fincore::user::User,
) -> Result<HomeBrief, AppError> {
    // 数据范围必须带着走：首页那三块数字与利润表、资产负债表是同一批数，
    // 利润表按权限裁了而首页不裁，等于「首页看到全公司、点进报表只剩自己的」。
    // 这类"两处数字不一样"的 bug 最难查，因为两边都觉得自己对。
    let scope = Some(user);
    let t = findb::advanced::financial_totals(db, cur, cur, scope)?;
    // 本年累计：不再用 `financial_totals(db, cur, 年初, ..)`——那是另一组查询键，
    // 等于把整本账重扫一遍（6000 张凭证规模下约 8ms）。快照里已经算好了
    // 1 月至本期的累计发生额，`financial_ytd` 命中同一份缓存直接读它。
    let ytd = findb::advanced::financial_ytd(db, cur, scope)?;
    // 上月净利润：只为了算环比。"这个月赚 8 万"本身没有意义，老板真正想
    // 知道的是"比上个月好还是差"。取不到（建账首月）就当 0。
    let prev_np = findb::advanced::financial_totals(db, cur.prev(), cur.prev(), scope)
        .map(|x| x.net_profit)
        .unwrap_or(fincore::Money::ZERO);
    let nc = t.net_profit;

    // 利润率：收入为 0 时不显示（不是 0%，是"没法算"）
    let margin = if t.revenue.is_zero() {
        None
    } else {
        Some((t.net_profit.to_f64() / t.revenue.to_f64() * 100.0 * 10.0).round() / 10.0)
    };

    // 成本构成 Top 3：老板一定会问"钱都花哪了"，最大的几块直接摆出来。
    //
    // 用 cost_drivers_of 而不是 financial_analysis：后者为了画整年趋势要
    // 逐月调 financial_totals（10 个月 = 10 次全表扫），而这里只要三条。
    // 实测首页 233ms 里有 160ms 是这么白烧的。
    let mut cost_top = Vec::new();
    if let Ok(drivers) = findb::advanced::cost_drivers_of(db, cur, cur, None) {
        for d in drivers.iter().take(3) {
            if d.amount.is_zero() {
                continue;
            }
            cost_top.push(HomeDriver {
                name: d.name.clone(),
                amount: d.amount.fmt_money(),
            });
        }
    }

    // 没记账的凭证：这些**没算进**上面的收入/成本/净利润，必须当面说清楚，
    // 否则老板看到"这个月只赚这么点"根本不知道是自己还没记账。
    let (draft, audited, _, _) = findb::vouchers::status_summary(db, cur).unwrap_or((0, 0, 0, 0));

    // 账龄以「当月最后一天」为基准：用今天算会让补录上个月的凭证显示成 0 天。
    let as_of = month_end(cur);
    let mp = findb::advanced::money_position(db, cur, as_of, scope)
        .unwrap_or_else(|_| empty_money_position());

    let party = |p: &findb::advanced::PartyMoney| HomeParty {
        code: p.code.clone(),
        name: p.name.clone(),
        amount: p.amount.fmt_money(),
        max_days: p.max_days,
        also: p.also.fmt_money(),
        net: p.net_with().fmt_money(),
        net_is_receivable: p.net_is_receivable(),
        both: !p.also.is_zero(),
    };
    let net = mp.net();
    Ok(HomeBrief {
        period: period_to_str(cur),
        revenue: t.revenue.fmt_money(),
        cost: t.cost.fmt_money(),
        net_profit: t.net_profit.fmt_money(),
        net_profit_ytd: ytd.net_profit.fmt_money(),
        net_profit_prev: prev_np.fmt_money(),
        net_profit_diff: (nc - prev_np).fmt_money(),
        margin,
        cost_top,
        unposted: draft + audited,
        money: HomeMoney {
            receivable: mp.receivable.fmt_money(),
            payable: mp.payable.fmt_money(),
            advance_out: mp.advance_out.fmt_money(),
            advance_in: mp.advance_in.fmt_money(),
            cash: mp.cash.fmt_money(),
            net: net.fmt_money(),
            net_is_receivable: net >= fincore::Money::ZERO,
            overdue_90: mp.overdue_90.fmt_money(),
            overdue_90_count: mp.overdue_90_count,
            top_customers: mp.top_customers.iter().map(&party).collect(),
            top_suppliers: mp.top_suppliers.iter().map(&party).collect(),
        },
    })
}


/// 当月最后一天（账龄基准）
fn month_end(p: Period) -> chrono::NaiveDate {
    let first = chrono::NaiveDate::from_ymd_opt(p.year(), p.month(), 1);
    let next = if p.month() == 12 {
        chrono::NaiveDate::from_ymd_opt(p.year() + 1, 1, 1)
    } else {
        chrono::NaiveDate::from_ymd_opt(p.year(), p.month() + 1, 1)
    };
    match (first, next) {
        (Some(f), Some(n)) => n.pred_opt().unwrap_or(f),
        _ => chrono::Local::now().date_naive(),
    }
}


/// 「钱在哪」取数失败时的空壳：首页显示 0，不让整页 500。
fn empty_money_position() -> findb::advanced::MoneyPosition {
    use fincore::Money;
    findb::advanced::MoneyPosition {
        receivable: Money::ZERO,
        payable: Money::ZERO,
        advance_in: Money::ZERO,
        advance_out: Money::ZERO,
        cash: Money::ZERO,
        receivable_gross: Money::ZERO,
        payable_gross: Money::ZERO,
        top_customers: Vec::new(),
        top_suppliers: Vec::new(),
        overdue_90: Money::ZERO,
        overdue_90_count: 0,
    }
}


/// 从语句表按行名（包含匹配）提取首个数值列，取不到 → None（前端显示 "—"）
fn stmt_val(t: &fincore::report::ReportTable, names: &[&str]) -> Option<Money> {
    t.rows
        .iter()
        .find(|r| names.iter().any(|n| r.name.contains(n)))
        .and_then(|r| r.values.first())
        .copied()
}


/// 报表域：资产/负债/权益/收入/净利润卡片 + 资产与利润多期趋势
fn wb_report_domain(
    db: &findb::Db,
    user: &CurrentUser,
    cur: Period,
    n: i32,
    out: &mut findb::workbench::WbOut,
) -> Result<(), AppError> {
    // 当期卡片：仍用完整报表（含期初余额、结转损益等），保证数字准确
    let bs_one = |p: Period| -> Result<fincore::report::ReportTable, AppError> {
        statement_table(
            db,
            user,
            "balance_sheet",
            p,
            p,
            vec![
                Box::new(fincore::report::identity),
                Box::new(fincore::report::to_begin),
            ],
        )
    };
    let is_one = |p: Period| -> Result<fincore::report::ReportTable, AppError> {
        statement_table(
            db,
            user,
            "income_statement",
            p,
            p,
            vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
        )
    };
    let g = |t: &fincore::report::ReportTable, names: &[&str]| -> String {
        stmt_val(t, names)
            .map(|m| m.fmt_money())
            .unwrap_or_else(|| "—".to_string())
    };
    let bs = bs_one(cur)?;
    let is = is_one(cur)?;
    let push = |out: &mut findb::workbench::WbOut, key: &str, label: &str, value: String| {
        out.cards.push(findb::workbench::WbCard {
            domain: "报表".into(),
            key: key.into(),
            label: label.into(),
            value,
            unit: "元".into(),
        });
    };
    push(out, "assets", "资产合计", g(&bs, &["资产合计", "资产总计"]));
    push(out, "liab", "负债合计", g(&bs, &["负债合计", "负债总计"]));
    push(
        out,
        "equity",
        "所有者权益合计",
        g(&bs, &["所有者权益合计", "所有者权益总计"]),
    );
    push(out, "revenue", "本期营业收入", g(&is, &["营业收入"]));
    push(out, "profit", "本期净利润", g(&is, &["净利润"]));

    // 趋势图：用专用查询一次性算出 12 期 × 5 个指标，避免 26 次完整报表渲染
    // 返回 BTreeMap<period_ymm, (assets, liabilities, equity, revenue, net_profit)>
    let trends = findb::workbench::collect_report_trends(db, &user.user, cur, n)?;
    let ps = findb::workbench::period_series(cur, n);
    let labels = findb::workbench::period_labels(&ps);
    let mut assets_pts = Vec::new();
    let mut liab_pts = Vec::new();
    let mut equity_pts = Vec::new();
    let mut inc_pts = Vec::new();
    let mut prof_pts = Vec::new();
    for p in &ps {
        if let Some((a, l, e, rev, np)) = trends.get(&p.ymm()) {
            assets_pts.push(findb::workbench::money_f64(*a));
            liab_pts.push(findb::workbench::money_f64(*l));
            equity_pts.push(findb::workbench::money_f64(*e));
            inc_pts.push(findb::workbench::money_f64(*rev));
            prof_pts.push(findb::workbench::money_f64(*np));
        } else {
            assets_pts.push(0.0);
            liab_pts.push(0.0);
            equity_pts.push(0.0);
            inc_pts.push(0.0);
            prof_pts.push(0.0);
        }
    }
    out.trends.push(findb::workbench::WbTrend {
        domain: "报表".into(),
        key: "report_assets".into(),
        title: "资产合计走势".into(),
        unit: "元".into(),
        periods: labels.clone(),
        series: vec![findb::workbench::WbSeries {
            name: "资产合计".into(),
            color: "#1976d2".into(),
            points: assets_pts.clone(),
        }],
    });
    out.trends.push(findb::workbench::WbTrend {
        domain: "报表".into(),
        key: "report_liab".into(),
        title: "负债合计走势".into(),
        unit: "元".into(),
        periods: labels.clone(),
        series: vec![findb::workbench::WbSeries {
            name: "负债合计".into(),
            color: "#e64a19".into(),
            points: liab_pts.clone(),
        }],
    });
    out.trends.push(findb::workbench::WbTrend {
        domain: "报表".into(),
        key: "report_equity".into(),
        title: "所有者权益走势".into(),
        unit: "元".into(),
        periods: labels.clone(),
        series: vec![findb::workbench::WbSeries {
            name: "所有者权益".into(),
            color: "#6a1b9a".into(),
            points: equity_pts,
        }],
    });
    out.trends.push(findb::workbench::WbTrend {
        domain: "报表".into(),
        key: "report_profit".into(),
        title: "收入与净利润走势".into(),
        unit: "元".into(),
        periods: labels,
        series: vec![
            findb::workbench::WbSeries {
                name: "营业收入".into(),
                color: "#1565c0".into(),
                points: inc_pts,
            },
            findb::workbench::WbSeries {
                name: "净利润".into(),
                color: "#c62828".into(),
                points: prof_pts,
            },
        ],
    });
    Ok(())
}


/// 期末结账请求
#[derive(Deserialize, Default)]
struct ClosePeriodReq {
    /// 是否要求先结转损益才能结账
    #[serde(default)]
    pub require_carry: bool,
}


fn can_view_vouchers(user: &CurrentUser) -> bool {
    user.can(Perm::Report)
        || user.can(Perm::VoucherNew)
        || user.can(Perm::VoucherEdit)
        || user.can(Perm::VoucherAudit)
        || user.can(Perm::VoucherPost)
}


/// 「仅看本人经手的业务单据」（own_doc_only）：工资按员工、报销按申请人匹配
/// 当前登录人的用户名/显示名。只过滤列表是不够的——按 id/参数的读写入口都要过这里。
fn doc_in_scope(user: &CurrentUser, who: &str) -> bool {
    if !user.user.data_scope.own_doc_only {
        return true;
    }
    let who = who.trim();
    who == user.user.username || who == user.user.display_name
}


/// 一张报销单是否在当前用户的可见范围内。
///
/// 口径 = 部门范围 ∧「仅看本人经手的业务单据」，和工资那边 `scope_payroll` 一致。
/// 这已经是第三个地方抄同一句了（工资 4 个出口共用 `scope_payroll`，报销却散在
/// `list_claims` / `export_claims` / 四个写接口里各写一遍），所以收成两个函数：
/// 读用 `scope_claims`，写用 `claim_visible`。
///
/// 写侧必须和读侧同口径：**看不见的单据不能改/审批/生成凭证**。否则会出现
/// 「列表里看不到，但拿着 id 能操作」——这比起看不到更糟，因为没人会发现。
fn claim_visible(user: &CurrentUser, c: &business::Claim) -> bool {
    user.user.data_scope.allows_dept(&c.dept) && doc_in_scope(user, &c.applicant)
}


/// 报销单列表/导出共用的数据范围过滤
fn scope_claims<'a>(
    user: &CurrentUser,
    rows: &'a [business::Claim],
) -> Vec<&'a business::Claim> {
    rows.iter().filter(|c| claim_visible(user, c)).collect()
}


fn to_item(v: &Voucher) -> VoucherListItem {
    VoucherListItem {
        id: v.id,
        period: period_to_str(v.period),
        date: v.date.format("%Y-%m-%d").to_string(),
        word: v.word.clone(),
        no: v.no,
        voucher_no: v.voucher_no(),
        summary: v.first_summary(),
        debit_total: v.debit_total().fmt_money(),
        credit_total: v.credit_total().fmt_money(),
        status: serde_json::to_value(v.status)
            .ok()
            .and_then(|x| x.as_str().map(String::from))
            .unwrap_or_default(),
        status_label: v.status.label().to_string(),
        prepared_by: v.prepared_by.clone(),
    }
}


/// 从查询参数构造凭证列表条件。`list_vouchers` 与 `list_vouchers_page` **共用**
/// 这一份：分页要显示「共 N 张」和合计，两者必须和列表看到的是同一批数据，
/// 条件一旦各写一份就会漂移（页码算错 / 合计对不上）。
fn voucher_query_from(q: &HashMap<String, String>, user: &CurrentUser) -> VoucherQuery {
    // 落地数据范围：仅本人凭证或按科目区间过滤
    let mut query = VoucherQuery::default().with_data_scope(&user.user);
    query.asc = true;
    if let Some(p) = q.get("period").and_then(|s| parse_period(s)) {
        query.from = Some(p);
        query.to = Some(p);
    }
    if let Some(kw) = q.get("q") {
        let kw = kw.trim().to_string();
        if !kw.is_empty() {
            query.keyword = Some(kw);
        }
    }
    if let Some(st) = q.get("status").and_then(|s| parse_voucher_status(s)) {
        query.status = Some(st);
    }
    query
}


/// 解析银行科目：支持尾号简写。
/// - 输入已是完整科目编码（如 `100201`）：原样返回
/// - 输入是纯数字尾号（如 `01`）：在 `1002*` 科目中查找编码以该尾号结尾的唯一科目，
///   解析为完整编码；无唯一匹配则返回 None（交给后续科目校验报错）
fn normalize_bank_account(chart: &fincore::Chart, account_code: &str) -> Option<String> {
    let trimmed = account_code.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 完整编码：科目表里能查到就直接用
    if chart.get(trimmed).is_some() {
        return Some(trimmed.to_string());
    }
    // 纯数字尾号：在银行科目里按尾号匹配
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut hits: Vec<&str> = Vec::new();
    for a in chart.all() {
        if a.code.starts_with("1002") && a.code.ends_with(trimmed) && a.code.len() > 4 {
            hits.push(&a.code);
        }
    }
    if hits.len() == 1 {
        Some(hits[0].to_string())
    } else {
        None
    }
}


fn default_true() -> bool {
    true
}


#[derive(Deserialize)]
struct AmendReq {
    /// 更正原因（必填）。没有原因的更正等于没更正。
    reason: String,
}


#[derive(Deserialize)]
struct VoidReq {
    /// 缺省 = true（作废）；恢复请显式传 false
    #[serde(default = "default_true")]
    void: bool,
}


/// 红字冲销请求：冲销凭证落到的期间与日期
#[derive(Deserialize, Default)]
struct ReverseVoucherReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
}


/// 断号重排请求：period + word
#[derive(Deserialize, Default)]
struct RenumberVoucherReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub word: String,
}


// ---------------------------------------------------------------------------
// 凭证附件（上传/下载/删除；数据超过阈值落 .attachments 目录，否则入库）
// ---------------------------------------------------------------------------

fn attachment_json(a: &findb::attach::Attachment) -> serde_json::Value {
    json!({
        "id": a.id,
        "voucher_id": a.voucher_id,
        "name": a.name,
        "kind": a.kind,
        "size": a.size,
        "size_text": a.size_text(),
        "is_image": a.is_image(),
        "added_by": a.added_by,
        "added_at": a.added_at,
    })
}


// ---------------------------------------------------------------------------
// 发票管理
// ---------------------------------------------------------------------------

/// 发票 → 响应 JSON（金额已格式化）
fn invoice_json(inv: &findb::invoices::Invoice) -> serde_json::Value {
    json!({
        "id": inv.id,
        "kind": inv.kind,
        "code": inv.code,
        "number": inv.number,
        "date": inv.date,
        "buyer": inv.buyer,
        "seller": inv.seller,
        "amount_tax": inv.amount_tax.fmt_money(),
        "amount": inv.amount.fmt_money(),
        "tax": inv.tax.fmt_money(),
        "tax_rate": inv.tax_rate,
        "status": inv.status,
        "status_label": inv.status_label(),
        "memo": inv.memo,
        "attach_id": inv.attach_id,
        "created_by": inv.created_by,
    })
}


fn invoice_from_req(r: &InvoiceReq) -> Result<findb::invoices::Invoice, AppError> {
    Ok(findb::invoices::Invoice {
        id: r.id,
        kind: if r.kind.is_empty() {
            "in".to_string()
        } else {
            r.kind.clone()
        },
        code: r.code.clone(),
        number: r.number.clone(),
        date: r.date.clone(),
        buyer: r.buyer.clone(),
        seller: r.seller.clone(),
        amount_tax: parse_money_checked(&r.amount_tax)?,
        amount: parse_money_checked(&r.amount)?,
        tax: parse_money_checked(&r.tax)?,
        tax_rate: r.tax_rate.clone(),
        status: if r.status.is_empty() {
            "pending".to_string()
        } else {
            r.status.clone()
        },
        memo: r.memo.clone(),
        attach_id: 0,
        created_by: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
    })
}


#[derive(Deserialize)]
struct InvoiceFromPo {
    po_id: i64,
}


#[derive(Deserialize)]
struct InvoiceFromSo {
    so_id: i64,
}


/// 发票列表查询参数
#[derive(Deserialize, Default)]
pub struct InvoiceListQuery {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub keyword: String,
}


// ---------------------------------------------------------------------------
// 数据导入（其他软件 / CSV / Excel）
// ---------------------------------------------------------------------------

/// 解析 base64（纯标准 base64 字母表，无依赖实现）
fn b64_decode(s: &str) -> Result<Vec<u8>, AppError> {
    let s = s.trim();
    // 兼容 data URL 前缀（data:application/...;base64,xxx）
    let s = s.split(',').next_back().unwrap_or(s);
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &b in s.as_bytes() {
        let v = match b {
            b'A'..=b'Z' => (b - b'A') as u32,
            b'a'..=b'z' => (b - b'a' + 26) as u32,
            b'0'..=b'9' => (b - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\r' | b'\n' | b' ' => continue,
            _ => return Err(AppError::bad_request("文件不是合法的 base64 编码")),
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
}


/// 导入 per-kind 鉴权：页面入口宽（NAV Report），动作按 kind 严管（迁移操作者=对应岗位）
fn import_perm(kind: &str) -> Result<Perm, AppError> {
    Ok(match kind {
        "voucher" | "begin" | "arap_opening" => Perm::VoucherNew,
        "account" => Perm::AccountEdit,
        "aux" => Perm::AuxEdit,
        "item" | "opening_stock" => Perm::Warehouse,
        _ => return Err(AppError::bad_request("未知导入类型 kind")),
    })
}


/// query 里的 period（YYYYMM 或 YYYY-MM），缺省用会话当前期间。
///
/// 不传就默认当前期间，是有意的：客户视图是「现在谁欠我钱」，
/// 而不是一个历史查询工具。要看历史用 ?period= 显式指定。
fn period_from_query(state: &WebState, user: &CurrentUser, raw: Option<&String>) -> Period {
    let cur = current_period(state, user);
    match raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => cur,
        Some(s) => Period::from_ymm_checked(period_checked_digits(s))
            .or_else(|_| Period::from_ymm_checked(cur.ymm()))
            .unwrap_or(cur),
    }
}


/// `2026-01` / `202601` 都接受（前端两种都可能传来）。
fn period_checked_digits(s: &str) -> i32 {
    s.replace('-', "").parse().unwrap_or(0)
}


// ---------------------------------------------------------------------------
// 账簿 / 报表
// ---------------------------------------------------------------------------

/// 账簿查询参数（明细/总账/日记账共用）
fn ledger_query_from<'a>(
    state: &WebState,
    user: &'a CurrentUser,
    q: &HashMap<String, String>,
) -> Result<(&'a findb::Db, fincore::Chart, LedgerQuery), AppError> {
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少科目编码参数 code"));
    }
    let from = period_param(q.get("from"), current_period(state, user))?;
    let to = period_param(q.get("to"), from)?;
    if from > to {
        return Err(AppError::bad_request(format!(
            "起始期间不能晚于结束期间：{} > {}",
            period_to_str(from),
            period_to_str(to)
        )));
    }
    let include_children = q
        .get("include_children")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    // H-3 定案：缺省即"仅已记账"，与余额表/报表口径一致
    let posted_only = q
        .get("posted_only")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let db = &user.db;
    let chart = accounts::chart(&db)?;
    if !user.user.can_see_account(&code) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let lq = LedgerQuery {
        code,
        include_children,
        aux: None,
        from,
        to,
        posted_only,
        prepared_by: None,
        code_from: None,
        code_to: None,
    }
    .with_user_scope(&user.user);
    Ok((db, chart, lq))
}


/// 计算科目余额表（供 JSON / 打印 / 导出复用）
fn trial_balance_data(
    state: &WebState,
    user: &CurrentUser,
    q: &HashMap<String, String>,
) -> Result<
    (
        Vec<fincore::balance::BalanceRow>,
        fincore::balance::TrialBalance,
    ),
    AppError,
> {
    let db = &user.db;
    let start = db.options().start_period;
    // 与其它报表同一套校验：区间敲错 / 倒置都返回 400，不静默换一个区间
    // （理由与实测数据见 `state::period_param`）
    let (from, to) = period_range_param(q, start, current_period(state, user))?;
    let bq = BalanceQuery::range(from, to).with_user_scope(&user.user);
    let snap = BalanceSnapshot::load(&db, &bq)?;
    let chart = accounts::chart(&db)?;
    let rows = snap.account_table(&chart, &bq);
    let totals = snap.trial_balance(&chart);
    Ok((rows, totals))
}


// ---------------------------------------------------------------------------
// 数据导出（CSV，需 Export 权限；带 BOM 便于 Excel 直接打开中文）
// ---------------------------------------------------------------------------

/// CSV 单元格转义：除了引号/逗号/换行，还要防 Excel 公式注入。
///
/// 用户可控文本（摘要、客户名、科目名）以 `= + @` 开头时，Excel 会当公式执行；
/// `-` 只有后面不是合法数字时才危险（负数金额必须保持数字形态，不能变成文本）。
fn csv_escape(s: &str) -> String {
    let dangerous = match s.chars().next() {
        Some('=') | Some('+') | Some('@') | Some('\t') | Some('\r') => true,
        Some('-') => s.replace(',', "").parse::<f64>().is_err(),
        _ => false,
    };
    let s = if dangerous {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}


fn csv_response(filename: &str, rows: Vec<Vec<String>>) -> Response {
    let mut out = String::from("\u{feff}");
    for r in &rows {
        out.push_str(&r.iter().map(|c| csv_escape(c)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    let mut resp = out.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(v) =
        axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}


// ---------------------------------------------------------------------------
// 报表渲染
// ---------------------------------------------------------------------------

fn trial_balance_html(
    company: &str,
    q: &HashMap<String, String>,
    rows: &[fincore::balance::BalanceRow],
    totals: &fincore::balance::TrialBalance,
    po: &fincore::PrintOptions,
    can_edit: bool,
) -> String {
    let from = q.get("from").cloned().unwrap_or_default();
    let to = q.get("to").cloned().unwrap_or_default();
    let mut body = String::new();
    for r in rows {
        let (b_dir, b_amt) = r.begin_dir_amount();
        let (e_dir, e_amt) = r.end_dir_amount();
        body.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td class='r'>{} {}</td>\
             <td class='r'>{}</td><td class='r'>{}</td>\
             <td class='r'>{} {}</td><td class='r'>{} {}</td></tr>",
            html_escape(&r.account_code),
            html_escape(&r.account_name),
            b_dir.label(),
            b_amt.fmt_money(),
            r.debit.fmt_money(),
            r.credit.fmt_money(),
            e_dir.label(),
            e_amt.fmt_money(),
            r.ytd_debit.fmt_money(),
            r.ytd_credit.fmt_money(),
        ));
    }
    // 表体自己拼，壳用 `report_html::page_shell` —— 以前这里是一整套独立 `<style>`，
    // 于是「账套里设了页码/横向/偏移，科目余额表不跟着变」成了长期没人发现的分叉。
    let table = format!(
        "<table><thead><tr>\
         <th>科目编码</th><th>科目名称</th><th>期初</th>\
         <th>本期借方</th><th>本期贷方</th><th>期末</th><th>本年累计</th>\
         </tr></thead><tbody>{}</tbody>\
         <tfoot><tr><td colspan='2'>合计</td>\
         <td class='r'>借 {} / 贷 {}</td>\
         <td class='r'>{}</td><td class='r'>{}</td>\
         <td class='r'>借 {} / 贷 {}</td>\
         <td class='r'>—</td></tr></tfoot></table>",
        body,
        totals.begin_debit.fmt_money(),
        totals.begin_credit.fmt_money(),
        totals.period_debit.fmt_money(),
        totals.period_credit.fmt_money(),
        totals.end_debit.fmt_money(),
        totals.end_credit.fmt_money(),
    );
    crate::report_html::page_shell(
        "科目余额表",
        company,
        &format!("期间：{} 至 {}", from, to),
        "元",
        &table,
        7,
        po,
        can_edit,
    )
}


fn trial_balance_csv(rows: &[fincore::balance::BalanceRow]) -> String {
    let mut s = String::from("科目编码,科目名称,期初方向,期初余额,本期借方,本期贷方,期末方向,期末余额,本年累计借方,本年累计贷方\n");
    for r in rows {
        let (b_dir, b_amt) = r.begin_dir_amount();
        let (e_dir, e_amt) = r.end_dir_amount();
        s.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{}\n",
            csv_escape(&r.account_code),
            csv_escape(&r.account_name),
            b_dir.label(),
            b_amt.fmt_plain(),
            r.debit.fmt_plain(),
            r.credit.fmt_plain(),
            e_dir.label(),
            e_amt.fmt_plain(),
            r.ytd_debit.fmt_plain(),
            r.ytd_credit.fmt_plain(),
        ));
    }
    s
}


fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}


// ═══════════════════════════════════════════════════════════════
// 税务申报表（增值税一般纳税人）
// ═══════════════════════════════════════════════════════════════
//
// 边界：只取数、算表、导出可手工填报的表。**不做**发票查验、不做电子税务局网报、
// 不碰税控设备——网报要 CA 证书与税控盘，是另一套基础设施。
//
// 报表结构的依据写在 findb::taxdecl 的模块注释里（数据来源、法定口径、哪些项
// 刻意不算）。这里只做 HTTP 层：取期、拼 CSV、进 JSON。

/// 申报表权限：与财务报表同级（`Perm::Report`），但导出单独要求「导出」权限
///
/// 分两级是有意的：看一眼本期数不落纸，导出 CSV 才是数据出系统。
const _TAX_DOC: &str = "税务申报表";



fn parse_voucher_status(s: &str) -> Option<fincore::VoucherStatus> {
    match s {
        "draft" => Some(fincore::VoucherStatus::Draft),
        "audited" => Some(fincore::VoucherStatus::Audited),
        "posted" => Some(fincore::VoucherStatus::Posted),
        "void" => Some(fincore::VoucherStatus::Void),
        _ => None,
    }
}


// ---------------------------------------------------------------------------
// 自定义报表（UFO 公式）
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct CustomLineReq {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub indent: u8,
    #[serde(default)]
    pub formulas: Vec<String>,
    #[serde(default)]
    pub bold: bool,
}


#[derive(Deserialize, Default)]
struct CustomReportReq {
    #[serde(default)]
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub lines: Vec<CustomLineReq>,
}


fn custom_json(r: &findb::mgmt::CustomReport) -> serde_json::Value {
    json!({
        "key": r.key,
        "name": r.name,
        "columns": r.columns,
        "lines": r.lines.iter().map(|l| json!({
            "name": l.name, "indent": l.indent, "formulas": l.formulas, "bold": l.bold,
        })).collect::<Vec<_>>(),
    })
}


// ---- 存货核算：成本调整 ----

#[derive(Deserialize)]
struct StockAdjustReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub item: String,
    #[serde(default)]
    pub warehouse: String,
    /// 调整数量（正=盘盈/调增，负=盘亏/调减）。**建账与盘盈必填**：
    /// 只给金额不给数量会造出「数量 0、金额 > 0」的幽灵存货。
    #[serde(default)]
    pub qty: String,
    /// 调整金额（正=调增，负=调减），十进制字符串
    pub delta: String,
    #[serde(default)]
    pub memo: String,
}


// ---- 库存深度：序列号 / 多单位 / 账龄 / ABC / 组装拆卸 / 分仓库 ----

#[derive(Deserialize)]
struct SerialInReq {
    pub item: String,
    pub serials: Vec<String>,
    #[serde(default)]
    pub batch_no: String,
    #[serde(default)]
    pub date: String,
}


#[derive(Deserialize)]
struct SerialOutReq {
    pub serials: Vec<String>,
    #[serde(default)]
    pub date: String,
}


#[derive(Deserialize)]
struct UnitReq {
    pub item: String,
    #[serde(default)]
    pub base_unit: String,
    #[serde(default)]
    pub alt_unit: String,
    #[serde(default)]
    pub factor: String,
}


#[derive(Deserialize)]
struct AssembleReq {
    pub parent: String,
    pub children: Vec<(String, String)>,
    #[serde(default)]
    pub memo: String,
    #[serde(default)]
    pub date: String,
}


// ---- 采购/销售深度：暂估 / 对账 / 配额 / 订单变更 ----

#[derive(Deserialize)]
struct EstimateReq {
    pub po_id: i64,
    #[serde(default)]
    pub period: i32,
    pub item: String,
    #[serde(default)]
    pub est_amount: String,
}


#[derive(Deserialize)]
struct QuotaReq {
    #[serde(default)]
    pub period: i32,
    pub supplier: String,
    pub item: String,
    #[serde(default)]
    pub quota_qty: String,
}


#[derive(Deserialize)]
struct ReceiptReq {
    pub po_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub qty: String,
    #[serde(default)]
    pub memo: String,
    /// 仓库编码（留空 = 默认仓）
    #[serde(default)]
    pub warehouse: String,
    /// 收货针对哪一行（物料编码）。留空 = 整单收货（单行订单 / 旧客户端）。
    ///
    /// 多行订单**必须**指定：否则这批货属于哪一行说不清，金额只能按订购量占比
    /// 折算，而那个折算对部分收货是错的（两张行各 100 件、含税单价 11，实收 100 件
    /// 全是第一行，折算摊成 550 而实际 1100），暂估封顶额度也会跟着错。
    #[serde(default)]
    pub item_code: String,
}


#[derive(Deserialize)]
struct PaymentReq {
    pub po_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}


#[derive(Deserialize)]
struct ShipmentReq {
    pub so_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub qty: String,
    #[serde(default)]
    pub memo: String,
    /// 仓库编码（留空 = 默认仓）
    #[serde(default)]
    pub warehouse: String,
}


#[derive(Deserialize)]
struct SoPaymentReq {
    pub so_id: i64,
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}


#[derive(Deserialize)]
struct WfTemplateApplyReq {
    key: String,
    biz_type: String,
}


// ---------------- 单据套打（订单 / 收付款单）：字段白名单 + 批量紧凑分页 ----------------

/// 从查询串取打印字段：`fields=no,date,...` 白名单（空=全显）；`pack=0` 一单一页
fn doc_fields_from_q(q: &HashMap<String, String>) -> findb::printform::DocPrintFields {
    let mut f =
        findb::printform::fields_from_tokens(q.get("fields").map(|s| s.as_str()).unwrap_or(""));
    if q.get("pack").map(|s| s.as_str()) == Some("0") {
        f.pack = false;
    }
    f
}


/// 打印纸张：`a4` / `a5` / `third` / 自定义 `宽x高`（mm，50..=600），非法回退 a4
fn doc_size_from_q(q: &HashMap<String, String>) -> String {
    let s = q
        .get("size")
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if s == "a4" || s == "a5" || s == "third" {
        return s;
    }
    if let Some((w, h)) = s
        .split_once('x')
        .and_then(|(a, b)| Some((a.parse::<i32>().ok()?, b.parse::<i32>().ok()?)))
    {
        if (50..=600).contains(&w) && (50..=600).contains(&h) {
            return format!("{w}x{h}");
        }
    }
    "a4".to_string()
}


/// `ids=1,2,3`（去重保序）；缺省返回空 = 由调用方按期间/全部取数
fn print_ids(q: &HashMap<String, String>) -> Vec<i64> {
    let mut ids: Vec<i64> = q
        .get("ids")
        .map(|s| {
            s.split(',')
                .filter_map(|x| x.trim().parse::<i64>().ok())
                .collect()
        })
        .unwrap_or_default();
    ids.sort_unstable();
    ids.dedup();
    ids
}


fn so_to_print(o: findb::scm::SalesOrder) -> findb::printform::OrderPrint {
    findb::printform::OrderPrint {
        title: "销售订单".to_string(),
        no: o.no,
        date: o.date.format("%Y-%m-%d").to_string(),
        status: o.status.label().to_string(),
        party_label: "客户".to_string(),
        party: format!("{} {}", o.customer_code, o.customer_name),
        prepared_by: o.prepared_by,
        memo: o.memo,
        rows: o
            .lines
            .into_iter()
            .map(|l| findb::printform::OrderPrintRow {
                code: l.item_code,
                name: l.item_name,
                qty: l.qty_ordered,
                price: l.unit_price,
                rate: l.tax_rate,
                amount: l.amount,
                tax: l.tax_amount,
                memo: l.memo,
            })
            .collect(),
        amount_total: o.total_amount,
        tax_total: o.total_tax,
    }
}


fn po_to_print(o: findb::scm::PurchaseOrder) -> findb::printform::OrderPrint {
    findb::printform::OrderPrint {
        title: "采购订单".to_string(),
        no: o.no,
        date: o.date.format("%Y-%m-%d").to_string(),
        status: o.status.label().to_string(),
        party_label: "供应商".to_string(),
        party: format!("{} {}", o.supplier_code, o.supplier_name),
        prepared_by: o.prepared_by,
        memo: o.memo,
        rows: o
            .lines
            .into_iter()
            .map(|l| findb::printform::OrderPrintRow {
                code: l.item_code,
                name: l.item_name,
                qty: l.qty_ordered,
                price: l.unit_price,
                rate: l.tax_rate,
                amount: l.amount,
                tax: l.tax_amount,
                memo: l.memo,
            })
            .collect(),
        amount_total: o.total_amount,
        tax_total: o.total_tax,
    }
}


fn receipt_to_print(d: findb::receipt::ReceiptDoc) -> findb::printform::ReceiptPrint {
    findb::printform::ReceiptPrint {
        no: d.no,
        date: d.date.format("%Y-%m-%d").to_string(),
        kind_label: if d.kind == "receipt" {
            "收款"
        } else {
            "付款"
        }
        .to_string(),
        fund: d.fund_account,
        party: d.party,
        amount: d.amount,
        memo: d.memo,
        voucher_no: d
            .voucher_id
            .map(|v| format!("记-{v:04}"))
            .unwrap_or_default(),
    }
}


// ---------------- 订单 CRUD（销售/采购，对标金蝶订单流程） ----------------

#[derive(Deserialize)]
struct OrderLineInput {
    #[serde(default)]
    item_code: String,
    #[serde(default)]
    item_name: String,
    #[serde(default)]
    qty_ordered: String,
    #[serde(default)]
    unit_price: String,
    #[serde(default)]
    tax_rate: String,
    #[serde(default)]
    qty_shipped: String,
    #[serde(default)]
    qty_received: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct SoInput {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    customer_code: String,
    #[serde(default)]
    customer_name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    lines: Vec<OrderLineInput>,
}


#[derive(Deserialize)]
struct PoInput {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    supplier_code: String,
    #[serde(default)]
    supplier_name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    lines: Vec<OrderLineInput>,
}


#[derive(Deserialize)]
struct OrderTransitionReq {
    status: String,
}


/// 订单日期：缺省今天（非法值回退今天，不让单据失败）
fn order_date(s: &str) -> NaiveDate {
    if s.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
            .unwrap_or_else(|_| chrono::Local::now().date_naive())
    }
}


fn so_status_parse(s: &str) -> Result<findb::scm::SoStatus, AppError> {
    findb::scm::status_from::<findb::scm::SoStatus>(s.trim()).ok_or_else(|| {
        AppError::bad_request("订单状态取值：Draft/Confirmed/PartialShip/Completed/Cancelled")
    })
}


fn po_status_parse(s: &str) -> Result<findb::scm::PoStatus, AppError> {
    findb::scm::status_from::<findb::scm::PoStatus>(s.trim()).ok_or_else(|| {
        AppError::bad_request("订单状态取值：Draft/Confirmed/PartialIn/Completed/Cancelled")
    })
}


/// 工作流闸门的判定结果
enum WfGate {
    /// 无流程，或已到「通过」终态 → 调用方照常执行原业务动作
    Continue,
    /// 未到终态，已推进到某节点 → 调用方只推进流程、**不**改业务状态
    Pending(String),
    /// 本次动作被驳回 → 调用方不要改业务状态
    Rejected,
}


/// 工作流闸门。收成函数是为了让「哪种单据走工作流」只有这一处实现。
///
/// 早先 4 个调用点各自抄一遍 `match intercept(...)? { Pending => …,
/// Final{false} => …, _ => {} }`，新接单据时很容易只写业务动作、忘了插这一段
/// ——结果是配了流程却永远不生效，而且**不报任何错**，比没有流程更坏。
///
/// `doc_label` 只进日志，让人能从审计日志看出卡在哪张单据的哪个节点。
fn workflow_gate(
    db: &findb::Db,
    user: &CurrentUser,
    biz_type: &str,
    id: i64,
    doc_label: &str,
) -> Result<WfGate, AppError> {
    use findb::workflow::Gate;
    match findb::workflow::intercept(db, biz_type, id, &user.user, true, "")? {
        Gate::Pending { next } => {
            db.log(
                user.username(),
                "审批",
                "工作流节点",
                &format!("{doc_label}#{id} → {next}"),
            )?;
            Ok(WfGate::Pending(next))
        }
        Gate::Final { approved: false } => Ok(WfGate::Rejected),
        _ => Ok(WfGate::Continue),
    }
}


#[derive(Deserialize)]
struct NoticeReq {
    qty: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}


// ---- 工艺路线 / 报工 ----

#[derive(Deserialize)]
struct RoutingOpDto {
    #[serde(default)]
    pub seq: i32,
    pub op_code: String,
    pub op_name: String,
    #[serde(default)]
    pub work_center: String,
    #[serde(default)]
    pub std_hours: String,
    #[serde(default)]
    pub rate: String,
    /// 工序检验点（完工前需录工序检验单）
    #[serde(default)]
    pub qc_required: bool,
}


#[derive(Deserialize)]
struct OpReportReq {
    pub op_id: i64,
    #[serde(default)]
    pub qty: String,
    #[serde(default)]
    pub hours: String,
}


#[derive(Deserialize)]
struct OpFinishReq {
    pub op_id: i64,
}


#[derive(Deserialize)]
struct MrpDemandDto {
    pub item_code: String,
    #[serde(default)]
    pub qty: String,
    #[serde(default)]
    pub source: String,
}


#[derive(Deserialize)]
struct MrpRunReq {
    #[serde(default)]
    pub demands: Vec<MrpDemandDto>,
    /// 若为 true 且 demands 为空，则从已确认销售订单收集需求
    #[serde(default)]
    pub from_sales: bool,
}


// ---- 库存作业：形态转换 / 质检 / 低库存预警（对标金蝶） ----

#[derive(Deserialize)]
struct FormConvertReq {
    from_item: String,
    to_item: String,
    qty: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct QcReq {
    po_id: i64,
    qty_insp: String,
    #[serde(default)]
    qty_fail: String,
    #[serde(default)]
    inspector: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}


// ---- 生产订单：下达 / 开工 / 领料 / 完工入库 + BOM（工厂链「业务单据同步凭证」） ----

#[derive(Deserialize)]
struct OutsourceFeeReq {
    amount: String,
    #[serde(default)]
    date: String,
}


#[derive(Deserialize)]
struct ProdCreateReq {
    item_code: String,
    #[serde(default)]
    qty: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    work_center: String,
    /// inhouse（默认）/ outsourcing 委外
    #[serde(default)]
    kind: String,
    #[serde(default)]
    supplier_code: String,
    #[serde(default)]
    supplier_name: String,
}


fn prod_act_date(s: &str) -> chrono::NaiveDate {
    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .unwrap_or_else(|_| chrono::Local::now().date_naive())
}


// ---- MPS 主生产计划 / 粗排 / 细排（链6） ----

#[derive(Deserialize)]
struct MpsRunReq {
    #[serde(default)]
    pub demands: Vec<MrpDemandDto>,
    /// true = 合并已确认销售订单未发量（与手工行并集）
    #[serde(default)]
    pub from_sales: bool,
    /// 建议交期（默认今天 +7）
    #[serde(default)]
    pub due_date: String,
}


#[derive(Deserialize)]
struct MpsConvertReq {
    /// 空 = 用行净算计划量
    #[serde(default)]
    qty: String,
}


#[derive(Deserialize)]
struct RoughReq {
    /// 日产能（件/日），空 = 10
    #[serde(default)]
    daily_qty: String,
    #[serde(default)]
    period: i32,
}


#[derive(Deserialize)]
struct SchedItem {
    id: i64,
    #[serde(default)]
    start: String,
    #[serde(default)]
    end: String,
}


#[derive(Deserialize)]
struct ScheduleReq {
    items: Vec<SchedItem>,
}


#[derive(Deserialize)]
struct ProdUpdateReq {
    #[serde(default)]
    qty: String,
    /// 缺省=不变；传空串=清空计划日期
    #[serde(default)]
    plan_start: Option<String>,
    #[serde(default)]
    plan_end: Option<String>,
    /// 缺省=不变
    #[serde(default)]
    memo: Option<String>,
}


#[derive(Deserialize)]
struct ProdQcReq {
    qty_insp: String,
    #[serde(default)]
    qty_fail: String,
    #[serde(default)]
    disposition: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct ProdActReq {
    #[serde(default)]
    date: String,
    #[serde(default)]
    qty: String,
}


#[derive(Deserialize)]
struct BomReq {
    parent: String,
    #[serde(default)]
    children: Vec<BomChildReq>,
}


#[derive(Deserialize)]
struct BomChildReq {
    child: String,
    #[serde(default)]
    qty: String,
    #[serde(default)]
    loss: String,
}


#[derive(Deserialize)]
struct BVersionReq {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub is_current: bool,
    #[serde(default)]
    pub memo: String,
}


#[derive(Deserialize)]
struct BCopyReq {
    pub from: String,
    pub to: String,
}


#[derive(Deserialize)]
struct ApprovalStartReq {
    pub biz_kind: String,
    pub biz_id: i64,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub approvers: Vec<String>,
}


#[derive(Deserialize)]
struct ApprovalActReq {
    pub approve: bool,
    #[serde(default)]
    pub comment: String,
}


#[derive(Deserialize)]
struct NoteReq {
    #[serde(default)]
    pub id: i64,
    pub report_key: String,
    pub period: i32,
    #[serde(default)]
    pub seq: i32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub content: String,
}


#[derive(Deserialize)]
struct ArchiveReq {
    pub period: i32,
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub file_no: String,
    pub payload: String,
}


// ---------------------------------------------------------------------------
// 资金：票据 / 融资 / 资金日报 / 资金预测
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct BillReq {
    #[serde(default)]
    pub id: i64,
    pub kind: String,
    pub no: String,
    pub period: i32,
    pub issue_date: String,
    pub due_date: String,
    #[serde(default)]
    pub counterpart: String,
    #[serde(default)]
    pub bank: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub memo: String,
}


#[derive(Deserialize)]
struct BillTransitionReq {
    pub status: String,
    pub date: String,
}


#[derive(Deserialize)]
struct LoanReq {
    #[serde(default)]
    pub id: i64,
    pub kind: String,
    pub no: String,
    #[serde(default)]
    pub bank: String,
    #[serde(default)]
    pub principal: String,
    #[serde(default)]
    pub rate_pct: String,
    pub start_date: String,
    pub end_date: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub memo: String,
}


#[derive(Deserialize)]
struct LoanSettleReq {
    #[serde(default)]
    date: Option<String>,
}


#[derive(Deserialize)]
struct CashCountReq {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    date: String,
    #[serde(default)]
    account_code: String,
    #[serde(default)]
    counted: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct DayClearReq {
    #[serde(default)]
    account_code: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    clear: bool,
}


#[derive(Deserialize)]
struct CashShiftReq {
    #[serde(default)]
    date: String,
    #[serde(default)]
    to_user: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct CheckReq {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    no: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    bank_account: String,
    #[serde(default)]
    payee: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    issued_date: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct AdvanceReq {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    no: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    employee: String,
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    pay_account: String,
    #[serde(default)]
    expense_account: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct AdvanceSettleReq {
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    expense_account: String,
    #[serde(default)]
    expense_amount: String,
}


fn opt_body_date(body: &axum::body::Bytes) -> Result<Option<chrono::NaiveDate>, AppError> {
    if body.is_empty() {
        return Ok(None);
    }
    #[derive(Deserialize)]
    struct D {
        #[serde(default)]
        date: Option<String>,
    }
    let d: D = serde_json::from_slice(body)
        .map_err(|_| AppError::bad_request("请求体日期格式应为 {\"date\":\"YYYY-MM-DD\"}"))?;
    match d.date {
        Some(s) => Ok(Some(
            chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
        )),
        None => Ok(None),
    }
}


#[derive(Deserialize)]
struct ReceiptCreateReq {
    #[serde(default)]
    date: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    fund_account: String,
    #[serde(default)]
    party: String,
    #[serde(default)]
    bank: String,
    #[serde(default)]
    cross_party: String,
    #[serde(default)]
    cross_amount: String,
    #[serde(default)]
    amount: String,
    #[serde(default)]
    memo: String,
}


// ---------------- 收付款单审核流 + 存货盘点 ----------------

/// 资金类单据（收付款单 / 资金调拨单）的「审核」判据：**VoucherAudit 或 VoucherPost 二者其一**。
///
/// 原来只认 `VoucherAudit`，于是「只有会计 + 出纳、没有财务主管」的小微企业
/// **永远把收付款单变不成凭证** —— 出纳录单，会计想审，但 `Role::Accountant`
/// 按三权分离刻意不带 `VoucherAudit`（制单/审核/记账要分开），于是两个人谁也
/// 审不了。这条死路是在 2026-09-29 补出纳日常 E2E 时实测撞出来的。
///
/// 为什么放开给 `VoucherPost` 是安全的、而且才是对的控制：
/// · 收付款单 / 调拨单的凭证是**系统按单据自动生成**的（不是人手敲的），三方分离
///   针对的是「自己录的凭证自己审自己记」；单据来源不同，可审的人也就该不同。
/// · 真正的控制是「**要记账的那个人看过这张单**」。`VoucherPost` 恰好就是记账权
///   持有者 —— 会计审完直接记；出纳（只有 VoucherNew/Edit/Sign）自己审不了自己
///   提的单，两头仍然分开。
/// · 不动 `enable_audit`：手工凭证的审核闸门原样保留，两人公司在建账向导里选
///   「不启用审核环节」即可，两条路径互不影响。
///
/// 「审核」与「撤审」**必须用同一个判据** —— 只给"审"放开而不给"撤审"，会出现
/// 「会计能审但撤不回来」，那比一开始就没有还糟。
fn require_fund_doc_gate(user: &CurrentUser) -> Result<(), AppError> {
    if user.can(Perm::VoucherAudit) || user.can(Perm::VoucherPost) {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "没有「凭证审核」或「记账」权限，不能审核该单据",
        ))
    }
}


// ---------------- 资金调拨（账户间转账） ----------------

#[derive(Deserialize)]
struct TransferCreateReq {
    #[serde(default)]
    date: String,
    pub from_account: String,
    pub to_account: String,
    #[serde(default)]
    pub bank: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}


#[derive(Deserialize)]
struct CountLineInput {
    #[serde(default)]
    item: String,
    #[serde(default)]
    batch_no: String,
    #[serde(default)]
    count_qty: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct CountReq {
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    warehouse: String,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    lines: Vec<CountLineInput>,
}


// ---------------- 批次与库位（对标金蝶批号/保质期/货位） ----------------

#[derive(Deserialize)]
struct BatchRegisterReq {
    #[serde(default)]
    item: String,
    #[serde(default)]
    batch_no: String,
    #[serde(default)]
    production_date: String,
    #[serde(default)]
    warehouse: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    qty: String,
    #[serde(default)]
    direction: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct TransferReq {
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    item: String,
    #[serde(default)]
    batch_no: String,
    #[serde(default)]
    from_warehouse: String,
    #[serde(default)]
    to_warehouse: String,
    #[serde(default)]
    qty: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct FefoReq {
    #[serde(default)]
    item: String,
    #[serde(default)]
    qty: String,
}


#[derive(Deserialize)]
struct LocationReq {
    #[serde(default)]
    code: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    memo: String,
}


// ---------------------------------------------------------------------------
// 成本：计价配置 + 期末结价
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CostMethodReq {
    pub item: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub standard_cost: String,
}


#[derive(Deserialize)]
struct SalesCostReq {
    /// yyyymm，0/缺省 = 当前期间
    #[serde(default)]
    period: i32,
    /// 计价方法 code（moving_average/fifo/…，缺省移动加权）
    #[serde(default)]
    method: String,
    /// 结转日期，缺省 = 期间末日
    #[serde(default)]
    date: String,
}


// ---------------------------------------------------------------------------
// 固定资产（与桌面端对齐：卡片 / 折旧计划 / 计提 / 清理）
// ---------------------------------------------------------------------------

fn asset_json(a: &findb::assets::Asset) -> serde_json::Value {
    json!({
        "id": a.id,
        "code": a.code,
        "name": a.name,
        "category": a.category,
        "spec": a.spec,
        "dept": a.dept,
        "asset_account": a.asset_account,
        "dep_account": a.dep_account,
        "expense_account": a.expense_account,
        "original_value": a.original_value.fmt_money(),
        "residual_rate": (a.residual_rate * Money::from_i64(100)).fmt_plain(),
        "life_months": a.life_months,
        "method": a.method.code(),
        "method_label": a.method.label(),
        "start_period": period_to_str(a.start_period),
        "disposed_period": a.disposed_period.map(period_to_str),
        "dispose_amount": a.dispose_amount.map(|m| m.fmt_money()),
        "status": a.status.code(),
        "status_label": a.status.label(),
        "voucher_id": a.voucher_id,
        "memo": a.memo,
    })
}


#[derive(Deserialize)]
struct AssetReq {
    #[serde(default)]
    pub id: i64,
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub spec: String,
    #[serde(default)]
    pub dept: String,
    pub asset_account: String,
    pub dep_account: String,
    pub expense_account: String,
    pub original_value: String,
    /// 残值率按百分数录入（5 = 5%）
    #[serde(default)]
    pub residual_rate: String,
    pub life_months: i32,
    #[serde(default)]
    pub method: String,
    /// 启用期间 YYYYMM
    pub start_period: i32,
    #[serde(default)]
    pub memo: String,
}


#[derive(Deserialize, Default)]
struct AssetDisposeReq {
    pub ymm: i32,
    #[serde(default)]
    pub amount: String,
}


fn asset_from_req(r: &AssetReq) -> Result<findb::assets::Asset, AppError> {
    if r.code.trim().is_empty() {
        return Err(AppError::bad_request("资产编码不能为空"));
    }
    if r.name.trim().is_empty() {
        return Err(AppError::bad_request("资产名称不能为空"));
    }
    let start = period_checked(r.start_period)?;
    let original = parse_money_checked(&r.original_value)?;
    if !original.is_positive() {
        return Err(AppError::bad_request("资产原值必须大于 0"));
    }
    // 残值率缺省 = 0（无残值），不是 5%：字段按百分数录入，0 本身是合法值，
    // 漏传表单若被当成 5% 会凭空抬高残值、低估折旧。取 0 是会计中性的默认值；
    // 确有残值的资产必须显式录入残值率。
    let rate = if r.residual_rate.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&r.residual_rate)?
            .checked_div(rust_decimal::Decimal::from(100))
            .expect("字面量 100 非零")
    };
    if rate.is_negative() || rate > Money::ONE {
        return Err(AppError::bad_request("残值率必须在 0% ~ 100% 之间"));
    }
    Ok(findb::assets::Asset {
        id: r.id,
        code: r.code.trim().to_string(),
        name: r.name.trim().to_string(),
        category: r.category.trim().to_string(),
        spec: r.spec.trim().to_string(),
        dept: r.dept.trim().to_string(),
        asset_account: r.asset_account.trim().to_string(),
        dep_account: r.dep_account.trim().to_string(),
        expense_account: r.expense_account.trim().to_string(),
        original_value: original,
        residual_rate: rate,
        life_months: r.life_months,
        method: fincore::engine::depreciation::DepMethod::parse(&r.method),
        start_period: start,
        disposed_period: None,
        dispose_amount: None,
        status: findb::assets::AssetStatus::InUse,
        voucher_id: None,
        memo: r.memo.trim().to_string(),
    })
}


#[derive(Deserialize)]
struct AssetImpairReq {
    #[serde(default)]
    period: i32,
    amount: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct AssetCountLineReq {
    asset_id: i64,
    /// 缺省 = true（盘实）
    #[serde(default)]
    found: Option<bool>,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct AssetCountReq {
    #[serde(default)]
    period: i32,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
    lines: Vec<AssetCountLineReq>,
}


// ---------------------------------------------------------------------------
// 银行对账（与桌面端对齐）
// ---------------------------------------------------------------------------

fn stmt_json(s: &findb::bank::Statement) -> serde_json::Value {
    json!({
        "id": s.id,
        "period": period_to_str(s.period),
        "account": s.account_code,
        "date": s.biz_date.format("%Y-%m-%d").to_string(),
        "summary": s.summary,
        "settle_no": s.settle_no,
        "debit": s.debit.fmt_money(),
        "credit": s.credit.fmt_money(),
        "balance": s.balance.fmt_money(),
        "entry_id": s.entry_id,
        "matched_by": s.matched_by,
    })
}


fn book_entry_json(b: &findb::bank::BookEntry) -> serde_json::Value {
    json!({
        "entry_id": b.entry_id,
        "voucher_id": b.voucher_id,
        "date": b.date.format("%Y-%m-%d").to_string(),
        "voucher_no": b.voucher_label(),
        "summary": b.summary,
        "settle_no": b.settle_no,
        "debit": b.debit.fmt_money(),
        "credit": b.credit.fmt_money(),
    })
}


fn reconcile_json(r: &findb::bank::Reconciliation) -> serde_json::Value {
    json!({
        "period": period_to_str(r.period),
        "account": r.account_code,
        "bank_balance": r.bank_balance.fmt_money(),
        "book_balance": r.book_balance.fmt_money(),
        "bank_adjusted": r.bank_adjusted.fmt_money(),
        "book_adjusted": r.book_adjusted.fmt_money(),
        "balanced": r.balanced(),
        "diff": r.diff().fmt_money(),
        "book_only_in": r.book_only_in.iter().map(book_entry_json).collect::<Vec<_>>(),
        "book_only_out": r.book_only_out.iter().map(book_entry_json).collect::<Vec<_>>(),
        "bank_only_in": r.bank_only_in.iter().map(stmt_json).collect::<Vec<_>>(),
        "bank_only_out": r.bank_only_out.iter().map(stmt_json).collect::<Vec<_>>(),
    })
}


#[derive(Deserialize, Default)]
struct BankImportReq {
    pub ymm: i32,
    pub account: String,
    #[serde(default)]
    pub text: String,
}


#[derive(Deserialize)]
struct BankAutoReq {
    pub ymm: i32,
    pub account: String,
    #[serde(default)]
    pub tolerance: i64,
}


#[derive(Deserialize)]
struct BankLinkReq {
    pub stmt_id: i64,
    pub entry_id: i64,
}


#[derive(Deserialize)]
struct BankStmtIdReq {
    pub stmt_id: i64,
}


// ---------------------------------------------------------------------------
// 往来核销（与桌面端对齐）
// ---------------------------------------------------------------------------

/// 未核销清单里额外带「挂了几天」。
///
/// 账龄表只给「最老天数」，答不了「华达这 5 万是哪几张单、各挂了多久」——
/// 而这正是催款时第一句要问的。天数按 `upto` 期间最后一天算，与账龄表的
/// `max_days` 同一个基准，两处不能一个按今天、一个按期末。
fn open_entry_json_with_days(
    e: &findb::settle::OpenEntry,
    as_of: chrono::NaiveDate,
) -> serde_json::Value {
    let mut v = open_entry_json(e);
    v["days"] = serde_json::json!((as_of - e.date).num_days().max(0));
    v
}


fn open_entry_json(e: &findb::settle::OpenEntry) -> serde_json::Value {
    json!({
        "entry_id": e.entry_id,
        "voucher_id": e.voucher_id,
        "period": period_to_str(e.period),
        "date": e.date.format("%Y-%m-%d").to_string(),
        "voucher_no": format!("{}-{:04}", e.word, e.no),
        "line": e.line,
        "summary": e.summary,
        "account_code": e.account_code,
        "aux_key": e.aux_key,
        "settle_no": e.settle_no,
        "debit": e.debit.fmt_money(),
        "credit": e.credit.fmt_money(),
        "settled": e.settled.fmt_money(),
        "open": e.open().fmt_money(),
        "dir": e.dir_label(),
        // 已记账 = 可参与核销；false 仅在列表页勾了「含未记账」时出现
        "posted": e.posted,
    })
}


#[derive(Deserialize, Default)]
struct SettleAutoReq {
    pub account: String,
    #[serde(default)]
    pub ymm: i32,
    #[serde(default)]
    pub tolerance: String,
}


#[derive(Deserialize)]
struct SettleRunReq {
    pub from_entry: i64,
    pub to_entry: i64,
    pub amount: String,
    /// 差异（现金折扣 / 抹零）金额：> 0 时自动出一张差异凭证把零头记入损益
    #[serde(default)]
    pub diff: String,
    /// 差异科目（如 660305 财务费用-其他 / 6711 营业外支出）；差异为 0 时可空
    #[serde(default)]
    pub diff_account: String,
}


#[derive(Deserialize)]
struct UnsettleReq {
    pub id: i64,
    /// 只撤销这么多（缺省 = 整条撤销）。
    ///
    /// 核销三张单共 5 万，事后发现其中一张该是 3 万 —— 以前只能整条撤销再重录，
    /// 撤销的瞬间三张单全变「未核销」，账龄立刻虚高。
    #[serde(default)]
    pub amount: Option<String>,
}


/// 通用往来互抵请求：借方挂账 + 贷方挂账，科目/往来单位可以不同
#[derive(Deserialize)]
struct SettleCrossReq {
    /// 借方挂账分录（被冲减的应收 / 预付等）
    pub debit_entry: i64,
    /// 贷方挂账分录（被冲减的应付 / 预收等）
    pub credit_entry: i64,
    pub amount: String,
    /// 过渡凭证日期（`YYYY-MM-DD`，缺省今天）
    #[serde(default)]
    pub date: String,
}


/// 核销「未核销清单」默认返回多少行、最多多少行。
///
/// 默认 200：一屏十几行，200 行足够翻；上限 2000 是防止有人写 `limit=999999`
/// 把 700KB 的包又拉回来。



// ---------------- 往来对冲单（应收 ↔ 应付相互抵销） ----------------

#[derive(Deserialize)]
struct OffsetCreateReq {
    #[serde(default)]
    date: String,
    pub customer: String,
    pub supplier: String,
    #[serde(default)]
    pub ar_account: String,
    #[serde(default)]
    pub ap_account: String,
    pub amount: String,
    #[serde(default)]
    pub memo: String,
}


// ---- 往来对账单（对标金蝶「客户对账单」：按期间出账单 + 客户回签确认）----

#[derive(Deserialize)]
struct StatementReq {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    account: String,
    party_code: String,
    #[serde(default)]
    party_name: String,
    /// 起始期间 ymm
    from: i32,
    /// 截止期间 ymm
    to: i32,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct StatementStatusReq {
    status: String,
    /// 回签确认人（confirmed 时必填，用于留痕"谁替客户签的"）
    #[serde(default)]
    confirmed_by: String,
}


fn statement_json(s: &findb::statement::Statement) -> serde_json::Value {
    json!({
        "id": s.id,
        "no": s.no,
        "kind": s.kind,
        "kind_label": s.kind_label(),
        "account": s.account,
        "party_code": s.party_code,
        "party_name": s.party_name,
        "from": period_to_str(s.period_from),
        "to": period_to_str(s.period_to),
        "begin_balance": s.begin_balance.fmt_money(),
        "period_increase": s.period_increase.fmt_money(),
        "period_decrease": s.period_decrease.fmt_money(),
        "end_balance": s.end_balance.fmt_money(),
        "line_count": s.line_count,
        "status": s.status,
        "status_label": s.status_label(),
        "memo": s.memo,
        "created_by": s.created_by,
        "created_at": s.created_at,
        "sent_at": s.sent_at,
        "confirmed_by": s.confirmed_by,
        "confirmed_at": s.confirmed_at,
        "lines": s.lines.iter().map(|l| json!({
            "date": l.date,
            "doc_no": l.doc_no,
            "summary": l.summary,
            "increase": l.increase.fmt_money(),
            "decrease": l.decrease.fmt_money(),
            "balance": l.balance.fmt_money(),
        })).collect::<Vec<_>>(),
    })
}


// ---- 催款单 / 对账函（应收催收闭环） ----

#[derive(Deserialize)]
struct DunningReq {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    account: String,
    party_code: String,
    #[serde(default)]
    party_name: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct DunningStatusReq {
    status: String,
}


// ---------------------------------------------------------------------------
// 三大报表：资产负债表 / 利润表 / 现金流量表（JSON + 打印预览）
// ---------------------------------------------------------------------------

/// 从 query 解析 起/止期间（默认当前期间）
/// 报表/账簿的期间区间。
///
/// 非法输入（敲错的格式、或者 from 晚于 to）现在返回 400，不再静默回退成
/// 另一个区间 —— 理由与实测数据见 `state::period_param`。这一处是资产负债表、
/// 利润表、现金流量表等 11 个报表/账簿端点的共同入口。
fn report_range(
    state: &WebState,
    user: &CurrentUser,
    q: &HashMap<String, String>,
) -> Result<(Period, Period), AppError> {
    let cur = current_period(state, user);
    let year_start = fincore::Period::new(cur.year(), 1).unwrap_or(cur);
    period_range_param(q, year_start, cur)
}


/// 加载报表定义（优先账套内自定义，否则内置模板）
fn report_def(
    db: &findb::Db,
    key: &str,
    fallback: fincore::report::ReportDef,
) -> fincore::report::ReportDef {
    findb::reports::get_def(db, key)
        .ok()
        .flatten()
        .unwrap_or(fallback)
}


/// 资产负债表 / 利润表共用渲染：返回 ReportTable
fn statement_table(
    db: &findb::Db,
    user: &CurrentUser,
    key: &str,
    from: Period,
    to: Period,
    kind_maps: Vec<Box<dyn Fn(fincore::report::AmountKind) -> fincore::report::AmountKind>>,
) -> Result<fincore::report::ReportTable, AppError> {
    let def = match key {
        "balance_sheet" => report_def(db, key, fincore::report::balance_sheet::balance_sheet_def()),
        _ => report_def(db, key, fincore::report::income::income_statement_def()),
    };
    // 资产负债表第二列是"年初余额"：取数基准必须从会计年度 1 月起，
    // 不能用请求里的 from（否则 5 月查表会把 5 月初当成"年初"）。
    let (bq_from, subtitle) = if key == "balance_sheet" {
        (
            Period::new(to.year(), 1).unwrap_or(from),
            format!("{} 期末", to.label()),
        )
    } else {
        (from, format!("{} 至 {}", from.label(), to.label()))
    };
    let mut bq = BalanceQuery::range(bq_from, to);
    bq = bq.with_user_scope(&user.user);
    // 利润表剔除结转损益凭证：结完账损益科目借贷相抵，不剔利润表全是 0
    if key != "balance_sheet" {
        bq = bq.with_exclude_carry(true);
    }
    let snap = BalanceSnapshot::load(db, &bq)?;
    let company = db.options().company;
    Ok(fincore::report::render(
        &def, &snap, &company, &subtitle, kind_maps,
    ))
}


/// ReportTable → JSON（行/值/样式）
fn table_json(t: &fincore::report::ReportTable) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = t
        .rows
        .iter()
        .map(|r| {
            json!({
                "no": r.no,
                "name": r.name,
                "indent": r.indent,
                "style": format!("{:?}", r.style).to_lowercase(),
                "values": r.values.iter().map(|v| v.fmt_money()).collect::<Vec<_>>(),
                "negative": r.show_negative_red,
            })
        })
        .collect();
    json!({
        "title": t.title,
        "subtitle": t.subtitle,
        "company": t.company,
        "columns": t.columns,
        "rows": rows,
    })
}


/// 打印页的「试打」覆盖：`?po_font=9&po_orient=landscape…`
///
/// 设置页的「试打一张」要能在**保存之前**看到效果，否则用户只能「改 → 存 → 去报表 →
/// 打印 → 发现不对 → 回来再改」。这里只覆盖这次请求的渲染，不写库 —— 参数非法一律
/// 忽略并沿用账套设置，宁可打不出新样式也不要打个 500。
fn print_opts_from_q(mut po: fincore::PrintOptions, q: &HashMap<String, String>) -> fincore::PrintOptions {
    let get = |k: &str| q.get(k).map(|v| v.trim()).filter(|v| !v.is_empty());
    if let Some(v) = get("po_orient") {
        if matches!(v, "auto" | "portrait" | "landscape") {
            po.orientation = v.to_string();
        }
    }
    if let Some(v) = get("po_font").and_then(|v| v.parse::<u8>().ok()) {
        if (9..=14).contains(&v) {
            po.font_pt = v;
        }
    }
    if let Some(v) = get("po_margin").and_then(|v| v.parse::<u8>().ok()) {
        if (5..=30).contains(&v) {
            po.margin_mm = v;
        }
    }
    let flag = |k: &str, cur: bool| match get(k) {
        Some("1") => true,
        Some("0") => false,
        _ => cur,
    };
    po.show_company = flag("po_company", po.show_company);
    po.show_printed_at = flag("po_time", po.show_printed_at);
    po.show_page_no = flag("po_pageno", po.show_page_no);
    po.show_signatures = flag("po_sign", po.show_signatures);
    if let Some(v) = get("po_signline") {
        if !v.is_empty() {
            po.sign_line = v.to_string();
        }
    }
    // 套打偏移：正数=向右/向下（针式打印机走纸偏时对位用）
    if let Some(v) = get("po_ox").and_then(|v| v.parse::<i16>().ok()) {
        if (-20..=20).contains(&v) {
            po.offset_x_mm = v;
        }
    }
    if let Some(v) = get("po_oy").and_then(|v| v.parse::<i16>().ok()) {
        if (-20..=20).contains(&v) {
            po.offset_y_mm = v;
        }
    }
    po
}


fn to_ytd(k: fincore::report::AmountKind) -> fincore::report::AmountKind {
    match k {
        fincore::report::AmountKind::PeriodDebit => fincore::report::AmountKind::YearDebit,
        fincore::report::AmountKind::PeriodCredit => fincore::report::AmountKind::YearCredit,
        other => other,
    }
}


// ---------------- 期初建账 ----------------
#[derive(Deserialize)]
struct BeginRowInput {
    account_code: String,
    #[serde(default)]
    aux: AuxRef,
    dir: Direction,
    yb: String,
    #[serde(default)]
    ad: String,
    #[serde(default)]
    ac: String,
    #[serde(default)]
    qty: Option<String>,
}


#[derive(Deserialize)]
struct AccountReq {
    account: Account,
    #[serde(default)]
    aux_kinds: Vec<String>,
}


/// 前端传辅助核算维度的 kind 字符串列表（如 ["customer","project"]），
/// 由后端统一转成 AuxMask 位掩码，避免前端依赖位序。
fn build_aux_mask(kinds: &[String]) -> AuxMask {
    let mut mask = AuxMask::NONE;
    for k in kinds {
        if let Some(kind) = AuxKind::from_code(k) {
            mask.set(kind, true);
        }
    }
    mask
}


/// 期初保存请求。
///
/// `rows` + `delete_ids` 是有意设计的：原来 body 是裸数组、只有 upsert 语义，
/// **界面上的「移除」永远落不了地** —— 前端把行从草稿删掉、提交时只带剩下的行，
/// 后端按 (account_code, aux_key) 做 upsert，于是被移除的那行留在库里。实测：
/// 移除 140501 → 保存 → DB 里那行纹丝不动，试算平衡从平衡变成差 999,999。
/// 会计改个错字重新保存，期初金额就翻倍。
///
/// 不做「整表替换」而按 id 精确删：同一张表可能还有别人维护的行（带辅助核算
/// 维度的期初），按提交集合反推该删谁会误删。
///
/// 保留 `#[serde(untagged)]` 的裸数组分支：老客户端与既有集成测试仍在发裸数组，
/// 改成只认对象会让它们全部 422。
#[derive(Deserialize)]
#[serde(untagged)]
enum BeginSaveReq {
    Wrapped {
        #[serde(default)]
        rows: Vec<BeginRowInput>,
        #[serde(default)]
        delete_ids: Vec<i64>,
    },
    Rows(Vec<BeginRowInput>),
}


// ---------------- 期初余额批量粘贴 ----------------

/// 解析请求
#[derive(Deserialize)]
struct BeginBatchParseReq {
    text: String,
}


/// 导入请求（前端回传确认过的行）
#[derive(Deserialize)]
struct BeginBatchImportReq {
    rows: Vec<BeginBatchRow>,
    /// 前端草稿里已有的行也要避开 —— 与桌面的 `existing` 口径一致
    #[serde(default)]
    existing: Vec<String>,
}


#[derive(Deserialize, serde::Serialize, Clone)]
struct BeginBatchRow {
    code: String,
    /// "debit" / "credit"（前端不自己判断方向，只回传解析结果里的值）
    dir: String,
    amount: String,
}


/// 期末余额上下文：方向 + 末级编码（解析要靠它）
///
/// **每次都从库里现取**，不接受前端传 —— 方向是「归位」的依据，
/// 让前端传等于让前端决定借贷方向，那正是期初不平的头号原因。
fn chart_ctx(
    db: &findb::Db,
) -> Result<(std::collections::HashMap<String, String>, Vec<String>), findb::DbError> {
    let chart = findb::accounts::chart(&db)?;
    let all = chart.all(); // Vec<&Account>
    let dirs: std::collections::HashMap<String, String> = all
        .iter()
        .map(|a| {
            (
                a.code.clone(),
                match a.dir {
                    Direction::Debit => "debit".to_string(),
                    Direction::Credit => "credit".to_string(),
                },
            )
        })
        .collect();
    let leaves: Vec<String> = all
        .iter()
        .filter(|a| chart.is_leaf(&a.code))
        .map(|a| a.code.clone())
        .collect();
    Ok((dirs, leaves))
}


// ---------------- 备份 / 恢复 ----------------
#[derive(Deserialize)]
struct BackupRestoreReq {
    file: String,
}


/// 强制把 WAL 合并回主文件（TRUNCATE），使文件级复制（`fs::copy`）拿到一致快照。
///
/// 数据库为 WAL 模式，最新提交可能滞留在 `-wal` 文件中；若不先 checkpoint 直接复制
/// 主文件，备份会缺最新数据，恢复后丢账。checkpoint 后主文件即完整、`-wal` 被清空。
fn checkpoint_wal(db: &findb::Db) -> Result<(), AppError> {
    let _row: (i64, i64, i64) = db
        .conn()
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(findb::DbError::from)?;
    Ok(())
}


/// 当前账套引用的大附件文件（相对 `.attachments/` 的文件名）。
fn book_attachment_paths(db: &Db) -> Result<Vec<String>, AppError> {
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT DISTINCT path FROM attachment \
             WHERE inline=0 AND path IS NOT NULL AND path <> ''",
        )
        .map_err(|e| AppError::Internal(format!("读取附件清单失败：{e}")))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| AppError::Internal(format!("读取附件清单失败：{e}")))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Internal(format!("读取附件清单失败：{e}")))
}


/// 备份文件名是否属于 `key` 账套。
///
/// 文件名格式由 `create_backup` / `restore_backup` 生成：
/// - `YYYYMMDD_HHMMSS_<key>.fbk`
/// - `auto_YYYYMMDD_HHMMSS_<key>.fbk`
///
/// 关键是**先剥掉固定宽度的时间戳前缀，再比较剩余部分是否与 key 完全相等**。
/// 原先的 `ends_with("_{key}.fbk")` 会把 key 当成后缀匹配：`alice_202610.fbk`
/// 会被 key=`202610` 的账套认领。
/// 备份文件名时间戳：精确到毫秒，避免同一秒连续点击互相覆盖。
/// 格式 `YYYYMMDD_HHMMSSmmm`（9 位时间），旧备份仍是 6 位，解析两种都兼容。
fn backup_stamp() -> String {
    let now = chrono::Local::now();
    format!(
        "{}{:03}",
        now.format("%Y%m%d_%H%M%S"),
        now.timestamp_subsec_millis()
    )
}


fn backup_belongs_to(name: &str, key: &str) -> bool {
    let Some(stem) = name.strip_suffix(".fbk") else {
        return false;
    };
    let stem = stem.strip_prefix("auto_").unwrap_or(stem);
    // 固定前缀：8 位日期 + '_' + 6 或 9 位时间 + '_' + key。
    let bytes = stem.as_bytes();
    if bytes.len() < 16 {
        return false;
    }
    let digits = |s: &[u8]| s.iter().all(|b| b.is_ascii_digit());
    if !(digits(&bytes[..8]) && bytes[8] == b'_') {
        return false;
    }
    let rest = &stem[9..];
    let Some(pos) = rest.find('_') else {
        return false;
    };
    let time = &rest[..pos];
    if !(time.len() == 6 || time.len() == 9) || !digits(time.as_bytes()) {
        return false;
    }
    &rest[pos + 1..] == key
}


#[derive(Deserialize)]
struct TemplateGenerateReq {
    /// 期间 ymm，如 202601，缺省当前期间
    #[serde(default)]
    period: Option<i32>,
    /// 凭证日期，缺省期间末日
    #[serde(default)]
    date: String,
}


#[derive(Deserialize)]
struct ItemPlanReq {
    #[serde(default)]
    item_code: String,
    #[serde(default)]
    safety_stock: String,
    #[serde(default)]
    lead_days: i32,
    #[serde(default)]
    lot_size: String,
}


/// 预置档案端点的入参：只要类别
#[derive(serde::Deserialize)]
struct KindReq {
    kind: AuxKind,
}


/// `?kind=` 查询串。
///
/// **`kind` 必须有 `#[serde(default)]`**：axum 对查询串反序列化失败时返回 422，
/// 而不是交给处理函数里那行 `unwrap_or(Dept)`。少写这一个属性，
/// 前端 `loadAuxPreset` 就会 catch 到 422 直接 return —— 按钮保持 hidden、
/// 提示区空白，整条预置链路静默消失，而且**编译期和 Rust 测试都发现不了**
/// （E2E 才暴露）。这是「参数从 URL 进来」比「从 body 进来」更危险的地方：
/// body 少字段会 422，query 少字段也会 422，两者都很难在本地顺手测到。
#[derive(serde::Deserialize, Default)]
struct KindQuery {
    #[serde(default)]
    kind: String,
}


/// 辅助档案批量粘贴：解析（预览）
#[derive(Deserialize)]
struct AuxBatchParseReq {
    kind: AuxKind,
    text: String,
}


/// 辅助档案批量粘贴：真正落库
#[derive(Deserialize)]
struct AuxBatchImportReq {
    kind: AuxKind,
    /// 只导入前端回传的这些行。**不接受「重新解析一遍文本」**：
    /// 用户在预览之后可能改过文本（比如去掉一行），若后端重新解析，
    /// 实际导入的就不是他看到并确认过的那批 —— 正是「确认了 A 写进去 B」
    /// 这类最难查的错。
    rows: Vec<AuxBatchRow>,
}


#[derive(Deserialize, serde::Serialize, Clone)]
struct AuxBatchRow {
    code: String,
    name: String,
    /// 「导出文件」粘回来时才有的三列。前端只做透传（不渲染），
    /// 所以桌面端能还原的上级/停用/备注，网页端也一并还原。
    #[serde(default)]
    parent_code: Option<String>,
    #[serde(default)]
    memo: String,
    #[serde(default)]
    disabled: bool,
}


/// 解析结果里的一行（给前端渲染预览表格）
#[derive(serde::Serialize)]
struct AuxBatchRowView {
    code: String,
    name: String,
    /// None = 可导入；Some = 跳过原因（中文文案）
    skip: Option<String>,
    parent_code: Option<String>,
    memo: String,
    disabled: bool,
}


// ===========================================================================
// 工资管理（对齐桌面端 finui：工资表 / 个税明细 / 凭证生成，入口权限 VoucherNew）
// ===========================================================================

/// 从查询串取期间，缺省用当前期间
fn query_period(state: &WebState, user: &CurrentUser, q: &HashMap<String, String>) -> Period {
    q.get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(state, user))
}


/// 凭证生成请求里的日期，缺省用期间末日
fn req_date(s: &str, fallback: NaiveDate) -> Result<NaiveDate, AppError> {
    if s.trim().is_empty() {
        return Ok(fallback);
    }
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))
}


#[derive(Deserialize)]
struct PayrollInput {
    employee: String,
    #[serde(default)]
    dept: String,
    gross: String,
    #[serde(default)]
    social: String,
    #[serde(default)]
    housing: String,
    #[serde(default)]
    deduction: String,
    /// 专项附加扣除
    #[serde(default)]
    additional: String,
    #[serde(default)]
    social_co: String,
    #[serde(default)]
    housing_co: String,
    #[serde(default)]
    memo: String,
}


/// 工资行的数据范围过滤（列表 / 导出 / 银行代发 / 个税申报表共用，避免各处漏抄）
/// 部门范围与「仅看本人经手的业务单据」叠加，与 own_doc_only 口径一致。
fn scope_payroll<'a>(
    user: &CurrentUser,
    rows: &'a [business::Payroll],
) -> Vec<&'a business::Payroll> {
    rows.iter()
        .filter(|r| user.user.data_scope.allows_dept(&r.dept) && doc_in_scope(user, &r.employee))
        .collect()
}


/// 员工银行信息（辅助档案 props.bank_account / bank_name）
fn employee_bank(db: &findb::Db, code: &str) -> Result<(String, String), AppError> {
    let e = auxs::get(db, AuxKind::Employee, code)?;
    let props = e.map(|x| x.props).unwrap_or_default();
    let acc = props.get("bank_account").cloned().unwrap_or_default();
    let name = props.get("bank_name").cloned().unwrap_or_default();
    Ok((acc.trim().to_string(), name.trim().to_string()))
}


#[derive(Deserialize)]
struct GeneratePayrollReq {
    #[serde(default)]
    period: Option<String>,
    rows: Vec<PayrollInput>,
}


#[derive(Deserialize)]
struct PayrollAccrueReq {
    #[serde(default)]
    date: String,
    expense: String,
    wage_payable: String,
    social_payable: String,
    housing_payable: String,
}


#[derive(Deserialize)]
struct PayrollSocialReq {
    #[serde(default)]
    date: String,
    social_payable: String,
    housing_payable: String,
    personal_payable: String,
    bank_account: String,
}


#[derive(Deserialize)]
struct PayrollPayReq {
    #[serde(default)]
    date: String,
    payable_account: String,
    bank_account: String,
    tax_account: String,
    social_account: String,
}


// ===========================================================================
// 费用报销（对齐桌面端 finui：草稿→提交→审批→支付→生成凭证，入口权限 VoucherNew）
// ===========================================================================

#[derive(Deserialize)]
struct ClaimItemInput {
    expense_account: String,
    amount: String,
    #[serde(default)]
    memo: String,
}


#[derive(Deserialize)]
struct ClaimInput {
    /// 期间 ymm，如 202601
    #[serde(default)]
    period: Option<i32>,
    /// 业务日期 YYYY-MM-DD
    biz_date: String,
    applicant: String,
    #[serde(default)]
    dept: String,
    reason: String,
    amount: String,
    #[serde(default)]
    items: Vec<ClaimItemInput>,
}


fn claim_items(items: &[ClaimItemInput]) -> Result<Vec<business::ClaimItem>, AppError> {
    let mut out = Vec::with_capacity(items.len());
    for i in items {
        out.push(business::ClaimItem {
            expense_account: i.expense_account.trim().to_string(),
            amount: parse_money_checked(&i.amount)?,
            memo: i.memo.trim().to_string(),
        });
    }
    Ok(out)
}


#[derive(Deserialize)]
struct ClaimTransitionReq {
    /// draft / submitted / approved / rejected / paid
    status: String,
}


#[derive(Deserialize)]
struct ClaimVoucherReq {
    /// 贷方支付科目（如 100201 银行存款）
    pay_account: String,
}


// ---------------------------------------------------------------------------
// Excel（xlsx）导出 —— 需 Export 权限
//
// 与上面的 CSV 导出**并存**而不替换：那批（凭证/明细账/工资/报销/催款/派工）
// 已经有前端和外部脚本在用，换格式会打断它们。这里加的是两类真需要 xlsx 的表：
//   1. 基础资料（客户/存货/科目/期初）—— 必须能「导出 → 改 → 导回」，
//      所以列头与 `findb::imports` 的解析器逐字对齐，见 `crates/finweb/src/sheets.rs`；
//   2. 三大报表 + 科目余额表 —— 金额要是真数字，能求和、能透视。
// 早期用户拿到的 CSV 在 Excel 里金额是文本（左对齐、不能求和），要手动「分列」，
// 这是本次改造最直接的动力。
// ---------------------------------------------------------------------------

/// 附件下载头。中文文件名要走 RFC 5987 的 `filename*`。
///
/// 只写 `filename="报表.xlsx"` 是不够的：`HeaderValue::from_str` 遇到非 ASCII
/// 会**失败**，而旧代码是 `if let Ok(v) = ... { insert }` —— 于是响应里干脆
/// 没有 Content-Disposition，浏览器把文件存成随机名。用户看不出是这个原因。
pub fn attachment_header(filename: &str) -> Option<axum::http::HeaderValue> {
    let ascii: String = filename
        .chars()
        .map(|c| if c.is_ascii() && c != '"' && c != '\\' { c } else { '_' })
        .collect();
    let mut pct = String::new();
    for b in filename.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'~') {
            pct.push(*b as char);
        } else {
            pct.push_str(&format!("%{b:02X}"));
        }
    }
    axum::http::HeaderValue::from_str(&format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{pct}"
    ))
    .ok()
}


const XLSX_MIME: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

fn xlsx_response(filename: &str, bytes: Vec<u8>) -> Response {
    let mut resp = bytes.into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, axum::http::HeaderValue::from_static(XLSX_MIME));
    if let Some(v) = attachment_header(filename) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}


fn build_xlsx(sheets: &[crate::xlsxout::Sheet], filename: &str, bytes_hint: &str) -> Result<Response, AppError> {
    let bytes = crate::xlsxout::build(sheets).map_err(AppError::BadRequest)?;
    if bytes.is_empty() {
        return Err(AppError::Internal(format!("生成 {bytes_hint} 失败：内容为空")));
    }
    Ok(xlsx_response(filename, bytes))
}


/// 三大报表 + 所有者权益变动表的导出目标
#[derive(Clone, Copy)]
enum StatementKind {
    BalanceSheet,
    Income,
    CashFlow,
    Equity,
}


impl StatementKind {
    fn filename(self) -> &'static str {
        match self {
            StatementKind::BalanceSheet => "资产负债表",
            StatementKind::Income => "利润表",
            StatementKind::CashFlow => "现金流量表",
            StatementKind::Equity => "所有者权益变动表",
        }
    }
}


fn export_statement_xlsx(
    state: &WebState,
    user: &CurrentUser,
    q: &HashMap<String, String>,
    kind: StatementKind,
) -> Result<Response, AppError> {
    let db = &user.db;
    let (from, to) = report_range(state, user, q)?;
    let subtitle = format!("{} 至 {}", from.label(), to.label());
    // 取数与打印页/JSON 接口**走同一条路**（statement_table / cash_flow_statement /
    // equity_statement）。另起一套取数逻辑是这类改造最容易犯的错：导出的表和
    // 页面上的表对不上，而两张表看起来都"挺对"。
    let sheets: Vec<crate::xlsxout::Sheet> = match kind {
        StatementKind::BalanceSheet => {
            let t = statement_table(
                db,
                user,
                "balance_sheet",
                from,
                to,
                vec![
                    Box::new(fincore::report::identity),
                    Box::new(fincore::report::to_begin),
                ],
            )?;
            vec![crate::sheets::report_sheet(&t, "元")]
        }
        StatementKind::Income => {
            let t = statement_table(
                db,
                user,
                "income_statement",
                from,
                to,
                vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
            )?;
            vec![crate::sheets::report_sheet(&t, "元")]
        }
        StatementKind::CashFlow => {
            // 两张表：本期主表 + 本期与上期对比
            let (cf, prev_cf) = findb::reports::cash_flow_compare(db, from, to, Some(&user.user))?;
            let (pf, pt) = findb::reports::prior_range(from, to);
            vec![
                crate::sheets::cash_flow_sheet(&cf, &subtitle),
                crate::sheets::cash_flow_compare_sheet(
                    &cf,
                    &prev_cf,
                    &format!("本期 {}", to.label()),
                    &format!("上期 {}~{}", pf.label(), pt.label()),
                ),
            ]
        }
        StatementKind::Equity => {
            let st = findb::reports::equity_statement(db, from, to, Some(&user.user))?;
            vec![crate::sheets::equity_sheet(&st, &subtitle)]
        }
    };
    user.db.log(
        user.username(),
        "导出",
        "导出报表",
        &format!("{} {subtitle}", kind.filename()),
    )?;
    build_xlsx(
        &sheets,
        &format!("{}_{}.xlsx", kind.filename(), period_to_str(to)),
        kind.filename(),
    )
}


// ---------------------------------------------------------------------------
// 账套导入：把外部 .fbk（桌面端账套 / 另一台服务器导出的备份）搬进本服务
//
// 原来这是 Web 端**唯一的硬缺口**：`restore_restore` 那条路只认
// `books_dir/backups/` 下、经 `backup_belongs_to` 校验属于当前账套的备份文件，
// 所以桌面版辛苦建好的账套一个字都上不来。用户要的就是把 `.fbk` 传上去。
// ---------------------------------------------------------------------------

/// 单次上传上限。账套含附件时可能上百 MB，所以这条路单独放宽（全局是 12MB）。
const MAX_IMPORT_BOOK_BYTES: usize = 512 * 1024 * 1024;

/// 校验一个上传上来的文件「是不是一个能用的 FinBook 账套」。
///
/// 三步都要做，缺一步就会把垃圾文件注册成账套、后面每次开都 500：
/// 1. `PRAGMA integrity_check` —— 传一半断线的文件是能打开 SQLite 的；
/// 2. 有没有 `account` 表 —— 任意 Sqlite 文件都能被 `Db::open` 迁成空账套，
///    不先认出来就会把一张随便什么库"变成"一个科目全空的账套；
/// 3. schema 版本不能比本二进制新 —— 新版本的列旧代码不认识，
///    放进来之后第一次用到就 no such column，而且没有任何线索指向"版本太新"。
fn validate_book_file(path: &std::path::Path) -> Result<(), AppError> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| {
        AppError::bad_request(format!(
            "这个文件不是 SQLite 数据库（{e}）。请确认上传的是 .fbk 账套文件或本系统导出的备份 zip"
        ))
    })?;
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| AppError::bad_request(format!("账套文件无法读取：{e}")))?;
    if check != "ok" {
        return Err(AppError::bad_request(format!(
            "账套文件已损坏（integrity_check: {check}），不能导入"
        )));
    }
    let has_account: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='account'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if has_account == 0 {
        return Err(AppError::bad_request(
            "这个文件不是 FinBook 账套（缺少科目表 account）",
        ));
    }
    let v = findb::schema::version(&conn);
    if v > findb::schema::SCHEMA_VERSION {
        return Err(AppError::bad_request(format!(
            "这个账套是更新版本的 FinBook 建的（数据结构 v{v}），当前服务只支持到 v{}。请先升级服务再导入",
            findb::schema::SCHEMA_VERSION
        )));
    }
    Ok(())
}


/// 从上传内容里取出 (账套字节, 附件列表)。
///
/// 认两种格式，都是为了跟「导出」对上：
/// - 裸 `.fbk`（桌面端账套文件、单文件备份）；
/// - `.zip`（本服务 `/api/backup/download` 的产物，含 `.attachments/` 大附件）。
///   只收 `.fbk` 会丢掉附件 —— 附件不认后账套里那些凭证的扫描件就全是空链接。
fn unpack_book_upload(
    data: &[u8],
    filename: &str,
) -> Result<(Vec<u8>, Vec<(String, Vec<u8>)>), AppError> {
    // SQLite 文件头 16 字节：`SQLite format 3\0`
    if data.starts_with(b"SQLite format 3\0") {
        return Ok((data.to_vec(), Vec::new()));
    }
    // ZIP 本地文件头
    if data.starts_with(b"PK\x03\x04") {
        let mut zip = zip::ZipArchive::new(io::Cursor::new(data))
            .map_err(|e| AppError::bad_request(format!("zip 无法解析：{e}")))?;
        let mut book: Option<Vec<u8>> = None;
        let mut attach: Vec<(String, Vec<u8>)> = Vec::new();
        for i in 0..zip.len() {
            let mut f = zip
                .by_index(i)
                .map_err(|e| AppError::bad_request(format!("zip 条目无法读取：{e}")))?;
            if f.is_dir() {
                continue;
            }
            let name = f.name().to_string();
            // zip-slip：只接受「根目录下的 .fbk」和「.attachments/<扁平文件名>」，
            // 任何含 `..` 或额外目录层的条目一律丢弃，绝不落盘到目标路径之外。
            if name.contains("..") || name.starts_with('/') || name.contains('\\') {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".fbk") && !name.contains('/') {
                if book.is_some() {
                    return Err(AppError::bad_request(
                        "压缩包里不止一个 .fbk 账套文件，无法确定要导入哪个",
                    ));
                }
                let mut buf = Vec::new();
                io::Read::read_to_end(&mut f, &mut buf)
                    .map_err(|e| AppError::bad_request(format!("读取 zip 内账套失败：{e}")))?;
                book = Some(buf);
            } else if let Some(rel) = name.strip_prefix(".attachments/") {
                if rel.is_empty() || rel.contains('/') {
                    continue;
                }
                if !rel
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                {
                    continue;
                }
                let mut buf = Vec::new();
                io::Read::read_to_end(&mut f, &mut buf)
                    .map_err(|e| AppError::bad_request(format!("读取 zip 内附件失败：{e}")))?;
                attach.push((rel.to_string(), buf));
            }
        }
        let book = book.ok_or_else(|| {
            AppError::bad_request(format!(
                "压缩包里没有 .fbk 账套文件（收到的是 {filename}）"
            ))
        })?;
        return Ok((book, attach));
    }
    Err(AppError::bad_request(
        "不认识这个文件格式：需要 .fbk 账套文件，或本系统导出的备份 .zip",
    ))
}


/// 导入期初收集表：上传填好的《期初收集表.xlsx》→ 往来台账 + 科目期初一次落库。
///
/// 与「数据导入」里的期初导入不同：那个吃单张纸的 CSV/Excel，这个吃**整本四张纸**的
/// 收集表，并且先算平再写——不平就整批拒绝，不留半个账套。
/// 从 multipart 里取出 `file` 字段（期初收集表的三个入口共用）。
async fn sheet_upload(mut multipart: axum::extract::Multipart) -> Result<Vec<u8>, AppError> {
    let mut data: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("上传解析失败：{e}")))?
    {
        if field.name() == Some("file") {
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad_request(format!("读取上传内容失败：{e}")))?;
            data = Some(bytes.to_vec());
        }
    }
    let data = data.ok_or_else(|| AppError::bad_request("缺少文件字段 file"))?;
    if data.is_empty() {
        return Err(AppError::bad_request("上传文件为空"));
    }
    Ok(data)
}


/// 《期初收集表》的空模板：已预填当前账套的科目、客户、供应商清单。
/// 期初对账视图的 DTO。
///
/// 金额一律给**字符串**（`fmt_plain`）：前端直接显示，不做二次格式化 ——
/// 对账页上一个千分位错位就会让人怀疑是数字错了，而它只是显示问题。
#[derive(serde::Serialize)]
struct ReconcileSheetDto {
    label: String,
    rows: usize,
    total: String,
    hint: String,
}


#[derive(serde::Serialize)]
struct ReconcileCheckDto {
    name: String,
    ok: bool,
    detail: String,
}


#[derive(serde::Serialize)]
struct ReconcileDto {
    /// 全部校验通过且不空
    ok: bool,
    one_liner: String,
    empty: bool,
    sheets: Vec<ReconcileSheetDto>,
    debit_total: String,
    credit_total: String,
    balanced: bool,
    asset_total: String,
    liab_total: String,
    equity_total: String,
    checks: Vec<ReconcileCheckDto>,
    issues: Vec<String>,
}


#[cfg(test)]
mod security_regression_tests {
    use super::{backup_belongs_to, csv_escape};

    #[test]
    fn backup_name_must_match_key_exactly() {
        assert!(backup_belongs_to(
            "20261022_120000_alice_202610.fbk",
            "alice_202610"
        ));
        assert!(backup_belongs_to(
            "auto_20261022_120000_alice_202610.fbk",
            "alice_202610"
        ));
        assert!(backup_belongs_to("20261022_120000_202610.fbk", "202610"));
        // 毫秒级时间戳（v1.8.7 起）也必须能识别，且不能和 6 位时间的旧备份混淆。
        assert!(backup_belongs_to(
            "20261022_120000123_alice_202610.fbk",
            "alice_202610"
        ));
        assert!(backup_belongs_to(
            "auto_20261022_120000123_alice_202610.fbk",
            "alice_202610"
        ));
        assert!(!backup_belongs_to(
            "20261022_120000123_alice_202610.fbk",
            "202610"
        ));
        // 旧实现的后缀碰撞：key=202610 不能认领 alice_202610 的备份
        assert!(!backup_belongs_to(
            "20261022_120000_alice_202610.fbk",
            "202610"
        ));
        assert!(!backup_belongs_to(
            "auto_20261022_120000_alice_202610.fbk",
            "202610"
        ));
        // 形状不符或非 .fbk 一律不认
        assert!(!backup_belongs_to("20261022_120000_alice.fbk2", "alice"));
        assert!(!backup_belongs_to("2026102_120000_alice.fbk", "alice"));
        assert!(!backup_belongs_to("not-a-backup.fbk", "alice"));
    }

    #[test]
    fn csv_escape_neutralizes_formula_injection() {
        assert!(csv_escape("=1+1").starts_with('\''));
        assert!(csv_escape("+1+1").starts_with('\''));
        assert!(csv_escape("@SUM(A1)").starts_with('\''));
        assert!(csv_escape("-1+2").starts_with('\''));
        // 负数金额必须保持数字形态
        assert_eq!(csv_escape("-123.45"), "-123.45");
        assert_eq!(csv_escape("-1,234.56"), "\"-1,234.56\"");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
    }
}
