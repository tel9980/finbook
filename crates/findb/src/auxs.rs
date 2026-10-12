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
    check_parent(db, e.kind, &e.code, e.parent_code.as_deref())?;
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
    check_parent(db, e.kind, &e.code, e.parent_code.as_deref())?;
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

// ---------------- parent_code（分级档案）的校验 ----------------
//
// 全部四个问题都会让「按层级汇总」崩掉或丢数据：
//   ① 上级不存在   → 填个不存在的编码，那个客户在层级视图里凭空消失
//   ② 自环（C01→C01）→ 递归不收敛
//   ③ 多级环（C01→C02→C01）→ 同样不收敛
//   ④ 删父不管子   → 子节点变孤儿
//
// ①③ 的做法：**先扫全图有没有环**，有就拒（并报出环上的编码）；
// 没有再模拟这次修改。有历史脏数据时，先修数据再改档案 ——
// 否则每次改任何一行都会撞上同一个错，用户会觉得「档案不能编辑」。

/// (kind, code) -> 上级编码。**包含平级档案**（值为空串）。
///
/// 这个「包含平级」是必须的：我第一版只把有上级的行放进 map，
/// 于是「上级是否存在」这一问（`map.contains_key(p)`）对**任何平级档案**
/// 都返回 false —— 结果是「给 C02 指定上级 C01」被判成「C01 不存在」。
/// 校验一装上，所有给平级档案设上级的操作全被拒。
/// 这类失败比不校验更糟：它让功能整体不可用，而且报错指向「数据不存在」，
/// 真实原因是「校验写错了」。
fn parent_map(db: &Db, kind: AuxKind) -> DbResult<std::collections::BTreeMap<String, String>> {
    let mut st = db.conn().prepare(
        "SELECT code, COALESCE(parent_code,'') FROM aux_entity WHERE kind=?1"
    )?;
    let rows = st.query_map([kind_str(kind)], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut m = std::collections::BTreeMap::new();
    for r in rows {
        let (c, p) = r?;
        m.insert(c, p);
    }
    Ok(m)
}

/// 图里从 `start` 出发按 parent 上溯，能不能走回自己（= 有环）
fn reaches(map: &std::collections::BTreeMap<String, String>, start: &str, target: &str) -> bool {
    let mut cur = map.get(start).map(String::as_str);
    // 上限：编码数量 + 1。没有这个上限，一个 1000 节点的链会走 1000 步才停，
    // 而图有环时会**永远**走下去。这里必须靠 map.len() 兜底，不能靠 while 无界。
    let mut steps = 0usize;
    while let Some(c) = cur {
        if c == target {
            return true;
        }
        // 平级档案（上级为空串）就是链的终点。parent_map 现在**包含**平级档案，
        // 所以不加这一句就会把空串当成一个编码继续上溯（map 里没有 "" 这个键，
        // 恰好返回 None，但那是靠「查不到」而不是靠「它确实是终点」停下来的）。
        if c.trim().is_empty() {
            return false;
        }
        steps += 1;
        if steps > map.len() + 1 {
            return true; // 超过节点数还没走回自己 = 一定成环
        }
        cur = map.get(c).map(String::as_str);
    }
    false
}

/// 保存/更新前校验 parent_code。`code` 是本行自己的编码（更新时传入）。
pub fn check_parent(db: &Db, kind: AuxKind, code: &str, parent: Option<&str>) -> DbResult<()> {
    let p = parent.unwrap_or("").trim();
    if p.is_empty() {
        return Ok(());
    }
    if p == code.trim() {
        return Err(fincore::FinError::validate(
            format!("上级编码不能是自己（{code}）—— 会形成环，层级汇总无法收敛"),
        )
        .into());
    }
    let map = parent_map(db, kind)?;
    // ① 上级必须存在
    if !map.contains_key(p) {
        return Err(fincore::FinError::validate(format!(
            "上级编码 {p} 不存在（{} 的上级必须在同类档案里已建档）",
            code.trim()
        ))
        .into());
    }
    // ③ 这次修改会不会成环：把 p 的上级链走一遍，看能不能走到自己
    if reaches(&map, p, code.trim()) {
        return Err(fincore::FinError::validate(format!(
            "上级 {p} 的上级链已指向 {}，形成环 —— 层级汇总无法收敛",
            code.trim()
        ))
        .into());
    }
    Ok(())
}

/// 库里的层级图是否已经成环（历史脏数据）。返回环上的一个编码。
pub fn find_parent_cycle(db: &Db, kind: AuxKind) -> DbResult<Option<String>> {
    let map = parent_map(db, kind)?;
    for c in map.keys() {
        if reaches(&map, c, c) {
            return Ok(Some(c.clone()));
        }
    }
    Ok(None)
}

/// 删除前的子节点检查：返回挂在 `code` 下面的直接子编码
pub fn children_of(db: &Db, kind: AuxKind, code: &str) -> DbResult<Vec<String>> {
    let mut st = db.conn().prepare(
        "SELECT code FROM aux_entity WHERE kind=?1 AND parent_code=?2 ORDER BY code"
    )?;
    let rows = st.query_map(rusqlite::params![kind_str(kind), code], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
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
///
/// 两侧补 `\x1f` 分隔符做**整段**匹配：`%customer=C001%` 会把
/// `customer=C0011` 的引用也算进来，导致 C001 明明没被引用却删不掉。
pub fn usage(db: &Db, kind: AuxKind, code: &str) -> DbResult<i64> {
    let pattern = format!(
        "%\u{1f}{}={}\u{1f}%",
        kind.code(),
        crate::escape_like(code)
    );
    let c: i64 = db.conn().query_row(
        "SELECT COUNT(*) FROM voucher_entry
         WHERE (char(31) || aux_key || char(31)) LIKE ?1 ESCAPE '\\'",
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

/// 批量导入（跳过已存在的编码，**不覆盖**）
///
/// 与 `import_many` 的区别就在那句 `INSERT OR REPLACE`：那个会**静默覆盖**同名档案 ——
/// 用户从 Excel 粘一份名单进来，其中一条编码恰好和库里重了，名字被悄悄改掉，
/// 而他毫无察觉地继续用（尤其当那份 Excel 是旧版本时）。
/// 这里宁可少导一条、并在返回值里报告跳过了哪些，也不替用户做决定。
///
/// 返回 (成功条数, 跳过的编码列表)。
pub fn insert_many_skip_existing(db: &Db, items: &[AuxEntity]) -> DbResult<(usize, Vec<String>)> {
    let tx = db.write_tx()?;
    let mut n = 0;
    let mut skipped: Vec<String> = Vec::new();
    for e in items {
        let exists: i64 = tx.query_row(
            "SELECT COUNT(1) FROM aux_entity WHERE kind=?1 AND code=?2",
            rusqlite::params![kind_str(e.kind), e.code],
            |r| r.get(0),
        )?;
        if exists > 0 {
            skipped.push(e.code.clone());
            continue;
        }
        tx.execute(
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
        n += 1;
    }
    tx.commit()?;
    Ok((n, skipped))
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
mod parent_tests {
    use super::*;
    use crate::tests::mem;

    fn mk(db: &Db, code: &str, parent: Option<&str>) -> AuxEntity {
        AuxEntity {
            id: 0,
            kind: AuxKind::Customer,
            code: code.into(),
            name: format!("客户{code}"),
            parent_code: parent.map(String::from),
            disabled: false,
            props: Default::default(),
            memo: String::new(),
        }
    }

    /// 上级必须已建档 —— 填一个不存在的编码，那个客户会在层级视图里凭空消失。
    #[test]
    fn parent_must_exist() {
        let db = mem();
        insert(&db, &mk(&db, "C01", None)).unwrap();
        let e = mk(&db, "C02", Some("C99"));
        let err = insert(&db, &e).unwrap_err().to_string();
        assert!(err.contains("C99"), "报错要点名不存在的上级编码：{err}");
        assert!(err.contains("不存在"), "要说清是「不存在」而不是「无效」：{err}");
    }

    /// 自环：C01 的上级填 C01。
    #[test]
    fn parent_cannot_be_self() {
        let db = mem();
        insert(&db, &mk(&db, "C01", None)).unwrap();
        let err = insert(&db, &mk(&db, "C01", Some("C01"))).unwrap_err().to_string();
        assert!(err.contains("自己"), "要说清是「不能填自己」：{err}");
    }

    /// 多级环：C01→C02→C01。**这个最容易漏** —— 只查自环的实现挡不住它，
    /// 而它同样让递归不收敛。
    #[test]
    fn parent_cycle_of_length_two_rejected() {
        let db = mem();
        insert(&db, &mk(&db, "C01", None)).unwrap();
        insert(&db, &mk(&db, "C02", Some("C01"))).unwrap();

        // 现在改 C01，让它的上级是 C02 —— C01→C02→C01
        let mut e = mk(&db, "C01", Some("C02"));
        e.id = 1; // C01 的 id 是第一个插入的
        let id = {
            let r = db
                .conn()
                .query_row(
                    "SELECT id FROM aux_entity WHERE kind='customer' AND code='C01'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap();
            r
        };
        e.id = id;
        let err = update(&db, &e).unwrap_err().to_string();
        assert!(err.contains("环"), "多级环必须被拒：{err}");

        // 对照组：改成不存在的上级，仍然是「不存在」而不是「环」——
        // 两种错法要能分开，否则用户不知道自己错在哪。
        let e2 = mk(&db, "C01", Some("ZZZ"));
        let err2 = update(&db, &e2).unwrap_err().to_string();
        assert!(err2.contains("不存在"), "不存在的上级要说「不存在」：{err2}");
    }

    /// 删除前要知道有没有子节点 —— 删了父，子就成孤儿且没人知道。
    #[test]
    fn children_of_reports_subordinates() {
        let db = mem();
        insert(&db, &mk(&db, "C00", None)).unwrap();
        insert(&db, &mk(&db, "C01", Some("C00"))).unwrap();
        insert(&db, &mk(&db, "C02", Some("C00"))).unwrap();
        insert(&db, &mk(&db, "C03", Some("C01"))).unwrap();

        let mut kids = children_of(&db, AuxKind::Customer, "C00").unwrap();
        kids.sort();
        assert_eq!(kids, vec!["C01".to_string(), "C02".to_string()]);
        // 只返回**直接**子节点；孙节点不在内（调用方要自己决定是拒绝还是级联）
        assert!(
            children_of(&db, AuxKind::Customer, "C01").unwrap() == vec!["C03".to_string()]
        );
        assert!(children_of(&db, AuxKind::Customer, "C03").unwrap().is_empty());
    }

    /// find_parent_cycle：没有环时返回 None；有环时能指出环上的一个编码。
    ///
    /// 直接构造环（绕开 check_parent，因为 check_parent 会拦住）——
    /// 目的是验「历史脏数据能被检出」，那正是 find_parent_cycle 的用途。
    #[test]
    fn find_parent_cycle_detects_existing_cycle() {
        let db = mem();
        insert(&db, &mk(&db, "C01", None)).unwrap();
        insert(&db, &mk(&db, "C02", Some("C01"))).unwrap();
        assert_eq!(
            find_parent_cycle(&db, AuxKind::Customer).unwrap(),
            None,
            "干净的图不该报环"
        );

        // 绕过校验直接造环：C01 的上级改成 C02
        db.conn()
            .execute(
                "UPDATE aux_entity SET parent_code='C02' WHERE kind='customer' AND code='C01'",
                [],
            )
            .unwrap();
        let hit = find_parent_cycle(&db, AuxKind::Customer).unwrap();
        // `matches!` 会 move `hit`，而后面 assert 的格式化参数还要用它 —— 借用
        assert!(
            matches!(&hit, Some(c) if c == "C01" || c == "C02"),
            "应报出环上的编码，实际：{hit:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    /// 批量导入的核心约定：**不覆盖**。
    ///
    /// `import_many` 用的是 `INSERT OR REPLACE`，同名档案会被静默改掉 ——
    /// 用户粘一份旧 Excel 进来，其中一条编码和库里重了，客户名就悄悄变了，
    /// 而凭证上的历史金额对不上任何东西，之后极难排查。
    /// 所以这里必须保持「跳过并报告」。
    #[test]
    fn insert_many_skip_existing_不覆盖已有档案() {
        let db = mem();
        insert(&db, &AuxEntity::new(AuxKind::Customer, "C01", "原客户名")).unwrap();

        let items = vec![
            AuxEntity::new(AuxKind::Customer, "C01", "新客户名"),
            AuxEntity::new(AuxKind::Customer, "C02", "新客户乙"),
        ];
        let (n, skipped) = insert_many_skip_existing(&db, &items).unwrap();

        assert_eq!(n, 1, "只有 C02 该被导入");
        assert_eq!(skipped, vec!["C01".to_string()]);
        assert_eq!(
            get(&db, AuxKind::Customer, "C01").unwrap().unwrap().name,
            "原客户名",
            "已有档案的名字不能被批量导入改掉"
        );
        assert_eq!(get(&db, AuxKind::Customer, "C02").unwrap().unwrap().name, "新客户乙");
    }

    #[test]
    fn insert_many_skip_existing_空列表不报错() {
        let db = mem();
        let (n, skipped) = insert_many_skip_existing(&db, &[]).unwrap();
        assert_eq!(n, 0);
        assert!(skipped.is_empty());
    }

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

    /// 引用计数必须按 `kind=code` **整段**匹配。
    /// 回归：旧的 `%customer=C001%` 会把 `customer=C0011` 的引用也算进来，
    /// 于是 C001 明明没有任何凭证引用，却因"被引用"而删不掉。
    #[test]
    fn usage_counts_exact_code_only() {
        use fincore::{AuxRef, Entry, Money, Period, Voucher};
        let db = mem();
        let p = Period::new(2026, 1).unwrap();

        let mk = |cust: &str, no: i32| {
            let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            let mut v = Voucher::new(p, d, "记", no);
            v.prepared_by = "张三".to_string();
            v.push_entry(Entry {
                debit: Money::parse("100").unwrap(),
                aux: AuxRef {
                    customer: Some(cust.into()),
                    ..Default::default()
                },
                ..Entry::new(1, "112201", "应收货款")
            });
            v.push_entry(Entry {
                credit: Money::parse("100").unwrap(),
                ..Entry::new(2, "600101", "产品销售收入")
            });
            crate::vouchers::save(&db, &mut v).unwrap();
        };
        mk("C0011", 1);
        mk("C0011", 2);
        mk("ACME", 3);

        assert_eq!(
            usage(&db, AuxKind::Customer, "C001").unwrap(),
            0,
            "C001 未被引用，前缀相近的 C0011 / ACME 不得计入"
        );
        assert_eq!(usage(&db, AuxKind::Customer, "C0011").unwrap(), 2);
        assert_eq!(usage(&db, AuxKind::Customer, "ACME").unwrap(), 1);
        assert_eq!(usage(&db, AuxKind::Customer, "AC").unwrap(), 0);
        assert_eq!(usage(&db, AuxKind::Supplier, "C0011").unwrap(), 0, "维度不同不算");
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
