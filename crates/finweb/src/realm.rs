//! 平台身份层（realm）
//!
//! 独立于账套（`.fbk`）的一层全局存储，负责两件事：
//! 1. **全局账号** `realm_user`：能登录 Web 的"账号"。管理员与普通用户都是这里面的账号。
//! 2. **账套目录** `realm_book`：每个账套一条记录，记录 `key`（文件名）、`path`、`owner_username`（创建者）。
//!
//! 账套内部仍用 `.fbk` 自带的 `user` 表做授权；进入账套时由 `state::CurrentUser` 按本文件的
//! 归属关系做"授权 + 身份对账"（见 [`ensure_book_admin`]），从而完整复用账套引擎的权限/设备绑定逻辑。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use fincore::user::{burn_argon2, hash_password, verify_password, PasswordPolicy, Role, User};
use findb::{users, Db, DbError, DbResult};
use rusqlite::{Connection, OptionalExtension};
use tracing::error;

/// 全局账号（平台层）
#[derive(Clone, Debug)]
pub struct RealmUser {
    pub id: i64,
    pub username: String,
    pub display_name: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub must_change_pwd: bool,
    /// **第一台**设备的指纹。只为账号列表的「已绑定/未绑定」显示同步；
    /// 授权判定一律读 `realm_user_device` 表（见 `device_allowed`）——
    /// 用它判就退回了一台机。空 = 尚未绑定，下次登录自动绑定
    pub device_id: String,
    pub created_at: String,
    /// 锁定截止时间（`%Y-%m-%d %H:%M:%S`，空 = 未锁定）；由平台口令策略 max_fail/lock_minutes 驱动
    pub locked_until: String,
}

