//! Web 服务共享状态：账号库、账套注册表、会话管理、鉴权提取器与错误类型。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;
use fincore::user::{PasswordPolicy, Perm, Role, User};
use findb::{users, Db, DbError};
use rand::Rng;

use crate::realm::{ensure_book_admin, RealmDb, RealmUser as RealmAccount};

/// Mutex 毒化守卫：任一持有锁的线程 panic 后，后续请求仍能取到内部数据
/// （ poisoned 锁直接 unwrap 会把一次 panic 放大成全服务 500）。
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 全局共享状态（以 Arc 包裹，可被多请求并发引用）
pub struct WebState {
    /// 账套注册表（多账套支持）：key → 文件路径
    pub books: BookRegistry,
    pub sessions: SessionStore,
    /// 登录限流（账号维度）
    pub login_limiter: LoginLimiter,
    /// 登录限流（来源 IP 维度，防同一出口跨账号扫号）
    pub login_ip_limiter: LoginLimiter,
    /// 账号库（全局账号 + 账套目录）
    pub realm: RealmDb,
    /// 用户自建账套的存放目录
    pub books_dir: PathBuf,
    /// 公司名缓存（建账后可由 refresh_company 更新；多账套下更推荐用 CurrentUser.company）
    pub company: std::sync::RwLock<String>,
    pub version: String,
    /// 账套默认（启用）期间，ymm 形式，作为会话期间的初值
    pub default_period: i32,
    /// 静态资源目录（前端 SPA 所在位置）
    pub static_dir: PathBuf,
    /// 前端资源版本号（由静态文件 mtime 计算，变化时浏览器缓存自动失效）
    pub assets_ver: String,
}

impl WebState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        books: BookRegistry,
        sessions: SessionStore,
        realm: RealmDb,
        books_dir: PathBuf,
        version: String,
        default_period: i32,
        static_dir: PathBuf,
        assets_ver: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            books,
            sessions,
            login_limiter: LoginLimiter::new(),
            login_ip_limiter: LoginLimiter::with_max(LOGIN_IP_MAX_FAILURES),
            realm,
            books_dir,
            company: std::sync::RwLock::new(String::new()),
            version,
            default_period,
            static_dir,
            assets_ver,
        })
    }

    /// 平台口令策略（读账号库；读取失败回退默认策略，不因策略库异常阻断登录）
    pub fn policy(&self) -> PasswordPolicy {
        self.realm.policy().unwrap_or_default()
    }

    /// 读取公司名
    pub fn company_name(&self) -> String {
        self.company.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// 借出指定账套的连接（owned，多账套）
    pub fn db_for(&self, key: &str) -> Result<Db, DbError> {
        self.books.open(key)
    }

    /// 借出默认（首个）账套的连接
    pub fn default_db(&self) -> Result<Db, DbError> {
        let key = self.books.first_key();
        self.books.open(&key)
    }

    /// 账套归属迁移（版本升级时执行，幂等）：普通账号名下的存量账套接管到
    /// 最早创建的管理员名下，并尽力把该管理员补进各套的套内管理员成员行
    /// （管理员进入他人套也可走临时身份，此步保证套内身份与工具链始终可用）。
    /// realm 归属接管必做；套内补行对缺失/损坏的账套文件跳过并打日志。
    pub fn migrate_book_owners_to_admin(&self) -> usize {
        let moved = match self.realm.reassign_books_to_admin() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[finweb] 账套归属迁移失败: {e}");
                return 0;
            }
        };
        if moved.is_empty() {
            return 0;
        }
        let admin = match self.realm.first_admin() {
            Ok(Some(a)) => a,
            _ => return moved.len(), // 归属已接管；管理员信息缺失时跳过补行
        };
        for (key, path) in &moved {
            if !std::path::PathBuf::from(path).exists() {
                continue;
            }
            let db = match self.books.open(key) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("[finweb] 迁移补套内管理员失败（{key} 打不开）: {e}");
                    continue;
                }
            };
            let ensured = (|| -> findb::DbResult<()> {
                match findb::users::get(&db, &admin.0)? {
                    None => {
                        let mut u = fincore::User::new(&admin.0, &admin.1, fincore::Role::Admin);
                        if let Ok(Some(ru)) = self.realm.get_user(&admin.0) {
                            u.password_hash = ru.password_hash;
                            u.must_change_pwd = ru.must_change_pwd;
                        }
                        findb::users::insert(&db, &u)?;
                    }
                    Some(mut u) if !u.is_admin() => {
                        u.role = fincore::Role::Admin;
                        u.roles.retain(|r| *r != fincore::Role::Admin);
                        findb::users::update(&db, &u)?;
                    }
                    Some(_) => {}
                }
                Ok(())
            })();
            if let Err(e) = ensured {
                eprintln!("[finweb] 迁移补套内管理员失败（{key}）: {e}");
            }
        }
        println!(
            "  账套归属迁移：接管 {} 个存量账套 → {}",
            moved.len(),
            admin.0
        );
        moved.len()
    }
}

