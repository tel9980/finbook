//! # findb —— FinBook 持久化层
//!
//! 一个账套 = 一个 `.fbk` 文件（SQLite）。备份就是复制文件，恢复就是覆盖文件。
//!
//! 提供能力：
//! - [`Db::create`] 新建账套并灌入内置科目表、现金流量项目、常用摘要
//! - 科目、辅助档案、凭证、用户的增删改查
//! - 余额与账簿的实时聚合（[`balances::BalanceSnapshot`]）
//! - 期末结账 / 反结账
//! - 备份恢复、操作日志

pub mod accounts;
pub mod advanced;
pub mod attach;
pub mod automation;
pub mod assets;
pub mod auxs;
pub mod balances;
pub mod bank;
pub mod business;
pub mod funds;
pub mod imports;
pub mod inventory2;
pub mod stocktake;
pub mod workflow;
pub mod batch;
pub mod workbench;
pub mod docflow;
pub mod invoices;
pub mod mgmt;
pub mod periods;
pub mod printform;
pub mod procurement;
pub mod security;
pub mod reports;
pub mod sales;
pub mod schema;
pub mod summaries;
pub mod settle;
pub mod receipt;
pub mod template;
pub mod scm;
pub mod scm2;
pub mod manufacturing;
pub mod stock;
pub mod users;
pub mod vouchers;
pub mod warehouse;
pub mod exports;

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use fincore::chart::{
    default_accounts, default_settle_types, default_summaries, default_voucher_words,
};
use fincore::report::cashflow::{default_cash_flow_items, CashFlowDirection, CashFlowGroup};
use fincore::user::{AuditLog, User};
use fincore::{BookOptions, FinError};
use rusqlite::Connection;

pub use balances::BalanceSnapshot;

/// 账套文件默认扩展名
pub const BOOK_EXT: &str = "fbk";

