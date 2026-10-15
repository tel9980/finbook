//! Web 服务共享状态：账号库、账套注册表、会话管理、鉴权提取器与错误类型。

use std::collections::{HashMap, HashSet};
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
use tracing::error;

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
                error!(error = %e, "账套归属迁移失败");
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
                    error!(book = %key, error = %e, "迁移补套内管理员失败：账套打不开");
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
                error!(book = %key, error = %e, "迁移补套内管理员失败");
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
    /// 已完成 schema 初始化的账套 key。
    ///
    /// Web 端每个请求都要借出账套连接，而 `Db::open` 里的 `schema::init` 每次
    /// 都得为几十张表跑一遍存在性检查（防"版本号对但表不全"的半损坏库）。
    /// 账套文件是长期不变的，迁移只需在首次打开时做一次，这里记住结果：
    /// 命中时走 `open_no_migrate`（只设连接级 PRAGMA，省掉全表检查）。
    ///
    /// 账套文件被换掉时标记同步清除（恢复账套会 `unregister` + `register`），
    /// 下次打开重新走完整迁移检查。
    migrated: Mutex<HashSet<String>>,
    /// 启动时批量迁移的进度与结果（供 readiness 探针汇报）。
    migration: Mutex<MigrationStatus>,
}

/// 启动批量迁移的状态。
///
/// 为什么要专门记一份：迁移原本是**懒执行**的 —— 账套第一次被打开时才跑
/// `schema::init`。这带来两个真实问题：
///
/// 1. 升级后探针**报绿**，但账套 schema 还停在旧版本。生产上就是这样踩到的：
///    部署完成、readiness 全绿，而用户第一次点「收货」才报
///    `no such column: item_code`。
/// 2. 故障点和原因隔得很远 —— 部署的人不会想到问题在部署几小时后才出现，
///    更不会联想到「schema 版本」。
///
/// 所以改成**启动时后台把全部账套迁一遍**，并把结果如实汇报出去：
/// 迁完 = 就绪；没迁完 = 未就绪；某个账套迁不动 = 明确报出来是哪个、报什么。
#[derive(Clone, Debug, Default)]
pub struct MigrationStatus {
    /// 是否仍在进行中
    pub running: bool,
    /// 已注册账套总数
    pub total: usize,
    /// 迁移成功的账套数
    pub ok: usize,
    /// 迁移失败的账套：key + 错误信息
    pub failed: Vec<(String, String)>,
}

impl MigrationStatus {
    /// 未开始时（启动到后台任务拉起之间）视为「迁移已开始但未完成」，
    /// 这样 readiness 在这个窗口内是 503，而不是假装就绪。
    fn pending(total: usize) -> Self {
        Self {
            running: true,
            total,
            ok: 0,
            failed: Vec::new(),
        }
    }

    pub fn done(&self) -> bool {
        !self.running
    }

    /// 是否有账套没能迁移
    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }
}

