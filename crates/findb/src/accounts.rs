//! 科目仓储

use fincore::{Account, AcctCategory, AuxMask, Chart, CodeScheme, Direction, FinError};
use rusqlite::OptionalExtension;

use crate::{Db, DbError, DbResult};

fn map_account(r: &rusqlite::Row) -> rusqlite::Result<Account> {
    let cat: String = r.get(2)?;
    let dir: String = r.get(3)?;
    Ok(Account {
        code: r.get(0)?,
        name: r.get(1)?,
        category: serde_json::from_str::<AcctCategory>(&format!("\"{cat}\""))
            .unwrap_or(AcctCategory::Asset),
        dir: serde_json::from_str::<Direction>(&format!("\"{dir}\"")).unwrap_or(Direction::Debit),
        aux: AuxMask(r.get(4)?),
        unit: r.get(5)?,
        currency: r.get(6)?,
        has_qty: r.get::<_, i64>(7)? != 0,
        is_cash: r.get::<_, i64>(8)? != 0,
        is_bank: r.get::<_, i64>(9)? != 0,
        cash_flow_item: r.get(10)?,
        bs_item: r.get(11)?,
        pl_item: r.get(12)?,
        disabled: r.get::<_, i64>(13)? != 0,
        memo: r.get(14)?,
    })
}

/// 全部科目（按编码排序）
pub fn list(db: &Db) -> DbResult<Vec<Account>> {
    list_of(db.conn())
}

/// 同 `list`，但只依赖连接，可在事务内调用。
pub fn list_of(conn: &rusqlite::Connection) -> DbResult<Vec<Account>> {
    let mut stmt = conn.prepare(
        "SELECT code,name,category,dir,aux_mask,unit,currency,has_qty,is_cash,is_bank,
                cf_item,bs_item,pl_item,disabled,memo
         FROM account ORDER BY code",
    )?;
    let rows = stmt
        .query_map([], map_account)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 补齐内置科目表：把当前版本默认科目表中账套里还没有的科目补进来。
///
/// 旧账套建账时灌入的是当时的科目表（早期版本仅 119 个，新版已扩到 199 个），
/// 本函数只做"缺哪个补哪个"，不改动任何已有科目，返回补入数量。
pub fn fill_missing_defaults(db: &Db) -> DbResult<usize> {
    let existing: std::collections::HashSet<String> =
        list(db)?.into_iter().map(|a| a.code).collect();
    let mut n = 0usize;
    for a in fincore::chart::default_accounts() {
        if existing.contains(&a.code) {
            continue;
        }
        insert(db, &a)?;
        n += 1;
    }
    Ok(n)
}

pub fn get(db: &Db, code: &str) -> DbResult<Option<Account>> {
    db.conn()
        .query_row(
            "SELECT code,name,category,dir,aux_mask,unit,currency,has_qty,is_cash,is_bank,
                    cf_item,bs_item,pl_item,disabled,memo
             FROM account WHERE code=?1",
            rusqlite::params![code],
            map_account,
        )
        .optional()
        .map_err(DbError::from)
}

/// 构建科目表（含树形关系）
pub fn chart(db: &Db) -> DbResult<Chart> {
    chart_of(db.conn())
}

/// 同 `chart`，但只依赖连接：凭证守卫要在自己的事务里取科目表，
/// 不能反过来要求调用方先交出一个 `&Db`。
pub fn chart_of(conn: &rusqlite::Connection) -> DbResult<Chart> {
    let scheme = CodeScheme(crate::options_of(conn).code_scheme.clone());
    Ok(Chart::with_accounts(scheme, list_of(conn)?))
}

pub fn insert(db: &Db, a: &Account) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO account(code,name,category,dir,aux_mask,unit,currency,has_qty,is_cash,is_bank,
                             cf_item,bs_item,pl_item,disabled,memo)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        rusqlite::params![
            a.code,
            a.name,
            serde_json::to_value(a.category)?.as_str().unwrap_or("asset"),
            serde_json::to_value(a.dir)?.as_str().unwrap_or("debit"),
            a.aux.0 as i64,
            a.unit,
            a.currency,
            a.has_qty as i64,
            a.is_cash as i64,
            a.is_bank as i64,
            a.cash_flow_item,
            a.bs_item,
            a.pl_item,
            a.disabled as i64,
            a.memo,
        ],
    )?;
    Ok(())
}