// ---------------------------------------------------------------------------
// 账套注册表（多账套）
// ---------------------------------------------------------------------------

/// 账套注册表：key（文件名，不带扩展名）→ 文件路径
pub struct BookRegistry {
    inner: Mutex<Vec<(String, PathBuf)>>,
}

impl BookRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
        }
    }

    /// 注册一个账套；key 取文件名（不含扩展名）
    pub fn register(&self, path: &Path, _max: usize) {
        let key = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "default".to_string());
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.iter().any(|(k, _)| *k == key) {
            return;
        }
        g.push((key, path.to_path_buf()));
    }

    /// 注销账套（删除账套时调用）：避免后续请求把已删除文件重新打开成空库
    pub fn unregister(&self, key: &str) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).retain(|(k, _)| k != key);
    }

    /// 所有账套 key + 文件路径（供列表展示）
    pub fn list(&self) -> Vec<(String, PathBuf)> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.iter().map(|(k, p)| (k.clone(), p.clone())).collect()
    }

    pub fn first_key(&self) -> String {
        lock(&self.inner)
            .first()
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| "default".to_string())
    }

    /// 打开指定账套（owned 连接，无借用生命周期问题）
    pub fn open(&self, key: &str) -> Result<Db, DbError> {
        let path = lock(&self.inner)
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, p)| p.clone())
            .ok_or_else(|| DbError::Fin(fincore::FinError::msg(format!("账套不存在：{key}"))))?;
        Db::open(&path).map_err(Into::into)
    }
}

// ---------------------------------------------------------------------------
// 登录限流（账号维度滑动窗口）
// ---------------------------------------------------------------------------

/// 登录失败限流窗口（15 分钟）
const LOGIN_WINDOW: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// 窗口内允许的最大失败次数，超过则拒绝后续尝试直至窗口滑出
const LOGIN_MAX_FAILURES: usize = 10;
/// IP 维度窗口内允许的最大失败次数：同一出口（办公室 NAT / 反代）可能多人共用，
/// 阈值放宽，只拦"一个来源持续扫号"，不误伤正常打错口令。
pub const LOGIN_IP_MAX_FAILURES: usize = 50;
/// 攒够这么多条目才做一次全局裁剪，摊薄扫描成本
const LOGIN_SWEEP_MARK: usize = 1024;
/// 限流表的账号数上界，超过直接清空
const LOGIN_MAX_TRACKED: usize = 10_000;

/// 登录限流：按 key（账号 / 来源 IP）做滑动窗口计数。
///
/// 单机内存实现——本服务为单机部署，进程重启即清零；对 WireGuard 等私有组网场景
/// 主要作纵深防御（防授权设备被攻破后的账号爆破、防内部误操作），不追求跨实例一致性。
pub struct LoginLimiter {
    inner: Mutex<HashMap<String, Vec<Instant>>>,
    max_failures: usize,
}

impl LoginLimiter {
    pub fn new() -> Self {
        Self::with_max(LOGIN_MAX_FAILURES)
    }

