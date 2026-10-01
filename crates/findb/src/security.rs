//! 账户安全：登录失败锁定、口令策略、会话空闲超时
//!
//! 单机软件做这些不是形式主义——代账会计一台机器上挂着几十个账套，
//! 客户把电脑借给别人用时，"锁 15 分钟 + 90 天改一次口令"就是最实用的那道闸门。
//! 口令只存加盐摘要，永不回显明文。

use chrono::{NaiveDate, NaiveDateTime};
use fincore::user::{PasswordPolicy, User};
use fincore::FinError;

use crate::{Db, DbResult};

/// 登录失败计数窗口（分钟）：独立于锁定时长，默认 10 分钟
/// 防止 lock_minutes×60 的误用导致窗口变成 15 小时
pub const LOCK_WINDOW_MIN: i64 = 10;

/// 一条登录尝试
#[derive(Clone, Debug)]
pub struct Attempt {
    pub id: i64,
    pub username: String,
    pub ts: String,
    pub ok: bool,
}

fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn log_attempt(db: &Db, username: &str, ok: bool) -> DbResult<()> {
    let tx = db.write_tx()?;
    tx.execute(
        "INSERT INTO login_attempt(username, ts, ok) VALUES(?1,?2,?3)",
        rusqlite::params![username, now_str(), ok as i64],
    )?;
    // 只保留最近 500 条，避免日志表无限增长（与插入同事务，防裁剪失败后表无限涨）
    tx.execute(
        "DELETE FROM login_attempt WHERE id NOT IN (
            SELECT id FROM login_attempt ORDER BY id DESC LIMIT 500
         )",
        [],
    )?;
    tx.commit()?;
    Ok(())
}

/// 最近 `within_min` 分钟内的连续失败次数（遇到一次成功就清零）
pub fn recent_fails(db: &Db, username: &str, within_min: i64) -> DbResult<i64> {
    let since = (chrono::Local::now().naive_local() - chrono::Duration::minutes(within_min))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    // 按 id 而不是时间戳划界：同一秒内的登录/失败时间戳完全相同，用 ts>? 会漏算
    let last_ok: Option<i64> = db
        .conn()
        .query_row(
            "SELECT MAX(id) FROM login_attempt WHERE username=?1 AND ok=1 AND ts>=?2",
            rusqlite::params![username, since],
            |r| r.get(0),
        )
        .unwrap_or(None);
    let n: i64 = match last_ok {
        Some(id) => db.conn().query_row(
            "SELECT COUNT(*) FROM login_attempt WHERE username=?1 AND ok=0 AND id>?2",
            rusqlite::params![username, id],
            |r| r.get(0),
        )?,
        None => db.conn().query_row(
            "SELECT COUNT(*) FROM login_attempt WHERE username=?1 AND ok=0 AND ts>=?2",
            rusqlite::params![username, since],
            |r| r.get(0),
        )?,
    };
    Ok(n)
}

pub fn attempts(db: &Db, username: Option<&str>, limit: i64) -> DbResult<Vec<Attempt>> {
    let (sql, has_user) = match username {
        Some(_) => (
            "SELECT id,username,ts,ok FROM login_attempt WHERE username=?1 ORDER BY id DESC LIMIT ?2",
            true,
        ),
        None => (
            "SELECT id,username,ts,ok FROM login_attempt ORDER BY id DESC LIMIT ?1",
            false,
        ),
    };
    let mut st = db.conn().prepare(sql)?;
    let rows = if has_user {
        st.query_map(
            rusqlite::params![username.unwrap_or(""), limit],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)? != 0)),
        )?
        .collect::<Result<Vec<_>, _>>()?
    } else {
        st.query_map(rusqlite::params![limit], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)? != 0))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    Ok(rows
        .into_iter()
        .map(|(id, username, ts, ok)| Attempt { id, username, ts, ok })
        .collect())
}

pub fn clear_attempts(db: &Db, username: Option<&str>) -> DbResult<usize> {
    clear_attempts_on(db.conn(), username)
}

