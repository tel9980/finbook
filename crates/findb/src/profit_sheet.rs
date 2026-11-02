//! 月份利润分析表：照抄厂里自己那张表的样子。
//!
//! 小厂不按会计准则的科目结构看利润。他们看的是一张固定清单：
//! 厂租、电费、水费、化工、染料封闭剂、工资、包装材料、货车运输费、外发加工费……
//! 这些名目在标准科目表里根本对不上——「化工」不是一个科目，「挂具费」也不是。
//!
//! 所以把**行做成数据**：每行挂若干科目编码，金额 = 这些科目本期发生额之和。
//! 要改口径就改这一行的科目，不用改代码，也不用等发版。
//!
//! # 为什么还要「按这张表生成科目」
//!
//! 只把行做出来是不够的：默认科目表里 `5101 制造费用` 只有工资/折旧/水电/办公/其他五个末级，
//! 化工、染料、维修费、挂具费全都得塞进「其他」，那样这张表永远是三行有数、十行空着。
//!
//! 所以给一个按钮：没挂科目的行，一键在 `5101` 下建一个同名末级科目（510106 化工、510107 染料封闭剂…），
//! 建完回写到行上。以后录凭证按这些科目记，这张表就自动出数。
//! 这是「先有表、后有科目」的顺序——小厂本来就是先想知道看什么，再去想怎么记。
//!
//! # 不重复计算
//!
//! 同一个科目只能挂在一行（保存时校验）。挂两行的话合计会翻倍，而报表看起来完全正常——
//! 这种错只有对账时才发现，所以宁可保存时直接拦下来。

use std::collections::BTreeSet;

use fincore::{AcctCategory, Money, Period};

use crate::{Db, DbError, DbResult};

/// 行类型
pub const KIND_REVENUE: &str = "revenue";
pub const KIND_EXPENSE: &str = "expense";

/// 默认行（照抄「氧化（老厂）月份利润分析表」）。
///
/// 字段：序号、科目（行名）、备注、类型。
/// 科目编码一律留空——由 [`create_accounts`] 按这张表生成，或者用户自己填。
/// 硬塞一个「最接近的科目」看着好看，实际会把化工、维修费、染料全算进同一个
/// 「制造费用-其他」，表上一行有数九行空着，比空着更误导。
pub const DEFAULT_ROWS: &[(i64, &str, &str, &str)] = &[
    (1, "收入：月总产量", "", KIND_REVENUE),
    (2, "厂租", "含宿租金及房东杂费", KIND_EXPENSE),
    (4, "电费", "", KIND_EXPENSE),
    (5, "水费", "", KIND_EXPENSE),
    (6, "化工", "亚钠、片碱、硝酸、磷酸、硫酸", KIND_EXPENSE),
    (7, "染料封闭剂", "染料、封闭剂、除油剂", KIND_EXPENSE),
    (8, "工资", "全体工资", KIND_EXPENSE),
    (9, "包装材料", "报纸、膜、胶袋", KIND_EXPENSE),
    (
        10,
        "货车运输费",
        "餐费、加油、充电、高速、维修保养费用",
        KIND_EXPENSE,
    ),
    (11, "外发加工费", "喷砂、抛光、拉丝费", KIND_EXPENSE),
    (12, "税金", "", KIND_EXPENSE),
    (13, "社保费", "", KIND_EXPENSE),
    (14, "业务费", "餐费及车油费", KIND_EXPENSE),
    (15, "办公用品费", "纸、笔、茶叶等", KIND_EXPENSE),
    (16, "维修费", "车间设备购买配件维修费用", KIND_EXPENSE),
    (17, "挂具费", "", KIND_EXPENSE),
];

/// 收入行默认挂的科目（这两条标准表里一定有）
const REVENUE_ACCOUNTS: &str = "6001,6051";

/// 生成缺失科目时挂在哪一级下面（制造费用）
const COST_PARENT: &str = "5101";

