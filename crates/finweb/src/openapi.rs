//! 开放 API（`/api/v1/*`）—— 让 WMS / MES / 电商 / 银行 / 税务等外部系统接进来
//!
//! ## 为什么不复用浏览器那套端点
//!
//! 现有 289 个端点全部是 **session cookie + CSRF 双重门禁**的私有契约，绑死浏览器：
//! 第三方系统既拿不到 cookie，也过不了 CSRF 的 `Sec-Fetch-Site` / `Origin` 检查。
//! 所以**新开一套**，而不是给旧端点加个 if 分支：
//!
//! - 旧端点的语义（谁能看什么）已经被 500+ 个测试锁住，加分支风险远大于新写一层
//! - 两套鉴权物理隔离，将来收紧/下线 API 不影响 Web 端
//!
//! ## v1 的边界（刻意做窄）
//!
//! 只暴露 **只读 + 少量幂等写入**。理由：开放接口一旦对外，接进来的系统数量不可控，
//! 暴露写操作就是把「谁能改账」的决定权交给了接口设计者。第一版先把「读得到、
//! 接得上」解决，写操作按需再加，并且每个写操作都要求幂等键。
//!
//! ## 鉴权
//!
//! `Authorization: Bearer fbk_xxx`。密钥在账号库存 **sha2 哈希**，明文只在签发时返回一次。
//! 权限用 scope 列表收窄（`report` / `fin_report` / `voucher_new` …），
//! 空 scope = **只读**。密钥绑账套；平台级密钥（`book_key` 为空）只发给平台管理员。
//!
//! CSRF 不适用于此：Bearer token 不会由浏览器自动携带，跨站表单也拿不到它。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde_json::json;

use fincore::user::{Role, User};

use crate::realm::ApiKeyAuth;
use crate::state::{AppError, RealmUser, WebState};

/// 通过开放 API 鉴权的调用方
pub struct ApiCaller {
    pub auth: ApiKeyAuth,
    /// 目标账套（平台级密钥按请求头 `X-Book-Key` 指定）
    pub book_key: String,
}

impl ApiCaller {
    pub fn db(&self, state: &WebState) -> Result<findb::Db, AppError> {
        state
            .db_for(&self.book_key)
            .map_err(|_| AppError::not_found(format!("账套不存在或无法打开：{}", self.book_key)))
    }

    /// 要求某权限。空 scope 的密钥只能过 read 类端点。
    pub fn require(&self, perm: &str) -> Result<(), AppError> {
        if self.auth.allows(perm) {
            return Ok(());
        }
        Err(AppError::forbidden(format!(
            "该密钥没有「{perm}」权限（当前 scope：{}）",
            if self.auth.scopes.trim().is_empty() {
                "只读"
            } else {
                &self.auth.scopes
            }
        )))
    }

    /// 供业务代码做数据范围判断的只读身份。
    ///
    /// 刻意**不**套用真实岗位角色：API 密钥的权限边界由 scope 列表决定，
    /// 借用某个岗位角色会让人误以为「换个岗位就放开了」，反而模糊了真实边界。
    pub fn synthetic_user(&self) -> User {
        let mut u = User::new(
            &format!("api:{}", self.auth.prefix),
            "开放接口",
            Role::Viewer,
        );
        // Viewer 本身只读；写操作靠上面的 require(scope) 单独把关，不靠角色
        u.data_scope.own_voucher_only = false;
        u
    }
}

fn bearer(headers: &axum::http::HeaderMap) -> Option<String> {
    let v = headers.get(axum::http::header::AUTHORIZATION)?;
    let s = v.to_str().ok()?.trim();
    // 只接受 Bearer，不接受 Basic / 裸 token —— 少一种格式就少一类误用
    let rest = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))?
        .trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