/// 同 `clear_attempts`，但只依赖连接，可在调用方的事务内执行
pub fn clear_attempts_on(conn: &rusqlite::Connection, username: Option<&str>) -> DbResult<usize> {
    let n = match username {
        Some(u) => conn.execute(
            "DELETE FROM login_attempt WHERE username=?1",
            rusqlite::params![u],
        )?,
        None => conn.execute("DELETE FROM login_attempt", [])?,
    };
    Ok(n)
}

/// 登录设备身份（由 UI 层从操作系统采集）
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceIdentity {
    /// 稳定的设备指纹（machine-id / MachineGuid 等）
    pub id: String,
    /// 展示名（主机名等），方便管理员辨认
    pub name: String,
}

impl DeviceIdentity {
    pub fn new(id: &str, name: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
        }
    }
}

/// 登录结果
#[derive(Clone, Debug)]
pub enum LoginResult {
    /// 登录成功
    Ok(User),
    /// 口令错误；`remaining` 是还剩几次机会
    BadPassword { remaining: i64 },
    /// 账户被锁定；`minutes` 是剩余锁定分钟
    Locked { minutes: i64 },
    /// 账户已停用
    Disabled,
    /// 用户不存在
    NoSuchUser,
    /// 登录成功但必须立即修改口令
    MustChangePassword(User),
    /// 该账号已绑定其他设备（管理员不受限）
    DeviceBound { device_name: String },
}

/// 带失败锁定、口令策略与设备绑定的完整登录流程
///
/// `device` 传当前设备身份：
/// - 管理员不受设备限制，也不绑定设备
/// - 普通账号首次登录自动绑定当前设备；之后换设备登录会被拒绝，
///   需管理员在「安全中心」重置该账号的设备绑定
pub fn login(
    db: &Db,
    username: &str,
    password: &str,
    policy: &PasswordPolicy,
    device: Option<&DeviceIdentity>,
) -> DbResult<LoginResult> {
    let u = match crate::users::get(db, username)? {
        Some(u) => u,
        None => {
            // 用户不存在也付出一次同等成本的 Argon2，避免用响应时间枚举用户名。
            // （只记一笔失败，用于防暴力枚举。）
            let _ = fincore::user::burn_argon2(password);
            log_attempt(db, username, false)?;
            return Ok(LoginResult::NoSuchUser);
        }
    };
    if u.disabled {
        // 同样做等时处理，否则"已停用"比"口令错误"快得多，也能暴露账号存在性
        let _ = fincore::user::burn_argon2(password);
        return Ok(LoginResult::Disabled);
    }
    if u.is_locked_out() {
        return Ok(LoginResult::Locked {
            minutes: u.lock_remaining_min(),
        });
    }
    if !u.verify_password(password) {
        log_attempt(db, username, false)?;
        // 注意：本次失败已经写进 login_attempt 了，recent_fails 会把它算进去，别再加 1
        // 使用独立的失败窗口常量，不与锁定时长耦合
        let fails = recent_fails(db, username, LOCK_WINDOW_MIN)?;
        if fails >= policy.max_fail.max(1) {
            lock_user(db, username, policy.lock_minutes)?;
            return Ok(LoginResult::Locked {
                minutes: policy.lock_minutes,
            });
        }
        return Ok(LoginResult::BadPassword {
            remaining: (policy.max_fail - fails).max(0),
        });
    }
    // 设备绑定校验：口令正确但设备不符时不算登录成功，不计成功次数
    if !u.is_admin() {
        if let Some(d) = device {
            if u.device_id.is_empty() {
                // 首次登录：自动绑定当前设备
                crate::users::bind_device(db, username, &d.id, &d.name)?;
                db.log(username, "安全", "绑定设备", &format!("自动绑定「{}」", d.name))?;
            } else if u.device_id != d.id {
                return Ok(LoginResult::DeviceBound {
                    device_name: if u.device_name.is_empty() {
                        "其他设备".to_string()
                    } else {
                        u.device_name.clone()
                    },
                });
            }
        }
    }
    // 口令已验证 + 设备校验通过 = 本次登录成功：清失败计数、写登录时间
    // （须在强制改密 / 过期提示等提前返回之前，保证成功登录总能清零失败计数）
    log_attempt(db, username, true)?;
    crate::users::touch_login(db, username)?;
    // 成功前检查是否强制改密（在口令升级前检查，避免升级过程清除标志）
    if u.must_change_pwd {
        // 若为遗留哈希需先升级，再返回强制改密结果
        let mut nu = u.clone();
        if fincore::user::is_legacy_hash(&u.password_hash) {
            nu.set_password(password);
            // set_password 会把 must_change_pwd 清掉；强制改密场景需保留该标志，
            // 否则本次返回后用户关闭页面重登即可绕过改密。
            nu.must_change_pwd = true;
            crate::users::update(db, &nu)?;
            db.log(username, "安全", "口令升级", "旧版口令哈希已升级为 argon2，并强制改密")?;
        } else {
            db.log(username, "安全", "强制改密", "账号处于强制修改口令状态")?;
        }
        return Ok(LoginResult::MustChangePassword(nu));
    }
    // 旧版 `salt$sha256` 哈希登录成功：透明升级为 argon2，不打断用户
    if fincore::user::is_legacy_hash(&u.password_hash) {
        let mut nu = u.clone();
        nu.set_password(password);
        crate::users::update(db, &nu)?;
        db.log(username, "安全", "口令升级", "旧版口令哈希已升级为 argon2")?;
    }
    if policy.is_expired(&u.pwd_changed_at, chrono::Local::now().date_naive()) {
        return Ok(LoginResult::MustChangePassword(u));
    }
    Ok(LoginResult::Ok(u))
}