    /// 指定窗口内最大失败次数的限流器（窗口固定 15 分钟）
    pub fn with_max(max_failures: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_failures,
        }
    }

    /// 检查该账号当前是否允许再尝试登录；返回 `Err(剩余等待秒数)` 表示已被限流。
    ///
    /// 未限流的账号一律不建条目：本函数在口令校验之前调用，用户名由客户端任意
    /// 提交，若在这里 `or_default()`，任何一次探测都会永久留下一个 key。
    pub fn check(&self, key: &str) -> Result<(), u64> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let mut stale = false;
        let verdict = match g.get_mut(key) {
            Some(v) => {
                v.retain(|t| now.duration_since(*t) < LOGIN_WINDOW);
                if v.is_empty() {
                    stale = true;
                    Ok(())
                } else if v.len() >= self.max_failures {
                    // 窗口滑出到最早一次失败时，允许再次尝试
                    let wait = LOGIN_WINDOW.saturating_sub(now.duration_since(v[0]));
                    Err(wait.as_secs().max(1))
                } else {
                    Ok(())
                }
            }
            None => Ok(()),
        };
        if stale {
            g.remove(key);
        }
        verdict
    }

    /// 记录一次失败尝试
    pub fn record_failure(&self, key: &str) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        // 只记录失败、从不裁剪的话，用随机用户名刷登录就能把表无限撑大。
        // 攒够一批再全局扫，避免每次失败都 O(n) 遍历。
        if g.len() > LOGIN_SWEEP_MARK {
            g.retain(|_, v| {
                v.retain(|t| now.duration_since(*t) < LOGIN_WINDOW);
                !v.is_empty()
            });
            // 大量账号同时被爆破时仍超限，直接清空换回内存上界：
            // 这是纵深防御，宁可偶尔放宽限流也不能耗尽内存拖垮整个服务。
            if g.len() > LOGIN_MAX_TRACKED {
                g.clear();
            }
        }
        g.entry(key.to_string()).or_default().push(now);
    }

    /// 登录成功后清零该账号的失败记录
    pub fn clear(&self, key: &str) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }
}

// ---------------------------------------------------------------------------
// 会话管理
// ---------------------------------------------------------------------------

pub struct SessionStore {
    inner: Mutex<HashMap<String, SessionInfo>>,
}

#[derive(Clone)]
pub struct SessionInfo {
    pub username: String,
    /// 管理员标志（决定能否看全部账套）
    pub is_admin: bool,
    /// 登录时的设备指纹（用于逐请求复核"一人一机"策略）
    pub device_id: String,
    pub last_active: i64,
    /// 会话创建时刻：用于绝对寿命上限，touch 不能把它往后推
    pub created_at: i64,
    /// 当前工作期间（ymm），0 表示未设定（用账套默认值）
    pub period_ymm: i32,
    /// 当前账套 key（多账套切换）
    pub book_key: String,
}

/// 会话最长保留时间（秒）：与登录 Cookie 的 Max-Age 一致
const SESSION_MAX_SECS: i64 = 60 * 60 * 24 * 7;