impl ApiCaller {
    /// 从请求头解析调用方身份。
    ///
    /// 刻意**不用** `FromRequestParts` 自定义提取器：自定义提取器要匹配
    /// `async fn from_request_parts(&mut Parts, &S)` 的精确签名，一旦签名细节对不上
    /// 就是 E0195「lifetimes do not match」这种难懂的编译错。handler 本来就要拿
    /// `State<Arc<WebState>>`，多一个 HeaderMap 提取器没有任何额外成本。
    pub fn from_headers(
        state: &WebState,
        headers: &axum::http::HeaderMap,
    ) -> Result<Self, AppError> {
        let secret = bearer(headers)
            .ok_or_else(|| AppError::unauthorized("缺少 Authorization: Bearer <密钥>"))?;
        let auth = state
            .realm
            .api_key_verify(&secret)
            .map_err(AppError::from)?
            .ok_or_else(|| AppError::unauthorized("密钥无效、已停用或已过期"))?;
        // 平台级密钥必须显式指定目标账套：不给「默认账套」这种隐式兜底，
        // 否则调用方会以为自己写的是 A 套、实际落在 B 套。
        let book_key = if auth.book_key.trim().is_empty() {
            headers
                .get("x-book-key")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::bad_request("平台级密钥需用 X-Book-Key 指定目标账套"))?
        } else {
            auth.book_key.clone()
        };
        // 账套必须真实存在——否则错误会延后到第一次业务查询才暴露，难排查
        if state.realm.get_book(&book_key)?.is_none() {
            return Err(AppError::not_found("账套不存在或已被删除"));
        }
        Ok(ApiCaller { auth, book_key })
    }

    /// 审计用的调用方标识（只含密钥前缀，不含密钥本身）
    pub fn audit_who(&self) -> String {
        format!("api:{}", self.auth.prefix)
    }
}

// ═══════════════════════════════════════════════════════════════
// 密钥管理（平台管理员，走浏览器那套 session 鉴权）
// ═══════════════════════════════════════════════════════════════

/// 签发请求。字段用 `#[serde(default)]` 是为了将来加字段不破坏旧前端，
/// 但**必填项不做默认值兜底**——密钥名/账套给错会签出一把没人知道属于谁的凭据。
#[derive(serde::Deserialize)]
pub struct ApiKeyIn {
    #[serde(default)]
    name: String,
    #[serde(default)]
    book_key: String,
    #[serde(default)]
    scopes: String,
    #[serde(default)]
    expires_at: String,
}

/// 允许签发的 scope 白名单。
///
/// 界面只给「只读」和「只读+财务报表」两个选项，后端也只认这两个：
/// 写操作（voucher_new 之类）**故意没有实现**——开放接口一旦有了写入口，
/// 「谁能改账」就变成接口设计说了算，而接进来的系统数量不可控。
/// 白名单在写入口真正落地之前，就是防止有人手滑签出一个「什么都能干」的密钥。
const ALLOWED_SCOPES: &[&str] = &["fin_report"];

fn normalize_scopes(raw: &str) -> Result<String, AppError> {
    let mut out: Vec<&str> = Vec::new();
    for s in raw.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()) {
        if !ALLOWED_SCOPES.contains(&s) {
            return Err(AppError::bad_request(format!(
                "未知的权限码「{s}」；v1 只支持：{}（空 = 只读基础档案）",
                ALLOWED_SCOPES.join(" / ")
            )));
        }
        if !out.contains(&s) {
            out.push(s);
        }
    }
    Ok(out.join(","))
}

pub async fn list_api_keys(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("仅平台管理员可管理开放 API 密钥"));
    }
    let keys = state.realm.api_key_list()?;
    Ok(Json(json!({ "keys": keys })))
}

pub async fn create_api_key(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Json(inp): Json<ApiKeyIn>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("仅平台管理员可签发开放 API 密钥"));
    }
    let name = inp.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request(
            "请填密钥名称（便于日后辨认是哪套系统在用）",
        ));
    }
    if name.len() > 64 {
        return Err(AppError::bad_request("密钥名称过长（上限 64 字符）"));
    }
    let book_key = inp.book_key.trim();
    if !book_key.is_empty() && state.realm.get_book(book_key)?.is_none() {
        return Err(AppError::not_found(format!("账套不存在：{book_key}")));
    }
    let scopes = normalize_scopes(&inp.scopes)?;
    // 到期时间接受 "YYYY-MM-DD HH:MM:SS" 与 ISO8601 两种写法；解析不了就 400，
    // 不静默当成「永不过期」——那等于签出一把忘记会失效的密钥。
    let expires = inp.expires_at.trim();
    if !expires.is_empty()
        && chrono::NaiveDateTime::parse_from_str(expires, "%Y-%m-%d %H:%M:%S").is_err()
        && chrono::NaiveDateTime::parse_from_str(expires, "%Y-%m-%dT%H:%M:%S").is_err()
    {
        return Err(AppError::bad_request(format!(
            "到期时间格式应为 YYYY-MM-DD HH:MM:SS，收到：{expires}"
        )));
    }
    let (id, secret) =
        state
            .realm
            .api_key_create(name, book_key, &scopes, expires, &user.username)?;
    Ok(Json(json!({
        "id": id,
        // 明文只在这一次响应里出现。之后无论列表、详情还是数据库都拿不到。
        "secret": secret,
        "book_key": book_key,
        "scopes": scopes,
        "expires_at": expires,
    })))
}