/// 账套目录项（平台层）
#[derive(Clone, Debug)]
pub struct RealmBook {
    pub id: i64,
    pub key: String,
    pub path: String,
    pub owner_username: String,
    pub company: String,
    pub created_at: String,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS realm_user (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    username        TEXT UNIQUE NOT NULL,
    display_name    TEXT NOT NULL DEFAULT '',
    password_hash   TEXT NOT NULL,
    is_admin        INTEGER NOT NULL DEFAULT 0,
    disabled        INTEGER NOT NULL DEFAULT 0,
    must_change_pwd INTEGER NOT NULL DEFAULT 0,
    device_id       TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL DEFAULT '',
    locked_until    TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS realm_option (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS realm_login_attempt (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    username    TEXT NOT NULL,
    ts          TEXT NOT NULL,
    ok          INTEGER NOT NULL,
    ip          TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS realm_book (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    key             TEXT UNIQUE NOT NULL,
    path            TEXT NOT NULL,
    owner_username  TEXT NOT NULL,
    company         TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL DEFAULT ''
);
-- 开放 API 密钥（v1）：让 WMS / MES / 电商 / 银行 / 税务等外部系统能接进来。
--
-- 存哈希不存明文：数据库被读走也不能直接冒用密钥（与 password_hash 同理）。
-- 只存哈希意味着**明文只在创建那一刻返回一次**，之后无法找回——这是刻意的，
-- 宁可让用户重发一次，也不要留一个能被数据库 dump 带走的明文凭据。
CREATE TABLE IF NOT EXISTS realm_api_key (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT NOT NULL,
    -- 密钥前缀（形如 fbk_ab12cd），仅供界面辨认与定位，不参与鉴权
    prefix       TEXT NOT NULL,
    key_hash     TEXT NOT NULL,
    -- 绑定的账套；空 = 平台级（可访问所有账套，只给平台管理员签发）
    book_key     TEXT NOT NULL DEFAULT '',
    -- 逗号分隔的权限码；空 = 只读
    scopes       TEXT NOT NULL DEFAULT '',
    disabled     INTEGER NOT NULL DEFAULT 0,
    created_by   TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL DEFAULT '',
    last_used_at TEXT NOT NULL DEFAULT '',
    expires_at   TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_realm_api_key_prefix ON realm_api_key(prefix);
"#;

/// 账号 ↔ 设备的绑定名单（首次登录自动登记；此后换设备须管理员重置）。
///
/// 为什么单独一张表，而不是在 `realm_user` 上加 `device_id2`：加列只能表达固定台数，
/// 想改上限就得再迁移一次；而且「哪台是哪台」一旦超过两台就没法用列表达。
const DEVICE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS realm_user_device (
    username    TEXT NOT NULL,
    device_id   TEXT NOT NULL,
    device_name TEXT NOT NULL DEFAULT '',
    bound_at    TEXT NOT NULL DEFAULT '',
    last_seen   TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (username, device_id)
);
CREATE INDEX IF NOT EXISTS idx_realm_user_device_username
    ON realm_user_device(username);
"#;

/// 账号库（单进程内以 Mutex<Connection> 持有，WAL 模式下并发安全）
/// 密码策略缓存（仅在变更时失效）
pub struct RealmDb {
    inner: Mutex<Connection>,
    path: PathBuf,
    /// 密码策略缓存（仅在变更时失效）
    policy_cache: RwLock<Option<PasswordPolicy>>,
}

impl RealmDb {
    /// 取库连接。
    ///
    /// 为什么不用 `self.conn()`：一个 panic 会把 Mutex 打成「中毒」，
    /// 此后**每一次登录都 panic** —— 一次局部 bug 变成永久不可用，只能重启进程。
    ///
    /// 这里锁保护的是一条 SQLite 连接，不是"必须完全一致的内存不变量"：数据一致性
    /// 由 SQLite 自己的事务保证，毒化后继续用这条连接是安全的。所以从毒化里取回内值。
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 同上：策略缓存也是 `RwLock`，毒化后取回内值而不是 panic
    fn policy_read(&self) -> std::sync::RwLockReadGuard<'_, Option<PasswordPolicy>> {
        self.policy_cache.read().unwrap_or_else(|e| e.into_inner())
    }
    fn policy_write(&self) -> std::sync::RwLockWriteGuard<'_, Option<PasswordPolicy>> {
        self.policy_cache.write().unwrap_or_else(|e| e.into_inner())
    }
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 开放 API 密钥的存储哈希（sha2-256，hex）
///
/// 用快哈希而不是 argon2：密钥是 32 字节 CSPRNG 随机值，熵高到无从「猜」，
/// 慢哈希的防暴力破解意义在这里不存在；而鉴权在**每个请求**上都要跑一次，
/// argon2 的 ~100ms 会让外部系统对接的吞吐直接崩掉。
/// 真正防住「密钥泄露」的是存哈希不存明文——数据库被 dump 走也拿不到可用密钥。
fn hash_api_secret(secret: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 设备指纹：`sha256(secret | username | device_id)`。
///
/// 存哈希而不是原样的 `device_id`，是为了让**名单本身不能当通行证**：能读到库的人
/// 抄一行 `device_id` 出去，也换不来一台「被授权的设备」。
///
/// 但要说清楚它挡不住什么：`device_id` 终究是客户端自己上报的（见 SECURITY_TODO M-4），
/// 所以真正的防线不是哈希，而是下面 `bind_device` 的政策 —— **首次绑定之后，服务端
/// 不再自动往名单里加新设备**，换设备必须由管理员在账号页显式重置。
fn device_fingerprint(secret: &str, username: &str, device_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    h.update(b"|");
    h.update(username.as_bytes());
    h.update(b"|");
    h.update(device_id.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 平台级的设备指纹盐：首次用到时随机生成并落 `realm_option`。
///
/// 每个 realm 一份，跟着库走 —— 库拷到别处，同一台设备的指纹仍然对得上；
/// 反过来，把 `device_id` 抄到另一个部署里也换不出同样的指纹。
fn device_secret(conn: &rusqlite::Connection) -> DbResult<String> {
    if let Ok(v) = conn.query_row(
        "SELECT value FROM realm_option WHERE key='device_secret'",
        [],
        |r| r.get::<_, String>(0),
    ) {
        if !v.trim().is_empty() {
            return Ok(v);
        }
    }
    let mut raw = [0u8; 32];
    rand::Rng::fill(&mut rand::thread_rng(), &mut raw);
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    conn.execute(
        "INSERT OR REPLACE INTO realm_option(key, value) VALUES('device_secret', ?1)",
        rusqlite::params![hex],
    )?;
    Ok(hex)
}

/// 密钥列表项（**不含哈希**，也不含明文——明文只在创建时返回一次）
#[derive(Clone, Debug, serde::Serialize)]
pub struct ApiKeyInfo {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub book_key: String,
    pub scopes: String,
    pub disabled: bool,
    pub created_by: String,
    pub created_at: String,
    pub last_used_at: String,
    pub expires_at: String,
}

/// 密钥鉴权通过后的身份
#[derive(Clone, Debug)]
pub struct ApiKeyAuth {
    pub id: i64,
    /// 密钥前缀：写审计时用来标识「是哪把密钥调的」，不泄露密钥本身
    pub prefix: String,
    /// 绑定的账套；空 = 平台级
    pub book_key: String,
    /// 逗号分隔的权限码；空 = 只读
    pub scopes: String,
}

impl ApiKeyAuth {
    /// 是否被授予某权限。空 scopes = 只读（只允许 Report 类只读端点）。
    pub fn allows(&self, perm: &str) -> bool {
        self.scopes
            .split(',')
            .map(|s| s.trim())
            .any(|s| !s.is_empty() && s == perm)
    }
    pub fn is_readonly(&self) -> bool {
        self.scopes.trim().is_empty()
    }
}

impl RealmDb {
    /// 打开（必要时创建）账号库并初始化表结构
    pub fn open<P: AsRef<Path>>(path: P) -> DbResult<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        let conn = Connection::open(&path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Self {
            inner: Mutex::new(conn),
            path,
            policy_cache: RwLock::new(None),
        };
        db.init()?;
        Ok(db)
    }

    /// 库文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn init(&self) -> DbResult<()> {
        self.conn().execute_batch(SCHEMA)?;
        self.migrate()?;
        Ok(())
    }

    /// 轻量迁移：为早期创建的库补上后加的列（device_id / locked_until），
    /// 以及「一个账号可绑多台设备」用的 `realm_user_device` 表。
    fn migrate(&self) -> DbResult<()> {
        let conn = self.conn();
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(realm_user)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(Result::ok)
            .collect();
        if !cols.iter().any(|c| c == "device_id") {
            conn.execute(
                "ALTER TABLE realm_user ADD COLUMN device_id TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        if !cols.iter().any(|c| c == "locked_until") {
            conn.execute(
                "ALTER TABLE realm_user ADD COLUMN locked_until TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }

        // 设备绑定从「单列 device_id」升级成「集合」。单列表达不了两台：第二台设备
        // 要么覆盖第一台（旧的立刻失效），要么被拒。所以新建一张表，并把旧列里已有的
        // 绑定搬进去 —— **不搬的话升级后所有账号的设备绑定会静默消失**，等于给全部
        // 用户解绑：安全后果是「绑定形同虚设」，症状是「谁都能登」，而且**不报任何错**。
        conn.execute_batch(DEVICE_SCHEMA)?;
        // v1.8.53：设备名单加指纹列（存量行在下面回填）。
        // 必须放在 `DEVICE_SCHEMA` 之后 —— 老库里这张表可能是上一段才建出来的。
        let dcols: Vec<String> = conn
            .prepare("PRAGMA table_info(realm_user_device)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .filter_map(Result::ok)
            .collect();
        if !dcols.iter().any(|c| c == "device_hash") {
            conn.execute(
                "ALTER TABLE realm_user_device ADD COLUMN device_hash TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        // execute() 返回受影响行数 —— 这里**必须**用 execute 而不是 query_row：
        // query_row 期望恰好一行，而全新 realm 里没有任何已绑定设备，这条
        // INSERT...SELECT 影响 0 行，会返回 QueryReturnedNoRows 把建库整个搞崩
        // （实测 186 个集成测试同时挂在 realm 打开那一行）。
        let moved = conn.execute(
            "INSERT OR IGNORE INTO realm_user_device(username, device_id, bound_at, last_seen)
             SELECT username, device_id, created_at, created_at
             FROM realm_user WHERE device_id <> ''",
            [],
        )?;
        if moved > 0 {
            // 旧库一个账号最多一个值，去重天然成立；bound_at/last_seen 用 created_at 顶替
            tracing::info!(
                count = moved,
                "已把单列 device_id 的绑定迁移到 realm_user_device"
            );
        }

        // v1.8.53：给还没有指纹的存量绑定行补上。
        // **不补的后果与上一段同类**：指纹为空 = 这台设备永远匹配不上 = 所有老用户
        // 升级后当场被锁在门外，症状是「登录成功但每个请求都说设备未授权」。
        let secret = device_secret(&conn)?;
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT username, device_id FROM realm_user_device WHERE device_hash = ''")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        for (u, d) in rows {
            conn.execute(
                "UPDATE realm_user_device SET device_hash=?3 WHERE username=?1 AND device_id=?2",
                rusqlite::params![u, d, device_fingerprint(&secret, &u, &d)],
            )?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // 账号
    // ---------------------------------------------------------------

    pub fn count_users(&self) -> DbResult<i64> {
        let conn = self.conn();
        Ok(conn.query_row("SELECT COUNT(*) FROM realm_user", [], |r| r.get(0))?)
    }

    /// 首次启动引导：若没有任何账号，则创建管理员。
    /// - `admin_pass` 非空：用给定口令创建（运维通过环境变量注入，推荐）。
    /// - 否则：自动生成强口令，调用方负责打印一次性凭据。
    ///   返回 `(username, password)`：若管理员已存在则返回 `None`。
    pub fn ensure_bootstrap(
        &self,
        admin_user: &str,
        admin_pass: &str,
        must_change: bool,
        policy: &PasswordPolicy,
    ) -> DbResult<Option<(String, String)>> {
        if self.count_users()? > 0 {
            return Ok(None);
        }
        let (user, pass) = if admin_pass.is_empty() {
            let p = generate_password();
            (admin_user.to_string(), p)
        } else {
            (admin_user.to_string(), admin_pass.to_string())
        };
        self.create_user(&user, "管理员", &pass, true, must_change, policy)?;
        Ok(Some((user, pass)))
    }

    pub fn create_user(
        &self,
        username: &str,
        display_name: &str,
        password: &str,
        is_admin: bool,
        must_change: bool,
        policy: &PasswordPolicy,
    ) -> DbResult<i64> {
        let username = username.trim().to_string();
        if username.is_empty() {
            return Err(DbError::Fin(fincore::FinError::msg("用户名不能为空")));
        }
        // 口令强度统一走应用级 PasswordPolicy，不再各入口手写 len() < 6。
        policy
            .check(password)
            .map_err(fincore::FinError::msg)
            .map_err(DbError::Fin)?;
        if self.get_user(&username)?.is_some() {
            return Err(DbError::Fin(fincore::FinError::msg("该用户名已存在")));
        }
        let conn = self.conn();
        conn.execute(
            "INSERT INTO realm_user(username,display_name,password_hash,is_admin,disabled,must_change_pwd,device_id,created_at)
             VALUES(?1,?2,?3,?4,0,?5,'',?6)",
            rusqlite::params![
                username,
                display_name,
                hash_password(password),
                is_admin as i64,
                must_change as i64,
                now()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn authenticate(&self, username: &str, password: &str) -> DbResult<Option<RealmUser>> {
        match self.get_user(username)? {
            Some(u) => {
                if u.disabled {
                    return Ok(None);
                }
                if verify_password(password, &u.password_hash) {
                    // 顺手把旧版 salt$sha256 升级成 argon2id 并回写。桌面端登录走
                    // findb::security::login 有这一步，Web 端走本函数；缺了它，
                    // 历史账号会永久停在弱哈希上，且弱哈希还会被同步进各账套。
                    let mut u = u;
                    if fincore::user::is_legacy_hash(&u.password_hash) {
                        u.password_hash = hash_password(password);
                        let conn = self.conn();
                        conn.execute(
                            "UPDATE realm_user SET password_hash=?2 WHERE username=?1",
                            rusqlite::params![username, u.password_hash],
                        )?;
                    }
                    Ok(Some(u))
                } else {
                    // 口令错误时做等价空校验，降低用户名枚举的时序差异
                    let _ = burn_argon2(password);
                    Ok(None)
                }
            }
            None => {
                // 用户不存在也做等价空校验：真实执行一次 argon2，耗时与真实校验相当
                let _ = burn_argon2(password);
                Ok(None)
            }
        }
    }

    pub fn get_user(&self, username: &str) -> DbResult<Option<RealmUser>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id,username,display_name,password_hash,is_admin,disabled,must_change_pwd,device_id,created_at,locked_until
             FROM realm_user WHERE username=?1",
            rusqlite::params![username],
            map_user,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_users(&self) -> DbResult<Vec<RealmUser>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id,username,display_name,password_hash,is_admin,disabled,must_change_pwd,device_id,created_at,locked_until
             FROM realm_user ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], map_user)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---------------------------------------------------------------
    // 平台安全：口令策略 / 登录审计 / 账号锁定（Web 安全中心）
    // ---------------------------------------------------------------

    /// 平台口令策略（realm_option 存储；缺省 = 与桌面端一致的默认策略）
    pub fn policy(&self) -> DbResult<PasswordPolicy> {
        // 先尝试读取缓存
        if let Some(cached) = self.policy_read().clone() {
            return Ok(cached);
        }
        let conn = self.conn();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM realm_option WHERE key='password_policy'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let policy = raw
            .and_then(|s| serde_json::from_str::<PasswordPolicy>(&s).ok())
            .unwrap_or_default();
        *self.policy_write() = Some(policy.clone());
        Ok(policy)
    }

    /// 保存平台口令策略（幂等 upsert）
    pub fn set_policy(&self, p: &PasswordPolicy) -> DbResult<()> {
        let json = serde_json::to_string(p)
            .map_err(|e| DbError::Fin(fincore::FinError::msg(format!("策略序列化失败：{e}"))))?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO realm_option(key,value) VALUES('password_policy',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![json],
        )?;
        *self.policy_write() = Some(p.clone());
        Ok(())
    }

    /// 记录一次登录尝试（成功/失败），并只保留最近 500 条防膨胀
    pub fn record_login_attempt(&self, username: &str, ok: bool, ip: &str) -> DbResult<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO realm_login_attempt(username,ts,ok,ip) VALUES(?1,?2,?3,?4)",
            rusqlite::params![username, now(), ok as i64, ip],
        )?;
        conn.execute(
            "DELETE FROM realm_login_attempt WHERE id NOT IN (
                 SELECT id FROM realm_login_attempt ORDER BY id DESC LIMIT 500
             )",
            [],
        )?;
        Ok(())
    }

    /// 自上次成功登录以来的连续失败次数（与桌面 findb::security 同口径）
    pub fn recent_fail_count(&self, username: &str) -> DbResult<i64> {
        let conn = self.conn();
        let last_ok: i64 = conn.query_row(
            "SELECT COALESCE(MAX(id),0) FROM realm_login_attempt WHERE username=?1 AND ok=1",
            rusqlite::params![username],
            |r| r.get(0),
        )?;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM realm_login_attempt WHERE username=?1 AND ok=0 AND id>?2",
            rusqlite::params![username, last_ok],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// 清空某账号的登录尝试记录（登录成功 / 管理员解锁时调用）
    pub fn clear_login_attempts(&self, username: &str) -> DbResult<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM realm_login_attempt WHERE username=?1",
            rusqlite::params![username],
        )?;
        Ok(())
    }

    /// 最近登录记录（可按账号过滤，倒序）——(id, username, ts, ok, ip)
    pub fn login_attempts(
        &self,
        username: Option<&str>,
        limit: usize,
    ) -> DbResult<Vec<(i64, String, String, bool, String)>> {
        let conn = self.conn();
        let limit = limit.clamp(1, 500) as i64;
        let mut out = Vec::new();
        let filter = username.map(str::trim).filter(|s| !s.is_empty());
        if let Some(u) = filter {
            let mut stmt = conn.prepare(
                "SELECT id,username,ts,ok,ip FROM realm_login_attempt
                 WHERE username=?1 ORDER BY id DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![u, limit], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get(4)?,
                ))
            })?;
            for row in rows {
                out.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT id,username,ts,ok,ip FROM realm_login_attempt ORDER BY id DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![limit], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get(4)?,
                ))
            })?;
            for row in rows {
                out.push(row?);
            }
        }
        Ok(out)
    }

    /// 锁定账号 N 分钟（写入持久化截止时间）
    pub fn lock_user(&self, username: &str, minutes: i64) -> DbResult<()> {
        let until = (chrono::Local::now() + chrono::Duration::minutes(minutes.max(1)))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let conn = self.conn();
        conn.execute(
            "UPDATE realm_user SET locked_until=?2 WHERE username=?1",
            rusqlite::params![username, until],
        )?;
        Ok(())
    }

    /// 解锁账号（同时清空失败计数）
    pub fn unlock_user(&self, username: &str) -> DbResult<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE realm_user SET locked_until='' WHERE username=?1",
            rusqlite::params![username],
        )?;
        drop(conn);
        self.clear_login_attempts(username)
    }

    /// 剩余锁定分钟（未锁定/已过期 = 0；过期顺手清锁）
    pub fn lock_remaining_min(&self, username: &str) -> DbResult<i64> {
        let conn = self.conn();
        let raw: String = conn
            .query_row(
                "SELECT COALESCE(locked_until,'') FROM realm_user WHERE username=?1",
                rusqlite::params![username],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_default();
        if raw.trim().is_empty() {
            return Ok(0);
        }
        let remain = chrono::NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%d %H:%M:%S")
            .map(|t| {
                (t - chrono::Local::now().naive_local())
                    .num_minutes()
                    .max(0)
            })
            .unwrap_or(0);
        if remain == 0 {
            conn.execute(
                "UPDATE realm_user SET locked_until='' WHERE username=?1",
                rusqlite::params![username],
            )?;
        }
        Ok(remain)
    }

    /// 仅更新展示名 / 停用状态 / 是否管理员（不改口令）
    pub fn update_user(
        &self,
        username: &str,
        display_name: Option<&str>,
        disabled: Option<bool>,
        is_admin: Option<bool>,
    ) -> DbResult<()> {
        let mut u = self
            .get_user(username)?
            .ok_or_else(|| fincore::FinError::not_found("账号不存在"))?;
        if let Some(d) = display_name {
            u.display_name = d.to_string();
        }
        if let Some(d) = disabled {
            u.disabled = d;
        }
        if let Some(a) = is_admin {
            // 不允许把最后一个管理员改成非管理员，否则无人能管理系统
            if u.is_admin && !a {
                let admins = self
                    .list_users()?
                    .into_iter()
                    .filter(|x| x.is_admin)
                    .count();
                if admins <= 1 {
                    return Err(DbError::Fin(fincore::FinError::msg(
                        "至少保留一个管理员账号",
                    )));
                }
            }
            u.is_admin = a;
        }
        let conn = self.conn();
        conn.execute(
            "UPDATE realm_user SET display_name=?2,disabled=?3,is_admin=?4 WHERE username=?1",
            rusqlite::params![
                username,
                u.display_name,
                u.disabled as i64,
                u.is_admin as i64
            ],
        )?;
        Ok(())
    }

    /// 删除账号。拒绝删除最后一个管理员，避免出现无人能管理系统的状态。
    pub fn delete_user(&self, username: &str) -> DbResult<()> {
        let target = self
            .get_user(username)?
            .ok_or_else(|| fincore::FinError::not_found("账号不存在"))?;
        if target.is_admin {
            let admins = self
                .list_users()?
                .into_iter()
                .filter(|u| u.is_admin)
                .count();
            if admins <= 1 {
                return Err(DbError::Fin(fincore::FinError::msg(
                    "至少保留一个管理员账号",
                )));
            }
        }
        let conn = self.conn();
        conn.execute(
            "DELETE FROM realm_user WHERE username=?1",
            rusqlite::params![username],
        )?;
        Ok(())
    }

    pub fn reset_password(
        &self,
        username: &str,
        new: &str,
        policy: &PasswordPolicy,
    ) -> DbResult<()> {
        policy
            .check(new)
            .map_err(fincore::FinError::msg)
            .map_err(DbError::Fin)?;
        let conn = self.conn();
        // 管理员重置的口令是管理员已知的口令，必须强制用户下次登录自行改一次，
        // 否则口令会长期停留在管理员设定值上，失去重置的意义。
        conn.execute(
            "UPDATE realm_user SET password_hash=?2,must_change_pwd=1 WHERE username=?1",
            rusqlite::params![username, hash_password(new)],
        )?;
        Ok(())
    }

    /// 一个账号最多绑几台设备。
    ///
    /// 为什么是 2 而不是 1：实际场景里同一个人有两台机器是常态（公司台式机 + 家里
    /// 笔记本，或者手机 + 电脑），锁死 1 台的结果是用户隔几天就要找管理员重置一次
    /// 设备绑定 —— 一个安全功能如果天天要人工介入，实际效果就是被绕过。
    ///
    /// 为什么不是"不设上限"：上限是这个功能**全部**的控制力来源。不限台数等于没有
    /// 绑定（口令泄露后攻击者从任何机器都能登）。2 台是"覆盖真实需要"与"保留控制"的交点。
    /// 仍然**不做 LRU 自动挤出** —— 悄悄踢掉用户正在用的那台，比直接拒绝更让人困惑。
    /// 设备绑定：登录时把当前设备登记进来。
    ///
    /// **策略（v1.8.53 起）**：
    /// - 名单为空 → 这是第一次登录，自动登记（否则新装好的系统谁都进不去）；
    /// - 名单里已有这台（按**指纹**比对）→ 只刷新 `last_seen` / `device_name`；
    /// - 名单非空且不认识这台 → **拒绝登录**，换设备必须由管理员在「账号」页重置。
    ///
    /// 原来这里能自动绑到 `MAX_DEVICES_PER_USER` 台，等于「拿着口令的人可以悄悄多绑
    /// 一台自己的设备，然后一直用」。SECURITY_TODO M-4 记的就是这件事 —— `device_id`
    /// 由客户端上报、本来就可伪造，所以能做的不是「验证它」，而是**不给它再扩容的机会**。
    ///
    /// 返回 `Ok(Err(msg))` 而不是 `Err`：拒绝是**业务结论**（要在登录页原样显示给用户
    /// 「找管理员重置」），而 `Err` 是数据库故障，两者混在一起会让运维分不清是不是库坏了。
    pub fn bind_device(
        &self,
        username: &str,
        device_id: &str,
        device_name: &str,
    ) -> DbResult<Result<(), String>> {
        let conn = self.conn();
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let secret = device_secret(&conn)?;
        let fp = device_fingerprint(&secret, username, device_id);
        // 账号不存在要报错（原来查 device_id 列顺带做了这件事，现在要显式查）
        let exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM realm_user WHERE username=?1",
            rusqlite::params![username],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Err(DbError::Fin(fincore::FinError::msg("账号不存在")));
        }
        // 已在名单内（按指纹，兼容指纹为空的存量行按 device_id 兜底）
        let updated = conn.execute(
            "UPDATE realm_user_device SET last_seen=?3, device_name=?4, device_hash=?5
             WHERE username=?1 AND (device_hash=?5 OR (device_hash='' AND device_id=?2))",
            rusqlite::params![username, device_id, now, device_name, fp],
        )?;
        if updated > 0 {
            return Ok(Ok(()));
        }
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM realm_user_device WHERE username=?1",
            rusqlite::params![username],
            |r| r.get(0),
        )?;
        if count > 0 {
            return Ok(Err(format!(
                "这台设备还没有被授权。该账号已在 {count} 台设备上绑定过；\
                 换设备需要管理员在「账号」页点「重置设备绑定」，之后用新设备重新登录。"
            )));
        }
        // 首次绑定：INSERT OR IGNORE 防同一设备并发首登时插入两次
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO realm_user_device(username, device_id, device_name, bound_at, last_seen, device_hash)
             VALUES(?1,?2,?3,?4,?4,?5)",
            rusqlite::params![username, device_id, device_name, now, fp],
        )?;
        if inserted == 0 {
            return Ok(Ok(()));
        }
        // 旧列同步"第一台"，只为账号列表的"已绑定/未绑定"显示（授权判定一律读表）
        conn.execute(
            "UPDATE realm_user SET device_id=?2 WHERE username=?1 AND device_id=''",
            rusqlite::params![username, device_id],
        )?;
        Ok(Ok(()))
    }