impl SessionStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn new_token() -> String {
        let mut rng = rand::thread_rng();
        (0..32)
            .map(|_| {
                let n: u8 = rng.gen_range(0..16);
                char::from_digit(n as u32, 16).unwrap()
            })
            .collect()
    }

    pub fn create(
        &self,
        username: &str,
        is_admin: bool,
        device_id: &str,
        period_ymm: i32,
        book_key: &str,
    ) -> String {
        let token = Self::new_token();
        let now = now_secs();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // 清理的是「超过绝对寿命」的会话，不是空闲超时的：活跃用户会不断 touch
        // last_active，若按它裁剪，一直用的会话就永远留在表里。
        g.retain(|_, i| now - i.created_at < SESSION_MAX_SECS);
        g.insert(
            token.clone(),
            SessionInfo {
                username: username.to_string(),
                is_admin,
                device_id: device_id.to_string(),
                last_active: now,
                created_at: now,
                period_ymm,
                book_key: book_key.to_string(),
            },
        );
        token
    }

    /// 取会话；空闲超时（分钟）大于 0 且已超时，或已超过绝对寿命，则视为失效并清除。
    ///
    /// 两个判据都要有：只有空闲判据的话，用户持续操作就会不断 touch，服务端会话
    /// 永不过期，被长期遗留的令牌一旦被窃取可无限续命。绝对上限与登录 Cookie 的
    /// Max-Age 同值（7 天），所以不会比浏览器侧更早把用户踢下线。
    pub fn get(&self, token: &str, idle_minutes: i64) -> Option<SessionInfo> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let info = g.get(token)?;
        let now = now_secs();
        let idle_out = idle_minutes > 0 && now - info.last_active > idle_minutes * 60;
        let expired = now - info.created_at > SESSION_MAX_SECS;
        if idle_out || expired {
            g.remove(token);
            return None;
        }
        Some(info.clone())
    }

    pub fn touch(&self, token: &str) {
        if let Some(i) = self.inner.lock().unwrap_or_else(|e| e.into_inner()).get_mut(token) {
            i.last_active = now_secs();
        }
    }

    pub fn period(&self, token: &str) -> Option<i32> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).get(token).map(|i| i.period_ymm)
    }

    pub fn set_period(&self, token: &str, ymm: i32) {
        if let Some(i) = self.inner.lock().unwrap_or_else(|e| e.into_inner()).get_mut(token) {
            i.period_ymm = ymm;
        }
    }

    /// 切换当前账套（登录后选账套时调用）
    pub fn set_book_key(&self, token: &str, key: &str) {
        if let Some(i) = self.inner.lock().unwrap_or_else(|e| e.into_inner()).get_mut(token) {
            i.book_key = key.to_string();
        }
    }

    pub fn remove(&self, token: &str) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).remove(token);
    }

    /// 账套被删除后，把仍停留在该账套的会话退回"未选账套"状态，
    /// 否则用户会卡在 404「账套不存在」而无法自行回到选择页。
    pub fn clear_book_key(&self, key: &str) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        for info in g.values_mut() {
            if info.book_key == key {
                info.book_key.clear();
            }
        }
    }

    /// 清掉某个用户的全部会话（重置设备绑定 / 删除 / 停用账号时调用），
    /// 否则旧设备上的会话还能继续用到自然过期，"一人一机"会被绕过。
    pub fn remove_by_username(&self, username: &str) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, i| i.username != username);
    }

    /// 吊销某个用户名下除当前会话外的全部会话（改密后调用：当前设备是本人，
    /// 其他设备上的旧会话必须立即失效，不必把本人也踢去重新登录）。
    pub fn remove_others(&self, username: &str, keep_token: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|t, i| i.username != username || t == keep_token);
    }
}

// ---------------------------------------------------------------------------
// 鉴权提取器
// ---------------------------------------------------------------------------

/// 取出会话令牌与会话信息（与 RealmUser/CurrentUser 提取器的前两步共用）。
///
/// M-9：无 cookie / 已过期时返回的 401 文案必须与提取器**逐字一致**——
/// 门禁 `handlers::api_auth_gate` 靠它让"真实接口"与"不存在的接口"不可区分。
pub(crate) fn session_of(
    headers: &axum::http::HeaderMap,
    state: &WebState,
) -> Result<(String, SessionInfo), AppError> {
    let jar = CookieJar::from_headers(headers);
    let token = jar
        .get("finbook_sid")
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::unauthorized("未登录或会话已失效"))?;
    let info = state
        .sessions
        .get(&token, state.policy().idle_minutes)
        .ok_or_else(|| AppError::unauthorized("会话已过期，请重新登录"))?;
    Ok((token, info))
}

/// 平台级登录用户（不绑定具体账套）：用于登录、账套列表、建账、平台用户管理等。
pub struct RealmUser {
    pub username: String,
    pub is_admin: bool,
    pub display_name: String,
    pub must_change_pwd: bool,
    pub token: String,
    pub device_id: String,
}