pub fn update(db: &Db, a: &Account) -> DbResult<()> {
    let n = db.conn().execute(
        "UPDATE account SET name=?2,category=?3,dir=?4,aux_mask=?5,unit=?6,currency=?7,has_qty=?8,
                is_cash=?9,is_bank=?10,cf_item=?11,bs_item=?12,pl_item=?13,disabled=?14,memo=?15
         WHERE code=?1",
        rusqlite::params![
            a.code,
            a.name,
            serde_json::to_value(a.category)?.as_str().unwrap_or("asset"),
            serde_json::to_value(a.dir)?.as_str().unwrap_or("debit"),
            a.aux.0 as i64,
            a.unit,
            a.currency,
            a.has_qty as i64,
            a.is_cash as i64,
            a.is_bank as i64,
            a.cash_flow_item,
            a.bs_item,
            a.pl_item,
            a.disabled as i64,
            a.memo,
        ],
    )?;
    if n == 0 {
        return Err(FinError::not_found(format!("科目 {}", a.code)).into());
    }
    Ok(())
}

/// 删除科目。业务层可先行提示，但最后防线在数据层：
/// 已被凭证分录/期初引用（含下级）或仍有下级的科目一律拒绝物理删除。
pub fn delete(db: &Db, code: &str) -> DbResult<()> {
    let (entries, begins) = usage_with_children(db, code)?;
    if entries > 0 || begins > 0 {
        return Err(FinError::state(format!(
            "科目 {code}（或其下级）已被 {entries} 条凭证分录、{begins} 条期初引用，不能删除；请改为停用"
        ))
        .into());
    }
    let children: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM account WHERE code LIKE ?1 ESCAPE '\\' AND code <> ?2",
        rusqlite::params![format!("{}%", crate::escape_like(code)), code],
        |r| r.get(0),
    )?;
    if children > 0 {
        return Err(FinError::state(format!(
            "科目 {code} 仍有 {children} 个下级科目，请先删除下级"
        ))
        .into());
    }
    db.conn()
        .execute("DELETE FROM account WHERE code=?1", rusqlite::params![code])?;
    Ok(())
}

/// 科目被引用情况：（分录行数, 期初行数）
pub fn usage(db: &Db, code: &str) -> DbResult<(i64, i64)> {
    let e: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM voucher_entry WHERE account_code=?1",
        rusqlite::params![code],
        |r| r.get(0),
    )?;
    let b: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM begin_balance WHERE account_code=?1",
        rusqlite::params![code],
        |r| r.get(0),
    )?;
    Ok((e, b))
}

/// 科目及其所有下级被引用情况
pub fn usage_with_children(db: &Db, code: &str) -> DbResult<(i64, i64)> {
    let like = format!("{}%", crate::escape_like(code));
    let e: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM voucher_entry WHERE account_code LIKE ?1 ESCAPE '\\'",
        rusqlite::params![like],
        |r| r.get(0),
    )?;
    let b: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM begin_balance WHERE account_code LIKE ?1 ESCAPE '\\'",
        rusqlite::params![like],
        |r| r.get(0),
    )?;
    Ok((e, b))
}

