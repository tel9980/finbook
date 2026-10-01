//! 用户仓储与认证

use fincore::user::DataScope;
use fincore::{Perm, Role, User};
use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn map_user(r: &rusqlite::Row) -> rusqlite::Result<User> {
    let role_s: String = r.get(4)?;
    let perms_s: String = r.get(6)?;
    let scope_s: String = r.get(12)?;
    let deny_s: String = r.get::<_, String>(15).unwrap_or_default();
    Ok(User {
        id: r.get(0)?,
        username: r.get(1)?,
        display_name: r.get(2)?,
        password_hash: r.get(3)?,
        role: serde_json::from_str::<Role>(&format!("\"{role_s}\"")).unwrap_or(Role::Viewer),
        disabled: r.get::<_, i64>(5)? != 0,
        extra_perms: serde_json::from_str::<Vec<Perm>>(&perms_s).unwrap_or_default(),
        deny_perms: serde_json::from_str::<Vec<Perm>>(&deny_s).unwrap_or_default(),
        roles: serde_json::from_str::<Vec<Role>>(&r.get::<_, String>(16).unwrap_or_default())
            .unwrap_or_default(),
        memo: r.get(7)?,
        pwd_changed_at: r.get(8)?,
        must_change_pwd: r.get::<_, i64>(9)? != 0,
        locked_until: r.get(10)?,
        last_login_at: r.get(11)?,
        device_id: r.get(13)?,
        device_name: r.get(14)?,
        data_scope: serde_json::from_str::<DataScope>(&scope_s).unwrap_or_default(),
    })
}

const COLS: &str = "id,username,display_name,password_hash,role,disabled,extra_perms,memo,\
pwd_changed_at,must_change_pwd,locked_until,last_login_at,data_scope_json,device_id,device_name,deny_perms_json,roles_json";

pub fn list(db: &Db) -> DbResult<Vec<User>> {
    let mut stmt = db
        .conn()
        .prepare(&format!("SELECT {COLS} FROM user ORDER BY username"))?;
    let rows = stmt.query_map([], map_user)?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get(db: &Db, username: &str) -> DbResult<Option<User>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM user WHERE username=?1"),
            rusqlite::params![username],
            map_user,
        )
        .optional()
        .map_err(Into::into)
}

pub fn get_by_id(db: &Db, id: i64) -> DbResult<Option<User>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM user WHERE id=?1"),
            rusqlite::params![id],
            map_user,
        )
        .optional()
        .map_err(Into::into)
}