#[derive(Clone, Debug, serde::Serialize)]
pub struct Row {
    pub id: i64,
    /// 序号（照抄厂里那张表）
    pub seq: i64,
    /// 科目（行名）
    pub label: String,
    /// 备注（这一行含哪些内容）
    pub hint: String,
    /// revenue / expense
    pub kind: String,
    /// 挂的科目编码，逗号分隔
    pub accounts: String,
    pub disabled: bool,
}

impl Row {
    pub fn is_revenue(&self) -> bool {
        self.kind == KIND_REVENUE
    }

    /// 拆成科目编码列表（逗号/中文逗号/空格都认，去重保序）
    pub fn codes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for part in self
            .accounts
            .split([',', '，', ' ', '\t', ';', '；'])
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            if !out.iter().any(|x| x == part) {
                out.push(part.to_string());
            }
        }
        out
    }
}

/// 表上的一行（算完金额之后）
#[derive(Clone, Debug, serde::Serialize)]
pub struct Line {
    pub seq: i64,
    pub label: String,
    pub hint: String,
    pub amount: Money,
    /// 收入行的「月总产量」（本月完工数量）；其他行为空。
    /// 厂里那张表第一行写的是「收入：月总产量」——产量和收入并排看才有意义。
    pub qty: Option<Money>,
    /// 这一行还没挂科目（金额必然为 0，界面要提示「你的账里这笔记在哪」）
    pub unmapped: bool,
}

/// 整张表
#[derive(Clone, Debug, serde::Serialize)]
pub struct Sheet {
    pub period: Period,
    pub rows: Vec<Line>,
    pub revenue: Money,
    pub expense: Money,
    pub profit: Money,
    /// 未挂科目的行数
    pub unmapped: usize,
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 首次使用时把默认行灌进去（表里有行就不动，避免覆盖用户改过的口径）
pub fn ensure_defaults(db: &Db) -> DbResult<()> {
    let n: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM profit_sheet_row", [], |r| r.get(0))?;
    if n > 0 {
        return Ok(());
    }
    let tx = db.write_tx()?;
    for (seq, label, hint, kind) in DEFAULT_ROWS {
        // 收入行直接挂上标准收入科目；其余留空，等「按这张表生成科目」
        let accounts = if *kind == KIND_REVENUE {
            REVENUE_ACCOUNTS
        } else {
            ""
        };
        tx.execute(
            "INSERT INTO profit_sheet_row(seq,label,hint,kind,accounts,disabled) VALUES(?1,?2,?3,?4,?5,0)",
            rusqlite::params![seq, label, hint, kind, accounts],
        )?;
    }
    tx.commit()?;
    let _ = now();
    Ok(())
}

const COLS: &str = "id,seq,label,hint,kind,accounts,disabled";

fn map_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        seq: r.get(1)?,
        label: r.get(2)?,
        hint: r.get(3)?,
        kind: r.get(4)?,
        accounts: r.get(5)?,
        disabled: r.get::<_, i64>(6)? != 0,
    })
}