/// 批量导入（覆盖同名编码）
pub fn import_many(db: &Db, accounts: &[Account]) -> DbResult<usize> {
    let tx = db.write_tx()?;
    let mut n = 0;
    for a in accounts {
        tx.execute(
            "INSERT OR REPLACE INTO account(code,name,category,dir,aux_mask,unit,currency,has_qty,
                    is_cash,is_bank,cf_item,bs_item,pl_item,disabled,memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            rusqlite::params![
                a.code,
                a.name,
                serde_json::to_value(a.category).unwrap_or_default().as_str().unwrap_or("asset"),
                serde_json::to_value(a.dir).unwrap_or_default().as_str().unwrap_or("debit"),
                a.aux.0 as i64,
                a.unit,
                a.currency,
                a.has_qty as i64,
                a.is_cash as i64,
                a.is_bank as i64,
                a.cash_flow_item,
                a.bs_item,
                a.pl_item,
                a.disabled as i64,
                a.memo,
            ],
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// 关键字搜索（编码或名称）
pub fn search(db: &Db, kw: &str, limit: i64) -> DbResult<Vec<Account>> {
    let like = format!("%{}%", crate::escape_like(kw));
    let mut stmt = db.conn().prepare(
        "SELECT code,name,category,dir,aux_mask,unit,currency,has_qty,is_cash,is_bank,
                cf_item,bs_item,pl_item,disabled,memo
         FROM account WHERE code LIKE ?1 ESCAPE '\\' OR name LIKE ?1 ESCAPE '\\' ORDER BY code LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![like, limit], map_account)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn roundtrip() {
        let db = mem();
        let mut a = Account::new("1009", "其他货币资金-测试", AcctCategory::Asset);
        a.memo = "备注".to_string();
        insert(&db, &a).unwrap();

        let got = get(&db, "1009").unwrap().unwrap();
        assert_eq!(got.name, a.name);
        assert_eq!(got.category, AcctCategory::Asset);
        assert_eq!(got.dir, Direction::Debit);
        assert_eq!(got.memo, "备注");

        a.name = "改过名字".to_string();
        update(&db, &a).unwrap();
        assert_eq!(get(&db, "1009").unwrap().unwrap().name, "改过名字");

        delete(&db, "1009").unwrap();
        assert!(get(&db, "1009").unwrap().is_none());
    }

    #[test]
    fn chart_builds() {
        let db = mem();
        let c = chart(&db).unwrap();
        assert!(c.contains("1001"));
        assert!(c.is_leaf("1001"));
        assert!(!c.is_leaf("1002"));
        assert_eq!(c.full_name("100201"), "银行存款 / 工行基本户");
    }

    #[test]
    fn usage_counts() {
        let db = mem();
        assert_eq!(usage(&db, "1001").unwrap(), (0, 0));
        assert_eq!(usage_with_children(&db, "1002").unwrap(), (0, 0));
    }

    #[test]
    fn search_works() {
        let db = mem();
        assert!(!search(&db, "现金", 20).unwrap().is_empty());
        assert!(search(&db, "不存在的科目xyz", 20).unwrap().is_empty());
    }

    #[test]
    fn fill_missing_defaults_restores_old_chart() {
        let db = mem();
        let full = fincore::chart::default_accounts().len();
        assert_eq!(list(&db).unwrap().len(), full, "新账套应包含完整默认科目表");

        // 模拟旧版账套：只保留 1001/1002 两个一级科目，其余全部删掉
        let keep: std::collections::HashSet<String> = ["1001", "1002"].iter().map(|s| s.to_string()).collect();
        let mut codes: Vec<String> = list(&db)
            .unwrap()
            .into_iter()
            .map(|a| a.code)
            .filter(|c| !keep.contains(c))
            .collect();
        // 数据层有"仍有下级不能删"的防线：先删下级（编码大的）再删上级
        codes.sort();
        codes.reverse();
        for c in codes {
            delete(&db, &c).unwrap();
        }
        let before = list(&db).unwrap().len();
        assert_eq!(before, 2, "模拟旧账套仅剩 2 个科目");

        // 一键补齐：补入全部缺失的内置科目，且不重复
        let inserted = fill_missing_defaults(&db).unwrap();
        assert_eq!(inserted, full - 2, "应补入缺失的科目");
        let after = list(&db).unwrap().len();
        assert_eq!(after, full);
        // 幂等：再次补齐不再新增
        assert_eq!(fill_missing_defaults(&db).unwrap(), 0);
    }
}