#[axum::async_trait]
impl FromRequestParts<Arc<WebState>> for RealmUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<WebState>,
    ) -> Result<Self, Self::Rejection> {
        let (token, info) = session_of(&parts.headers, state)?;
        let ru = state
            .realm
            .get_user(&info.username)?
            .ok_or_else(|| AppError::unauthorized("账号已不存在，请重新登录"))?;
        if ru.disabled {
            state.sessions.remove(&token);
            return Err(AppError::forbidden("账号已被停用，请联系管理员"));
        }
        // "一人一机"逐请求复核。CurrentUser 提取器里本来就有这段，但平台级接口
        // （建账套、选账套、改密）只走本提取器；缺了它，被管理员重置过设备的旧
        // 浏览器仍能拿着旧会话在账套之外建套、改密。
        if !ru.is_admin && !ru.device_id.is_empty() && ru.device_id != info.device_id {
            state.sessions.remove(&token);
            return Err(AppError::unauthorized(
                "该账号已在其他设备登录，本设备会话已被下线",
            ));
        }
        // 强制改密拦截：必须改密的用户只能访问改密和退出接口
        // （不删除会话——否则改密请求自身也会被挡在门外，用户被迫重新登录）
        if ru.must_change_pwd {
            let path = parts.uri.path();
            let is_allowed = path == "/api/change-password"
                || path == "/api/logout"
                || path == "/api/login";
            if !is_allowed {
                return Err(AppError::unauthorized("你的口令已过期或需首次设置，请先修改口令"));
            }
        }
        state.sessions.touch(&token);
        Ok(RealmUser {
            username: ru.username,
            is_admin: ru.is_admin,
            display_name: ru.display_name,
            must_change_pwd: ru.must_change_pwd,
            token,
            device_id: info.device_id,
        })
    }
}

/// 当前账套内用户（从会话 + 账套归属授权 + 身份对账得来）
pub struct CurrentUser {
    pub user: User,
    /// 会话令牌（服务端内部使用，不向外暴露）
    pub token: String,
    /// 当前账套 key（多账套）
    pub book_key: String,
    /// 当前账套的公司名（多账套下每套不同）
    pub company: String,
}

impl CurrentUser {
    pub fn username(&self) -> &str {
        &self.user.username
    }

    /// 是否拥有某权限（角色权限 + 额外权限）
    pub fn can(&self, p: Perm) -> bool {
        self.user.can(p)
    }

    /// 校验权限，无权限返回 403
    pub fn require(&self, p: Perm) -> Result<(), AppError> {
        if self.can(p) {
            Ok(())
        } else {
            Err(AppError::forbidden(format!("没有「{}」权限", p.label())))
        }
    }
}

