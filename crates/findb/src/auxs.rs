//! 辅助核算档案仓储（客户 / 供应商 / 部门 / 职员 / 项目 / 存货 / 银行账户）

use std::collections::BTreeMap;

use fincore::{AuxEntity, AuxKind, AuxQuery};

use rusqlite::OptionalExtension;

use crate::{Db, DbResult};

fn kind_str(k: AuxKind) -> &'static str {
    match k {
        AuxKind::Customer => "customer",
        AuxKind::Supplier => "supplier",
        AuxKind::Dept => "dept",
        AuxKind::Employee => "employee",
        AuxKind::Project => "project",
        AuxKind::Item => "item",
        AuxKind::CashFlow => "cashflow",
        AuxKind::Bank => "bank",
    }
}

fn kind_from(s: &str) -> Option<AuxKind> {
    AuxKind::from_code(s)
}

fn map_entity(r: &rusqlite::Row) -> rusqlite::Result<AuxEntity> {
    let kind_s: String = r.get(1)?;
    let props: String = r.get(6)?;
    Ok(AuxEntity {
        id: r.get(0)?,
        kind: kind_from(&kind_s).unwrap_or(AuxKind::Dept),
        code: r.get(2)?,
        name: r.get(3)?,
        parent_code: r.get(4)?,
        disabled: r.get::<_, i64>(5)? != 0,
        props: serde_json::from_str::<BTreeMap<String, String>>(&props).unwrap_or_default(),
        memo: r.get(7)?,
    })
}

const COLS: &str = "id,kind,code,name,parent_code,disabled,props_json,memo";

/// 条件查询
pub fn list(db: &Db, q: &AuxQuery) -> DbResult<Vec<AuxEntity>> {
    let mut sql = format!("SELECT {COLS} FROM aux_entity WHERE 1=1");
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(k) = q.kind {
        sql.push_str(" AND kind = ?");
        params.push(Box::new(kind_str(k).to_string()));
    }
    if let Some(ref p) = q.parent_code {
        sql.push_str(" AND parent_code = ?");
        params.push(Box::new(p.clone()));
    }
    if !q.include_disabled {
        sql.push_str(" AND disabled = 0");
    }
    if let Some(ref kw) = q.keyword {
        sql.push_str(" AND (code LIKE ? ESCAPE '\\' OR name LIKE ? ESCAPE '\\')");
        let k = format!("%{}%", crate::escape_like(kw));
        params.push(Box::new(k.clone()));
        params.push(Box::new(k));
    }
    sql.push_str(" ORDER BY code");
    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), map_entity)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get(db: &Db, kind: AuxKind, code: &str) -> DbResult<Option<AuxEntity>> {
    db.conn()
        .query_row(
            &format!("SELECT {COLS} FROM aux_entity WHERE kind=?1 AND code=?2"),
            rusqlite::params![kind_str(kind), code],
            map_entity,
        )
        .optional()
        .map_err(Into::into)
}

pub fn insert(db: &Db, e: &AuxEntity) -> DbResult<i64> {
    db.conn().execute(
        "INSERT INTO aux_entity(kind,code,name,parent_code,disabled,props_json,memo)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![
            kind_str(e.kind),
            e.code,
            e.name,
            e.parent_code,
            e.disabled as i64,
            serde_json::to_string(&e.props)?,
            e.memo
        ],
    )?;
    Ok(db.conn().last_insert_rowid())
}

pub fn update(db: &Db, e: &AuxEntity) -> DbResult<()> {
    db.conn().execute(
        "UPDATE aux_entity SET name=?2,parent_code=?3,disabled=?4,props_json=?5,memo=?6
         WHERE id=?1",
        rusqlite::params![
            e.id,
            e.name,
            e.parent_code,
            e.disabled as i64,
            serde_json::to_string(&e.props)?,
            e.memo
        ],
    )?;
    Ok(())
}

pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM aux_entity WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 某类档案的全部编码（校验重复用）
pub fn codes(db: &Db, kind: AuxKind) -> DbResult<Vec<String>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT code FROM aux_entity WHERE kind=?1 ORDER BY code")?;
    let rows = stmt
        .query_map(rusqlite::params![kind_str(kind)], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 编码 → 名称 的映射（界面展示用，避免逐条查）
pub fn name_map(db: &Db, kind: AuxKind) -> DbResult<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    for e in list(
        db,
        &AuxQuery {
            kind: Some(kind),
            include_disabled: true,
            ..Default::default()
        },
    )? {
        m.insert(e.code.clone(), e.name.clone());
    }
    Ok(m)
}