#[derive(serde::Deserialize)]
pub struct ApiKeyToggle {
    disabled: bool,
}

pub async fn toggle_api_key(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(id): Path<i64>,
    Json(inp): Json<ApiKeyToggle>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("仅平台管理员可管理开放 API 密钥"));
    }
    state.realm.api_key_set_disabled(id, inp.disabled)?;
    Ok(Json(
        json!({ "ok": true, "id": id, "disabled": inp.disabled }),
    ))
}

pub async fn delete_api_key(
    State(state): State<Arc<WebState>>,
    user: RealmUser,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !user.is_admin {
        return Err(AppError::forbidden("仅平台管理员可管理开放 API 密钥"));
    }
    state.realm.api_key_delete(id)?;
    Ok(Json(json!({ "ok": true, "id": id })))
}

// ═══════════════════════════════════════════════════════════════
// v1 端点
// ═══════════════════════════════════════════════════════════════
//
// 刻意做窄：只读为主。开放接口一旦对外就收不回来，第一版先把「外部系统读得到账」
// 这件事做扎实，写操作等真实对接场景出现后再按需加，且每个写操作都要求幂等键。
//
// 响应体一律包一层：直接返回裸数组的话，调用方无法做分页游标演进；
// 包 {items, page} 后将来加 next_cursor 不算破坏性变更。

/// 分页参数。上限 500 条：账套数据量是「一家中小企业的账」，一次拉太多
/// 只会把调用方的内存和 SQLite 的单次查询时间一起拖垮。真正要全量导出的
/// 场景应该走文件导出端点，不该靠调大 limit。
fn page_of(q: &HashMap<String, String>) -> Result<(i64, i64), AppError> {
    // 给了却解析不了 → 400，不要静默吞成默认值。
    // 开放 API 的调用方是程序：把 `page=abc` 当成第 1 页返回，对方会拿到
    // 「看起来正常但其实是首页」的数据，重复拉取或漏拉都很难发现。
    // 静默容错在浏览器端是优点，在 API 端是 bug。
    let num = |k: &str, def: i64| -> Result<i64, AppError> {
        match q.get(k).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            None => Ok(def),
            Some(t) => t
                .parse::<i64>()
                .map_err(|_| AppError::bad_request(format!("参数 {k} 必须是整数，收到：{t}"))),
        }
    };
    Ok((num("page", 1)?.max(1), num("page_size", 50)?.clamp(1, 500)))
}

fn parse_period_opt(s: Option<&String>) -> Result<Option<fincore::Period>, AppError> {
    match s.map(|x| x.trim()).filter(|x| !x.is_empty()) {
        None => Ok(None),
        Some(t) => fincore::Period::parse(t)
            .map(Some)
            .map_err(|e| AppError::bad_request(e.to_string())),
    }
}

async fn v1_health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "api": "v1", "version": env!("CARGO_PKG_VERSION") }))
}

/// 免鉴权：调用方拿密钥之前得先知道服务活着、知道有哪些端点
async fn v1_spec() -> Json<serde_json::Value> {
    Json(json!({
        "openapi": "3.0.3",
        "info": {
            "title": "FinBook 开放 API",
            "version": "1.0.0",
            "description":
                "只读为主的开放接口。鉴权：Authorization: Bearer fbk_xxx。\
                 平台级密钥需带 X-Book-Key 指定账套。所有响应为 JSON。\
                 v1 边界：不含写操作。"
        },
        "components": {
            "securitySchemes": {
                "bearer": { "type": "http", "scheme": "bearer", "description": "在「全部账套」页签签发的 API 密钥" }
            }
        },
        "security": [{ "bearer": [] }],
        "paths": {
            "/api/v1/health": { "get": { "summary": "存活探测（免鉴权）", "security": [] } },
            "/api/v1/me": { "get": { "summary": "当前密钥身份、绑定账套与权限" } },
            "/api/v1/accounts": { "get": { "summary": "会计科目表" } },
            "/api/v1/items": { "get": { "summary": "存货档案" } },
            "/api/v1/vouchers": { "get": { "summary": "记账凭证列表（分页）", "parameters": [
                { "name": "page", "in": "query", "schema": { "type": "integer" } },
                { "name": "page_size", "in": "query", "schema": { "type": "integer" } },
                { "name": "from", "in": "query", "description": "期间起，YYYYMM 或 YYYY-MM" },
                { "name": "to", "in": "query", "description": "期间止" },
                { "name": "status", "in": "query", "description": "draft/audited/posted" },
                { "name": "keyword", "in": "query", "description": "摘要/科目/凭证号关键字" }
            ] } },
            "/api/v1/vouchers/{id}": { "get": { "summary": "单张凭证（含分录）" } },
            "/api/v1/trial-balance": { "get": { "summary": "试算平衡（需 fin_report scope）", "parameters": [
                { "name": "from", "in": "query" }, { "name": "to", "in": "query" }
            ] } },
            "/api/v1/ledger": { "get": { "summary": "明细账（需 fin_report scope）", "parameters": [
                { "name": "code", "in": "query", "required": true, "description": "科目编码" },
                { "name": "from", "in": "query" }, { "name": "to", "in": "query" }
            ] } },
            "/api/v1/statements": { "get": { "summary": "往来对账单列表" } }
        }
    }))
}

