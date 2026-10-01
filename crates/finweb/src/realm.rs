//! 平台身份层（realm）
//!
//! 独立于账套（`.fbk`）的一层全局存储，负责两件事：
//! 1. **全局账号** `realm_user`：能登录 Web 的"账号"。管理员与普通用户都是这里面的账号。
//! 2. **账套目录** `realm_book`：每个账套一条记录，记录 `key`（文件名）、`path`、`owner_username`（创建者）。
//!
//! 账套内部仍用 `.fbk` 自带的 `user` 表做授权；进入账套时由 `state::CurrentUser` 按本文件的
//! 归属关系做"授权 + 身份对账"（见 [`ensure_book_admin`]），从而完整复用账套引擎的权限/设备绑定逻辑。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use findb::{users, Db, DbError, DbResult};
use fincore::user::{burn_argon2, hash_password, verify_password, PasswordPolicy, Role, User};
use rusqlite::{Connection, OptionalExtension};

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
    /// 绑定的设备指纹（Web 端"一人一机"）；空 = 尚未绑定，下次登录自动绑定
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
"#;

/// 账号库（单进程内以 Mutex<Connection> 持有，WAL 模式下并发安全）
pub struct RealmDb {
    inner: Mutex<Connection>,
    path: PathBuf,
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
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
        };
        db.init()?;
        Ok(db)
    }

    /// 库文件路径
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn init(&self) -> DbResult<()> {
        self.inner.lock().unwrap().execute_batch(SCHEMA)?;
        self.migrate()?;
        Ok(())
    }

    /// 轻量迁移：为早期创建的库补上后加的列（device_id / locked_until）
    fn migrate(&self) -> DbResult<()> {
        let conn = self.inner.lock().unwrap();
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
        Ok(())
    }

    // ---------------------------------------------------------------
    // 账号
    // ---------------------------------------------------------------

    pub fn count_users(&self) -> DbResult<i64> {
        let conn = self.inner.lock().unwrap();
        Ok(conn.query_row("SELECT COUNT(*) FROM realm_user", [], |r| r.get(0))?)
    }

    /// 首次启动引导：若没有任何账号，则创建管理员。
    /// - `admin_pass` 非空：用给定口令创建（运维通过环境变量注入，推荐）。
    /// - 否则：自动生成强口令，调用方负责打印一次性凭据。
    /// 返回 `(username, password)`：若管理员已存在则返回 `None`。
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
        let conn = self.inner.lock().unwrap();
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
                        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,username,display_name,password_hash,is_admin,disabled,must_change_pwd,device_id,created_at,locked_until
             FROM realm_user ORDER BY id",
        )?;
        let rows = stmt.query_map([], map_user)?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---------------------------------------------------------------
    // 平台安全：口令策略 / 登录审计 / 账号锁定（Web 安全中心）
    // ---------------------------------------------------------------

    /// 平台口令策略（realm_option 存储；缺省 = 与桌面端一致的默认策略）
    pub fn policy(&self) -> DbResult<PasswordPolicy> {
        let conn = self.inner.lock().unwrap();
        let raw: Option<String> = conn
            .query_row(
                "SELECT value FROM realm_option WHERE key='password_policy'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw
            .and_then(|s| serde_json::from_str::<PasswordPolicy>(&s).ok())
            .unwrap_or_default())
    }

    /// 保存平台口令策略（幂等 upsert）
    pub fn set_policy(&self, p: &PasswordPolicy) -> DbResult<()> {
        let json = serde_json::to_string(p)
            .map_err(|e| DbError::Fin(fincore::FinError::msg(format!("策略序列化失败：{e}"))))?;
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO realm_option(key,value) VALUES('password_policy',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![json],
        )?;
        Ok(())
    }

    /// 记录一次登录尝试（成功/失败），并只保留最近 500 条防膨胀
    pub fn record_login_attempt(&self, username: &str, ok: bool, ip: &str) -> DbResult<()> {
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE realm_user SET locked_until=?2 WHERE username=?1",
            rusqlite::params![username, until],
        )?;
        Ok(())
    }

    /// 解锁账号（同时清空失败计数）
    pub fn unlock_user(&self, username: &str) -> DbResult<()> {
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE realm_user SET locked_until='' WHERE username=?1",
            rusqlite::params![username],
        )?;
        drop(conn);
        self.clear_login_attempts(username)
    }

    /// 剩余锁定分钟（未锁定/已过期 = 0；过期顺手清锁）
    pub fn lock_remaining_min(&self, username: &str) -> DbResult<i64> {
        let conn = self.inner.lock().unwrap();
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
            .map(|t| (t - chrono::Local::now().naive_local()).num_minutes().max(0))
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
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE realm_user SET display_name=?2,disabled=?3,is_admin=?4 WHERE username=?1",
            rusqlite::params![username, u.display_name, u.disabled as i64, u.is_admin as i64],
        )?;
        Ok(())
    }

    /// 删除账号。拒绝删除最后一个管理员，避免出现无人能管理系统的状态。
    pub fn delete_user(&self, username: &str) -> DbResult<()> {
        let target = self
            .get_user(username)?
            .ok_or_else(|| fincore::FinError::not_found("账号不存在"))?;
        if target.is_admin {
            let admins = self.list_users()?.into_iter().filter(|u| u.is_admin).count();
            if admins <= 1 {
                return Err(DbError::Fin(fincore::FinError::msg(
                    "至少保留一个管理员账号",
                )));
            }
        }
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "DELETE FROM realm_user WHERE username=?1",
            rusqlite::params![username],
        )?;
        Ok(())
    }

    pub fn reset_password(&self, username: &str, new: &str, policy: &PasswordPolicy) -> DbResult<()> {
        policy
            .check(new)
            .map_err(fincore::FinError::msg)
            .map_err(DbError::Fin)?;
        let conn = self.inner.lock().unwrap();
        // 管理员重置的口令是管理员已知的口令，必须强制用户下次登录自行改一次，
        // 否则口令会长期停留在管理员设定值上，失去重置的意义。
        conn.execute(
            "UPDATE realm_user SET password_hash=?2,must_change_pwd=1 WHERE username=?1",
            rusqlite::params![username, hash_password(new)],
        )?;
        Ok(())
    }

    /// 设备绑定（Web 端"一人一机"）：首次登录自动绑定当前设备。
    /// 单次持锁内完成"读-判-写"，并用条件 UPDATE 兜底，
    /// 避免两个设备几乎同时首次登录时互相覆盖绑定。
    /// 返回 Result：Ok = 绑定成功或无绑定；Err(msg) = 该账号已绑定其它设备。
    pub fn bind_device(&self, username: &str, device_id: &str) -> DbResult<Result<(), String>> {
        let conn = self.inner.lock().unwrap();
        let cur: Option<String> = conn
            .query_row(
                "SELECT device_id FROM realm_user WHERE username=?1",
                rusqlite::params![username],
                |r| r.get(0),
            )
            .optional()?;
        let cur = match cur {
            Some(c) => c,
            None => {
                return Err(DbError::Fin(fincore::FinError::msg("账号不存在")));
            }
        };
        if !cur.is_empty() {
            return if cur == device_id {
                Ok(Ok(()))
            } else {
                Ok(Err("该账号已绑定其它设备，如需更换请联系管理员重置设备".to_string()))
            };
        }
        // 条件更新：仅当仍为空时写入，并发首登不会互相覆盖
        let n = conn.execute(
            "UPDATE realm_user SET device_id=?2 WHERE username=?1 AND device_id=''",
            rusqlite::params![username, device_id],
        )?;
        Ok(if n == 1 {
            Ok(())
        } else {
            // 竞态下另一请求抢先绑定：回读最新值判定
            let now: String = conn.query_row(
                "SELECT device_id FROM realm_user WHERE username=?1",
                rusqlite::params![username],
                |r| r.get(0),
            )?;
            if now == device_id {
                Ok(())
            } else {
                Err("该账号已绑定其它设备，如需更换请联系管理员重置设备".to_string())
            }
        })
    }

    /// 解绑设备（管理员「重置设备」）：清空后该账号下次登录自动重新绑定
    pub fn clear_device(&self, username: &str) -> DbResult<()> {
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO realm_book(key,path,owner_username,company,created_at) VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![key, path, owner_username, company, now()],
        )?;
        Ok(())
    }

    /// 最早创建的（未停用）管理员：账号模型二元化后的唯一治理者候选
    pub fn first_admin(&self) -> DbResult<Option<(String, String)>> {
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
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
        let conn = self.inner.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT owner_username FROM realm_book WHERE key=?1",
                rusqlite::params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn delete_book(&self, key: &str) -> DbResult<()> {
        let conn = self.inner.lock().unwrap();
        conn.execute("DELETE FROM realm_book WHERE key=?1", rusqlite::params![key])?;
        Ok(())
    }

    pub fn count_books_of(&self, username: &str) -> DbResult<i64> {
        let conn = self.inner.lock().unwrap();
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM realm_book WHERE owner_username=?1",
            rusqlite::params![username],
            |r| r.get(0),
        )?)
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
                Err(e) => eprintln!("[realm] 扫描账套失败 {}: {e}", p.display()),
            }
        }
        Ok(n)
    }

    /// 启动时把目录全量载入（返回 path，供注册进 BookRegistry）
    pub fn load_all_book_paths(&self) -> DbResult<Vec<PathBuf>> {
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare("SELECT path FROM realm_book ORDER BY id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
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
            if let Some(dir) = books_dir.canonicalize().ok() {
                if let Some(pc) = p.canonicalize().ok() {
                    if !pc.starts_with(&dir) {
                        continue;
                    }
                }
            }
            let db = match Db::open(&p) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("[realm] 同步口令打不开账套 {}: {e}", p.display());
                    continue;
                }
            };
            match users::get(&db, username) {
                Ok(Some(mut u)) => {
                    u.password_hash = new_hash.to_string();
                    u.must_change_pwd = must_change;
                    u.locked_until = None;
                    if let Err(e) = users::update(&db, &u) {
                        eprintln!("[realm] 同步口令写账套失败 {}: {e}", p.display());
                        continue;
                    }
                    if let Err(e) = findb::security::clear_attempts(&db, Some(username)) {
                        eprintln!("[realm] 同步口令清失败计数失败 {}: {e}", p.display());
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("[realm] 同步口令读账套失败 {}: {e}", p.display());
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
/// 之后账套引擎的权限/数据范围/设备绑定逻辑对这行用户照常生效。
pub fn ensure_book_admin(db: &Db, ru: &RealmUser) -> DbResult<()> {
    if users::get(db, &ru.username)?.is_none() {
        let mut u = User::new(&ru.username, &ru.display_name, Role::Admin);
        u.password_hash = ru.password_hash.clone();
        u.must_change_pwd = false;
        users::insert(db, &u)?;
    }
    Ok(())
}