pub fn list(db: &Db) -> DbResult<Vec<Row>> {
    ensure_defaults(db)?;
    let mut stmt = db
        .conn()
        .prepare(&format!("SELECT {COLS} FROM profit_sheet_row ORDER BY seq"))?;
    let rows = stmt.query_map([], |r| map_row(r))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn get(db: &Db, id: i64) -> DbResult<Option<Row>> {
    let mut stmt = db
        .conn()
        .prepare(&format!("SELECT {COLS} FROM profit_sheet_row WHERE id=?1"))?;
    let mut rows = stmt.query_map([id], |r| map_row(r))?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// 校验：序号/行名必填，类型只能是 revenue/expense，**同一科目不许挂两行**
fn check(db: &Db, row: &Row) -> DbResult<()> {
    if row.label.trim().is_empty() {
        return Err(DbError::Fin(fincore::FinError::validate(
            "科目（行名）必填",
        )));
    }
    if row.kind != KIND_REVENUE && row.kind != KIND_EXPENSE {
        return Err(DbError::Fin(fincore::FinError::validate(format!(
            "行类型不认识：{}（只能是 revenue / expense）",
            row.kind
        ))));
    }
    if row.seq <= 0 {
        return Err(DbError::Fin(fincore::FinError::validate("序号必须大于 0")));
    }
    // 同一科目挂两行 → 合计翻倍，而报表看起来完全正常。
    let mine: BTreeSet<String> = row.codes().into_iter().collect();
    if mine.is_empty() {
        return Ok(());
    }
    for other in list(db)? {
        if other.id == row.id {
            continue;
        }
        for code in other.codes() {
            if mine.contains(&code) {
                return Err(DbError::Fin(fincore::FinError::validate(format!(
                    "科目 {code} 已经挂在第 {} 行「{}」上了：同一科目挂两行，合计会翻倍",
                    other.seq, other.label
                ))));
            }
        }
    }
    Ok(())
}

pub fn save(db: &Db, row: &mut Row) -> DbResult<i64> {
    check(db, row)?;
    let accounts = row.codes().join(",");
    if row.id > 0 {
        db.conn().execute(
            "UPDATE profit_sheet_row SET seq=?2,label=?3,hint=?4,kind=?5,accounts=?6,disabled=?7 WHERE id=?1",
            rusqlite::params![
                row.id,
                row.seq,
                row.label.trim(),
                row.hint.trim(),
                row.kind,
                accounts,
                if row.disabled { 1 } else { 0 },
            ],
        )?;
        row.accounts = accounts;
        Ok(row.id)
    } else {
        db.conn().execute(
            "INSERT INTO profit_sheet_row(seq,label,hint,kind,accounts,disabled) VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                row.seq,
                row.label.trim(),
                row.hint.trim(),
                row.kind,
                accounts,
                if row.disabled { 1 } else { 0 },
            ],
        )?;
        row.id = db.conn().last_insert_rowid();
        row.accounts = accounts;
        Ok(row.id)
    }
}

pub fn delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM profit_sheet_row WHERE id=?1", [id])?;
    Ok(())
}

/// 恢复默认行（会清掉用户改过的行，只给「重置」按钮用）
pub fn reset_defaults(db: &Db) -> DbResult<usize> {
    db.conn().execute("DELETE FROM profit_sheet_row", [])?;
    ensure_defaults(db)?;
    Ok(DEFAULT_ROWS.len())
}

