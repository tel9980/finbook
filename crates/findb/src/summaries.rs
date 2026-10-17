//! 常用摘要
//!
//! 摘要这东西看着不起眼，实际是录入效率的大头——"提现""报销差旅费""计提折旧"
//! 一天要敲几十遍。表里同时记了使用次数，补全列表按热度排序。

use crate::{Db, DbResult};

/// 一条常用摘要
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub text: String,
    pub use_count: i64,
}

/// 列出全部摘要（按使用次数倒序）
pub fn list(db: &Db) -> DbResult<Vec<Summary>> {
    let mut st = db
        .conn()
        .prepare("SELECT text,use_count FROM summary ORDER BY use_count DESC, text")?;
    let rows = st
        .query_map([], |r| {
            Ok(Summary {
                text: r.get(0)?,
                use_count: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 关键词过滤
pub fn search(db: &Db, kw: &str) -> DbResult<Vec<Summary>> {
    let kw = kw.trim();
    if kw.is_empty() {
        return list(db);
    }
    let pat = format!("%{}%", crate::escape_like(kw));
    let mut st = db.conn().prepare(
        "SELECT text,use_count FROM summary WHERE text LIKE ?1 ESCAPE '\\'
         ORDER BY use_count DESC, text LIMIT 50",
    )?;
    let rows = st
        .query_map(rusqlite::params![pat], |r| {
            Ok(Summary {
                text: r.get(0)?,
                use_count: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 记录一次使用：已存在则计数 +1，不存在则插入（计数从 1 起）
pub fn bump(db: &Db, text: &str) -> DbResult<()> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(());
    }
    db.conn().execute(
        "INSERT INTO summary(text,use_count) VALUES(?1,1)
         ON CONFLICT(text) DO UPDATE SET use_count = use_count + 1",
        rusqlite::params![t],
    )?;
    Ok(())
}

/// 批量记录（一张凭证的多行摘要只算一次）
pub fn bump_many(db: &Db, texts: &[String]) -> DbResult<()> {
    let mut seen = std::collections::HashSet::new();
    for t in texts {
        let t = t.trim().to_string();
        if !t.is_empty() && seen.insert(t.clone()) {
            bump(db, &t)?;
        }
    }
    Ok(())
}

pub fn insert(db: &Db, text: &str) -> DbResult<()> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(());
    }
    db.conn().execute(
        "INSERT OR IGNORE INTO summary(text,use_count) VALUES(?1,0)",
        rusqlite::params![t],
    )?;
    Ok(())
}

pub fn delete(db: &Db, text: &str) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM summary WHERE text=?1", rusqlite::params![text])?;
    Ok(())
}

pub fn update(db: &Db, old: &str, new: &str) -> DbResult<()> {
    let n = new.trim();
    if n.is_empty() {
        return Ok(());
    }
    db.conn().execute(
        "UPDATE summary SET text=?2 WHERE text=?1",
        rusqlite::params![old, n],
    )?;
    Ok(())
}

/// 把最近用过的 N 条摘要交给界面做下拉补全
pub fn top(db: &Db, n: usize) -> DbResult<Vec<String>> {
    let mut st = db
        .conn()
        .prepare("SELECT text FROM summary ORDER BY use_count DESC, text LIMIT ?1")?;
    let rows = st
        .query_map(rusqlite::params![n as i64], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn bump_increments_and_dedups() {
        let db = mem();
        bump(&db, "提现").unwrap();
        bump(&db, "提现").unwrap();
        bump(&db, "报销差旅费").unwrap();
        let all = list(&db).unwrap();
        let first = all.first().unwrap();
        assert_eq!(first.text, "提现");
        assert_eq!(first.use_count, 2);
    }

    #[test]
    fn bump_many_dedups_in_one_pass() {
        let db = mem();
        bump_many(&db, &["a".into(), "a".into(), "b".into()]).unwrap();
        let a = list(&db).unwrap();
        assert_eq!(a.iter().find(|s| s.text == "a").unwrap().use_count, 1);
    }

    #[test]
    fn search_filters() {
        let db = mem();
        for t in ["报销差旅费", "报销办公费", "计提折旧"] {
            bump(&db, t).unwrap();
        }
        assert_eq!(search(&db, "报销").unwrap().len(), 2);
        assert!(search(&db, "").unwrap().len() >= 3);
    }
}