/// 数据库错误
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("SQLite 错误：{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("序列化错误：{0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Fin(#[from] FinError),
}

impl From<DbError> for FinError {
    fn from(e: DbError) -> Self {
        FinError::db(e.to_string())
    }
}

/// 结果别名
pub type DbResult<T> = Result<T, DbError>;

/// 能借出一个 `&Connection` 的对象：`Db` 本身，以及调用方已开启的
/// `rusqlite::Transaction`。
///
/// 仓储函数把参数声明成 `&impl AsConn` 后，既有调用点传 `&Db` 照常编译，
/// 新的事务内调用传 `&tx` 也能编译——读、写因此可以放进同一个事务而不必
/// 复制一份 SQL。
pub trait AsConn {
    fn conn_ref(&self) -> &rusqlite::Connection;
}

impl AsConn for Db {
    fn conn_ref(&self) -> &rusqlite::Connection {
        &self.conn
    }
}

impl AsConn for rusqlite::Transaction<'_> {
    fn conn_ref(&self) -> &rusqlite::Connection {
        self
    }
}

/// 账套数据库连接
pub struct Db {
    conn: Connection,
    path: PathBuf,
}

impl Db {
    /// 打开已有账套
    pub fn open<P: AsRef<Path>>(path: P) -> DbResult<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Err(FinError::msg(format!("账套文件不存在：{}", path.display())).into());
        }
        let conn = Connection::open(&path)?;
        schema::init(&conn)?;
        Ok(Self { conn, path })
    }

    /// 新建账套。若文件已存在则报错，避免误覆盖。
    pub fn create<P: AsRef<Path>>(path: P, opts: &BookOptions) -> DbResult<Self> {
        Self::create_with(path, opts, true)
    }

    /// 新建账套但不内置管理员账号。
    ///
    /// 用于 Web 服务端「首次登录即管理员」的初始化流程：账套建好后用户表为空，
    /// 第一个成功登录的账号会被自动创建为系统管理员。
    pub fn create_no_admin<P: AsRef<Path>>(path: P, opts: &BookOptions) -> DbResult<Self> {
        Self::create_with(path, opts, false)
    }

    fn create_with<P: AsRef<Path>>(path: P, opts: &BookOptions, seed_admin: bool) -> DbResult<Self> {
        let path = path.as_ref().to_path_buf();
        if path.exists() {
            return Err(FinError::msg(format!("文件已存在，未覆盖：{}", path.display())).into());
        }
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| FinError::io(e.to_string()))?;
            }
        }
        let conn = Connection::open(&path)?;
        schema::init(&conn)?;
        let db = Self { conn, path };
        db.seed_builtin(opts, seed_admin)?;
        Ok(db)
    }

    /// 打开内存数据库（测试 / 演示用）
    pub fn in_memory(opts: &BookOptions) -> DbResult<Self> {
        Self::in_memory_with(opts, true)
    }

    /// 打开内存数据库但不内置管理员（Web 初始化流程测试用）
    pub fn in_memory_no_admin(opts: &BookOptions) -> DbResult<Self> {
        Self::in_memory_with(opts, false)
    }

    fn in_memory_with(opts: &BookOptions, seed_admin: bool) -> DbResult<Self> {
        let conn = Connection::open_in_memory()?;
        schema::init(&conn)?;
        let db = Self {
            conn,
            path: PathBuf::from(":memory:"),
        };
        db.seed_builtin(opts, seed_admin)?;
        Ok(db)
    }

    /// 灌入内置基础资料
    ///
    /// `seed_admin=false` 时跳过内置管理员账号，留给上层走「首次登录即管理员」流程。
    fn seed_builtin(&self, opts: &BookOptions, seed_admin: bool) -> DbResult<()> {
        for a in default_accounts() {
            accounts::insert(self, &a)?;
        }
        {
            let mut stmt = self.conn.prepare(
                "INSERT OR REPLACE INTO cash_flow_item(code,name,grp,dir,disabled) VALUES(?1,?2,?3,?4,0)",
            )?;
            for it in default_cash_flow_items() {
                let grp = match it.group {
                    CashFlowGroup::Operating => "operating",
                    CashFlowGroup::Investing => "investing",
                    CashFlowGroup::Financing => "financing",
                };
                let dir = match it.dir {
                    CashFlowDirection::In => "in",
                    CashFlowDirection::Out => "out",
                };
                stmt.execute(rusqlite::params![it.code, it.name, grp, dir])?;
            }
        }
        {
            let mut stmt = self
                .conn
                .prepare("INSERT OR IGNORE INTO summary(text,use_count) VALUES(?1,0)")?;
            for s in default_summaries() {
                stmt.execute(rusqlite::params![s])?;
            }
        }
        {
            let mut stmt = self
                .conn
                .prepare("INSERT OR IGNORE INTO settle_type(name,sort) VALUES(?1,?2)")?;
            for (i, s) in default_settle_types().iter().enumerate() {
                stmt.execute(rusqlite::params![s, i as i64])?;
            }
        }
        if seed_admin {
            // 只内置一个管理员账号：首次登录即管理员，其余账号由管理员在
            // 「安全中心 → 用户管理」里开通（可设角色、口令与数据范围）。
            let mut u = User::new("admin", "系统管理员", fincore::Role::Admin);
            u.set_password("admin123");
            u.must_change_pwd = true;
            users::insert(self, &u)?;
        }
        reports::ensure_defaults(self)?;
        self.set_options(opts)?;
        Ok(())
    }

    #[inline]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// 开启写事务（BEGIN IMMEDIATE）。多表写入一律用它。
    ///
    /// 不要再用 `conn().transaction()` / `unchecked_transaction()`：两者默认
    /// DEFERRED，事务里先读后写时 SQLite 要到真正写入那一刻才去拿写锁，WAL 下
    /// 若期间快照已被别的事务改动，会直接返回 SQLITE_BUSY_SNAPSHOT —— 而
    /// busy_timeout 对这个错误码不会重试，只读请求也跟着失败。
    ///
    /// rusqlite 0.32 的 `transaction_with_behavior` 要 `&mut self`，而账套连接一律以
    /// `&Db` 流转，故走 `Transaction::new_unchecked`：它只借 `&Connection`，自己发
    /// `BEGIN IMMEDIATE`，drop 时回滚。嵌套开启仍会由 SQLite 报错，与原先
    /// `unchecked_transaction()` 的运行时行为一致。
    pub fn write_tx(&self) -> DbResult<rusqlite::Transaction<'_>> {
        Ok(rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?)
    }
    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    // ---------------- meta ----------------
    pub fn meta_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key=?1", rusqlite::params![key], |r| {
                r.get(0)
            })
            .ok()
    }
    pub fn meta_set(&self, key: &str, value: &str) -> DbResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// 账套参数
    pub fn options(&self) -> BookOptions {
        options_of(&self.conn)
    }
    pub fn set_options(&self, o: &BookOptions) -> DbResult<()> {
        let s = serde_json::to_string(o)?;
        self.meta_set("options", &s)
    }

    /// 读取存在 meta 里的 JSON 配置（失败时返回 None）
    pub fn meta_json<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.meta_get(key)
            .and_then(|s| serde_json::from_str::<T>(&s).ok())
    }
    /// 写入 JSON 配置到 meta
    pub fn meta_set_json<T: serde::Serialize>(&self, key: &str, v: &T) -> DbResult<()> {
        let s = serde_json::to_string(v)?;
        self.meta_set(key, &s)
    }

    /// 可用凭证字
    pub fn voucher_words(&self) -> Vec<String> {
        let w = self.options().voucher_words;
        if w.is_empty() {
            default_voucher_words()
        } else {
            w
        }
    }

    // ---------------- 备份与维护 ----------------

    /// 备份账套（`VACUUM INTO` 产出的是已整理过的紧凑副本，可在软件运行时热备）
    pub fn backup<P: AsRef<Path>>(&self, to: P) -> DbResult<()> {
        let to = to.as_ref();
        if let Some(dir) = to.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(|e| FinError::io(e.to_string()))?;
            }
        }
        let p = to.to_string_lossy().replace('\'', "''");
        self.conn
            .execute_batch(&format!("VACUUM INTO '{p}'"))
            .map_err(DbError::Sqlite)?;
        Ok(())
    }

    /// 自动备份：写入 `dir` 下以 `auto_` 开头、带时间戳的文件，
    /// 并只保留最近 `keep` 份（轮转删除最旧的），返回实际生成的文件路径。
    ///
    /// 命名 `auto_YYYYMMDD_HHMMSSmmm.fbk`（毫秒级防止同一秒冲突），
    /// 供 [`prune_auto_backups`] 识别。
    pub fn backup_auto<P: AsRef<Path>>(&self, dir: P, keep: usize) -> DbResult<PathBuf> {
        let dir = dir.as_ref().to_path_buf();
        let base = format!(
            "auto_{}",
            chrono::Local::now().format("%Y%m%d_%H%M%S%3f")
        );
        // 毫秒级时间戳极少冲突；万一冲突（时钟回拨等）则追加序号
        let mut path = dir.join(format!("{base}.{BOOK_EXT}"));
        let mut n = 1;
        while path.exists() {
            path = dir.join(format!("{base}_{n}.{BOOK_EXT}"));
            n += 1;
        }
        self.backup(&path)?;
        prune_auto_backups(&dir, keep)?;
        Ok(path)
    }

    /// 整理数据库文件（回收删除产生的空闲页）
    pub fn vacuum(&self) -> DbResult<()> {
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    /// 完整性检查
    pub fn integrity_check(&self) -> DbResult<Vec<String>> {
        let mut stmt = self.conn.prepare("PRAGMA integrity_check")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 统计信息：（凭证数, 分录数, 科目数）
    pub fn stats(&self) -> DbResult<(i64, i64, i64)> {
        let v: i64 = self.conn.query_row("SELECT COUNT(*) FROM voucher", [], |r| r.get(0))?;
        let e: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM voucher_entry", [], |r| r.get(0))?;
        let a: i64 = self.conn.query_row("SELECT COUNT(*) FROM account", [], |r| r.get(0))?;
        Ok((v, e, a))
    }

    // ---------------- 日志 ----------------
    pub fn log(&self, user: &str, module: &str, action: &str, detail: &str) -> DbResult<()> {
        log_on(&self.conn, user, module, action, detail)
    }

    pub fn recent_logs(&self, limit: i64) -> DbResult<Vec<AuditLog>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,ts,user,module,action,detail FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit], map_log)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn search_logs(&self, keyword: &str, limit: i64) -> DbResult<Vec<AuditLog>> {
        let kw = format!("%{}%", escape_like(keyword));
        let mut stmt = self.conn.prepare(
            "SELECT id,ts,user,module,action,detail FROM audit_log
             WHERE user LIKE ?1 ESCAPE '\\' OR module LIKE ?1 ESCAPE '\\'
               OR action LIKE ?1 ESCAPE '\\' OR detail LIKE ?1 ESCAPE '\\'
             ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![kw, limit], map_log)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 清空全部业务数据，保留基础资料（用于重新建账 / 演示重置）
    pub fn clear_vouchers(&self) -> DbResult<()> {
        let tx = self.write_tx()?;
        tx.execute_batch(
            "DELETE FROM voucher_entry;
             DELETE FROM voucher;
             DELETE FROM begin_balance;
             DELETE FROM period_state;",
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn map_log(r: &rusqlite::Row) -> rusqlite::Result<AuditLog> {
    Ok(AuditLog {
        id: r.get(0)?,
        ts: r.get(1)?,
        user: r.get(2)?,
        module: r.get(3)?,
        action: r.get(4)?,
        detail: r.get(5)?,
    })
}

/// 在给定连接上写操作日志（[`Db::log`] 的事务内版本：`Transaction` 可 Deref 为
/// `Connection`，多表写入时与业务 SQL 共用同一事务，避免"业务已落库、日志没写上"的半成品）。
pub fn log_on(
    conn: &rusqlite::Connection,
    user: &str,
    module: &str,
    action: &str,
    detail: &str,
) -> DbResult<()> {
    conn.execute(
        "INSERT INTO audit_log(ts,user,module,action,detail) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            user,
            module,
            action,
            detail
        ],
    )?;
    Ok(())
}

/// 从行中读取金额（金额以 TEXT 存储）
pub fn read_money(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<fincore::Money> {
    let s: String = row.get(idx)?;
    Ok(fincore::Money::parse_or_zero(&s))
}

/// 从行中读取可空金额
pub fn read_money_opt(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<fincore::Money>> {
    let s: Option<String> = row.get(idx)?;
    Ok(s.map(|x| fincore::Money::parse_or_zero(&x)))
}

/// 从行中读取可空日期
pub fn read_date_opt(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<NaiveDate>> {
    let s: Option<String> = row.get(idx)?;
    match s {
        None => Ok(None),
        Some(s) => Ok(NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
    }
}

/// 金额写入参数：统一两位小数的十进制字符串
/// 读取账套里的一段 JSON 配置
pub fn options_json<T: serde::de::DeserializeOwned>(db: &Db, key: &str) -> Option<T> {
    db.meta_json::<T>(key)
}

/// 自动备份轮转：删除目录下 `auto_*.fbk` 中按修改时间排序最旧的，
/// 只保留最近 `keep` 份。`keep == 0` 表示不清理任何文件。
///
/// 只匹配 `auto_` 前缀的自动备份，绝不误删用户手工命名的备份。
pub fn prune_auto_backups(dir: &Path, keep: usize) -> DbResult<()> {
    if keep == 0 {
        return Ok(());
    }
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| FinError::io(e.to_string()))? {
        let entry = entry.map_err(|e| FinError::io(e.to_string()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("auto_") || !name.ends_with(&format!(".{BOOK_EXT}")) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(mt) = meta.modified() {
                files.push((mt, path));
            }
        }
    }
    if files.len() <= keep {
        return Ok(());
    }
    files.sort_by_key(|f| f.0); // 最旧的在前
    let excess = files.len() - keep;
    for (_, p) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(p); // 删除失败不致命，只留下孤儿文件
    }
    Ok(())
}

/// 写入一段 JSON 配置到账套
pub fn set_options_json<T: serde::Serialize>(db: &Db, key: &str, v: &T) -> DbResult<()> {
    db.meta_set_json(key, v)
}

/// 读取账套参数。只依赖连接，因此可以在已开启的事务内调用
/// （`Db::options` 只是它的一层委托）。
pub fn options_of(conn: &rusqlite::Connection) -> fincore::BookOptions {
    conn.query_row("SELECT value FROM meta WHERE key='options'", [], |r| {
        r.get::<_, String>(0)
    })
    .ok()
    .and_then(|s| serde_json::from_str(&s).ok())
    .unwrap_or_default()
}

#[inline]
pub fn money_param(m: fincore::Money) -> String {
    m.fmt_plain()
}

/// 单据号前缀（账套参数 `doc_prefixes` 可自定义；空/未配置用内置默认）。
/// 键：po / so / req / quo / prod（采购订单/销售订单/请购/报价/生产订单）。
pub fn doc_prefix(db: &Db, kind: &str, default: &str) -> String {
    db.options()
        .doc_prefixes
        .get(kind)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// 给报表 SQL 追加数据范围条件（科目区间 + 仅本人填制的凭证）。
///
/// 调用方需保证 SQL 中分录表别名是 `e`、凭证表别名是 `v`，且 `params`
/// 已按现有占位符顺序放好参数（占位符编号按 `params.len()+1` 递增）。
pub(crate) fn push_report_scope(
    sql: &mut String,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    scope: &fincore::user::DataScope,
    username: &str,
) {
    let lo = scope.account_from.trim();
    if !lo.is_empty() {
        params.push(Box::new(lo.to_string()));
        sql.push_str(&format!(" AND e.account_code >= ?{}", params.len()));
    }
    let hi = scope.account_to.trim();
    if !hi.is_empty() {
        params.push(Box::new(hi.to_string()));
        sql.push_str(&format!(" AND e.account_code <= ?{}", params.len()));
    }
    if scope.own_voucher_only {
        params.push(Box::new(username.to_string()));
        sql.push_str(&format!(" AND v.prepared_by = ?{}", params.len()));
    }
}

/// 数量 / 单价 / 汇率 / 费率落库用：保留满精度。
///
/// 与 `money_param` 的区别是它不量化到 2 位。凡是被 `costing` 之类引擎按
/// QTY_DP(6) 以上口径参与的字段，写入都必须走这里，否则读回来已经丢了尾数。
#[inline]
pub fn exact_param(m: fincore::Money) -> String {
    m.fmt_exact()
}

/// LIKE 转义：把 `\`、`%`、`_` 逐个加 `\` 前缀，配合 `ESCAPE '\'` 使用。
///
/// 调用方统一用 `format!("%{}%", escape_like(kw))` 构造含通配符的查询参数，
/// 避免用户输入的 `%_\\` 被当成通配符（LIKE 注入导致越权/漏数）。
#[inline]
pub fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用账套：固定启用期间为 2026 年 1 月，避免依赖当前系统时间
    pub(crate) fn mem() -> Db {
        let mut o = BookOptions::default();
        o.start_period = fincore::Period::new(2026, 1).unwrap();
        Db::in_memory(&o).unwrap()
    }

    /// 测试用账套：不内置管理员，用于「首次登录即管理员」流程测试
    pub(crate) fn mem_no_admin() -> Db {
        let mut o = BookOptions::default();
        o.start_period = fincore::Period::new(2026, 1).unwrap();
        Db::in_memory_no_admin(&o).unwrap()
    }

    #[test]
    fn create_and_seed() {
        let db = mem();
        let (v, e, a) = db.stats().unwrap();
        assert_eq!((v, e), (0, 0));
        assert!(a > 100, "内置科目表应有一百多个科目，实际 {a}");
        assert_eq!(db.options().base_currency, "CNY");
        assert!(users::get(&db, "admin").unwrap().is_some());
    }

    /// 汇率与数量/单价必须满精度落库。Money 的 Display 会截到 2 位，
    /// 所以这些字段只能走 exact_param。
    #[test]
    fn fx_rate_and_qty_roundtrip_full_precision() {
        use fincore::Money;
        let db = mem();
        let p = fincore::Period::new(2026, 1).unwrap();

        automation::fx_set(&db, p, "USD", Money::parse("7.2345").unwrap()).unwrap();
        let raw: String = db
            .conn()
            .query_row(
                "SELECT rate FROM fx_rate WHERE period=?1 AND currency=?2",
                rusqlite::params![p.ymm(), "USD"],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw, "7.2345", "汇率落库被截断为 {raw}");
        let back = automation::fx_get(&db, p, "USD").unwrap().unwrap();
        assert_eq!(back.rate, Money::parse("7.2345").unwrap());

        let mv = business::StockMove {
            id: 0,
            period: p,
            biz_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
            kind: business::StockKind::Purchase,
            item: "A001".to_string(),
            warehouse: "W1".to_string(),
            batch_no: String::new(),
            qty: Money::parse("0.123456").unwrap(),
            price: Money::parse("9.876543").unwrap(),
            amount: Money::parse("1.22").unwrap(),
            voucher_id: None,
            memo: String::new(),
        };
        let id = business::stock_insert(&db, &mv).unwrap();
        let (q, pr): (String, String) = db
            .conn()
            .query_row(
                "SELECT qty, price FROM stock_move WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(q, "0.123456", "数量落库被截断为 {q}");
        assert_eq!(pr, "9.876543", "单价落库被截断为 {pr}");
    }

    /// schema 已是最新版本时，重新打开账套走免写事务的快速路径，且数据完好。
    #[test]
    fn reopen_book_keeps_data_on_fast_path() {
        let dir = std::env::temp_dir().join(format!("finbook_test_reopen_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("book.fbk");

        let p = fincore::Period::new(2026, 1).unwrap();
        {
            let db = Db::create(&path, &BookOptions::default()).unwrap();
            assert_eq!(schema::version(db.conn()), schema::SCHEMA_VERSION);
            automation::fx_set(&db, p, "EUR", fincore::Money::parse("7.8543").unwrap()).unwrap();
        }
        // 二次打开：版本已最新，init 应跳过 DDL，且已有数据可读回
        let db = Db::open(&path).unwrap();
        let rate = automation::fx_get(&db, p, "EUR").unwrap().unwrap().rate;
        assert_eq!(rate, fincore::Money::parse("7.8543").unwrap());
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 版本号已是最新、但 DDL 里某张表缺失时，重新打开必须把它补建出来。
    /// 历史上 `invoice` / `prod_cost` 就是只加 DDL 没升 SCHEMA_VERSION，
    /// 快速路径若只看版本号，这两张表对老账套就永远回不来了。
    #[test]
    fn reopen_recreates_table_missing_at_current_version() {
        let dir = std::env::temp_dir().join(format!(
            "finbook_test_repair_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("b2.fbk");

        {
            let db = Db::create(&path, &BookOptions::default()).unwrap();
            db.conn()
                .execute_batch("DROP TABLE invoice;")
                .expect("删表失败");
            assert_eq!(schema::version(db.conn()), schema::SCHEMA_VERSION);
        }
        let db = Db::open(&path).unwrap();
        let n: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='invoice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "invoice 表应在重开时被补建");
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `write_tx` 必须真的发 `BEGIN IMMEDIATE`：持有它之后，另一连接应立即抢不到
    /// 写锁。退回 DEFERRED 时 BEGIN 本身不拿锁，第二个连接就会成功，此测试失败。
    #[test]
    fn write_tx_holds_write_lock_from_begin() {
        let dir = std::env::temp_dir().join(format!(
            "finbook_test_immediate_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("b4.fbk");
        {
            let _ = Db::create(&path, &BookOptions::default()).unwrap();
        }
        let holder = Db::open(&path).unwrap();
        let other = Db::open(&path).unwrap();
        other
            .conn()
            .execute_batch("PRAGMA busy_timeout = 50;")
            .unwrap();

        let tx = holder.write_tx().expect("开写事务失败");
        let locked = other.conn().execute_batch("BEGIN IMMEDIATE;");
        assert!(
            locked.is_err(),
            "write_tx 只开了 DEFERRED 事务：另一连接仍能拿到写锁"
        );
        drop(tx);
        other
            .conn()
            .execute_batch("BEGIN IMMEDIATE; COMMIT;")
            .expect("事务释放后应能拿到写锁");
        drop(other);
        drop(holder);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 快速路径的实际收益：一个连接正持有写事务时，另一个连接仍应立即打开账套，
    /// 而不是等 busy_timeout 超时。旧实现每次 open 都跑 BEGIN IMMEDIATE + DDL，
    /// 纯读请求也会去抢写锁，Web 端并发下就是成片 500。
    #[test]
    fn open_is_not_blocked_by_another_write_tx() {
        let dir = std::env::temp_dir().join(format!(
            "finbook_test_conc_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("b3.fbk");
        {
            let _ = Db::create(&path, &BookOptions::default()).unwrap();
        }
        let writer = Db::open(&path).unwrap();
        writer
            .conn()
            .execute_batch("BEGIN IMMEDIATE;")
            .expect("开写事务失败");
        let t = std::time::Instant::now();
        let reader = Db::open(&path);
        assert!(
            reader.is_ok() && t.elapsed() < std::time::Duration::from_secs(2),
            "只读打开被写事务挡住（耗时 {:?}）",
            t.elapsed()
        );
        writer.conn().execute_batch("COMMIT;").unwrap();
        drop(writer);
        drop(reader);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn options_roundtrip() {
        let db = mem();
        let mut o = BookOptions::default();
        o.company = "某某科技有限公司".to_string();
        o.start_period = fincore::Period::new(2026, 3).unwrap();
        db.set_options(&o).unwrap();
        assert_eq!(db.options().company, "某某科技有限公司");
        assert_eq!(
            db.options().start_period,
            fincore::Period::new(2026, 3).unwrap()
        );
    }

    #[test]
    fn log_records() {
        let db = mem();
        db.log("admin", "凭证", "新增", "记-0001").unwrap();
        let logs = db.recent_logs(10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].user, "admin");
        assert_eq!(db.search_logs("记-0001", 10).unwrap().len(), 1);
        assert!(db.search_logs("不存在的关键字", 10).unwrap().is_empty());
    }

    #[test]
    fn integrity() {
        let db = mem();
        let r = db.integrity_check().unwrap();
        assert_eq!(r, vec!["ok".to_string()]);
    }

    #[test]
    fn backup_auto_with_retention() {
        let db = mem();
        let dir = std::env::temp_dir().join(format!(
            "finbook_test_backup_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 故意放一个手工命名的备份，验证轮转不会误删它
        let manual = dir.join("user_backup.fbk");
        std::fs::write(&manual, b"manual").unwrap();

        // 生成 5 份自动备份，保留 3 份
        for _ in 0..5 {
            db.backup_auto(&dir, 3).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20)); // 保证时间戳/修改时间不同
        }
        let autos: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("auto_"))
            .collect();
        assert_eq!(autos.len(), 3, "自动备份应只保留最近 3 份：{autos:?}");
        assert!(
            std::fs::read_dir(&dir).unwrap().any(|e| {
                e.ok()
                    .map(|x| x.file_name().to_string_lossy() == "user_backup.fbk")
                    .unwrap_or(false)
            }),
            "手工备份不应被轮转删除"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