    /// 逐请求复核：当前设备是否在该账号的绑定名单内。
    ///
    /// 名单**为空** = 还没绑过 → 放行（登录时的 `bind_device` 会去登记）。
    /// 管理员豁免（`Role::Admin => Perm::all()`，运维账号不该被设备绑定锁死）。
    ///
    /// 这里读表而不是读 `realm_user.device_id` 那一列：那是"第一台"的快照，只为列表显示
    /// 保留同步。用它做授权就退回了"一台机"，而且两处判定会各写一份必然分叉。
    pub fn device_allowed(&self, username: &str, device_id: &str) -> DbResult<bool> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM realm_user_device WHERE username=?1",
            rusqlite::params![username],
            |r| r.get(0),
        )?;
        if count == 0 {
            return Ok(true);
        }
        // 按指纹比对；指纹为空的存量行按 device_id 兜底（升级瞬间的请求不能被踢）
        let secret = device_secret(&conn)?;
        let fp = device_fingerprint(&secret, username, device_id);
        let hit: i64 = conn.query_row(
            "SELECT COUNT(*) FROM realm_user_device
             WHERE username=?1 AND (device_hash=?3 OR (device_hash='' AND device_id=?2))",
            rusqlite::params![username, device_id, fp],
            |r| r.get(0),
        )?;
        Ok(hit > 0)
    }

    /// 该账号已绑定的设备数（账号列表显示"已绑定/未绑定"用）
    pub fn bound_device_count(&self, username: &str) -> DbResult<i64> {
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM realm_user_device WHERE username=?1",
            rusqlite::params![username],
            |r| r.get(0),
        )?)
    }

    /// 解绑全部设备（管理员「重置设备」）：清空后该账号可在任意设备重新登录
    pub fn clear_device(&self, username: &str) -> DbResult<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM realm_user_device WHERE username=?1",
            rusqlite::params![username],
        )?;
        // 旧列一并清掉，否则列表会显示"已绑定"但实际没绑
        conn.execute(
            "UPDATE realm_user SET device_id='' WHERE username=?1",
            rusqlite::params![username],
        )?;
        Ok(())
    }
    /// 修改自身口令（需校验旧口令）
    pub fn change_password(
        &self,
        username: &str,
        old: &str,
        new: &str,
        policy: &PasswordPolicy,
    ) -> DbResult<Result<(), String>> {
        let u = self
            .get_user(username)?
            .ok_or_else(|| fincore::FinError::not_found("账号不存在"))?;
        if !verify_password(old, &u.password_hash) {
            return Ok(Err("原口令不正确".to_string()));
        }
        if let Err(msg) = policy.check(new) {
            return Ok(Err(msg));
        }
        let conn = self.conn();
        conn.execute(
            "UPDATE realm_user SET password_hash=?2,must_change_pwd=0 WHERE username=?1",
            rusqlite::params![username, hash_password(new)],
        )?;
        Ok(Ok(()))
    }

    // ---------------------------------------------------------------
    // 账套目录
    // ---------------------------------------------------------------

    pub fn register_book(
        &self,
        key: &str,
        path: &str,
        owner_username: &str,
        company: &str,
    ) -> DbResult<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO realm_book(key,path,owner_username,company,created_at) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![key, path, owner_username, company, now()],
        )?;
        Ok(())
    }

    /// 最早创建的（未停用）管理员：账号模型二元化后的唯一治理者候选
    pub fn first_admin(&self) -> DbResult<Option<(String, String)>> {
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT username, display_name FROM realm_user
                 WHERE is_admin=1 AND disabled=0 ORDER BY id LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// 存量账套归属接管（账号模型二元化，幂等，每次启动可执行）：
    /// owner 非管理员的账套 → 归最早创建的管理员；owner 已是任意管理员的不动
    /// （多管理员各自归属保留）。返回被接管的 (key, path) 列表供调用方补套内成员。
    pub fn reassign_books_to_admin(&self) -> DbResult<Vec<(String, String)>> {
        let Some((admin, _)) = self.first_admin()? else {
            return Ok(Vec::new()); // 尚未引导管理员时跳过
        };
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT key, path FROM realm_book
             WHERE owner_username NOT IN (SELECT username FROM realm_user WHERE is_admin=1)",
        )?;
        let rows: Vec<(String, String)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(st);
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        conn.execute(
            "UPDATE realm_book SET owner_username=?1
             WHERE owner_username NOT IN (SELECT username FROM realm_user WHERE is_admin=1)",
            [&admin],
        )?;
        Ok(rows)
    }

    pub fn get_book(&self, key: &str) -> DbResult<Option<RealmBook>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id,key,path,owner_username,company,created_at FROM realm_book WHERE key=?1",
            rusqlite::params![key],
            map_book,
        )
        .optional()
        .map_err(Into::into)
    }

    /// 按归属返回账套列表：管理员返回全部，普通用户只返回自己创建的
    pub fn list_books_for(&self, username: &str, is_admin: bool) -> DbResult<Vec<RealmBook>> {
        let conn = self.conn();
        let sql: &str = if is_admin {
            "SELECT id,key,path,owner_username,company,created_at FROM realm_book ORDER BY id"
        } else {
            "SELECT id,key,path,owner_username,company,created_at FROM realm_book WHERE owner_username=?1 ORDER BY id"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = if is_admin {
            stmt.query_map([], map_book)?
        } else {
            stmt.query_map(rusqlite::params![username], map_book)?
        };
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn book_owner(&self, key: &str) -> DbResult<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT owner_username FROM realm_book WHERE key=?1",
                rusqlite::params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn delete_book(&self, key: &str) -> DbResult<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM realm_book WHERE key=?1",
            rusqlite::params![key],
        )?;
        Ok(())
    }

    pub fn count_books_of(&self, username: &str) -> DbResult<i64> {
        let conn = self.conn();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM realm_book WHERE owner_username=?1",
            rusqlite::params![username],
            |r| r.get(0),
        )?)
    }

    /// 设置平台账号的「首登改密」开关。
    ///
    /// 为什么需要单独一个方法：登录门禁 `post_login` 读的就是
    /// `realm_user.must_change_pwd` 这一位，账套内的 `user.must_change_pwd`
    /// 只影响账套内的记录、**完全不参与登录判断**。于是管理员在账套里建号时
    /// 明确传 `must_change_pwd:false`（"这人是我设的口令，不用再改"），用户
    /// 仍被拦在「必须先改密」上。实测确认过这条不一致。
    pub fn set_must_change_pwd(&self, username: &str, v: bool) -> DbResult<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE realm_user SET must_change_pwd=?2 WHERE username=?1",
            rusqlite::params![username, v as i64],
        )?;
        Ok(())
    }

    // ---- 开放 API 密钥 ----

    /// 签发一把密钥。**明文只在返回值里出现一次**，库里只留哈希。
    ///
    /// 明文为什么用 32 字节随机而不是可读的 token：密钥要能被外部系统长期持有
    /// 在配置文件/环境变量里，不可读反而减少「被人瞄一眼」的机会；定位问题靠
    /// `prefix` 字段，不是靠密钥本身。
    pub fn api_key_create(
        &self,
        name: &str,
        book_key: &str,
        scopes: &str,
        expires_at: &str,
        who: &str,
    ) -> DbResult<(i64, String)> {
        let mut raw = [0u8; 32];
        rand::Rng::fill(&mut rand::thread_rng(), &mut raw);
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let secret = format!("fbk_{hex}");
        // 存哈希：不能用 sha256 明文比（key 长度固定，直接哈希即可），也不存明文
        let hash = hash_api_secret(&secret);
        let prefix: String = secret.chars().take(12).collect();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO realm_api_key(name,prefix,key_hash,book_key,scopes,created_by,created_at,expires_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                name.trim(),
                prefix,
                hash,
                book_key.trim(),
                scopes.trim(),
                who,
                now(),
                expires_at.trim()
            ],
        )?;
        Ok((conn.last_insert_rowid(), secret))
    }

    pub fn api_key_list(&self) -> DbResult<Vec<ApiKeyInfo>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id,name,prefix,book_key,scopes,disabled,created_by,created_at,last_used_at,expires_at
             FROM realm_api_key ORDER BY id DESC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(ApiKeyInfo {
                id: r.get(0)?,
                name: r.get(1)?,
                prefix: r.get(2)?,
                book_key: r.get(3)?,
                scopes: r.get(4)?,
                disabled: r.get::<_, i64>(5)? != 0,
                created_by: r.get(6)?,
                created_at: r.get(7)?,
                last_used_at: r.get(8)?,
                expires_at: r.get(9)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// 把密钥表整表拼成文本，用它断言「库里没有明文」。
    ///
    /// 为什么用 dump 而不去查具体字段：`api_key_list` 已经在结构上排除了
    /// 明文，用它来证明「没存明文」是循环论证。真正要验的是**表里那一列
    /// 的实际内容**，所以直接把整行列出来——包括将来可能新增的列。
    ///
    /// 公开可见：集成测试（tests/api.rs）是独立 crate，拿不到 `#[cfg(test)]`
    /// 的内部项，而「库里有没有明文」恰恰是最该被集成测试盯住的一条。
    pub fn debug_dump_api_keys(&self) -> String {
        let conn = self.conn();
        let mut st = conn
            .prepare("SELECT * FROM realm_api_key")
            .expect("准备 dump 失败");
        let cols: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let mut out = cols.join("|");
        out.push('\n');
        let mut rows = st.query([]).expect("查询密钥表失败");
        while let Some(row) = rows.next().expect("迭代密钥行失败") {
            for (i, c) in cols.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                out.push_str(&format!(
                    "{c}={:?}",
                    row.get::<_, rusqlite::types::Value>(i)
                ));
            }
            out.push('\n');
        }
        out
    }

    /// 停用 / 启用（不物理删除：审计需要知道「曾经有过」）
    pub fn api_key_set_disabled(&self, id: i64, disabled: bool) -> DbResult<()> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE realm_api_key SET disabled=?2 WHERE id=?1",
            rusqlite::params![id, disabled as i64],
        )?;
        if n == 0 {
            return Err(fincore::FinError::not_found("密钥不存在").into());
        }
        Ok(())
    }

    pub fn api_key_delete(&self, id: i64) -> DbResult<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM realm_api_key WHERE id=?1", [id])?;
        Ok(())
    }

    /// 校验一把密钥，返回其身份。**每次调用都重新算哈希**——
    /// 密钥用哈希存，鉴权就只能靠比对哈希，没法像 session 那样查表比对明文。
    ///
    /// 顺带更新 `last_used_at`：排查「到底是谁在调」时唯一的线索。
    pub fn api_key_verify(&self, secret: &str) -> DbResult<Option<ApiKeyAuth>> {
        if !secret.starts_with("fbk_") || secret.len() < 20 {
            return Ok(None);
        }
        let hash = hash_api_secret(secret);
        let conn = self.conn();
        let row = conn
            .query_row(
                "SELECT id,prefix,book_key,scopes,disabled,expires_at FROM realm_api_key WHERE key_hash=?1",
                rusqlite::params![hash],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, prefix, book_key, scopes, disabled, expires)) = row else {
            return Ok(None);
        };
        if disabled != 0 {
            return Ok(None);
        }
        // 过期判断：expires_at 非空且已过 → 失效。空 = 永不过期。
        if !expires.trim().is_empty() {
            if let Ok(exp) =
                chrono::NaiveDateTime::parse_from_str(expires.trim(), "%Y-%m-%d %H:%M:%S")
            {
                if exp < chrono::Local::now().naive_local() {
                    return Ok(None);
                }
            }
        }
        let _ = conn.execute(
            "UPDATE realm_api_key SET last_used_at=?2 WHERE id=?1",
            rusqlite::params![id, now()],
        );
        Ok(Some(ApiKeyAuth {
            id,
            prefix,
            book_key,
            scopes,
        }))
    }

    /// 统计该用户名当前出现在多少个账套的用户表中（含自建账套）。
    ///
    /// 供"账套管理员重置成员口令"做越权防护：只有仅属于当前账套的成员
    /// 才允许由账套管理员重置平台口令，否则可借"邀请 + 重置"接管他人账号。
    pub fn count_books_containing(&self, books_dir: &Path, username: &str) -> DbResult<usize> {
        let paths = self.load_all_book_paths()?;
        let dir = books_dir.canonicalize().ok();
        let mut n = 0usize;
        for p in paths {
            if !p.exists() {
                continue;
            }
            // 与 sync_password_to_books 相同：仅扫描指定目录下的账套
            if let Some(dir) = &dir {
                match p.canonicalize() {
                    Ok(pc) if pc.starts_with(dir) => {}
                    _ => continue,
                }
            }
            match Db::open(&p) {
                Ok(db) => {
                    if matches!(users::get(&db, username), Ok(Some(_))) {
                        n += 1;
                    }
                }
                Err(e) => error!(book = %p.display(), error = %e, "扫描账套失败"),
            }
        }
        Ok(n)
    }

    /// 启动时把目录全量载入（返回 path，供注册进 BookRegistry）
    pub fn load_all_book_paths(&self) -> DbResult<Vec<PathBuf>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT path FROM realm_book ORDER BY id")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().map(PathBuf::from).collect())
    }

    /// 把平台口令同步到该用户出现过的**所有账套**（自建 + 被邀请加入的）。
    ///
    /// 单密码统一后账套内不再设独立口令：账套内同名用户行只是平台口令的镜像
    /// （供桌面端直连 .fbk 登录用）。同步时一并同步改密标志并解除锁定，
    /// 与旧"账套内重置"语义对齐。缺行则跳过（管理员看他人账套是临时身份，
    /// 不留行）；单个账套失败只记日志，不影响其他账套。
    pub fn sync_password_to_books(
        &self,
        books_dir: &Path,
        username: &str,
        new_hash: &str,
        must_change: bool,
    ) -> DbResult<()> {
        let paths = self.load_all_book_paths()?;
        for p in paths {
            if !p.exists() {
                continue;
            }
            // 仅当账套目录在指定目录下（防越权改到其他位置的库）
            if let Ok(dir) = books_dir.canonicalize() {
                if let Ok(pc) = p.canonicalize() {
                    if !pc.starts_with(&dir) {
                        continue;
                    }
                }
            }
            let db = match Db::open(&p) {
                Ok(db) => db,
                Err(e) => {
                    error!(book = %p.display(), error = %e, "同步口令：账套打不开");
                    continue;
                }
            };
            match users::get(&db, username) {
                Ok(Some(mut u)) => {
                    u.password_hash = new_hash.to_string();
                    u.must_change_pwd = must_change;
                    u.locked_until = None;
                    if let Err(e) = users::update(&db, &u) {
                        error!(book = %p.display(), error = %e, "同步口令：写账套失败");
                        continue;
                    }
                    if let Err(e) = findb::security::clear_attempts(&db, Some(username)) {
                        error!(book = %p.display(), error = %e, "同步口令：清失败计数失败");
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    error!(book = %p.display(), error = %e, "同步口令：读账套失败");
                }
            }
        }
        Ok(())
    }
}