impl BookRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            migrated: Mutex::new(HashSet::new()),
            migration: Mutex::new(MigrationStatus::default()),
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
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(k, _)| k != key);
        // 账套文件可能已被恢复成另一份（或已删除）：清掉初始化标记，
        // 下次打开重新走 schema 完整检查，而不是沿用旧文件的结论。
        lock(&self.migrated).remove(key);
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
        if lock(&self.migrated).contains(key) {
            // 首次打开已跑过 schema 初始化：只设连接级 PRAGMA，跳过全表检查
            return Db::open_no_migrate(&path);
        }
        let db = Db::open(&path)?;
        lock(&self.migrated).insert(key.to_string());
        Ok(db)
    }

    /// 迁移状态（readiness 探针用）
    pub fn migration_status(&self) -> MigrationStatus {
        lock(&self.migration).clone()
    }

    /// 已注册但**尚未**完成 schema 初始化的账套（readiness 用）。
    ///
    /// 比「启动时批量迁移跑过没有」更可靠：批量任务只覆盖启动那一刻的账套快照，
    /// 而运行期新增/恢复的账套不在其中。这里直接比对「已注册」与「已迁移」两个集合，
    /// 任何来源的漏网账套都会被抓到。
    pub fn unmigrated_books(&self) -> Vec<String> {
        let done = lock(&self.migrated);
        self.list()
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| !done.contains(k))
            .collect()
    }

    /// 启动时先把状态标成「迁移进行中」，再由后台任务跑 [`Self::migrate_all`]。
    ///
    /// 为什么要单独一步：服务开始监听到后台任务真正跑起来之间有个窗口。
    /// 不先 arm 的话，readiness 在这个窗口里会看到「默认状态 = 没在迁移」
    /// 而**误报就绪** —— 正好是这次要消灭的那种「探针说好了、其实没好」。
    ///
    /// 幂等：只在状态还是初始值时生效，重复调用不会抹掉已有结果。
    pub fn arm_migration(&self) {
        let mut st = lock(&self.migration);
        if !st.running && st.total == 0 && st.failed.is_empty() {
            *st = MigrationStatus::pending(self.list().len());
        }
    }

    /// 把所有已注册账套的 schema 迁移跑一遍（幂等）。
    ///
    /// 为什么在后台线程而不是启动流程里同步做：
    /// 迁移是**文件 I/O + 写事务**，耗时随账套数线性增长。同步做会让
    /// `/api/health`（liveness）在迁移期间不响应 —— 而 liveness 一旦碰
    /// 重活，编排器就会把容器判 unhealthy 并重启，**放大抖动**。
    /// 所以：先开始监听，再在后台迁移，readiness 负责如实汇报进度。
    ///
    /// 单个账套失败不阻断其它账套 —— 失败信息记录下来由 readiness 报出，
    /// 而不是让整个服务起不来（那样会掩盖问题，且重启也修不好）。
    pub fn migrate_all(&self) -> MigrationStatus {
        let books = self.list();
        {
            let mut st = lock(&self.migration);
            *st = MigrationStatus::pending(books.len());
        }
        let mut st = MigrationStatus {
            running: true,
            total: books.len(),
            ok: 0,
            failed: Vec::new(),
        };
        for (key, _path) in &books {
            // 走 open 而不是直接 Db::open：已迁移过的账套会命中 migrated 标记跳过，
            // 避免重复的全表存在性检查。
            match self.open(key) {
                Ok(_) => st.ok += 1,
                Err(e) => st.failed.push((key.clone(), e.to_string())),
            }
        }
        st.running = false;
        *lock(&self.migration) = st.clone();
        st
    }
}

impl Default for BookRegistry {
    fn default() -> Self {
        Self::new()
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
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new()
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
    /// 登录时的设备指纹（用于逐请求复核「设备在不在绑定名单内」）
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
        if let Some(i) = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(token)
        {
            i.last_active = now_secs();
        }
    }