#[axum::async_trait]
impl FromRequestParts<Arc<WebState>> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<WebState>,
    ) -> Result<Self, Self::Rejection> {
        let (token, info) = session_of(&parts.headers, state)?;

        // 1) 账号存在且未停用
        let ru: RealmAccount = state
            .realm
            .get_user(&info.username)?
            .ok_or_else(|| AppError::unauthorized("账号已不存在，请重新登录"))?;
        if ru.disabled {
            state.sessions.remove(&token);
            return Err(AppError::forbidden("账号已被停用，请联系管理员"));
        }

        // 2) 账套归属授权：管理员可看全部；归属者可进；账套内已有该用户行 = 被邀请的成员
        let book_key = info.book_key.clone();
        if book_key.is_empty() {
            return Err(AppError::unauthorized("请先选择账套"));
        }
        let book = state
            .realm
            .get_book(&book_key)?
            .ok_or_else(|| AppError::not_found("账套不存在或已被删除"))?;
        let db = state.db_for(&book_key)?;
        let in_book = users::get(&db, &ru.username)?;
        // 账套归属授权必须以账号库的最新 is_admin 为准，而非登录时快照进会话的
        // info.is_admin：否则管理员被降权后，旧会话在自然过期前仍能越权查看全部账套。
        let allowed = ru.is_admin || book.owner_username == ru.username || in_book.is_some();
        if !allowed {
            return Err(AppError::forbidden("无权访问该账套"));
        }

        // 3) 身份对账
        let user = match in_book {
            // 账套内已有该用户行：沿用其账套内角色 / 权限 / 数据范围，不擅自升级
            Some(u) => u,
            None => {
                if ru.is_admin && book.owner_username != ru.username {
                    // 管理员查看他人账套：构造临时账套管理员身份，不写入该账套 user 表，
                    // 避免在他人账套留下账号记录；操作仍按管理员用户名记入审计与凭证。
                    let mut u = User::new(&ru.username, &ru.display_name, Role::Admin);
                    u.password_hash = ru.password_hash.clone();
                    u
                } else {
                    // 归属者：本就是自己的账套，缺失时补种一行（落库）
                    ensure_book_admin(&db, &ru)?;
                    users::get(&db, &ru.username)?
                        .ok_or_else(|| AppError::unauthorized("账套内账号缺失，请重新进入账套"))?
                }
            }
        };
        if user.disabled {
            state.sessions.remove(&token);
            return Err(AppError::forbidden("账号已被停用，请联系管理员"));
        }
        // "一人一机"逐请求复核（平台层 Web 设备绑定，与桌面端账套内 device_id 相互独立）：
        // 管理员豁免；普通账号一旦在平台层绑定了新设备，旧设备会话立即失效。
        // 不能拿账套内 user.device_id 与浏览器指纹比较——那是桌面端绑定的机器指纹，
        // 会令「桌面端登录过 → Web 端同账号所有请求 403」的双端互斥。
        if !ru.is_admin && !ru.device_id.is_empty() && ru.device_id != info.device_id {
            state.sessions.remove(&token);
            return Err(AppError::unauthorized(
                "该账号已在其他设备登录，本设备会话已被下线",
            ));
        }
        // 强制改密拦截（不删除会话，仅拒绝非改密/退出请求）
        if user.must_change_pwd {
            let path = parts.uri.path();
            let is_allowed = path == "/api/change-password"
                || path == "/api/logout"
                || path == "/api/login";
            if !is_allowed {
                return Err(AppError::unauthorized("你的口令已过期或需首次设置，请先修改口令"));
            }
        }
        let company = db.options().company;
        drop(db);
        state.sessions.touch(&token);
        Ok(CurrentUser {
            user,
            token,
            book_key,
            company,
        })
    }
}

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

pub enum AppError {
    Unauthorized(String),
    Forbidden(String),
    BadRequest(String),
    NotFound(String),
    Db(DbError),
    /// 内部错误（阻塞任务失败等）：详情只进服务端日志，不返给客户端
    Internal(String),
    /// 请求过于频繁（如登录限流），附剩余等待秒数
    RateLimited { msg: String, retry_secs: u64 },
}

impl From<DbError> for AppError {
    fn from(e: DbError) -> Self {
        // 领域错误（状态机/校验/不存在）是客户端可修正的，返回 400 并把引擎提示
        // 透给用户；SQLite/序列化等基础设施错误仍走 500（详情只进服务端日志）。
        match e {
            DbError::Fin(fe) => AppError::BadRequest(fe.to_string()),
            other => AppError::Db(other),
        }
    }
}