/// 密钥自查：接进来之后第一件事就是确认「我以为的权限」和「实际的权限」一致
async fn v1_me(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let c = ApiCaller::from_headers(&state, &headers)?;
    let book = state.realm.get_book(&c.book_key)?;
    Ok(Json(json!({
        "key_prefix": c.auth.prefix,
        "book_key": c.book_key,
        "company": book.as_ref().map(|b| b.company.clone()).unwrap_or_default(),
        "scopes": if c.auth.is_readonly() { vec!["*readonly*"] } else {
            c.auth.scopes.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>()
        },
        "readonly": c.auth.is_readonly(),
    })))
}

async fn v1_accounts(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let c = ApiCaller::from_headers(&state, &headers)?;
    let db = c.db(&state)?;
    let rows = findb::accounts::list(&db)?;
    Ok(Json(json!({ "items": rows, "count": rows.len() })))
}

async fn v1_items(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let c = ApiCaller::from_headers(&state, &headers)?;
    let db = c.db(&state)?;
    let rows = findb::inventory2::item_master(&db)?;
    Ok(Json(json!({ "items": rows, "count": rows.len() })))
}

async fn v1_statements(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let c = ApiCaller::from_headers(&state, &headers)?;
    let db = c.db(&state)?;
    let kind = q.get("kind").map(|s| s.trim()).filter(|s| !s.is_empty());
    let rows = findb::statement::list(&db, kind, None)?;
    Ok(Json(json!({ "items": rows, "count": rows.len() })))
}

async fn v1_vouchers(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let c = ApiCaller::from_headers(&state, &headers)?;
    let db = c.db(&state)?;
    let (page, size) = page_of(&q)?;
    let offset = (page - 1) * size;

    let mut vq = findb::vouchers::VoucherQuery {
        asc: q
            .get("asc")
            .map(|s| s != "0" && s != "false")
            .unwrap_or(false),
        // 走 SQL 侧的 LIMIT/OFFSET + COUNT(*)，不在内存里裁。
        // 原来这里是「取到 offset+size+1 条再 skip(offset)」（注释说引擎层没有
        // offset，改签名要动几十个调用点）。现在引擎层新增了 `list_page`，
        // 老的 `list` 签名一个字没动，所以不用改任何调用点就能换成真分页：
        //   · 第 50 页不再把 2501 张凭证读进内存
        //   · 并且首次能给出精确 `total`（原来只有 has_more，调用方无法渲染页码）
        limit: None,
        ..Default::default()
    };
    vq.from = parse_period_opt(q.get("from"))?;
    vq.to = parse_period_opt(q.get("to"))?;
    vq.keyword = q
        .get("keyword")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    vq.account_code = q
        .get("account_code")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(s) = q.get("status").map(|s| s.trim()).filter(|s| !s.is_empty()) {
        vq.status = Some(match s {
            "draft" | "unposted" => fincore::VoucherStatus::Draft,
            "audited" => fincore::VoucherStatus::Audited,
            "posted" => fincore::VoucherStatus::Posted,
            other => {
                return Err(AppError::bad_request(format!(
                    "status 只能是 draft/audited/posted，收到：{other}"
                )))
            }
        });
    }
    // API 密钥的数据范围：synthetic_user 明确放开 own_voucher_only、不限科目，
    // 这里仍走 with_data_scope 让 vq 带上 scope 字段（值都是空 = 不限），
    // 与 Web 端共用同一条 WHERE，不会出现两套条件算出不同 total 的情况。
    let caller_user = c.synthetic_user();
    vq = vq.with_data_scope(&caller_user);

    let (items, total) = findb::vouchers::list_page(&db, &vq, offset, size)?;
    Ok(Json(json!({
        "items": items,
        "page": page,
        "page_size": size,
        "total": total,
        "has_more": offset + size < total,
    })))
}