/// 全部档案的 "kind:code" → 名称 映射（辅助核算列展示用）
pub fn full_name_map(db: &Db) -> DbResult<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    let mut stmt = db
        .conn()
        .prepare("SELECT kind,code,name FROM aux_entity")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })?;
    for r in rows {
        let (k, c, n) = r?;
        m.insert(format!("{k}:{c}"), n);
    }
    Ok(m)
}

/// 档案被凭证引用的次数
pub fn usage(db: &Db, kind: AuxKind, code: &str) -> DbResult<i64> {
    let pattern = format!("%{}={}%", kind.code(), crate::escape_like(code));
    let c: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM voucher_entry WHERE aux_key LIKE ?1 ESCAPE '\\'",
        rusqlite::params![pattern],
        |r| r.get(0),
    )?;
    Ok(c)
}

/// 批量导入
pub fn import_many(db: &Db, items: &[AuxEntity]) -> DbResult<usize> {
    let tx = db.write_tx()?;
    let mut n = 0;
    for e in items {
        tx.execute(
            "INSERT OR REPLACE INTO aux_entity(kind,code,name,parent_code,disabled,props_json,memo)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            rusqlite::params![
                kind_str(e.kind),
                e.code,
                e.name,
                e.parent_code,
                e.disabled as i64,
                serde_json::to_string(&e.props)?,
                e.memo
            ],
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// 常用摘要
pub fn list_summaries(db: &Db) -> DbResult<Vec<String>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT text FROM summary ORDER BY use_count DESC, text")?;
    let rows = stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn add_summary(db: &Db, text: &str) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO summary(text,use_count) VALUES(?1,1)
         ON CONFLICT(text) DO UPDATE SET use_count=use_count+1",
        rusqlite::params![text],
    )?;
    Ok(())
}

/// 结算方式
pub fn list_settle_types(db: &Db) -> DbResult<Vec<String>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT name FROM settle_type ORDER BY sort")?;
    let rows = stmt.query_map([], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    #[test]
    fn crud() {
        let db = mem();
        let mut c = AuxEntity::new(AuxKind::Customer, "C001", "华东商贸");
        c.set_prop("tax_no", "91310000XXXX");
        let id = insert(&db, &c).unwrap();
        assert!(id > 0);

        let got = get(&db, AuxKind::Customer, "C001").unwrap().unwrap();
        assert_eq!(got.name, "华东商贸");
        assert_eq!(got.prop("tax_no").unwrap(), "91310000XXXX");

        c.id = id;
        c.name = "华东商贸有限公司".into();
        update(&db, &c).unwrap();
        assert_eq!(
            get(&db, AuxKind::Customer, "C001").unwrap().unwrap().name,
            "华东商贸有限公司"
        );

        delete(&db, id).unwrap();
        assert!(get(&db, AuxKind::Customer, "C001").unwrap().is_none());
    }

    #[test]
    fn query_by_kind() {
        let db = mem();
        insert(&db, &AuxEntity::new(AuxKind::Dept, "D01", "销售部")).unwrap();
        insert(&db, &AuxEntity::new(AuxKind::Dept, "D02", "财务部")).unwrap();
        insert(&db, &AuxEntity::new(AuxKind::Supplier, "S01", "钢构厂")).unwrap();

        assert_eq!(list(&db, &AuxQuery::kind(AuxKind::Dept)).unwrap().len(), 2);
        assert_eq!(
            list(&db, &AuxQuery::kind(AuxKind::Dept).with_keyword("财务"))
                .unwrap()
                .len(),
            1
        );
        // 停用的默认不返回
        let mut e = AuxEntity::new(AuxKind::Dept, "D03", "停用部门");
        e.disabled = true;
        insert(&db, &e).unwrap();
        assert_eq!(list(&db, &AuxQuery::kind(AuxKind::Dept)).unwrap().len(), 2);
        assert_eq!(
            list(&db, &AuxQuery::kind(AuxKind::Dept).with_disabled(true))
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn name_maps() {
        let db = mem();
        insert(&db, &AuxEntity::new(AuxKind::Dept, "D01", "销售部")).unwrap();
        assert_eq!(name_map(&db, AuxKind::Dept).unwrap().get("D01").unwrap(), "销售部");
        assert_eq!(
            full_name_map(&db).unwrap().get("dept:D01").unwrap(),
            "销售部"
        );
    }

    #[test]
    fn summaries() {
        let db = mem();
        let before = list_summaries(&db).unwrap().len();
        add_summary(&db, "测试摘要").unwrap();
        add_summary(&db, "测试摘要").unwrap(); // 重复只增加计数
        assert_eq!(list_summaries(&db).unwrap().len(), before + 1);
        assert!(!list_settle_types(&db).unwrap().is_empty());
    }
}