impl From<fincore::FinError> for AppError {
    fn from(e: fincore::FinError) -> Self {
        AppError::BadRequest(format!("导入数据有误：{e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        // 详情（含路径）只进服务端日志，不透给客户端
        AppError::Internal(format!("文件操作失败：{e}"))
    }
}

impl AppError {
    pub fn unauthorized(m: impl Into<String>) -> Self {
        AppError::Unauthorized(m.into())
    }
    pub fn forbidden(m: impl Into<String>) -> Self {
        AppError::Forbidden(m.into())
    }
    pub fn bad_request(m: impl Into<String>) -> Self {
        AppError::BadRequest(m.into())
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        AppError::NotFound(m.into())
    }
    pub fn rate_limited(m: impl Into<String>, retry_secs: u64) -> Self {
        AppError::RateLimited {
            msg: m.into(),
            retry_secs,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Unauthorized(m) => {
                (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({ "error": m }))).into_response()
            }
            AppError::Forbidden(m) => {
                (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({ "error": m }))).into_response()
            }
            AppError::BadRequest(m) => {
                (StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({ "error": m }))).into_response()
            }
            AppError::NotFound(m) => {
                (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "error": m }))).into_response()
            }
            AppError::Db(e) => {
                // 内部错误详情只记服务端日志，不返给客户端（防表名/路径/SQL 泄露）。
                eprintln!("[finweb] db error: {e}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({ "error": "数据库错误，请稍后重试或联系管理员" })),
                )
                    .into_response()
            }
            AppError::Internal(m) => {
                eprintln!("[finweb] internal error: {m}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({ "error": "服务器内部错误，请稍后重试" })),
                )
                    .into_response()
            }
            AppError::RateLimited { msg, retry_secs } => {
                let mut resp = (
                    StatusCode::TOO_MANY_REQUESTS,
                    axum::Json(serde_json::json!({ "error": msg, "retry_after": retry_secs })),
                )
                    .into_response();
                if let Ok(v) = HeaderValue::from_str(&retry_secs.to_string()) {
                    resp.headers_mut()
                        .insert(axum::http::header::RETRY_AFTER, v);
                }
                resp
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 通用工具
// ---------------------------------------------------------------------------

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 构造 Set-Cookie 头值
pub fn cookie_header(token: &str, max_age_secs: i64) -> HeaderValue {
    // 默认不加 Secure：允许内网/HTTP/WireGuard 隧道等明文场景直接使用。
    // 若前面挂了 HTTPS 反向代理并对外暴露，设置 FINWEB_SECURE_COOKIE=true
    // 强制会话 Cookie 仅经 HTTPS 传输。
    let secure = std::env::var("FINWEB_SECURE_COOKIE")
        .map(|v| v != "0" && v != "false")
        .unwrap_or(false);
    let suffix = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "finbook_sid={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
        token, max_age_secs, suffix
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("finbook_sid=; Path=/; Max-Age=0"))
}

pub fn clear_cookie_header() -> HeaderValue {
    HeaderValue::from_static("finbook_sid=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

/// 期间 -> "YYYY-MM"
pub fn period_to_str(p: fincore::Period) -> String {
    format!("{:04}-{:02}", p.year(), p.month())
}

/// 解析 "2026-01" 或 "202601" 为 Period
pub fn parse_period(s: &str) -> Option<fincore::Period> {
    let s = s.trim();
    let (y, m) = if s.len() == 6 && s.chars().all(|c| c.is_ascii_digit()) {
        (s[0..4].parse().ok()?, s[4..6].parse().ok()?)
    } else if let Some((y, m)) = s.split_once('-') {
        (y.parse().ok()?, m.parse().ok()?)
    } else {
        return None;
    };
    fincore::Period::new(y, m).ok()
}

/// 解析金额字符串（默认 0）
/// 解析用户提交的金额：非法输入返回 400，不再静默归零（错值=0 会把
/// 工资、收付款、库存调整等写错且无任何提示）。支持千分位/全角等惯例写法。
pub fn parse_money_checked(s: &str) -> Result<fincore::Money, AppError> {
    fincore::Money::parse(s).map_err(|_| {
        AppError::bad_request(format!("金额格式不正确：{}", s.trim()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 探测式登录（用户名乱填、永远认证失败）不该在限流表里留下条目：
    /// check 早于口令校验，若它 or_default() 建键，随机用户名就能把表无限撑大。
    #[test]
    fn check_does_not_create_entries() {
        let l = LoginLimiter::new();
        for i in 0..500 {
            let name = format!("ghost{i}");
            assert!(l.check(&name).is_ok());
        }
        assert!(l.inner.lock().unwrap_or_else(|e| e.into_inner()).is_empty(), "check 不该建条目");
    }

    #[test]
    fn throttles_after_max_failures() {
        let l = LoginLimiter::new();
        for _ in 0..LOGIN_MAX_FAILURES {
            l.record_failure("bob");
        }
        assert!(l.check("bob").is_err(), "达到阈值应拒绝");
        l.clear("bob");
        assert!(l.check("bob").is_ok(), "登录成功清零后应放行");
    }
}