/// 锁定账户 `minutes` 分钟
pub fn lock_user(db: &Db, username: &str, minutes: i64) -> DbResult<()> {
    let until = (chrono::Local::now().naive_local() + chrono::Duration::minutes(minutes))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    db.conn().execute(
        "UPDATE user SET locked_until=?2 WHERE username=?1",
        rusqlite::params![username, until],
    )?;
    Ok(())
}

/// 解锁
pub fn unlock_user(db: &Db, username: &str) -> DbResult<()> {
    // 解绑锁定与清失败计数同事务：避免"锁已解、计数没清，下一次失败立即又锁"
    let tx = db.write_tx()?;
    tx.execute(
        "UPDATE user SET locked_until=NULL WHERE username=?1",
        rusqlite::params![username],
    )?;
    clear_attempts_on(&tx, Some(username))?;
    tx.commit()?;
    Ok(())
}

/// 修改口令（走完整策略校验）
pub fn change_password_checked(
    db: &Db,
    username: &str,
    old: &str,
    new: &str,
    policy: &PasswordPolicy,
) -> DbResult<Result<(), String>> {
    let mut u = match crate::users::get(db, username)? {
        Some(u) => u,
        None => return Ok(Err("用户不存在".to_string())),
    };
    if !u.verify_password(old) {
        return Ok(Err("原口令不正确".to_string()));
    }
    if let Err(e) = policy.check(new) {
        return Ok(Err(e));
    }
    if new == old {
        return Ok(Err("新口令不能与旧口令相同".to_string()));
    }
    u.set_password(new);
    let tx = db.write_tx()?;
    crate::users::update_on(&tx, &u)?;
    clear_attempts_on(&tx, Some(username))?;
    tx.commit()?;
    Ok(Ok(()))
}

/// 管理员重置口令（强制下次登录修改）
pub fn admin_reset_password(
    db: &Db,
    username: &str,
    new: &str,
    policy: &PasswordPolicy,
) -> DbResult<Result<(), String>> {
    if let Err(e) = policy.check(new) {
        return Ok(Err(e));
    }
    let mut u = match crate::users::get(db, username)? {
        Some(u) => u,
        None => return Ok(Err("用户不存在".to_string())),
    };
    u.set_password(new);
    u.must_change_pwd = true;
    // 重置口令同时解除锁定，否则用户拿到新口令还是进不来
    u.locked_until = None;
    let tx = db.write_tx()?;
    crate::users::update_on(&tx, &u)?;
    clear_attempts_on(&tx, Some(username))?;
    tx.commit()?;
    Ok(Ok(()))
}

// ---------------- 口令策略存取 ----------------

/// 口令策略存在账套参数里（`meta` 表），不存在则用默认
pub fn password_policy(db: &Db) -> PasswordPolicy {
    crate::options_json::<PasswordPolicy>(db, "password_policy").unwrap_or_default()
}

pub fn set_password_policy(db: &Db, p: &PasswordPolicy) -> DbResult<()> {
    crate::set_options_json(db, "password_policy", p)
}