async fn v1_voucher_one(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let c = ApiCaller::from_headers(&state, &headers)?;
    let db = c.db(&state)?;
    let v = findb::vouchers::get(&db, id)?
        .ok_or_else(|| AppError::not_found(format!("凭证不存在：{id}")))?;
    Ok(Json(json!(v)))
}

async fn v1_trial_balance(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let c = ApiCaller::from_headers(&state, &headers)?;
    // 报表是「只有会计/财务岗才能看」的东西，密钥也得显式拿到 fin_report
    c.require("fin_report")?;
    let db = c.db(&state)?;
    let start = db.options().start_period;
    let from = parse_period_opt(q.get("from"))?.unwrap_or(start);
    let to = parse_period_opt(q.get("to"))?.unwrap_or(findb::periods::current_period(&db)?);
    let bq = findb::balances::BalanceQuery::range(from, to);
    let snap = findb::balances::BalanceSnapshot::load(&db, &bq)?;
    let chart = findb::accounts::chart(&db)?;
    let rows = snap.account_table(&chart, &bq);
    let t = snap.trial_balance(&chart);
    Ok(Json(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "rows": rows,
        "totals": {
            "begin_debit": t.begin_debit.fmt_money(),
            "begin_credit": t.begin_credit.fmt_money(),
            "debit": t.period_debit.fmt_money(),
            "credit": t.period_credit.fmt_money(),
            "end_debit": t.end_debit.fmt_money(),
            "end_credit": t.end_credit.fmt_money(),
        },
    })))
}

async fn v1_ledger(
    State(state): State<Arc<WebState>>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let c = ApiCaller::from_headers(&state, &headers)?;
    c.require("fin_report")?;
    let db = c.db(&state)?;
    let code = q
        .get("code")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("缺少参数 code（科目编码）"))?;
    let start = db.options().start_period;
    let from = parse_period_opt(q.get("from"))?.unwrap_or(start);
    let to = parse_period_opt(q.get("to"))?.unwrap_or(findb::periods::current_period(&db)?);
    let chart = findb::accounts::chart(&db)?;
    let lq = findb::balances::LedgerQuery {
        code,
        include_children: q.get("include_children").map(|s| s != "0").unwrap_or(true),
        from,
        to,
        ..Default::default()
    };
    let rows = findb::balances::ledger(&db, &chart, &lq)?;
    Ok(Json(json!({
        "from": from.to_string(),
        "to": to.to_string(),
        "rows": rows,
        "count": rows.len(),
    })))
}

/// 挂在主 Router 上的 v1 路由。
///
/// 关键：`.route("/api/v1/...")` 加在**主 router** 而不是嵌套 router 上，
/// 这样 `/api/v1/health` 不会被 `api_auth_gate` 的 `path.starts_with("/api/")`
/// 拦掉（它对所有 /api/ 路径强制要 session）。gate 只豁免了 4 个硬编码路径，
/// 要加豁免就得改那个 middleware 的判断条件 —— 改已验证的门禁比在门外单开
/// 一个白名单更可控：v1 的鉴权完全由 ApiCaller 自己承担，不依赖 gate。
pub fn routes() -> axum::Router<Arc<WebState>> {
    use axum::routing::get;
    axum::Router::new()
        .route("/api/v1/health", get(v1_health))
        .route("/api/v1/openapi.json", get(v1_spec))
        .route("/api/v1/me", get(v1_me))
        .route("/api/v1/accounts", get(v1_accounts))
        .route("/api/v1/items", get(v1_items))
        .route("/api/v1/statements", get(v1_statements))
        .route("/api/v1/vouchers", get(v1_vouchers))
        // 参数段用 `:id` 而不是 `{id}`：本仓库锁的是 axum 0.7（matchit 0.7），
        // 那个版本只认 `:id`；`{id}` 会被当成**字面量**路径，于是
        // GET /api/v1/vouchers/1 匹配不上任何路由，掉进 spa_fallback，
        // 未登录时又返回 401「未登录或会话已失效」——一个把 404 说成 401 的
        // 极难排查的假象。写 `{id}` 时它没有编译错也没有测试红，只是安静地不工作。
        .route("/api/v1/vouchers/:id", get(v1_voucher_one))
        .route("/api/v1/trial-balance", get(v1_trial_balance))
        .route("/api/v1/ledger", get(v1_ledger))
}