    pub fn period(&self, token: &str) -> Option<i32> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(token)
            .map(|i| i.period_ymm)
    }

    pub fn set_period(&self, token: &str, ymm: i32) {
        if let Some(i) = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(token)
        {
            i.period_ymm = ymm;
        }
    }

    /// 切换当前账套（登录后选账套时调用）
    pub fn set_book_key(&self, token: &str, key: &str) {
        if let Some(i) = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(token)
        {
            i.book_key = key.to_string();
        }
    }

    pub fn remove(&self, token: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(token);
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
    /// 否则旧设备上的会话还能继续用到自然过期，设备绑定会被绕过。
    pub fn remove_by_username(&self, username: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, i| i.username != username);
    }

    /// 只清掉「某账号在**某台设备**上」的会话。
    ///
    /// 登录时用：同一台设备重复登录要顶掉它自己上一个会话（否则旧 token 一直有效，
    /// 泄露后失效不了），但**别的设备的会话必须留着** —— 一个账号允许绑
    /// `MAX_DEVICES_PER_USER` 台，这些会话要能并存，否则"允许绑两台"就只是个数字：
    /// 第 2 台一登录就把第 1 台踢下线，用户体感仍然是只能登一台。
    ///
    /// 与 `remove_by_username`（全清）、`remove_others`（改密后清别人那几台）是三种
    /// 不同语义，别混用：
    /// · 登录 → 本设备（这里）
    /// · 改密 → 别人的设备（remove_others）
    /// · 重置设备 / 停用 / 删号 → 全部（remove_by_username）
    pub fn remove_by_username_and_device(&self, username: &str, device_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, i| !(i.username == username && i.device_id == device_id));
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

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
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
        // 设备绑定逐请求复核。CurrentUser 提取器里本来就有这段，但平台级接口
        // （建账套、选账套、改密）只走本提取器；缺了它，被管理员重置过设备的旧
        // 浏览器仍能拿着旧会话在账套之外建套、改密。
        // 逐请求复核「当前设备在不在该账号的绑定名单内」。
        //
        // 判定读 `realm_user_device` 表，不读 `realm_user.device_id` 那一列 —— 那是
        // 「第一台」的快照（只为了账号列表显示同步），用它判就退回了一台机，而且这两处
        // 各写一份必然分叉。名单为空 = 还没绑过，放行（登录时会去登记）。
        if !ru.is_admin
            && !state
                .realm
                .device_allowed(&info.username, &info.device_id)?
        {
            state.sessions.remove(&token);
            return Err(AppError::unauthorized(
                "该账号已在其他设备登录，本设备会话已被下线",
            ));
        }
        // 强制改密拦截：必须改密的用户只能访问改密和退出接口
        // （不删除会话——否则改密请求自身也会被挡在门外，用户被迫重新登录）
        if ru.must_change_pwd {
            let path = parts.uri.path();
            let is_allowed =
                path == "/api/change-password" || path == "/api/logout" || path == "/api/login";
            if !is_allowed {
                return Err(AppError::unauthorized(
                    "你的口令已过期或需首次设置，请先修改口令",
                ));
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
        // 设备绑定逐请求复核（平台层 Web 设备绑定，与桌面端账套内 device_id 相互独立）：
        // 管理员豁免；普通账号的设备必须在该账号的绑定名单内（上限
        // `RealmDb::MAX_DEVICES_PER_USER` = 2 台）。
        // 不能拿账套内 user.device_id 与浏览器指纹比较——那是桌面端绑定的机器指纹，
        // 会令「桌面端登录过 → Web 端同账号所有请求 403」的双端互斥。
        //
        // 判定读 `realm_user_device` 表，不读 `realm_user.device_id` 那一列 —— 那是
        // 「第一台」的快照（只为了账号列表显示同步），用它判就退回了一台机，而且这两处
        // 各写一份必然分叉。名单为空 = 还没绑过，放行（登录时会去登记）。
        if !ru.is_admin
            && !state
                .realm
                .device_allowed(&info.username, &info.device_id)?
        {
            state.sessions.remove(&token);
            return Err(AppError::unauthorized(
                "该账号已在其他设备登录，本设备会话已被下线",
            ));
        }
        // 强制改密拦截（不删除会话，仅拒绝非改密/退出请求）
        if user.must_change_pwd {
            let path = parts.uri.path();
            let is_allowed =
                path == "/api/change-password" || path == "/api/logout" || path == "/api/login";
            if !is_allowed {
                return Err(AppError::unauthorized(
                    "你的口令已过期或需首次设置，请先修改口令",
                ));
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
    RateLimited {
        msg: String,
        retry_secs: u64,
    },
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
        // 原来给**所有**引擎错误都加了「导入数据有误：」前缀。这是错的：这个 From
        // 是全局转换，任何一处 `?` 掉一个 FinError 都会经过它 —— 结转损益、凭证
        // 记账、结账校验全都算。实测重复点「结转损益」，用户看到的是
        //   ⚠ 导入数据有误：本期没有需要结转的损益类科目（损益发生额均为零）
        // 而他根本没有在导入数据，也确实有 658,000 的损益额（第一次已结转）。
        // 引擎层的信息本来就是写给人看的（「科目表缺少「本年利润」科目…」），
        // 这里原样透传；确需标注来源的，由具体调用点自己拼。
        AppError::BadRequest(e.to_string())
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
            AppError::Unauthorized(m) => (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({ "error": m })),
            )
                .into_response(),
            AppError::Forbidden(m) => (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({ "error": m })),
            )
                .into_response(),
            AppError::BadRequest(m) => (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({ "error": m })),
            )
                .into_response(),
            AppError::NotFound(m) => (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({ "error": m })),
            )
                .into_response(),
            AppError::Db(e) => {
                // 内部错误详情只记服务端日志，不返给客户端（防表名/路径/SQL 泄露）。
                error!(error = %e, "数据库错误");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(
                        serde_json::json!({ "error": "数据库错误，请稍后重试或联系管理员" }),
                    ),
                )
                    .into_response()
            }
            AppError::Internal(m) => {
                error!(error = %m, "内部错误");
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

/// 解析**期间参数**：缺失/纯空白 → 用 `default`；存在但解析不了 → 400。
///
/// 为什么不能写成 `.and_then(parse_period).unwrap_or(default)`：
/// 那会把「用户把期间敲错」静默替换成「查另一个区间」，**而报表照样借贷平衡**
/// —— 错值在结果里根本看不出来，只能靠「平不平」判断，偏偏它是平的。
///
/// 生产实测（同一账套，10 万张已记账凭证，全年真实值 9,951,930）：
///
/// | from 传法 | 返回的本期借方 | 对不对 |
/// |---|---|---|
/// | `2026-01` | 9,951,930 | ✓ |
/// | `202601` | 9,951,930 | ✓ |
/// | `2026-01-01`（日期格式） | 829,300 | ✗ 只算了一个月 |
/// | `garbage` | 829,300 | ✗ **HTTP 200** |
/// | 不传 | 829,300 | ✗ **HTTP 200** |
///
/// 界面上报表的起止期间是**无 `type` 的自由文本框**（`#r-from` / `#r-to`），
/// 用户敲错完全正常。财务数字错了还查不出来，比直接报错危险得多 ——
/// 与 `parse_money_checked` 同一原则：非法输入返回 400，不静默回退。
pub fn period_param(
    s: Option<&String>,
    default: fincore::Period,
) -> Result<fincore::Period, AppError> {
    match s.map(|x| x.trim()).filter(|x| !x.is_empty()) {
        None => Ok(default),
        Some(t) => parse_period(t).ok_or_else(|| {
            AppError::bad_request(format!("期间格式不正确：{t}（应为 202601 或 2026-01）"))
        }),
    }
}

/// 解析**期间区间**（from/to）：缺失 → 默认值；非法 → 400；`from > to` → 400。
///
/// 区间倒置也要拦：SQL 是 `period BETWEEN ?1 AND ?2`，from > to 会**安静地**
/// 返回空表，用户看到「一张凭证都没有」，而真实原因是他把区间填反了。
/// 同样属于「静默给出一个合法外观的错误结果」。
pub fn period_range_param(
    q: &HashMap<String, String>,
    default_from: fincore::Period,
    default_to: fincore::Period,
) -> Result<(fincore::Period, fincore::Period), AppError> {
    let from = period_param(q.get("from"), default_from)?;
    let to = period_param(q.get("to"), default_to)?;
    if from > to {
        return Err(AppError::bad_request(format!(
            "起始期间不能晚于结束期间：{} > {}",
            period_to_str(from),
            period_to_str(to)
        )));
    }
    Ok((from, to))
}

/// 解析金额字符串（默认 0）
/// 解析用户提交的金额：非法输入返回 400，不再静默归零（错值=0 会把
/// 工资、收付款、库存调整等写错且无任何提示）。支持千分位/全角等惯例写法。
pub fn parse_money_checked(s: &str) -> Result<fincore::Money, AppError> {
    fincore::Money::parse(s)
        .map_err(|_| AppError::bad_request(format!("金额格式不正确：{}", s.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 期间参数：缺失/空白 → 默认值；**存在但解析不了 → 必须 400**。
    ///
    /// 这条是整个报表校验的地基。原来各 handler 直接写
    /// `.and_then(parse_period).unwrap_or(默认值)`，而 `and_then` 在解析失败时
    /// 也给 `None`，于是「敲错」和「没传」被混同 —— 静默换一个区间，
    /// 而且返回的报表照样借贷平衡，错值在结果里看不出来。
    #[test]
    fn period_param_distinguishes_absent_from_garbage() {
        let dflt = fincore::Period::new(2026, 3).unwrap();
        let got = |s: Option<&str>| match s {
            None => period_param(None, dflt),
            Some(v) => period_param(Some(&v.to_string()), dflt),
        };
        let val = |s: Option<&str>| match got(s) {
            Ok(p) => p,
            // AppError 既没 Debug 也没 Display，所以只能报输入值
            Err(_) => panic!("{s:?} 应当解析成功，却返回了错误"),
        };

        // 缺失 / 空串 / 纯空白 → 用默认值（这是合法语义，必须保留）
        assert_eq!(val(None), dflt);
        assert_eq!(val(Some("")), dflt);
        assert_eq!(val(Some("   ")), dflt);

        // 两种合法格式都要认（界面上两种都在用：月份式与期间式）
        assert_eq!(val(Some("202603")), dflt);
        assert_eq!(val(Some("2026-03")), dflt);
        // 首尾空白要容忍：URL 里手写参数常带空格
        assert_eq!(val(Some(" 2026-03 ")), dflt);

        // 下面这些以前全都静默回退成 dflt，现在必须 400
        for bad in [
            "2026-01-01",       // 日期格式：用户最容易敲的
            "2026/01",          // 斜杠
            "2026.01",          // 点号
            "202613",           // 月份 13
            "202600",           // 月份 00
            "20261",            // 位数不对
            "2026",             // 只有年
            "garbage",          // 乱填
            "-2026-01",         // 负号
            "2026-13",          // 月份 13（月份式）
            "2601",             // 两位年
            "2026-01-01T00:00", // ISO 时间戳
            "<script>",         // 顺带确认不会当 HTML/注入面
        ] {
            if let Ok(p) = got(Some(bad)) {
                panic!("{bad:?} 解析不了，必须报错，却回退成了 {p:?}")
            }
        }
    }

    /// 区间：两端都要校验，且 from > to 必须拦。
    ///
    /// 倒置之所以要拦：SQL 是 `period BETWEEN ?1 AND ?2`，from > to 会
    /// **安静地**返回空表 —— 用户看到「一张凭证都没有」，真实原因却是填反了。
    #[test]
    fn period_range_param_validates_both_ends_and_order() {
        let d1 = fincore::Period::new(2026, 1).unwrap();
        let d12 = fincore::Period::new(2026, 12).unwrap();
        let q = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };

        // 缺省 → 用默认区间
        assert_eq!(
            period_range_param(&q(&[]), d1, d12).unwrap_or((d1, d12)),
            (d1, d12)
        );
        // 只给 from → to 落到默认
        let got = period_range_param(&q(&[("from", "2026-05")]), d1, d12);
        assert_eq!(
            got.unwrap_or((d1, d12)),
            (fincore::Period::new(2026, 5).unwrap(), d12)
        );
        // 单月（from == to）与跨年都要过
        assert!(period_range_param(&q(&[("from", "2026-03"), ("to", "2026-03")]), d1, d12).is_ok());
        assert!(period_range_param(&q(&[("from", "2025-11"), ("to", "2026-02")]), d1, d12).is_ok());

        // 倒置 → 400（否则安静返回空表）
        for (f, t) in [("2026-12", "2026-01"), ("202601", "202512")] {
            assert!(
                period_range_param(&q(&[("from", f), ("to", t)]), d1, d12).is_err(),
                "区间倒置 {f}..{t} 必须报错，不能静默返回空表"
            );
        }
        // 两端任一非法 → 400
        assert!(period_range_param(&q(&[("from", "garbage")]), d1, d12).is_err());
        assert!(period_range_param(&q(&[("to", "2026-01-01")]), d1, d12).is_err());
        assert!(
            period_range_param(&q(&[("from", "2026-01"), ("to", "2026-01-01")]), d1, d12).is_err()
        );
    }

    /// 探测式登录（用户名乱填、永远认证失败）不该在限流表里留下条目：
    /// check 早于口令校验，若它 or_default() 建键，随机用户名就能把表无限撑大。
    #[test]
    fn check_does_not_create_entries() {
        let l = LoginLimiter::new();
        for i in 0..500 {
            let name = format!("ghost{i}");
            assert!(l.check(&name).is_ok());
        }
        assert!(
            l.inner.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
            "check 不该建条目"
        );
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