// ---------------- 操作日志 ----------------

/// 日志查询条件
#[derive(Clone, Debug, Default)]
pub struct LogQuery {
    pub user: String,
    pub module: String,
    pub keyword: String,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub limit: i64,
}

impl LogQuery {
    pub fn new(limit: i64) -> Self {
        Self {
            limit,
            ..Default::default()
        }
    }
}

/// 记录一条操作日志
pub fn audit(db: &Db, user: &str, module: &str, action: &str, detail: &str) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO audit_log(ts,user,module,action,detail) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![now_str(), user, module, action, detail],
    )?;
    Ok(())
}

/// 查询操作日志
pub fn audit_query(db: &Db, q: &LogQuery) -> DbResult<Vec<fincore::user::AuditLog>> {
    let mut sql = String::from(
        "SELECT id,ts,user,module,action,detail FROM audit_log WHERE 1=1",
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if !q.user.is_empty() {
        sql.push_str(" AND user = ?");
        args.push(Box::new(q.user.clone()));
    }
    if !q.module.is_empty() {
        sql.push_str(" AND module = ?");
        args.push(Box::new(q.module.clone()));
    }
    if !q.keyword.is_empty() {
        sql.push_str(" AND (action LIKE ? ESCAPE '\\' OR detail LIKE ? ESCAPE '\\')");
        let kw = format!("%{}%", crate::escape_like(&q.keyword));
        args.push(Box::new(kw.clone()));
        args.push(Box::new(kw));
    }
    if let Some(d) = q.from {
        sql.push_str(" AND ts >= ?");
        args.push(Box::new(d.format("%Y-%m-%d").to_string()));
    }
    if let Some(d) = q.to {
        sql.push_str(" AND ts <= ?");
        // 含当天：上界取次日零点
        args.push(Box::new(
            (d + chrono::Duration::days(1)).format("%Y-%m-%d").to_string(),
        ));
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    args.push(Box::new(if q.limit <= 0 { 500 } else { q.limit }));

    let mut st = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = args.iter().map(|a| a.as_ref()).collect();
    let rows = st
        .query_map(refs.as_slice(), |r| {
            Ok(fincore::user::AuditLog {
                id: r.get(0)?,
                ts: r.get(1)?,
                user: r.get(2)?,
                module: r.get(3)?,
                action: r.get(4)?,
                detail: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 日志里出现过的模块（用于筛选下拉框）
pub fn audit_modules(db: &Db) -> DbResult<Vec<String>> {
    let mut st = db
        .conn()
        .prepare("SELECT DISTINCT module FROM audit_log WHERE module <> '' ORDER BY module")?;
    let rows = st.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 只保留最近 `keep` 条日志。
///
/// 审计日志是追责证据，不能成为"抹除痕迹"的工具：最少保留 200 条，
/// 且裁剪动作本身写入一条审计（谁在什么时候裁掉了多少）。
pub fn audit_trim(db: &Db, keep: i64) -> DbResult<usize> {
    let keep = keep.max(200);
    let n = db.conn().execute(
        "DELETE FROM audit_log WHERE id NOT IN (
            SELECT id FROM audit_log ORDER BY id DESC LIMIT ?1
         )",
        rusqlite::params![keep],
    )?;
    if n > 0 {
        db.log("系统", "审计", "裁剪日志", &format!("保留最近 {keep} 条，删除 {n} 条"))?;
    }
    Ok(n)
}

// ---------------- 会话空闲 ----------------

/// 会话状态：记录最后一次操作时间，用于空闲自动登出
#[derive(Clone, Copy, Debug)]
pub struct Session {
    last_active: Option<NaiveDateTime>,
    idle_minutes: i64,
}

impl Session {
    pub fn new(idle_minutes: i64) -> Self {
        Self {
            last_active: None,
            idle_minutes,
        }
    }
    /// 开始会话
    pub fn start(&mut self) {
        self.last_active = Some(chrono::Local::now().naive_local());
    }
    /// 用户有动作，刷新计时
    pub fn touch(&mut self) {
        if self.last_active.is_some() {
            self.last_active = Some(chrono::Local::now().naive_local());
        }
    }
    /// 已空闲分钟数
    pub fn idle_minutes(&self) -> i64 {
        match self.last_active {
            Some(t) => (chrono::Local::now().naive_local() - t).num_minutes().max(0),
            None => 0,
        }
    }
    /// 是否该登出了
    pub fn expired(&self) -> bool {
        self.idle_minutes > 0 && self.idle_minutes() >= self.idle_minutes
    }
    /// 距离自动登出还有几分钟
    pub fn remaining(&self) -> i64 {
        (self.idle_minutes - self.idle_minutes()).max(0)
    }
    pub fn end(&mut self) {
        self.last_active = None;
    }
}

/// 把用户的数据权限套用到一行数据上（供 UI 过滤用）
pub fn visible_dept(u: &User, dept: &str) -> bool {
    u.can_see_dept(dept)
}

pub fn visible_account(u: &User, code: &str) -> bool {
    u.can_see_account(code)
}

/// 数据权限校验失败时抛出的错误
pub fn denied(what: &str) -> FinError {
    FinError::Denied(format!("没有权限查看{what}的数据"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use fincore::user::Role;

    fn mkuser(db: &Db, name: &str, pwd: &str) {
        let mut u = User::new(name, name, Role::Accountant);
        u.set_password(pwd);
        crate::users::insert(db, &u).unwrap();
    }

    fn dev_a() -> DeviceIdentity {
        DeviceIdentity::new("dev-aaa", "会计室-01")
    }
    fn dev_b() -> DeviceIdentity {
        DeviceIdentity::new("dev-bbb", "经理室-02")
    }

    #[test]
    fn login_success_and_lockout() {
        let db = mem();
        mkuser(&db, "u1", "Passw0rd1");
        let pol = PasswordPolicy {
            max_fail: 3,
            lock_minutes: 15,
            ..Default::default()
        };
        assert!(matches!(
            login(&db, "u1", "Passw0rd1", &pol, Some(&dev_a())).unwrap(),
            LoginResult::Ok(_)
        ));
        // 第 1 次错：还剩 2 次机会
        assert!(matches!(
            login(&db, "u1", "bad", &pol, Some(&dev_a())).unwrap(),
            LoginResult::BadPassword { remaining: 2 }
        ));
        // 第 2 次错：还剩 1 次机会
        assert!(matches!(
            login(&db, "u1", "bad", &pol, Some(&dev_a())).unwrap(),
            LoginResult::BadPassword { remaining: 1 }
        ));
        // 第 3 次错：达到上限，锁定
        assert!(matches!(
            login(&db, "u1", "bad", &pol, Some(&dev_a())).unwrap(),
            LoginResult::Locked { .. }
        ));
        // 锁定期间正确口令也进不来
        assert!(matches!(
            login(&db, "u1", "Passw0rd1", &pol, Some(&dev_a())).unwrap(),
            LoginResult::Locked { .. }
        ));
        // 解锁后恢复
        unlock_user(&db, "u1").unwrap();
        assert!(matches!(
            login(&db, "u1", "Passw0rd1", &pol, Some(&dev_a())).unwrap(),
            LoginResult::Ok(_)
        ));
    }

    #[test]
    fn unknown_user_no_such() {
        let db = mem();
        assert!(matches!(
            login(&db, "ghost", "x", &PasswordPolicy::default(), None).unwrap(),
            LoginResult::NoSuchUser
        ));
    }

    #[test]
    fn device_binding() {
        let db = mem();
        mkuser(&db, "u_dev", "Passw0rd1");
        let pol = PasswordPolicy::default();

        // 首次登录自动绑定当前设备
        match login(&db, "u_dev", "Passw0rd1", &pol, Some(&dev_a())).unwrap() {
            LoginResult::Ok(_) => {}
            other => panic!("首次登录应成功，实际 {other:?}"),
        }
        let u = crate::users::get(&db, "u_dev").unwrap().unwrap();
        assert_eq!(u.device_id, "dev-aaa");
        assert_eq!(u.device_name, "会计室-01");

        // 同一设备再次登录：正常
        assert!(matches!(
            login(&db, "u_dev", "Passw0rd1", &pol, Some(&dev_a())).unwrap(),
            LoginResult::Ok(_)
        ));

        // 换设备：拒绝，且不计成功登录
        match login(&db, "u_dev", "Passw0rd1", &pol, Some(&dev_b())).unwrap() {
            LoginResult::DeviceBound { device_name } => assert_eq!(device_name, "会计室-01"),
            other => panic!("换设备应被拒绝，实际 {other:?}"),
        }

        // 管理员重置绑定后，新设备可以登录并重新绑定
        crate::users::reset_device(&db, "u_dev").unwrap();
        assert!(matches!(
            login(&db, "u_dev", "Passw0rd1", &pol, Some(&dev_b())).unwrap(),
            LoginResult::Ok(_)
        ));
        let u = crate::users::get(&db, "u_dev").unwrap().unwrap();
        assert_eq!(u.device_id, "dev-bbb");
        // 旧设备从此进不来
        assert!(matches!(
            login(&db, "u_dev", "Passw0rd1", &pol, Some(&dev_a())).unwrap(),
            LoginResult::DeviceBound { .. }
        ));
    }

    #[test]
    fn admin_unlimited_devices() {
        let db = mem();
        let pol = PasswordPolicy::default();
        // 内置 admin 就是管理员：任意设备都能登录，且不绑定
        for dev in [dev_a(), dev_b()] {
            match login(&db, "admin", "admin123", &pol, Some(&dev)).unwrap() {
                LoginResult::MustChangePassword(_) => {} // 首次登录强制改密，允许
                other => panic!("管理员应不受设备限制，实际 {other:?}"),
            }
        }
        let u = crate::users::get(&db, "admin").unwrap().unwrap();
        assert!(u.device_id.is_empty(), "管理员不应绑定设备");
    }

    #[test]
    fn password_policy_enforced() {
        let db = mem();
        mkuser(&db, "u2", "Passw0rd1");
        let pol = PasswordPolicy::default();
        assert!(change_password_checked(&db, "u2", "Passw0rd1", "123", &pol)
            .unwrap()
            .is_err()); // 太短
        assert!(change_password_checked(&db, "u2", "Passw0rd1", "abcdefgh", &pol)
            .unwrap()
            .is_err()); // 无数字
        assert!(change_password_checked(&db, "u2", "wrongold", "Passw0rd2", &pol)
            .unwrap()
            .is_err()); // 旧口令错
        assert!(change_password_checked(&db, "u2", "Passw0rd1", "Passw0rd2", &pol)
            .unwrap()
            .is_ok());
    }

    #[test]
    fn admin_reset_forces_change() {
        let db = mem();
        mkuser(&db, "u3", "Passw0rd1");
        assert!(admin_reset_password(&db, "u3", "Newpass123", &PasswordPolicy::default())
            .unwrap()
            .is_ok());
        let u = crate::users::get(&db, "u3").unwrap().unwrap();
        assert!(u.must_change_pwd);
        assert!(matches!(
            login(&db, "u3", "Newpass123", &PasswordPolicy::default(), None).unwrap(),
            LoginResult::MustChangePassword(_)
        ));
    }

    #[test]
    fn audit_log_roundtrip() {
        let db = mem();
        audit(&db, "admin", "凭证", "记账", "记-1").unwrap();
        audit(&db, "admin", "科目", "新增", "1001").unwrap();
        audit(&db, "u1", "凭证", "删除", "记-2").unwrap();
        let all = audit_query(&db, &LogQuery::new(100)).unwrap();
        assert_eq!(all.len(), 3);
        // 按模块过滤
        let q = LogQuery {
            module: "凭证".into(),
            limit: 100,
            ..Default::default()
        };
        assert_eq!(audit_query(&db, &q).unwrap().len(), 2);
        // 关键词
        let q = LogQuery {
            keyword: "1001".into(),
            limit: 100,
            ..Default::default()
        };
        assert_eq!(audit_query(&db, &q).unwrap().len(), 1);
        assert_eq!(audit_modules(&db).unwrap(), vec!["凭证", "科目"]);
    }

    #[test]
    fn session_idle() {
        let mut s = Session::new(30);
        assert!(!s.expired()); // 还没开始
        s.start();
        assert!(!s.expired());
        assert_eq!(s.remaining(), 30);
        s.end();
        assert_eq!(s.idle_minutes(), 0);
    }
}