fn map_user(r: &rusqlite::Row) -> rusqlite::Result<RealmUser> {
    Ok(RealmUser {
        id: r.get(0)?,
        username: r.get(1)?,
        display_name: r.get(2)?,
        password_hash: r.get(3)?,
        is_admin: r.get::<_, i64>(4)? != 0,
        disabled: r.get::<_, i64>(5)? != 0,
        must_change_pwd: r.get::<_, i64>(6)? != 0,
        device_id: r.get(7)?,
        created_at: r.get(8)?,
        locked_until: r.get(9)?,
    })
}

fn map_book(r: &rusqlite::Row) -> rusqlite::Result<RealmBook> {
    Ok(RealmBook {
        id: r.get(0)?,
        key: r.get(1)?,
        path: r.get(2)?,
        owner_username: r.get(3)?,
        company: r.get(4)?,
        created_at: r.get(5)?,
    })
}

/// 生成 16 位强口令（大小写字母 + 数字）
fn generate_password() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

/// 身份对账：确保账套内存在该平台用户对应的 `user` 行（角色 Admin）。
///
/// - 普通用户自建账套时已被 [`crate::handlers::create_book`] 种子为 Admin；
/// - 管理员查看他人账套时，首次进入自动以其全局口令哈希种子一行 Admin。
///   之后账套引擎的权限/数据范围/设备绑定逻辑对这行用户照常生效。
pub fn ensure_book_admin(db: &Db, ru: &RealmUser) -> DbResult<()> {
    if users::get(db, &ru.username)?.is_none() {
        let mut u = User::new(&ru.username, &ru.display_name, Role::Admin);
        u.password_hash = ru.password_hash.clone();
        u.must_change_pwd = false;
        users::insert(db, &u)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_realm(tag: &str) -> (RealmDb, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("fb-realm-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("realm.db");
        let r = RealmDb::open(&path).unwrap();
        r.create_user(
            "u1",
            "用户一",
            "Init@123456",
            false,
            false,
            &PasswordPolicy::default(),
        )
        .unwrap();
        (r, path)
    }

    /// v1.8.53 的核心政策：**第一次登录自动登记，之后不再自动放新设备进来**。
    ///
    /// 原来的行为是「自动绑到 2 台」—— 拿到口令的人可以悄悄多绑一台自己的设备，
    /// 然后长期用；用户自己完全看不出来（列表里那一行看起来跟自己的手机一样）。
    #[test]
    fn first_bind_is_automatic_but_a_second_device_is_refused() {
        let (r, _p) = tmp_realm("freeze");
        // 第一次：名单为空 → 自动登记
        assert_eq!(r.bind_device("u1", "dev-A", "办公室电脑").unwrap(), Ok(()));
        assert!(r.device_allowed("u1", "dev-A").unwrap());
        // 第二台：拒绝，并给出「找管理员重置」的下一步
        let second = r.bind_device("u1", "dev-B", "家里的电脑").unwrap();
        let msg = second.expect_err("第二台设备必须被拒绝");
        assert!(msg.contains("重置设备绑定"), "提示里要写清怎么办：{msg}");
        assert!(!r.device_allowed("u1", "dev-B").unwrap());
        assert_eq!(r.bound_device_count("u1").unwrap(), 1);
    }

    /// 同一台设备再登录只刷新时间，不新增行 —— 否则每次登录都会把名单撑爆。
    #[test]
    fn rebinding_the_same_device_refreshes_instead_of_growing() {
        let (r, _p) = tmp_realm("refresh");
        r.bind_device("u1", "dev-A", "办公室电脑").unwrap().unwrap();
        r.bind_device("u1", "dev-A", "办公室电脑（Chrome 131）")
            .unwrap()
            .unwrap();
        assert_eq!(r.bound_device_count("u1").unwrap(), 1);
    }

    /// 管理员重置之后，新设备能重新自动登记（否则换电脑就只能换人）。
    #[test]
    fn reset_lets_a_new_device_in() {
        let (r, _p) = tmp_realm("reset");
        r.bind_device("u1", "dev-A", "旧电脑").unwrap().unwrap();
        assert!(r.bind_device("u1", "dev-B", "新电脑").unwrap().is_err());
        r.clear_device("u1").unwrap();
        assert_eq!(r.bound_device_count("u1").unwrap(), 0);
        assert_eq!(r.bind_device("u1", "dev-B", "新电脑").unwrap(), Ok(()));
        assert!(r.device_allowed("u1", "dev-B").unwrap());
        // 旧设备从此不在名单里
        assert!(!r.device_allowed("u1", "dev-A").unwrap());
    }

    /// 锁中毒后**不能连带把登录打死**。
    ///
    /// `Mutex::lock().unwrap()` 的老写法有个连锁反应：任一处持锁时 panic，锁就中毒，
    /// 之后每一次 `.lock().unwrap()` 都 panic —— 本进程里所有登录/改密/开号全部失效，
    /// 只能重启。一次局部 bug 变成永久不可用。
    ///
    /// 这里的锁只保护一条 SQLite 连接，一致性由 SQLite 事务保证，所以毒化后继续用
    /// 是安全的。这条测试故意在持锁时 panic 把锁打成中毒，然后断言登录链路照常工作。
    #[test]
    fn a_poisoned_lock_does_not_kill_login() {
        let (r, _p) = tmp_realm("poison");
        r.create_user(
            "pz1",
            "张三",
            "Pass@1234",
            false,
            false,
            &PasswordPolicy::default(),
        )
        .unwrap();

        // 在同一线程作用域里制造一次"持锁 panic"：join 掉结果，不让它传播
        std::thread::scope(|s| {
            let h = s.spawn(|| {
                let _g = r.inner.lock().unwrap();
                panic!("故意 panic，把锁打成中毒");
            });
            assert!(h.join().is_err(), "子线程应当 panic 了");
        });

        // 锁现在是中毒状态：老写法在这里会 panic
        assert!(
            r.get_user("pz1").unwrap().is_some(),
            "毒化后仍应读得到用户"
        );
        assert!(
            r.authenticate("pz1", "Pass@1234").unwrap().is_some(),
            "毒化后仍应能登录"
        );
        // 写也一样
        r.create_user(
            "pz2",
            "李四",
            "Pass@1234",
            false,
            false,
            &PasswordPolicy::default(),
        )
        .unwrap();
        assert!(r.get_user("pz2").unwrap().is_some());
    }

    /// 名单里存的是指纹，不是 `device_id` 原文。
    ///
    /// 这不是主要防线（`device_id` 终究由客户端上报），但它让「读到库的人」不能
    /// 把某一行抄出去当通行证用；同时换了 realm（换盐）同一台设备的指纹也对不上。
    #[test]
    fn binding_rows_hold_a_fingerprint_not_the_raw_device_id() {
        let (r, _p) = tmp_realm("hash");
        r.bind_device("u1", "dev-A", "办公室电脑").unwrap().unwrap();
        let conn = r.inner.lock().unwrap();
        let (did, hash): (String, String) = conn
            .query_row(
                "SELECT device_id, device_hash FROM realm_user_device WHERE username='u1'",
                [],
                |x| Ok((x.get(0)?, x.get(1)?)),
            )
            .unwrap();
        assert_eq!(did, "dev-A", "原值保留只为「重置」与排障时能对上人");
        assert_eq!(hash.len(), 64, "sha256 十六进制是 64 位");
        assert_ne!(hash, "dev-A");
        // 同一输入必须稳定（否则每次登录都匹配不上，全员被锁）
        drop(conn);
        assert!(r.device_allowed("u1", "dev-A").unwrap());
    }

    /// 旧库的「单列 device_id」必须在打开时搬进 realm_user_device。
    ///
    /// 不搬的后果**不报任何错**：升级后所有账号的设备绑定静默消失，等于给全员
    /// 解绑 —— 症状是「谁都能登」，而绑定相关的界面看起来一切正常。
    /// 这条测试直接构造旧库形态（单列有值、集合表空），再关掉重开。
    #[test]
    fn legacy_single_column_device_binding_is_migrated() {
        let dir = std::env::temp_dir().join(format!("fb-realm-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("realm.db");

        let r = RealmDb::open(&path).unwrap();
        r.create_user(
            "u1",
            "用户一",
            "Init@123456",
            false,
            false,
            &PasswordPolicy::default(),
        )
        .unwrap();

        // 把库造成「升级前的样子」：单列有绑定，集合表里没有。
        {
            let conn = r.inner.lock().unwrap();
            conn.execute("DELETE FROM realm_user_device", []).unwrap();
            conn.execute(
                "UPDATE realm_user SET device_id = 'dev-legacy' WHERE username = 'u1'",
                [],
            )
            .unwrap();
        }
        drop(r);

        let r2 = RealmDb::open(&path).unwrap();
        // 先看集合表本身：`device_allowed` 在「一台都没绑」时是放行的，
        // 只看它会把「没搬过来」当成「正常」。
        assert_eq!(
            r2.bound_device_count("u1").unwrap(),
            1,
            "打开旧库时没把单列 device_id 搬进 realm_user_device —— \
             升级后全部账号静默解绑，且不报任何错"
        );
        assert!(
            r2.device_allowed("u1", "dev-legacy").unwrap(),
            "旧库里已有的设备绑定必须在打开时搬进 realm_user_device —— \
             不搬等于升级后给全部账号静默解绑，且不会报错"
        );
        // 搬过来的行也必须补上指纹：光有 device_id 的行只能靠兜底路径匹配，
        // 而兜底是为「升级那一瞬间的请求」留的，不是常态。指纹为空 = 这行没被回填。
        {
            let conn = r2.inner.lock().unwrap();
            let hash: String = conn
                .query_row(
                    "SELECT device_hash FROM realm_user_device WHERE username='u1'",
                    [],
                    |x| x.get(0),
                )
                .unwrap();
            assert_eq!(
                hash.len(),
                64,
                "存量绑定行没有回填设备指纹 —— 指纹为空的行只能靠 device_id 兜底匹配"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