pub fn insert(db: &Db, u: &User) -> DbResult<i64> {
    // 不相容职务：会计与出纳权限不可同现（建号即校验，两端共用）
    if let Err(m) = fincore::user::User::validate_duty_separation(u) {
        return Err(fincore::FinError::validate(m).into());
    }
    db.conn().execute(
        "INSERT INTO user(username,display_name,password_hash,role,disabled,extra_perms,memo,
            pwd_changed_at,must_change_pwd,locked_until,last_login_at,data_scope_json,device_id,device_name,deny_perms_json,roles_json)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        rusqlite::params![
            u.username,
            u.display_name,
            u.password_hash,
            serde_json::to_value(u.role)?.as_str().unwrap_or("viewer"),
            u.disabled as i64,
            serde_json::to_string(&u.extra_perms)?,
            u.memo,
            u.pwd_changed_at,
            u.must_change_pwd as i64,
            u.locked_until,
            u.last_login_at,
            serde_json::to_string(&u.data_scope)?,
            u.device_id,
            u.device_name,
            serde_json::to_string(&u.deny_perms)?,
            serde_json::to_string(&u.roles)?,
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn update(db: &Db, u: &User) -> DbResult<()> {
    // 不相容职务：每次权限变更后重新校验（Web 改权限 / 桌面保存 / 数据范围保存共用）
    if let Err(m) = fincore::user::User::validate_duty_separation(u) {
        return Err(fincore::FinError::validate(m).into());
    }
    update_on(db.conn(), u)
}

/// 同 `update`，但只依赖连接，可在调用方的事务内执行
pub fn update_on(conn: &rusqlite::Connection, u: &User) -> DbResult<()> {
    conn.execute(
        "UPDATE user SET display_name=?2,password_hash=?3,role=?4,disabled=?5,extra_perms=?6,memo=?7,
            pwd_changed_at=?8,must_change_pwd=?9,locked_until=?10,last_login_at=?11,data_scope_json=?12,
            device_id=?13,device_name=?14,deny_perms_json=?15,roles_json=?16
         WHERE id=?1",
        rusqlite::params![
            u.id,
            u.display_name,
            u.password_hash,
            serde_json::to_value(u.role)?.as_str().unwrap_or("viewer"),
            u.disabled as i64,
            serde_json::to_string(&u.extra_perms)?,
            u.memo,
            u.pwd_changed_at,
            u.must_change_pwd as i64,
            u.locked_until,
            u.last_login_at,
            serde_json::to_string(&u.data_scope)?,
            u.device_id,
            u.device_name,
            serde_json::to_string(&u.deny_perms)?,
            serde_json::to_string(&u.roles)?,
        ],
    )?;
    Ok(())
}

pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    // 先取用户名留审计：删用户不写日志的话，人员变动就无迹可查
    let name: Option<String> = db
        .conn()
        .query_row("SELECT username FROM user WHERE id=?1", rusqlite::params![id], |r| r.get(0))
        .optional()?;
    db.conn()
        .execute("DELETE FROM user WHERE id=?1", rusqlite::params![id])?;
    if let Some(n) = name {
        db.log("系统", "安全", "删除用户", &format!("删除账套账号「{n}」"))?;
    }
    Ok(())
}

/// 记录最近登录时间
pub fn touch_login(db: &Db, username: &str) -> DbResult<()> {
    db.conn().execute(
        "UPDATE user SET last_login_at=?2 WHERE username=?1",
        rusqlite::params![
            username,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(())
}

/// 写入/清空锁定到期时间
pub fn set_locked_until(db: &Db, username: &str, until: Option<&str>) -> DbResult<()> {
    db.conn().execute(
        "UPDATE user SET locked_until=?2 WHERE username=?1",
        rusqlite::params![username, until],
    )?;
    Ok(())
}

/// 绑定登录设备（首次登录时自动调用）
pub fn bind_device(db: &Db, username: &str, device_id: &str, device_name: &str) -> DbResult<()> {
    db.conn().execute(
        "UPDATE user SET device_id=?2, device_name=?3 WHERE username=?1",
        rusqlite::params![username, device_id, device_name],
    )?;
    Ok(())
}

/// 重置设备绑定（管理员操作；清空后该账号可在任意设备重新绑定）
pub fn reset_device(db: &Db, username: &str) -> DbResult<()> {
    db.conn().execute(
        "UPDATE user SET device_id='', device_name='' WHERE username=?1",
        rusqlite::params![username],
    )?;
    Ok(())
}

pub fn count(db: &Db) -> DbResult<i64> {
    Ok(db.conn().query_row("SELECT COUNT(*) FROM user", [], |r| r.get(0))?)
}

/// 是否已有管理员账号（用于 Web「首次登录即管理员」的状态判断）
pub fn admin_exists(db: &Db) -> DbResult<bool> {
    let n: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM user WHERE role='admin'", [], |r| r.get(0))?;
    Ok(n > 0)
}

/// 取第一个管理员账号的用户名（用于状态提示展示）
pub fn first_admin_username(db: &Db) -> DbResult<Option<String>> {
    Ok(db
        .conn()
        .query_row(
            "SELECT username FROM user WHERE role='admin' ORDER BY id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?)
}

/// 创建首个管理员账号（Web「首次登录即管理员」流程调用）。
///
/// 仅在账套尚无任何用户时由上层调用；这里不重复校验唯一性。
pub fn create_admin(
    db: &Db,
    username: &str,
    password: &str,
    display_name: &str,
) -> DbResult<User> {
    let mut u = User::new(username, display_name, Role::Admin);
    u.set_password(password);
    u.must_change_pwd = false; // 刚由用户自己设定口令，无需强制改密
    let id = insert(db, &u)?;
    u.id = id;
    db.log(
        username,
        "安全",
        "初始化管理员",
        &format!("首次登录创建管理员账号「{username}」"),
    )?;
    Ok(u)
}

/// 认证。为降低时序侧信道，用户不存在时也执行一次等价的口令校验。
pub fn authenticate(db: &Db, username: &str, password: &str) -> DbResult<Option<User>> {
    match get(db, username)? {
        Some(u) => {
            if u.disabled {
                return Ok(None);
            }
            if u.verify_password(password) {
                Ok(Some(u))
            } else {
                Ok(None)
            }
        }
        None => {
            // 等价的空校验：哈希长度对齐后执行一次真实 sha256，降低用户名枚举的时序差异
            let _ = fincore::user::verify_password(password, &format!("0${}", "0".repeat(64)));
            Ok(None)
        }
    }
}

/// 修改口令（需校验旧口令）
pub fn change_password(db: &Db, username: &str, old: &str, new: &str) -> DbResult<Result<(), String>> {
    let mut u = match get(db, username)? {
        Some(u) => u,
        None => return Ok(Err("用户不存在".to_string())),
    };
    if !u.verify_password(old) {
        return Ok(Err("原口令不正确".to_string()));
    }
    if new.trim().len() < 6 {
        return Ok(Err("新口令至少 6 位".to_string()));
    }
    u.set_password(new);
    update(db, &u)?;
    Ok(Ok(()))
}

/// 重置口令（管理员操作，不需旧口令）
pub fn reset_password(db: &Db, username: &str, new: &str) -> DbResult<()> {
    let mut u = get(db, username)?.ok_or_else(|| fincore::FinError::not_found("用户不存在"))?;
    u.set_password(new);
    update(db, &u)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn seeded_users() {
        let db = mem();
        // 新账套只内置一个管理员账号，其余账号由管理员开通
        assert_eq!(count(&db).unwrap(), 1);
        let u = get(&db, "admin").unwrap().unwrap();
        assert_eq!(u.role, Role::Admin);
        assert!(u.can(Perm::UserManage));
        assert!(u.must_change_pwd, "管理员首次登录应强制改密");
        // 管理员拥有一切权限，包括导出
        assert!(u.can(Perm::Export));
    }

    #[test]
    fn auth_ok_and_bad() {
        let db = mem();
        assert!(authenticate(&db, "admin", "admin123").unwrap().is_some());
        assert!(authenticate(&db, "admin", "wrong").unwrap().is_none());
        assert!(authenticate(&db, "nobody", "x").unwrap().is_none());
    }

    #[test]
    fn disabled_cannot_login() {
        let db = mem();
        let mut u = User::new("u_dis", "停用测试", Role::Accountant);
        u.set_password("Passw0rd1");
        u.id = insert(&db, &u).unwrap();
        u.disabled = true;
        update(&db, &u).unwrap();
        assert!(authenticate(&db, "u_dis", "Passw0rd1").unwrap().is_none());
    }

    #[test]
    fn change_pwd() {
        let db = mem();
        assert!(change_password(&db, "admin", "wrong", "newpass")
            .unwrap()
            .is_err());
        assert!(change_password(&db, "admin", "admin123", "123")
            .unwrap()
            .is_err()); // 太短
        assert!(change_password(&db, "admin", "admin123", "newpass123")
            .unwrap()
            .is_ok());
        assert!(authenticate(&db, "admin", "newpass123").unwrap().is_some());
    }

    #[test]
    fn crud_extra_perms() {
        let db = mem();
        let mut u = User::new("test", "测试", Role::Cashier);
        u.extra_perms = vec![Perm::PeriodClose];
        u.set_password("test123");
        let id = insert(&db, &u).unwrap();
        let got = get_by_id(&db, id).unwrap().unwrap();
        assert!(got.can(Perm::PeriodClose));
        delete(&db, id).unwrap();
        assert!(get_by_id(&db, id).unwrap().is_none());
    }

    #[test]
    fn no_admin_book_and_create_admin() {
        use crate::tests::mem_no_admin;
        let db = mem_no_admin();
        assert_eq!(count(&db).unwrap(), 0);
        assert!(!admin_exists(&db).unwrap());
        let u = create_admin(&db, "boss", "BossPass123", "老板").unwrap();
        assert_eq!(u.role, Role::Admin);
        assert!(admin_exists(&db).unwrap());
        assert_eq!(first_admin_username(&db).unwrap().as_deref(), Some("boss"));
        // 该账号应能正常登录
        assert!(authenticate(&db, "boss", "BossPass123").unwrap().is_some());
    }
}