/// 给还没挂科目的行，在 `5101 制造费用` 下建一个同名末级科目并回写到行上。
///
/// 返回 (新建科目数, 回写行数)。
pub fn create_accounts(db: &Db) -> DbResult<(usize, usize)> {
    ensure_defaults(db)?;
    let chart = crate::accounts::chart(db)?;
    let existing = crate::accounts::list(db)?;
    // 下一个可用的 5101xx 编号
    let mut next: i64 = existing
        .iter()
        .filter(|a| a.code.starts_with(COST_PARENT) && a.code.len() == COST_PARENT.len() + 2)
        .filter_map(|a| a.code[COST_PARENT.len()..].parse::<i64>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    let mut created = 0usize;
    let mut touched = 0usize;
    for mut row in list(db)? {
        if row.disabled || row.is_revenue() {
            continue;
        }
        if !row.codes().is_empty() {
            continue;
        }
        // 建一个与行同名的末级费用科目：以后录凭证按它记，这张表就自动出数
        let code = format!("{COST_PARENT}{next:02}");
        next += 1;
        if chart.get(&code).is_some() {
            continue;
        }
        let acc = fincore::Account::new(code.clone(), row.label.trim(), AcctCategory::Cost);
        crate::accounts::insert(db, &acc)?;
        created += 1;
        row.accounts = code;
        save(db, &mut row)?;
        touched += 1;
    }
    Ok((created, touched))
}

/// 算出整张表。
pub fn report(db: &Db, period: Period) -> DbResult<Sheet> {
    use crate::balances::{BalanceQuery, BalanceSnapshot};

    ensure_defaults(db)?;
    // 排除结转损益凭证：结完账后损益科目借贷相抵，否则整张表会变成一片 0
    let snap = BalanceSnapshot::load(db, &BalanceQuery::period(period).with_exclude_carry(true))?;
    let rows_all = list(db)?;

    // 本月完工数量：厂里那张表第一行写的是「收入：月总产量」
    let mut output_qty = Money::ZERO;
    for o in crate::scm::prod_list(db, period, None)? {
        output_qty += o.completed_qty;
    }

    let mut lines: Vec<Line> = Vec::new();
    let mut revenue = Money::ZERO;
    let mut expense = Money::ZERO;
    let mut unmapped = 0usize;
    for row in rows_all.iter().filter(|r| !r.disabled) {
        let codes = row.codes();
        let mut amount = Money::ZERO;
        for code in &codes {
            let r = snap.for_account(code, None);
            // 收入类看贷方发生额，费用类看借方发生额
            amount += if row.is_revenue() {
                r.credit - r.debit
            } else {
                r.debit - r.credit
            };
        }
        let is_unmapped = codes.is_empty();
        if is_unmapped {
            unmapped += 1;
        }
        if row.is_revenue() {
            revenue += amount;
        } else {
            expense += amount;
        }
        lines.push(Line {
            seq: row.seq,
            label: row.label.clone(),
            hint: row.hint.clone(),
            amount,
            qty: if row.is_revenue() {
                Some(output_qty)
            } else {
                None
            },
            unmapped: is_unmapped,
        });
    }

    Ok(Sheet {
        period,
        rows: lines,
        revenue,
        expense,
        profit: revenue - expense,
        unmapped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use crate::vouchers;
    use chrono::NaiveDate;
    use fincore::{Entry, Voucher};

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    fn post(db: &Db, period: Period, day: u32, lines: Vec<(&str, &str, &str)>) {
        let date = NaiveDate::from_ymd_opt(period.year(), period.month(), day).unwrap();
        let mut v = Voucher::new(
            period,
            date,
            "记",
            crate::vouchers::next_no(db, period, "记").unwrap(),
        );
        let mut no = 1;
        for (code, debit, credit) in lines {
            let mut e = Entry::new(no, code, "利润分析表");
            e.debit = m(debit);
            e.credit = m(credit);
            v.entries.push(e);
            no += 1;
        }
        let id = vouchers::save(db, &mut v).unwrap();
        vouchers::post(db, id, "t").unwrap();
    }

    fn line<'a>(s: &'a Sheet, label: &str) -> &'a Line {
        s.rows
            .iter()
            .find(|r| r.label == label)
            .unwrap_or_else(|| panic!("没有 {label} 这一行"))
    }

    #[test]
    fn 默认行照抄厂里那张表并且收入行挂上收入科目() {
        let db = mem();
        let rows = list(&db).unwrap();
        assert_eq!(rows.len(), DEFAULT_ROWS.len());
        assert_eq!(rows[0].seq, 1);
        assert_eq!(rows[0].label, "收入：月总产量");
        assert!(rows[0].is_revenue());
        assert_eq!(rows[0].codes(), vec!["6001", "6051"]);
        // 支出行默认不挂科目——硬塞「最接近的科目」会把化工和维修费算进同一行
        assert!(rows[1].codes().is_empty(), "厂租默认应该空着等生成科目");
        assert_eq!(rows[1].hint, "含宿租金及房东杂费");
        // 表里没有 3 号（原表就跳号），照抄
        assert!(!rows.iter().any(|r| r.seq == 3));
    }

    #[test]
    fn 按这张表生成科目后每行都有独立科目且不重复() {
        let db = mem();
        let (created, touched) = create_accounts(&db).unwrap();
        // 15 个支出行，每行一个新科目
        assert_eq!(created, 15, "每个支出行都该有一个独立科目");
        assert_eq!(touched, 15);
        let rows = list(&db).unwrap();
        let mut all: Vec<String> = Vec::new();
        for r in rows.iter().filter(|r| !r.is_revenue()) {
            let codes = r.codes();
            assert_eq!(codes.len(), 1, "{} 应该恰好挂一个科目", r.label);
            all.push(codes[0].clone());
        }
        let uniq: BTreeSet<&String> = all.iter().collect();
        assert_eq!(uniq.len(), all.len(), "科目不许重复");
        // 科目名与行名一致，录凭证时才认得出
        let chart = crate::accounts::chart(&db).unwrap();
        for r in rows.iter().filter(|r| !r.is_revenue()) {
            let code = &r.codes()[0];
            assert_eq!(chart.get(code).unwrap().name, r.label);
        }
        // 再点一次不会重复建
        let (again, _) = create_accounts(&db).unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn 利润分析表按行取数并算出净利润() {
        let db = mem();
        let p = Period::from_ymm(202601);
        create_accounts(&db).unwrap();
        let rows = list(&db).unwrap();
        let code_of =
            |label: &str| rows.iter().find(|r| r.label == label).unwrap().codes()[0].clone();
        let rent = code_of("厂租");
        let power = code_of("电费");
        let chem = code_of("化工");
        let wages = code_of("工资");

        // 收入 10000
        post(
            &db,
            p,
            5,
            vec![("1001", "10000", "0"), ("600101", "0", "10000")],
        );
        // 厂租 3000 / 电费 800 / 化工 1200 / 工资 4000
        post(&db, p, 6, vec![(&rent, "3000", "0"), ("1001", "0", "3000")]);
        post(&db, p, 7, vec![(&power, "800", "0"), ("1001", "0", "800")]);
        post(&db, p, 8, vec![(&chem, "1200", "0"), ("1001", "0", "1200")]);
        post(
            &db,
            p,
            9,
            vec![(&wages, "4000", "0"), ("1001", "0", "4000")],
        );

        let s = report(&db, p).unwrap();
        assert_eq!(s.revenue, m("10000"));
        assert_eq!(line(&s, "厂租").amount, m("3000"));
        assert_eq!(line(&s, "电费").amount, m("800"));
        assert_eq!(line(&s, "化工").amount, m("1200"));
        assert_eq!(line(&s, "工资").amount, m("4000"));
        // 没记账的行金额为 0，不是报错
        assert_eq!(line(&s, "水费").amount, m("0"));
        assert_eq!(s.expense, m("9000"));
        assert_eq!(s.profit, m("1000"));
        assert_eq!(s.unmapped, 0);
        // 收入行带月总产量
        assert!(line(&s, "收入：月总产量").qty.is_some());
    }

    #[test]
    fn 同一科目挂两行要报错() {
        let db = mem();
        create_accounts(&db).unwrap();
        let rows = list(&db).unwrap();
        let mut water = rows.iter().find(|r| r.label == "水费").unwrap().clone();
        let power_code = rows.iter().find(|r| r.label == "电费").unwrap().codes()[0].clone();
        water.accounts = power_code.clone();
        let err = save(&db, &mut water).unwrap_err().to_string();
        assert!(err.contains("合计会翻倍"), "错误要说清后果：{err}");
    }

    #[test]
    fn 未挂科目的行会被标出来() {
        let db = mem();
        let p = Period::from_ymm(202601);
        let s = report(&db, p).unwrap();
        // 一个科目都还没建：15 个支出行全空着
        assert_eq!(s.unmapped, 15);
        assert!(line(&s, "厂租").unmapped);
        assert!(!line(&s, "收入：月总产量").unmapped);
        assert_eq!(s.profit, m("0"));
    }
}
