//! HTTP 请求处理函数

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{Datelike, NaiveDate};
use fincore::user::Perm;
use fincore::{
    Account, AuxEntity, AuxKind, AuxMask, AuxQuery, AuxRef, BookOptions, Direction, Entry, Money,
    Period, Role, User, Voucher, VoucherSource, VoucherStatus,
};
use findb::accounts;
use findb::advanced;
use findb::balances::{self, BalanceQuery, BalanceSnapshot, BeginRow, LedgerQuery};
use findb::periods;
use zip::{CompressionMethod, ZipWriter};
use zip::write::{FileOptions, FileOptionExtension};
use findb::security;
use findb::users;
use findb::vouchers::{self, VoucherQuery};
use findb::Db;
use findb::{auxs, business, template};
use serde::Deserialize;
use serde_json::json;
use tracing::error;

use crate::dto::*;
use crate::realm::{RealmBook, RealmDb};
use tower_http::compression::predicate::{NotForContentType, Predicate, SizeAbove};
use tower_http::compression::CompressionLayer;
use crate::state::{
    clear_cookie_header, parse_money_checked, parse_period, period_param, period_range_param,
    period_to_str, AppError, CurrentUser, RealmUser, WebState,
};

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

/// 深度探针（readiness）：真正去碰依赖，回答「数据层还能用吗」
///
/// ## 与 `/api/health` 的分工
///
/// - `/api/health` = **liveness**：进程还在收请求吗。**不碰任何依赖**，
///   所以数据库慢不会让它失败。
/// - `/api/health/ready` = **readiness**：账号库 / 账套库 / 数据目录还能读写吗。
///
/// 这个区分是硬要求，不是洁癖：liveness 一旦碰 DB，数据库变慢就会把容器判成
/// unhealthy，编排器随即重启应用 —— 于是「数据库一慢就重启应用，重启又让
/// 数据库更慢」，把一次本可自愈的抖动放大成宕机。负载均衡与监控该用 readiness。
///
/// ## 为什么原探针不够用
///
/// 原 `/api/health` 返回常量 `ok`，不碰 DB。于是下面这种最常见的故障它一律
/// 报「健康」：磁盘写满 → SQLite 写失败；账套文件损坏 → 打开即报错；
/// 账号库被锁 → 所有登录 500。进程活着、端口通，但系统已经不能用了。
/// healthcheck.sh 拿着这样的答案会一直显示「一切正常」—— 虚假的安全感比
/// 没有监控更危险，因为它让人不去查别的地方。
///
/// ## 探针自身必须廉价且无副作用
///
/// - 账套用 `open_no_migrate` 打开：只设连接级 PRAGMA，**不写库**。
///   探针绝不能触发迁移 —— 迁移是写事务，5 分钟一次的探针去抢写锁，
///   会在业务正忙时制造额外的锁竞争。
/// - 目录可写性用「建临时文件 → 立即删」，文件名不以 `.fbk` 结尾，
///   不会被账套扫描当成新账套。
async fn health_ready(
    State(state): State<Arc<WebState>>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let mut checks = serde_json::Map::new();
    let mut ok = true;

    // ① 账号库：真发一条 SQL（realm.db 损坏 / 被锁 / 权限不对都会在这里暴露）
    match state.realm.count_users() {
        Ok(n) => {
            checks.insert("realm".into(), json!({ "ok": true, "users": n }));
        }
        Err(e) => {
            ok = false;
            checks.insert(
                "realm".into(),
                json!({ "ok": false, "error": e.to_string() }),
            );
        }
    }

    // ② 账套目录：能列出来，且至少一个账套能真的读
    let mut book_files: Vec<std::path::PathBuf> = std::fs::read_dir(&state.books_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "fbk").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    book_files.sort();
    checks.insert("books_count".into(), json!(book_files.len()));

    // ③ 抽一个账套做只读查询 —— 文件损坏 / 权限不对 / 磁盘 I/O 错误都在这里现形
    if let Some(p) = book_files.first() {
        match findb::Db::open_no_migrate(p).and_then(|db| {
            // 走一次真实查询：Connection::open 是惰性的，坏文件也要 SELECT 才报错
            db.conn()
                .query_row("SELECT COUNT(*) FROM voucher", [], |r| r.get::<_, i64>(0))
                .map_err(Into::into)
        }) {
            Ok(_) => {
                checks.insert("book_sample".into(), json!({ "ok": true }));
            }
            Err(e) => {
                ok = false;
                checks.insert(
                    "book_sample".into(),
                    json!({
                        "ok": false,
                        "file": p.file_name().unwrap_or_default().to_string_lossy(),
                        "error": e.to_string(),
                    }),
                );
            }
        }
    }

    // ④ 数据目录可写：磁盘满 / 只读挂载 / 属主不对时，服务看着健康却录不了凭证。
    //    healthcheck.sh 只能查宿主机视角的权限，这里查进程自己的真实能力。
    let probe = state.books_dir.join(".finbook-write-probe");
    let writable = std::fs::write(&probe, b"ok").is_ok() && std::fs::remove_file(&probe).is_ok();
    if !writable {
        ok = false;
    }
    checks.insert(
        "books_dir_writable".into(),
        json!({ "ok": writable, "path": state.books_dir.display().to_string() }),
    );

    // ⑤ schema 迁移：升级后所有账套是否都已迁到当前版本。
    //
    // 这一项是为了消灭「探针报绿、账套还没升级」这种状态。迁移原本是懒执行
    // 的（账套第一次被打开时才跑），于是部署完成、readiness 全绿，而用户第一次
    // 点某个功能才报 `no such column: xxx` —— 生产上真实发生过。
    //
    // 判定：
    // - 仍在进行 → 未就绪（503）：实例还没真正可用
    // - 有账套迁不动 → 未就绪（503），并**指名道姓**报出是哪个、报什么。
    //   财务系统里一个账套打不开不是「可以先用着」，必须让人看见。
    let mig = state.books.migration_status();
    // 权威判据是「有没有账套还没迁过」，而不是「批量任务跑过没有」——
    // 前者能抓到运行期新增/恢复的账套，后者只看启动那一刻的快照。
    let pending = state.books.unmigrated_books();
    if !mig.done() || mig.has_failures() || !pending.is_empty() {
        ok = false;
    }
    checks.insert(
        "migration".into(),
        json!({
            "ok": mig.done() && !mig.has_failures() && pending.is_empty(),
            "done": mig.done(),
            "running": mig.running,
            "total": mig.total,
            "migrated": mig.ok,
            "pending": pending,
            "failed": mig.failed
                .iter()
                .map(|(k, e)| json!({ "book": k, "error": e }))
                .collect::<Vec<_>>(),
            "schema_version": findb::schema::SCHEMA_VERSION,
        }),
    );

    let body = json!({ "ok": ok, "checks": checks });
    // 有依赖不可用时返回 503：编排器据此把实例摘出负载均衡，
    // 但**不**触发重启（那是 liveness 的职责）。返回 200 会让负载均衡
    // 继续往一个明确不能用的实例上打流量。
    Ok((
        if ok {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(body),
    ))
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

// ---------------------------------------------------------------------------
// 认证 / 初始化状态
// ---------------------------------------------------------------------------

async fn get_setup_status(
    State(state): State<Arc<WebState>>,
) -> Result<Json<SetupStatus>, AppError> {
    // 管理员是否已初始化（首次启动已引导）
    let admin_set = state.realm.count_users()? > 0;
    Ok(Json(SetupStatus {
        admin_set,
        needs_setup: false,
        company: String::new(),
        version: state.version.clone(),
        book: None,
    }))
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

/// 账套列表（需登录）：管理员返回全部，普通用户返回自己创建的 + 被邀请加入的
/// 同时返回平台身份摘要（前端在"已登录未选账套"状态下据此渲染选择页）
async fn list_books(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<serde_json::Value>, AppError> {
    let books = visible_books(&state, &user.username, user.is_admin)?;
    let items: Vec<serde_json::Value> = books
        .iter()
        .map(|b| json!({ "key": b.key, "company": b.company, "owner": b.owner_username }))
        .collect();
    Ok(Json(json!({
        "user": {
            "username": user.username,
            "display_name": user.display_name,
            "is_admin": user.is_admin,
            "must_change_pwd": user.must_change_pwd,
        },
        "books": items,
    })))
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

async fn post_login(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Result<Response, AppError> {
    // 全局登录（认账号库，而非某一套账）
    let username = req.username.trim().to_string();
    // 设备指纹必须非空：空串会被写进绑定名单，等于给这个账号留了一条万能通道
    let device_id = req.device_id.trim().to_string();
    if device_id.is_empty() || device_id.len() > 128 {
        return Err(AppError::bad_request(
            "缺少有效的设备标识，请刷新页面后重试",
        ));
    }
    let ip = client_ip(&headers);
    // 平台持久锁（realm 层，跨重启生效）：策略 max_fail 次连续失败后由本接口写入
    let policy = state.policy();
    if let Ok(remain) = state.realm.lock_remaining_min(&username) {
        if remain > 0 {
            return Err(AppError::rate_limited(
                format!("账号已锁定，请约 {remain} 分钟后再试（管理员可在安全中心解锁）"),
                (remain as u64) * 60,
            ));
        }
    }
    // 登录限流：先查是否已被锁定，避免锁定期内继续做昂贵/可枚举的密码校验
    if let Err(secs) = state.login_limiter.check(&username) {
        let mins = secs.div_ceil(60).max(1);
        return Err(AppError::rate_limited(
            format!("尝试过于频繁，请约 {mins} 分钟后再试"),
            secs,
        ));
    }
    if let Some(ip) = &ip {
        if let Err(secs) = state.login_ip_limiter.check(ip) {
            let mins = secs.div_ceil(60).max(1);
            return Err(AppError::rate_limited(
                format!("该来源尝试过于频繁，请约 {mins} 分钟后再试"),
                secs,
            ));
        }
    }
    let ru = match state.realm.authenticate(&username, &req.password) {
        Ok(Some(ru)) => ru,
        Ok(None) => {
            state.login_limiter.record_failure(&username);
            if let Some(ip) = &ip {
                state.login_ip_limiter.record_failure(ip);
            }
            // 登录审计（平台层）+ 连续失败达标 → 持久锁
            let _ = state
                .realm
                .record_login_attempt(&username, false, ip.as_deref().unwrap_or(""));
            if let Ok(fails) = state.realm.recent_fail_count(&username) {
                if fails >= policy.max_fail.max(1) {
                    let _ = state.realm.lock_user(&username, policy.lock_minutes);
                }
            }
            return Err(AppError::unauthorized("用户名或口令错误"));
        }
        Err(e) => return Err(AppError::from(e)),
    };
    // 登录成功，清空该账号的失败计数（内存限流 + 平台失败记录 + 遗留锁）
    state.login_limiter.clear(&username);
    if let Some(ip) = &ip {
        state.login_ip_limiter.clear(ip);
    }
    let _ = state.realm.unlock_user(&username);
    let _ = state
        .realm
        .record_login_attempt(&username, true, ip.as_deref().unwrap_or(""));
    // 设备绑定（平台层）：普通账号最多绑 RealmDb::MAX_DEVICES_PER_USER 台，超出需管理员
    if !ru.is_admin {
        match state
            .realm
            .bind_device(&username, &device_id, &req.device_name)?
        {
            Ok(()) => {}
            Err(msg) => return Err(AppError::forbidden(msg)),
        }
    }
    // 顶掉「同一台设备」上的旧会话，但**保留其它设备的会话**。
    //
    // 原来这里是 remove_by_username（清该账号全部会话），那与「一个账号可绑两台设备」
    // 直接冲突：第 2 台设备一登录就把第 1 台踢下线，用户体感仍然是只能登一台，
    // 绑定上限也就只是个数字。
    //
    // 管理员仍然不受限：运维账号不该被这套机制管。
    if !ru.is_admin {
        state
            .sessions
            .remove_by_username_and_device(&username, &device_id);
    }
    let token = state.sessions.create(
        &username,
        ru.is_admin,
        &device_id,
        state.default_period,
        "", // 账套登录后由"选择账套"设定
    );
    // 返回该用户可进入的账套列表（管理员=全部，普通=本人创建 + 被邀请加入的成员）
    let books = visible_books(&state, &username, ru.is_admin)?;
    let book_list: Vec<serde_json::Value> = books
        .iter()
        .map(|b| json!({ "key": b.key, "company": b.company, "owner": b.owner_username }))
        .collect();
    let resp = LoginResp {
        user: PlatformUser {
            username: ru.username,
            display_name: ru.display_name,
            is_admin: ru.is_admin,
            must_change_pwd: ru.must_change_pwd,
        },
        must_change_pwd: ru.must_change_pwd,
        setup: false,
        books: book_list,
    };
    let mut r = Json(resp).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        crate::state::cookie_header(&token, SESSION_SECS),
    );
    Ok(r)
}

// ---------------------------------------------------------------------------
// 平台安全中心（口令策略 / 登录审计 / 账号解锁；均仅管理员）
// ---------------------------------------------------------------------------

/// 口令策略读取
async fn get_security_policy(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    Ok(Json(json!(state.realm.policy()?)))
}

/// 口令策略保存（值域校验：过小会把全员锁死，过大等于策略失效）
async fn set_security_policy(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(p): Json<fincore::user::PasswordPolicy>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if !(1..=64).contains(&p.min_len) {
        return Err(AppError::bad_request("最小口令长度需在 1-64 之间"));
    }
    if !(1..=20).contains(&p.max_fail) {
        return Err(AppError::bad_request("连续失败次数需在 1-20 之间"));
    }
    if !(1..=1440).contains(&p.lock_minutes) {
        return Err(AppError::bad_request("锁定时长需在 1-1440 分钟之间"));
    }
    if !(0..=1440).contains(&p.idle_minutes) {
        return Err(AppError::bad_request(
            "空闲超时需在 0-1440 分钟之间（0=不自动登出）",
        ));
    }
    if !(0..=3650).contains(&p.max_age_days) {
        return Err(AppError::bad_request(
            "口令有效期需在 0-3650 天之间（0=永不过期）",
        ));
    }
    state.realm.set_policy(&p)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LoginAttemptsQuery {
    username: Option<String>,
    limit: Option<u32>,
}

/// 登录审计：最近 N 条平台登录尝试（成功/失败 + 来源 IP）
async fn list_security_attempts(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Query(q): Query<LoginAttemptsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 500) as usize;
    let rows = state.realm.login_attempts(q.username.as_deref(), limit)?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|(id, username, ts, ok, ip)| {
            json!({ "id": id, "username": username, "ts": ts, "ok": ok, "ip": ip })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

/// 当前处于锁定状态的平台账号
async fn list_locked_security_users(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    let mut items = Vec::new();
    for u in state.realm.list_users()? {
        let remain = state.realm.lock_remaining_min(&u.username)?;
        if remain > 0 {
            items.push(json!({
                "username": u.username,
                "display_name": u.display_name,
                "locked_until": u.locked_until,
                "remaining_min": remain,
            }));
        }
    }
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
struct UnlockSecurityReq {
    username: String,
}

/// 解锁平台账号（同时清空失败计数）
async fn unlock_security_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<UnlockSecurityReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    let name = req.username.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("账号不能为空"));
    }
    if state.realm.get_user(name)?.is_none() {
        return Err(AppError::not_found("账号不存在"));
    }
    state.realm.unlock_user(name)?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// 跨账套合并汇总（平台级，仅管理员；v1 汇总，不含内部往来抵销）
// ---------------------------------------------------------------------------

/// 合并汇总：对选中账套按期间汇总各科目期末余额（各套独立取数，仅已记账 H-3）。
/// `books=b1,b2`；返回科目 × 账套矩阵 + 合计。
/// 平台经营总览（跨账套 × 各业务域可视化；仅平台管理员）
///
/// 复用 `workbench::collect`——它已经按 9 个业务域（凭证/资金/销售/采购/仓管/生产/
/// 成本/报销/审批）出卡片，不必为每个域重写一套查询。多账套时逐套打开取数再按
/// (域, 指标, 单位) 聚合。
///
/// 两个刻意的设计：
/// - **单套失败不影响整体**：某个账套文件损坏/被占用时记 `ok:false` + 原因，
///   继续取下一套。整体 500 会让管理员看不到其他账套的真实状况。
/// - **上限 50 套**：逐套 open 是 IO 密集操作，账套数量失控时宁可截断并明确告知，
///   也不让一个请求把连接数/文件句柄打满。
async fn platform_overview(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该入口仅限系统管理员使用"));
    }
    const MAX_BOOKS: usize = 50;
    let want_period: Option<i32> = q
        .get("period")
        .and_then(|s| parse_period(s))
        .map(|p| p.ymm());

    let mut books = visible_books(&state, &user.username, true)?;
    // 可选：只统计指定账套
    if let Some(list) = q.get("books") {
        let want: Vec<&str> = list
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if !want.is_empty() {
            books.retain(|b| want.contains(&b.key.as_str()));
        }
    }
    books.sort_by(|a, b| a.company.cmp(&b.company));
    let total = books.len();
    let truncated = total > MAX_BOOKS;
    books.truncate(MAX_BOOKS);

    let mut per_book: Vec<serde_json::Value> = Vec::with_capacity(books.len());
    // 聚合键：(域, 指标, 单位) —— 单位进键，避免把「元」和「张」加到一起
    let mut agg: std::collections::BTreeMap<(String, String, String), f64> = Default::default();
    let mut labels: std::collections::BTreeMap<(String, String, String), String> =
        Default::default();
    let mut domains: std::collections::BTreeMap<String, usize> = Default::default();
    let mut failed = 0usize;

    for b in &books {
        let db = match state.db_for(&b.key) {
            Ok(d) => d,
            Err(e) => {
                failed += 1;
                per_book.push(json!({
                    "key": b.key, "company": b.company, "owner": b.owner_username,
                    "ok": false, "error": e.to_string(),
                    "period": "", "cards": [], "todos": [],
                }));
                continue;
            }
        };
        // 期间：请求指定的优先；早于该账套启用期间时回落到启用期间（否则全是空）
        let opts = db.options();
        let start = opts.start_period;
        let cur = match want_period {
            Some(p) if p >= start.ymm() => Period::from_ymm_checked(p).unwrap_or(start),
            _ => start,
        };
        // 账套注册表里公司名可能没填，回落到账套自身参数
        let company = if b.company.trim().is_empty() {
            opts.company.clone()
        } else {
            b.company.clone()
        };
        // 以临时管理员身份看全部业务域（与 UI「全部账套」的进入语义一致）
        let admin = fincore::User::new("__platform_admin__", "平台管理员", fincore::Role::Admin);
        let wbo = match findb::workbench::collect(&db, &admin, cur, 6) {
            Ok(w) => w,
            Err(e) => {
                failed += 1;
                per_book.push(json!({
                    "key": b.key, "company": company, "owner": b.owner_username,
                    "ok": false, "error": e.to_string(),
                    "period": period_to_str(cur), "cards": [], "todos": [],
                }));
                continue;
            }
        };
        for c in &wbo.cards {
            // 卡片值是已格式化的纯数字（fmt2 无千分位 / 整数 to_string），可安全解析
            let n: f64 = c.value.replace(',', "").parse().unwrap_or(0.0);
            let k = (c.domain.clone(), c.key.clone(), c.unit.clone());
            *agg.entry(k.clone()).or_insert(0.0) += n;
            labels.entry(k).or_insert_with(|| c.label.clone());
            *domains.entry(c.domain.clone()).or_insert(0) += 1;
        }
        per_book.push(json!({
            "key": b.key, "company": company, "owner": b.owner_username,
            "ok": true, "error": "",
            "period": wbo.period,
            "cards": wbo.cards.iter().map(|c| json!({
                "domain": c.domain, "key": c.key, "label": c.label,
                "value": c.value, "unit": c.unit,
                "num": c.value.replace(',', "").parse::<f64>().unwrap_or(0.0),
            })).collect::<Vec<_>>(),
            "todos": wbo.todos.iter().map(|t| json!({
                "domain": t.domain, "key": t.key, "label": t.label,
                "count": t.count, "view": t.view,
            })).collect::<Vec<_>>(),
            // 趋势：近 6 期各业务域走势，供前端画迷你折线（跨账套对比用）
            "trends": wbo.trends.iter().map(|t| json!({
                "domain": t.domain, "key": t.key, "title": t.title, "unit": t.unit,
                "periods": t.periods,
                "series": t.series.iter().map(|s| json!({
                    "name": s.name, "color": s.color, "points": s.points,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }));
    }

    // 跨账套汇总（保持领域顺序稳定，便于前端分组渲染）
    let mut totals: Vec<serde_json::Value> = agg
        .into_iter()
        .map(|((domain, key, unit), v)| {
            json!({
                "domain": domain, "key": key, "unit": unit,
                "label": labels.get(&(domain.clone(), key.clone(), unit.clone())).cloned().unwrap_or_default(),
                "value": (v * 100.0).round() / 100.0,
            })
        })
        .collect();
    totals.sort_by(|a, b| {
        let da = a["domain"].as_str().unwrap_or("");
        let db2 = b["domain"].as_str().unwrap_or("");
        da.cmp(db2).then(
            a["key"]
                .as_str()
                .unwrap_or("")
                .cmp(b["key"].as_str().unwrap_or("")),
        )
    });

    // 汇总待办（跨账套累计待处理条数）
    let mut todo_total: std::collections::BTreeMap<String, i64> = Default::default();
    for b in &per_book {
        for t in b["todos"].as_array().cloned().unwrap_or_default() {
            let d = t["domain"].as_str().unwrap_or("").to_string();
            *todo_total.entry(d).or_insert(0) += t["count"].as_i64().unwrap_or(0);
        }
    }

    Ok(Json(json!({
        "book_count": total,
        "scanned": books.len(),
        "truncated": truncated,
        "max_books": MAX_BOOKS,
        "failed": failed,
        "domains": domains.into_iter().map(|(k, n)| json!({ "domain": k, "cards": n })).collect::<Vec<_>>(),
        "totals": totals,
        "todos": todo_total.into_iter().map(|(k, v)| json!({ "domain": k, "count": v })).collect::<Vec<_>>(),
        "books": per_book,
    })))
}

async fn consolidate_preview(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该入口仅限系统管理员使用"));
    }
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| Period::from_ymm(state.default_period));
    let keys: Vec<String> = q
        .get("books")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if keys.is_empty() {
        return Err(AppError::bad_request("请选择要合并的账套（books=b1,b2）"));
    }
    // 逐套取数：科目 → (名称, 期末余额)
    let mut per_book: Vec<(
        String,
        String,
        std::collections::BTreeMap<String, (String, Money)>,
    )> = Vec::new();
    let mut book_meta = Vec::new();
    for key in &keys {
        let db = state
            .db_for(key)
            .map_err(|_| AppError::bad_request(format!("账套 {key} 不存在或无法打开")))?;
        let company = db.options().company.clone();
        let chart = accounts::chart(&db)?;
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(period))?;
        let mut map: std::collections::BTreeMap<String, (String, Money)> =
            std::collections::BTreeMap::new();
        for a in chart.all() {
            if !chart.is_leaf(&a.code) {
                continue;
            }
            let end = snap.for_account(&a.code, None).end();
            if end.is_zero() {
                continue;
            }
            map.insert(a.code.clone(), (a.name.clone(), end));
        }
        book_meta.push(json!({ "key": key, "company": company }));
        per_book.push((key.clone(), company, map));
    }
    // 科目并集 → 行（账套列 + 合计）
    let mut codes: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (_, _, m) in &per_book {
        for c in m.keys() {
            codes.insert(c.clone());
        }
    }
    let mut rows = Vec::new();
    for code in &codes {
        let mut values = serde_json::Map::new();
        let mut total = Money::ZERO;
        let mut name = String::new();
        for (key, _, m) in &per_book {
            let v = m
                .get(code)
                .map(|(n, v)| {
                    if name.is_empty() {
                        name = n.clone();
                    }
                    *v
                })
                .unwrap_or(Money::ZERO);
            values.insert(key.clone(), json!(v.fmt_money()));
            total += v;
        }
        rows.push(json!({
            "account_code": code,
            "account_name": name,
            "values": values,
            "total": total.fmt_money(),
        }));
    }
    Ok(Json(json!({
        "period": period_to_str(period),
        "books": book_meta,
        "rows": rows,
        "note": "汇总口径：各账套仅已记账余额（H-3）；v1 为跨账套汇总，内部往来抵销留待 v2",
    })))
}

/// 在制品成本（WIP）：按期间列未完工订单的料/工/费
async fn cost_wip_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": findb::manufacturing::wip_cost(&db, period)?,
    })))
}

/// 可承诺量（ATP）
///
/// 读权限与库存报表同级：ATP 由现货、在途、已占用三项算出，实质是库存视图。
/// 三个分量一并返回而不是只给结果——只看到一个 ATP 数字，被算错了也不知道错在哪。
async fn get_atp(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let item = q.get("item").map(|s| s.trim()).unwrap_or("");
    if item.is_empty() {
        return Err(AppError::bad_request("缺少存货编码 item"));
    }
    let a = findb::scm::atp(&db, item)?;
    let mut out = json!({
        "item": a.item,
        "on_hand": a.on_hand,
        "incoming": a.incoming,
        "incoming_dated": a.incoming_dated,
        "incoming_undated": a.incoming_undated,
        "earliest_ready": a.earliest_ready,
        "committed": a.committed,
        "atp": a.atp,
        "formula": "ATP = 现有可用库存 + 在途（未完工生产计划） − 已占用（已确认订单未发货）",
        "note": "待检（来料检验未转正）库存不计入可用；草稿/作废订单不占用；在途含已下推的草稿产单（草稿也是已认领的产能）",
    });
    // 带 date 时改用「到该日为止可承诺量」口径：只认**有计划完工日且不晚于该日**的
    // 在途。没排期的那部分答不出「什么时候到」，算进来等于给一个自己都给不出的
    // 交期承诺 —— 所以它单列在 incoming_undated，由界面提示「不能用来承诺交期」。
    if let Some(d) = q.get("date").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let on = NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
        let u = findb::scm::atp_until(&db, item, on)?;
        out["as_of"] = json!(d);
        out["incoming"] = json!(u.incoming);
        out["atp"] = json!(u.atp);
        out["date_formula"] = json!("到该日为止 = 现有可用 + 计划完工日 ≤ 该日的在途 − 已占用（**未排期的在途不计入**，它没有交期）");
    }
    Ok(Json(out))
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

/// 销售订单下推生产订单
///
/// 下推数量封顶在引擎里（`scm::prod_from_so`），前端再封一次只会变成两处口径。
async fn prod_from_so_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ProdFromSoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.trim().is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let want = if req.qty.trim().is_empty() {
        None
    } else {
        let q = parse_money_checked(&req.qty)?;
        if !q.is_positive() {
            return Err(AppError::bad_request("下推数量必须大于 0"));
        }
        Some(q)
    };
    let id = findb::scm::prod_from_so(&db, req.so_id, period, date, want, user.username())?;
    let o = findb::scm::prod_get(&db, id)?.ok_or_else(|| AppError::not_found("生产订单不存在"))?;
    let atp = findb::scm::atp(&db, &o.item_code)?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "no": o.no,
        "item_code": o.item_code,
        "item_name": o.item_name,
        "planned_qty": o.planned_qty,
        "so_id": o.so_id,
        "atp": atp,
    })))
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

async fn list_landed_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let rows = findb::landedcost::list(&db, period)?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": rows.iter().map(lc_json).collect::<Vec<_>>(),
        "total_charges": rows.iter().map(|r| r.total_charges()).sum::<fincore::Money>().fmt_money(),
        "total_to_stock": rows.iter().map(|r| r.total_to_stock()).sum::<fincore::Money>().fmt_money(),
        "total_to_expense": rows.iter().map(|r| r.total_to_expense()).sum::<fincore::Money>().fmt_money(),
    })))
}

async fn save_landed_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<LcSaveReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.trim().is_empty() {
        period.first_day().format("%Y-%m-%d").to_string()
    } else {
        req.date.trim().to_string()
    };
    let mut lines = Vec::new();
    for l in &req.lines {
        lines.push(findb::landedcost::LcLine {
            po_id: l.po_id,
            item_code: l.item_code.trim().to_string(),
            item_name: l.item_name.trim().to_string(),
            qty: parse_money_checked(&l.qty)?,
            weight: parse_money_checked(&l.weight)?,
            amount: parse_money_checked(&l.amount)?,
            manual_amount: parse_money_checked(&l.manual_amount)?,
            memo: l.memo.clone(),
            ..Default::default()
        });
    }
    // 借方/贷方科目：留空则退到账套的存货科目与资金账户
    let biz = db.options().biz_accounts.clone();
    let mut lc = findb::landedcost::LandedCost {
        id: req.id,
        no: String::new(),
        period: period.ymm(),
        date: date.clone(),
        supplier_code: req.supplier_code.trim().to_string(),
        supplier_name: req.supplier_name.trim().to_string(),
        basis: if req.basis.trim().is_empty() {
            "amount".to_string()
        } else {
            req.basis.trim().to_string()
        },
        freight: parse_money_checked(&req.freight)?,
        duty: parse_money_checked(&req.duty)?,
        insurance: parse_money_checked(&req.insurance)?,
        clearing: parse_money_checked(&req.clearing)?,
        other: parse_money_checked(&req.other)?,
        dr_account: if req.dr_account.trim().is_empty() {
            biz.material.clone()
        } else {
            req.dr_account.trim().to_string()
        },
        cr_account: if req.cr_account.trim().is_empty() {
            biz.fund.clone()
        } else {
            req.cr_account.trim().to_string()
        },
        expense_account: req.expense_account.trim().to_string(),
        bank_account: req.bank_account.trim().to_string(),
        status: "draft".to_string(),
        voucher_id: None,
        memo: req.memo.clone(),
        prepared_by: user.username().to_string(),
        created_at: String::new(),
        updated_at: String::new(),
        lines: Vec::new(),
    };
    let id = findb::landedcost::save(&db, &mut lc, &lines, user.username())?;
    let after =
        findb::landedcost::get(&db, id)?.ok_or_else(|| AppError::not_found("到岸成本单不存在"))?;
    Ok(Json(
        json!({ "ok": true, "id": id, "row": lc_json(&after) }),
    ))
}

async fn post_landed_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let vid = findb::landedcost::post(&db, id, user.username())?;
    let after =
        findb::landedcost::get(&db, id)?.ok_or_else(|| AppError::not_found("到岸成本单不存在"))?;
    Ok(Json(json!({
        "ok": true,
        "voucher_id": vid,
        "total_to_stock": after.total_to_stock().fmt_money(),
        "total_to_expense": after.total_to_expense().fmt_money(),
        "row": lc_json(&after),
    })))
}

/// 从采购订单取可分摊的存货行
///
/// 刻意只返回**已收到货**的行：没收到的货没有附加成本可摊，摊上去就是给一份
/// 不存在的存货加成本。运费到了货还没到是常事，所以这个限制必须在取数层就做。
async fn landed_cost_from_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let po_id = q
        .get("po_id")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("缺少采购订单 po_id"))?;
    let lines = findb::landedcost::fetch_lines_from_po(&db, po_id)?;
    Ok(Json(json!({
        "po_id": po_id,
        "rows": lines.iter().map(|l| json!({
            "po_id": l.po_id,
            "po_no": l.po_no,
            "item_code": l.item_code,
            "item_name": l.item_name,
            "qty": l.qty.fmt_qty(),
            "weight": l.weight.fmt_qty(),
            "amount": l.amount.fmt_money(),
        })).collect::<Vec<_>>(),
        "note": "只列出已收货的存货行 —— 未收到的货没有附加成本可摊",
    })))
}

/// 成本差异：实际 vs 标准（按订单）
async fn cost_variance_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": findb::manufacturing::cost_variance_report(&db, period)?,
    })))
}

/// 成本预测：BOM 参考料本 × 计划量
async fn cost_forecast_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": findb::manufacturing::cost_forecast_report(&db, period)?,
    })))
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

/// 制造费用分摊：按基准试算/应用（写入 CostType::Overhead 归集）
async fn cost_overhead_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OverheadReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = period_checked(req.period)?;
    let amount = parse_money_checked(&req.amount)?;
    if !amount.is_positive() {
        return Err(AppError::bad_request("分摊金额必须大于 0"));
    }
    let base = findb::manufacturing::OverheadBase::parse(&req.base);
    let rows = findb::manufacturing::overhead_allocate_with(&db, period, amount, base, req.apply)?;
    if req.apply {
        db.log(
            user.username(),
            "成本",
            "制造费用分摊",
            &format!(
                "{} {} {} 笔",
                period_to_str(period),
                amount.fmt_money(),
                rows.len()
            ),
        )?;
    }
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|(id, m)| json!({ "po_id": id, "amount": m.fmt_money() }))
        .collect();
    Ok(Json(json!({
        "ok": true,
        "applied": req.apply,
        "base": base.label(),
        "rows": items,
    })))
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

/// 预算行列表（当前激活版本）
async fn list_budget_rows(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let ver = findb::advanced::bversion_current(&*db)?;
    let rows: Vec<serde_json::Value> = findb::mgmt::budget_list(&db, period)?
        .into_iter()
        .filter(|b| b.version == ver)
        .map(|b| {
            json!({
                "id": b.id, "period": period_to_str(b.period),
                "account_code": b.account_code, "dept": b.dept,
                "amount": b.amount.fmt_money(), "memo": b.memo, "version": b.version,
            })
        })
        .collect();
    Ok(Json(json!({
        "period": period_to_str(period),
        "version": ver,
        "rows": rows,
    })))
}

/// 新增/修改预算行（按 期间+科目+部门+版本 upsert；版本 = 当前激活版本）
async fn save_budget_row(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BudgetRowReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = period_checked(req.period)?;
    if req.account_code.trim().is_empty() {
        return Err(AppError::bad_request("科目编码不能为空"));
    }
    let amount = parse_money_checked(&req.amount)?;
    if amount.is_negative() {
        return Err(AppError::bad_request("预算金额不能为负"));
    }
    let ver = findb::advanced::bversion_current(&*db)?;
    let b = findb::mgmt::Budget {
        id: 0,
        period,
        account_code: req.account_code.trim().to_string(),
        dept: req.dept.trim().to_string(),
        amount,
        memo: req.memo,
        version: ver,
    };
    let id = findb::mgmt::budget_upsert_version(&db, &b)?;
    db.log(
        user.username(),
        "预算",
        "预算编制",
        &format!(
            "{} {} {} {}",
            period_to_str(period),
            b.account_code,
            if b.dept.is_empty() {
                ""
            } else {
                b.dept.as_str()
            },
            b.amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// 删除预算行
async fn delete_budget_row(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    findb::mgmt::budget_delete(&db, id)?;
    db.log(user.username(), "预算", "删除预算行", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

// ---- 导出计划任务（每日定时 CSV） ----

/// 计划任务列表
async fn list_export_schedules(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::exports::sched_list(&*db)? })))
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

/// 新增/修改计划任务
async fn save_export_schedule(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ExportScheduleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let s = findb::exports::ExportSchedule {
        id: req.id,
        kind: req.kind.trim().to_string(),
        period_mode: req.period_mode.trim().to_string(),
        at_time: if req.at_time.trim().is_empty() {
            "08:00".to_string()
        } else {
            req.at_time.trim().to_string()
        },
        enabled: req.enabled.unwrap_or(true),
        last_run: String::new(),
        memo: req.memo,
        created_by: String::new(),
        created_at: String::new(),
    };
    let id = findb::exports::sched_save(&db, &s, user.username())?;
    db.log(
        user.username(),
        "导出",
        "计划任务",
        &format!("#{id} {} {}", s.kind, s.at_time),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

/// 删除计划任务
async fn delete_export_schedule(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    findb::exports::sched_delete(&db, id)?;
    db.log(user.username(), "导出", "删除计划任务", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 立即执行计划任务（写 books_dir/exports/）
async fn run_export_schedule(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let dir = state.books_dir.join("exports");
    let path = findb::exports::sched_run(&db, id, &dir)?;
    db.log(user.username(), "导出", "立即执行计划任务", &path)?;
    Ok(Json(json!({ "ok": true, "path": path })))
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

/// 普通用户自建账套：创建者为该账套的所有者，并成为账套内管理员
async fn create_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<CreateBookReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 账号模型二元化：仅管理员可新建账套（普通账号由管理员开通并邀请进入账套工作）
    if !user.is_admin {
        return Err(AppError::forbidden("仅管理员可新建账套"));
    }
    let owner = user.username.clone();
    // 限制每账号账套数量，防止无限建账占满磁盘
    let owned = state.realm.count_books_of(&owner)?;
    if owned >= MAX_BOOKS_PER_USER {
        return Err(AppError::bad_request(format!(
            "每个账号最多创建 {MAX_BOOKS_PER_USER} 个账套（当前已创建 {owned} 个）"
        )));
    }
    let company = req.company.trim().to_string();
    let key = make_book_key(&req.key, &owner, &state.realm)?;
    let path = state.books_dir.join(format!("{key}.fbk"));
    if path.exists() {
        return Err(AppError::bad_request("账套文件已存在"));
    }
    let mut opts = BookOptions::default();
    if !company.is_empty() {
        opts.company = company.clone();
    }
    if req.start_period > 0 {
        opts.start_period = period_checked(req.start_period)?;
    }
    let db = Db::create_no_admin(&path, &opts)?;
    // 种子所有者为账套内管理员（复制平台口令哈希，便于必要时直接登账套）
    let mut u = User::new(&owner, &user.display_name, Role::Admin);
    if let Ok(Some(ru)) = state.realm.get_user(&owner) {
        u.password_hash = ru.password_hash;
    }
    users::insert(&db, &u)?;
    drop(db);
    // 注册进运行期账套表 + 平台账套目录
    state.books.register(&path, 16);
    state
        .realm
        .register_book(&key, &path.to_string_lossy(), &owner, &company)?;
    Ok(Json(json!({ "key": key, "company": company })))
}

/// 选择当前账套（登录后进入某套账前调用）
async fn select_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let book = state
        .realm
        .get_book(&key)?
        .ok_or_else(|| AppError::not_found("账套不存在"))?;
    // 授权三层：管理员 / 归属者 / 账套内已有该用户的行（被邀请成员）。
    // 注意：账套打不开（文件损坏等）必须透出 500，不能吞成"无权访问"误导用户。
    let is_member = match state.db_for(&key) {
        Ok(db) => users::get(&db, &user.username).ok().flatten().is_some(),
        Err(e) => return Err(e.into()),
    };
    let allowed = user.is_admin || book.owner_username == user.username || is_member;
    if !allowed {
        return Err(AppError::forbidden("无权访问该账套"));
    }
    // 进入账套时把会话当前期间同步为该账套的**当前期间**（最后一个已结账
    // 期间的下一期），不是启用期间。桌面版就是这么做的
    // （crates/finui/src/state.rs 的 `periods::current_period`）。
    //
    // 曾经这里取值是 `db.options().start_period`，后果是：账套结账推到 2026-10
    // 之后，网页一进来仍停在 2025-01，首页整块「本期经营结果」「钱在哪」
    // 显示的是建账首月的数，且不会自己动——老板以为系统算错了。
    if let Ok(db) = state.db_for(&key) {
        let ymm = periods::current_period(&db)
            .unwrap_or_else(|_| db.options().start_period)
            .ymm();
        state.sessions.set_period(&user.token, ymm);
        drop(db);
    }
    state.sessions.set_book_key(&user.token, &key);
    Ok(Json(json!({ "ok": true })))
}

/// 删除账套（管理员 或 账套归属者）：解除「有账套的用户无法删除」的死锁
async fn delete_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let book = state
        .realm
        .get_book(&key)?
        .ok_or_else(|| AppError::not_found("账套不存在"))?;
    if !user.is_admin && book.owner_username != user.username {
        return Err(AppError::forbidden("无权删除该账套"));
    }
    // 删除前先记下本套引用的大附件；共享目录按内容寻址，不能整目录删。
    let own_attach = state
        .db_for(&key)
        .ok()
        .and_then(|db| book_attachment_paths(&db).ok())
        .unwrap_or_default();
    // 顺序要点：先摘运行期注册（否则后续请求会把已删文件重新打开成空库），
    // 再清会话、删目录记录，最后落盘删除文件。
    state.books.unregister(&key);
    state.sessions.clear_book_key(&key);
    state.realm.delete_book(&key)?;
    let p = std::path::PathBuf::from(&book.path);
    if p.exists() {
        if let Err(e) = std::fs::remove_file(&p) {
            error!(path = %p.display(), error = %e, "删除账套文件失败");
        }
        let _ = std::fs::remove_file(format!("{}-wal", p.display()));
        let _ = std::fs::remove_file(format!("{}-shm", p.display()));
    }

    // 删除该账套的手工/自动备份：否则同名 key 被重新创建后，新租户能在
    // 备份列表里看到并恢复上一个租户的数据。
    let backup_dir = state.books_dir.join("backups");
    if let Ok(entries) = std::fs::read_dir(&backup_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if backup_belongs_to(&name, &key) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    // 清理仅属于该账套的大附件。附件是跨账套共享的内容寻址目录，
    // 必须确认其他账套没引用同一个文件；有任何一个账套扫不开就整体放弃，
    // 宁可留下孤儿文件，也不能删掉别的租户还在用的附件。
    if !own_attach.is_empty() {
        let mut used_elsewhere = std::collections::HashSet::new();
        let mut scan_complete = true;
        match std::fs::read_dir(&state.books_dir) {
            Ok(entries) => {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) != Some("fbk") {
                        continue;
                    }
                    if p.file_stem().and_then(|s| s.to_str()) == Some(key.as_str()) {
                        continue;
                    }
                    let conn = match rusqlite::Connection::open_with_flags(
                        &p,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    ) {
                        Ok(c) => c,
                        Err(_) => {
                            scan_complete = false;
                            break;
                        }
                    };
                    let mut stmt = match conn.prepare(
                        "SELECT DISTINCT path FROM attachment \
                         WHERE inline=0 AND path IS NOT NULL AND path <> ''",
                    ) {
                        Ok(s) => s,
                        Err(_) => {
                            scan_complete = false;
                            break;
                        }
                    };
                    let rows = match stmt.query_map([], |r| r.get::<_, String>(0)) {
                        Ok(r) => r,
                        Err(_) => {
                            scan_complete = false;
                            break;
                        }
                    };
                    for row in rows.flatten() {
                        used_elsewhere.insert(row);
                    }
                }
            }
            Err(_) => scan_complete = false,
        }
        if scan_complete {
            let attach_dir = state.books_dir.join(".attachments");
            for rel in own_attach {
                if rel.contains("..") || rel.contains('/') || rel.contains('\\') {
                    continue;
                }
                if !used_elsewhere.contains(&rel) {
                    let _ = std::fs::remove_file(attach_dir.join(&rel));
                }
            }
        }
    }
    Ok(Json(json!({ "ok": true })))
}

async fn post_logout(
    State(state): State<Arc<WebState>>,
    jar: axum_extra::extract::cookie::CookieJar,
) -> Response {
    if let Some(c) = jar.get("finbook_sid") {
        state.sessions.remove(c.value());
    }
    let mut r = Json(json!({"ok": true})).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, clear_cookie_header());
    r
}

async fn get_me(user: CurrentUser) -> Result<Json<PublicUser>, AppError> {
    // 直接使用身份对账后的用户：管理员查看他人账套时是临时身份
    // （不写入账套 user 表），回查数据库会 404
    Ok(Json(PublicUser::from_user(&user.user)))
}

/// 凡是要把口令写进存储的入口，都必须先过这道校验。
///
/// 单密码统一后所有口令入口都走应用级 PasswordPolicy（默认 8 位且需含字母+数字）：
/// Web 层先由本函数做 400 校验，realm 层再以 policy 参数复核，双层一致。
fn check_password(state: &WebState, pwd: &str) -> Result<(), AppError> {
    state.policy().check(pwd).map_err(AppError::bad_request)
}

async fn post_change_password(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<ChangePwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 改的是平台口令（与登录身份一致）
    check_password(&state, &req.new)?;
    let r = state
        .realm
        .change_password(&user.username, &req.old, &req.new, &state.policy())?;
    match r {
        Ok(()) => {
            // 同步到该用户出现过的所有账套内的同名用户行，保持单密码一致
            if let Ok(Some(ru)) = state.realm.get_user(&user.username) {
                let _ = state.realm.sync_password_to_books(
                    &state.books_dir,
                    &user.username,
                    &ru.password_hash,
                    ru.must_change_pwd,
                );
            }
            // 口令已变：吊销本人其他设备上的旧会话，当前设备保留
            state.sessions.remove_others(&user.username, &user.token);
            Ok(Json(json!({"ok": true})))
        }
        Err(msg) => Err(AppError::bad_request(msg)),
    }
}

// ---------------------------------------------------------------------------
// 账号管理（仅管理员）
// ---------------------------------------------------------------------------

async fn list_platform_users(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<Vec<PlatformUserItem>>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    let list = state
        .realm
        .list_users()?
        .into_iter()
        .map(|u| PlatformUserItem::from_realm(&u))
        .collect();
    Ok(Json(list))
}

async fn create_platform_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(req): Json<PlatformUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if req.username.trim().is_empty() {
        return Err(AppError::bad_request("用户名不能为空"));
    }
    check_password(&state, &req.password)?;
    // 管理员开的号，口令是管理员定的——首次登录必须自己改一次
    let id = state.realm.create_user(
        &req.username,
        &req.display_name,
        &req.password,
        req.is_admin,
        true,
        &state.policy(),
    )?;
    Ok(Json(json!({ "id": id })))
}

async fn update_platform_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
    Json(req): Json<UpdatePlatformUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if username == user.username {
        return Err(AppError::bad_request(
            "不能修改当前登录的账号，请使用改密功能",
        ));
    }
    state.realm.update_user(
        &username,
        req.display_name.as_deref(),
        req.disabled,
        req.is_admin,
    )?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_platform_user(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if username == user.username {
        return Err(AppError::bad_request("不能删除当前登录的账号"));
    }
    if state.realm.count_books_of(&username)? > 0 {
        return Err(AppError::bad_request(
            "该用户仍拥有账套，请先删除其账套后再删除账号",
        ));
    }
    state.realm.delete_user(&username)?;
    state.sessions.remove_by_username(&username);
    Ok(Json(json!({"ok": true})))
}

async fn reset_platform_password(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
    Json(req): Json<ResetPwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    check_password(&state, &req.new)?;
    state
        .realm
        .reset_password(&username, &req.new, &state.policy())?;
    // 口令已变，立即吊销该账号的全部旧会话（被盗会话不能继续用满 7 天）
    state.sessions.remove_by_username(&username);
    if let Ok(Some(ru)) = state.realm.get_user(&username) {
        let _ = state.realm.sync_password_to_books(
            &state.books_dir,
            &username,
            &ru.password_hash,
            ru.must_change_pwd,
        );
    }
    Ok(Json(json!({"ok": true})))
}

/// 平台层重置设备绑定：解绑**全部**已绑定设备后，该账号可在任意设备重新登录
/// （重新绑定仍受 `RealmDb::MAX_DEVICES_PER_USER` 上限约束）
async fn reset_platform_device(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("该操作仅限管理员"));
    }
    if username == user.username {
        return Err(AppError::bad_request("不能重置当前登录账号的设备"));
    }
    state.realm.clear_device(&username)?;
    // 强制重新登录：旧会话不能再沿用
    state.sessions.remove_by_username(&username);
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// 用户管理（UserManage）
// ---------------------------------------------------------------------------

async fn list_users(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<PublicUser>>, AppError> {
    user.require(Perm::UserManage)?;
    let db = &user.db;
    let list = users::list(&*db)?
        .into_iter()
        .map(|u| PublicUser::from_user(&u))
        .collect();
    Ok(Json(list))
}

async fn create_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CreateUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    // 权限调整收归管理员：即使被额外授予 UserManage，非管理员也不能铸号分配角色/权限
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以创建账号并分配权限"));
    }
    let username = req.username.trim().to_string();
    if username.is_empty() {
        return Err(AppError::bad_request("用户名不能为空"));
    }
    // Web 端登录只认账号库：账套内子账号必须对应一个已存在的账号，
    // 否则开出来的账号无法登录（死账号）。单密码统一后账套内不再独立设口令，
    // 直接沿用平台口令哈希，前端若传了 password 则忽略（兼容旧前端）。
    let ru = state.realm.get_user(&username)?.ok_or_else(|| {
        AppError::bad_request("该账号尚未开通，请先让管理员在「账号管理」中开通同名账号")
    })?;
    // 账套管理员不能把管理员拉进自己的账套：账套内重置口令会重置平台口令，
    // 否则任何能建账的用户都能借此接管管理员账号。
    if ru.is_admin {
        return Err(AppError::forbidden("不能将管理员加入账套"));
    }
    let db = &user.db;
    if users::get(&db, &username)?.is_some() {
        return Err(AppError::bad_request("该用户名已存在"));
    }
    let mut u = User::new(&username, &req.display_name, req.role);
    u.roles = req
        .roles
        .iter()
        .copied()
        .filter(|r| *r != req.role)
        .collect();
    u.password_hash = ru.password_hash.clone();
    // 管理员开的号，口令是管理员定的——首次登录必须自己改一次
    u.must_change_pwd = req.must_change_pwd;
    u.memo = req.memo;
    // 凭证可见性默认放开（多岗位协作）：需要收紧的账号由管理员在「用户编辑 →
    // 数据范围」逐账号勾选「仅看本人填制的凭证」（过滤机制见 DataScope）
    // 角色基础上的逐项覆盖（管理员角色忽略，避免把自己锁在门外）
    if !u.is_admin() {
        u.extra_perms = req.extra_perms;
        u.deny_perms = req.deny_perms;
    }
    let id = users::insert(&db, &u)?;
    // 账套层的「首登改密」要同步到平台账号，否则不生效。
    //
    // 为什么不生效：登录门禁 `post_login` 读的是**平台层**的
    // `ru.must_change_pwd`（见 post_login 的 LoginResp），账套里那一位只影响
    // 账套内的记录。实测：建号时账套层明确传 `must_change_pwd:false`，平台层
    // 开号时没传（默认 true），结果仍然被拦在「必须先改密」上。
    //
    // 为什么在这里同步而不是在登录时取交集：平台账号是为这个账套的身份而存在
    // 的，管理员在账套里表达的「这个人不用改密」就是他的真实意图；让登录门禁
    // 去扫所有账套成员关系，既多一次 IO，又会出现「A 账套开着、B 账套关着」
    // 这种没法解释的组合。只在**显式传值**时同步，不做隐式覆盖。
    if !req.must_change_pwd && ru.must_change_pwd {
        state.realm.set_must_change_pwd(&username, false)?;
        db.log(
            user.username(),
            "安全",
            "新建用户",
            &format!("账号「{username}」设为免首登改密（同步到平台账号）"),
        )?;
    }
    Ok(Json(json!({"id": id})))
}

async fn update_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
    Json(req): Json<UpdateUserReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let db = &user.db;
    let mut u =
        users::get(&db, &username)?.ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;
    // 账套归属者不可被停用 / 降权 / 改名，否则账套会失去主人
    if let Some(owner) = state.realm.book_owner(&user.book_key)? {
        if username == owner {
            return Err(AppError::bad_request("不能修改账套归属者的账套内身份"));
        }
    }
    // 不能改自己的授权字段：`UserManage` 是一个可单独授予主管的权限，若能改自己，
    // 把 role 设成 Admin 或清空 deny_perms 就是一次自我提权。显示名/备注不涉及授权，
    // 允许自助修改。
    if username == user.username() {
        let touches_grant = req.role.is_some()
            || req.roles.is_some()
            || req.extra_perms.is_some()
            || req.deny_perms.is_some()
            || req.data_scope.is_some()
            || req.disabled == Some(true);
        if touches_grant {
            return Err(AppError::bad_request(
                "不能修改自己的角色、权限矩阵或停用本人，请由其他管理员操作",
            ));
        }
    }
    // 权限调整收归管理员（2026-09 审计）：非管理员即使持有 UserManage，也不能调整
    // 其他账号的角色/权限矩阵/数据范围/停用；显示名、备注等非授权字段仍可代改。
    if username != user.username() {
        let touches_grant = req.role.is_some()
            || req.roles.is_some()
            || req.extra_perms.is_some()
            || req.deny_perms.is_some()
            || req.data_scope.is_some()
            || req.disabled.is_some();
        if touches_grant && !user.user.is_admin() {
            return Err(AppError::forbidden("只有管理员可以调整其他账号的权限"));
        }
    }
    if let Some(d) = req.display_name {
        u.display_name = d;
    }
    if let Some(r) = req.role {
        // 不允许把最后一个管理员改成其他角色
        if u.is_admin() && r != Role::Admin {
            let admins = users::list(&*db)?
                .into_iter()
                .filter(|x| x.is_admin())
                .count();
            if admins <= 1 {
                return Err(AppError::bad_request("至少保留一个系统管理员账号"));
            }
        }
        u.role = r;
    }
    if let Some(rs) = req.roles {
        u.roles = rs.into_iter().filter(|r| *r != u.role).collect();
    }
    if let Some(d) = req.disabled {
        u.disabled = d;
        if d {
            // 停用账号：立即下线其全部会话
            state.sessions.remove_by_username(&username);
        }
    }
    if let Some(m) = req.must_change_pwd {
        u.must_change_pwd = m;
    }
    if let Some(m) = req.memo {
        u.memo = m;
    }
    if let Some(s) = req.data_scope {
        u.data_scope = s;
    }
    if let Some(p) = req.extra_perms {
        u.extra_perms = p;
    }
    if let Some(p) = req.deny_perms {
        u.deny_perms = p;
    }
    users::update(&db, &u)?;
    db.log(
        user.username(),
        "安全",
        "修改用户",
        &format!("更新「{username}」的信息"),
    )?;
    Ok(Json(json!({"ok": true})))
}

async fn reset_user_password(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
    Json(req): Json<ResetPwdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    // 口令即身份：与建号/改权/删号同口径收归套内管理员（持有 UserManage 的非管理员
    // 不得改写任何成员的全局口令——口令同步全部账套）
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以重置账号口令"));
    }
    let db = &user.db;
    // 单密码统一：账套内没有独立口令，重置即重置该用户的平台口令，
    // 再同步到其出现过的所有账套。目标必须是当前账套成员，防越权重置陌生人口令。
    if users::get(&db, &username)?.is_none() {
        return Err(AppError::not_found("该用户不在当前账套"));
    }
    let ru = state
        .realm
        .get_user(&username)?
        .ok_or_else(|| AppError::bad_request("该账号不存在"))?;
    // 管理员的口令只能由管理员在「账号」中重置。账套管理员若能把
    // 管理员邀请进本套再调本接口，就能重置其平台口令并同步到全部账套。
    if ru.is_admin {
        return Err(AppError::forbidden(
            "管理员的口令请由管理员在「账号管理」中重置",
        ));
    }
    // 平台口令是全局的：账套管理员只允许重置「仅属于本账套、且不拥有任何账套」的
    // 成员。否则"把任意账号邀请进自己的账套，再重置其全局口令"即可跨租户
    // 接管/锁死他人账号（受害者口令被改，且会同步覆盖其名下所有账套）。
    let caller_is_platform_admin = state
        .realm
        .get_user(user.username())?
        .map(|u| u.is_admin)
        .unwrap_or(false);
    if !caller_is_platform_admin {
        if state.realm.count_books_of(&username)? > 0 {
            return Err(AppError::forbidden(
                "该账号拥有自己的账套，账套管理员不能重置其口令，请由管理员处理",
            ));
        }
        if state
            .realm
            .count_books_containing(&state.books_dir, &username)?
            > 1
        {
            return Err(AppError::forbidden(
                "该账号还属于其他账套，账套管理员不能重置其口令，请由管理员处理",
            ));
        }
    }
    check_password(&state, &req.new)?;
    state
        .realm
        .reset_password(&username, &req.new, &state.policy())?;
    // 口令已变，立即吊销该账号的全部旧会话（被盗会话不能继续用满 7 天）
    state.sessions.remove_by_username(&username);
    if let Ok(Some(ru)) = state.realm.get_user(&username) {
        let _ = state.realm.sync_password_to_books(
            &state.books_dir,
            &username,
            &ru.password_hash,
            ru.must_change_pwd,
        );
    }
    db.log(
        user.username(),
        "安全",
        "重置口令",
        &format!("重置「{username}」的口令（平台口令）"),
    )?;
    Ok(Json(json!({"ok": true})))
}

async fn reset_user_device(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    // 设备绑定 + 会话下线 = 账号安全操作，收归套内管理员（非管理员可借此踢任意成员下线）
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以重置设备绑定"));
    }
    let db = &user.db;
    // 目标必须是本账套成员：否则凭用户名即可强制下线任意平台用户的全部会话
    if users::get(&db, &username)?.is_none() {
        return Err(AppError::not_found("该用户不在当前账套"));
    }
    users::reset_device(&db, &username)?;
    // 立刻下线该用户**全部**会话（含其它已绑定设备）：旧设备不能靠存量会话继续用
    state.sessions.remove_by_username(&username);
    db.log(
        user.username(),
        "安全",
        "重置设备绑定",
        &format!("重置「{username}」的设备绑定"),
    )?;
    Ok(Json(json!({"ok": true})))
}

async fn unlock_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    // 解锁被停用账号 = 抵消管理员的停用决定，属授权变更，收归套内管理员
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以解锁账号"));
    }
    let db = &user.db;
    security::unlock_user(&db, &username)?;
    db.log(
        user.username(),
        "安全",
        "解锁用户",
        &format!("解锁「{username}」"),
    )?;
    Ok(Json(json!({"ok": true})))
}

/// 角色 → 权限矩阵（供前端在新建/改角色时实时预览）
async fn list_roles(user: CurrentUser) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    let roles: Vec<serde_json::Value> = Role::all()
        .iter()
        .map(|r| {
            let perms: Vec<serde_json::Value> = r
                .perms()
                .iter()
                .map(|p| {
                    let code = serde_json::to_value(*p)
                        .ok()
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    json!({ "code": code, "label": p.label() })
                })
                .collect();
            json!({ "role": serde_json::to_value(*r).ok().and_then(|v| v.as_str().map(|s| s.to_string())).unwrap_or_default(), "label": r.label(), "perms": perms })
        })
        .collect();
    Ok(Json(json!(roles)))
}

async fn delete_user(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::UserManage)?;
    if username == user.username() {
        return Err(AppError::bad_request("不能删除当前登录的账号"));
    }
    // 删号 = 调整他人账号，收归管理员
    if !user.user.is_admin() {
        return Err(AppError::forbidden("只有管理员可以删除账号"));
    }
    let db = &user.db;
    let u =
        users::get(&db, &username)?.ok_or_else(|| AppError::NotFound("用户不存在".to_string()))?;
    // 账套归属者不可删除（删了账套就没有主人了）
    if let Some(owner) = state.realm.book_owner(&user.book_key)? {
        if username == owner {
            return Err(AppError::bad_request("不能删除账套归属者"));
        }
    }
    if u.is_admin() {
        let admins = users::list(&*db)?
            .into_iter()
            .filter(|x| x.is_admin())
            .count();
        if admins <= 1 {
            return Err(AppError::bad_request("至少保留一个系统管理员账号"));
        }
    }
    users::delete(&db, u.id)?;
    state.sessions.remove_by_username(&username);
    db.log(
        user.username(),
        "安全",
        "删除用户",
        &format!("删除账号「{username}」"),
    )?;
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// 账套参数 / 仪表盘 / 期间
// ---------------------------------------------------------------------------

async fn get_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<fincore::BookOptions>, AppError> {
    let db = &user.db;
    // 读参数放宽到「能记账的人」，写仍然要 SysOption。
    //
    // 为什么：前端的凭证编辑器要用 `enable_audit` 决定「记账」按钮的行为，
    // 而 `afterLogin()` 也要读参数。原来会计/出纳读参数一律 403，于是
    // `state.bookOptions = null`、界面按「未开审核」处理 —— 用户完全看不到
    // 「本账套开了审核环节」这个事实，只在点记账被拒时才第一次知道。
    // 让人能读自己每天都要面对的那个开关，成本几乎为零。
    if !user.can(Perm::SysOption) && !(user.can(Perm::VoucherPost) || user.can(Perm::VoucherAudit))
    {
        return Err(AppError::forbidden("没有「账套参数」权限"));
    }
    Ok(Json(db.options()))
}

async fn put_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(opts): Json<fincore::BookOptions>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    // 落库前校验：非法启用期间会灌进会话并污染年度累计口径，后续 first_day/last_day panic
    period_checked(opts.start_period.ymm())?;
    // 字段校验统一走 `BookOptions::validate()`。
    //
    // 原来这里手写了一遍「级长 / 凭证字号」检查，桌面端参数页又只校了
    // 「期间能解析」「级长能解析成数字」—— 同一个字段两端规则不同，这类分叉
    // 最后都表现成「网页存得进、桌面存不进」或者反过来，使用者只会觉得
    // 「保存按钮有时候不灵」。校验留在 fincore，两端共用。
    let problems = opts.validate();
    if !problems.is_empty() {
        return Err(AppError::bad_request(problems.join("；")));
    }
    // set_options 内部还会补一道「业务默认科目必须存在且是末级」的查库校验
    db.set_options(&opts)?;
    Ok(Json(json!({"ok": true})))
}

/// 导出账套参数成 JSON 文件
///
/// 为什么不是「页面上的复制按钮」：换台机器要把它放到指定位置，一个下载文件比
/// 一段要手动粘进编辑器的文本可靠。整个 `BookOptions` 原样出去，没有删减——
/// 少导出一个字段，导入后就会静默回到默认值。
async fn export_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let opts = db.options();
    let body = serde_json::to_vec_pretty(&opts)
        .map_err(|e| AppError::Internal(format!("序列化账套参数失败：{e}")))?;
    let name = format!(
        "{}_账套参数_{}.json",
        if opts.company.trim().is_empty() {
            "账套"
        } else {
            opts.company.trim()
        },
        chrono::Local::now().format("%Y%m%d")
    );
    let mut resp = Response::builder()
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8");
    if let Some(hv) = attachment_header(&name) {
        resp = resp.header(header::CONTENT_DISPOSITION, hv);
    }
    resp.body(Body::from(body))
        .map_err(|e| AppError::Internal(format!("生成下载响应失败：{e}")))
}

/// 导入账套参数
///
/// 结构项（编码级长 / 启用期间）与当前账套不同就整体拒绝，一个字段都不改 ——
/// 见 `BookOptions::adopt_config`。
async fn import_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(incoming): Json<fincore::BookOptions>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let mut opts = db.options();
    if let Err(problems) = opts.adopt_config(&incoming) {
        return Err(AppError::bad_request(problems.join("；")));
    }
    period_checked(opts.start_period.ymm())?;
    db.set_options(&opts)?;
    Ok(Json(json!({"ok": true, "options": opts})))
}

/// 恢复默认（只清偏好类设置）
///
/// 企业名称、编码级长、启用期间、默认科目、审核/出纳开关都不动 ——
/// 它们与已经记过的账绑在一起，见 `BookOptions::reset_preferences`。
async fn reset_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let mut opts = db.options();
    opts.reset_preferences();
    db.set_options(&opts)?;
    Ok(Json(json!({"ok": true, "options": opts})))
}

/// 打印预览页上的调整直接落库。
///
/// 收的是 [`fincore::print::PrintPatch`]（全是 `Option`），只合并请求里带来的字段。
/// 为什么不做成「整份 `PrintOptions` 覆盖」：预览页不认识的字段（凭证/账簿/单据
/// 纸型）会被 `serde(default)` 悄悄重置成默认值，用户下次打凭证才发现纸型没了。
///
/// 权限跟「系统设置」一致：版式是**账套级**的，改一次所有人都变。没有这个权限的
/// 账号，预览页只给「本次打印」的临时调整（页面会写明）。
async fn post_print_settings(
    user: CurrentUser,
    Json(patch): Json<fincore::print::PrintPatch>,
) -> Result<Json<fincore::PrintOptions>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let mut opts = db.options();
    opts.print.apply_patch(&patch);
    // 校验只走 `Db::set_options` 里那一道。
    //
    // 这里曾经又写了一遍 `opts.validate()`——两层校验看着更保险，实际是「两层都拦」
    // 谁也证明不了另一层有没有在干活：把外层删掉，测试照样绿，`set_options` 里那道
    // 就已经把越界值挡回去了。真正的守卫只有一处，也就只有一条变异能盯住它。
    db.set_options(&opts)?;
    Ok(Json(opts.print))
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

async fn get_dashboard(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Dashboard>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let (v, e, a) = db.stats()?;
    let start = db.options().start_period;
    let cur = current_period(&state, &user);
    let closed = periods::closed_upto(&*db)?;
    Ok(Json(Dashboard {
        company: user.company.clone(),
        start_period: period_to_str(start),
        current_period: period_to_str(cur),
        closed_upto: closed.map(period_to_str),
        vouchers: v,
        entries: e,
        accounts: a,
        brief: home_brief(db, cur).ok(),
    }))
}

/// 首页首屏两块：本期赚没赚（利润口径）+ 钱在哪（往来口径）
///
/// 取数全部复用报表那条路径（`findb::advanced::financial_totals` /
/// `financial_analysis` / `money_position`），**不另算一套**。首页自己算一遍
/// 的下场是「首页说赚 10 万，点进利润表是 8.5 万」，而两边都"看起来对"。
fn home_brief(db: &findb::Db, cur: Period) -> Result<HomeBrief, AppError> {
    let t = findb::advanced::financial_totals(db, cur, cur, None)?;
    // 本年累计：不再用 `financial_totals(db, cur, 年初, ..)`——那是另一组查询键，
    // 等于把整本账重扫一遍（6000 张凭证规模下约 8ms）。快照里已经算好了
    // 1 月至本期的累计发生额，`financial_ytd` 命中同一份缓存直接读它。
    let ytd = findb::advanced::financial_ytd(db, cur, None)?;
    // 上月净利润：只为了算环比。"这个月赚 8 万"本身没有意义，老板真正想
    // 知道的是"比上个月好还是差"。取不到（建账首月）就当 0。
    let prev_np = findb::advanced::financial_totals(db, cur.prev(), cur.prev(), None)
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
    let mp = findb::advanced::money_position(db, cur, as_of, None)
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

/// 我的工作台：按岗位权限动态聚合业务卡片 / 我的待办 / 多期趋势。
/// 凭证/资金/销售/采购/仓管/生产/成本/报销/审批 域在 findb::workbench（复用既有 Perm），
/// 报表域（语句表）在此注入——门槛 Report（人人首页可进），域内再按各域权限裁剪。
async fn get_workbench(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<findb::workbench::WbOut>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let cur = current_period(&state, &user);
    let n = q
        .get("periods")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(12)
        .clamp(3, 24);
    let mut out = findb::workbench::collect(&db, &user.user, cur, n)?;
    if user.can(Perm::FinReport) {
        wb_report_domain(&db, &user, cur, n, &mut out)?;
    }
    Ok(Json(out))
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

/// Ctrl+K 快速搜索：凭证（复用 VoucherQuery → data_scope 落地）/ 采购订单 / 销售订单 /
/// 请购（后三类需 OrderOps）/ 我的报销（doc_in_scope 数据范围，防跨人泄露）。
async fn quick_search(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let kw = q.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    if kw.is_empty() {
        return Ok(Json(json!({ "rows": [] })));
    }
    let db = &user.db;
    let cur = current_period(&state, &user);
    let like = format!("%{kw}%");
    let mut rows: Vec<serde_json::Value> = Vec::new();

    // 凭证：复用列表口径（关键字 = 摘要/科目/凭证号，数据范围同步生效）
    let vq = findb::vouchers::VoucherQuery {
        keyword: Some(kw.clone()),
        limit: Some(4),
        asc: false,
        ..Default::default()
    }
    .with_data_scope(&user.user);
    for v in findb::vouchers::list(&db, &vq)? {
        rows.push(json!({
            "kind": "voucher",
            "id": v.id,
            "label": format!("{}-{} {}", v.word, v.no, v.memo),
            "sub": v.date.format("%Y-%m-%d").to_string(),
            "view": "vouchers",
        }));
    }

    if user.can(Perm::OrderOps) {
        let pack = |kind: &str,
                    view: &str,
                    sql: &str,
                    db: &findb::Db,
                    like: &str|
         -> Vec<serde_json::Value> {
            let mut out = Vec::new();
            if let Ok(mut st) = db.conn().prepare(sql) {
                if let Ok(iter) = st.query_map(rusqlite::params![like, like], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                }) {
                    for row in iter.flatten() {
                        out.push(json!({
                            "kind": kind,
                            "id": row.0,
                            "label": format!("{} {}", row.1, row.2),
                            "sub": "",
                            "view": view,
                        }));
                    }
                }
            }
            out
        };
        rows.extend(pack(
            "po",
            "po-doc",
            "SELECT id, no, supplier_name FROM purchase_order WHERE no LIKE ?1 OR supplier_name LIKE ?2 ORDER BY id DESC LIMIT 3",
            &db,
            &like,
        ));
        rows.extend(pack(
            "so",
            "so-doc",
            "SELECT id, no, customer_name FROM sales_order WHERE no LIKE ?1 OR customer_name LIKE ?2 ORDER BY id DESC LIMIT 3",
            &db,
            &like,
        ));
        rows.extend(pack(
            "req",
            "po-doc",
            "SELECT id, no, item_name FROM purchase_req WHERE no LIKE ?1 OR item_name LIKE ?2 ORDER BY id DESC LIMIT 3",
            &db,
            &like,
        ));
    }

    // 档案：编码/名称直达辅助档案页（kind 带到前端切页签）。
    // 门槛与档案页的新增按钮一致（AuxEdit），只读账号不打扰。
    if user.can(Perm::AuxEdit) {
        let kind_label = |k: &str| match k {
            "customer" => "客户",
            "supplier" => "供应商",
            "dept" => "部门",
            "employee" => "职员",
            "project" => "项目",
            "item" => "存货",
            "bank" => "银行账户",
            _ => "档案",
        };
        if let Ok(mut st) = db.conn().prepare(
            "SELECT id, kind, code, name FROM aux_entity
             WHERE disabled = 0 AND (code LIKE ?1 OR name LIKE ?2)
             ORDER BY kind, code LIMIT 6",
        ) {
            if let Ok(iter) = st.query_map(rusqlite::params![like, like], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            }) {
                for row in iter.flatten() {
                    rows.push(json!({
                        "kind": "aux",
                        "id": row.0,
                        "label": format!("{} {} {}", kind_label(&row.1), row.2, row.3),
                        "sub": "",
                        "view": "aux",
                        "aux_kind": row.1,
                    }));
                }
            }
        }
    }

    // 我的报销（数据范围与列表页一致）
    let mut mine = 0;
    for c in findb::business::claim_list(&db, cur, None)? {
        if mine >= 3 {
            break;
        }
        if !doc_in_scope(&user, &c.applicant) {
            continue;
        }
        if c.no.contains(&kw) || c.reason.contains(&kw) {
            rows.push(json!({
                "kind": "claim",
                "id": c.id,
                "label": format!("{} {}", c.no, c.reason),
                "sub": c.applicant,
                "view": "claims",
            }));
            mine += 1;
        }
    }

    Ok(Json(json!({ "rows": rows })))
}

/// 管理员「账目总览」：只读视角的账目全貌（仅系统管理员可访问）
async fn get_overview(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.user.is_admin() {
        return Err(AppError::forbidden("该入口仅限系统管理员使用"));
    }
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    let o = findb::reports::overview(&db, period)?;
    let recent: Vec<VoucherListItem> = o.recent.iter().map(to_item).collect();
    let a = findb::advanced::financial_analysis(&db, period, Some(&user.user))?;
    let trend: Vec<serde_json::Value> = a
        .trend
        .iter()
        .map(|t| {
            json!({
                "period": period_to_str(t.period),
                "revenue": t.revenue.fmt_money(),
                "cost": t.cost.fmt_money(),
                "net_profit": t.net_profit.fmt_money(),
                "cum_revenue": t.cum_revenue.fmt_money(),
                "cum_cost": t.cum_cost.fmt_money(),
                "cum_net_profit": t.cum_net_profit.fmt_money(),
                "anomaly_revenue": t.anomaly_revenue,
                "anomaly_cost": t.anomaly_cost,
                "anomaly_net_profit": t.anomaly_net_profit,
            })
        })
        .collect();
    let driver_json = |d: &findb::advanced::DriverItem| {
        json!({
            "name": d.name,
            "amount": d.amount.fmt_money(),
            "prev_amount": d.prev_amount.fmt_money(),
        })
    };
    Ok(Json(json!({
        "company": o.company,
        "period": period_to_str(o.period),
        "closed_upto": o.closed_upto.map(period_to_str),
        "vouchers": o.vouchers,
        "entries": o.entries,
        "accounts": o.accounts,
        "unposted": o.unposted,
        "posted": o.posted,
        "totals": {
            "total_asset": o.totals.total_asset.fmt_money(),
            "total_liab": o.totals.total_liab.fmt_money(),
            "equity": o.totals.equity.fmt_money(),
            "revenue": o.totals.revenue.fmt_money(),
            "cost": o.totals.cost.fmt_money(),
            "net_profit": o.totals.net_profit.fmt_money(),
        },
        "invoice_in": {"amount_tax": o.invoice_in.0.fmt_money(), "count": o.invoice_in.1},
        "invoice_out": {"amount_tax": o.invoice_out.0.fmt_money(), "count": o.invoice_out.1},
        "recent": recent,
        "analysis": {
            "trend": trend,
            "revenue_drivers": a.revenue_drivers.iter().map(driver_json).collect::<Vec<_>>(),
            "cost_drivers": a.cost_drivers.iter().map(driver_json).collect::<Vec<_>>(),
            "profit_drivers": a.profit_drivers.iter().map(driver_json).collect::<Vec<_>>(),
            "anomaly_notes": a.anomaly_notes,
            "ratios": a.ratios.iter().map(|r| json!({
                "key": r.key, "name": r.name, "value": r.value.fmt_plain(), "display": r.display, "formula": r.formula,
            })).collect::<Vec<_>>(),
        },
    })))
}

async fn get_periods(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let start = db.options().start_period;
    let this_year = Period::default().year();
    let end = Period::new(this_year + 1, 12).unwrap_or_default();
    let list: Vec<String> = if end >= start {
        Period::range(start, end)
            .into_iter()
            .map(period_to_str)
            .collect()
    } else {
        vec![period_to_str(start)]
    };
    let cur = current_period(&state, &user);
    let closed = periods::closed_upto(&*db)?;
    Ok(Json(json!({
        "current": period_to_str(cur),
        "closed_upto": closed.map(period_to_str),
        "start": period_to_str(start),
        "list": list,
    })))
}

async fn post_period(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (year, month) = (req.ymm / 100, req.ymm % 100);
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) {
        return Err(AppError::bad_request(format!(
            "非法期间 {ymm}：应为 YYYYMM（年份 1970-9999，月份 1-12）",
            ymm = req.ymm
        )));
    }
    state.sessions.set_period(&user.token, req.ymm);
    Ok(Json(
        json!({"ok": true, "period": period_to_str(period_checked(req.ymm)?)}),
    ))
}

/// 期末结账请求
#[derive(Deserialize, Default)]
struct ClosePeriodReq {
    /// 是否要求先结转损益才能结账
    #[serde(default)]
    pub require_carry: bool,
}

/// 期末预检：返回结账前需要处理的问题清单与损益概况
/// 月结向导：返回带状态的步骤清单
///
/// 与 `/precheck` 的区别只是形状：precheck 是给人看的错误串，这里是给界面画的清单。
/// 判定口径必须一致 —— `findb::month_close` 有测试钉住这件事。
async fn period_wizard(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let mc = findb::month_close::report(&user.db, period)?;
    let steps: Vec<serde_json::Value> = mc
        .steps
        .iter()
        .map(|s| {
            json!({
                "code": s.code,
                "label": s.label,
                "ok": s.ok,
                "blocking": s.blocking,
                "detail": s.detail,
                "hint": s.hint,
            })
        })
        .collect();
    Ok(Json(json!({
        "period": period_to_str(period),
        "closed": mc.closed,
        "can_close": mc.can_close(),
        "steps": steps,
        "finance_ready": mc.can_close(),
    })))
}

async fn period_precheck(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    let issues = periods::precheck(&db, period, true)?;
    let chart = accounts::chart(&db)?;
    // 预检展示与【结转】按钮同口径：结转按含草稿取数（见 period_carry_forward）
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(period).with_posted_only(false))?;
    let pl_rows = snap.carry_forward_rows(&chart);
    let profit = snap
        .for_account(fincore::engine::period_end::PROFIT_ACCOUNT, None)
        .end();
    Ok(Json(json!({
        "period": period_to_str(period),
        "issues": issues,
        "pl_count": pl_rows.len(),
        "profit_balance": profit.fmt_money(),
        "closed_upto": periods::closed_upto(&*db)?.map(period_to_str),
    })))
}

/// 结转损益：生成一张把损益类科目净额转入「本年利润」的凭证
async fn period_carry_forward(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CarryForward)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    let chart = accounts::chart(&db)?;
    // 结转是"操作"不是报表：按含未记账（排除作废）取数。结转生成的凭证本身是草稿，
    // 若按已记账口径取数，草稿看不见 → 第二次调用时损益仍未清零 → 重复结转。
    // 结账前 checklist 要求全部记账，届时两种口径结果相同（H-3 定案的已记账口径
    // 用于余额表/报表/账簿，不用于本操作的幂等判定）。
    // 取数、幂等判断、取号、落库必须同事务：原实现先读余额再单独 save，
    // 两个并发请求都能读到“有损益”，各自生成一张结转凭证；等第二张提交时，
    // 损益已经被第一张清零，账上就多了一笔重复结转。BEGIN IMMEDIATE 让后来者
    // 等前者提交后再取数，第二次看到的净额已是 0。
    let tx = db.write_tx()?;
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(period).with_posted_only(false))?;
    let rows = snap.carry_forward_rows(&chart);
    if rows.is_empty() {
        return Err(AppError::bad_request("本期损益类（含成本类）科目没有发生额，无需结转"));
    }
    // 已结转过就先说清楚，不要让用户对着「没有可结转的分录」去猜。
    //
    // 账结法天生幂等：结转凭证把各损益科目的本期净发生额清零，再点一次自然没有
    // 分录可结。功能上没错，错的是提示——原来只说「净发生额均为 0」，用户无法
    // 区分「本期压根没做业务」和「已经结过了」，只能去翻凭证。实测重复点击时账面
    // 明明有 658,000 的损益额，提示却说「损益发生额均为零」。
    //
    // 这里主动查本期已有的结转凭证，把因果和补救一起给出。
    let carried: Vec<String> = vouchers::list(&db, &vouchers::VoucherQuery::period(period))?
        .into_iter()
        .filter(|v| v.source == VoucherSource::CarryForward && v.status != VoucherStatus::Void)
        .map(|v| format!("{} #{}", v.word, v.no))
        .collect();
    if !carried.is_empty() && rows.iter().all(|r| r.debit.round2().eq(&r.credit.round2())) {
        return Err(AppError::bad_request(format!(
            "本期损益已结转过（{}），各损益科目净发生额已清零，无需重复结转。\
             如需重新结转，请先删除该凭证再执行",
            carried.join("、")
        )));
    }
    let date = period.last_day();
    let word = db
        .voucher_words()
        .first()
        .cloned()
        .unwrap_or_else(|| "记".to_string());
    let no = vouchers::next_no_of(&tx, period, &word)?;
    let mut v = fincore::engine::period_end::generate_carry_forward(
        period,
        date,
        &word,
        no,
        &rows,
        &chart,
        fincore::engine::period_end::PROFIT_ACCOUNT,
        user.username(),
    )?;
    let id = vouchers::save_in(&tx, &mut v)?;
    tx.commit().map_err(findb::DbError::from)?;
    db.log(
        user.username(),
        "期末",
        "结转损益",
        &format!("{} 凭证 #{}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id, "voucher_no": v.voucher_no() })))
}

/// 年末结转：把「本年利润」余额转入「利润分配—未分配利润」
async fn period_year_end(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CarryForward)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    let chart = accounts::chart(&db)?;
    // 同 period_carry_forward：含草稿取数，保证第一次生成的结转草稿能清零 4103，
    // 第二次调用（余额 0）才会被拒，否则同一笔利润会被结转两次。
    // 取数与落库放同一事务，避免并发请求都读到结转前余额。
    let tx = db.write_tx()?;
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(period).with_posted_only(false))?;
    let profit = snap
        .for_account(fincore::engine::period_end::PROFIT_ACCOUNT, None)
        .end();
    let date = period.last_day();
    let word = db
        .voucher_words()
        .first()
        .cloned()
        .unwrap_or_else(|| "记".to_string());
    let no = vouchers::next_no_of(&tx, period, &word)?;
    let mut v = fincore::engine::period_end::generate_year_end_carry(
        period,
        date,
        &word,
        no,
        profit,
        fincore::engine::period_end::UNDISTRIBUTED_ACCOUNT,
        &chart,
        user.username(),
    )?;
    let id = vouchers::save_in(&tx, &mut v)?;
    tx.commit().map_err(findb::DbError::from)?;
    db.log(
        user.username(),
        "期末",
        "年末结转",
        &format!("{} 凭证 #{}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id, "voucher_no": v.voucher_no() })))
}

/// 期末结账：有未处理问题时返回 400 + 问题清单
async fn period_close(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
    Json(req): Json<ClosePeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    let issues = periods::close(&db, period, user.username(), req.require_carry)?;
    if !issues.is_empty() {
        return Err(AppError::bad_request(issues.join("；")));
    }
    Ok(Json(json!({ "ok": true, "closed": period_to_str(period) })))
}

/// 反结账
async fn period_unclose(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(ymm): Path<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::PeriodClose)?;
    let period = period_checked(ymm)?;
    let db = &user.db;
    periods::unclose(&db, period, user.username())?;
    Ok(Json(
        json!({ "ok": true, "unclosed": period_to_str(period) }),
    ))
}

// ---------------------------------------------------------------------------
// 科目 / 凭证
// ---------------------------------------------------------------------------

async fn list_accounts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<fincore::Account>>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(accounts::list(&*db)?))
}

/// 补齐内置科目表（旧账套补入新版本新增的科目，仅管理员）
async fn fill_default_accounts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.user.is_admin() {
        return Err(AppError::forbidden("该操作仅限系统管理员"));
    }
    let db = &user.db;
    let before = accounts::list(&*db)?.len();
    let inserted = accounts::fill_missing_defaults(&*db)?;
    if inserted > 0 {
        db.log(
            user.username(),
            "科目",
            "补齐科目表",
            &format!(
                "补入 {inserted} 个内置科目（{before} → {}）",
                before + inserted
            ),
        )?;
    }
    Ok(Json(json!({
        "inserted": inserted,
        "total": before + inserted,
    })))
}

async fn next_voucher_no(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let word = q.get("word").cloned().unwrap_or_else(|| "记".to_string());
    let no = vouchers::next_no(&db, period, &word)?;
    Ok(Json(json!({"no": no})))
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

async fn list_vouchers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<VoucherListItem>>, AppError> {
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = &user.db;
    let mut query = voucher_query_from(&q, &user);
    // 上限 1000：limit 传负数时 SQLite 视为不限制，会把全库凭证+分录读进内存
    query.limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .map(|v| v.clamp(1, 1000))
        .or(Some(200));
    let mut list = vouchers::list(&db, &query)?;
    // 列表需要摘要与借贷合计：`vouchers::list` 只读表头，这里批量补充分录
    vouchers::fill_entries(&db, &mut list)?;
    // 数据范围：科目区间等限制需逐张过滤（查询层只处理了「仅本人凭证」）
    list.retain(|v| user.user.can_see_voucher(v));
    let list = list.iter().map(to_item).collect();
    Ok(Json(list))
}

/// 凭证列表（分页 + 全量合计）。
///
/// 为什么必须有这个端点：老的 `GET /api/vouchers` 默认 `limit=200`，而排序
/// 是 `asc=true`（日期+凭证号升序）。于是一个期间超过 200 张凭证后，
/// **后面的凭证通过界面完全看不到**，而且界面上没有任何提示说「还有更多」。
/// 对一个月两三百张的单据，这事每天都在发生。
///
/// 参数非法一律 400（`page=abc` 不能静默当第 1 页——那会让调用方拿到
/// 「看起来正常但其实是首页」的数据，漏拉很难发现）。
async fn list_vouchers_page(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<VoucherPage>, AppError> {
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = &user.db;
    let num = |k: &str, def: i64| -> Result<i64, AppError> {
        match q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            None => Ok(def),
            Some(t) => t
                .parse::<i64>()
                .map_err(|_| AppError::bad_request(format!("参数 {k} 必须是整数，收到：{t}"))),
        }
    };
    let page = num("page", 1)?.max(1);
    // 单页上限 500：一次拉太多只会把浏览器和服务端一起拖垮。
    // 要全量导出走 export 端点，不该靠调大 page_size。
    let page_size = num("page_size", 50)?.clamp(1, 500);
    let offset = (page - 1) * page_size;

    let query = voucher_query_from(&q, &user);
    let (mut rows, total) = vouchers::list_page(&db, &query, offset, page_size)?;
    vouchers::fill_entries(&db, &mut rows)?;

    // 兜底：科目区间已由 SQL 下推（vouchers::build_where），这里再 retain 一次。
    // 若真的过滤掉了行，说明 SQL 条件与 can_see_voucher 不一致 —— 那时 total 就是
    // 错的，页码会算错。与其默默给用户一个打不开的末页，不如把 9 张说成 10 张这
    // 类错误暴露出来。所以这里显式断言一致。
    let before = rows.len();
    rows.retain(|v| user.user.can_see_voucher(v));
    debug_assert_eq!(
        before,
        rows.len(),
        "SQL 下推的数据范围与 can_see_voucher 不一致，total 会算错页码"
    );

    // 合计走全量（不分页）：财务界面要核对的是本期间总账数字
    let (debit_total, credit_total) = vouchers::totals(&db, &query)?;
    Ok(Json(VoucherPage {
        items: rows.iter().map(to_item).collect(),
        page,
        page_size,
        total,
        debit_total: debit_total.fmt_money(),
        credit_total: credit_total.fmt_money(),
    }))
}

async fn get_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<VoucherDetail>, AppError> {
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    // 数据权限：非全量权限用户不得查看自己不可见的凭证
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权查看该凭证"));
    }
    let mut detail = VoucherDetail::from_voucher(v);
    // 更正链不是凭证本身的内容（`Voucher` 结构里没有这三个字段），
    // 所以查凭证接口顺带补上，前端一次请求就能画出链。
    detail.amend = vouchers::amend_link(&db, id)?;
    Ok(Json(detail))
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

async fn save_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SaveVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 新增需 VoucherNew，修改需 VoucherEdit
    if req.id > 0 {
        user.require(Perm::VoucherEdit)?;
    } else {
        user.require(Perm::VoucherNew)?;
    }
    let db = &user.db;
    // 期间非法（如 202513）直接 400：静默落到当前期间会把凭证写进错误的账期。
    // 0 保持原语义 = 当前期间（与 renumber / reverse 一致）。
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    let word = if req.word.is_empty() {
        "记".to_string()
    } else {
        req.word.clone()
    };

    // 修改时保留原分录的未提交要素（辅助核算/数量/外币/结算号等）
    let mut prev_entries: Vec<Entry> = Vec::new();
    let mut v = if req.id > 0 {
        let mut existing = vouchers::get(&db, req.id)?
            .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
        // 数据权限校验：非全量权限用户不得修改自己不可见的凭证
        if !user.user.can_see_voucher(&existing) {
            return Err(AppError::forbidden("无权修改该凭证"));
        }
        if !existing.status.can_edit() {
            return Err(AppError::forbidden(
                "该凭证已记账或已作废，不能修改（已记账请先反记账）",
            ));
        }
        // 启用审核环节的账套：已审核凭证须先反审核（预检给 400，避免落到引擎错误的 500）
        if existing.status == VoucherStatus::Audited && db.options().enable_audit {
            return Err(AppError::bad_request("已审核凭证不能修改，请先反审核"));
        }
        // 日期不能漂移到凭证期间之外（期间本身不可改，改的是日期）
        if (date.year(), date.month()) != (existing.period.year(), existing.period.month()) {
            return Err(AppError::bad_request(format!(
                "凭证日期 {} 不在其所属期间 {} 内",
                date.format("%Y-%m-%d"),
                existing.period.label()
            )));
        }
        // 凭证字在修改时不可变：号段按 (期间, 凭证字, 号码) 唯一，中途换字会让本凭证
        // 占住另一字下的同一号（那可能已经是别人的凭证），或在该字下留一个永久断号——
        // 因为 existing.no 不会跟着重新取号。与上面"期间不可改"同一口径的处理：
        // 要换字请作废后重录。客户端不传 word（空串）时视为不表达意图，保留原字。
        let explicit_word = req.word.trim();
        if !explicit_word.is_empty() && explicit_word != existing.word {
            return Err(AppError::bad_request(format!(
                "凭证字不可由「{}」改为「{}」（号段按凭证字独立编号），如需换字请作废后重录",
                existing.word, explicit_word
            )));
        }
        existing.date = date;
        existing.attachments = req.attachments;
        existing.memo = req.memo.clone();
        prev_entries = existing.entries.clone();
        existing.entries.clear();
        existing
    } else {
        if (date.year(), date.month()) != (period.year(), period.month()) {
            return Err(AppError::bad_request(format!(
                "凭证日期 {} 不在所选期间 {} 内",
                date.format("%Y-%m-%d"),
                period.label()
            )));
        }
        let no = if req.no > 0 {
            // 调用方自选号码撞上 UNIQUE(period,word,no) 时 rusqlite 约束错误会一路冒成
            // AppError::Db → HTTP 500（看着像服务端坏了，其实是用户填重了）。
            // 预先查一次占用并回 400；并发下仍可能漏判，那时唯一索引继续兜底。
            if vouchers::no_taken(&db, period, &word, req.no, 0)? {
                return Err(AppError::bad_request(format!(
                    "凭证号 {}-{:04} 已被占用，请换一个号或留空自动取号",
                    word, req.no
                )));
            }
            req.no
        } else {
            vouchers::next_no(&db, period, &word)?
        };
        let mut v = Voucher::new(period, date, word, no);
        v.attachments = req.attachments;
        v.memo = req.memo.clone();
        v
    };
    v.prepared_by = user.username().to_string();
    // 加载科目表：银行尾号简写需要按科目表解析为完整编码
    let chart = accounts::chart(&db)?;
    for (i, e) in req.entries.iter().enumerate() {
        let account_code = normalize_bank_account(&chart, &e.account_code)
            .unwrap_or_else(|| e.account_code.clone());
        let mut en = Entry::new(e.line, account_code.clone(), e.summary.clone());
        en.debit = parse_money_checked(&e.debit)?;
        en.credit = parse_money_checked(&e.credit)?;
        // 未提交的要素沿用原行：Web 编辑桌面录入的凭证时，辅助/数量/外币/结算号不丢
        if let Some(prev) = prev_entries.get(i) {
            en.aux = prev.aux.clone();
            en.qty = prev.qty;
            en.price = prev.price;
            en.currency = prev.currency.clone();
            en.rate = prev.rate;
            en.amount_for = prev.amount_for;
            en.settle_type = prev.settle_type.clone();
            en.settle_no = prev.settle_no.clone();
            en.biz_date = prev.biz_date;
        }
        if let Some(aux) = &e.aux {
            en.aux = aux.clone();
        }
        if let Some(q) = e.qty {
            en.qty = Some(q);
        }
        if let Some(p) = e.price {
            en.price = Some(p);
        }
        if let Some(cur) = e
            .currency
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            en.currency = Some(cur.to_ascii_uppercase());
        }
        if let Some(r) = e.rate {
            en.rate = Some(r);
        }
        if let Some(a) = e.amount_for {
            en.amount_for = Some(a);
        }
        if let Some(cf) = e.cf.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            en.aux.cash_flow = Some(cf.to_string());
        }
        // 银行科目：尾号存入辅助核算银行字段（仅在未显式提供时兜底）
        if en.account_code.starts_with("1002") && en.aux.bank.is_none() {
            en.aux.bank = Some(account_code);
        }
        v.entries.push(en);
    }
    if !v.balanced() {
        return Err(AppError::bad_request("借贷不平衡，请检查分录金额"));
    }
    // 预算控制（账套参数 budget_control：off/warn/strong）：费用/成本类借方分录比对预算执行
    let budget_ctl = db.options().budget_control.trim().to_string();
    if budget_ctl == "warn" || budget_ctl == "strong" {
        let adds: Vec<(String, String, Money)> = v
            .entries
            .iter()
            .filter(|e| e.debit.is_positive())
            .map(|e| {
                (
                    e.account_code.clone(),
                    e.aux.dept.clone().unwrap_or_default(),
                    e.debit,
                )
            })
            .collect();
        let overs = findb::mgmt::budget_check(&db, v.period, &adds)?;
        if !overs.is_empty() {
            let msg = overs
                .iter()
                .map(|o| {
                    format!(
                        "{} {} 预算 {} 已执行 {} 本次 {} 超 {}",
                        o.account_code,
                        if o.dept.is_empty() {
                            ""
                        } else {
                            o.dept.as_str()
                        },
                        o.budget.fmt_money(),
                        o.actual.fmt_money(),
                        o.add.fmt_money(),
                        o.over.fmt_money()
                    )
                })
                .collect::<Vec<_>>()
                .join("；");
            if budget_ctl == "strong" {
                return Err(AppError::bad_request(format!("预算强控：{msg}")));
            }
            db.log(user.username(), "预算", "超预算提醒", &msg)?;
        }
    }
    // 保存为「未记账」，核对无误后在界面点「记账」确认入账（无审核环节）
    v.status = VoucherStatus::Draft;
    let id = vouchers::save(&db, &mut v)?;
    // 凭证已提交：日志失败不得把请求打成 500，否则客户端重试会多录一张凭证
    if let Err(e) = db.log(
        user.username(),
        "凭证",
        if req.id > 0 { "修改" } else { "新增" },
        &v.voucher_no(),
    ) {
        error!(voucher = id, error = %e, "写操作日志失败（凭证）");
    }
    Ok(Json(json!({"id": id})))
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

/// 凭证作废 / 恢复（已结账期间拒绝；作废后不参与账簿汇总，可恢复）
async fn voucher_void(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<VoidReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权操作该凭证"));
    }
    vouchers::set_void(&db, id, req.void, user.username())?;
    db.log(
        user.username(),
        "凭证",
        if req.void { "作废" } else { "恢复作废" },
        &format!("凭证 #{id}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

/// 凭证更正：作废原凭证 + 生成一张带链接的新凭证（对标 ERPNext 的 Cancel -> Amend）
///
/// 权限沿用 `VoucherDelete`（作废凭证的权限）：更正是比作废更重的动作 ——
/// 作废只是把一张凭证移出账外，更正还会**生成**一张进入流程的新凭证。
/// 单独的权限点会让「谁能改历史账」变成配置问题，先复用既有口径。
async fn voucher_amend(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AmendReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    // 权限校验必须在 amend 之前：数据权限不只管「看」，也管「改」
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权更正该凭证"));
    }
    let new_id = vouchers::amend(&db, id, &req.reason, user.username())?;
    // 操作日志由引擎在**同一事务内**写入（amend 里 log_on）：
    // 更正已提交、日志还没写就崩溃的话，账改了却查不到谁改的 ——
    // 那正是这套更正链要解决的审计洞，不能自己再捅一个。这里再写一条会重复。
    Ok(Json(json!({ "ok": true, "new_id": new_id })))
}

async fn voucher_post(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherPost)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权记账该凭证"));
    }
    if v.status == VoucherStatus::Posted {
        return Ok(Json(json!({"ok": true, "already_posted": true})));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("该凭证不参与账簿汇总"));
    }
    // 启用审核环节的账套：先审核再记账（预检给 400，避免落到引擎错误的 500）
    if db.options().enable_audit && v.status != VoucherStatus::Audited {
        return Err(AppError::bad_request(
            "该账套启用了审核环节，请先审核凭证再记账",
        ));
    }
    vouchers::post(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 审核（未记账 → 已审核）
async fn voucher_audit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherAudit)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权审核该凭证"));
    }
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能审核"));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("已作废凭证不能审核"));
    }
    vouchers::audit(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 反审核（已审核 → 未记账）
async fn voucher_unaudit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherUnaudit)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权反审核该凭证"));
    }
    if v.status != VoucherStatus::Audited {
        return Err(AppError::bad_request("只有已审核凭证才能反审核"));
    }
    vouchers::unaudit(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 出纳签字（未记账凭证记录签字人；require_cashier 账套作为现金/银行凭证记账前置）
async fn voucher_sign(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权对该凭证签字"));
    }
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能签字，请先反记账"));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("已作废凭证不能签字"));
    }
    vouchers::sign(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

/// 取消出纳签字
async fn voucher_unsign(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权对该凭证取消签字"));
    }
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能取消签字，请先反记账"));
    }
    if v.status == VoucherStatus::Void {
        return Err(AppError::bad_request("已作废凭证不能取消签字"));
    }
    vouchers::unsign(&db, id, user.username())?;
    Ok(Json(json!({"ok": true})))
}

async fn voucher_batch_post(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BatchPostReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherPost)?;
    let db = &user.db;
    // 数据范围过滤：仅记账本人可见的凭证，越权 id 直接拒掉并计数
    let mut allowed: Vec<i64> = Vec::new();
    let mut denied = 0usize;
    for id in &req.ids {
        match vouchers::get(&db, *id)? {
            Some(v) if user.user.can_see_voucher(&v) => allowed.push(*id),
            _ => denied += 1,
        }
    }
    let (n, errs) = vouchers::post_many(&db, &allowed, user.username())?;
    db.log(
        user.username(),
        "凭证",
        "批量记账",
        &format!("{n} 张（拒绝 {denied} 张越权）"),
    )?;
    Ok(Json(json!({ "ok": n, "errors": errs, "denied": denied })))
}

async fn voucher_unpost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    // 反记账（已记账 → 未记账），修改前需先反记账
    user.require(Perm::VoucherUnpost)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权反记账该凭证"));
    }
    vouchers::unpost(&db, id)?;
    Ok(Json(json!({"ok": true, "action": "unposted"})))
}

/// 红字冲销请求：冲销凭证落到的期间与日期
#[derive(Deserialize, Default)]
struct ReverseVoucherReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub date: String,
}

async fn voucher_reverse(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ReverseVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权冲销该凭证"));
    }
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    // 缺省用期间末日：红字冲销常发生在已结账的历史期间，用"今天"会与所属期间不一致
    let date = if req.date.is_empty() {
        period.last_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let nid = vouchers::reverse(&db, id, user.username(), period, date)?;
    Ok(Json(json!({"id": nid})))
}

/// 断号重排请求：period + word
#[derive(Deserialize, Default)]
struct RenumberVoucherReq {
    #[serde(default)]
    pub period: i32,
    #[serde(default)]
    pub word: String,
}

async fn voucher_renumber(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<RenumberVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = &user.db;
    // 断号重排会重排该期间全部字号（含他人凭证），数据范围受限的账号不得操作
    let scope = &user.user.data_scope;
    if scope.own_voucher_only
        || !scope.account_from.trim().is_empty()
        || !scope.account_to.trim().is_empty()
    {
        return Err(AppError::forbidden("数据范围受限的账号不能执行断号重排"));
    }
    let period = if req.period > 0 {
        // 非法期间（如 202513）必须 400：静默落到当前期间会重排错期间的凭证号
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let word = if req.word.trim().is_empty() {
        "记".to_string()
    } else {
        req.word.trim().to_string()
    };
    let n = vouchers::renumber(&db, period, &word)?;
    Ok(Json(json!({"ok": true, "renumbered": n})))
}

async fn voucher_delete(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    // 加载后做守卫校验：状态、结账期间、数据权限
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权删除该凭证"));
    }
    // 启用审核环节的账套：已审核凭证须先反审核
    if v.status == VoucherStatus::Audited && db.options().enable_audit {
        return Err(AppError::bad_request("已审核凭证不能删除，请先反审核"));
    }
    // 已记账凭证需先反记账才能删除（未记账凭证可直接删除）
    if v.status == VoucherStatus::Posted {
        return Err(AppError::bad_request("已记账凭证不能直接删除，请先反记账"));
    }
    // 期间是否已结账
    if let Some(closed) = findb::periods::closed_upto(&*db)? {
        if v.period <= closed {
            return Err(AppError::bad_request(format!(
                "{} 及以前期间已结账，不能删除该凭证",
                closed.label()
            )));
        }
    }
    vouchers::delete(&db, id)?;
    db.log(user.username(), "凭证", "删除", &v.voucher_no())?;
    Ok(Json(json!({"ok": true})))
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

async fn list_voucher_attachments(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let v = vouchers::get(&db, id)?.ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权查看该凭证的附件"));
    }
    let list: Vec<serde_json::Value> = findb::attach::list(&db, id)?
        .iter()
        .map(attachment_json)
        .collect();
    Ok(Json(list))
}

async fn upload_voucher_attachment(
    State(_state): State<Arc<WebState>>,
    Path(id): Path<i64>,
    user: CurrentUser,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    // 注意：`&findb::Db` 持有的是 !Sync 的连接引用，**不能跨 await 持有**，
    // 否则整个 future 变成 !Send，Handler 不成立（路由注册处会报 trait 未实现）。
    // 所以先在块内做完「这张凭证能不能动」的校验，再解析上传体，最后落库。
    let voucher_no = {
        let db = &user.db;
        let v = vouchers::get(&db, id)?
            .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
        if !user.user.can_see_voucher(&v) {
            return Err(AppError::forbidden("无权为该凭证上传附件"));
        }
        v.voucher_no().to_string()
    };

    // 用 axum 自带的 multipart 提取器（背后是 multer）：边界解析、分片、
    // Content-Disposition 都按 RFC 处理，且不引入任何新依赖
    // （旧的 multipart 0.18 会拖进 hyper 0.10/iron/time 0.1 等 4 个 RUSTSEC 漏洞）。
    // 体大小已由路由层的 DefaultBodyLimit（12 MB）兜底。
    let mut name = String::new();
    let mut data: Option<Vec<u8>> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("上传解析失败：{e}")))?
    {
        if field.name() == Some("file") {
            name = field
                .file_name()
                .unwrap_or("attachment")
                .to_string();
            let bytes = field
                .bytes()
                .await
                .map_err(|e| AppError::bad_request(format!("读取上传内容失败：{e}")))?;
            data = Some(bytes.to_vec());
            break;
        }
    }
    let data = data.ok_or_else(|| AppError::bad_request("缺少文件字段 file"))?;
    if data.is_empty() {
        return Err(AppError::bad_request("上传文件为空"));
    }
    if data.len() > findb::attach::MAX_FILE_SIZE {
        return Err(AppError::bad_request("文件超过 10MB 上限"));
    }
    let db = &user.db;
    let aid = findb::attach::add(&db, id, &name, &data, user.username())?;
    db.log(
        user.username(),
        "凭证",
        "上传附件",
        &format!("{} 附件#{} {}", voucher_no, aid, name),
    )?;
    Ok(Json(json!({ "id": aid })))
}

async fn download_attachment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let a =
        findb::attach::get(&db, id)?.ok_or_else(|| AppError::NotFound("附件不存在".to_string()))?;
    let v = vouchers::get(&db, a.voucher_id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权查看该附件"));
    }
    let data = findb::attach::read(&db, id)?;
    let ctype = match a.kind.to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    };
    let encoded: String = a.name.bytes().map(|b| format!("%{b:02X}")).collect();
    let mut resp = data.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(ctype),
    );
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
        "inline; filename=\"{}\"; filename*=UTF-8''{}",
        a.name.replace('"', ""),
        encoded
    )) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

async fn delete_attachment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = &user.db;
    let a =
        findb::attach::get(&db, id)?.ok_or_else(|| AppError::NotFound("附件不存在".to_string()))?;
    let v = vouchers::get(&db, a.voucher_id)?
        .ok_or_else(|| AppError::NotFound("凭证不存在".to_string()))?;
    if !user.user.can_see_voucher(&v) {
        return Err(AppError::forbidden("无权删除该附件"));
    }
    findb::attach::delete(&db, id)?;
    db.log(
        user.username(),
        "凭证",
        "删除附件",
        &format!("{} 附件#{} {}", v.voucher_no(), id, a.name),
    )?;
    Ok(Json(json!({ "ok": true })))
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

async fn list_invoices(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<InvoiceListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    // 上限 500 条（与凭证报表列表同口径）：limit=None 等于不限，把全库发票一次性
    // 读进内存并整包序列化。超限时按开票日期倒序截断，truncated 回报给调用方，
    // 让它收窄筛选条件而不是误以为"就这么多"。
    const INVOICE_LIST_MAX: i64 = 500;
    let rows = findb::invoices::list(
        &db,
        &findb::invoices::InvoiceQuery {
            kind: if q.kind.is_empty() {
                None
            } else {
                Some(q.kind)
            },
            status: if q.status.is_empty() {
                None
            } else {
                Some(q.status)
            },
            keyword: if q.keyword.is_empty() {
                None
            } else {
                Some(q.keyword)
            },
            limit: Some(INVOICE_LIST_MAX),
        },
    )?;
    let items: Vec<serde_json::Value> = rows.iter().map(invoice_json).collect();
    Ok(Json(json!({
        "rows": items,
        "total": items.len(),
        "limit": INVOICE_LIST_MAX,
        "truncated": items.len() as i64 >= INVOICE_LIST_MAX,
    })))
}

async fn invoice_summary(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let sum = findb::invoices::summary(&*db)?;
    let by_kind: serde_json::Map<String, serde_json::Value> = sum
        .into_iter()
        .map(|(k, tax_total, tax_amt, n)| {
            (
                k.clone(),
                json!({ "amount_tax": tax_total.fmt_money(), "tax": tax_amt.fmt_money(), "count": n }),
            )
        })
        .collect();
    Ok(Json(json!({ "by_kind": by_kind })))
}

#[derive(Deserialize)]
struct InvoiceFromPo {
    po_id: i64,
}

#[derive(Deserialize)]
struct InvoiceFromSo {
    so_id: i64,
}

/// 下推开票：采购订单 → 进项发票（金额=订单整单，状态=待认证），记录单据勾稽
async fn invoice_from_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<InvoiceFromPo>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let id = findb::invoices::push_from_po(&db, req.po_id, user.username())?;
    db.log(
        user.username(),
        "发票",
        "下推采购发票",
        &format!("PO#{} → 发票#{id}", req.po_id),
    )?;
    Ok(Json(json!({ "ok": true, "invoice_id": id })))
}

/// 下推开票：销售订单 → 销项发票（金额=订单整单，状态=待认证），记录单据勾稽
async fn invoice_from_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<InvoiceFromSo>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let id = findb::invoices::push_from_so(&db, req.so_id, user.username())?;
    db.log(
        user.username(),
        "发票",
        "下推销售发票",
        &format!("SO#{} → 发票#{id}", req.so_id),
    )?;
    Ok(Json(json!({ "ok": true, "invoice_id": id })))
}

async fn create_invoice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<InvoiceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let inv = invoice_from_req(&req)?;
    let id = findb::invoices::insert(&db, &inv, user.username())?;
    Ok(Json(json!({ "id": id })))
}

async fn update_invoice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<InvoiceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = &user.db;
    // 归属校验 + 保留服务端字段：普通用户不能改他人发票，
    // 且表单未提交的 attach_id/created_by/status 不能被覆盖清零
    let existing =
        findb::invoices::get(&db, id)?.ok_or_else(|| AppError::not_found("发票不存在"))?;
    if existing.created_by != user.username() && !user.user.is_admin() {
        return Err(AppError::forbidden("无权修改他人录入的发票"));
    }
    let mut inv = invoice_from_req(&req)?;
    inv.id = id;
    inv.attach_id = existing.attach_id;
    inv.created_by = existing.created_by.clone();
    inv.created_at = existing.created_at.clone();
    if inv.status != existing.status {
        // 状态变更走专用接口（带状态机校验），此处不接受表单覆盖
        inv.status = existing.status.clone();
    }
    findb::invoices::update(&db, &inv)?;
    db.log(user.username(), "发票", "更新", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_invoice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    let existing =
        findb::invoices::get(&db, id)?.ok_or_else(|| AppError::not_found("发票不存在"))?;
    if existing.created_by != user.username() && !user.user.is_admin() {
        return Err(AppError::forbidden("无权删除他人录入的发票"));
    }
    findb::invoices::delete(&db, id)?;
    db.log(user.username(), "发票", "删除", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn invoice_set_status(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<InvoiceStatusReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherEdit)?;
    let db = &user.db;
    // 归属校验：状态机接口同样不得改他人录入的发票（否则绕过 update_invoice 的守卫）
    let existing =
        findb::invoices::get(&db, id)?.ok_or_else(|| AppError::not_found("发票不存在"))?;
    if existing.created_by != user.username() && !user.user.is_admin() {
        return Err(AppError::forbidden("无权修改他人录入的发票状态"));
    }
    let inv = findb::invoices::set_status(&db, id, &req.status, user.username())?;
    Ok(Json(invoice_json(&inv)))
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

/// 往来期初明细列表（账龄页管理区块；合计 ar/ap 分列）
async fn list_arap_opening(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let kind = q.get("kind").map(String::as_str).filter(|k| !k.is_empty());
    let rows = findb::settle::arap_opening_list(&db, kind)?;
    // 合计按 kind 显式分列：原来的 `if ar {…} else {ap += …}` 会把空串、大小写笔误
    // （"AR"）等一切非 "ar" 的行默默并进应付合计，让用户看到一个凭空变大的数。
    // 未知 kind 单独计数回报，既不混进 ar/ap 也不因此整页报错（行明细照常返回，
    // 用户能自己看出来是哪几条有问题）。金额一律 fmt_money（fmt_qty 只留 6 位小数
    // 且去尾零，5000.00 会印成 "5000"，与本文件其它金额字段口径不一致）。
    let (mut ar, mut ap) = (Money::ZERO, Money::ZERO);
    let mut unknown_kind = 0usize;
    for r in &rows {
        match r.kind.as_str() {
            "ar" => ar += r.amount,
            "ap" => ap += r.amount,
            _ => unknown_kind += 1,
        }
    }
    Ok(Json(json!({
        "rows": rows,
        "total_ar": ar.fmt_money(),
        "total_ap": ap.fmt_money(),
        "unknown_kind": unknown_kind,
    })))
}

/// 删除往来期初明细（导错可删；会计权）
async fn delete_arap_opening(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::settle::arap_opening_delete(&db, id)?;
    db.log(user.username(), "档案", "删除往来期初", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 存货档案一站式列表（独立存货档案页：档案+计划参数+现量+主单位）
// ---- 客户视图 ----------------------------------------------------------
//
// 两个都是**只读**接口。改客户档案仍然只有 `/api/aux` 一个入口 ——
// 这里若也能改，就会出现「销售单据认的客户」与「客户管理里的客户」两份主数据。
async fn list_customers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Perm::Report 而不是 Perm::AuxEdit：出纳要看这张名单（催款要用）、
    // 要能核销，但不该能改客户档案。`Report` 在 Role::Cashier 的权限位里，
    // `aux_edit` 不在 —— 这正是设计 §4 要的分界。
    user.require(Perm::Report)?;
    let db = &user.db;
    let per = period_from_query(&state, &user, q.get("period"));
    let keyword = q.get("q").map(String::as_str).unwrap_or("");
    let only_open = matches!(q.get("only_open").map(String::as_str), Some("1" | "true"));
    let page = q.get("page").and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
    let page_size = q.get("page_size").and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    let result = findb::customers::list(&db, per, keyword, only_open, page, page_size)?;
    Ok(Json(json!({
        "rows": result.rows,
        "period": per.ymm(),
        "total": result.total,
        "page": result.page,
        "page_size": result.page_size,
        "open_count": result.rows.iter().filter(|r| !r.open_amount.is_zero()).count(),
        "open_sum": result
            .rows
            .iter()
            .fold(fincore::Money::ZERO, |a, r| a + r.open_amount)
            .fmt_money(),
    })))
}

async fn get_customer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let per = period_from_query(&state, &user, q.get("period"));
    let code = code.trim().to_string();
    // 编码空的处理：返回 400 而不是「查不到」。
    // `Path("")` 打不进这个路由（路由就匹配不到），但显式拒绝更清楚，
    // 且将来若加了 `/api/customers/` 这样的写法不会被静默吞掉。
    if code.is_empty() {
        return Err(AppError::bad_request("缺少客户编码 code"));
    }
    match findb::customers::detail(&db, &code, per)? {
        Some(d) => Ok(Json(json!({
            "customer": d,
            "period": per.ymm(),
            "credit": findb::sales::credit_check(&db, &code, per)
                .map(|(recv, lim, over)| json!({
                    "receivable": recv.fmt_money(),
                    "limit": lim.fmt_money(),
                    "over": over,
                }))
                .unwrap_or(serde_json::Value::Null),
        }))),
        None => Err(AppError::not_found(format!("客户 {code} 未建档"))),
    }
}

/// 客户账龄。桶与算法全部来自 `findb::customers::aging_of` → `settle::aging`，
/// 本 handler **不参与任何计算**（只挑桶标签与金额）。
async fn get_customer_aging(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let per = period_from_query(&state, &user, q.get("period"));
    let code = code.trim().to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少客户编码 code"));
    }
    // 账龄页与客户页对「客户不存在」的处理必须一致：一个不存在的客户，
    // 详情页报 404，账龄 Tab 也得报 404 —— 否则界面上会出现
    // 「有档案、明细有数据、账龄说此人没有未核销余额」这种自相矛盾的三件套。
    if findb::auxs::get(&db, fincore::AuxKind::Customer, &code)?.is_none() {
        return Err(AppError::not_found(format!("客户 {code} 未建档")));
    }
    // as_of 缺省用**期末**而不是今天：as_of 决定「账龄天数怎么算」，
    // 用今天算历史期间会得到一份「天数按今天算」的老账龄。
    // （账龄页 /settle/aging 用的是今天，两处口径不同是有意的：
    //   那页是「当前视角」，这页是「这个期间视角」。文档 §3.2 有记。）
    let as_of = match q.get("as_of").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request(format!("as_of 不是合法日期：{s}")))?,
        None => per.last_day(),
    };
    let buckets = if q.get("scheme").map(|s| s == "year").unwrap_or(false) {
        fincore::engine::aging::buckets_by_year()
    } else {
        fincore::engine::aging::buckets_by_days()
    };
    let a = findb::customers::aging_of(&db, &code, per, as_of, &buckets)?;
    // 金额一律走 `fmt_money()`，与 `/api/settle/aging` **同一个格式化**。
    // 直接把 `Money` 序列化出去会得到 `"3000.00"` / `"0"`（无千分位、零值没有
    // 两位小数），而账龄页给的是 `"3,000.00"` / `"0.00"` —— 同一份数据两种长相，
    // 用户把两个页面对着看会以为算错了。
    Ok(Json(json!({
        "code": code,
        "period": per.ymm(),
        "as_of": as_of.format("%Y-%m-%d").to_string(),
        "aging": {
            "buckets": a.buckets,
            "amounts": a.amounts.iter().map(|m| m.fmt_money()).collect::<Vec<_>>(),
            "total": a.total.fmt_money(),
            "credit_total": a.credit_total.fmt_money(),
            "net": (a.total - a.credit_total).fmt_money(),
            "max_days": a.max_days,
            "empty": a.empty,
        },
    })))
}

/// 客户交易历史：报价 / 销售订单 / 发货 / 收款。
///
/// 期间口径：`to` 缺省用 `period`（会话当前期间），`from` 缺省用 `to` 的**年初**
/// —— 与资产负债表第二列「年初余额」同一个理由：交易历史看的是「本年买过什么」，
/// 默认给一个 12 个月的窗口，而不是单月。
/// 只读，权限与另外两个客户视图一致（`Perm::Report`）。
async fn get_customer_trade(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let code = code.trim().to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少客户编码 code"));
    }
    if findb::auxs::get(&db, fincore::AuxKind::Customer, &code)?.is_none() {
        return Err(AppError::not_found(format!("客户 {code} 未建档")));
    }
    let per = period_from_query(&state, &user, q.get("period"));
    let to = match q.get("to").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => per,
        Some(s) => Period::from_ymm_checked(period_checked_digits(s))
            .map_err(|_| AppError::bad_request(format!("to 不是合法期间：{s}")))?,
    };
    let from = match q.get("from").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Period::new(to.year(), 1).unwrap_or(to),
        Some(s) => Period::from_ymm_checked(period_checked_digits(s))
            .map_err(|_| AppError::bad_request(format!("from 不是合法期间：{s}")))?,
    };
    let rows = findb::customers::trade(&db, &code, from, to)?;
    Ok(Json(json!({
        "code": code,
        "from": from.ymm(),
        "to": to.ymm(),
        "rows": rows
            .iter()
            .map(|r| json!({
                "date": r.date,
                "kind": r.kind,
                "kind_label": r.kind_label,
                "doc_no": r.doc_no,
                "summary": r.summary,
                "amount": r.amount.fmt_money(),
                "qty": r.qty.fmt_money(),
                "status": r.status,
                "status_label": r.status_label,
            }))
            .collect::<Vec<_>>(),
        "total": rows.len(),
    })))
}

/// 客户催款记录：催款单 + 往来对账单。只读展示，状态由各自模块决定。
async fn get_customer_dunning(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let code = code.trim().to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少客户编码 code"));
    }
    if findb::auxs::get(&db, fincore::AuxKind::Customer, &code)?.is_none() {
        return Err(AppError::not_found(format!("客户 {code} 未建档")));
    }
    let rows = findb::customers::dunning_records(&db, &code)?;
    Ok(Json(json!({
        "code": code,
        "rows": rows
            .iter()
            .map(|r| json!({
                "date": r.date,
                "kind": r.kind,
                "kind_label": r.kind_label,
                "no": r.no,
                "summary": r.summary,
                "amount": r.amount.fmt_money(),
                "item_count": r.item_count,
                "status": r.status,
                "status_label": r.status_label,
            }))
            .collect::<Vec<_>>(),
        "total": rows.len(),
    })))
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

async fn items_master(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    Ok(Json(
        json!({ "rows": findb::inventory2::item_master(&*db)? }),
    ))
}

/// 下载导入模板（列头 + 示例行；主数据三平台列头一致，CSV 带 BOM 供 Excel 直接打开）
///
/// 与其余导入接口一样挂登录 + Report 权限：模板虽只含静态列头，但它是导入入口的
/// 一半（另一半 `import_analyze` 严管），放行未登录请求会让"能否探测本系统用哪些
/// 科目/存货字段"绕过所有鉴权。
async fn import_template(
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let kind = q.get("kind").map(String::as_str).unwrap_or("");
    let rows: Vec<Vec<&str>> = match kind {
        // 模板的列头必须与 `findb::imports` 的解析器和 `sheets.rs` 的导出一致 ——
        // 三处任何一处单独改，用户就会拿到「导得出来、导不回去」的表。
        //
        // 档案模板把客户/供应商/银行/存货各自的扩展列并成一张宽表（模板只能给
        // 一种形状）；解析器先认列名，所以哪一种档案都能照着只填自己那几列。
        // 老的四列文件仍然能导（认不出列名就退回按位置取）。
        "aux" => vec![
            vec![
                "类型", "编码", "名称", "备注", "上级编码", "状态", "税点%", "开票情况",
                "信用额度", "开户行", "银行账号", "规格型号", "计量单位",
            ],
            vec![
                "客户", "C01", "示例客户", "月结 30 天", "", "启用", "13", "每次都开票",
                "50000", "", "", "", "",
            ],
            vec![
                "供应商", "S01", "示例供应商", "", "", "启用", "3", "有时开有时不开", "", "", "",
                "", "",
            ],
            vec![
                "银行", "B01", "基本户", "", "", "启用", "", "", "", "工商银行江门分行",
                "6222xxxx", "", "",
            ],
            vec![
                "存货", "I01", "示例存货", "", "", "启用", "", "", "", "", "", "φ20 圆钢", "吨",
            ],
        ],
        "item" => vec![
            vec!["编码", "名称", "保质期天", "安全库存", "备注", "计量单位", "状态"],
            vec!["I01", "示例存货", "365", "100", "", "吨", "启用"],
        ],
        "account" => vec![
            vec![
                "编码", "名称", "类别", "方向", "备注", "辅助核算维度", "数量单位", "币种",
                "现金科目", "银行科目", "状态",
            ],
            vec![
                "1001", "库存现金", "资产", "借", "", "", "", "", "是", "", "启用",
            ],
            vec![
                "112201", "应收账款-货款", "资产", "借", "", "客户", "", "", "", "", "启用",
            ],
            vec![
                "140501", "原材料", "资产", "借", "", "存货", "吨", "", "", "", "启用",
            ],
        ],
        "opening_stock" => vec![
            vec![
                "存货编码",
                "仓库",
                "数量",
                "单价",
                "批次号",
                "生产日期",
                "备注",
            ],
            vec![
                "I01",
                "W01",
                "100",
                "12.5",
                "BT0001",
                "2026-01-01",
                "盘点转入",
            ],
        ],
        "begin" => vec![
            vec![
                "科目编码",
                "科目名称",
                "方向",
                "期初余额",
                "累计借方",
                "累计贷方",
                "数量",
                "客户编码",
                "供应商编码",
                "存货编码",
                "项目编码",
                "部门编码",
                "职员编码",
                "银行账户",
            ],
            vec![
                "1001", "库存现金", "借", "1000", "0", "0", "", "", "", "", "", "", "", "",
            ],
            // 分户期初：金额挂在应收账款上、客户靠「客户编码」这一列分。
            // 小厂最常搬的就是这笔「华达还欠我 5 万」，没有这一列搬过去就查不到分户。
            vec![
                "112201", "应收账款-货款", "借", "50000", "0", "0", "", "C01", "", "", "", "",
                "", "",
            ],
        ],
        "arap_opening" => vec![
            vec![
                "类型",
                "客商编码",
                "单据号",
                "单据日期",
                "金额",
                "客商名称",
                "备注",
            ],
            vec![
                "应收",
                "C01",
                "XSQ-0001",
                "2025-12-31",
                "5000",
                "客户甲",
                "期初欠款",
            ],
            vec![
                "应付",
                "S01",
                "CGQ-0001",
                "2025-12-31",
                "3000",
                "供应商甲",
                "",
            ],
        ],
        "voucher" => vec![
            vec!["日期", "凭证字", "摘要", "科目编码", "借方", "贷方"],
            vec!["2026-01-31", "记", "期初入库", "1001", "100", "0"],
            vec!["2026-01-31", "记", "期初入库", "1405", "0", "100"],
        ],
        _ => {
            return Err(AppError::bad_request(
                "未知模板 kind：aux / item / account / opening_stock / begin / voucher",
            ))
        }
    };
    let mut body = String::from("\u{feff}");
    for row in &rows {
        body.push_str(&row.join(","));
        body.push('\n');
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{kind}_template.csv\"").as_str(),
            ),
        ],
        body,
    )
        .into_response())
}

/// 预检：写入**之前**把会失败 / 要留意的问题一次说清楚。
///
/// 原来只回答「有没有不存在的科目」，而导入真正会失败的常见原因还有几个：
/// 期间已结账、科目已停用、编码在文件里重复。用户点完「执行导入」才发现时，
/// 错的那部分数据可能已经写进去了（档案类是逐条幂等写入、凭证类更麻烦）。
///
/// 缺失科目不算阻断 —— 它可以靠映射解决，所以由 `blocking()` 排除。
async fn import_analyze(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ImportAnalyzeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(import_perm(&req.kind)?)?;
    let has_file = req
        .file
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !has_file && req.text.trim().is_empty() {
        return Err(AppError::bad_request("请选择 Excel 文件或粘贴 CSV 内容"));
    }
    let file_bytes: Option<Vec<u8>> = if has_file {
        Some(b64_decode(req.file.as_deref().unwrap_or(""))?)
    } else {
        None
    };
    let key = user.book_key.clone();
    let state2 = state.clone();
    // 预检也要解析整份 Excel：同样放阻塞池
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, AppError> {
        let db = state2.db_for(&key)?;
        let tmpl = findb::imports::ImportTemplate::parse(&req.template);
        let is_begin = req.kind != "voucher";
        let text = if let Some(bytes) = &file_bytes {
            let rows = findb::imports::read_xlsx_bytes(bytes)?;
            findb::imports::xlsx_to_csv_text(&rows)
        } else {
            req.text.clone()
        };
        // 主数据类（aux/item/account/opening_stock）无科目引用——按科目列的那部分
        // 预检跳过（行级错误由执行时 warnings 呈现），但**期间结账检查对所有类型
        // 都要做**：凭证与期初都落到具体期间，闭着的时候导进去也记账不了。
        let has_account_refs = !matches!(
            req.kind.as_str(),
            "aux" | "item" | "account" | "opening_stock" | "arap_opening"
        );
        let ymm = if req.period > 0 {
            period_checked(req.period)?.ymm()
        } else {
            current_period(&state2, &user).ymm()
        };
        let pv = findb::imports::analyze_import_mapped(
            &db,
            if has_account_refs { &text } else { "" },
            tmpl,
            is_begin,
            Some(fincore::Period::from_ymm(ymm)),
            &req.mapping,
        )?;
        let items: Vec<serde_json::Value> = pv
            .missing_accounts
            .iter()
            .map(|m| json!({ "code": m.code, "count": m.count }))
            .collect();
        let issues: Vec<serde_json::Value> = pv
            .issues
            .iter()
            .map(|i| json!({ "level": i.level, "category": i.category, "detail": i.detail }))
            .collect();
        Ok(json!({
            "missing": items,
            // 新增：逐条问题 + 汇总。前端把 issues 按 category 分组显示，
            // blocking=true 时禁用「执行导入」按钮（但不阻断 API 调用 ——
            // 真正把关仍在写入路径，这里是「早告诉」而不是「唯一拦点」）。
            "issues": issues,
            "errors": pv.errors(),
            "warns": pv.warns(),
            "blocking": pv.blocking(),
            "total_rows": pv.total_rows,
        }))
    })
    .await
    .map_err(|e| AppError::Internal(format!("导入预检失败：{e}")))??;
    Ok(Json(out))
}

/// 执行导入（期初余额表 / 凭证），带科目映射；支持 CSV 文本或 Excel 文件
async fn import_run(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ImportRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(import_perm(&req.kind)?)?;
    let has_file = req
        .file
        .as_ref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if !has_file && req.text.trim().is_empty() {
        return Err(AppError::bad_request("请粘贴 CSV 内容或选择 Excel 文件"));
    }
    // 文件解码放请求线程，重活（解析 Excel + 批量写库）整体进阻塞池
    let file_bytes: Option<Vec<u8>> = if has_file {
        Some(b64_decode(req.file.as_deref().unwrap_or(""))?)
    } else {
        None
    };
    let key = user.book_key.clone();
    let who = user.username().to_string();
    let fallback_ymm = current_period(&state, &user).ymm();
    let explicit_ymm = if req.period > 0 {
        Some(period_checked(req.period)?.ymm())
    } else {
        None
    };
    let state2 = state.clone();
    let kind = req.kind.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, AppError> {
        let db = state2.db_for(&key)?;
        let tmpl = findb::imports::ImportTemplate::parse(&req.template);
        let res = match req.kind.as_str() {
            "voucher" => {
                let period = fincore::Period::from_ymm(explicit_ymm.unwrap_or(fallback_ymm));
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_vouchers_bytes(
                        &db,
                        period,
                        bytes,
                        &who,
                        &req.mapping,
                        tmpl,
                    )?
                } else {
                    findb::imports::import_vouchers(
                        &db,
                        period,
                        &req.text,
                        &who,
                        &req.mapping,
                        tmpl,
                    )?
                }
            }
            "aux" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_aux_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_aux(&db, &req.text, &who)?
                }
            }
            "item" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_items_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_items(&db, &req.text, &who)?
                }
            }
            "account" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_accounts_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_accounts(&db, &req.text, &who)?
                }
            }
            "opening_stock" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_opening_stock_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_opening_stock(&db, &req.text, &who)?
                }
            }
            "arap_opening" => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_arap_opening_bytes(&db, bytes, &who)?
                } else {
                    findb::imports::import_arap_opening(&db, &req.text, &who)?
                }
            }
            // "begin" 及未列出的既有类型（kind 已由 import_perm 校验，未知 kind 到不了这里）
            _ => {
                if let Some(bytes) = &file_bytes {
                    findb::imports::import_begin_bytes(&db, bytes, &who, &req.mapping, tmpl)?
                } else {
                    findb::imports::import_begin(&db, &req.text, &who, &req.mapping, tmpl)?
                }
            }
        };
        // 明确告诉用户「导完之后该干什么」。
        //
        // 为什么要加这个：导入的凭证落 `Draft`（与手工录入一致，导入不绕过内控），
        // 但「导入成功」很容易被理解成「已经进账了」。实际上还要审核 → 记账，
        // 资金类凭证还要出纳签字。不说清楚，用户会以为导完就算完，下个月
        // 对账才发现一笔没上。
        let next_steps: Vec<String> = match kind.as_str() {
            "voucher" => {
                let opts = findb::options_of(db.conn());
                let mut v = vec![format!(
                    "{} 张凭证已导入为**草稿**（导入不绕过内控，与手工录入一致）",
                    res.ok
                )];
                if opts.enable_audit {
                    v.push("本账套开启了审核环节 → 需先「审核」再「记账」".to_string());
                }
                if opts.require_cashier {
                    v.push(
                        "本账套要求出纳签字 → 涉及现金/银行科目的凭证，出纳签字后才能记账"
                            .to_string(),
                    );
                }
                v.push("核对无误后到「凭证」页批量审核 → 批量记账".to_string());
                v
            }
            "begin" | "opening_stock" | "arap_opening" => {
                vec![
                    "期初数据已写入。请确认「期初余额」页的合计与对方账套一致后再启用记账"
                        .to_string(),
                ]
            }
            _ => vec![
                "档案类导入为逐条幂等写入：中断后重跑会跳过已存在的编码，可安全重导".to_string(),
            ],
        };
        Ok(json!({
            "ok": res.ok,
            "skipped": res.skipped,
            "warnings": res.warnings,
            "next_steps": next_steps,
        }))
    })
    .await
    .map_err(|e| AppError::Internal(format!("导入任务失败：{e}")))??;
    Ok(Json(out))
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

async fn get_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::balance::LedgerRow>>, AppError> {
    user.require(Perm::FinReport)?;
    let (db, chart, lq) = ledger_query_from(&state, &user, &q)?;
    let rows = balances::ledger(&db, &chart, &lq)?;
    Ok(Json(rows))
}

/// 总账（按期间汇总）
async fn get_general_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::GeneralLedgerRow>>, AppError> {
    user.require(Perm::FinReport)?;
    let (db, _chart, lq) = ledger_query_from(&state, &user, &q)?;
    Ok(Json(balances::general_ledger(&db, &lq)?))
}

/// 日记账（现金/银行）
async fn get_journal(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::JournalRow>>, AppError> {
    user.require(Perm::FinReport)?;
    let (db, chart, lq) = ledger_query_from(&state, &user, &q)?;
    Ok(Json(balances::journal(&db, &chart, &lq)?))
}

async fn print_voucher_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    if !can_view_vouchers(&user) {
        return Err(AppError::forbidden("没有查看凭证的权限"));
    }
    let db = &user.db;
    let company = user.company.clone();
    let chart = accounts::chart(&db)?;
    let aux_names = findb::auxs::full_name_map(&*db)?;

    let mut query = VoucherQuery::default().with_data_scope(&user.user);
    query.asc = true;
    let from = period_param(q.get("from"), current_period(&state, &user))?;
    let to = period_param(q.get("to"), from)?;
    if from > to {
        return Err(AppError::bad_request(format!(
            "起始期间不能晚于结束期间：{} > {}",
            period_to_str(from),
            period_to_str(to)
        )));
    }
    query.from = Some(from);
    query.to = Some(to);
    query.limit = q
        .get("limit")
        .and_then(|s| s.parse::<i64>().ok())
        .map(|v| v.clamp(1, 1000))
        .or(Some(500));
    let mut list = vouchers::list(&db, &query)?;
    vouchers::fill_entries(&db, &mut list)?;
    list.retain(|v| user.user.can_see_voucher(v));
    if list.is_empty() {
        return Err(AppError::bad_request("没有可打印的凭证"));
    }

    let aux_label = |aux: &fincore::AuxRef| {
        let mut parts = Vec::new();
        for k in fincore::account::AuxKind::ALL {
            if *k == fincore::account::AuxKind::CashFlow {
                continue;
            }
            if let Some(v) = aux.get(*k) {
                let name = aux_names
                    .get(&format!("{}:{}", k.code(), v))
                    .cloned()
                    .unwrap_or_default();
                parts.push(if name.is_empty() { v.to_string() } else { name });
            }
        }
        parts.join("/")
    };
    let prints = findb::printform::vouchers_to_print(&list, &chart, &aux_label);
    let html = findb::printform::voucher_form_html(
        &company,
        &format!("{}~{}", period_to_str(from), period_to_str(to)),
        &prints,
        true,
        &db.options().print,
    );
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 账簿套打（明细账 / 总账 / 日记账）HTML
async fn print_ledger_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::FinReport)?;
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少科目编码参数 code"));
    }
    if !user.user.can_see_account(&code) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let db = &user.db;
    let company = user.company.clone();
    let chart = accounts::chart(&db)?;
    let from = period_param(q.get("from"), current_period(&state, &user))?;
    let to = period_param(q.get("to"), from)?;
    if from > to {
        return Err(AppError::bad_request(format!(
            "起始期间不能晚于结束期间：{} > {}",
            period_to_str(from),
            period_to_str(to)
        )));
    }
    let ktype = q.get("type").map(String::as_str).unwrap_or("detail");
    let include_children = q
        .get("include_children")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let posted_only = q
        .get("posted_only")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let lq = LedgerQuery {
        code: code.clone(),
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
    let acct = chart
        .get(&code)
        .map(|a| format!("{} {}", code, a.name))
        .unwrap_or(code.clone());

    // 期初余额
    let snap = BalanceSnapshot::load(
        &db,
        &BalanceQuery::range(from, from).with_user_scope(&user.user),
    )?;
    let (bd, bamt) = fincore::signed_to_dir_amount(snap.for_account(&code, None).begin);
    let begin_dir = if bamt.is_zero() {
        "平".to_string()
    } else {
        bd.label().to_string()
    };

    let (title, rows) = match ktype {
        "general" => {
            let list = balances::general_ledger(&db, &lq)?;
            let rows = list
                .into_iter()
                .map(|r| findb::printform::LedgerPrintRow {
                    date: r.period.code(),
                    voucher_no: String::new(),
                    summary: r.summary,
                    debit: r.debit,
                    credit: r.credit,
                    dir: if r.balance.is_zero() {
                        "平".to_string()
                    } else {
                        r.dir.label().to_string()
                    },
                    balance: r.balance,
                })
                .collect::<Vec<_>>();
            ("总账".to_string(), rows)
        }
        "journal" => {
            let list = balances::journal(&db, &chart, &lq)?;
            let rows = list
                .into_iter()
                .map(|r| findb::printform::LedgerPrintRow {
                    date: r.date.format("%Y-%m-%d").to_string(),
                    voucher_no: r.voucher_no,
                    summary: r.summary,
                    debit: r.debit,
                    credit: r.credit,
                    dir: if r.balance.is_zero() {
                        "平".to_string()
                    } else {
                        r.dir.label().to_string()
                    },
                    balance: r.balance,
                })
                .collect::<Vec<_>>();
            ("日记账".to_string(), rows)
        }
        _ => {
            let list = balances::ledger(&db, &chart, &lq)?;
            let rows = list
                .into_iter()
                .map(|r| findb::printform::LedgerPrintRow {
                    date: r.date.format("%Y-%m-%d").to_string(),
                    voucher_no: r.voucher_no,
                    summary: r.summary,
                    debit: r.debit,
                    credit: r.credit,
                    dir: if r.balance.is_zero() {
                        "平".to_string()
                    } else {
                        r.dir.label().to_string()
                    },
                    balance: r.balance,
                })
                .collect::<Vec<_>>();
            ("明细账".to_string(), rows)
        }
    };

    let ledger = findb::printform::LedgerPrint {
        title,
        account_name: acct,
        period_label: format!("{}~{}", period_to_str(from), period_to_str(to)),
        begin_dir,
        begin_balance: bamt,
        rows,
        page_from_1: true,
    };
    let html = findb::printform::ledger_form_html(&company, &ledger, &db.options().print);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
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

async fn get_trial_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let rows: Vec<TrialRow> = rows.iter().map(TrialRow::from_row).collect();
    Ok(Json(json!({
        "from": q.get("from").cloned().unwrap_or_default(),
        "to": q.get("to").cloned().unwrap_or_default(),
        "rows": rows,
        "totals": {
            "begin_debit": totals.begin_debit.fmt_money(), "begin_credit": totals.begin_credit.fmt_money(),
            "debit": totals.period_debit.fmt_money(), "credit": totals.period_credit.fmt_money(),
            "end_debit": totals.end_debit.fmt_money(), "end_credit": totals.end_credit.fmt_money(),
        },
    })))
}

/// 科目明细账（链7 数字钻取）：期初 + 分录逐笔 + 合计；行点击可开凭证
async fn account_detail_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let account = q
        .get("account")
        .map(String::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少 account 科目编码"));
    }
    if !user.user.can_see_account(&account) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let (begin, rows) = findb::balances::account_detail(&db, &account, from, to)?;
    let (td, tc): (Money, Money) = rows.iter().fold((Money::ZERO, Money::ZERO), |(ad, ac), r| {
        (ad + r.debit, ac + r.credit)
    });
    Ok(Json(json!({
        "account": account,
        "from": period_to_str(from),
        "to": period_to_str(to),
        "begin": begin,
        "rows": rows,
        "total_debit": td,
        "total_credit": tc,
    })))
}

async fn print_trial_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 打印预览：所有有「账簿报表」权限的角色都可使用（不落地文件）
    user.require(Perm::Report)?;
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let po = print_opts_from_q(user.db.options().print, &q);
    let html = trial_balance_html(&user.company, &q, &rows, &totals, &po, can_edit);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

async fn export_trial_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 导出数据：仅管理员与财务主管（防止把整表数据带出）
    user.require(Perm::Export)?;
    let (rows, _totals) = trial_balance_data(&state, &user, &q)?;
    let csv = trial_balance_csv(&rows);
    let body = csv.into_bytes();
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"trial_balance.csv\"",
            ),
        ],
        body,
    )
        .into_response())
}

/// 科目余额表导出 PDF（仅管理员与财务主管）
async fn export_trial_balance_pdf(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let from = q.get("from").cloned().unwrap_or_default();
    let to = q.get("to").cloned().unwrap_or_default();
    let company = user.company.clone();
    let bytes = crate::pdf::trial_balance_pdf(&company, &from, &to, &rows, &totals)
        .map_err(AppError::bad_request)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"trial_balance.pdf\"",
            ),
        ],
        bytes,
    )
        .into_response())
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

async fn export_vouchers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let mut query = VoucherQuery::default().with_data_scope(&user.user);
    if let Some(p) = q.get("period").and_then(|s| parse_period(s)) {
        query.from = Some(p);
        query.to = Some(p);
    }
    if let Some(f) = q.get("from").and_then(|s| parse_period(s)) {
        query.from = Some(f);
    }
    if let Some(t) = q.get("to").and_then(|s| parse_period(s)) {
        query.to = Some(t);
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
    query.asc = true;
    query.limit = Some(5000);
    let mut list = vouchers::list(&db, &query)?;
    vouchers::fill_entries(&db, &mut list)?;
    let chart = accounts::chart(&db)?;
    let mut rows: Vec<Vec<String>> = vec![[
        "日期",
        "凭证号",
        "状态",
        "摘要",
        "科目编码",
        "科目名称",
        "借方",
        "贷方",
        "制单人",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()];
    let mut n = 0usize;
    for v in &list {
        if !user.user.can_see_voucher(v) {
            continue;
        }
        for e in &v.entries {
            if e.is_blank() {
                continue;
            }
            let name = chart
                .get(&e.account_code)
                .map(|a| a.name.clone())
                .unwrap_or_default();
            rows.push(vec![
                v.date.format("%Y-%m-%d").to_string(),
                v.voucher_no(),
                v.status.label().to_string(),
                e.summary.clone(),
                e.account_code.clone(),
                name,
                e.debit.fmt_plain(),
                e.credit.fmt_plain(),
                v.prepared_by.clone(),
            ]);
            n += 1;
        }
    }
    db.log(user.username(), "导出", "导出凭证", &format!("{n} 行"))?;
    Ok(csv_response("vouchers.csv", rows))
}

async fn export_ledger(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let (db, chart, lq) = ledger_query_from(&state, &user, &q)?;
    let acct = chart
        .get(&lq.code)
        .map(|a| format!("{} {}", a.code, a.name))
        .unwrap_or_else(|| lq.code.clone());
    let list = balances::ledger(&db, &chart, &lq)?;
    let has_qty = list.iter().any(|r| r.qty_balance.is_some());
    let mut header: Vec<String> = ["日期", "凭证号", "摘要", "借方", "贷方", "方向", "余额"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if has_qty {
        header.push("数量余额".to_string());
    }
    let mut rows = vec![header];
    for r in &list {
        let mut row = vec![
            r.date.format("%Y-%m-%d").to_string(),
            r.voucher_no.clone(),
            r.summary.clone(),
            r.debit.fmt_plain(),
            r.credit.fmt_plain(),
            r.dir.label().to_string(),
            r.balance.fmt_plain(),
        ];
        if has_qty {
            row.push(r.qty_balance.map(|q| q.fmt_qty()).unwrap_or_default());
        }
        rows.push(row);
    }
    db.log(
        user.username(),
        "导出",
        "导出明细账",
        &format!("{acct} {} 行", rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response("ledger.csv", rows))
}

async fn export_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    let list = business::payroll_list(&db, period)?;
    let list = scope_payroll(&user, &list);
    let mut rows = vec![[
        "员工",
        "部门",
        "应发",
        "社保(个人)",
        "公积金(个人)",
        "专项附加",
        "个税",
        "实发",
        "社保(单位)",
        "公积金(单位)",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>()];
    for p in &list {
        rows.push(vec![
            p.employee.clone(),
            p.dept.clone(),
            p.gross.fmt_plain(),
            p.social.fmt_plain(),
            p.housing.fmt_plain(),
            p.additional.fmt_plain(),
            p.tax.fmt_plain(),
            p.net.fmt_plain(),
            p.social_co.fmt_plain(),
            p.housing_co.fmt_plain(),
        ]);
    }
    db.log(
        user.username(),
        "导出",
        "导出工资表",
        &format!("{} {} 行", period.label(), rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response("payroll.csv", rows))
}

async fn export_claims(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    // 空串 = 全部状态：UI 下拉默认值为空，直接 parse 会落到 Draft 只显示草稿
    let status = q
        .get("status")
        .filter(|s| !s.is_empty())
        .map(|s| business::ClaimStatus::parse(s));
    let list = business::claim_list(&db, period, status)?;
    // 「仅看本人经手的业务单据」：报销按申请人匹配当前登录人（与 list_claims 一致）
    let list: Vec<&business::Claim> = list
        .iter()
        .filter(|c| doc_in_scope(&user, &c.applicant))
        .collect();
    let mut rows = vec![[
        "单号",
        "业务日期",
        "申请人",
        "部门",
        "事由",
        "金额",
        "状态",
        "审批人",
        "付款人",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect::<Vec<_>>()];
    for c in &list {
        rows.push(vec![
            c.no.clone(),
            c.biz_date.format("%Y-%m-%d").to_string(),
            c.applicant.clone(),
            c.dept.clone(),
            c.reason.clone(),
            c.amount.fmt_plain(),
            c.status.label().to_string(),
            c.approver.clone(),
            c.payer.clone(),
        ]);
    }
    db.log(
        user.username(),
        "导出",
        "导出报销单",
        &format!("{} {} 行", period.label(), rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response("claims.csv", rows))
}


async fn export_dunning(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty()).unwrap_or("ar");
    if kind != "ar" && kind != "ap" {
        return Err(AppError::bad_request("kind 必须是 ar 或 ap"));
    }
    let csv = findb::exports::build_csv(&db, &format!("dunning_{}", kind), period)?;
    let (_, rows) = csv;
    db.log(
        user.username(),
        "导出",
        "导出催款记录",
        &format!("{} {} 行", period.label(), rows.len().saturating_sub(1)),
    )?;
    Ok(csv_response(&format!("dunning_{}_{}.csv", kind, period.ymm()), rows))
}


/// 催款单 / 对账函导出成 Excel：**带明细**。
///
/// CSV 那份（`/api/export/dunning`）只有一行汇总 —— 客户问「是哪几张单凑的 5 万」
/// 时没法回答，会计只能回凭证里手工抄单一号。这里出两个 Sheet：汇总 + 逐单明细。
///
/// 明细里带「方向」列：预收/预付是我方欠对方的钱，**不计入**应收合计。
/// 没有这一列的话，客户预存 3 万会被算成欠 3 万（v1.8.36 之前的真实 bug）。
async fn export_dunning_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    let kind = q
        .get("kind")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("ar");
    if kind != "ar" && kind != "ap" {
        return Err(AppError::bad_request("kind 必须是 ar 或 ap"));
    }
    let all = findb::settle::dunning_list(db, Some(kind), None)?;
    let list: Vec<_> = all
        .into_iter()
        .filter(|d| d.period.ymm() == period.ymm())
        .collect();
    let type_label = if kind == "ar" { "催款单" } else { "对账函" };
    let head = if kind == "ar" { "应收金额" } else { "应付金额" };
    let owed_head = if kind == "ar" { "应收" } else { "应付" };
    let adv_head = if kind == "ar" { "预收" } else { "预付" };

    let mut summary_rows: Vec<Vec<String>> = Vec::new();
    let mut detail_rows: Vec<Vec<String>> = Vec::new();
    let mut mean: Money = Money::ZERO;
    for d in &list {
        mean = mean + d.owed_total();
        summary_rows.push(vec![
            d.no.clone(),
            d.date.format("%Y-%m-%d").to_string(),
            type_label.to_string(),
            d.account.clone(),
            d.party_code.clone(),
            d.party_name.clone(),
            d.owed_total().fmt_plain(),
            d.advance_total().fmt_plain(),
            d.item_count.to_string(),
            match d.status.as_str() {
                "draft" => "草稿",
                "sent" => "已发出",
                "settled" => "已结清",
                "cancelled" => "已作废",
                other => other,
            }
            .to_string(),
            d.memo.clone(),
        ]);
        for it in &d.detail {
            detail_rows.push(vec![
                d.no.clone(),
                d.party_code.clone(),
                d.party_name.clone(),
                it.doc_no.clone(),
                it.date.clone(),
                it.summary.clone(),
                it.amount.fmt_plain(),
                if findb::settle::dunning_item_is_owed(&it.direction) {
                    owed_head.to_string()
                } else {
                    adv_head.to_string()
                },
            ]);
        }
    }

    let mut cols = vec![crate::xlsxout::ColFmt::Text; 6];
    cols.push(crate::xlsxout::ColFmt::Money);
    cols.push(crate::xlsxout::ColFmt::Money);
    cols.push(crate::xlsxout::ColFmt::Int);
    cols.push(crate::xlsxout::ColFmt::Text);
    cols.push(crate::xlsxout::ColFmt::Text);
    let mut sheet = crate::xlsxout::Sheet::titled(
        type_label,
        &format!("{} {}", type_label, period.label()),
        &format!("共 {} 张 · {} 合计 {}", list.len(), owed_head, mean.fmt_money()),
        &[
            "单号", "日期", "类型", "科目", "客商编码", "客商名称", head, adv_head, "笔数", "状态",
            "备注",
        ],
        &cols,
    )
    .with_rows(summary_rows);

    let dcols = vec![
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Text,
        crate::xlsxout::ColFmt::Money,
        crate::xlsxout::ColFmt::Text,
    ];
    let dsheet = crate::xlsxout::Sheet::titled(
        "明细",
        &format!("{}明细 {}", type_label, period.label()),
        "方向为「预收/预付」的行是我方欠对方的钱，不计入合计",
        &[
            "单号", "客商编码", "客商名称", "单据号", "日期", "摘要", "金额", "方向",
        ],
        &dcols,
    )
    .with_rows(detail_rows.clone());

    db.log(
        user.username(),
        "导出",
        "导出催款单 Excel",
        &format!(
            "{} {} {} 张 {} 行明细",
            period.label(),
            type_label,
            list.len(),
            detail_rows.len()
        ),
    )?;
    let fname = format!("{}_{}_{}.xlsx", kind, type_label, period.ymm());
    build_xlsx(&[sheet, dsheet], &fname, "催款单")
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

async fn get_tax_vat(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let form = findb::taxdecl::vat_main_form(&db, period)?;
    let opts = findb::taxdecl::TaxOptions::load(&*db)?;
    let company = db.options().company;
    let tax_no = db.options().tax_no;
    Ok(Json(json!({
        "form": form,
        "options": opts,
        "company": company,
        "tax_no": tax_no,
        // 「不代算」的项在 UI 上要能填回去（留痕，不参与计算）
        "manual_fields": ["上期留抵税额", "进项税额转出额", "附加税费减免", "留抵结转下期"],
    })))
}

/// 某一税率档的构成发票：回答「这个汇总数由哪几张票构成」
async fn get_tax_vat_detail(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let kind = q.get("kind").map(|s| s.trim()).unwrap_or("out");
    if kind != "out" && kind != "in" {
        return Err(AppError::bad_request(
            "kind 只能是 out（销项）或 in（进项）",
        ));
    }
    let rate = q
        .get("rate")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("缺少参数 rate（税率档，如 13）"))?;
    let rows = findb::taxdecl::bucket_detail(&db, kind, period, rate)?;
    Ok(Json(
        json!({ "kind": kind, "rate": rate, "rows": rows, "count": rows.len() }),
    ))
}

async fn get_tax_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    Ok(Json(json!(findb::taxdecl::TaxOptions::load(&*db)?)))
}

async fn save_tax_options(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<findb::taxdecl::TaxOptions>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    // 附加税率上限取 10%，不是 100%：教育费附加法定 3%、地方教育附加各省不超过 2%，
    // 10% 已经远高于任何合法情形，再高基本是「3% 误录成 30%」这类手误。
    // 不设上限的后果很直接：附加税费算成几十倍，申报表上一眼就是错的。
    const MAX_SURTAX_RATE: &str = "0.10";
    for (name, v) in [
        ("教育费附加", &req.edu_rate),
        ("地方教育附加", &req.local_edu_rate),
    ] {
        let m = fincore::Money::parse(v.trim())
            .map_err(|_| AppError::bad_request(format!("{name}税率无法解析：{v}")))?;
        if m.is_negative() {
            return Err(AppError::bad_request(format!("{name}税率不能为负：{v}")));
        }
        if m > fincore::Money::parse(MAX_SURTAX_RATE).unwrap() {
            return Err(AppError::bad_request(format!(
                "{name}税率应在 0（不征）到 {MAX_SURTAX_RATE}（10%）之间，收到：{v}"
            )));
        }
    }
    if !["general", "small"].contains(&req.taxpayer.as_str()) {
        return Err(AppError::bad_request(
            "纳税人身份只能是 general（一般纳税人）或 small（小规模纳税人）",
        ));
    }
    if !["city", "county", "other"].contains(&req.city_tax_zone.as_str()) {
        return Err(AppError::bad_request(
            "城建税适用地区只能是 city / county / other",
        ));
    }
    let db = &user.db;
    req.save(&*db)?;
    db.log(user.username(), "税务", "税务参数", "更新申报表参数")?;
    Ok(Json(json!({ "ok": true, "options": req })))
}

/// 导出申报表 CSV（带 BOM，Excel 双击直接打开不乱码）
async fn export_tax_vat(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let f = findb::taxdecl::vat_main_form(&db, period)?;
    let company = db.options().company;

    let mut out = String::from("\u{feff}");
    let mut row = |cells: &[String]| {
        out.push_str(
            &cells
                .iter()
                .map(|c| csv_escape(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push_str("\r\n");
    };
    let s = |v: &fincore::Money| v.fmt_money();

    row(&[
        "增值税一般纳税人申报表（主表）".into(),
        format!("税款所属期：{}", f.period),
        format!("纳税人名称：{company}"),
        format!("纳税人识别号：{}", db.options().tax_no),
    ]);
    row(&[]);
    row(&["一、销项税额".into(), "本期数".into()]);
    row(&["一般计税方法计税销售额".into(), s(&f.sales_general)]);
    row(&["简易计税方法计税销售额".into(), s(&f.sales_simple)]);
    row(&["免征增值税销售额".into(), s(&f.sales_exempt)]);
    row(&["销项税额".into(), s(&f.output_tax)]);
    row(&[]);
    row(&["二、进项税额".into(), "本期数".into()]);
    row(&["本期认证相符的进项税额".into(), s(&f.input_tax)]);
    row(&["上期留抵税额".into(), "（人工填报）".into()]);
    row(&["进项税额转出额".into(), "（人工填报，见附列资料三）".into()]);
    row(&[]);
    row(&["三、税额计算".into(), "本期数".into()]);
    row(&["应纳税额".into(), s(&f.payable)]);
    row(&["未开票销售额".into(), s(&f.unbilled_sales)]);
    row(&["未开票收入销项税额".into(), s(&f.unbilled_tax)]);
    row(&[]);
    row(&["四、附加税费".into(), "本期数".into()]);
    row(&[
        format!("城市维护建设税（{}）", f.surcharge.city_rate),
        s(&f.surcharge.city_tax),
    ]);
    row(&[
        format!("教育费附加（{}）", f.surcharge.edu_rate),
        s(&f.surcharge.edu),
    ]);
    row(&[
        format!("地方教育附加（{}）", f.surcharge.local_edu_rate),
        s(&f.surcharge.local_edu),
    ]);
    row(&["附加税费合计".into(), s(&f.surcharge.total)]);

    for t in [&f.details_out, &f.details_in] {
        row(&[]);
        row(&[
            t.title.clone(),
            "税率".into(),
            "张数".into(),
            "销售额".into(),
            "税额".into(),
            "价税合计".into(),
        ]);
        for b in &t.buckets {
            row(&[
                String::new(),
                format!("{}%", b.rate),
                b.count.to_string(),
                s(&b.net),
                s(&b.tax),
                s(&b.gross),
            ]);
        }
        row(&[
            "合计".into(),
            String::new(),
            t.count.to_string(),
            s(&t.net_total),
            s(&t.tax_total),
            s(&t.gross_total),
        ]);
    }

    // 提示随表导出：拿到 CSV 的人（不一定是出表的会计）也要知道哪些数没算
    if !f.warnings.is_empty() {
        row(&[]);
        row(&["口径说明（以下各项系统不代算，需人工填报）".into()]);
        for w in &f.warnings {
            row(&[format!("· {w}")]);
        }
    }

    // 中文文件名必须用 filename*=UTF-8'' 百分号编码，否则浏览器下下来名字乱码。
    // 与附件下载（send_attachment）同一套写法，不引新依赖。
    let fname = format!("增值税申报表_{}_{}.csv", company, f.period);
    let encoded: String = fname.bytes().map(|b| format!("%{b:02X}")).collect();
    let mut resp = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/csv; charset=utf-8")],
        out,
    )
        .into_response();
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!(
        "attachment; filename=\"vat_{}.csv\"; filename*=UTF-8''{encoded}",
        f.period
    )) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(resp)
}

/// 单条出入库流水（差异分析点行看构成明细）
async fn get_stock_move(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let m = findb::business::stock_move_get(&db, id)?
        .ok_or_else(|| AppError::not_found(format!("出入库流水不存在：{id}")))?;
    Ok(Json(json!({
        "id": m.id,
        "biz_date": m.biz_date.to_string(),
        "kind": m.kind.code(),
        "kind_label": m.kind.label(),
        "item": m.item,
        "qty": m.qty.fmt_qty(),
        "price": m.price.fmt_money(),
        "amount": m.amount.fmt_money(),
        "memo": m.memo,
    })))
}

/// 标准成本差异分析
///
/// 读权限与报表同级：差异分析用的是收发存汇总与成本配置，看它等于看成本结构。
async fn get_cost_variance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let r = findb::stdcost::variance_report(&db, period)?;
    Ok(Json(json!({
        "period": r.period,
        "rows": r.rows,
        "total_price": r.total_price,
        "total_spend": r.total_spend,
        "missing": r.missing,
        "warnings": r.warnings,
        // 「算了但没算的」要能在界面上说清楚，而不是让人以为差异表是完整的
        "std_items": findb::business::cost_configs(&*db)?,
    })))
}

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
// 高级功能：多栏账 / 摘要汇总表 / 财务指标 / 工艺路线 / 报工 / MRP / 预算版本 /
// 审批流 / 报表附注 / 电子档案
// ---------------------------------------------------------------------------

async fn get_multi_column(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let main = q.get("main").cloned().unwrap_or_default();
    if main.is_empty() {
        return Err(AppError::bad_request("缺少主科目 main"));
    }
    let cols: Vec<String> = q
        .get("cols")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if cols.is_empty() {
        return Err(AppError::bad_request("缺少栏目科目 cols（逗号分隔）"));
    }
    let (from, to) = period_range_param(
        &q,
        current_period(&state, &user),
        current_period(&state, &user),
    )?;
    let db = &user.db;
    // 数据范围：主科目与各栏目科目均须在可见范围内
    if !user.user.can_see_account(&main) || cols.iter().any(|c| !user.user.can_see_account(c)) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    let rows = advanced::multi_column_table(&db, &main, &cols, from, to, Some(&user.user))?;
    Ok(Json(
        serde_json::json!({ "main": main, "cols": cols, "rows": rows }),
    ))
}

async fn get_summary_table(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let (from, to) = period_range_param(
        &q,
        current_period(&state, &user),
        current_period(&state, &user),
    )?;
    let db = &user.db;
    let rows = advanced::summary_table(&db, from, to, Some(&user.user))?;
    Ok(Json(
        serde_json::json!({ "from": period_to_str(from), "to": period_to_str(to), "rows": rows }),
    ))
}

async fn get_fin_ratios(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| {
            // 默认年初（同一会计年度 1 月）
            fincore::Period::new(period.year(), 1).unwrap_or(period)
        });
    let db = &user.db;
    let rows = advanced::fin_ratios(&db, period, from, Some(&user.user))?;
    Ok(Json(
        serde_json::json!({ "period": period_to_str(period), "ratios": rows }),
    ))
}

// ---- 所有者权益变动表 / 报表对比 / 科目日报表 / 期末对账 ----

async fn get_equity_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    // 原本只有 from 是区间参数、to 固定等于 period，这里保持不变（不凭空加 to 参数）
    let year_start = fincore::Period::new(period.year(), 1).unwrap_or(period);
    let from = period_param(q.get("from"), year_start)?;
    if from > period {
        return Err(AppError::bad_request(format!(
            "起始期间不能晚于结束期间：{} > {}",
            period_to_str(from),
            period_to_str(period)
        )));
    }
    let db = &user.db;
    let stmt = findb::reports::equity_statement(&db, from, period, Some(&user.user))?;
    Ok(Json(serde_json::json!({
        "from": period_to_str(from),
        "to": period_to_str(period),
        // 前端要显示勾稽结果；ties() 是方法，不会随 stmt 一起序列化。
        "ties": stmt.ties(),
        "statement": stmt,
    })))
}

async fn get_report_compare(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let key = q
        .get("key")
        .cloned()
        .unwrap_or_else(|| "balance_sheet".to_string());
    let cur = period_param(q.get("period"), current_period(&state, &user))?;
    let prev = period_param(q.get("prev"), cur.prev())?;
    let yearly = q
        .get("yearly")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    // 默认按年累计：当前期 1 月→当前期；上期 1 月→上期
    let (cur_from, prev_from) = if yearly {
        (
            fincore::Period::new(cur.year(), 1).unwrap_or(cur),
            fincore::Period::new(prev.year(), 1).unwrap_or(prev),
        )
    } else {
        (cur, prev)
    };
    let db = &user.db;
    let rows = findb::reports::report_compare(
        &db,
        &key,
        cur_from,
        cur,
        prev_from,
        prev,
        Some(&user.user),
    )?;
    Ok(Json(serde_json::json!({
        "key": key,
        "current": period_to_str(cur),
        "previous": period_to_str(prev),
        "rows": rows,
    })))
}

async fn get_account_daily(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let code = q.get("code").cloned().unwrap_or_default();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少科目编码 code"));
    }
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let to = q.get("to").and_then(|s| parse_period(s)).unwrap_or(from);
    let db = &user.db;
    // 数据范围：仅可见科目
    if !user.user.can_see_account(&code) {
        return Err(AppError::forbidden("无权查看该科目"));
    }
    // H-3：科目日报默认只统计已记账，与账簿「只含已记账」开关同参
    let posted_only = q
        .get("posted_only")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(true);
    let rows =
        findb::reports::account_daily_report(&db, &code, from, to, Some(&user.user), posted_only)?;
    Ok(Json(serde_json::json!({ "code": code, "rows": rows })))
}

async fn get_period_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let db = &user.db;
    let items = findb::reports::period_reconcile(&db, period)?;
    Ok(Json(
        serde_json::json!({ "period": period_to_str(period), "items": items }),
    ))
}

/// 辅助账：按辅助核算维度汇总各单位的期初/发生/期末
async fn get_aux_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let kind_code = q.get("kind").map(String::as_str).unwrap_or("customer");
    let kind = fincore::AuxKind::from_code(kind_code)
        .ok_or_else(|| AppError::bad_request("非法辅助核算维度 kind"))?;
    if kind == fincore::AuxKind::CashFlow {
        return Err(AppError::bad_request("现金流量项目不是余额维度"));
    }
    let (from, to) = report_range(&state, &user, &q)?;
    let db = &user.db;
    let rows = balances::aux_balance(&db, kind, from, to, Some(&user.user))?;
    Ok(Json(json!({
        "kind": kind.code(),
        "kind_label": kind.label(),
        "from": period_to_str(from),
        "to": period_to_str(to),
        "rows": rows,
    })))
}

/// 数量金额账：数量核算科目的数量与金额对照
async fn get_qty_balance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let (from, to) = report_range(&state, &user, &q)?;
    let db = &user.db;
    let rows = balances::qty_balance_sheet(&db, from, to, Some(&user.user))?;
    Ok(Json(json!({
        "from": period_to_str(from),
        "to": period_to_str(to),
        "rows": rows,
    })))
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

async fn list_custom_reports(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let rows: Vec<serde_json::Value> = findb::mgmt::custom_list(&*db)?
        .iter()
        .map(custom_json)
        .collect();
    Ok(Json(rows))
}

async fn get_custom_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let r = findb::mgmt::custom_get(&db, &key)?
        .ok_or_else(|| AppError::not_found("自定义报表不存在"))?;
    let period = period_param(q.get("period"), current_period(&state, &user))?;
    // 用 detailed 版本：公式写错/取数失败（数据库忙、脏数据）时该单元格按 0 占位，
    // 但**必须把原因回传**。旧的 `custom_report_values` 直接丢弃 warnings，
    // 报表会打印出一片像样的 0 而用户完全看不出哪里错了——财务报表最坏的失败模式。
    let (values, warnings) =
        findb::mgmt::custom_report_values_detailed(&db, &r, period, Some(&user.user))?;
    let matrix: Vec<Vec<String>> = values
        .iter()
        .map(|row| row.iter().map(|m| m.fmt_money()).collect())
        .collect();
    Ok(Json(json!({
        "report": custom_json(&r),
        "period": period_to_str(period),
        "values": matrix,
        "warnings": warnings,
    })))
}

async fn save_custom_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CustomReportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    if req.name.trim().is_empty() {
        return Err(AppError::bad_request("报表名称不能为空"));
    }
    let key = if req.key.trim().is_empty() {
        findb::mgmt::custom_next_key(&*db)?
    } else {
        req.key.trim().to_string()
    };
    let r = findb::mgmt::CustomReport {
        key: key.clone(),
        name: req.name.trim().to_string(),
        columns: req.columns.iter().map(|c| c.trim().to_string()).collect(),
        lines: req
            .lines
            .iter()
            .map(|l| findb::mgmt::CustomLine {
                name: l.name.trim().to_string(),
                indent: l.indent,
                formulas: l.formulas.clone(),
                bold: l.bold,
            })
            .collect(),
    };
    let errors: Vec<serde_json::Value> = findb::mgmt::custom_check(&r)
        .into_iter()
        .map(|(li, ci, e)| json!({ "line": li, "column": ci, "error": e }))
        .collect();
    findb::mgmt::custom_save(&db, &r)?;
    db.log(
        user.username(),
        "报表",
        "保存自定义报表",
        &format!("{} {}（{} 行）", key, r.name, r.lines.len()),
    )?;
    Ok(Json(json!({ "key": key, "errors": errors })))
}

async fn delete_custom_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    findb::mgmt::custom_delete(&db, &key)?;
    db.log(user.username(), "报表", "删除自定义报表", &key)?;
    Ok(Json(json!({ "ok": true })))
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

async fn stock_adjust_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<StockAdjustReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    if req.item.trim().is_empty() {
        return Err(AppError::bad_request("缺少存货 item"));
    }
    let delta = parse_money_checked(&req.delta)?;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let qty = parse_money_checked(&req.qty)?;
    let id = findb::business::stock_adjust_full(
        &db,
        period,
        date,
        &req.item,
        &req.warehouse,
        qty,
        delta,
        &req.memo,
    )?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "qty": qty, "delta": delta }),
    ))
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

async fn serial_in_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SerialInReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let n = findb::inventory2::serial_in(&db, &req.item, &req.serials, &req.batch_no, date)?;
    Ok(Json(serde_json::json!({ "ok": true, "count": n })))
}

#[derive(Deserialize)]
struct SerialOutReq {
    pub serials: Vec<String>,
    #[serde(default)]
    pub date: String,
}

async fn serial_out_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SerialOutReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let n = findb::inventory2::serial_out(&db, &req.serials, date)?;
    Ok(Json(serde_json::json!({ "ok": true, "count": n })))
}

async fn list_serial(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let item = q.get("item").cloned().unwrap_or_default();
    if item.is_empty() {
        return Err(AppError::bad_request("缺少 item"));
    }
    let db = &user.db;
    let rows = findb::inventory2::serial_list(&db, &item)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_unit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let item = q.get("item").cloned().unwrap_or_default();
    let db = &user.db;
    let u = findb::inventory2::unit_get(&db, &item)?;
    Ok(Json(serde_json::json!({ "unit": u })))
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

async fn set_unit(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<UnitReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::inventory2::unit_set(
        &db,
        &findb::inventory2::ItemUnit {
            item: req.item,
            base_unit: req.base_unit,
            alt_unit: req.alt_unit,
            factor: parse_money_checked(&req.factor)?,
        },
    )?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn get_inv_aging(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::inventory2::inv_aging(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_abc(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::inventory2::abc_analysis(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
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

async fn assemble_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssembleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let mut children: Vec<(String, Money)> = Vec::with_capacity(req.children.len());
    for (i, q) in req.children {
        children.push((i, parse_money_checked(&q)?));
    }
    findb::inventory2::assemble(&db, period, date, &req.parent, &children, &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn disassemble_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssembleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let mut children: Vec<(String, Money)> = Vec::with_capacity(req.children.len());
    for (i, q) in req.children {
        children.push((i, parse_money_checked(&q)?));
    }
    findb::inventory2::disassemble(&db, period, date, &req.parent, &children, &req.memo)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn get_warehouse_stock(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let item = q.get("item").cloned().unwrap_or_default();
    if item.is_empty() {
        return Err(AppError::bad_request("缺少 item"));
    }
    let db = &user.db;
    let rows = findb::inventory2::warehouse_stock(&db, &item)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_transfer_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::inventory2::transfer_report(&db, current_period(&state, &user))?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|m| {
            serde_json::json!({
                "date": m.biz_date.format("%Y-%m-%d").to_string(),
                "item": m.item,
                "batch_no": m.batch_no,
                "warehouse": m.warehouse,
                "qty": m.qty.fmt_qty(),
                "memo": m.memo,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "rows": items })))
}

// ---- 仓库主数据（v30） ----

/// 仓库档案列表（读：Report；默认仓在前）
async fn list_warehouses(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::warehouse::list(&*db)? })))
}

/// 新增/修改仓库（写：Warehouse；设默认仓自动清其它默认标记）
async fn save_warehouse(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(w): Json<findb::warehouse::Warehouse>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::warehouse::save(&db, &w)?;
    db.log(
        user.username(),
        "库存",
        "仓库档案",
        &format!("{} {}（默认={}）", w.code, w.name, w.is_default),
    )?;
    Ok(Json(json!({ "ok": true })))
}

/// 删除仓库：默认仓不可删、被流水/盘点引用不可删
async fn delete_warehouse(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::warehouse::delete(&db, &code)?;
    db.log(user.username(), "库存", "删除仓库", &code)?;
    Ok(Json(json!({ "ok": true })))
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

async fn add_estimate(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<EstimateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    // 登记暂估即同事务生成暂估凭证（借 存货 / 贷 应付-订单供应商）
    let (est_id, vid) = findb::scm2::po_estimate_add(
        &db,
        req.po_id,
        period,
        &req.item,
        parse_money_checked(&req.est_amount)?,
        user.username(),
    )?;
    db.log(
        user.username(),
        "采购",
        "登记暂估",
        &format!(
            "PO#{} {} {} 凭证 #{vid}",
            req.po_id, req.item, req.est_amount
        ),
    )?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": est_id, "voucher_id": vid }),
    ))
}

/// 逐行列出暂估状态：可暂估额度 / 已暂估 / 还能登多少。
///
/// 收货与暂估之间原本**没有任何联动** —— 收完货不会提示「该登暂估了」，
/// 而登记暂估的金额还要用户手敲（敲错也没人拦）。这个端点把三笔数摆出来，
/// 让界面能显示「可暂估 / 已暂估 / 未暂估」，并可一键带出金额。
async fn estimate_status(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let po_id = q
        .get("po_id")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if po_id == 0 {
        return Err(AppError::bad_request("缺少 po_id"));
    }
    let db = &user.db;
    let rows = findb::scm2::estimate_status_by_line(&db, po_id)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn list_estimates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let po_id = q
        .get("po_id")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if po_id == 0 {
        return Err(AppError::bad_request("缺少 po_id"));
    }
    let db = &user.db;
    let rows = findb::scm2::po_estimate_list(&db, po_id)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn settle_estimate(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let today = chrono::Local::now().date_naive();
    let date = if body.is_empty() {
        today
    } else {
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        match v.get("date").and_then(|d| d.as_str()) {
            Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
            None => today,
        }
    };
    // 冲回同事务生成反向凭证（借 应付 / 贷 存货）
    let vid = findb::scm2::po_estimate_settle(&db, id, date, user.username())?;
    db.log(
        user.username(),
        "采购",
        "暂估冲回",
        &format!(
            "#{id}{}",
            vid.map(|v| format!("，冲回凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(serde_json::json!({ "ok": true, "voucher_id": vid })))
}

async fn get_po_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::scm2::po_reconcile(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_so_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::scm2::so_reconcile(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_quota(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let supplier = q.get("supplier").cloned().unwrap_or_default();
    let item = q.get("item").cloned().unwrap_or_default();
    let remaining = findb::scm2::quota_remaining(&db, period, &supplier, &item)?;
    Ok(Json(serde_json::json!({ "remaining": remaining })))
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

async fn set_quota(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<QuotaReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    findb::scm2::quota_set(
        &db,
        period,
        &req.supplier,
        &req.item,
        parse_money_checked(&req.quota_qty)?,
    )?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn get_change_log(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let order_type = q.get("type").cloned().unwrap_or_else(|| "po".to_string());
    let order_id = q.get("id").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let rows = findb::scm2::change_log_list(&db, &order_type, order_id)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

// ---- 采购/销售全生命周期：请购 / 报价 / 到货 / 发货 / 付款 / 收款 / 退货 / 信用 ----

async fn list_purchase_req(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let rows = findb::procurement::pr_list(&db, period)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_purchase_req(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(mut req): Json<findb::procurement::PurchaseReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    if req.no.is_empty() {
        req.no = findb::procurement::pr_next_no(&db, req.period)?;
    }
    if req.requester.is_empty() {
        req.requester = user.user.display_name.clone();
    }
    let id = findb::procurement::pr_save(&db, &mut req)?;
    Ok(Json(serde_json::json!({ "id": id, "no": req.no })))
}

async fn approve_purchase_req(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    // 工作流拦截：有已发布流程 → 先走节点链，终态才执行原审批
    match findb::workflow::intercept(
        &db,
        findb::workflow::BIZ_PURCHASE_REQ,
        id,
        &user.user,
        true,
        "",
    )? {
        findb::workflow::Gate::Pending { next } => {
            db.log(
                user.username(),
                "审批",
                "工作流节点",
                &format!("请购#{id} → {next}"),
            )?;
            return Ok(Json(json!({ "ok": true, "pending": next })));
        }
        findb::workflow::Gate::Final { approved: false } => {
            return Ok(Json(json!({ "ok": true, "rejected": true })));
        }
        _ => {}
    }
    findb::procurement::pr_approve(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
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

async fn add_po_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::procurement::po_receipt_with_stock(
        &db,
        &findb::procurement::PoReceipt {
            id: 0,
            po_id: req.po_id,
            period,
            date,
            qty: parse_money_checked(&req.qty)?,
            memo: req.memo,
            item_code: req.item_code,
            warehouse: req.warehouse.clone(),
        },
        &req.warehouse,
    )?;
    // 待检提示：首行存货勾选了来料检验 → 入库为待检状态（质检转正后方可领用）
    let qc_pending = findb::scm::po_get(&db, req.po_id)?
        .and_then(|p| {
            p.lines
                .first()
                .map(|l| findb::business::item_qc_required(&db, &l.item_code))
        })
        .unwrap_or(false);
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "qc_pending": qc_pending }),
    ))
}

async fn add_po_return(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let id = findb::procurement::po_return_with_stock(
        &db,
        req.po_id,
        period,
        date,
        parse_money_checked(&req.qty)?,
        &req.memo,
        &req.warehouse,
        &req.item_code,
    )?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
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

async fn add_po_payment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PaymentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let amount = parse_money_checked(&req.amount)?;
    // 对标金蝶：付款动作即出台账凭证并按供应商自动核销（默认资金账户取账套配置）
    let po =
        findb::scm::po_get(&db, req.po_id)?.ok_or_else(|| AppError::not_found("采购订单不存在"))?;
    // 审核流：先落草稿收付款单（凭证由审核人审核时同事务生成并自动核销）
    //
    // 写入顺序刻意是「依赖数据先、单据最后」：findb 目前没有 receipt_create /
    // po_payment_add 的 `_in(tx,…)` 变体，无法开一个事务把三步包在一起，于是只能
    // 调换顺序收敛失败面——先建单、后建付款流水，一旦建付款流水失败就会留下一个
    // 没有任何付款流水的孤立草稿单，而它仍可被
    // `POST /api/funds/receipts/:id/audit` 审核并生成付款凭证。
    // 残留的非原子性：建单成功后若勾稽建边失败，会留下「有付款流水、无勾稽边」的记录，
    // 业务数据本身仍自洽（单据与流水成对），仅勾稽边缺失，可在「票款勾稽」手工补。
    let id = findb::procurement::po_payment_add(
        &db,
        &findb::procurement::PoPayment {
            id: 0,
            po_id: req.po_id,
            period,
            date,
            amount,
            memo: req.memo.clone(),
        },
    )?;
    let doc_id = findb::receipt::receipt_create(
        &db,
        "payment",
        date,
        "",
        &po.supplier_code,
        "",
        "",
        fincore::Money::ZERO,
        amount,
        &req.memo,
        user.username(),
    )?;
    // 票↔款勾稽：本订单已下推进项发票与本次收付款单自动建边
    findb::docflow::link_receipt_to_src_invoice(&db, "po", req.po_id, doc_id)?;
    // 审计日志失败不得把已提交的付款变成 500：客户端重试会重复入账（见 sales_cost_ep 同理）
    if let Err(e) = db.log(
        user.username(),
        "采购",
        "采购付款登记",
        &format!(
            "#{} {} 付款单 #{doc_id}（待审核）",
            req.po_id,
            amount.fmt_money()
        ),
    ) {
        error!(doc_id, error = %e, "写操作日志失败（采购付款登记）");
    }
    Ok(Json(
        json!({ "ok": true, "id": id, "doc_id": doc_id, "status": "draft" }),
    ))
}

async fn get_price_history(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let item = q.get("item").cloned().unwrap_or_default();
    let rows = findb::procurement::price_history(&db, &item)?;
    let rows: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(s, p, d)| serde_json::json!({ "supplier": s, "unit_price": p.fmt_money(), "date": d }))
        .collect();
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_po_track(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::procurement::po_execution_track(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_purchase_stats(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::procurement::purchase_stats(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn list_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let rows = findb::sales::quo_list(&db, period)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(mut req): Json<findb::sales::Quotation>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    if req.no.is_empty() {
        req.no = findb::sales::quo_next_no(&db, req.period)?;
    }
    if req.prepared_by.is_empty() {
        req.prepared_by = user.user.display_name.clone();
    }
    let id = findb::sales::quo_save(&db, &mut req)?;
    Ok(Json(serde_json::json!({ "id": id, "no": req.no })))
}

async fn approve_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    // 工作流拦截：有已发布流程 → 先走节点链（Pending 只推进不批单；终态才执行原审批）
    match findb::workflow::intercept(
        &db,
        findb::workflow::BIZ_QUOTATION,
        id,
        &user.user,
        true,
        "",
    )? {
        findb::workflow::Gate::Pending { next } => {
            db.log(
                user.username(),
                "审批",
                "工作流节点",
                &format!("报价#{id} → {next}"),
            )?;
            return Ok(Json(json!({ "ok": true, "pending": next })));
        }
        findb::workflow::Gate::Final { approved: false } => {
            return Ok(Json(json!({ "ok": true, "rejected": true })));
        }
        _ => {}
    }
    findb::sales::quo_approve(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
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

async fn add_so_shipment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ShipmentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let qty = parse_money_checked(&req.qty)?;
    if qty.is_negative() || qty.is_zero() {
        return Err(AppError::bad_request("发货数量必须为正数"));
    }
    // 对标金蝶：未确认订单不能出库（草稿/作废订单先确认）
    let so =
        findb::scm::so_get(&db, req.so_id)?.ok_or_else(|| AppError::not_found("销售订单不存在"))?;
    if matches!(
        so.status,
        findb::scm::SoStatus::Draft | findb::scm::SoStatus::Cancelled
    ) {
        return Err(AppError::bad_request(
            "订单未确认，不能发货（请先「确认」订单）",
        ));
    }
    // 收入确认凭证 + 发货执行行 + 销售出库流水**同一事务**：
    // 早先拆成 so_income_voucher（自行提交）+ so_shipment_with_stock（另一个事务），
    // 第二步失败（仓库不存在等）时收入与应收已入账提交，却没有任何发货记录，
    // 留下无法自动撤销的孤儿凭证。
    let (id, ivid) = findb::sales::so_shipment_all(
        &db,
        req.so_id,
        period,
        date,
        qty,
        &req.memo,
        &req.warehouse,
        user.username(),
    )?;
    findb::scm::so_progress_update(&db, req.so_id)?;
    // 出库成功 → 完成该订单最早一条待发通知（备货指令闭环）
    let _ = findb::sales::notice_fulfill_on_shipment(&db, req.so_id)?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "voucher_id": ivid }),
    ))
}

async fn add_so_return(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ShipmentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let qty = parse_money_checked(&req.qty)?;
    if qty.is_negative() || qty.is_zero() {
        return Err(AppError::bad_request("退货数量必须为正数"));
    }
    // 对标金蝶：未确认订单不能退货
    let so =
        findb::scm::so_get(&db, req.so_id)?.ok_or_else(|| AppError::not_found("销售订单不存在"))?;
    if matches!(
        so.status,
        findb::scm::SoStatus::Draft | findb::scm::SoStatus::Cancelled
    ) {
        return Err(AppError::bad_request(
            "订单未确认，不能退货（请先「确认」订单）",
        ));
    }
    // 收入冲回凭证 + 退货执行行 + 回库流水**同一事务**。
    // 关键收益：超退校验现在在任何写入之前。旧写法先提交收入冲回凭证，
    // 再由 so_return_with_stock 报「退货数量超过净发货」——收入和应收已被冲掉，
    // 却没有退货单，用户只看到一个 400。
    let (id, ivid) = findb::sales::so_return_all(
        &db,
        req.so_id,
        period,
        date,
        qty,
        &req.memo,
        &req.warehouse,
        user.username(),
    )?;
    findb::scm::so_progress_update(&db, req.so_id)?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "voucher_id": ivid }),
    ))
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

async fn add_so_payment(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SoPaymentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d").unwrap_or_else(|_| period.first_day())
    };
    let amount = parse_money_checked(&req.amount)?;
    // 对标金蝶：收款动作即出台账凭证并按客户自动核销（默认资金账户取账套配置）
    let so =
        findb::scm::so_get(&db, req.so_id)?.ok_or_else(|| AppError::not_found("销售订单不存在"))?;
    // 审核流：先落草稿收付款单（凭证由审核人审核时同事务生成并自动核销）
    //
    // 写入顺序与 add_po_payment 刻意一致：依赖数据（收款流水）先落，草稿收付款单最后建。
    // findb 没有 so_payment_add / receipt_create 的 `_in(tx,…)` 变体，三步无法共事务，
    // 只能靠顺序收敛失败面——建单在前时，建流水失败会留下可被审核并生成付款凭证的孤立草稿单。
    // 残留的非原子性：建单成功后勾稽建边失败，仅缺一条勾稽边，业务数据仍自洽。
    let id = findb::sales::so_payment_add(&db, req.so_id, period, date, amount, &req.memo)?;
    let doc_id = findb::receipt::receipt_create(
        &db,
        "receipt",
        date,
        "",
        &so.customer_code,
        "",
        "",
        fincore::Money::ZERO,
        amount,
        &req.memo,
        user.username(),
    )?;
    // 票↔款勾稽：本订单已下推的销项发票与本次收付款单自动建边
    findb::docflow::link_receipt_to_src_invoice(&db, "so", req.so_id, doc_id)?;
    // 审计日志失败不得把已提交的收款变成 500：客户端重试会重复入账
    if let Err(e) = db.log(
        user.username(),
        "销售",
        "销售收款登记",
        &format!(
            "#{} {} 收款单 #{doc_id}（待审核）",
            req.so_id,
            amount.fmt_money()
        ),
    ) {
        error!(doc_id, error = %e, "写操作日志失败（销售收款登记）");
    }
    Ok(Json(
        json!({ "ok": true, "id": id, "doc_id": doc_id, "status": "draft" }),
    ))
}

// ---------------- 可视化工作流（对标金蝶审批流设计器） ----------------

/// 流程列表（含节点/连线）。查看=Report；保存/发布/删除=SysOption（账套配置权）。
async fn list_workflows(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::workflow::flow_list(&*db)? })))
}

#[derive(Deserialize)]
struct WfTemplateApplyReq {
    key: String,
    biz_type: String,
}

/// 预置流程模板清单（不返回 nodes/edges 全文，前端只要元信息做选择卡片；
/// 真正的图在 apply 之后随流程一起返回）
async fn list_wf_templates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    // 带上「该单据是否已有流程」，前端好提示是新建还是会被替换
    let existing = findb::workflow::flow_list(&*db)?;
    let rows: Vec<serde_json::Value> = findb::workflow::templates()
        .into_iter()
        .map(|t| {
            let used: Vec<serde_json::Value> = t
                .biz_types
                .iter()
                .map(|b| {
                    let hit = existing.iter().find(|f| &f.biz_type == b);
                    json!({
                        "biz_type": b,
                        "biz_label": findb::workflow::biz_label(b),
                        "flow_id": hit.map(|f| f.id).unwrap_or(0),
                        "flow_status": hit.map(|f| f.status.clone()).unwrap_or_default(),
                    })
                })
                .collect();
            json!({
                "key": t.key, "name": t.name, "desc": t.desc,
                "biz_types": t.biz_types,
                "requires_fields": t.requires_fields,
                "node_count": t.nodes.len(),
                "applicable": used,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
}

/// 按预置模板落一条流程（草稿态）
///
/// 落的是草稿而非直接发布：模板是行业惯例，不是这家企业的授权制度；而且
/// 已有单据在流转时改流程会让在途实例指向不存在的节点。
async fn apply_wf_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<WfTemplateApplyReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let f = findb::workflow::apply_template(&db, &req.key, &req.biz_type, user.username())?;
    db.log(
        user.username(),
        "工作流",
        "应用模板",
        &format!("{}（{}）→ #{} 草稿", f.biz_type, f.name, f.id),
    )?;
    Ok(Json(json!({ "flow": f })))
}

async fn save_workflow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(f): Json<findb::workflow::WfFlowInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    let id = findb::workflow::flow_save(&db, &f, user.username())?;
    db.log(
        user.username(),
        "工作流",
        "保存流程",
        &format!(
            "{}（{}，节点 {}）",
            f.name,
            findb::workflow::biz_label(&f.biz_type),
            f.nodes.len()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn publish_workflow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    findb::workflow::flow_set_status(&db, id, true, user.username())?;
    db.log(user.username(), "工作流", "发布流程", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn unpublish_workflow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    findb::workflow::flow_set_status(&db, id, false, user.username())?;
    db.log(user.username(), "工作流", "撤回发布", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_workflow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::SysOption)?;
    let db = &user.db;
    findb::workflow::flow_delete(&db, id)?;
    db.log(user.username(), "工作流", "删除流程", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 运行实例（当前节点/状态/轨迹），供「工作流」页回放
async fn list_wf_instances(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::workflow::instances(&*db)? })))
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

/// 销售订单套打 HTML：`ids` 逗号分隔（缺省 = 期间内全部）；`fields` 选字段；`pack=0` 一单一页
async fn print_so_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let company = user.company.clone();
    let ids = print_ids(&q);
    let mut orders = Vec::new();
    if ids.is_empty() {
        let period = q
            .get("period")
            .and_then(|s| parse_period(s))
            .unwrap_or_else(|| current_period(&state, &user));
        for o in findb::scm::so_list(&db, period, None)? {
            orders.push(so_to_print(o));
        }
    } else {
        for id in ids {
            let o = findb::scm::so_get(&db, id)?
                .ok_or_else(|| AppError::bad_request(format!("销售订单 #{id} 不存在")))?;
            orders.push(so_to_print(o));
        }
    }
    if orders.is_empty() {
        return Err(AppError::bad_request("没有可打印的订单"));
    }
    let mut f = doc_fields_from_q(&q);
    // 字段级价格权限：无「价格查看」者，套打强制裁掉价格类列（单价/税率/金额/税额/合计）
    if !user.user.can(Perm::PriceView) {
        f.col_price = false;
        f.col_rate = false;
        f.col_amount = false;
        f.col_tax = false;
        f.totals = false;
    }
    let html = findb::printform::order_forms_sized_html(
        &company,
        &orders,
        &f,
        &doc_size_from_q(&q),
        q.get("auto").map(|s| s.as_str() == "1").unwrap_or(false),
        &db.options().print,
    );
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 采购订单套打 HTML（同销售订单：ids / fields / pack）
async fn print_po_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let company = user.company.clone();
    let ids = print_ids(&q);
    let mut orders = Vec::new();
    if ids.is_empty() {
        let period = q
            .get("period")
            .and_then(|s| parse_period(s))
            .unwrap_or_else(|| current_period(&state, &user));
        for o in findb::scm::po_list(&db, period, None)? {
            orders.push(po_to_print(o));
        }
    } else {
        for id in ids {
            let o = findb::scm::po_get(&db, id)?
                .ok_or_else(|| AppError::bad_request(format!("采购订单 #{id} 不存在")))?;
            orders.push(po_to_print(o));
        }
    }
    if orders.is_empty() {
        return Err(AppError::bad_request("没有可打印的订单"));
    }
    let mut f = doc_fields_from_q(&q);
    // 字段级价格权限：无「价格查看」者，套打强制裁掉价格类列（单价/税率/金额/税额/合计）
    if !user.user.can(Perm::PriceView) {
        f.col_price = false;
        f.col_rate = false;
        f.col_amount = false;
        f.col_tax = false;
        f.totals = false;
    }
    let html = findb::printform::order_forms_sized_html(
        &company,
        &orders,
        &f,
        &doc_size_from_q(&q),
        q.get("auto").map(|s| s.as_str() == "1").unwrap_or(false),
        &db.options().print,
    );
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 收付款单套打 HTML：`ids` 缺省 = 全部（可配 `period` 过滤）
async fn print_receipt_form(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let company = user.company.clone();
    let ids = print_ids(&q);
    let period = q.get("period").and_then(|s| parse_period(s));
    let all = findb::receipt::receipt_list(&*db)?;
    let mut prints = Vec::new();
    for d in all.into_iter() {
        if !ids.is_empty() {
            if ids.contains(&d.id) {
                prints.push(receipt_to_print(d));
            }
        } else if period.map(|p| d.period == p).unwrap_or(true) {
            prints.push(receipt_to_print(d));
        }
    }
    if prints.is_empty() {
        return Err(AppError::bad_request("没有可打印的收付款单"));
    }
    let f = doc_fields_from_q(&q);
    let html = findb::printform::receipt_forms_sized_html(
        &company,
        &prints,
        &f,
        &doc_size_from_q(&q),
        q.get("auto").map(|s| s.as_str() == "1").unwrap_or(false),
        &db.options().print,
    );
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// 报价单转销售订单（approved → converted）：生成草稿订单，税率 0 可在订单中再调整
async fn convert_quotation(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let so_id = findb::sales::quo_to_order(&db, id, user.username())?;
    db.log(
        user.username(),
        "销售",
        "报价转订单",
        &format!("报价 #{id} → 销售订单 #{so_id}"),
    )?;
    Ok(Json(json!({ "ok": true, "so_id": so_id })))
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

/// 某单据类型可用的条件字段（画布上配条件边时的候选清单）
///
/// 字段集**按单据类型不同**：生产订单没有 amount、报销单没有 planned_qty。
/// 前端以前是写死一句「amount/qty/customer_code 等」，用户在生产订单上照着写
/// `amount > 5000` 会一路保存成功、直到单据提交审批才报「条件字段不存在」——
/// 流程已经建了、实例已经起了，单据卡死。所以这里给前端一份真清单。
async fn get_wf_cond_fields(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let biz = q.get("biz_type").map(|s| s.trim()).unwrap_or_default();
    if findb::workflow::ALL_BIZ.iter().all(|(k, _)| *k != biz) {
        return Err(AppError::bad_request(format!(
            "非法业务类型：{biz}；可选：{}",
            findb::workflow::ALL_BIZ
                .iter()
                .map(|(k, l)| format!("{k}（{l}）"))
                .collect::<Vec<_>>()
                .join("、")
        )));
    }
    let _ = &state;
    Ok(Json(json!({
        "biz_type": biz,
        "biz_label": findb::workflow::biz_label(biz),
        "fields": findb::workflow::cond_fields_of(biz),
    })))
}

// 状态机本身在 findb：scm::so_transition_ok / po_transition_ok 由
// so_set_status / po_set_status 强制，草稿 → 已完成这类跳变由 findb 拒（FinError::state
// → 400）。这里不再复制一份流转表——两份图必然漂移，且 HTTP 层无从得知 findb 何时调整。

async fn list_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let mut rows = findb::scm::so_list(&db, period, None)?;
    // 字段级价格权限：无「价格查看」——列表中单价/税额/金额服务端置零（数量保留）
    if !user.user.can(Perm::PriceView) {
        for o in &mut rows {
            o.total_amount = Money::ZERO;
            o.total_tax = Money::ZERO;
            for l in &mut o.lines {
                l.unit_price = Money::ZERO;
                l.tax_rate = Money::ZERO;
                l.amount = Money::ZERO;
                l.tax_amount = Money::ZERO;
            }
        }
    }
    Ok(Json(json!({ "rows": rows })))
}

#[derive(Deserialize)]
struct NoticeReq {
    qty: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    memo: String,
}

/// 发货通知（订单确认后）：仓库备货指令，出库后自动完成最早一条待发通知
async fn create_notice(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<NoticeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let qty = parse_money_checked(&req.qty)?;
    let nid = findb::sales::notice_create(&db, id, qty, date, &req.memo, user.username())?;
    db.log(
        user.username(),
        "销售",
        "发货通知",
        &format!("SO#{id} ×{} 通知#{nid}", qty.fmt_qty()),
    )?;
    Ok(Json(json!({ "ok": true, "id": nid })))
}

/// 待发/历史发货通知列表
async fn list_notices(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::sales::notice_list(&*db)? })))
}

/// 保存销售订单：服务端计算行金额（数量×单价）与税额（金额×税率）；
/// 非草稿状态保存时 so_save 内做客户信用检查（对标金蝶卡控）。
async fn save_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SoInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let code = req.customer_code.trim();
    if code.is_empty() {
        return Err(AppError::bad_request("客户编码必填"));
    }
    let name = if req.customer_name.trim().is_empty() {
        code.to_string()
    } else {
        req.customer_name.trim().to_string()
    };
    let mut so =
        findb::scm::SalesOrder::new(period, order_date(&req.date), code, &name, user.username());
    so.id = req.id;
    so.memo = req.memo;
    so.status = so_status_parse(&req.status)?;
    // 字段级价格权限：无「价格修改」者——已有行保留原单价/税率，新行置零（数量照常可改）
    let price_ok = user.user.can(Perm::PriceEdit);
    let mut old_price: std::collections::HashMap<String, (Money, Money)> =
        std::collections::HashMap::new();
    if so.id > 0 {
        let old =
            findb::scm::so_get(&db, so.id)?.ok_or_else(|| AppError::not_found("销售订单不存在"))?;
        so.no = old.no;
        so.shipped_amount = old.shipped_amount;
        for l in &old.lines {
            old_price.insert(l.item_code.clone(), (l.unit_price, l.tax_rate));
        }
    } else {
        so.no = findb::scm::so_next_no(&db, period)?;
    }
    for l in &req.lines {
        if l.item_code.trim().is_empty() {
            continue;
        }
        let qty = parse_money_checked(&l.qty_ordered)?;
        let mut price = parse_money_checked(&l.unit_price)?;
        let mut rate = if l.tax_rate.trim().is_empty() {
            Money::ZERO
        } else {
            parse_money_checked(&l.tax_rate)?
        };
        if !price_ok {
            match old_price.get(l.item_code.trim()) {
                Some((op, orate)) => {
                    price = *op;
                    rate = *orate;
                }
                None => {
                    price = Money::ZERO;
                    rate = Money::ZERO;
                }
            }
        }
        let amount = (qty * price).round2();
        let tax = (amount * rate).round2();
        so.lines.push(findb::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: l.item_code.trim().to_string(),
            item_name: if l.item_name.trim().is_empty() {
                l.item_code.trim().to_string()
            } else {
                l.item_name.trim().to_string()
            },
            qty_ordered: qty,
            qty_shipped: if l.qty_shipped.trim().is_empty() {
                Money::ZERO
            } else {
                parse_money_checked(&l.qty_shipped)?
            },
            unit_price: price,
            tax_rate: rate,
            amount,
            tax_amount: tax,
            memo: l.memo.clone(),
        });
    }
    if so.lines.is_empty() {
        return Err(AppError::bad_request("订单至少一行明细"));
    }
    let id = findb::scm::so_save(&db, &mut so)?;
    db.log(
        user.username(),
        "销售",
        "保存销售订单",
        &format!(
            "#{} {} {} 不含税 {}",
            id,
            so.no,
            code,
            so.total_amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "total_amount": so.total_amount,
        "total_tax": so.total_tax
    })))
}

async fn transition_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<OrderTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let to = so_status_parse(&req.status)?;
    // 工作流拦截：**只在「确认」这一个动作上**拦。
    // 「确认」是订单的审批动作（签出即对外承诺）；partialShip / completed /
    // cancelled 是确认之后的执行动作，再走一遍审批链等于让仓库收货要财务再批一次。
    if to == findb::scm::SoStatus::Confirmed {
        match workflow_gate(&db, &user, findb::workflow::BIZ_SALES_ORDER, id, "销售订单")? {
            WfGate::Pending(next) => return Ok(Json(json!({ "ok": true, "pending": next }))),
            WfGate::Rejected => return Ok(Json(json!({ "ok": true, "rejected": true }))),
            WfGate::Continue => {}
        }
    }
    // 状态机由 findb 的 so_set_status 把关（非法跳变 → 400）
    findb::scm::so_set_status(&db, id, to)?;
    // 状态已提交：日志失败不得把请求打成 500，否则客户端重试会重复流转
    if let Err(e) = db.log(
        user.username(),
        "销售",
        "销售订单状态",
        &format!("#{} → {}", id, to.label()),
    ) {
        error!(id, error = %e, "写操作日志失败（销售订单状态）");
    }
    Ok(Json(json!({ "ok": true, "status": to.label() })))
}

async fn delete_so(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    findb::scm::so_delete(&db, id)?;
    db.log(user.username(), "销售", "删除销售订单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let mut rows = findb::scm::po_list(&db, period, None)?;
    // 字段级价格权限：无「价格查看」——列表中单价/税额/金额服务端置零（数量保留）
    if !user.user.can(Perm::PriceView) {
        for o in &mut rows {
            o.total_amount = Money::ZERO;
            o.total_tax = Money::ZERO;
            for l in &mut o.lines {
                l.unit_price = Money::ZERO;
                l.tax_rate = Money::ZERO;
                l.amount = Money::ZERO;
                l.tax_amount = Money::ZERO;
            }
        }
    }
    Ok(Json(json!({ "rows": rows })))
}

/// 保存采购订单（行金额/税额服务端计算，同销售订单）
async fn save_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PoInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let code = req.supplier_code.trim();
    if code.is_empty() {
        return Err(AppError::bad_request("供应商编码必填"));
    }
    let name = if req.supplier_name.trim().is_empty() {
        code.to_string()
    } else {
        req.supplier_name.trim().to_string()
    };
    let mut po =
        findb::scm::PurchaseOrder::new(period, order_date(&req.date), code, &name, user.username());
    po.id = req.id;
    po.memo = req.memo;
    po.status = po_status_parse(&req.status)?;
    // 字段级价格权限：无「价格修改」者——已有行保留原单价/税率，新行置零（数量照常可改）
    let price_ok = user.user.can(Perm::PriceEdit);
    let mut old_price: std::collections::HashMap<String, (Money, Money)> =
        std::collections::HashMap::new();
    if po.id > 0 {
        let old =
            findb::scm::po_get(&db, po.id)?.ok_or_else(|| AppError::not_found("采购订单不存在"))?;
        po.no = old.no;
        po.received_amount = old.received_amount;
        for l in &old.lines {
            old_price.insert(l.item_code.clone(), (l.unit_price, l.tax_rate));
        }
    } else {
        po.no = findb::scm::po_next_no(&db, period)?;
    }
    for l in &req.lines {
        if l.item_code.trim().is_empty() {
            continue;
        }
        let qty = parse_money_checked(&l.qty_ordered)?;
        let mut price = parse_money_checked(&l.unit_price)?;
        let mut rate = if l.tax_rate.trim().is_empty() {
            Money::ZERO
        } else {
            parse_money_checked(&l.tax_rate)?
        };
        if !price_ok {
            match old_price.get(l.item_code.trim()) {
                Some((op, orate)) => {
                    price = *op;
                    rate = *orate;
                }
                None => {
                    price = Money::ZERO;
                    rate = Money::ZERO;
                }
            }
        }
        let amount = (qty * price).round2();
        let tax = (amount * rate).round2();
        po.lines.push(findb::scm::PoLine {
            id: 0,
            po_id: 0,
            item_code: l.item_code.trim().to_string(),
            item_name: if l.item_name.trim().is_empty() {
                l.item_code.trim().to_string()
            } else {
                l.item_name.trim().to_string()
            },
            qty_ordered: qty,
            qty_received: if l.qty_received.trim().is_empty() {
                Money::ZERO
            } else {
                parse_money_checked(&l.qty_received)?
            },
            unit_price: price,
            tax_rate: rate,
            amount,
            tax_amount: tax,
            memo: l.memo.clone(),
        });
    }
    if po.lines.is_empty() {
        return Err(AppError::bad_request("订单至少一行明细"));
    }
    let id = findb::scm::po_save(&db, &mut po)?;
    // 价格历史沉淀（正价行 + 有供应商）：采购编辑器「最近价带出」的数据源
    for l in &po.lines {
        if l.unit_price.is_positive() && !po.supplier_code.trim().is_empty() {
            let _ = findb::procurement::price_history_record(
                &db,
                &l.item_code,
                &po.supplier_code,
                l.unit_price,
                po.date,
            );
        }
    }
    // 审计日志失败不得把已提交的订单变成 500：客户端重试会重复下单
    if let Err(e) = db.log(
        user.username(),
        "采购",
        "保存采购订单",
        &format!(
            "#{} {} {} 不含税 {}",
            id,
            po.no,
            code,
            po.total_amount.fmt_money()
        ),
    ) {
        error!(id, error = %e, "写操作日志失败（保存采购订单）");
    }
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "total_amount": po.total_amount,
        "total_tax": po.total_tax
    })))
}

async fn transition_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<OrderTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let to = po_status_parse(&req.status)?;
    // 同 transition_so：只在「确认」这个审批动作上走工作流
    if to == findb::scm::PoStatus::Confirmed {
        match workflow_gate(
            &db,
            &user,
            findb::workflow::BIZ_PURCHASE_ORDER,
            id,
            "采购订单",
        )? {
            WfGate::Pending(next) => return Ok(Json(json!({ "ok": true, "pending": next }))),
            WfGate::Rejected => return Ok(Json(json!({ "ok": true, "rejected": true }))),
            WfGate::Continue => {}
        }
    }
    // 同 transition_so：状态机由 findb 的 po_set_status 把关（非法跳变 → 400）
    findb::scm::po_set_status(&db, id, to)?;
    // 状态已提交：日志失败不得把请求打成 500，否则客户端重试会重复流转
    if let Err(e) = db.log(
        user.username(),
        "采购",
        "采购订单状态",
        &format!("#{} → {}", id, to.label()),
    ) {
        error!(id, error = %e, "写操作日志失败（采购订单状态）");
    }
    Ok(Json(json!({ "ok": true, "status": to.label() })))
}

async fn delete_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    findb::scm::po_delete(&db, id)?;
    db.log(user.username(), "采购", "删除采购订单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn get_credit_check(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let customer = q.get("customer").cloned().unwrap_or_default();
    let (receivable, limit, over) =
        findb::sales::credit_check(&db, &customer, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({
        "receivable": receivable.fmt_money(),
        "limit": limit.fmt_money(),
        "over": over,
    })))
}

async fn get_so_track(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::sales::so_execution_track(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_sales_stats(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::sales::sales_stats(&db, current_period(&state, &user))?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

// ---- 预算预警 ----

async fn get_budget_alerts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| fincore::Period::new(period.year(), 1).unwrap_or(period));
    let threshold = q
        .get("threshold")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(90);
    let rows = findb::mgmt::budget_alerts(&db, period, from, threshold)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

/// 坏账准备计提：按应收账龄生成「借 资产减值损失 / 贷 坏账准备」凭证
async fn bad_debt_provision_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = chrono::Local::now().date_naive();
    match findb::settle::bad_debt_provision_voucher(&db, period, date, user.username())? {
        Some(id) => {
            db.log(
                user.username(),
                "往来",
                "计提坏账准备",
                &format!("凭证 #{id}"),
            )?;
            Ok(Json(json!({"ok": true, "voucher_id": id})))
        }
        None => Ok(Json(
            json!({"ok": true, "voucher_id": 0, "message": "无可计提的坏账准备"}),
        )),
    }
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

async fn get_routing(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let ops = advanced::routing_list(&db, &item)?;
    Ok(Json(serde_json::json!({ "item_code": item, "ops": ops })))
}

async fn post_routing(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
    Json(req): Json<Vec<RoutingOpDto>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let mut ops: Vec<advanced::RoutingOp> = Vec::with_capacity(req.len());
    for d in req {
        ops.push(advanced::RoutingOp {
            id: 0,
            item_code: item.clone(),
            version: String::new(),
            seq: d.seq,
            op_code: d.op_code,
            op_name: d.op_name,
            work_center: d.work_center,
            std_hours: parse_money_checked(&d.std_hours)?,
            rate: parse_money_checked(&d.rate)?,
            qc_required: d.qc_required,
        });
    }
    advanced::routing_save(&db, &item, &ops)?;
    Ok(Json(serde_json::json!({ "ok": true, "count": ops.len() })))
}

async fn delete_routing(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    advanced::routing_delete(&db, &item)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn list_prod_orders(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let orders = findb::scm::prod_list(&db, period, None)?;
    let items: Vec<serde_json::Value> = orders
        .iter()
        .map(|o| {
            serde_json::json!({
                "id": o.id,
                "no": o.no,
                "item_code": o.item_code,
                "item_name": o.item_name,
                "planned_qty": o.planned_qty.fmt_qty(),
                "completed_qty": o.completed_qty.fmt_qty(),
                "status": format!("{:?}", o.status),
                "order_kind": o.order_kind,
                "so_id": o.so_id,
                "so_no": if o.so_id > 0 {
                    findb::scm::so_get(&db, o.so_id)
                        .ok()
                        .flatten()
                        .map(|s| s.no)
                        .unwrap_or_default()
                } else {
                    String::new()
                },
                "supplier_name": o.supplier_name,
                "plan_start": o.plan_start,
                "plan_end": o.plan_end,
            })
        })
        .collect();
    Ok(Json(
        serde_json::json!({ "period": period_to_str(period), "orders": items }),
    ))
}

async fn get_prod_ops(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let ops = advanced::prod_op_list(&db, id)?;
    Ok(Json(serde_json::json!({ "po_id": id, "ops": ops })))
}

#[derive(Deserialize)]
struct OpReportReq {
    pub op_id: i64,
    #[serde(default)]
    pub qty: String,
    #[serde(default)]
    pub hours: String,
}

async fn report_prod_op(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OpReportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    advanced::prod_op_report(
        &db,
        req.op_id,
        parse_money_checked(&req.qty)?,
        parse_money_checked(&req.hours)?,
    )?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct OpFinishReq {
    pub op_id: i64,
}

async fn finish_prod_op(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OpFinishReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    advanced::prod_op_finish(&db, req.op_id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- MRP ----

async fn get_mrp_latest(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let rows = advanced::mrp_latest(&*db)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
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

async fn run_mrp(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<MrpRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let mut demands: Vec<(String, Money, String)> = Vec::with_capacity(req.demands.len());
    for d in req.demands {
        demands.push((d.item_code, parse_money_checked(&d.qty)?, d.source));
    }
    if demands.is_empty() && req.from_sales {
        demands = advanced::mrp_demands_from_sales(&db, current_period(&state, &user))?;
    }
    if demands.is_empty() {
        return Err(AppError::bad_request(
            "请提供需求清单，或勾选「从销售订单收集」",
        ));
    }
    let run_at = advanced::mrp_run(&db, &demands)?;
    let rows = advanced::mrp_by_run(&db, &run_at)?;
    Ok(Json(serde_json::json!({ "run_at": run_at, "rows": rows })))
}

/// MRP 采购建议下推请购单（草稿，幂等）
async fn mrp_to_req_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    if findb::advanced::mrp_get(&db, id)?.is_none() {
        return Err(AppError::not_found("MRP 结果行不存在"));
    }
    let (req_id, no) = findb::advanced::mrp_to_req(&db, id, user.username())?;
    db.log(
        user.username(),
        "采购",
        "MRP 下推请购",
        &format!("MRP#{id} → 请购单 {no}"),
    )?;
    Ok(Json(json!({ "ok": true, "req_id": req_id, "no": no })))
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

/// 形态转换：源物料出库 → 目标物料入库（同数量，数量口径，无总账凭证）
async fn form_convert_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<FormConvertReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let qty = parse_money_checked(&req.qty)?;
    findb::inventory2::form_convert(
        &db,
        period,
        date,
        req.from_item.trim(),
        req.to_item.trim(),
        qty,
        &req.memo,
    )?;
    db.log(
        user.username(),
        "库存",
        "形态转换",
        &format!("{} → {} ×{}", req.from_item, req.to_item, qty.fmt_qty()),
    )?;
    Ok(Json(json!({ "ok": true })))
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

/// 质检单：合格留库；不合格**自动按订单单价退货**（负到货 + 负采购入库同事务）
async fn qc_order_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<QcReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let insp = parse_money_checked(&req.qty_insp)?;
    let fail = if req.qty_fail.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&req.qty_fail)?
    };
    let inspector = if req.inspector.trim().is_empty() {
        user.username().to_string()
    } else {
        req.inspector.trim().to_string()
    };
    let id = findb::procurement::qc_save(
        &db,
        req.po_id,
        insp,
        fail,
        &inspector,
        date,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "库存",
        "质检",
        &format!(
            "PO#{} 检验 {} 合格 {} 不合格 {}",
            req.po_id,
            insp.fmt_qty(),
            (insp - fail).fmt_qty(),
            fail.fmt_qty()
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "qty_pass": (insp - fail).fmt_qty(),
        "qty_fail": fail.fmt_qty(),
    })))
}

/// 低于安全库存（item_plan 安全量 vs 现有库存）
async fn below_safety_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let rows = findb::inventory2::below_safety(&*db)?;
    Ok(Json(json!({
        "rows": rows
            .iter()
            .map(|(i, on, s)| json!({ "item": i, "on_hand": on.fmt_qty(), "safety": s.fmt_qty() }))
            .collect::<Vec<_>>()
    })))
}

// ---- 单据下推与追溯（对标金蝶 源单→目标单） ----

/// 请购单下推采购订单：审批后可推，拆单允许多次（每次新订单 + 新勾稽）
async fn push_req_po(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let (po_id, po_no) = findb::docflow::req_push_po(&db, id, period, user.username())?;
    db.log(
        user.username(),
        "采购",
        "下推采购订单",
        &format!("请购#{id} → 采购订单 {po_no}"),
    )?;
    Ok(Json(json!({ "ok": true, "po_id": po_id, "po_no": po_no })))
}

/// 单据链：`?kind=req|po|so&id=` —— 上游源单（doc_link）+ 下游/执行单据（到货付款/发货收款）
async fn get_doc_links(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let kind = q.get("kind").cloned().unwrap_or_default();
    if !matches!(kind.as_str(), "req" | "po" | "so" | "invoice" | "receipt") {
        return Err(AppError::bad_request(
            "kind 只能是 req / po / so / invoice / receipt",
        ));
    }
    let id: i64 = q
        .get("id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::bad_request("缺少 id"))?;
    let rows = findb::docflow::doc_chain(&db, &kind, id)?;
    Ok(Json(json!({ "rows": rows })))
}

/// 最近采购价历史（按日期倒序）：采购订单编辑器「单价留空自动带出」数据源
async fn price_history_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::OrderOps)?;
    let db = &user.db;
    let item = q.get("item").map(|s| s.trim()).unwrap_or("");
    if item.is_empty() {
        return Err(AppError::bad_request("缺少 item"));
    }
    let rows = findb::procurement::price_history(&db, item)?;
    Ok(Json(json!({
        "rows": rows
            .iter()
            .map(|(s, p, d)| json!({ "supplier": s, "price": p.fmt_qty(), "date": d }))
            .collect::<Vec<_>>()
    })))
}

// ---- 生产订单：下达 / 开工 / 领料 / 完工入库 + BOM（工厂链「业务单据同步凭证」） ----

#[derive(Deserialize)]
struct OutsourceFeeReq {
    amount: String,
    #[serde(default)]
    date: String,
}

/// 委外加工费确认：借 500102 生产成本-直接人工 / 贷 应付科目（供应商辅助）；
/// 金额并入该订单人工要素，完工时随 140501 结转（委外=生产的变体，全链复用）。
async fn outsource_fee_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<OutsourceFeeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let date = prod_act_date(&req.date);
    let amount = parse_money_checked(&req.amount)?;
    let vid = findb::manufacturing::outsource_fee(&db, id, amount, date, user.username())?;
    db.log(
        user.username(),
        "生产",
        "委外加工费",
        &format!("PO#{id} ×{} 凭证#{vid}", amount.fmt_qty()),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
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

/// MPS 运行：需求聚合（销售未发量 + 手工行）→ 净算计划量（扣现有库存与在制）
async fn run_mps(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<MpsRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let mut extra: Vec<(String, Money, String)> = Vec::with_capacity(req.demands.len());
    for d in &req.demands {
        extra.push((
            d.item_code.clone(),
            parse_money_checked(&d.qty)?,
            d.source.clone(),
        ));
    }
    if req.from_sales {
        extra.extend(advanced::mrp_demands_from_sales(
            &db,
            current_period(&state, &user),
        )?);
    }
    if extra.is_empty() {
        return Err(AppError::bad_request(
            "请提供手工需求，或勾选「从销售订单收集」",
        ));
    }
    let due = if req.due_date.trim().is_empty() {
        (chrono::Local::now().date_naive() + chrono::Duration::days(7))
            .format("%Y-%m-%d")
            .to_string()
    } else {
        req.due_date.trim().to_string()
    };
    // 销售需求已在 handler 合并，findb 侧不再取（sales_period=0）
    let run_at = advanced::mps_run(&db, &extra, Period::from_ymm(0), &due)?;
    let rows = advanced::mps_latest(&*db)?;
    db.log(
        user.username(),
        "生产",
        "MPS 运算",
        &format!("{} 行，建议交期 {due}", rows.len()),
    )?;
    Ok(Json(
        json!({ "run_at": run_at, "due_date": due, "rows": rows }),
    ))
}

/// MPS 最近一次结果
async fn get_mps_latest(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": advanced::mps_latest(&*db)? })))
}

#[derive(Deserialize)]
struct MpsConvertReq {
    /// 空 = 用行净算计划量
    #[serde(default)]
    qty: String,
}

/// MPS 下达：状态 open → converted 并一键生成已下达生产订单（数量可改）
async fn convert_mps(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<MpsConvertReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let q = if req.qty.trim().is_empty() {
        None
    } else {
        Some(parse_money_checked(&req.qty)?)
    };
    let (oid, no) =
        advanced::mps_convert_to_order(&db, id, q, current_period(&state, &user), user.username())?;
    db.log(
        user.username(),
        "生产",
        "MPS 下达",
        &format!("行 #{id} → 生产订单 {no}"),
    )?;
    Ok(Json(json!({ "ok": true, "order_id": oid, "order_no": no })))
}

#[derive(Deserialize)]
struct RoughReq {
    /// 日产能（件/日），空 = 10
    #[serde(default)]
    daily_qty: String,
    #[serde(default)]
    period: i32,
}

/// 粗排：件/日产能顺排（v1 口径——工艺路线暂无标准工时，工时口径留待迭代）
async fn rough_mps(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<RoughReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let daily = parse_money_checked(if req.daily_qty.trim().is_empty() {
        "10"
    } else {
        req.daily_qty.trim()
    })?;
    let (orders, load) = advanced::rough_schedule(&db, period, daily)?;
    Ok(Json(
        json!({ "orders": orders, "load": load, "daily_qty": daily }),
    ))
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

/// 细排：批量写回计划开工/完工日（仅未完工订单可排）
async fn schedule_prod(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ScheduleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    if req.items.is_empty() {
        return Err(AppError::bad_request("排产清单为空"));
    }
    let db = &user.db;
    let items: Vec<(i64, String, String)> = req
        .items
        .into_iter()
        .map(|i| (i.id, i.start.trim().to_string(), i.end.trim().to_string()))
        .collect();
    let n = findb::scm::prod_schedule(&db, &items)?;
    db.log(
        user.username(),
        "生产",
        "生产排产",
        &format!("写回 {n} 单计划日期"),
    )?;
    Ok(Json(json!({ "ok": true, "updated": n })))
}

/// 下达生产订单（MRP 结果页「下达」/ 手工）：状态=已下达。
/// 此前生产订单全系统无创建入口（仅 MRP 计划与测试直造），此端点补齐工厂链第一环。
async fn create_prod_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ProdCreateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let code = req.item_code.trim();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少存货编码 item_code"));
    }
    let qty = parse_money_checked(&req.qty)?;
    if !qty.is_positive() {
        return Err(AppError::bad_request("计划数量必须大于 0"));
    }
    let period = current_period(&state, &user);
    let mut order = findb::scm::ProductionOrder {
        id: 0,
        no: String::new(),
        period,
        date: prod_act_date(&req.date),
        item_code: code.to_string(),
        item_name: code.to_string(),
        planned_qty: qty,
        completed_qty: Money::ZERO,
        status: findb::scm::ProdStatus::Released,
        work_center: req.work_center.trim().to_string(),
        so_id: 0, // 独立建单；来自销售订单的走 /api/prod/from-so
        prepared_by: user.username().to_string(),
        memo: String::new(),
        order_kind: if req.kind.trim() == "outsourcing" {
            "outsourcing".to_string()
        } else {
            "inhouse".to_string()
        },
        supplier_code: req.supplier_code.trim().to_string(),
        supplier_name: req.supplier_name.trim().to_string(),
        plan_start: String::new(),
        plan_end: String::new(),
    };
    order.no = findb::scm::prod_next_no(&db, period)?;
    let id = findb::scm::prod_save(&db, &mut order)?;
    db.log(
        user.username(),
        "生产",
        "下达生产订单",
        &format!("{} {} ×{}", order.no, code, qty.fmt_qty()),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "no": order.no })))
}

/// 下达生产订单：草稿 → 已下达。**这是生产订单的审批动作**。
///
/// 为什么要单开一个端点：原来只有 `create_prod_ep`，且创建即 `Released`——
/// 没有一个「下达前把关」的动作点，审批流根本挂不上去（配了流程也永远不生效，
/// 且不报任何错）。这里把「下达」显式化成一步，与金蝶星空「生产订单下达」一致。
///
/// 老路径不动：`create_prod_ep` 仍直接建 `Released`，避免既有调用方行为变化。
async fn prod_release_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    // 先过工作流：未到终态时只推进流程，不改订单状态
    match workflow_gate(
        &db,
        &user,
        findb::workflow::BIZ_PRODUCTION_ORDER,
        id,
        "生产订单",
    )? {
        WfGate::Pending(next) => return Ok(Json(json!({ "ok": true, "pending": next }))),
        WfGate::Rejected => return Ok(Json(json!({ "ok": true, "rejected": true }))),
        WfGate::Continue => {}
    }
    findb::scm::prod_release(&db, id)?;
    db.log(user.username(), "生产", "下达生产订单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true, "status": "已下达" })))
}

/// 开工：已下达 → 生产中（完工入库的前置状态；条件更新防并发）
async fn prod_start_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    findb::manufacturing::prod_start(&db, id)?;
    db.log(user.username(), "生产", "开工", &format!("PO#{id}"))?;
    Ok(Json(json!({ "ok": true })))
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

/// 生产订单变更（草稿/已下达）：数量不得低于已完工；逐字段留痕
async fn update_prod_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ProdUpdateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    if findb::manufacturing::get_prod_order(&db, id)?.is_none() {
        return Err(AppError::not_found("生产订单不存在"));
    }
    let qty = if req.qty.trim().is_empty() {
        None
    } else {
        Some(parse_money_checked(&req.qty)?)
    };
    findb::manufacturing::prod_update(
        &db,
        id,
        qty,
        req.plan_start.as_deref(),
        req.plan_end.as_deref(),
        req.memo.as_deref(),
        user.username(),
    )?;
    db.log(user.username(), "生产", "生产订单变更", &format!("PO#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 生产订单取消（仅草稿/已下达）
async fn cancel_prod_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    if findb::manufacturing::get_prod_order(&db, id)?.is_none() {
        return Err(AppError::not_found("生产订单不存在"));
    }
    findb::manufacturing::prod_cancel(&db, id, user.username())?;
    db.log(user.username(), "生产", "生产订单取消", &format!("PO#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 生产订单变更历史
async fn list_prod_changes(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows: Vec<serde_json::Value> = findb::scm2::change_log_list(&db, "prod", id)?
        .iter()
        .map(|(field, old_v, new_v, by, at)| {
            json!({
                "field": field, "old_value": old_v, "new_value": new_v,
                "changed_by": by, "changed_at": at,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
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

/// 工序检验（仅生产中订单；不合格必选处置；报废同步扣减计划量）
async fn prod_qc_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ProdQcReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    if findb::manufacturing::get_prod_order(&db, id)?.is_none() {
        return Err(AppError::not_found("生产订单不存在"));
    }
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let qty_fail = if req.qty_fail.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&req.qty_fail)?
    };
    let (qid, no, result) = findb::manufacturing::prod_qc_save(
        &db,
        id,
        parse_money_checked(&req.qty_insp)?,
        qty_fail,
        &req.disposition,
        date,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "生产",
        "工序检验",
        &format!("{no} PO#{id} 结论 {result}"),
    )?;
    Ok(Json(
        json!({ "ok": true, "id": qid, "no": no, "result": result }),
    ))
}

/// 工序检验记录
async fn list_prod_qc(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(
        json!({ "rows": findb::manufacturing::prod_qc_list(&db, id)? }),
    ))
}

#[derive(Deserialize)]
struct ProdActReq {
    #[serde(default)]
    date: String,
    #[serde(default)]
    qty: String,
}

/// 领料出库：按 BOM × 计划量 × (1+损耗) 展开，同事务 扣库存 + 归集生产成本 +
/// 出领料凭证（借 500101 生产成本-直接材料 / 贷各物料科目，数量核算）。
/// 凭证期间随订单；仅 已下达/生产中 可领。
async fn prod_issue_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ProdActReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let order = findb::manufacturing::get_prod_order(&db, id)?
        .ok_or_else(|| AppError::bad_request("生产订单不存在"))?;
    if !matches!(
        order.status,
        findb::scm::ProdStatus::Released | findb::scm::ProdStatus::InProgress
    ) {
        return Err(AppError::bad_request("仅已下达/生产中的订单可领料"));
    }
    // 按单限额领料（v1：BOM 全量一次领齐，每订单仅可领一次——重复领料按累计流水拦截；
    // 带数量的部分领/超额补料留待领料单流程迭代）
    let issued = findb::manufacturing::prod_issue_count(&db, &order.no)?;
    if issued > 0 {
        return Err(AppError::bad_request(format!(
            "该订单已领过料（累计 {issued} 笔流水）：按单限额只允许领料一次，补料走退料/人工调整"
        )));
    }
    let date = prod_act_date(&req.date);
    let rows =
        findb::manufacturing::prod_issue_materials(&db, id, date, order.period, user.username())?;
    let total: Money = rows.iter().map(|(_, _, a)| *a).sum();
    db.log(
        user.username(),
        "生产",
        "领料出库",
        &format!("{} 项数 {} 成本 {}", order.no, rows.len(), total.fmt_qty()),
    )?;
    Ok(Json(json!({
        "ok": true,
        "items": rows.len(),
        "total": total.fmt_qty(),
        "rows": rows
            .iter()
            .map(|(c, q, a)| json!({ "item": c, "qty": q.fmt_qty(), "amount": a.fmt_qty() }))
            .collect::<Vec<_>>()
    })))
}

/// 完工入库：qty 留空 = 其余未完工数量；同事务 入库 + 订单推进 completed +
/// 完工结转凭证（借 140501 库存商品数量核算 / 贷 500101~03 各要素，只出非零）。
/// 仅生产中订单可完工（条件更新防重复完工）。
async fn prod_complete_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ProdActReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let order = findb::manufacturing::get_prod_order(&db, id)?
        .ok_or_else(|| AppError::bad_request("生产订单不存在"))?;
    // 完工数量必须落在 [1, 未完工数量] 区间：prod_complete 把 completed_qty 直接覆盖写，
    // 其唯一守卫是条件更新 status='in_progress'——那只防重复完工，拦不住超完工。
    // 不设上限时 {"qty":"999999"} 会把 100 件的订单推进 999999，并按 total_cost/999999
    // 摊出单位成本、连带虚增库存商品。
    let remain = order.planned_qty - order.completed_qty;
    let qty = if req.qty.trim().is_empty() {
        remain
    } else {
        let q = parse_money_checked(&req.qty)?;
        if q > remain {
            return Err(AppError::bad_request(format!(
                "完工数量 {} 超过订单未完工数量 {}",
                q.fmt_qty(),
                remain.fmt_qty()
            )));
        }
        q
    };
    if !qty.is_positive() {
        return Err(AppError::bad_request(
            "完工数量必须大于 0（或订单已全部完工）",
        ));
    }
    let date = prod_act_date(&req.date);
    let move_id =
        findb::manufacturing::prod_complete(&db, id, date, order.period, qty, user.username())?;
    db.log(
        user.username(),
        "生产",
        "完工入库",
        &format!("{} ×{}", order.no, qty.fmt_qty()),
    )?;
    Ok(Json(
        json!({ "ok": true, "move_id": move_id, "qty": qty.fmt_qty() }),
    ))
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

/// BOM 查询（报工页「维护BOM」回显）
async fn get_bom_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let parent = q.get("parent").map(|s| s.trim()).unwrap_or("");
    if parent.is_empty() {
        return Err(AppError::bad_request("缺少 parent（父件编码）"));
    }
    let rows = findb::scm::bom_list(&db, parent)?;
    Ok(Json(json!({
        "rows": rows
            .iter()
            .map(|b| json!({
                "child_code": b.child_code,
                "qty": b.qty.fmt_qty(),
                "loss_rate": b.loss_rate.fmt_qty(),
            }))
            .collect::<Vec<_>>()
    })))
}

/// BOM 保存（覆盖当前版本；领料与 MRP 均按 BOM 展开）
async fn save_bom_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BomReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::ProductionOps)?;
    let db = &user.db;
    let parent = req.parent.trim();
    if parent.is_empty() {
        return Err(AppError::bad_request("缺少父件编码"));
    }
    let mut items: Vec<(String, Money, Money)> = Vec::new();
    for c in &req.children {
        let code = c.child.trim();
        if code.is_empty() {
            continue;
        }
        let qty = parse_money_checked(&c.qty)?;
        if !qty.is_positive() {
            return Err(AppError::bad_request(format!("子件 {code} 用量必须大于 0")));
        }
        let loss = if c.loss.trim().is_empty() {
            Money::ZERO
        } else {
            parse_money_checked(&c.loss)?
        };
        items.push((code.to_string(), qty, loss));
    }
    if items.is_empty() {
        return Err(AppError::bad_request("至少一个子件"));
    }
    findb::scm::bom_save(&db, parent, &items)?;
    db.log(
        user.username(),
        "生产",
        "维护BOM",
        &format!("{parent}：{}个子件", items.len()),
    )?;
    Ok(Json(json!({ "ok": true, "children": items.len() })))
}

// ---- 预算版本 ----

async fn list_budget_versions(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let versions = advanced::bversion_list(&*db)?;
    let current = advanced::bversion_current(&*db)?;
    Ok(Json(
        serde_json::json!({ "versions": versions, "current": current }),
    ))
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

// 预算版本 / 审批流 / 报表附注 / 会计档案的写操作统一要求 Perm::AccountEdit，
// 与其余业务写路由保持一致。不要用 Perm::Report 把关：Report 是只读权限，
// 且每个角色（含只读 Viewer）都自带它，等于对只读账号开放了写入。
async fn save_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BVersionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    advanced::bversion_save(
        &db,
        &advanced::BudgetVersion {
            key: req.key,
            name: req.name,
            is_current: req.is_current,
            created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            memo: req.memo,
        },
    )?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn delete_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    advanced::bversion_delete(&db, &key)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn activate_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(key): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let mut v = advanced::bversion_list(&*db)?
        .into_iter()
        .find(|v| v.key == key)
        .ok_or_else(|| AppError::NotFound(format!("预算版本 {key} 不存在")))?;
    v.is_current = true;
    advanced::bversion_save(&db, &v)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct BCopyReq {
    pub from: String,
    pub to: String,
}

async fn copy_budget_version(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BCopyReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let n = advanced::bversion_copy(&db, &req.from, &req.to)?;
    Ok(Json(serde_json::json!({ "ok": true, "copied": n })))
}

// ---- 审批流 ----

async fn list_approvals(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = advanced::approval_list(&db, 100)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn list_approval_todo(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = advanced::approval_todo(&db, user.username())?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn get_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let ap = advanced::approval_get(&db, id)?
        .ok_or_else(|| AppError::NotFound("审批流不存在".to_string()))?;
    Ok(Json(serde_json::json!({ "approval": ap })))
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

async fn start_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ApprovalStartReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let id = advanced::approval_start(
        &db,
        &req.biz_kind,
        req.biz_id,
        &req.title,
        user.username(),
        &req.approvers,
    )?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct ApprovalActReq {
    pub approve: bool,
    #[serde(default)]
    pub comment: String,
}

async fn act_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<ApprovalActReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let ap = advanced::approval_act(&db, id, user.username(), req.approve, &req.comment)?;
    Ok(Json(serde_json::json!({ "approval": ap })))
}

async fn cancel_approval(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    advanced::approval_cancel(&db, id, user.username())?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- 报表附注 ----

async fn list_notes(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let report_key = q
        .get("report_key")
        .cloned()
        .unwrap_or_else(|| "balance_sheet".to_string());
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let db = &user.db;
    let rows = advanced::note_list(&db, &report_key, period)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
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

async fn save_note(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<NoteReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let mut n = advanced::ReportNote {
        id: req.id,
        report_key: req.report_key,
        period: period_checked(req.period)?,
        seq: req.seq,
        title: req.title,
        content: req.content,
        updated_by: user.username().to_string(),
        updated_at: String::new(),
    };
    let id = advanced::note_save(&db, &mut n)?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn delete_note(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    advanced::note_delete(&db, id)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- 会计电子档案 ----

async fn list_archives(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    // 空串表示"全部"：UI 下拉默认值为空，不能当成具体类型去过滤
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let db = &user.db;
    let rows = advanced::archive_list(&db, period, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
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

async fn create_archive(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ArchiveReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = period_checked(req.period)?;
    let file_no = if req.file_no.trim().is_empty() {
        advanced::archive_next_no(&db, period, &req.kind)?
    } else {
        req.file_no
    };
    let id = advanced::archive_create(
        &db,
        period,
        &req.kind,
        &req.title,
        &file_no,
        &req.payload,
        user.username(),
    )?;
    Ok(Json(
        serde_json::json!({ "ok": true, "id": id, "file_no": file_no }),
    ))
}

async fn get_archive(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let a = advanced::archive_get(&db, id)?
        .ok_or_else(|| AppError::NotFound("档案不存在".to_string()))?;
    Ok(Json(serde_json::json!({ "archive": a })))
}

async fn verify_archive(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let a = advanced::archive_get(&db, id)?
        .ok_or_else(|| AppError::NotFound("档案不存在".to_string()))?;
    let ok = advanced::archive_verify(&a);
    Ok(Json(
        serde_json::json!({ "ok": ok, "content_hash": a.content_hash }),
    ))
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

async fn list_bills(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    // 空串表示"全部"：UI 下拉默认值为空，不能当成具体类型去过滤
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let rows = findb::funds::bill_list(&db, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_bill(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BillReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let parse_date = |s: &str| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))
    };
    let mut b = findb::funds::Bill {
        id: req.id,
        kind: req.kind,
        no: req.no,
        period: period_checked(req.period)?,
        issue_date: parse_date(&req.issue_date)?,
        due_date: parse_date(&req.due_date)?,
        counterpart: req.counterpart,
        bank: req.bank,
        amount: parse_money_checked(&req.amount)?,
        status: if req.status.is_empty() {
            "in_hand".to_string()
        } else {
            req.status
        },
        handled_date: None,
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
        voucher_id: None,
    };
    let id = findb::funds::bill_save(&db, &mut b)?;
    db.log(
        user.username(),
        "资金",
        "保存票据",
        &format!("#{id} {}", b.no),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn bill_transition(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<BillTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    let to = findb::funds::BillStatus::parse(&req.status);
    // 资金动作（背书/贴现/兑付）在 findb 同事务自动生成台账凭证草稿
    let vid = findb::funds::bill_transition(&db, id, to, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "票据流转",
        &format!(
            "#{} → {}{}",
            id,
            to.label(),
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

async fn delete_bill(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::bill_delete(&db, id)?;
    db.log(user.username(), "资金", "删除票据", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
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

async fn list_loans(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    // 空串表示"全部"：UI 下拉默认值为空，不能当成具体类型去过滤
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let rows = findb::funds::loan_list(&db, kind)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_loan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<LoanReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let parse_date = |s: &str| {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))
    };
    let mut l = findb::funds::Loan {
        id: req.id,
        kind: req.kind,
        no: req.no,
        bank: req.bank,
        principal: parse_money_checked(&req.principal)?,
        rate_pct: parse_money_checked(&req.rate_pct)?,
        start_date: parse_date(&req.start_date)?,
        end_date: parse_date(&req.end_date)?,
        status: if req.status.is_empty() {
            "active".to_string()
        } else {
            req.status
        },
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
        voucher_id: None,
        settle_voucher_id: None,
        settle_date: None,
    };
    let id = findb::funds::loan_save(&db, &mut l)?;
    db.log(
        user.username(),
        "资金",
        "保存融资",
        &format!("#{id} {}", l.no),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

#[derive(Deserialize)]
struct LoanSettleReq {
    #[serde(default)]
    date: Option<String>,
}

/// 结清融资（body 可选指定结清日期；findb 同事务自动生成还本凭证）
async fn loan_settle(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let today = chrono::Local::now().date_naive();
    let date = if body.is_empty() {
        today
    } else {
        let req: LoanSettleReq = serde_json::from_slice(&body)
            .map_err(|_| AppError::bad_request("请求体应为 {\"date\":\"YYYY-MM-DD\"}"))?;
        match req.date {
            Some(s) => chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
            None => today,
        }
    };
    let vid = findb::funds::loan_settle(&db, id, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "结清融资",
        &format!(
            "#{id}{}",
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

/// 票据补出台账凭证（流转已自动生成；本端点用于存量台账回填）
async fn bill_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let vid = findb::funds::bill_voucher(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "票据生成凭证",
        &format!("#{id} → 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "id": vid })))
}

/// 融资台账凭证（存续=到账凭证；已结清=还本凭证回填）
async fn loan_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let vid = findb::funds::loan_voucher(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "融资生成凭证",
        &format!("#{id} → 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "id": vid })))
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

async fn list_cash_counts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    Ok(Json(
        serde_json::json!({ "rows": findb::funds::cash_count_list(&*db)? }),
    ))
}

/// 新增/修改现金盘点：账面余额与差异由服务端按资金日报（按日）口径重算
async fn save_cash_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CashCountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let account = if req.account_code.trim().is_empty() {
        "1001".to_string()
    } else {
        req.account_code.trim().to_string()
    };
    let mut c = findb::funds::CashCount {
        id: req.id,
        period: fincore::Period::from_date(date),
        date,
        account_code: account,
        book_amount: Money::ZERO,
        counted: parse_money_checked(&req.counted)?,
        diff: Money::ZERO,
        memo: req.memo,
        voucher_id: None,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::cash_count_save(&db, &mut c)?;
    db.log(
        user.username(),
        "资金",
        "现金盘点",
        &format!(
            "{} {} 实盘 {}（账面 {} 差异 {}）",
            c.account_code,
            c.date.format("%Y-%m-%d"),
            c.counted.fmt_money(),
            c.book_amount.fmt_money(),
            c.diff.fmt_money()
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "book_amount": c.book_amount,
        "diff": c.diff
    })))
}

async fn delete_cash_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::cash_count_delete(&db, id)?;
    db.log(user.username(), "资金", "删除盘点记录", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 盘盈盘亏差异生成凭证
async fn cash_count_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let vid = findb::funds::cash_count_voucher(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "盘点差异出凭证",
        &format!("#{id} → 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "id": vid })))
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

/// 交接班列表（只读，财务报表权限）
async fn list_cash_shifts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let limit = q
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100);
    Ok(Json(
        json!({ "rows": findb::funds::cash_shift_list(&db, limit)? }),
    ))
}

/// 新建交班单：快照当日现金/银行结存、在库票据、未日清账户（服务端计算）
async fn create_cash_shift(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CashShiftReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let id = findb::funds::cash_shift_create(
        &db,
        date,
        user.username(),
        req.to_user.trim(),
        req.memo.trim(),
    )?;
    let s = findb::funds::cash_shift_get(&db, id)?
        .ok_or_else(|| AppError::from(fincore::FinError::msg("交班单创建失败")))?;
    db.log(
        user.username(),
        "资金",
        "交接班",
        &format!(
            "交班 {} 现金 {} 银行 {} 票据 {} 张 {} / 未日清 {} 户",
            date.format("%Y-%m-%d"),
            s.cash_balance.fmt_money(),
            s.bank_balance.fmt_money(),
            s.bill_count,
            s.bill_amount.fmt_money(),
            s.uncleared
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "shift": s })))
}

/// 接班确认（交班人不能自我确认）
async fn confirm_cash_shift(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    findb::funds::cash_shift_confirm(&db, id, user.username())?;
    db.log(user.username(), "资金", "交接班确认", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 取消交班单（仅待确认状态）
async fn cancel_cash_shift(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CashierSign)?;
    let db = &user.db;
    findb::funds::cash_shift_cancel(&db, id)?;
    db.log(user.username(), "资金", "交接班取消", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 日清日期查询：?account=&from=YYYY-MM-DD&to=YYYY-MM-DD（缺省 today~today）
async fn list_day_clear(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let account = q
        .get("account")
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "1001".to_string());
    let parse = |k: &str| -> Result<Option<chrono::NaiveDate>, AppError> {
        match q.get(k) {
            Some(s) => Ok(Some(
                chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
            )),
            None => Ok(None),
        }
    };
    let today = chrono::Local::now().date_naive();
    let from = parse("from")?.unwrap_or(today);
    let to = parse("to")?.unwrap_or(today);
    let dates = findb::funds::day_clear_dates(&db, &account, from, to)?;
    Ok(Json(json!({ "dates": dates })))
}

/// 日清标记 / 取消
async fn set_day_clear(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<DayClearReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let account = if req.account_code.trim().is_empty() {
        "1001".to_string()
    } else {
        req.account_code.trim().to_string()
    };
    let date = chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
        .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?;
    findb::funds::day_clear_set(&db, &account, date, req.clear, user.username())?;
    db.log(
        user.username(),
        "资金",
        if req.clear {
            "日记账日清"
        } else {
            "取消日清"
        },
        &format!("{} {}", account, date.format("%Y-%m-%d")),
    )?;
    Ok(Json(json!({ "ok": true })))
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

async fn list_checks(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::funds::check_list(&*db)? })))
}

/// 支票登记簿新增/修改（备查簿，不入账）
async fn save_check(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CheckReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    if req.no.trim().is_empty() {
        return Err(AppError::bad_request("支票号必填"));
    }
    let issued_date = if req.issued_date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.issued_date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let kind = if req.kind == "cash" {
        "cash"
    } else {
        "transfer"
    };
    let mut c = findb::funds::CheckRow {
        id: req.id,
        no: req.no.trim().to_string(),
        kind: kind.to_string(),
        bank_account: req.bank_account,
        payee: req.payee,
        amount: parse_money_checked(&req.amount)?,
        issued_date,
        status: "issued".to_string(),
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::check_save(&db, &mut c)?;
    db.log(
        user.username(),
        "资金",
        "保存支票",
        &format!("#{id} {}", c.no),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn check_status(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let status = req["status"].as_str().unwrap_or("");
    findb::funds::check_set_status(&db, id, status)?;
    db.log(
        user.username(),
        "资金",
        "支票状态",
        &format!("#{id} → {status}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_check(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::check_delete(&db, id)?;
    db.log(user.username(), "资金", "删除支票", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
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

async fn list_advances(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::funds::advance_list(&*db)? })))
}

/// 新增/修改借支单（建单即 approved；支付/核销走独立端点）
async fn save_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AdvanceReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let no = if req.no.trim().is_empty() {
        format!("JZ{}", chrono::Local::now().format("%Y%m%d%H%M%S"))
    } else {
        req.no.trim().to_string()
    };
    let mut a = findb::funds::Advance {
        id: req.id,
        no,
        period: fincore::Period::from_date(date),
        date,
        employee: req.employee,
        purpose: req.purpose,
        amount: parse_money_checked(&req.amount)?,
        pay_account: if req.pay_account.trim().is_empty() {
            "1001".to_string()
        } else {
            req.pay_account.trim().to_string()
        },
        status: "approved".to_string(),
        paid_date: None,
        paid_voucher_id: None,
        settle_date: None,
        settle_voucher_id: None,
        expense_account: if req.expense_account.trim().is_empty() {
            "660201".to_string()
        } else {
            req.expense_account.trim().to_string()
        },
        memo: req.memo,
        created_by: user.username().to_string(),
        created_at: String::new(),
    };
    let id = findb::funds::advance_save(&db, &mut a)?;
    db.log(
        user.username(),
        "资金",
        "借支建单",
        &format!("#{} {} {} {}", id, a.no, a.employee, a.amount.fmt_money()),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
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

/// 支付借支（body 可选 {date}，默认今天）；同事务生成支付凭证
async fn pay_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = opt_body_date(&body)?.unwrap_or_else(|| chrono::Local::now().date_naive());
    let vid = findb::funds::advance_pay(&db, id, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "借支支付",
        &format!(
            "#{id}{}",
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

/// 核销借支（body: {expense_account, expense_amount, date?}）；同事务生成核销凭证
async fn settle_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    if body.is_empty() {
        return Err(AppError::bad_request(
            "请求体应为 {\"expense_account\":\"660201\",\"expense_amount\":\"1500\",\"date\":\"YYYY-MM-DD\"?}",
        ));
    }
    let req: AdvanceSettleReq = serde_json::from_slice(&body)
        .map_err(|_| AppError::bad_request("请求体格式错误（见接口约定）"))?;
    let date = match req.date {
        Some(s) => chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
        None => chrono::Local::now().date_naive(),
    };
    let expense_account = if req.expense_account.trim().is_empty() {
        "660201".to_string()
    } else {
        req.expense_account.trim().to_string()
    };
    let expense = parse_money_checked(&req.expense_amount)?;
    let vid =
        findb::funds::advance_settle(&db, id, &expense_account, expense, date, user.username())?;
    db.log(
        user.username(),
        "资金",
        "借支核销",
        &format!(
            "#{id} 冲账 {expense}{}",
            vid.map(|v| format!("，凭证 #{v}")).unwrap_or_default()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

async fn delete_advance(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::advance_delete(&db, id)?;
    db.log(user.username(), "资金", "删除借支单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
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

async fn list_receipts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::receipt::receipt_list(&*db)? })))
}

/// 新增收付款单：同事务生成资金凭证 + FIFO 自动核销（对标金蝶收款单/付款单）
async fn create_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ReceiptCreateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let kind = if req.kind == "payment" {
        "payment"
    } else {
        "receipt"
    };
    let amount = parse_money_checked(&req.amount)?;
    // 审核流：只落草稿单据；凭证与自动核销在「审核」（VoucherAudit）时生成
    let id = findb::receipt::receipt_create(
        &db,
        kind,
        date,
        &req.fund_account,
        &req.party,
        &req.bank,
        &req.cross_party,
        parse_money_checked(&req.cross_amount)?,
        amount,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "资金",
        "新增收付款单（待审核）",
        &format!(
            "#{id} {} {}",
            if kind == "receipt" {
                "收款"
            } else {
                "付款"
            },
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "status": "draft" })))
}

async fn delete_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::receipt::receipt_delete(&db, id)?;
    db.log(user.username(), "资金", "删除收付款单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
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

/// 审核收付款单（对标金蝶）：同事务生成资金凭证 + FIFO 自动核销。审核权 =
/// **VoucherAudit 或 VoucherPost**（见 `require_fund_doc_gate`）——出纳录单，
/// 有记账权的人把关；两者都没有时（出纳想审自己提的单）仍然拦住。
async fn audit_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let db = &user.db;
    // 工作流优先：有流程 → 参与人/审核权由拦截器判定，终态才出凭证；
    // 无流程（默认流）→ 维持原审核权 VoucherAudit（出纳录单、审核人把关）
    match findb::workflow::intercept(&db, findb::workflow::BIZ_RECEIPT, id, &user.user, true, "")? {
        findb::workflow::Gate::Pending { next } => {
            db.log(
                user.username(),
                "审批",
                "工作流节点",
                &format!("收付款#{id} → {next}"),
            )?;
            return Ok(Json(json!({ "ok": true, "pending": next })));
        }
        findb::workflow::Gate::Final { approved: false } => {
            return Ok(Json(json!({ "ok": true, "rejected": true })));
        }
        findb::workflow::Gate::NoFlow => {
            require_fund_doc_gate(&user)?;
        }
        findb::workflow::Gate::Final { approved: true } => {}
    }
    let (vid, settled) = findb::receipt::receipt_audit(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "审核收付款单",
        &format!("#{id} 凭证 #{vid}（自动核销 {settled} 笔）"),
    )?;
    Ok(Json(
        json!({ "ok": true, "voucher_id": vid, "settled": settled }),
    ))
}

/// 撤销审核：删除未记账凭证（含核销配对清理）→ 单据回草稿
async fn unaudit_receipt(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    findb::receipt::receipt_unaudit(&db, id)?;
    db.log(
        user.username(),
        "资金",
        "撤销审核收付款单",
        &format!("#{id}"),
    )?;
    Ok(Json(json!({ "ok": true })))
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

async fn list_transfers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    Ok(Json(
        json!({ "rows": findb::transfer::transfer_list(&*db)? }),
    ))
}

/// 新增资金调拨单：只落草稿单据；凭证在「审核」时生成（对标金蝶录单 → 审核）
async fn create_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<TransferCreateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let amount = parse_money_checked(&req.amount)?;
    let id = findb::transfer::transfer_create(
        &db,
        date,
        &req.from_account,
        &req.to_account,
        &req.bank,
        amount,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "资金",
        "新增资金调拨单（待审核）",
        &format!(
            "#{id} {}→{} {}",
            req.from_account.trim(),
            req.to_account.trim(),
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "status": "draft" })))
}

/// 审核资金调拨单：生成凭证（借 转入 / 贷 转出）。审核权同收付款单
/// （`require_fund_doc_gate`：VoucherAudit 或 VoucherPost）
async fn audit_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    let vid = findb::transfer::transfer_audit(&db, id, user.username())?;
    db.log(
        user.username(),
        "资金",
        "审核资金调拨单",
        &format!("#{id} 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

/// 撤销审核：删除未记账凭证 → 单据回草稿（判据与审核一致）
async fn unaudit_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    findb::transfer::transfer_unaudit(&db, id)?;
    db.log(
        user.username(),
        "资金",
        "撤销审核资金调拨单",
        &format!("#{id}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::transfer::transfer_delete(&db, id)?;
    db.log(user.username(), "资金", "删除资金调拨单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
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

/// 盘点单列表（含明细：账面快照 vs 实盘）
async fn list_counts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::stocktake::count_list(&*db)? })))
}

/// 新建盘点单：服务端按仓库快照账面数量（Warehouse = 仓管作业）
async fn create_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let mut lines: Vec<(String, String, Money, String)> = Vec::new();
    for l in &req.lines {
        if l.item.trim().is_empty() {
            continue;
        }
        lines.push((
            l.item.trim().to_string(),
            l.batch_no.trim().to_string(),
            parse_money_checked(&l.count_qty)?,
            l.memo.clone(),
        ));
    }
    let (id, no) = findb::stocktake::count_create(
        &db,
        period,
        date,
        req.warehouse.trim(),
        &req.memo,
        &lines,
        user.username(),
    )?;
    db.log(
        user.username(),
        "库存",
        "新建盘点单",
        &format!("#{no} {} 行", lines.len()),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "no": no })))
}

/// 应用盘点单：生成其他入库/出库流水 + 盘盈盘亏凭证（金额=差异×标准价）
async fn apply_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let (rid, vid, value) = findb::stocktake::count_apply(&db, id, user.username())?;
    db.log(
        user.username(),
        "库存",
        "应用盘点",
        &format!(
            "#{rid} 价值 {} {}",
            value.fmt_money(),
            vid.map(|v| format!("凭证 #{v}"))
                .unwrap_or_else(|| "未出凭证".into())
        ),
    )?;
    Ok(Json(json!({
        "ok": true,
        "id": rid,
        "voucher_id": vid,
        "value": value.fmt_money(),
        "message": if vid.is_some() { "" } else { "未配置标准价，仅调整库存流水" },
    })))
}

async fn delete_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::stocktake::count_delete(&db, id)?;
    db.log(user.username(), "库存", "删除盘点单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
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

/// 批次出入登记：批号空=自动 BT+日期+序号；同事务写库存流水（带批号，与普通库存同一本账）
async fn register_batch(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BatchRegisterReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let direction = if req.direction == "out" { "out" } else { "in" };
    // 期间取**账套当前期间**，与 stock_adjust / 盘点 / 采购到货同一口径。
    // 曾经传的是 batch_register 内部自己算的「真实当天」，于是账套停在 2026-01
    // 而今天 2026-10 时，批次流水落在 202610：批次台账能看到余额
    //（它按 (item,batch) 汇总、不过滤期间），按期间查的数量金额账却看不到。
    let period = current_period(&state, &user);
    let (id, no, bal) = findb::batch::batch_register(
        &db,
        period,
        &req.item,
        &req.batch_no,
        &req.production_date,
        req.warehouse.trim(),
        req.location.trim(),
        parse_money_checked(&req.qty)?,
        direction,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "库存",
        if direction == "in" {
            "批次入库"
        } else {
            "批次出库"
        },
        &format!("{} {} ×{} 余额 {}", req.item, no, req.qty, bal.fmt_qty()),
    )?;
    Ok(Json(
        json!({ "ok": true, "id": id, "batch_no": no, "balance": bal.fmt_qty() }),
    ))
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

/// 批次调拨（Warehouse）：batch_no 空 → FEFO 近效期自动选批（首条不足量 400 提示分批）
async fn do_transfer(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<TransferReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.trim().is_empty() {
        period.first_day()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let qty = parse_money_checked(&req.qty)?;
    let mut bn = req.batch_no.trim().to_string();
    if bn.is_empty() {
        let rec = findb::batch::fefo_recommend(&db, &req.item, qty)?;
        let first = rec
            .first()
            .ok_or_else(|| AppError::not_found("该存货没有可用批次"))?;
        if first.2 < qty {
            return Err(AppError::bad_request(format!(
                "近效期批次仅余 {}，不足调拨 {}——请分批调拨或指定批号",
                first.2.fmt_qty(),
                qty.fmt_qty()
            )));
        }
        bn = first.0.clone();
    }
    let (out_id, in_id) = findb::inventory2::transfer_do(
        &db,
        period,
        date,
        &req.item,
        &bn,
        &req.from_warehouse,
        &req.to_warehouse,
        qty,
        &req.memo,
    )?;
    db.log(
        user.username(),
        "库存",
        "批次调拨",
        &format!(
            "{} {} {}→{} ×{}（流水 {out_id}/{in_id}）",
            req.item,
            bn,
            req.from_warehouse,
            req.to_warehouse,
            qty.fmt_qty()
        ),
    )?;
    Ok(Json(
        json!({ "ok": true, "out_id": out_id, "in_id": in_id, "batch_no": bn }),
    ))
}

/// 批次成本勾稽（CostOps）：批次层价值 vs 存货辅助账期末，逐存货差异
async fn batch_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let to = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let (detail, totals) = findb::inventory2::batch_cost_report(&db, to)?;
    Ok(Json(
        json!({ "detail": detail, "totals": totals, "period": to.ymm() }),
    ))
}

/// 批次列表（含余额；?item= 过滤）
async fn list_batches(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let item = q.get("item").cloned().unwrap_or_default();
    Ok(Json(
        json!({ "rows": findb::batch::batch_list(&db, &item)? }),
    ))
}

#[derive(Deserialize)]
struct FefoReq {
    #[serde(default)]
    item: String,
    #[serde(default)]
    qty: String,
}

/// FEFO 推荐（近效期先出）
async fn fefo_batches(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<FefoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    if req.item.trim().is_empty() {
        return Err(AppError::bad_request("缺少存货编码 item"));
    }
    let qty = parse_money_checked(&req.qty)?;
    let rows = findb::batch::fefo_recommend(&db, &req.item, qty)?;
    Ok(Json(json!({
        "rows": rows
            .iter()
            .map(|(b, e, t)| json!({ "batch_no": b, "expiry_date": e, "take": t.fmt_qty() }))
            .collect::<Vec<_>>()
    })))
}

/// 临期批次（默认30天窗口）
async fn expiring_batches_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let days = q
        .get("days")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(30);
    Ok(Json(
        json!({ "rows": findb::batch::expiring_batches(&db, days)? }),
    ))
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

async fn list_locations(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::batch::location_list(&*db)? })))
}

async fn save_location(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<LocationReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    let kind = if req.kind.is_empty() {
        "storage"
    } else {
        &req.kind
    };
    let id = findb::batch::location_save(&db, &req.code, &req.name, kind, &req.memo)?;
    db.log(
        user.username(),
        "库存",
        "保存库位",
        &format!("{} {}", req.code, req.name),
    )?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

async fn delete_location(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let db = &user.db;
    findb::batch::location_delete(&db, id)?;
    db.log(user.username(), "库存", "删除库位", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_loan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::funds::loan_delete(&db, id)?;
    // 融资记录挂着结算凭证与还款凭证，是资金模块里审计最敏感的一类，
    // 删除必须留痕（与 save_loan / delete_bill / delete_advance / delete_receipt 同口径）。
    // 这里保留 `?`：删除动作本身幂等（重试不会二次产生业务写入），
    // 与"提交后再写日志"导致的重复入账不是同一类问题。
    db.log(user.username(), "资金", "删除融资", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

/// 资金预算 vs 执行（当期，仅现金/银行科目；实际=当期已记账净额）
async fn get_funds_budget(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let rows = findb::funds::funds_budget(&db, period)?;
    Ok(Json(json!({ "rows": rows })))
}

/// 资金日报（按日）：?date=YYYY-MM-DD（默认今天）→ 上日结余/本日收支/日末结存
async fn get_funds_daily_date(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let date = match q.get("date") {
        Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?,
        None => chrono::Local::now().date_naive(),
    };
    let rows = findb::funds::funds_daily_by_date(&db, date)?;
    Ok(Json(json!({
        "date": date.format("%Y-%m-%d").to_string(),
        "rows": rows
    })))
}

async fn get_funds_daily(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let rows = findb::funds::funds_daily(&db, period)?;
    Ok(Json(
        serde_json::json!({ "period": period.label(), "rows": rows }),
    ))
}

async fn get_funds_forecast(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let period = current_period(&state, &user);
    let fc = findb::funds::funds_forecast(&db, period)?;
    Ok(Json(
        serde_json::json!({ "period": period.label(), "forecast": fc }),
    ))
}

/// 滚动资金预测（票据到期 + 融资起止按期间展开）
async fn funds_forecast_rolling_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let from = q
        .get("from")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let n = q
        .get("periods")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(6);
    Ok(Json(json!({
        "from": period_to_str(from),
        "rows": findb::funds::funds_forecast_rolling(&db, from, n)?,
    })))
}

// ---------------------------------------------------------------------------
// 预算分析
// ---------------------------------------------------------------------------

async fn get_budget_analysis(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let year = q
        .get("year")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or_else(|| current_period(&state, &user).year());
    let version = q.get("version").cloned().unwrap_or_default();
    let upto = q.get("upto").and_then(|s| parse_period(s));
    let rows = findb::mgmt::budget_analysis(&db, year, &version, upto)?;
    let summary = findb::mgmt::budget_analysis_summary(&db, year, &version)?;
    Ok(Json(
        serde_json::json!({ "year": year, "rows": rows, "summary": summary }),
    ))
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

async fn list_cost_configs(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let rows = findb::business::cost_configs(&*db)?;
    Ok(Json(serde_json::json!({ "rows": rows })))
}

async fn save_cost_method(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<CostMethodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let sc = parse_money_checked(&req.standard_cost)?;
    findb::business::item_cost_method_set(&db, &req.item, Some(&req.method), sc)?;
    db.log(
        user.username(),
        "成本",
        "设置计价方式",
        &format!("{} → {}", req.item, req.method),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn clear_cost_method(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(item): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    findb::business::item_cost_method_clear(&db, &item)?;
    Ok(Json(json!({ "ok": true })))
}

/// 通知中心：待办（按岗位实时聚合）+ 动态（审计日志按可见性过滤：AuditLog 权看全量，
/// 否则只看自己的操作）+ 未读计数。since=上次已读水位（前端 localStorage 存服务端 now，
/// 同格式同钟保证字典序=时间序）；缺省=今天内未读。
async fn get_notices(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let cur = current_period(&state, &user);
    let todos = findb::workbench::collect_todos(&db, &user.user, cur)?;
    let see_all = user.can(Perm::AuditLog);
    let me = user.username().to_string();
    let events = db
        .recent_logs(50)?
        .into_iter()
        .filter(|l| see_all || l.user == me)
        .collect::<Vec<_>>();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let since = q.get("since").cloned().filter(|s| !s.is_empty());
    let today = now.get(..10).unwrap_or("").to_string();
    // 未读判定只有一处定义：水位之后（缺省=今天内）。
    // 以前前端给每条事件都画未读点、再用 inline style 遮——重渲染就露回来了，
    // 所以「全部已读」看着像没生效。未读必须由服务端按水位判定并逐条下发。
    let is_unread = |ts: &str| -> bool {
        match &since {
            Some(s) => ts > s.as_str(),
            None => ts.starts_with(&today),
        }
    };
    let unread_events = events.iter().filter(|l| is_unread(&l.ts)).count();
    let ev: Vec<serde_json::Value> = events
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "ts": l.ts,
                "user": l.user,
                "module": l.module,
                "action": l.action,
                "detail": l.detail,
                "unread": is_unread(&l.ts),
            })
        })
        .collect();
    Ok(Json(json!({
        "todos": todos,
        "events": ev,
        "unread_events": unread_events,
        "todo_count": todos.iter().filter(|t| t.count > 0).count(),
        "now": now,
    })))
}

/// 单据的流程实例状态（流程条 / 列表行徽标）：仅工作流四类业务
async fn get_wf_instance_for(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let biz_type = q.get("biz_type").map(String::as_str).unwrap_or("");
    if !matches!(biz_type, "quotation" | "purchase_req" | "claim" | "receipt") {
        return Err(AppError::bad_request(
            "biz_type 只能是 quotation / purchase_req / claim / receipt",
        ));
    }
    let biz_id: i64 = q
        .get("id")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| AppError::bad_request("缺少 id"))?;
    let st = findb::workflow::instance_for(&db, biz_type, biz_id)?;
    Ok(Json(json!({
        "found": st.found,
        "status": st.status,
        "flow_name": st.flow_name,
        "current_label": st.current_label,
        "log": st.log,
    })))
}

/// 存货核算 ↔ 总账 对账（CostOps）：库存流水金额 vs 存货辅助余额，差异定位
async fn gl_reconcile_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = match q
        .get("period")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(p) => parse_period(&p)
            .ok_or_else(|| AppError::bad_request("期间格式应为 202601 或 2026-01"))?,
        None => current_period(&state, &user),
    };
    let rep = findb::balances::gl_reconcile(&db, period, Some(&user.user))?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "stock_total": rep.stock_total,
        "gl_total": rep.gl_total,
        "diff_total": rep.diff_total,
        "rows": rep.rows,
    })))
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

/// 销售成本结转：借 主营业务成本(6401) / 贷 库存商品(140501，数量+存货辅助)，
/// 按计价方法计算销售发出成本。口径已在 stock_summary 收敛为 kind=sale
///（领料/形态转换/调拨不属销售成本）；本期无销售出库 → 不生成凭证；同期间防重复结转。
async fn sales_cost_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SalesCostReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.trim().is_empty() {
        period.last_day()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let method = fincore::engine::costing::CostMethod::parse(&req.method);
    let vid = findb::business::stock_cost_voucher(
        &db,
        period,
        date,
        method,
        "6401",
        "140501",
        user.username(),
    )?;
    match vid {
        Some(id) => {
            // 结转凭证已由 stock_cost_voucher 提交；此处日志失败不能再把请求打成 500，
            // 否则客户端重试会重复结转（findb 只按期间防重，重试得到的是另一个错误）。
            if let Err(e) = db.log(
                user.username(),
                "存货",
                "结转销售成本",
                &format!("{} 凭证#{id}", period_to_str(period)),
            ) {
                error!(voucher = id, error = %e, "写操作日志失败（结转销售成本凭证）");
            }
            Ok(Json(json!({ "ok": true, "voucher_id": id })))
        }
        None => Ok(Json(json!({
            "ok": true,
            "none": true,
            "message": "本期无销售出库，未生成结转凭证"
        }))),
    }
}

async fn run_period_end_cost(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    method: axum::http::Method,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::CostOps)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let apply = q
        .get("apply")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(false);
    // 落库动作不能藏在 GET 里：顶层导航/预取都可能带 Cookie 触发写操作
    if apply && method != axum::http::Method::POST {
        return Err(AppError::bad_request(
            "期末结价落库请使用 POST /api/cost/period-end?apply=1",
        ));
    }
    let rows = findb::business::period_end_cost(&db, period, apply)?;
    Ok(Json(
        serde_json::json!({ "period": period.label(), "rows": rows, "apply": apply }),
    ))
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

async fn list_assets(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let cards: Vec<serde_json::Value> = findb::assets::list(&*db)?.iter().map(asset_json).collect();
    let ledger: Vec<serde_json::Value> = findb::assets::ledger(&db, period)?
        .iter()
        .map(|r| {
            json!({
                "asset": asset_json(&r.asset),
                "accum": r.accum.fmt_money(),
                "net": r.net.fmt_money(),
                "months": r.months,
            })
        })
        .collect();
    let plan: Vec<serde_json::Value> = findb::assets::dep_plan(&db, period)?
        .iter()
        .map(|p| {
            json!({
                "asset_id": p.asset_id, "code": p.code, "name": p.name, "dept": p.dept,
                "expense_account": p.expense_account, "dep_account": p.dep_account,
                "amount": p.amount.fmt_money(), "accum": p.accum.fmt_money(), "net": p.net.fmt_money(),
            })
        })
        .collect();
    let deps: Vec<serde_json::Value> = findb::assets::dep_list_period(&db, period)?
        .iter()
        .map(|d| {
            json!({
                "asset_id": d.asset_id, "period": period_to_str(d.period),
                "amount": d.amount.fmt_money(), "accum": d.accum.fmt_money(),
                "net_value": d.net_value.fmt_money(), "voucher_id": d.voucher_id,
            })
        })
        .collect();
    Ok(Json(json!({
        "period": period_to_str(period),
        "cards": cards,
        "ledger": ledger,
        "plan": plan,
        "deps": deps,
    })))
}

async fn create_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssetReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let a = asset_from_req(&req)?;
    if findb::assets::get_by_code(&db, &a.code)?.is_some() {
        return Err(AppError::bad_request("资产编码已存在"));
    }
    let id = findb::assets::insert(&db, &a)?;
    db.log(
        user.username(),
        "固定资产",
        "新增卡片",
        &format!("{} {}", a.code, a.name),
    )?;
    Ok(Json(json!({ "id": id })))
}

async fn update_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AssetReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let existing =
        findb::assets::get(&db, id)?.ok_or_else(|| AppError::not_found("资产卡片不存在"))?;
    if existing.status == findb::assets::AssetStatus::Disposed {
        return Err(AppError::bad_request("已清理的卡片不能修改"));
    }
    let mut a = asset_from_req(&req)?;
    a.id = id;
    a.status = existing.status;
    a.disposed_period = existing.disposed_period;
    a.dispose_amount = existing.dispose_amount;
    a.voucher_id = existing.voucher_id;
    // 已计提过折旧：影响计价的字段冻结，避免历史折旧与台账对不上
    if !findb::assets::dep_list(&db, id)?.is_empty() {
        let frozen = a.original_value != existing.original_value
            || a.residual_rate != existing.residual_rate
            || a.life_months != existing.life_months
            || a.start_period != existing.start_period
            || a.method != existing.method;
        if frozen {
            return Err(AppError::bad_request(
                "已计提折旧的卡片不能修改原值/残值率/年限/启用期间/折旧方法",
            ));
        }
    }
    findb::assets::update_logged(&db, &existing, &a, user.username(), "卡片编辑")?;
    db.log(
        user.username(),
        "固定资产",
        "修改卡片",
        &format!("{} {}", a.code, a.name),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let a = findb::assets::get(&db, id)?.ok_or_else(|| AppError::not_found("资产卡片不存在"))?;
    findb::assets::delete(&db, id)?;
    db.log(
        user.username(),
        "固定资产",
        "删除卡片",
        &format!("{} {}", a.code, a.name),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_asset_deps(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let rows: Vec<serde_json::Value> = findb::assets::dep_list(&db, id)?
        .iter()
        .map(|d| {
            json!({
                "period": period_to_str(d.period),
                "amount": d.amount.fmt_money(),
                "accum": d.accum.fmt_money(),
                "net_value": d.net_value.fmt_money(),
                "voucher_id": d.voucher_id,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
}

/// 资产卡片变更历史（对标金蝶固定资产变动历史）
async fn list_asset_changes(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::assets::changes(&db, id)? })))
}

/// 固定资产 ↔ 总账对账（原值/累计折旧逐科目差异）
async fn asset_gl_reconcile(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let r = findb::assets::gl_reconcile(&db, period)?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "rows": r.rows,
        "cost_asset": r.cost_asset.fmt_money(),
        "cost_gl": r.cost_gl.fmt_money(),
        "cost_diff": r.cost_diff.fmt_money(),
        "dep_asset": r.dep_asset.fmt_money(),
        "dep_gl": r.dep_gl.fmt_money(),
        "dep_diff": r.dep_diff.fmt_money(),
    })))
}

#[derive(Deserialize)]
struct AssetImpairReq {
    #[serde(default)]
    period: i32,
    amount: String,
    #[serde(default)]
    memo: String,
}

/// 资产减值（记录减值累计；净值口径由卡片原值-累计折旧-减值体现）
async fn impair_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AssetImpairReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    if findb::assets::get(&db, id)?.is_none() {
        return Err(AppError::not_found("资产卡片不存在"));
    }
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let amount = parse_money_checked(&req.amount)?;
    let aid = findb::assets::impair(&db, id, period, amount, &req.memo)?;
    db.log(
        user.username(),
        "固定资产",
        "资产减值",
        &format!("#{id} {} {}", period_to_str(period), amount.fmt_money()),
    )?;
    Ok(Json(json!({ "ok": true, "id": aid })))
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

/// 资产盘点单（草稿）：行 = 卡片 + 是否盘实
async fn save_asset_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AssetCountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    if req.lines.is_empty() {
        return Err(AppError::bad_request("盘点明细不能为空"));
    }
    let period = if req.period > 0 {
        period_checked(req.period)?
    } else {
        current_period(&state, &user)
    };
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let mut lines = Vec::with_capacity(req.lines.len());
    for l in &req.lines {
        if findb::assets::get(&db, l.asset_id)?.is_none() {
            return Err(AppError::bad_request(format!(
                "资产 #{} 不存在",
                l.asset_id
            )));
        }
        lines.push((l.asset_id, l.found.unwrap_or(true), l.memo.clone()));
    }
    let mut c = findb::assets::AssetCount {
        id: 0,
        no: findb::assets::ac_next_no(&db, period)?,
        period,
        date,
        status: "draft".into(),
        prepared_by: user.username().to_string(),
        memo: req.memo,
        lines,
    };
    let id = findb::assets::ac_save(&db, &mut c)?;
    db.log(
        user.username(),
        "固定资产",
        "资产盘点",
        &format!("{} 共 {} 行", c.no, c.lines.len()),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "no": c.no })))
}

/// 资产盘点过账：盘亏（found=false）标记为停用
async fn post_asset_count(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let n = findb::assets::ac_post(&db, id)?;
    db.log(
        user.username(),
        "固定资产",
        "资产盘点过账",
        &format!("#{id} 盘亏 {n}"),
    )?;
    Ok(Json(json!({ "ok": true, "lost": n })))
}

/// 资产盘点单列表（含明细）
async fn list_asset_counts(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::assets::ac_list(&*db)? })))
}

async fn dispose_asset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<AssetDisposeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = period_checked(req.ymm)?;
    let amount = if req.amount.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&req.amount)?
    };
    let vid = findb::assets::dispose(&db, id, period, amount, user.username())?;
    db.log(
        user.username(),
        "固定资产",
        "资产清理",
        &format!(
            "#{id} {} 金额 {} → 转销凭证 #{vid}",
            period_to_str(period),
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid })))
}

async fn depreciate_assets(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = period_checked(req.ymm)?;
    let res = findb::assets::depreciate_period(&db, period, user.username())?;
    Ok(Json(json!({
        "already": res.already,
        "count": res.count,
        "total": res.total.fmt_money(),
        "voucher_id": res.voucher_id,
        "voucher_no": res.voucher_no,
    })))
}

async fn delete_asset_deps_period(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<PeriodReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    let period = period_checked(req.ymm)?;
    let removed = findb::assets::dep_delete_period(&db, period)?;
    db.log(
        user.username(),
        "固定资产",
        "删除本期折旧",
        &format!("{} 共 {removed} 条", period_to_str(period)),
    )?;
    Ok(Json(json!({ "removed": removed })))
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

/// 银行流水生成凭证（出纳）
///
/// 只处理**未勾对**且**能从摘要里认出往来单位**的流水：
/// 进账认客户（贷应收）、支出认供应商（借应付）。认不出的原样留在对账页 ——
/// 刻意不给「待定」科目兜底，那种挂账要到期末才暴露，且往往没人处理。
///
/// 生成的凭证一律草稿：还要过审核、要过出纳签字（资金科目必然命中）才能记账。
/// **不自动记账** —— 拿银行流水直接记账等于把出纳签字这一关自动掉。
async fn gen_bank_vouchers(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = &user.db;
    let r = findb::bank::gen_vouchers(&db, period, &account, user.username())?;
    db.log(
        user.username(),
        "银行对账",
        "流水生成凭证",
        &format!("生成 {} 张 / 跳过 {} 条", r.generated, r.skipped.len()),
    )?;
    Ok(Json(json!({
        "generated": r.generated,
        "linked": r.linked,
        "voucher_ids": r.voucher_ids,
        "skipped": r.skipped.iter().map(|s| json!({
            "stmt_id": s.stmt_id,
            "date": s.biz_date,
            "summary": s.summary,
            "amount": s.amount.fmt_money(),
            "reason": s.reason,
        })).collect::<Vec<_>>(),
        // 把前提说清楚：生成的凭证只含「银行侧 + 往来侧」，不含收入/成本侧。
        // 往来未入账时应收/应付会出现负数 —— 这是这条路子的固有前提，不是 bug。
        "note": "生成的凭证只含银行侧与往来侧两行（借 银行/贷 应收 或 借 应付/贷 银行），\
                 不含收入与成本侧。适用于「往来已记好、只差银行侧没记」的场景；\
                 若销售/采购尚未入账，应收/应付会出现负数，请在记账前核对。\
                 凭证为草稿，需审核（若开启）并经出纳签字后才能记账。",
    })))
}

async fn get_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let period = q
        .get("period")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let db = &user.db;
    let statements = findb::bank::list(&*db, period, account.trim())?;
    let book = findb::bank::book_side(&*db, period, account.trim())?;
    let recon = findb::bank::reconcile(&*db, period, account.trim())?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "account": account.trim(),
        "statements": statements.iter().map(stmt_json).collect::<Vec<_>>(),
        "book": book.iter().map(book_entry_json).collect::<Vec<_>>(),
        "reconcile": reconcile_json(&recon),
    })))
}

async fn import_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankImportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = &user.db;
    let (n, warnings) = findb::bank::import_csv(&db, period, &account, &req.text)?;
    db.log(
        user.username(),
        "银行对账",
        "导入对账单",
        &format!("{account} {n} 条"),
    )?;
    Ok(Json(json!({ "imported": n, "warnings": warnings })))
}

async fn auto_match_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = &user.db;
    let r = findb::bank::auto_match(&db, period, &account, req.tolerance, user.username())?;
    db.log(
        user.username(),
        "银行对账",
        "自动勾对",
        &format!("{account} 成功 {} 对", r.matched),
    )?;
    Ok(Json(json!({
        "matched": r.matched,
        "by_no": r.by_no,
        "by_amount_date": r.by_amount_date,
        "by_amount": r.by_amount,
        "ambiguous": r.ambiguous,
    })))
}

async fn link_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankLinkReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::bank::link(&*db, req.stmt_id, req.entry_id, user.username())?;
    db.log(
        user.username(),
        "银行对账",
        "手工勾对",
        &format!("流水#{} ↔ 分录#{}", req.stmt_id, req.entry_id),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn unlink_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankStmtIdReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::bank::unlink(&db, req.stmt_id)?;
    db.log(
        user.username(),
        "银行对账",
        "取消勾对",
        &format!("流水#{}", req.stmt_id),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn clear_bank(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BankAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let period = period_checked(req.ymm)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少银行科目 account"));
    }
    let db = &user.db;
    let n = findb::bank::clear(&db, period, &account)?;
    db.log(
        user.username(),
        "银行对账",
        "清空对账单",
        &format!("{account} {} 条", period_to_str(period)),
    )?;
    Ok(Json(json!({ "removed": n })))
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
const DEFAULT_SETTLE_OPEN_LIMIT: usize = 200;
const MAX_SETTLE_OPEN_LIMIT: usize = 2000;

async fn get_settle_open(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = q
        .get("upto")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let include_all = q
        .get("all")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(false);
    // 「含未记账」只读开关（对标金蝶单据驱动模型：未审核单据在列表里看得见，
    // 但不参与核销）。默认关闭——核销/账龄/催款一律只认已记账（H-3）。
    // 注意：即使打开，这里返回的未记账行也只是**展示**，核销入口走
    // `auto_settle`/`settle`，它们固定用 posted-only 的 `open_entries`。
    let include_draft = q
        .get("draft")
        .map(|s| s == "1" || s == "true")
        .unwrap_or(false);
    // 可选的往来单位过滤（customer=C01 / supplier=S01），推送到 SQL 层面过滤
    let party_filter = q.get("party").map(|s| s.trim()).filter(|s| !s.is_empty());
    // 关键字过滤：单据号 / 摘要 / 往来单位编码。核销时会计常常只记得「那张 0031 号单」，
    // 让他能在 200 行里捞，而不是让前端去遍历全量（大账套那是 700KB）。
    let kw = q
        .get("q")
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty());
    let db = &user.db;
    let mut rows =
        findb::settle::open_entries_with(&db, account.trim(), upto, include_all, !include_draft, party_filter.as_deref())?;
    if !include_all {
        rows.retain(|e| e.is_open());
    }
    if let Some(kw) = kw.as_deref() {
        rows.retain(|e| {
            e.settle_no.to_lowercase().contains(kw)
                || e.summary.to_lowercase().contains(kw)
                || e.aux_key.to_lowercase().contains(kw)
                || format!("{}-{}", e.word, e.no).to_lowercase().contains(kw)
                || e.no.to_string().contains(kw)
        });
    }
    // 合计与笔数在**分页之前**算：核销页要显示的「共 N 笔、应收合计 X」不能随着
    // 你往后翻页而变小 —— 那会让人以为钱变少了。
    let total = rows.len();
    let draft_cnt = rows.iter().filter(|e| !e.posted).count();
    let sum_debit: fincore::Money = rows.iter().fold(fincore::Money::ZERO, |a, r| a + r.open().max(fincore::Money::ZERO));
    let sum_credit: fincore::Money = rows.iter().fold(fincore::Money::ZERO, |a, r| a + (-r.open()).max(fincore::Money::ZERO));
    // 分页：默认给 200 行。**为什么默认就截断而不保持「全量」**：v1.8.36 实测
    // 单科目 1980 张凭证时这个接口返回 222.9KB，6000 张会到近 700KB，
    // 而核销时人一次只看得到十几行。要全量就显式传 limit=0。
    let limit = q
        .get("limit")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_SETTLE_OPEN_LIMIT)
        .min(MAX_SETTLE_OPEN_LIMIT);
    let offset = q
        .get("offset")
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let page: Vec<&findb::settle::OpenEntry> = if limit == 0 {
        rows.iter().collect()
    } else {
        rows.iter().skip(offset).take(limit).collect()
    };
    Ok(Json(json!({
        "account": account.trim(),
        "upto": period_to_str(upto),
        "include_draft": include_draft,
        "draft_count": draft_cnt,
        "total": total,
        "returned": page.len(),
        "offset": offset,
        "limit": limit,
        "sum_debit": sum_debit.fmt_plain(),
        "sum_credit": sum_credit.fmt_plain(),
        "truncated": limit != 0 && offset + page.len() < total,
        "rows": page
            .iter()
            .map(|e| open_entry_json_with_days(e, upto.last_day()))
            .collect::<Vec<_>>(),
    })))
}

async fn auto_settle_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SettleAutoReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let account = req.account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = if req.ymm > 0 {
        period_checked(req.ymm)?
    } else {
        current_period(&state, &user)
    };
    let tolerance = if req.tolerance.trim().is_empty() {
        Money::parse("0.01").unwrap_or(Money::ZERO)
    } else {
        parse_money_checked(&req.tolerance)?
    };
    let db = &user.db;
    let r = findb::settle::auto_settle(&db, &account, upto, tolerance, user.username())?;
    db.log(
        user.username(),
        "往来",
        "自动核销",
        &format!("{account} {} 对 {}", r.pairs, r.amount.fmt_money()),
    )?;
    Ok(Json(json!({
        "pairs": r.pairs,
        "amount": r.amount.fmt_money(),
        "exact": r.exact,
        "written_off": r.written_off,
    })))
}

async fn manual_settle_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SettleRunReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let amount = parse_money_checked(&req.amount)?;
    let diff = if req.diff.trim().is_empty() {
        Money::ZERO
    } else {
        parse_money_checked(&req.diff)?
    };
    let db = &user.db;
    // 差异为 0 时 settle_with_diff 内部退化成普通核销，不必分支
    let r = findb::settle::settle_with_diff(
        &db,
        req.from_entry,
        req.to_entry,
        amount,
        diff,
        &req.diff_account,
        user.username(),
    )?;
    if diff.is_positive() {
        db.log(
            user.username(),
            "往来",
            "差异核销",
            &format!(
                "分录#{} ↔ 分录#{} {}（差异 {} 记入 {}，凭证 #{:?}）",
                req.from_entry,
                req.to_entry,
                amount.fmt_money(),
                diff.fmt_money(),
                req.diff_account.trim(),
                r.diff_voucher_id
            ),
        )?;
    } else {
        db.log(
            user.username(),
            "往来",
            "手工核销",
            &format!(
                "分录#{} ↔ 分录#{} {}",
                req.from_entry,
                req.to_entry,
                amount.fmt_money()
            ),
        )?;
    }
    Ok(Json(
        json!({ "id": r.settle_id, "diff_voucher_id": r.diff_voucher_id }),
    ))
}

/// 通用往来互抵：跨往来单位 / 跨科目（代收代付、三方抵账）。
/// 自动生成一张过渡凭证把两边接上，再各自核销。
async fn cross_settle_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<SettleCrossReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let amount = parse_money_checked(&req.amount)?;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let db = &user.db;
    let r = findb::settle::settle_cross(
        &db,
        req.debit_entry,
        req.credit_entry,
        amount,
        date,
        user.username(),
    )?;
    db.log(
        user.username(),
        "往来",
        "跨单位互抵",
        &format!(
            "借#{} ↔ 贷#{} {}（过渡凭证 #{:?}）",
            req.debit_entry,
            req.credit_entry,
            amount.fmt_money(),
            r.bridge_voucher_id
        ),
    )?;
    Ok(Json(json!({
        "bridge_voucher_id": r.bridge_voucher_id,
        "settle_ids": r.settle_ids,
    })))
}

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

async fn list_offsets(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    Ok(Json(json!({ "rows": findb::offset::offset_list(&*db)? })))
}

/// 新增往来对冲单：只落草稿单据；凭证与两端 FIFO 核销在「审核」时生成
async fn create_offset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<OffsetCreateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.is_empty() {
        chrono::Local::now().date_naive()
    } else {
        chrono::NaiveDate::parse_from_str(&req.date, "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let amount = parse_money_checked(&req.amount)?;
    let id = findb::offset::offset_create(
        &db,
        date,
        &req.customer,
        &req.supplier,
        &req.ar_account,
        &req.ap_account,
        amount,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "往来",
        "新增往来对冲单（待审核）",
        &format!(
            "#{id} 客户{} ↔ 供应商{} {}",
            req.customer.trim(),
            req.supplier.trim(),
            amount.fmt_money()
        ),
    )?;
    Ok(Json(json!({ "ok": true, "id": id, "status": "draft" })))
}

/// 审核往来对冲单：生成凭证（借 应付 / 贷 应收）+ 两端 FIFO 核销。
/// 判据同资金单据（`require_fund_doc_gate`：VoucherAudit 或 VoucherPost）
async fn audit_offset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    let (vid, n) = findb::offset::offset_audit(&db, id, user.username())?;
    db.log(
        user.username(),
        "往来",
        "审核往来对冲单",
        &format!("#{id} 凭证 #{vid} 核销 {n} 笔"),
    )?;
    Ok(Json(json!({ "ok": true, "voucher_id": vid, "settled": n })))
}

/// 撤销审核：删除未记账凭证（含核销清理）→ 单据回草稿（判据与审核一致）
async fn unaudit_offset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_fund_doc_gate(&user)?;
    let db = &user.db;
    findb::offset::offset_unaudit(&db, id)?;
    db.log(
        user.username(),
        "往来",
        "撤销审核往来对冲单",
        &format!("#{id}"),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_offset(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    findb::offset::offset_delete(&db, id)?;
    db.log(user.username(), "往来", "删除往来对冲单", &format!("#{id}"))?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_settle_records(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let db = &user.db;
    let rows: Vec<serde_json::Value> = findb::settle::list(&db, account.trim())?
        .iter()
        .map(|r| {
            json!({
                "id": r.id, "period": period_to_str(r.period), "account": r.account_code,
                "aux_key": r.aux_key, "from_entry": r.from_entry, "to_entry": r.to_entry,
                "amount": r.amount.fmt_money(), "settled_by": r.settled_by, "settled_at": r.settled_at,
            })
        })
        .collect();
    Ok(Json(json!({ "rows": rows })))
}

async fn unsettle_endpoint(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<UnsettleReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    // 带了金额就是部分撤销；没带或为空串就是整条撤销
    let part = match req.amount.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => Some(fincore::Money::parse(s).map_err(|e| {
            AppError::bad_request(format!("撤销金额「{s}」不是合法金额：{e}"))
        })?),
        _ => None,
    };
    match part {
        Some(amount) => {
            let left = findb::settle::unsettle_part(&db, req.id, amount)?;
            db.log(
                user.username(),
                "往来",
                "部分撤销核销",
                &format!("记录#{} 撤销 {amount}，余 {left}", req.id),
            )?;
            Ok(Json(json!({ "ok": true, "left": left.fmt_plain() })))
        }
        None => {
            findb::settle::unsettle(&db, req.id)?;
            db.log(
                user.username(),
                "往来",
                "取消核销",
                &format!("记录#{}", req.id),
            )?;
            Ok(Json(json!({ "ok": true, "left": "0.00" })))
        }
    }
}

async fn get_settle_aging(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let account = q.get("account").cloned().unwrap_or_default();
    if account.trim().is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = q
        .get("upto")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    // 基准日缺省 = 查询期间的**最后一天**，不是「今天」。
    //
    // 桌面端一直是 `upto.last_day()`（crates/finui/src/views/settle_view.rs:208）。
    // 网页端缺省「今天」的后果：查 2026-03 的账龄，所有单据都显示成「210 天」，
    // 全落在最老的那一档；同一份数据桌面端显示 3 天、网页显示 210 天，
    // 导出的 Excel 跟网页一致 —— 两个答案，谁也不敢拿去给老板看。
    let as_of = q
        .get("as_of")
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| upto.last_day());
    let by_year = q.get("scheme").map(|s| s == "year").unwrap_or(false);
    let buckets = if by_year {
        fincore::engine::aging::buckets_by_year()
    } else {
        fincore::engine::aging::buckets_by_days()
    };
    let db = &user.db;
    let lines = findb::settle::aging(&db, account.trim(), upto, as_of, &buckets, None)?;
    let labels: Vec<&str> = buckets.iter().map(|b| b.label).collect();
    // key 是**裸编码**（`C01`），名称另查 —— 页面上显示 `customer=C01` 是内部存储格式，
    // 对用户没有意义。以前 key 本身就是整条 aux_key，前端想显示名称也做不到。
    let party = findb::settle::party_kind_in(&db, account.trim());
    let names: std::collections::HashMap<String, String> =
        findb::auxs::list(&db, &fincore::AuxQuery::kind(party))?
            .into_iter()
            .map(|a| (a.code, a.name))
            .collect();
    let rows: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| {
            json!({
                "key": l.key,
                "name": names.get(&l.key).cloned().unwrap_or_default(),
                "amounts": l.amounts.iter().map(|m| m.fmt_money()).collect::<Vec<_>>(),
                "total": l.total.fmt_money(),
                "credit_total": l.credit_total.fmt_money(),
                "net": l.net().fmt_money(),
                "max_days": l.max_days,
                "pay_days": l.pay_days,
                "overdue_days": l.overdue_days,
            })
        })
        .collect();
    Ok(Json(json!({
        "account": account.trim(),
        "as_of": as_of.format("%Y-%m-%d").to_string(),
        "buckets": labels,
        "rows": rows,
    })))
}

/// 往来账龄**导出成 Excel**（同一份取数，只是换成文件）。
///
/// 为什么单做一件事：账龄表现在只能截图。老板要发给客户、或者存档给会计，
/// 截图既不能改也不能算。这里与 `/api/settle/aging` 走同一条取数路径
/// （`findb::settle::aging`），所以界面上的数字与文件里的数字不会分叉。
async fn export_settle_aging_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let account = q.get("account").cloned().unwrap_or_else(|| "1122".into());
    let account = account.trim().to_string();
    if account.is_empty() {
        return Err(AppError::bad_request("缺少往来科目 account"));
    }
    let upto = q
        .get("upto")
        .and_then(|s| parse_period(s))
        .unwrap_or_else(|| current_period(&state, &user));
    let as_of = q
        .get("as_of")
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| chrono::Local::now().date_naive());
    let by_year = q.get("scheme").map(|s| s == "year").unwrap_or(false);
    let buckets = if by_year {
        fincore::engine::aging::buckets_by_year()
    } else {
        fincore::engine::aging::buckets_by_days()
    };
    let db = &user.db;
    let lines = findb::settle::aging(db, &account, upto, as_of, &buckets, None)?;
    let party = findb::settle::party_kind_in(db, &account);
    let names: std::collections::HashMap<String, String> = findb::auxs::list(
        db,
        &fincore::AuxQuery::kind(party),
    )?
    .into_iter()
    .map(|a| (a.code, a.name))
    .collect();

    let mut headers: Vec<String> = vec![
        if party == fincore::AuxKind::Customer { "客户编码".into() } else { "供应商编码".into() },
        if party == fincore::AuxKind::Customer { "客户名称".into() } else { "供应商名称".into() },
    ];
    for b in &buckets {
        headers.push(b.label.to_string());
    }
    headers.push("往来余额".into());
    headers.push("其中预收预付".into());
    headers.push("净额".into());
    headers.push("最久天数".into());
    headers.push("账期".into());
    headers.push("逾期天数".into());

    let mut sheet = crate::xlsxout::Sheet::plain(
        "往来账龄",
        &headers.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &{
            use crate::xlsxout::ColFmt;
            let mut c = vec![ColFmt::Text, ColFmt::Text];
            for _ in &buckets {
                c.push(ColFmt::Money);
            }
            c.push(ColFmt::Money);
            c.push(ColFmt::Money);
            c.push(ColFmt::Money);
            c.push(ColFmt::Int);
            // 账期（没填就空着，不要写 0 —— 0 天账期和「没约定」是两件事）
            c.push(ColFmt::Int);
            c.push(ColFmt::Int);
            c
        },
    );
    let mut rows: Vec<Vec<String>> = Vec::new();
    let (mut t_sum, mut t_net, mut t_credit) = (Money::ZERO, Money::ZERO, Money::ZERO);
    let mut bucket_sum = vec![Money::ZERO; buckets.len()];
    for l in &lines {
        let mut r = vec![
            l.key.clone(),
            names.get(&l.key).cloned().unwrap_or_default(),
        ];
        for (i, m) in l.amounts.iter().enumerate() {
            r.push(m.fmt_plain());
            if let Some(slot) = bucket_sum.get_mut(i) {
                *slot += *m;
            }
        }
        r.push(l.total.fmt_plain());
        r.push(l.credit_total.fmt_plain());
        r.push(l.net().fmt_plain());
        r.push(l.max_days.to_string());
        r.push(l.pay_days.map(|d| d.to_string()).unwrap_or_default());
        r.push(l.overdue_days.to_string());
        t_sum += l.total;
        t_credit += l.credit_total;
        t_net += l.net();
        rows.push(r);
    }
    let mut total_row = vec!["".to_string(), "合计".to_string()];
    for m in &bucket_sum {
        total_row.push(m.fmt_plain());
    }
    total_row.push(t_sum.fmt_plain());
    total_row.push(t_credit.fmt_plain());
    total_row.push(t_net.fmt_plain());
    total_row.push(String::new());
    total_row.push(String::new());
    total_row.push(String::new());
    rows.push(total_row);
    let em = vec![rows.len() - 1];
    sheet.rows = rows;
    sheet = sheet.with_emphasis(em);
    // 导出是数据外带：应收/应付会计现在也有这个权限了，所以要留痕 ——
    // 谁在什么时候把多少家的往来余额带出去，事后要查得到。
    db.log(
        user.username(),
        "导出",
        "导出往来账龄",
        &format!("{} 至 {} · 科目 {} · {} 行", upto.label(), as_of, account, lines.len()),
    )?;
    let fname = format!("往来账龄_{}_{}.xlsx", account, as_of.format("%Y%m%d"));
    build_xlsx(&[sheet], &fname, "往来账龄")
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

/// 对账单列表（可按 kind=ar|ap 过滤）
async fn list_statements(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let kind = q.get("kind").map(|s| s.as_str()).filter(|s| !s.is_empty());
    let rows = findb::statement::list(&db, kind, None)?;
    Ok(Json(json!({
        "rows": rows.iter().map(statement_json).collect::<Vec<_>>(),
    })))
}

/// 生成对账单（服务端算期初/本期发生/期末并快照，不信前端金额）
async fn create_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<StatementReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let from = period_checked(req.from)?;
    let to = period_checked(req.to)?;
    let s = findb::statement::create(
        &db,
        &req.kind,
        &req.account,
        &req.party_code,
        &req.party_name,
        from,
        to,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "往来",
        "生成对账单",
        &format!(
            "{} {} {}~{}",
            s.no,
            s.kind_label(),
            s.party_code,
            period_to_str(to)
        ),
    )?;
    Ok(Json(statement_json(&s)))
}

/// 对账单状态流转：发出 / 客户回签确认 / 作废
async fn statement_status_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<StatementStatusReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    // 回签确认必须留痕是谁确认的（客户签字 or 代签人），不能空着
    let who = if req.status == "confirmed" {
        let c = req.confirmed_by.trim();
        if c.is_empty() {
            return Err(AppError::bad_request("回签确认必须填写确认人"));
        }
        c
    } else {
        user.username()
    };
    findb::statement::set_status(&db, id, &req.status, who)?;
    db.log(
        user.username(),
        "往来",
        "对账单状态",
        &format!(
            "#{} → {}（确认人 {}）",
            id,
            findb::statement::status_label(&req.status),
            who
        ),
    )?;
    let s = findb::statement::get(&db, id)?.ok_or_else(|| AppError::not_found("对账单"))?;
    Ok(Json(statement_json(&s)))
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

/// 催款单/对账函列表（可按 kind=ar|ap 过滤）
async fn list_dunnings(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let rows = findb::settle::dunning_list(&db, q.get("kind").map(String::as_str), None)?;
    Ok(Json(json!({ "rows": rows })))
}

/// 生成催款单：按客商快照未核销分录 + 往来期初（服务端计算，无欠款拒绝）
async fn create_dunning(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<DunningReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let date = if req.date.trim().is_empty() {
        chrono::Local::now().date_naive()
    } else {
        NaiveDate::parse_from_str(req.date.trim(), "%Y-%m-%d")
            .map_err(|_| AppError::bad_request("日期格式应为 YYYY-MM-DD"))?
    };
    let d = findb::settle::dunning_create(
        &db,
        &req.kind,
        &req.account,
        &req.party_code,
        &req.party_name,
        date,
        &req.memo,
        user.username(),
    )?;
    db.log(
        user.username(),
        "应收",
        "催款单",
        &format!(
            "{} {} {} 共 {} 笔（{}）",
            d.no,
            d.party_code,
            d.amount.fmt_money(),
            d.item_count,
            if d.kind == "ar" {
                "催款"
            } else {
                "对账函"
            }
        ),
    )?;
    Ok(Json(json!({ "ok": true, "dunning": d })))
}

/// 催款单状态流转：draft → sent/settled/cancelled；sent → settled/cancelled
async fn dunning_status_ep(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<DunningStatusReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    if findb::settle::dunning_get(&db, id)?.is_none() {
        return Err(AppError::not_found("催款单不存在"));
    }
    findb::settle::dunning_status(&db, id, req.status.trim())?;
    db.log(
        user.username(),
        "应收",
        "催款单状态",
        &format!("#{id} → {}", req.status.trim()),
    )?;
    Ok(Json(json!({ "ok": true })))
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

async fn get_balance_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let t = statement_table(
        &db,
        &user,
        "balance_sheet",
        from,
        to,
        vec![
            Box::new(fincore::report::identity),
            Box::new(fincore::report::to_begin),
        ],
    )?;
    Ok(Json(
        json!({ "from": period_to_str(from), "to": period_to_str(to), "table": table_json(&t) }),
    ))
}

async fn print_balance_sheet(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let t = statement_table(
        &db,
        &user,
        "balance_sheet",
        from,
        to,
        vec![
            Box::new(fincore::report::identity),
            Box::new(fincore::report::to_begin),
        ],
    )?;
    let html = crate::report_html::report_table_html(&t, &user.company, "元", &print_opts_from_q(db.options().print, &q), can_edit);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
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

async fn get_income_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let t = statement_table(
        &db,
        &user,
        "income_statement",
        from,
        to,
        vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
    )?;
    Ok(Json(
        json!({ "from": period_to_str(from), "to": period_to_str(to), "table": table_json(&t) }),
    ))
}

async fn print_income_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let t = statement_table(
        &db,
        &user,
        "income_statement",
        from,
        to,
        vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
    )?;
    let html = crate::report_html::report_table_html(&t, &user.company, "元", &print_opts_from_q(db.options().print, &q), can_edit);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

async fn get_cash_flow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let cf = findb::reports::cash_flow_statement(&db, from, to, Some(&user.user))?;
    let line = |l: &fincore::report::cashflow::CashFlowLine| json!({ "code": l.code, "name": l.name, "inflow": l.inflow.fmt_money(), "outflow": l.outflow.fmt_money(), "net": l.net.fmt_money() });
    Ok(Json(json!({
        "from": period_to_str(from), "to": period_to_str(to),
        "operating": cf.operating.iter().map(line).collect::<Vec<_>>(),
        "operating_net": cf.operating_net.fmt_money(),
        "investing": cf.investing.iter().map(line).collect::<Vec<_>>(),
        "investing_net": cf.investing_net.fmt_money(),
        "financing": cf.financing.iter().map(line).collect::<Vec<_>>(),
        "financing_net": cf.financing_net.fmt_money(),
        "net_increase": cf.net_increase.fmt_money(),
        "begin_cash": cf.begin_cash.fmt_money(),
        "end_cash": cf.end_cash.fmt_money(),
        "unassigned": cf.unassigned.fmt_money(),
        "auto_assigned": cf.auto_assigned,
        "ambiguous": cf.ambiguous,
        "fully_assigned": cf.fully_assigned(),
        "assignment_note": cf.assignment_note(),
        "ties": cf.ties(),
    })))
}

async fn print_cash_flow(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let cf = findb::reports::cash_flow_statement(&db, from, to, Some(&user.user))?;
    let subtitle = format!("{} 至 {}", from.label(), to.label());
    let html = crate::report_html::cash_flow_html(&cf, &user.company, &subtitle, &print_opts_from_q(db.options().print, &q), can_edit);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

async fn print_equity_statement(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    // 预览页要不要给「改了就存」的真控件：版式是账套级的，得跟系统设置同一个权限
    let can_edit = user.require(Perm::SysOption).is_ok();
    user.require(Perm::FinReport)?;
    let db = &user.db;
    let (from, to) = report_range(&state, &user, &q)?;
    let stmt = findb::reports::equity_statement(&db, from, to, Some(&user.user))?;
    let subtitle = format!("{} 至 {}", from.label(), to.label());
    let html = crate::report_html::equity_html(&stmt, &user.company, &subtitle, &print_opts_from_q(db.options().print, &q), can_edit);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

// ===========================================================================
// 账套内基础资料与系统功能（对齐桌面端 finui 补齐）
// ===========================================================================

// ---------------- 会计科目 ----------------
async fn create_account(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AccountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let mut acc = req.account;
    acc.aux = build_aux_mask(&req.aux_kinds);
    let db = &user.db;
    if accounts::get(&db, &acc.code)?.is_some() {
        return Err(AppError::bad_request("科目已存在"));
    }
    // 与桌面端同一套校验：编码必须是数字且符合级长方案、名称非空、上级存在。
    // 少了这一层，非数字编码会写进库，后续 BalanceSnapshot 的前缀聚合按字节
    // 切片时会 panic，整个账套的报表都打不开。
    accounts::chart(&db)?.validate_new(&acc).into_result()?;
    accounts::insert(&db, &acc)?;
    db.log(
        user.username(),
        "基础资料",
        "新建科目",
        &format!("{} {}", acc.code, acc.name),
    )?;
    Ok(Json(json!({"ok": true})))
}

async fn update_account(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AccountReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let mut acc = req.account;
    acc.aux = build_aux_mask(&req.aux_kinds);
    let db = &user.db;
    if accounts::get(&db, &acc.code)?.is_none() {
        return Err(AppError::not_found("科目不存在"));
    }
    accounts::chart(&db)?.validate_edit(&acc).into_result()?;
    accounts::update(&db, &acc)?;
    db.log(
        user.username(),
        "基础资料",
        "修改科目",
        &format!("{} {}", acc.code, acc.name),
    )?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_account(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(code): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AccountEdit)?;
    let db = &user.db;
    if accounts::get(&db, &code)?.is_none() {
        return Err(AppError::not_found("科目不存在"));
    }
    let (vouchers, entries) = accounts::usage(&db, &code)?;
    if vouchers > 0 || entries > 0 {
        return Err(AppError::bad_request("科目已被凭证使用，无法删除"));
    }
    accounts::delete(&db, &code)?;
    db.log(user.username(), "基础资料", "删除科目", &code)?;
    Ok(Json(json!({"ok": true})))
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

async fn list_begin(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<BeginRow>>, AppError> {
    user.require(Perm::Opening)?;
    let db = &user.db;
    // 数据范围：科目区间外的期初不可见（与账簿/报表同口径）
    let rows = balances::list_begin(&*db)?
        .into_iter()
        .filter(|r| user.user.can_see_account(&r.account_code))
        .collect::<Vec<_>>();
    Ok(Json(rows))
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

/// 期初批量粘贴：解析（预览）
async fn parse_begin_batch(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BeginBatchParseReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    let (dirs, leaves) = chart_ctx(&*db)?;
    let dir_of = |code: &str| -> Option<Direction> {
        match dirs.get(code).map(|s| s.as_str()) {
            Some("debit") => Some(Direction::Debit),
            Some("credit") => Some(Direction::Credit),
            _ => None,
        }
    };
    // 库里已有期初数的编码 —— 别覆盖用户填过的
    let existing: Vec<String> = balances::list_begin(&*db)?
        .into_iter()
        .map(|r| r.account_code)
        .collect();
    let parsed = fincore::batch::begin::parse(&req.text, &dir_of, &leaves, &existing);

    // 数据范围：范围外的科目连「能导入」都不该出现在预览里 ——
    // 否则用户会看到一条他无权写、点了才报错的行
    let rows: Vec<serde_json::Value> = parsed
        .rows
        .iter()
        .filter(|r| user.user.can_see_account(&r.code))
        .map(|r| {
            json!({
                "code": r.code,
                "dir": match r.dir { Direction::Debit => "debit", Direction::Credit => "credit" },
                "dir_label": r.dir.label(),
                "amount": r.amount.fmt_money(),
                "skip": r.skip.as_ref().map(|s| s.label()),
            })
        })
        .collect();
    let importable = parsed
        .rows
        .iter()
        .filter(|r| r.skip.is_none() && user.user.can_see_account(&r.code))
        .count();
    let (d, c, diff) = parsed.trial();
    Ok(Json(json!({
        "rows": rows,
        "total": rows.len(),
        "importable": importable,
        "skipped": rows.len() - importable,
        "empty": parsed.is_empty(),
        "trial": {
            "debit": d.fmt_money(),
            "credit": c.fmt_money(),
            "diff": diff.fmt_money(),
            "balanced": diff.is_zero(),
        },
    })))
}

/// 期初批量粘贴：导入前端确认过的行
///
/// 走和「保存」一样的校验路径（`upsert_begin` + 越权检查），而不是直接写 ——
/// 批量导入的量更大，但它并不比手工一行行填更特殊，特殊的是**一次能错几百行**。
async fn import_begin_batch(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BeginBatchImportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Opening)?;
    let db = &user.db;
    let (dirs, leaves) = chart_ctx(&*db)?;
    let mut seen: Vec<String> = Vec::new();
    let mut to_save: Vec<BeginRow> = Vec::new();
    let mut skipped: Vec<serde_json::Value> = Vec::new();

    for r in &req.rows {
        let code = r.code.trim().to_string();
        if code.is_empty() {
            continue;
        }
        if !user.user.can_see_account(&code) {
            return Err(AppError::forbidden(format!(
                "无权维护科目 {code} 的期初余额"
            )));
        }
        if !leaves.contains(&code) && !dirs.contains_key(&code) {
            skipped.push(json!({"code": code, "why": "无此科目"}));
            continue;
        }
        if seen.contains(&code) || req.existing.contains(&code) {
            skipped.push(json!({"code": code, "why": "已填过"}));
            continue;
        }
        seen.push(code.clone());
        let dir = if r.dir == "credit" {
            Direction::Credit
        } else {
            Direction::Debit
        };
        let amt = Money::parse_or_zero(&r.amount);
        if amt.is_zero() {
            skipped.push(json!({"code": code, "why": "金额为零"}));
            continue;
        }
        to_save.push(BeginRow {
            id: 0,
            account_code: code,
            aux: AuxRef::default(),
            year_begin: fincore::dir_amount_to_signed(dir, amt),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        });
    }

    if to_save.is_empty() {
        return Ok(json!({
            "ok": true, "inserted": 0, "skipped": skipped.len(), "detail": skipped
        })
        .into());
    }

    let n = to_save.len();
    for br in &to_save {
        balances::upsert_begin(&db, br)?;
    }
    db.log(
        user.username(),
        "期初",
        "批量粘贴期初余额",
        &format!("{n} 行"),
    )?;
    Ok(json!({
        "ok": true, "inserted": n, "skipped": skipped.len(), "detail": skipped
    })
    .into())
}

async fn save_begin(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BeginSaveReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Opening)?;
    let db = &user.db;
    let (rows, delete_ids) = match req {
        BeginSaveReq::Wrapped { rows, delete_ids } => (rows, delete_ids),
        BeginSaveReq::Rows(rows) => (rows, Vec::new()),
    };
    // 数据范围：范围外的科目不得写期初（防越权维护）
    for r in &rows {
        let code = r.account_code.trim();
        if !code.is_empty() && !user.user.can_see_account(code) {
            return Err(AppError::forbidden(format!(
                "无权维护科目 {code} 的期初余额"
            )));
        }
    }
    // 科目必须真实存在。
    //
    // 为什么这道必须有：期初是所有报表的起点。凭证录入那条路上有「非末级科目不能
    // 记账」「核算数量科目必须填数量」「借贷不平衡」一整套校验，唯独期初这条路
    // 原来是裸的。实测写一个不存在的 `999999` → 200 ok → 试算平衡**没有这一行**、
    // 总额照样报「平衡」→ 资产负债表也查不到。也就是**钱从所有报表里静默消失，
    // 而唯一的兜底守卫（试算平衡）报告一切正常**。会计把科目编码打错一位就踩到。
    //
    // 只校验「存在」，不校验「是不是末级」：期初按汇总层科目（1405 库存商品、
    // 1122 应收账款这类）建余额是常规做法。
    let codes: Vec<String> = rows
        .iter()
        .map(|r| r.account_code.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    if !codes.is_empty() {
        let mut missing: Vec<String> = Vec::new();
        for c in &codes {
            if accounts::get(&db, c)?.is_none() && !missing.contains(c) {
                missing.push(c.clone());
            }
        }
        if !missing.is_empty() {
            return Err(AppError::bad_request(format!(
                "科目 {} 不存在，期初未保存。请先在「会计科目」建好该科目，或检查编码是否输错",
                missing.join("、")
            )));
        }
    }
    // 期初整批导入必须原子：upsert_begin 里是裸 conn.execute，每行各自成隐式事务，
    // 第 k 行失败时前 k-1 行已经提交，客户端既无法判断落地了哪些行，也无法安全重试。
    // 改为在调用方事务里逐行 upsert_begin_on（事务可 Deref 成 Connection），
    // 整批要么全成、要么全回滚；操作日志同事务写入，不会出现"数据进了、日志没进"。
    let tx = db.write_tx()?;
    // 先删后写，且与写入同事务：任一步失败整体回滚，不会出现「删了但没补上」
    let mut deleted = 0usize;
    for id in &delete_ids {
        if *id > 0 {
            balances::delete_begin_on(&tx, *id)?;
            deleted += 1;
        }
    }
    let mut n = 0;
    for r in rows {
        let code = r.account_code.trim();
        if code.is_empty() {
            continue;
        }
        let yb = parse_money_checked(&r.yb)?;
        let year_begin = match r.dir {
            Direction::Debit => yb,
            Direction::Credit => -yb,
        };
        let br = BeginRow {
            id: 0,
            account_code: code.to_string(),
            aux: r.aux,
            year_begin,
            debit_accum: parse_money_checked(&r.ad)?,
            credit_accum: parse_money_checked(&r.ac)?,
            qty_begin: r
                .qty
                .as_ref()
                .filter(|s| !s.trim().is_empty())
                .map(|s| parse_money_checked(s))
                .transpose()?,
        };
        balances::upsert_begin_on(&tx, &br)?;
        n += 1;
    }
    findb::log_on(
        &tx,
        user.username(),
        "期初",
        "保存期初余额",
        &format!(
            "保存 {} 条{}",
            n,
            if deleted > 0 {
                format!("，删除 {deleted} 条")
            } else {
                String::new()
            }
        ),
    )?;
    tx.commit().map_err(findb::DbError::from)?;
    Ok(Json(json!({"ok": true, "count": n, "deleted": deleted})))
}

// ---------------- 操作日志 ----------------
async fn list_logs(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<fincore::user::AuditLog>>, AppError> {
    user.require(Perm::AuditLog)?;
    let db = &user.db;
    let limit: i64 = q
        .get("limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let logs = match q.get("q") {
        Some(kw) if !kw.trim().is_empty() => db.search_logs(kw.trim(), limit)?,
        _ => db.recent_logs(limit)?,
    };
    Ok(Json(logs))
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

async fn list_backups(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Backup)?;
    let dir = state.books_dir.join("backups");
    // 备份目录是全局共享的，只能列出属于当前账套的备份。
    //
    // 不能用 `name.ends_with("_{key}.fbk")`：key 允许下划线，`alice_202610` 的
    // 备份会被 key=`202610` 的账套命中（后缀碰撞 → 跨租户读/恢复）。这里按
    // 固定时间戳前缀解析出真正的 key，再逐字符比较。
    let mut items = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("fbk") {
                let name = e.file_name().to_string_lossy().to_string();
                if !backup_belongs_to(&name, &user.book_key) {
                    continue;
                }
                if let Ok(meta) = std::fs::metadata(&p) {
                    items.push(json!({
                        "name": name,
                        "size": meta.len(),
                        "mtime": meta.modified().map(|t| format!("{:?}", t)).unwrap_or_default(),
                    }));
                }
            }
        }
    }
    items.sort_by(|a, b| b["name"].as_str().cmp(&a["name"].as_str()));
    Ok(Json(json!({ "items": items })))
}

async fn create_backup(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Backup)?;
    let key = user.book_key.clone();
    if !state.books_dir.join(format!("{key}.fbk")).exists() {
        return Err(AppError::not_found("账套文件不存在"));
    }
    let state2 = state.clone();
    let username = user.username().to_string();
    // checkpoint + fs::copy 都是阻塞 IO，放阻塞池，别占死 async worker
    let name = tokio::task::spawn_blocking(move || -> Result<String, AppError> {
        let src = state2.books_dir.join(format!("{key}.fbk"));
        let dir = state2.books_dir.join("backups");
        let _ = std::fs::create_dir_all(&dir);
        // 先 checkpoint 把 WAL 合并回主文件，再复制主文件即可得到完整一致快照
        let db = state2.db_for(&key)?;
        checkpoint_wal(&db)?;
        drop(db);
        let stamp = backup_stamp();
        let dst_name = format!("{}_{}.fbk", stamp, key);
        std::fs::copy(&src, dir.join(&dst_name))?;
        let db = state2.db_for(&key)?;
        db.log(&username, "系统", "备份账套", &format!("备份 {dst_name}"))?;
        Ok(dst_name)
    })
    .await
    .map_err(|e| AppError::Internal(format!("备份任务失败：{e}")))??;
    Ok(Json(json!({"ok": true, "name": name})))
}

async fn restore_backup(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<BackupRestoreReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Backup)?;
    // 路径安全：仅允许 backups 目录下的纯文件名，禁止任何路径穿越；
    // 且只接受属于当前账套的备份（按固定时间戳前缀解析 key，见 backup_belongs_to），
    // 防止跨租户恢复。
    let file_name = match std::path::Path::new(&req.file)
        .file_name()
        .and_then(|s| s.to_str())
    {
        Some(n) if !n.contains("..") && !n.contains('/') && !n.contains('\\') => n.to_string(),
        _ => return Err(AppError::bad_request("非法的备份文件名")),
    };
    if !backup_belongs_to(&file_name, &user.book_key) {
        return Err(AppError::bad_request("该备份不属于当前账套"));
    }
    let src = state.books_dir.join("backups").join(&file_name);
    if !src.exists() {
        return Err(AppError::not_found("备份文件不存在"));
    }
    let key = user.book_key.clone();
    let username = user.username().to_string();
    let state2 = state.clone();
    // 覆盖主库 + 删 WAL + 重注册账套都是阻塞操作，放阻塞池
    tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        // 先验证备份文件本身可读、结构完整，避免把坏文件覆盖到当前库上。
        {
            let conn = rusqlite::Connection::open_with_flags(
                &src,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|e| AppError::bad_request(format!("备份文件无法打开：{e}")))?;
            let integrity: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .map_err(|e| AppError::bad_request(format!("备份文件校验失败：{e}")))?;
            if integrity != "ok" {
                return Err(AppError::bad_request(format!(
                    "备份文件已损坏，未恢复：{integrity}"
                )));
            }
            let has_voucher: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='voucher'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| AppError::bad_request(format!("备份文件校验失败：{e}")))?;
            if has_voucher == 0 {
                return Err(AppError::bad_request("该文件不是 FinBook 账套备份"));
            }
        }

        let dst = state2.books_dir.join(format!("{key}.fbk"));
        let backups_dir = state2.books_dir.join("backups");
        std::fs::create_dir_all(&backups_dir)
            .map_err(|e| AppError::Internal(format!("创建备份目录失败：{e}")))?;
        // 恢复前先 checkpoint 并自动备份一次，避免覆盖无法回退。
        // 自动备份失败必须中止 —— 否则用户以为有退路，实际没有。
        let db = state2.db_for(&key)?;
        checkpoint_wal(&db)?;
        drop(db);
        let stamp = backup_stamp();
        let auto_name = format!("auto_{}_{}.fbk", stamp, key);
        let auto_path = backups_dir.join(&auto_name);
        std::fs::copy(&dst, &auto_path).map_err(|e| {
            AppError::Internal(format!("恢复前自动备份失败，已中止（当前账套未被改动）：{e}"))
        })?;

        // 先复制到临时文件，成功后再替换主库：copy 中途失败不会留下半个 .fbk。
        let tmp = state2.books_dir.join(format!("{key}.fbk.restore.tmp"));
        std::fs::copy(&src, &tmp)
            .map_err(|e| AppError::Internal(format!("写入恢复临时文件失败：{e}")))?;
        state2.books.unregister(&key);
        for ext in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(state2.books_dir.join(format!("{key}.fbk{ext}")));
        }
        if let Err(e) = std::fs::rename(&tmp, &dst) {
            let _ = std::fs::remove_file(&tmp);
            if let Ok(path) = std::fs::canonicalize(&dst) {
                state2.books.register(&path, 16);
            }
            return Err(AppError::Internal(format!("替换账套文件失败：{e}")));
        }
        // 重新注册账套，使后续请求以新文件重新打开
        if let Ok(path) = std::fs::canonicalize(&dst) {
            state2.books.register(&path, 16);
        }
        let db = match state2.db_for(&key) {
            Ok(db) => db,
            Err(e) => {
                // 新库打不开：用刚生成的自动备份回滚，保证用户至少回到恢复前状态。
                let rolled = (|| -> std::io::Result<()> {
                    std::fs::copy(&auto_path, &dst)?;
                    let _ = std::fs::remove_file(
                        state2.books_dir.join(format!("{key}.fbk-wal")),
                    );
                    let _ = std::fs::remove_file(
                        state2.books_dir.join(format!("{key}.fbk-shm")),
                    );
                    Ok(())
                })();
                if let Ok(path) = std::fs::canonicalize(&dst) {
                    state2.books.unregister(&key);
                    state2.books.register(&path, 16);
                }
                if let Err(re) = rolled {
                    return Err(AppError::Internal(format!(
                        "恢复后账套无法打开（{e}），且回滚失败（{re}），请用 {auto_name} 手工恢复"
                    )));
                }
                return Err(AppError::Internal(format!(
                    "恢复后账套无法打开，已回滚到恢复前状态：{e}"
                )));
            }
        };
        // 账套文件已被覆盖，恢复已完成；此处日志失败不能把请求打成 500，
        // 否则客户端看到失败会重试恢复，重复覆盖刚恢复好的库。
        if let Err(e) = db.log(
            &username,
            "系统",
            "恢复账套",
            &format!("从 {file_name} 恢复"),
        ) {
            error!(file = %file_name, error = %e, "写操作日志失败（恢复账套）");
        }
        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(format!("恢复任务失败：{e}")))??;
    Ok(Json(json!({"ok": true})))
}

/// 下载账套备份包：把 .fbk 主文件与 .attachments/ 附件目录打成一个 zip，
/// 一次下载完整账套，避免只备份主库而漏掉附件。
async fn download_backup(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    user.require(Perm::Backup)?;
    let key = user.book_key.clone();
    let src = state.books_dir.join(format!("{key}.fbk"));
    if !src.exists() {
        return Err(AppError::not_found("账套文件不存在"));
    }
    let attach_dir = src.parent().unwrap().join(".attachments");

    // checkpoint 把 WAL 合并回主文件，保证打包的 .fbk 是完整一致快照
    let db = state.db_for(&key)?;
    checkpoint_wal(&db)?;
    // 只打包**当前账套引用**的大附件。`.attachments` 是所有账套共享的
    // 内容寻址目录（sha256 前缀命名），整目录打包会把其他租户的附件一起拖走。
    // 小附件（inline=1）本来就在 .fbk 里，不需要单独打包。
    let attach_paths = book_attachment_paths(&db)?;
    drop(db);

    let stamp = backup_stamp();
    let zip_name = format!("{key}_backup_{stamp}.zip");

    let buf = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, AppError> {
        let mut buf = Vec::new();
        let mut zip = ZipWriter::new(io::Cursor::new(&mut buf));
        let options: FileOptions<'_, ()> = FileOptions::default()
            .compression_method(CompressionMethod::Deflated);
        let zerr = |e| AppError::Internal(format!("zip: {e}"));

        // .fbk 主文件
        zip.start_file(format!("{key}.fbk"), options).map_err(&zerr)?;
        let mut f = File::open(&src)?;
        io::copy(&mut f, &mut zip)?;

        // 当前账套引用的大附件（路径是 attach.rs 生成的扁平 sha 文件名）。
        for rel in attach_paths {
            if rel.contains("..") || rel.contains('/') || rel.contains('\\') {
                continue;
            }
            let p = attach_dir.join(&rel);
            if !p.is_file() {
                // 数据库有记录但磁盘文件缺失：不阻断整包下载，跳过即可。
                continue;
            }
            let name = format!(".attachments/{rel}");
            zip.start_file(name, options).map_err(&zerr)?;
            let mut f = File::open(&p)?;
            io::copy(&mut f, &mut zip)?;
        }

        zip.finish().map_err(&zerr)?;
        Ok(buf)
    })
    .await
    .map_err(|e| AppError::Internal(format!("打包备份失败：{e}")))?; // -> Result<Vec<u8>, AppError>
    let buf = buf?; // -> Vec<u8> // buf: Vec<u8>

    let body = Body::from(buf);
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{zip_name}\""),
        )
        .body(body)
        .map_err(|e| AppError::Internal(format!("构建响应失败：{e}")))?)
}

// ---------------- 凭证模板 ----------------
async fn list_templates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<Vec<template::Template>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    Ok(Json(template::list(&*db)?))
}

async fn create_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(t): Json<template::Template>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let id = template::insert(&db, &t)?;
    db.log(user.username(), "凭证模板", "新建模板", &t.name)?;
    Ok(Json(json!({"id": id})))
}

async fn update_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(t): Json<template::Template>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let mut t = t;
    t.id = id;
    template::update(&db, &t)?;
    db.log(user.username(), "凭证模板", "修改模板", &t.name)?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    template::delete(&db, id)?;
    db.log(user.username(), "凭证模板", "删除模板", &id.to_string())?;
    Ok(Json(json!({"ok": true})))
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

/// 由模板直接生成凭证并回写 last_period（周期性模板据此推进「本期到期」判断）
async fn generate_template(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(req): Json<TemplateGenerateReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = match req.period.filter(|ym| *ym > 0) {
        Some(ym) => period_checked(ym)?,
        None => current_period(&state, &user),
    };
    let date = req_date(&req.date, period.last_day())?;
    let word = "记";
    // 生成凭证 + 回写 last_period 必须原子；同时把“本期已生成”判断放进
    // BEGIN IMMEDIATE，避免双击/并发各生成一张。手工模板允许多次调用，
    // 只有周期性模板才按 last_period 拒绝重复。
    let tx = db.write_tx()?;
    let t = template::get(&db, id)?.ok_or_else(|| AppError::not_found("模板不存在"))?;
    if t.freq != template::Freq::Manual {
        if let Some(last) = t.last_period {
            if period.ymm() <= last.ymm() {
                return Err(AppError::bad_request(format!(
                    "模板「{}」在 {} 已生成过（上次 {}），无需重复生成",
                    t.name,
                    period.label(),
                    last.label()
                )));
            }
        }
    }
    let no = vouchers::next_no_of(&tx, period, word)?;
    let mut v = t.to_voucher(period, date, word, no as i64, user.username())?;
    let vid = vouchers::save_in(&tx, &mut v)?;
    template::mark_generated_on(&tx, id, period)?;
    tx.commit().map_err(findb::DbError::from)?;
    db.log(
        user.username(),
        "凭证模板",
        "生成凭证",
        &format!("{} → 凭证 #{vid}", t.name),
    )?;
    Ok(Json(json!({ "id": vid })))
}

async fn due_templates(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<template::Template>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = match q.get("period").and_then(|s| s.parse::<i32>().ok()) {
        Some(ym) => period_checked(ym)?,
        None => db.options().start_period,
    };
    Ok(Json(template::due_list(&db, period)?))
}

// ---------------- 辅助核算档案 ----------------
async fn list_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<AuxEntity>>, AppError> {
    // 档案列表是**只读参照**（凭证/收付款单都要选客户、供应商、职员……），
    // 任何可记账角色都需要；增删改仍由 AuxEdit 把关。
    user.require(Perm::Report)?;
    let db = &user.db;
    let kind = q
        .get("kind")
        .and_then(|s| AuxKind::from_code(s))
        .unwrap_or(AuxKind::Customer);
    // 默认只列启用的（凭证/收付款单选客商时不该出现停用的），但基础资料页
    // 必须能看见停用档案 —— 否则「停用」在网页端等于**单向操作**：停掉之后
    // 再也找不到它，也就没法恢复。
    let include_disabled = matches!(
        q.get("include_disabled").map(|s| s.as_str()),
        Some("1") | Some("true")
    );
    Ok(Json(auxs::list(
        &db,
        &AuxQuery {
            kind: Some(kind),
            include_disabled,
            ..Default::default()
        },
    )?))
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

/// 存货计划参数读取（编辑器回显）：安全库存 / 前置期 / 批量
async fn get_item_plan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let code = q
        .get("item")
        .map(String::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少 item 存货编码"));
    }
    let db = &user.db;
    let p = findb::advanced::item_plan_get(&db, &code)?;
    Ok(Json(json!({
        "plan": p.as_ref().map(|p| json!({
            "safety_stock": p.safety_stock.fmt_qty(),
            "lead_days": p.lead_days,
            "lot_size": p.lot_size.fmt_qty(),
        })),
    })))
}

/// 存货计划参数保存（补齐「低库存预警 ↔ 设置入口」断链——迁移/建档配套）
async fn save_item_plan(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<ItemPlanReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Warehouse)?;
    let code = req.item_code.trim().to_string();
    if code.is_empty() {
        return Err(AppError::bad_request("缺少 item_code"));
    }
    let db = &user.db;
    findb::advanced::item_plan_upsert(
        &db,
        &findb::advanced::ItemPlan {
            item_code: code.clone(),
            safety_stock: parse_money_checked(&req.safety_stock)?,
            lead_days: req.lead_days.max(0),
            lot_size: parse_money_checked(&req.lot_size)?,
        },
    )?;
    db.log(
        user.username(),
        "档案",
        "存货计划参数",
        &format!(
            "{code} 安全库存 {} 前置期 {} 批量 {}",
            req.safety_stock, req.lead_days, req.lot_size
        ),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn create_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(e): Json<AuxEntity>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    let id = auxs::insert(&db, &e)?;
    db.log(
        user.username(),
        "档案",
        "新建档案",
        &format!("{} {}", e.kind.label(), e.name),
    )?;
    Ok(Json(json!({"id": id})))
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

/// 该类别可预置的常用档案清单（只读，不落库）
///
/// 前端据此决定要不要显示「预置常用档案」按钮、以及把条目名列在按钮旁边 ——
/// 清单只有 fincore 一份（`AuxPreset`），前端不自己硬编码，改一处就够。
async fn get_aux_presets(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<KindQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _ = (state, &user);
    let kind = AuxKind::from_code(&q.kind).unwrap_or(AuxKind::Dept);
    let items: Vec<serde_json::Value> = fincore::auxiliary::preset_aux(kind)
        .iter()
        .map(|p| json!({"code": p.code, "name": p.name, "memo": p.memo}))
        .collect();
    Ok(Json(json!({
        "kind": kind,
        "label": kind.label(),
        "items": items,
        "available": !items.is_empty(),
    })))
}

/// 一键预置常用档案（跳过已存在，不覆盖）
async fn preset_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(q): Json<KindReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    let (n, skipped) = auxs::insert_presets(&db, q.kind)?;
    db.log(
        user.username(),
        "档案",
        &format!("预置{}常用档案", q.kind.label()),
        &format!("{n} 条"),
    )?;
    Ok(Json(json!({"inserted": n, "skipped": skipped})))
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

/// 解析并返回预览。不写库 —— 前端把预览给用户看，确认后再调 PUT 导入。
///
/// 解析逻辑复用 `fincore::batch::auxiliary`，与桌面端**同一份代码**：两端解析结果
/// 必须一致，否则用户会发现「桌面上能粘的，网页上粘不了」。
async fn parse_aux_batch(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AuxBatchParseReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Report)?;
    let db = &user.db;
    // 库里已有的编码 —— 用来标「这个编码已经有了」，避免用户以为导进去了
    let existing = auxs::codes(&db, req.kind)?;
    let parsed = fincore::batch::auxiliary::parse(&req.text, req.kind, &existing);
    let rows: Vec<AuxBatchRowView> = parsed
        .rows
        .iter()
        .map(|r| AuxBatchRowView {
            code: r.code.clone(),
            name: r.name.clone(),
            skip: r.skip.as_ref().map(|s| s.label().to_string()),
            parent_code: r.parent_code.clone(),
            memo: r.memo.clone(),
            disabled: r.disabled,
        })
        .collect();
    let importable = parsed.importable().len();
    Ok(Json(json!({
        "rows": rows,
        "total": rows.len(),
        "importable": importable,
        "skipped": rows.len() - importable,
        "empty": parsed.is_empty(),
    })))
}

/// 导入前端确认过的那些行
async fn import_aux_batch(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<AuxBatchImportReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    // 再查一遍已存在的编码：解析到导入之间可能有人新增了档案。
    // insert_many_skip_existing 本身也会跳过，但那份结果只给「跳过了哪些」，
    // 不给「有多少行本该导入却没进」—— 前端就报不出「你以为导了 10 条，
    // 实际只进了 8 条」。这里自己先筛，好把差额说清楚。
    //
    // ⚠️ 两个集合必须分开：`in_book` 是「真的没进去」，`seen` 是「本批已经处理过
    // 这个编码，用来防重复」。第一版只用一个数组，装成功后被当成 skipped 报出去，
    // 用户看到「已导入 2 条，跳过 2 条」—— 2 + 2 ≠ 2，数字对不上，
    // 第一反应是「这软件出 bug 了」。
    let existing = auxs::codes(&db, req.kind)?;
    let mut items: Vec<AuxEntity> = Vec::new();
    let mut in_book: Vec<String> = Vec::new();
    let mut dup_in_batch: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for r in &req.rows {
        let code = r.code.trim().to_string();
        let name = r.name.trim().to_string();
        if code.is_empty() || name.is_empty() {
            continue;
        }
        if seen.contains(&code) {
            dup_in_batch.push(code);
            continue;
        }
        seen.push(code.clone());
        if existing.contains(&code) {
            in_book.push(code);
            continue;
        }
        let mut e = AuxEntity::new(req.kind, code, name);
        e.parent_code = r.parent_code.as_ref().map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        e.memo = r.memo.trim().to_string();
        e.disabled = r.disabled;
        items.push(e);
    }
    if items.is_empty() {
        return Ok(Json(json!({
            "inserted": 0,
            "skipped": in_book.len() + dup_in_batch.len(),
            "already": in_book,
            "duplicate_in_batch": dup_in_batch,
        })));
    }
    let (n, skipped) = auxs::insert_many_skip_existing(&db, &items)?;
    db.log(
        user.username(),
        "档案",
        &format!("批量导入{}档案", req.kind.label()),
        &format!("{n} 条"),
    )?;
    let mut all_skipped = in_book;
    all_skipped.extend(skipped);
    Ok(Json(json!({
        "inserted": n,
        "skipped": all_skipped.len() + dup_in_batch.len(),
        "already": all_skipped,
        "duplicate_in_batch": dup_in_batch,
    })))
}

async fn update_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(e): Json<AuxEntity>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    let mut e = e;
    e.id = id;
    auxs::update(&db, &e)?;
    db.log(user.username(), "档案", "修改档案", &e.name)?;
    Ok(Json(json!({"ok": true})))
}

async fn delete_aux(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::AuxEdit)?;
    let db = &user.db;
    auxs::delete(&db, id)?;
    db.log(user.username(), "档案", "删除档案", &id.to_string())?;
    Ok(Json(json!({"ok": true})))
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

async fn list_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<business::Payroll>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let rows = business::payroll_list(&db, period)?;
    let rows: Vec<business::Payroll> = scope_payroll(&user, &rows).into_iter().cloned().collect();
    Ok(Json(rows))
}

/// 员工银行信息（辅助档案 props.bank_account / bank_name）
fn employee_bank(db: &findb::Db, code: &str) -> Result<(String, String), AppError> {
    let e = auxs::get(db, AuxKind::Employee, code)?;
    let props = e.map(|x| x.props).unwrap_or_default();
    let acc = props.get("bank_account").cloned().unwrap_or_default();
    let name = props.get("bank_name").cloned().unwrap_or_default();
    Ok((acc.trim().to_string(), name.trim().to_string()))
}

/// 银行代发文件（CSV：账号,户名,金额；缺账号的员工跳过并计数）
async fn export_payroll_bank_file(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let rows = business::payroll_list(&db, period)?;
    let rows = scope_payroll(&user, &rows);
    let mut out = vec![vec![
        "账号".to_string(),
        "户名".to_string(),
        "金额".to_string(),
    ]];
    let mut skipped = 0usize;
    for p in &rows {
        if !p.net.is_positive() {
            continue;
        }
        let (acc, bank) = employee_bank(&db, &p.employee)?;
        if acc.is_empty() {
            skipped += 1;
            continue;
        }
        out.push(vec![
            acc,
            if bank.is_empty() {
                p.employee.clone()
            } else {
                bank
            },
            p.net.fmt_plain(),
        ]);
    }
    db.log(
        user.username(),
        "工资",
        "银行代发文件",
        &format!(
            "{} 共 {} 人（缺账号跳过 {skipped}）",
            period_to_str(period),
            out.len().saturating_sub(1)
        ),
    )?;
    Ok(csv_response(
        &format!("bank_payroll_{}.csv", period.ymm()),
        out,
    ))
}

/// 工资条（单人）：本期工资 + 本年累计
async fn get_payroll_slip(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let employee = q.get("employee").map(String::as_str).unwrap_or("").trim();
    if employee.is_empty() {
        return Err(AppError::bad_request("缺少 employee 参数"));
    }
    if !doc_in_scope(&user, employee) {
        return Err(AppError::forbidden("数据范围受限，不能查看他人的工资条"));
    }
    let p = business::payroll_get(&db, period, employee)?
        .ok_or_else(|| AppError::not_found("该员工本期无工资记录"))?;
    let ytd = business::payroll_ytd(&db, period, employee)?;
    Ok(Json(json!({
        "period": period_to_str(period),
        "payroll": {
            "employee": p.employee, "dept": p.dept,
            "gross": p.gross.fmt_money(), "social": p.social.fmt_money(),
            "housing": p.housing.fmt_money(), "deduction": p.deduction.fmt_money(),
            "additional": p.additional.fmt_money(), "tax_base": p.tax_base.fmt_money(),
            "tax": p.tax.fmt_money(), "net": p.net.fmt_money(),
            "social_co": p.social_co.fmt_money(), "housing_co": p.housing_co.fmt_money(),
            "memo": p.memo,
        },
        "ytd": {
            "income": ytd.income.fmt_money(), "special": ytd.special.fmt_money(),
            "additional": ytd.additional.fmt_money(), "withheld": ytd.withheld.fmt_money(),
            "months": ytd.months,
        },
    })))
}

/// 个税申报表（全员工资薪金，本期口径）
async fn get_payroll_tax_report(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let rows = business::payroll_list(&db, period)?;
    // 数据范围：受限账号只出自己（或本部门）的工资行，与列表口径一致
    let rows = scope_payroll(&user, &rows);
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|p| {
            json!({
                "employee": p.employee, "dept": p.dept,
                "income": p.gross.fmt_money(),
                "special": (p.social + p.housing).fmt_money(),
                "additional": p.additional.fmt_money(),
                "tax_base": p.tax_base.fmt_money(),
                "tax": p.tax.fmt_money(),
                "net": p.net.fmt_money(),
            })
        })
        .collect();
    Ok(Json(
        json!({ "period": period_to_str(period), "rows": items }),
    ))
}

/// 录入/修改一条工资：后端按累计预扣预缴法算个税与实发，前端无需自己算税
async fn save_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollInput>,
) -> Result<Json<business::Payroll>, AppError> {
    user.require(Perm::VoucherNew)?;
    let employee = r.employee.trim();
    if employee.is_empty() {
        return Err(AppError::bad_request("员工编码必填"));
    }
    if !doc_in_scope(&user, employee) {
        return Err(AppError::forbidden("数据范围受限，不能录入他人的工资行"));
    }
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let p = business::payroll_calc(
        &db,
        period,
        employee,
        r.dept.trim(),
        parse_money_checked(&r.gross)?,
        parse_money_checked(&r.social)?,
        parse_money_checked(&r.housing)?,
        parse_money_checked(&r.deduction)?,
        parse_money_checked(&r.additional)?,
        parse_money_checked(&r.social_co)?,
        parse_money_checked(&r.housing_co)?,
        r.memo.trim(),
    )?;
    let id = business::payroll_upsert(&db, &p)?;
    db.log(
        user.username(),
        "工资",
        "保存工资行",
        &format!(
            "{} {} 应发 {} 实发 {}",
            period.label(),
            employee,
            p.gross,
            p.net
        ),
    )?;
    let mut out = p;
    out.id = id;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct GeneratePayrollReq {
    #[serde(default)]
    period: Option<String>,
    rows: Vec<PayrollInput>,
}

/// 批量生成本月工资表（逐条累计预扣预缴算税后落库）
async fn generate_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(req): Json<GeneratePayrollReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    if user.user.data_scope.own_doc_only {
        return Err(AppError::forbidden("数据范围受限的账号不能批量生成工资表"));
    }
    let period = req
        .period
        .as_deref()
        .and_then(parse_period)
        .unwrap_or_else(|| current_period(&state, &user));
    let mut rows: Vec<(
        String,
        String,
        Money,
        Money,
        Money,
        Money,
        Money,
        Money,
        Money,
    )> = Vec::with_capacity(req.rows.len());
    for r in &req.rows {
        if r.employee.trim().is_empty() {
            continue;
        }
        rows.push((
            r.employee.trim().to_string(),
            r.dept.trim().to_string(),
            parse_money_checked(&r.gross)?,
            parse_money_checked(&r.social)?,
            parse_money_checked(&r.housing)?,
            parse_money_checked(&r.deduction)?,
            parse_money_checked(&r.additional)?,
            parse_money_checked(&r.social_co)?,
            parse_money_checked(&r.housing_co)?,
        ));
    }
    if rows.is_empty() {
        return Err(AppError::bad_request("没有可导入的工资行"));
    }
    let n = business::payroll_generate(&db, period, &rows)?;
    db.log(
        user.username(),
        "工资",
        "批量生成工资表",
        &format!("{} 生成 {} 条", period.label(), n),
    )?;
    Ok(Json(json!({ "ok": true, "count": n })))
}

async fn delete_payroll(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    // 删除前拦截已生成凭证的工资行
    let p =
        business::payroll_get_by_id(&db, id)?.ok_or_else(|| AppError::not_found("工资行不存在"))?;
    if !doc_in_scope(&user, &p.employee) {
        return Err(AppError::forbidden("无权操作他人的工资行"));
    }
    if p.voucher_id.is_some() || p.social_voucher_id.is_some() || p.paid_voucher_id.is_some() {
        return Err(AppError::bad_request(
            "该工资行已生成凭证，不能删除（请先删除/反记账相关凭证）",
        ));
    }
    business::payroll_delete(&db, id)?;
    db.log(user.username(), "工资", "删除工资行", &id.to_string())?;
    Ok(Json(json!({ "ok": true })))
}

async fn payroll_ytd(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<business::YtdPayroll>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let employee = q.get("employee").cloned().unwrap_or_default();
    if employee.trim().is_empty() {
        return Err(AppError::bad_request("缺少 employee 参数"));
    }
    if !doc_in_scope(&user, employee.trim()) {
        return Err(AppError::forbidden(
            "数据范围受限，不能查看他人的年度工资累计",
        ));
    }
    Ok(Json(business::payroll_ytd(&db, period, employee.trim())?))
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

/// 工资计提凭证
async fn payroll_accrue(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollAccrueReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let date = req_date(&r.date, period.last_day())?;
    let id = business::payroll_accrue_voucher(
        &db,
        period,
        date,
        r.expense.trim(),
        r.wage_payable.trim(),
        r.social_payable.trim(),
        r.housing_payable.trim(),
        user.username(),
    )?;
    db.log(
        user.username(),
        "工资",
        "生成计提凭证",
        &format!("{} 凭证 {:?}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id })))
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

/// 缴纳社保公积金凭证
async fn payroll_social_pay(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollSocialReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let date = req_date(&r.date, period.last_day())?;
    let id = business::payroll_social_voucher(
        &db,
        period,
        date,
        r.social_payable.trim(),
        r.housing_payable.trim(),
        r.personal_payable.trim(),
        r.bank_account.trim(),
        user.username(),
    )?;
    db.log(
        user.username(),
        "工资",
        "生成社保缴纳凭证",
        &format!("{} 凭证 {:?}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id })))
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

/// 工资发放凭证
async fn payroll_pay(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
    Json(r): Json<PayrollPayReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let date = req_date(&r.date, period.last_day())?;
    let id = business::payroll_pay_voucher(
        &db,
        period,
        date,
        r.payable_account.trim(),
        r.bank_account.trim(),
        r.tax_account.trim(),
        r.social_account.trim(),
        user.username(),
    )?;
    db.log(
        user.username(),
        "工资",
        "生成发放凭证",
        &format!("{} 凭证 {:?}", period.label(), id),
    )?;
    Ok(Json(json!({ "id": id })))
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

async fn list_claims(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Vec<business::Claim>>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    // 空串 = 全部状态：UI 下拉默认值为空，直接 parse 会落到 Draft 只显示草稿
    let status = q
        .get("status")
        .filter(|s| !s.is_empty())
        .map(|s| business::ClaimStatus::parse(s));
    let rows = business::claim_list(&db, period, status)?;
    // 「仅看本人经手的业务单据」：报销按申请人匹配当前登录人
    let rows: Vec<business::Claim> = if user.user.data_scope.own_doc_only {
        rows.into_iter()
            .filter(|c| c.applicant == user.user.display_name || c.applicant == user.user.username)
            .collect()
    } else {
        rows
    };
    Ok(Json(rows))
}

async fn next_claim_no(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let period = query_period(&state, &user, &q);
    let no = business::claim_next_no(&db, period)?;
    Ok(Json(json!({ "no": no })))
}

async fn create_claim(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Json(r): Json<ClaimInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let applicant = r.applicant.trim();
    if applicant.is_empty() {
        return Err(AppError::bad_request("申请人必填"));
    }
    if !doc_in_scope(&user, applicant) {
        return Err(AppError::forbidden("数据范围受限，不能为他人填报销单"));
    }
    let db = &user.db;
    let period = match r.period.filter(|ym| *ym > 0) {
        Some(ym) => period_checked(ym)?,
        None => current_period(&state, &user),
    };
    let date = req_date(&r.biz_date, period.last_day())?;
    let c = business::Claim {
        id: 0,
        period,
        no: business::claim_next_no(&db, period)?,
        biz_date: date,
        applicant: applicant.to_string(),
        dept: r.dept.trim().to_string(),
        reason: r.reason.trim().to_string(),
        amount: parse_money_checked(&r.amount)?,
        status: business::ClaimStatus::Draft,
        items: claim_items(&r.items)?,
        approver: String::new(),
        approved_at: None,
        payer: String::new(),
        paid_at: None,
        voucher_id: None,
        created_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    let id = business::claim_insert(&db, &c)?;
    db.log(
        user.username(),
        "报销",
        "新增报销单",
        &format!("{} {} {}", c.no, applicant, c.amount),
    )?;
    Ok(Json(json!({ "id": id, "no": c.no })))
}

async fn update_claim(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(r): Json<ClaimInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let mut c = business::claim_get(&db, id)?.ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden("数据范围受限，不能修改他人的报销单"));
    }
    if !c.status.editable() {
        return Err(AppError::bad_request("当前状态不允许修改内容"));
    }
    if c.voucher_id.is_some() {
        return Err(AppError::bad_request("已生成凭证的报销单不能修改"));
    }
    let date = req_date(&r.biz_date, c.period.last_day())?;
    c.biz_date = date;
    c.applicant = r.applicant.trim().to_string();
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden("数据范围受限，不能改由他人作为申请人"));
    }
    c.dept = r.dept.trim().to_string();
    c.reason = r.reason.trim().to_string();
    c.amount = parse_money_checked(&r.amount)?;
    c.items = claim_items(&r.items)?;
    business::claim_update(&db, &c)?;
    db.log(
        user.username(),
        "报销",
        "修改报销单",
        &format!("{} {}", c.no, c.amount),
    )?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_claim(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherDelete)?;
    let db = &user.db;
    let c = business::claim_get(&db, id)?.ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden("数据范围受限，不能删除他人的报销单"));
    }
    // 预检：引擎的通用错误会映射成 500，这里给出明确的 400
    if c.voucher_id.is_some() {
        return Err(AppError::bad_request("该报销单已生成凭证，请先删除凭证"));
    }
    let label = c.no.clone();
    business::claim_delete(&db, id)?;
    db.log(user.username(), "报销", "删除报销单", &label)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ClaimTransitionReq {
    /// draft / submitted / approved / rejected / paid
    status: String,
}

async fn claim_transition(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(r): Json<ClaimTransitionReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let to = business::ClaimStatus::parse(&r.status);
    let c = business::claim_get(&db, id)?.ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden("数据范围受限，不能操作他人的报销单"));
    }
    // 工作流拦截（仅 批准/驳回 两个目标）：有流程 → Pending 只推进；NoFlow/Final → 原流转
    let gate_for = if to == business::ClaimStatus::Approved {
        Some(true)
    } else if to == business::ClaimStatus::Rejected {
        Some(false)
    } else {
        None
    };
    if let Some(appr) = gate_for {
        if let findb::workflow::Gate::Pending { next } =
            findb::workflow::intercept(&db, findb::workflow::BIZ_CLAIM, id, &user.user, appr, "")?
        {
            db.log(
                user.username(),
                "报销",
                "工作流节点",
                &format!("#{id} → {next}"),
            )?;
            return Ok(Json(json!({ "ok": true, "status": to, "pending": next })));
        }
    }
    business::claim_transition(&db, id, to, user.username())?;
    db.log(
        user.username(),
        "报销",
        "状态流转",
        &format!("#{id} → {}", to.label()),
    )?;
    Ok(Json(json!({ "ok": true, "status": to })))
}

#[derive(Deserialize)]
struct ClaimVoucherReq {
    /// 贷方支付科目（如 100201 银行存款）
    pay_account: String,
}

async fn claim_voucher(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(r): Json<ClaimVoucherReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::VoucherNew)?;
    let db = &user.db;
    let pay = r.pay_account.trim();
    if pay.is_empty() {
        return Err(AppError::bad_request("请填写支付科目"));
    }
    // 预检给出友好的 400（引擎层也有同样拦截，此处避免落到 500）
    let c = business::claim_get(&db, id)?.ok_or_else(|| AppError::not_found("报销单不存在"))?;
    if !doc_in_scope(&user, &c.applicant) {
        return Err(AppError::forbidden(
            "数据范围受限，不能为他人报销单生成凭证",
        ));
    }
    if let Some(existing) = c.voucher_id {
        // 幂等：支付时已自动出过凭证 → 返回同一张（不新增）
        return Ok(Json(json!({ "id": existing, "already": true })));
    }
    let vid = business::claim_voucher(&db, id, pay, user.username())?;
    db.log(
        user.username(),
        "报销",
        "生成凭证",
        &format!("#{id} 凭证 #{vid}"),
    )?;
    Ok(Json(json!({ "id": vid })))
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

/// 辅助核算档案导出（客户/供应商/部门/职员/项目/存货/现金流量/银行账户）
///
/// `kind` 省略 = 全部维度导在一张表里。两种用法都有意义：按维度导出便于
/// 单独维护某类档案，全量导出用于整套搬运（导回时 `import_aux` 会按类型列
/// 分派，不要求一张表里只有一种）。
async fn export_aux_xlsx(
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let kind = match q.get("kind").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(s) => Some(
            AuxKind::from_code(s)
                .ok_or_else(|| AppError::bad_request(format!("未知的档案类型：{s}")))?,
        ),
        None => None,
    };
    let rows = auxs::list(
        &user.db,
        &AuxQuery {
            kind,
            include_disabled: true,
            ..Default::default()
        },
    )?;
    // 停用档案也导出：导出的目的是搬账套/留底，漏掉停用的会让历史凭证的
    // 辅助核算在目标账套里指向不存在的档案（余额表拆不出那一行）。
    let label = kind.map(|k| k.label()).unwrap_or("全部");
    let sheet = crate::sheets::aux_sheet(&rows, label);
    let n = rows.len();
    user.db.log(
        user.username(),
        "导出",
        "导出基础档案",
        &format!("{label} {n} 条"),
    )?;
    build_xlsx(
        &[sheet],
        &format!("基础档案_{}.xlsx", kind.map(|k| k.code()).unwrap_or("all")),
        "基础档案",
    )
}

/// 存货档案导出（含保质期天与安全库存）
async fn export_items_xlsx(user: CurrentUser) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let rows = auxs::list(
        &user.db,
        &AuxQuery {
            kind: Some(AuxKind::Item),
            include_disabled: true,
            ..Default::default()
        },
    )?;
    // 安全库存单独存在 item_plan 表里，一次全取再做内存查表
    let plans: HashMap<String, Money> = advanced::item_plan_all(&user.db)?
        .into_iter()
        .map(|p| (p.item_code, p.safety_stock))
        .collect();
    let sheet = crate::sheets::items_sheet(&rows, &|code| plans.get(code).copied());
    let n = rows.len();
    user.db
        .log(user.username(), "导出", "导出存货档案", &format!("{n} 条"))?;
    build_xlsx(&[sheet], "存货档案.xlsx", "存货档案")
}

/// 会计科目导出（停用的不导：导入器不接受 disabled 列，
/// 把停用科目混进去会在目标账套里变成启用状态，反而更错）
async fn export_accounts_xlsx(user: CurrentUser) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let chart = accounts::chart(&user.db)?;
    let all: Vec<&Account> = chart
        .all()
        .into_iter()
        .filter(|a| user.user.can_see_account(&a.code))
        .collect();
    let sheet = crate::sheets::accounts_sheet(&all);
    let n = all.len();
    user.db
        .log(user.username(), "导出", "导出会计科目", &format!("{n} 条"))?;
    build_xlsx(&[sheet], "会计科目.xlsx", "会计科目")
}

/// 期初余额导出（列头与通用导入模板一致，可直接改完导回）
async fn export_begin_xlsx(user: CurrentUser) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let chart = accounts::chart(&user.db)?;
    let rows: Vec<BeginRow> = balances::list_begin(&user.db)?
        .into_iter()
        .filter(|r| user.user.can_see_account(&r.account_code))
        .collect();
    let sheet = crate::sheets::begin_sheet(&rows, &|code| {
        chart.get(code).map(|a| a.name.clone()).unwrap_or_default()
    });
    let n = rows.len();
    user.db
        .log(user.username(), "导出", "导出期初余额", &format!("{n} 行"))?;
    build_xlsx(&[sheet], "期初余额.xlsx", "期初余额")
}

/// 科目余额表导出
async fn export_trial_balance_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    let (rows, totals) = trial_balance_data(&state, &user, &q)?;
    let from = q.get("from").cloned().unwrap_or_default();
    let to = q.get("to").cloned().unwrap_or_default();
    let sheet = crate::sheets::trial_balance_sheet(&rows, &totals, &from, &to);
    let n = rows.len();
    user.db.log(
        user.username(),
        "导出",
        "导出科目余额表",
        &format!("{from} 至 {to} {n} 行"),
    )?;
    build_xlsx(&[sheet], &format!("科目余额表_{to}.xlsx"), "科目余额表")
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
    let sheet = match kind {
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
            crate::sheets::report_sheet(&t, "元")
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
            crate::sheets::report_sheet(&t, "元")
        }
        StatementKind::CashFlow => {
            let cf = findb::reports::cash_flow_statement(db, from, to, Some(&user.user))?;
            crate::sheets::cash_flow_sheet(&cf, &subtitle)
        }
        StatementKind::Equity => {
            let st = findb::reports::equity_statement(db, from, to, Some(&user.user))?;
            crate::sheets::equity_sheet(&st, &subtitle)
        }
    };
    user.db.log(
        user.username(),
        "导出",
        "导出报表",
        &format!("{} {subtitle}", kind.filename()),
    )?;
    build_xlsx(
        &[sheet],
        &format!("{}_{}.xlsx", kind.filename(), period_to_str(to)),
        kind.filename(),
    )
}

async fn export_balance_sheet_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    export_statement_xlsx(&state, &user, &q, StatementKind::BalanceSheet)
}

async fn export_income_statement_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    export_statement_xlsx(&state, &user, &q, StatementKind::Income)
}

async fn export_cash_flow_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    export_statement_xlsx(&state, &user, &q, StatementKind::CashFlow)
}

async fn export_equity_xlsx(
    State(state): State<Arc<WebState>>,
    user: CurrentUser,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    user.require(Perm::Export)?;
    export_statement_xlsx(&state, &user, &q, StatementKind::Equity)
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

/// 期初导入后的对账视图。
///
/// 为什么不并进「预览」接口：预览回答「这份表能不能导」，对账回答「导完之后账上是不是
/// 真的变成了表里那个样子」。前者看的是文件，后者看的是库 —— 两次来的数据都可能对，
/// 但中间任何一步出问题，只有后者能发现。
async fn opening_sheet_reconcile(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Json<ReconcileDto>, AppError> {
    user.require(Perm::Opening)?;
    let r = findb::opening_check::reconcile_begin(&user.db)?;
    Ok(Json(ReconcileDto {
        ok: !r.empty && r.issues.is_empty(),
        one_liner: findb::opening_check::one_liner(&r),
        empty: r.empty,
        sheets: r
            .sheets
            .into_iter()
            .map(|s| ReconcileSheetDto {
                label: s.label,
                rows: s.rows,
                total: s.total.fmt_plain(),
                hint: s.hint,
            })
            .collect(),
        debit_total: r.debit_total.fmt_plain(),
        credit_total: r.credit_total.fmt_plain(),
        balanced: r.balanced,
        asset_total: r.asset_total.fmt_plain(),
        liab_total: r.liab_total.fmt_plain(),
        equity_total: r.equity_total.fmt_plain(),
        checks: r
            .checks
            .into_iter()
            .map(|c| ReconcileCheckDto {
                name: c.name,
                ok: c.ok,
                detail: c.detail,
            })
            .collect(),
        issues: r.issues,
    }))
}

async fn opening_sheet_template(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    user.require(Perm::Opening)?;
    let chart = findb::accounts::chart(&user.db)?;
    let accounts: Vec<&fincore::account::Account> = chart
        .all()
        .into_iter()
        .filter(|a| chart.is_leaf(&a.code))
        .collect();
    let customers = findb::auxs::list(
        &user.db,
        &AuxQuery {
            kind: Some(fincore::AuxKind::Customer),
            include_disabled: false,
            ..Default::default()
        },
    )?;
    let suppliers = findb::auxs::list(
        &user.db,
        &AuxQuery {
            kind: Some(fincore::AuxKind::Supplier),
            include_disabled: false,
            ..Default::default()
        },
    )?;
    let sheets = crate::sheets::opening_collection_template(&accounts, &customers, &suppliers);
    build_xlsx(&sheets, "期初收集表.xlsx", "期初收集表")
}

/// 只体检不落库 —— 与正式导入走同一条代码路径，只是不写。
async fn preview_opening_sheet(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Opening)?;
    let data = sheet_upload(multipart).await?;
    let r = findb::opening_book::preview_collection_sheet(&user.db, &data)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "dry_run": true,
        "ar": r.ar.ok,
        "ap": r.ap.ok,
        "begin": r.begin_ok,
        "arap_ok": r.arap_ok,
        "debit_total": r.debit_total.fmt_plain(),
        "credit_total": r.credit_total.fmt_plain(),
        "diff": (r.debit_total - r.credit_total).fmt_plain(),
        "auto_profit": r.auto_profit.fmt_plain(),
        "warnings": r.ar.warnings.iter().chain(r.ap.warnings.iter())
            .chain(r.balances.warnings.iter()).chain(r.equity.warnings.iter())
            .cloned().collect::<Vec<_>>(),
    })))
}

async fn import_opening_sheet(
    State(_state): State<Arc<WebState>>,
    user: CurrentUser,
    multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    user.require(Perm::Opening)?;
    let data = sheet_upload(multipart).await?;
    let r = findb::opening_book::import_collection_sheet(&user.db, &data, &user.username())?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "ar": r.ar.ok,
        "ap": r.ap.ok,
        "begin": r.begin_ok,
        "arap_ok": r.arap_ok,
        "debit_total": r.debit_total.fmt_plain(),
        "credit_total": r.credit_total.fmt_plain(),
        "auto_profit": r.auto_profit.fmt_plain(),
        "warnings": r.ar.warnings.iter().chain(r.ap.warnings.iter())
            .chain(r.balances.warnings.iter()).chain(r.equity.warnings.iter())
            .cloned().collect::<Vec<_>>(),
    })))
}

/// 导入账套：上传 .fbk（或含 .fbk 的备份 zip）→ 校验 → 落库到 books_dir → 注册
async fn import_book(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    mut multipart: axum::extract::Multipart,
) -> Result<Json<serde_json::Value>, AppError> {
    // 与 `create_book` 同一套门禁：仅管理员、每账号有账套数上限
    if !user.is_admin {
        return Err(AppError::forbidden("仅管理员可导入账套"));
    }
    let owner = user.username.clone();
    let owned = state.realm.count_books_of(&owner)?;
    if owned >= MAX_BOOKS_PER_USER {
        return Err(AppError::bad_request(format!(
            "每个账号最多创建 {MAX_BOOKS_PER_USER} 个账套（当前已有 {owned} 个），请先删除不用的账套"
        )));
    }

    let mut filename = String::new();
    let mut want_name = String::new();
    let mut data: Option<Vec<u8>> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::bad_request(format!("上传解析失败：{e}")))?
    {
        match field.name() {
            Some("file") => {
                filename = field.file_name().unwrap_or("book.fbk").to_string();
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| AppError::bad_request(format!("读取上传内容失败：{e}")))?;
                data = Some(bytes.to_vec());
            }
            Some("name") => {
                want_name = field.text().await.unwrap_or_default();
            }
            _ => {}
        }
    }
    let data = data.ok_or_else(|| AppError::bad_request("缺少文件字段 file"))?;
    if data.is_empty() {
        return Err(AppError::bad_request("上传文件为空"));
    }
    if data.len() > MAX_IMPORT_BOOK_BYTES {
        return Err(AppError::bad_request(format!(
            "文件超过 {} MB 上限",
            MAX_IMPORT_BOOK_BYTES / 1024 / 1024
        )));
    }
    let (book_bytes, attach) = unpack_book_upload(&data, &filename)?;

    // 先写临时文件再校验：校验要用 SQLite 打开，而校验不过时不能留下
    // 一个已注册、已出现在账套列表里的坏账套。
    let key = make_book_key(&want_name, &owner, &state.realm)?;
    let final_path = state.books_dir.join(format!("{key}.fbk"));
    if final_path.exists() {
        return Err(AppError::bad_request("账套文件已存在"));
    }
    let tmp_path = state.books_dir.join(format!(".import_{key}.tmp"));
    std::fs::write(&tmp_path, &book_bytes)
        .map_err(|e| AppError::Internal(format!("写入临时文件失败：{e}")))?;
    if let Err(e) = validate_book_file(&tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    // `Db::open` 会把老版本账套迁到当前 schema；放在 rename 之前，
    // 迁移失败就还没动过正式文件名。
    let company = match Db::open(&tmp_path) {
        Ok(db) => db.options().company,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(AppError::bad_request(format!("打开账套失败：{e}")));
        }
    };
    // `Db::open` 会在这个临时文件旁边留一对 `-wal` / `-shm`。连接关掉时
    // SQLite 一般会自己收走，但**收不走也不该留在账套目录里**：`books_dir`
    // 里躺着一堆 `.import_xxx.tmp-wal` 半年后没人敢删。
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", tmp_path.display()));
    }
    std::fs::rename(&tmp_path, &final_path)
        .map_err(|e| AppError::Internal(format!("落盘失败：{e}")))?;

    // 账套自己的 user 表用的是它自己那套口令。平台管理员要用平台口令进来，
    // 所以按 `create_book` 同样的做法，把平台身份同步成账套内管理员。
    //
    // 「账套里已有同名用户就一个字段都不动」曾经是这里的做法，它会让**从桌面版
    // 搬来的账套根本进不去**：桌面版建的 admin 那行 `must_change_pwd = 1`
    // （首次登录必须改密，而在桌面端从没改过），Web 端登录走的是平台口令，
    // 平台那边 `must_change_pwd = 0`，两边一比就卡在账套层的闸门上 ——
    // 每个账套内接口都 401「你的口令已过期或需首次设置」，前端表现是
    // 点了账套又弹回选择页，看起来就是「导入后进不了工作页面」。
    // 所以这里必须把平台身份**对齐**进去（与 `post_change_password` 里
    // `sync_password_to_books` 的口径一致：平台是唯一真身，账套内是同名的影子）。
    {
        let db = Db::open(&final_path)?;
        let realm_user = state.realm.get_user(&owner).ok().flatten();
        match users::get(&db, &owner)? {
            None => {
                let mut u = User::new(&owner, &user.display_name, Role::Admin);
                if let Some(ru) = &realm_user {
                    u.password_hash = ru.password_hash.clone();
                }
                users::insert(&db, &u)?;
            }
            Some(mut u) => {
                // **口令哈希故意不动**：Web 端从不校验账套内口令（登录只认平台
                // 口令），改它唯一的效果是让桌面端原本能用的口令失效 —— 那是把
                // 一个「进不去」换成了另一个「进不去」。
                // 要动的是那个闸门标记：它才是 Web 端真正会读的字段。
                let mut touched: Vec<&str> = Vec::new();
                if let Some(ru) = &realm_user {
                    if u.must_change_pwd != ru.must_change_pwd {
                        u.must_change_pwd = ru.must_change_pwd;
                        touched.push("改密标记");
                    }
                }
                // 停用状态也一起放开：这是刚导入的、属于当前账号的账套，
                // 账套里那行停用只可能来自桌面端的旧配置，留着就是把人锁在门外。
                if u.disabled {
                    u.disabled = false;
                    touched.push("停用");
                }
                if !touched.is_empty() {
                    let what = touched.join("、");
                    users::update(&db, &u)?;
                    db.log(
                        &owner,
                        "账套",
                        "导入账套",
                        &format!("账套内同名账号已与平台身份对齐（{what}）"),
                    )?;
                }
            }
        }
        db.log(
            &owner,
            "账套",
            "导入账套",
            &format!("从 {filename} 导入，编码 {key}，公司 {company}"),
        )?;
    }

    // 附件：内容寻址目录是全账套共享的，写入前先确认文件真的在（同名即同内容）
    if !attach.is_empty() {
        let dir = state.books_dir.join(findb::attach::DIR_NAME);
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Internal(format!("创建附件目录失败：{e}")))?;
        for (rel, bytes) in &attach {
            let p = dir.join(rel);
            if !p.exists() {
                std::fs::write(&p, bytes)
                    .map_err(|e| AppError::Internal(format!("写入附件 {rel} 失败：{e}")))?;
            }
        }
    }

    state.books.register(&final_path, 16);
    state
        .realm
        .register_book(&key, &final_path.to_string_lossy(), &owner, &company)?;

    Ok(Json(json!({
        "key": key,
        "company": company,
        "attachments": attach.len(),
    })))
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
