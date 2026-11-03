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
    /// 这一行是不是收入行。界面靠它判断「行次从 1 重排」，
    /// 不能再拿 `qty.is_some()` 兼职——没录生产工单的账套产量是空的。
    pub is_revenue: bool,
    /// 收入行的「月总产量」（本月完工数量）；支出行、以及**没录生产工单的账套**都为空。
    /// 厂里那张表第一行写的是「收入：月总产量」——产量和收入并排看才有意义，
    /// 没有工单时宁可留空，也不要摆一个假 0.00。
    pub qty: Option<Money>,
    /// 这一行还没挂科目（金额必然为 0，界面要提示「你的账里这笔记在哪」）
    pub unmapped: bool,
}

/// 没落到表上任何一行的费用。
///
/// 这是「体检」用的：这张表是照厂里那张纸抄的，纸上有 16 行，
/// 账上多出来的科目（折旧费、手续费……）不会自己长到表上。
/// 不列出来的话，支出合计少了钱也看不出来——表看起来完全正常。
#[derive(Clone, Debug, serde::Serialize)]
pub struct Uncovered {
    pub code: String,
    pub name: String,
    pub amount: Money,
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
    /// 没落到任何一行上的费用合计（体检用，**不参与**上面的支出/净利）
    pub uncovered: Money,
    /// 没落到任何一行上的费用明细，金额大的在前
    pub uncovered_rows: Vec<Uncovered>,
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
    let rows = stmt.query_map([], map_row)?;
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
    let mut rows = stmt.query_map([id], map_row)?;
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

/// 给还没挂科目的行挂上科目：**先认现有科目，认不出才新建**。
///
/// 老版本只会闷头建同名科目。真实账套上点两次，`5101` 就从 15 个末级变成 30 个：
/// 厂里自己的「厂房租金」和新造的「厂租」并排躺着，同一个月的房租记哪边都行，
/// 两边一分开，这张表上永远只剩一半——比空着更难查。
///
/// 所以顺序反过来：
/// 1. 先按 [`auto_map`] 那套关键字去现有科目里找（只认末级、一个科目只挂一行）；
/// 2. 实在找不到，才在 `5101 制造费用` 下建一个与行同名的末级科目。
///
/// 返回 (新建科目数, 挂上科目的行数)。
pub fn create_accounts(db: &Db) -> DbResult<(usize, usize)> {
    ensure_defaults(db)?;
    let mut chart = crate::accounts::chart(db)?;
    let mut existing = crate::accounts::list(db)?;
    let rows = list(db)?;

    // 已经被任何一行挂着的科目，不能再挂到别处
    let mut used: BTreeSet<String> = BTreeSet::new();
    for r in &rows {
        for c in r.codes() {
            used.insert(c);
        }
    }

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
    for mut row in rows {
        if row.disabled || row.is_revenue() {
            continue;
        }
        if !row.codes().is_empty() {
            continue;
        }
        // 1) 账里已经有对得上的科目：挂上去，别再造成第二套
        let (mut hit, _busy) = candidates(&chart, &existing, &row.label, false, &used);
        if hit.is_empty() {
            // 2) 认不出来才建：以后录凭证按它记，这张表就自动出数
            let code = format!("{COST_PARENT}{next:02}");
            next += 1;
            if chart.get(&code).is_some() {
                continue;
            }
            let name = row.label.trim().to_string();
            let acc = fincore::Account::new(code.clone(), &name, AcctCategory::Cost);
            crate::accounts::insert(db, &acc)?;
            chart = crate::accounts::chart(db)?;
            existing = crate::accounts::list(db)?;
            created += 1;
            hit.push((code, name));
        }
        let codes: Vec<String> = hit.iter().map(|(c, _)| c.clone()).collect();
        for c in &codes {
            used.insert(c.clone());
        }
        row.accounts = codes.join(",");
        save(db, &mut row)?;
        touched += 1;
    }
    Ok((created, touched))
}

/// 一个行名在现有科目里能挂哪几个（只认末级、类别要对）。
///
/// 返回 (能挂的, 名字像但已经被别的行占着的)。后者是给界面说人话用的：
/// 「不是账里没有，是被别的行占了」——不然用户会以为自动挂科目坏了。
fn candidates(
    chart: &fincore::Chart,
    all: &[fincore::Account],
    label: &str,
    want_income: bool,
    used: &BTreeSet<String>,
) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let kws = keywords_for(label);
    let mut added: Vec<(String, String)> = Vec::new();
    let mut busy: Vec<(String, String)> = Vec::new();
    for a in all {
        if !chart.is_leaf(&a.code) {
            continue;
        }
        let cat_ok = if want_income {
            a.category == AcctCategory::Income
        } else {
            matches!(a.category, AcctCategory::Cost | AcctCategory::Expense)
        };
        if !cat_ok || !kws.iter().any(|k| a.name.contains(k.as_str())) {
            continue;
        }
        if used.contains(&a.code) {
            busy.push((a.code.clone(), a.name.clone()));
        } else {
            added.push((a.code.clone(), a.name.clone()));
        }
    }
    added.sort_by(|a, b| a.0.cmp(&b.0));
    added.truncate(MAX_PER_ROW);
    (added, busy)
}

/// 每一行按什么关键字去找现有科目。
///
/// 名字取自厂里那张表，关键字取得比行名宽一点（「货车运输费」对「运输/运费/加油」），
/// 但**不用单字**——「电」会把电话费、电焊机都捞进来。
const KEYWORDS: &[(&str, &[&str])] = &[
    ("厂租", &["厂租", "房租", "租金", "租赁"]),
    ("电费", &["电费", "水电"]),
    ("水费", &["水费", "水电"]),
    ("化工", &["化工", "化学", "药剂"]),
    ("染料", &["染料", "封闭剂", "除油"]),
    ("工资", &["工资", "薪酬", "薪金"]),
    ("包装", &["包装"]),
    ("货车运输", &["运输", "运费", "加油", "油费", "车辆"]),
    ("外发加工", &["加工费", "委外", "外协", "外发"]),
    ("税金", &["税金", "税费", "附加", "增值税"]),
    (
        "社保",
        &["社保", "社会保险", "养老", "医疗", "工伤", "失业"],
    ),
    ("业务费", &["业务招待", "招待", "业务费"]),
    ("办公用品", &["办公"]),
    ("维修", &["维修", "修理", "配件"]),
    ("挂具", &["挂具", "夹具", "模具"]),
    ("收入", &["主营业务收入", "其他业务收入", "收入"]),
];

/// 一行最多自动挂几个科目。挂太多多半是关键字太宽（比如「材料」把化工和包装都捞进来）。
const MAX_PER_ROW: usize = 3;

fn keywords_for(label: &str) -> Vec<String> {
    let l = label
        .replace("支出：", "")
        .replace("收入：", "")
        .trim()
        .to_string();
    for (key, kws) in KEYWORDS {
        if l.contains(key) {
            return kws.iter().map(|s| s.to_string()).collect();
        }
    }
    // 认不出来的行名，就拿行名本身去找
    vec![l]
}

/// 自动挂科目：一行结果
#[derive(Clone, Debug, serde::Serialize)]
pub struct AutoRow {
    pub seq: i64,
    pub label: String,
    /// 这次要挂上去的科目（编码, 名称）
    pub added: Vec<(String, String)>,
    /// 本来就有科目、这次没动的
    pub kept: String,
    /// 给用户看的一句话
    pub note: String,
}

/// 自动挂科目的结果
#[derive(Clone, Debug, serde::Serialize)]
pub struct AutoMap {
    pub rows: Vec<AutoRow>,
    /// 没找到对应科目的行（序号, 行名）
    pub unmatched: Vec<(i64, String)>,
    /// 这次改了几行
    pub touched: usize,
    /// 真的写库了吗（false = 只是预览）
    pub applied: bool,
}

/// 按**现有科目名**把这张表的行挂上科目。
///
/// 为什么先预览再落地：猜错了不会报错，只会让某一行金额偏大、另一行偏小，
/// 合计还是对的——这种错最难发现。所以 `apply = false` 只算不写，
/// 界面上先把「哪行挂哪几个科目」摆出来，人看过再点应用。
///
/// 三条硬规则：
/// 1. 只认**末级**科目（挂上级等于把下级一起算进来，会和别的行重复）；
/// 2. 一个科目只能挂一行（`used` 集合，含本来就被别人挂着的）；
/// 3. 支出行只认成本/费用类、收入行只认收入类（不然「应收账款」会被「收入」两个字捞进来）。
pub fn auto_map(db: &Db, apply: bool) -> DbResult<AutoMap> {
    ensure_defaults(db)?;
    let chart = crate::accounts::chart(db)?;
    let all = crate::accounts::list(db)?;
    let rows = list(db)?;

    // 已经被任何一行挂着的科目，不能再挂到别处
    let mut used: BTreeSet<String> = BTreeSet::new();
    for r in &rows {
        for c in r.codes() {
            used.insert(c);
        }
    }

    let mut out = AutoMap {
        rows: Vec::new(),
        unmatched: Vec::new(),
        touched: 0,
        applied: apply,
    };
    for mut row in rows {
        if row.disabled {
            continue;
        }
        if !row.codes().is_empty() {
            out.rows.push(AutoRow {
                seq: row.seq,
                label: row.label.clone(),
                added: Vec::new(),
                kept: row.accounts.clone(),
                note: "已经有科目，没动".to_string(),
            });
            continue;
        }
        // 名字像、但已经被别的行挂走的科目：`busy` 单独记下来，
        // 好告诉用户「不是没有，是被占了」
        let (mut added, busy) = candidates(&chart, &all, &row.label, row.is_revenue(), &used);
        if added.is_empty() {
            let note = if busy.is_empty() {
                "现有科目里没有像它的名字：点「按这张表生成科目」新建，或手工填".to_string()
            } else {
                let names: Vec<String> = busy.iter().map(|(c, n)| format!("{c} {n}")).collect();
                format!(
                    "{} 已经挂在别的行上了：一个科目只能挂一行，要分开看就得拆科目",
                    names.join("、")
                )
            };
            if busy.is_empty() {
                out.unmatched.push((row.seq, row.label.clone()));
            }
            out.rows.push(AutoRow {
                seq: row.seq,
                label: row.label.clone(),
                added: Vec::new(),
                kept: String::new(),
                note,
            });
            continue;
        }
        added.sort_by(|a, b| a.0.cmp(&b.0));
        let codes: Vec<String> = added.iter().map(|(c, _)| c.clone()).collect();
        row.accounts = codes.join(",");
        if apply {
            save(db, &mut row)?;
            out.touched += 1;
        }
        for c in &codes {
            used.insert(c.clone());
        }
        out.rows.push(AutoRow {
            seq: row.seq,
            label: row.label.clone(),
            added,
            kept: String::new(),
            note: format!("挂上 {} 个科目", codes.len()),
        });
    }
    Ok(out)
}

/// 结转时「钱去哪了」的目标科目：生产成本、库存商品、发出商品、主营业务成本。
///
/// 月末把制造费用转进这些科目，是**做账的分摊动作**，不是又花了一笔钱。
/// 判定依据就是「同一张凭证上，这些科目在借方」（见 [`carry_out`]）。
const CARRY_TARGETS: &[&str] = &["5001", "1405", "1406", "6401"];

/// 体检范围：这几科里的钱没落到任何一行上，就是「表外的费用」。
///
/// 刻意不含 `5001 生产成本`（在产品余额不一定是本月花的钱，列出来会和
/// 制造费用那几行重复喊）和 `6401 主营业务成本`（它就是表上 16 行费用的汇总，
/// 再列一次会让人以为漏了一大笔）。
const COVER_SCOPE: &[&str] = &["5101", "5401", "6403", "6601", "6602", "6603", "6711"];

fn is_carry_target(code: &str) -> bool {
    CARRY_TARGETS.iter().any(|p| code.starts_with(p))
}

/// 本期「转出到成本/存货」的贷方发生额，按科目编码累计。
///
/// 这张表要看的是**本月实际花了多少**，不是账上这个科目还剩多少。
/// 月末制造费用会结转进生产成本/主营业务成本（自动转账模板「结转制造费用」、
/// 手工转账「结转本月加工成本」都会做这件事），结转凭证上这批科目借方一分没有、
/// 贷方一大笔——直接取「借方 − 贷方」，当月花掉的钱被全额抵成 0。
/// 演示账 202611 就是这样：16 行支出全是 0.00，净利等于全部收入 450,000。
///
/// 所以支出行 = (借方 − 贷方) + **转出到成本的那部分贷方**：
/// - 结转过账的月份，行上仍然是这个月实际花掉的钱；
/// - 红字冲销对（原凭证 + 反向冲销凭证）两边都没有成本借方，贷方不会被加回，
///   天然抵销成 0，不用去猜哪张是冲销。
fn carry_out(db: &Db, period: Period) -> DbResult<std::collections::BTreeMap<String, Money>> {
    let mut stmt = db.conn().prepare(
        "SELECT e.voucher_id, e.account_code, e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON e.voucher_id = v.id
         WHERE e.period = ?1 AND v.status <> 'void' AND v.source <> 'carry_forward'",
    )?;
    let mut rows = stmt.query([period.ymm()])?;
    let mut scanned: Vec<(i64, String, Money, Money)> = Vec::new();
    while let Some(r) = rows.next()? {
        scanned.push((
            r.get(0)?,
            r.get(1)?,
            crate::read_money(r, 2)?,
            crate::read_money(r, 3)?,
        ));
    }
    drop(rows);
    // 借方落在成本/存货科目上的凭证 = 结转凭证
    let mut carry_vouchers: BTreeSet<i64> = BTreeSet::new();
    for (vid, code, debit, _) in &scanned {
        if debit.is_positive() && is_carry_target(code) {
            carry_vouchers.insert(*vid);
        }
    }
    let mut out: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    for (vid, code, _, credit) in &scanned {
        if credit.is_positive() && carry_vouchers.contains(vid) {
            *out.entry(code.clone()).or_insert(Money::ZERO) += *credit;
        }
    }
    Ok(out)
}

/// 这个科目（含下级）的转出额：`5101` 挂行时要把 `510106` 的转出也算上。
fn carry_out_of(map: &std::collections::BTreeMap<String, Money>, code: &str) -> Money {
    let mut sum = Money::ZERO;
    for (c, v) in map {
        if c == code || c.starts_with(code) {
            sum += *v;
        }
    }
    sum
}

/// 算出整张表。
pub fn report(db: &Db, period: Period) -> DbResult<Sheet> {
    use crate::balances::{BalanceQuery, BalanceSnapshot};

    ensure_defaults(db)?;
    // 排除结转损益凭证：结完账后损益科目借贷相抵，否则整张表会变成一片 0
    let snap = BalanceSnapshot::load(db, &BalanceQuery::period(period).with_exclude_carry(true))?;
    let rows_all = list(db)?;
    // 转出到成本的贷方：结转过账的月份，靠它把「借方发生额」补回来
    let carried = carry_out(db, period)?;

    // 本月完工数量：厂里那张表第一行写的是「收入：月总产量」。
    // 这账套没录生产工单时，宁可留空（None），也不要摆一个假 0.00——
    // 用户看到 0.00 会以为「产量还没算，是不是软件坏了」。
    let mut output_qty = Money::ZERO;
    let mut has_orders = false;
    for o in crate::scm::prod_list(db, period, None)? {
        output_qty += o.completed_qty;
        has_orders = true;
    }

    let mut lines: Vec<Line> = Vec::new();
    let mut revenue = Money::ZERO;
    let mut expense = Money::ZERO;
    let mut unmapped = 0usize;
    // 表上已经被行认领的科目（体检时排除）
    let mut mapped: Vec<String> = Vec::new();
    for row in rows_all.iter().filter(|r| !r.disabled) {
        let codes = row.codes();
        let mut amount = Money::ZERO;
        for code in &codes {
            let r = snap.for_account(code, None);
            // 收入类看贷方发生额；费用类看「实际发生额」= 借方 − 贷方 + 转出到成本的部分
            amount += if row.is_revenue() {
                r.credit - r.debit
            } else {
                r.debit - r.credit + carry_out_of(&carried, code)
            };
            mapped.push(code.clone());
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
            is_revenue: row.is_revenue(),
            qty: if row.is_revenue() && has_orders {
                Some(output_qty)
            } else {
                None
            },
            unmapped: is_unmapped,
        });
    }

    // 表外体检：账上这几科（[`COVER_SCOPE`]）的费用，有没有一行都没认领的。
    // 只看**末级**科目：`snap.for_account` 会聚合下级，上下级各算一次就成了双份。
    let chart = crate::accounts::chart(db)?;
    let mut uncovered_rows: Vec<Uncovered> = Vec::new();
    let mut uncovered = Money::ZERO;
    for a in crate::accounts::list(db)? {
        // 停用的科目不算：它本来就不该再有发生额，报出来只会让人去翻旧账
        if a.disabled || !chart.is_leaf(&a.code) {
            continue;
        }
        if !COVER_SCOPE.iter().any(|p| a.code.starts_with(p)) {
            continue;
        }
        if mapped.iter().any(|m| a.code == *m || a.code.starts_with(m.as_str())) {
            continue;
        }
        let r = snap.for_account(&a.code, None);
        let amount = if a.category == AcctCategory::Income {
            r.credit - r.debit
        } else {
            r.debit - r.credit + carry_out_of(&carried, &a.code)
        };
        if amount.is_zero() {
            continue;
        }
        uncovered += amount;
        uncovered_rows.push(Uncovered {
            code: a.code.clone(),
            name: a.name.clone(),
            amount,
        });
    }
    uncovered_rows.sort_by(|a, b| b.amount.cmp(&a.amount).then(a.code.cmp(&b.code)));

    Ok(Sheet {
        period,
        rows: lines,
        revenue,
        expense,
        profit: revenue - expense,
        unmapped,
        uncovered,
        uncovered_rows,
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
        for (i, (code, debit, credit)) in lines.into_iter().enumerate() {
            let mut e = Entry::new(i as i32 + 1, code, "利润分析表");
            e.debit = m(debit);
            e.credit = m(credit);
            v.entries.push(e);
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
    fn 生成科目先认现有科目不会造出第二套() {
        let db = mem();
        let before: BTreeSet<String> = crate::accounts::list(&db)
            .unwrap()
            .into_iter()
            .map(|a| a.code)
            .collect();
        let (created, touched) = create_accounts(&db).unwrap();
        // 15 个支出行全部挂上
        assert_eq!(touched, 15);
        let fresh: BTreeSet<String> = crate::accounts::list(&db)
            .unwrap()
            .into_iter()
            .map(|a| a.code)
            .filter(|c| !before.contains(c))
            .collect();
        assert_eq!(fresh.len(), created, "只该新增 created 个科目");
        assert!(created < 15, "科目表里有现成的，不该一行造一个：{created}");
        let rows = list(&db).unwrap();
        // 标准科目表里已经有「水电费」：电费行直接挂现成的
        let power = rows.iter().find(|r| r.seq == 4).unwrap();
        assert_eq!(power.codes(), vec!["510103"]);
        // 厂租：厂里口语叫「厂租」，账上叫「租赁费」，挂现成的
        let rent = rows.iter().find(|r| r.seq == 2).unwrap();
        assert!(
            rent.codes().iter().any(|c| c == "660108"),
            "厂租该挂到现成的租赁费上：{:?}",
            rent.codes()
        );
        // 每行都有科目，全局不重复
        let mut all: Vec<String> = Vec::new();
        for r in rows.iter().filter(|r| !r.is_revenue()) {
            let codes = r.codes();
            assert!(!codes.is_empty(), "{} 没挂上科目", r.label);
            all.extend(codes);
        }
        let uniq: BTreeSet<&String> = all.iter().collect();
        assert_eq!(uniq.len(), all.len(), "科目不许重复");
        // 认不出来的才新建，名字与行名一致，录凭证时才认得出
        let chart = crate::accounts::chart(&db).unwrap();
        for r in rows.iter().filter(|r| !r.is_revenue()) {
            for c in r.codes() {
                if fresh.contains(&c) {
                    assert_eq!(chart.get(&c).unwrap().name, r.label);
                }
            }
        }
        // 再点一次：什么都不该变
        let (again_created, again_touched) = create_accounts(&db).unwrap();
        assert_eq!((again_created, again_touched), (0, 0));
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
        // 收入行：这账套没录生产工单，产量留空（不许摆一个假 0.00），
        // 但「是不是收入行」这件事必须自己说清楚——界面靠它重排行次
        let rev = line(&s, "收入：月总产量");
        assert!(rev.is_revenue);
        assert!(rev.qty.is_none());
        assert!(!line(&s, "厂租").is_revenue);
    }

    #[test]
    fn 自动挂科目按现有科目名找末级科目且不重复() {
        let db = crate::tests::mem();
        let preview = auto_map(&db, false).unwrap();
        assert!(!preview.applied);
        assert_eq!(preview.touched, 0);

        let find = |seq: i64| preview.rows.iter().find(|r| r.seq == seq).unwrap().clone();

        // 电费 → 510103 水电费（末级）
        let e = find(4);
        assert_eq!(e.added.len(), 1, "电费只该挂一个：{:?}", e.added);
        assert_eq!(e.added[0].0, "510103");

        // 水费 → 名字像的只有同一个「水电费」，已被电费占了：不许重复挂
        let w = find(5);
        assert!(w.added.is_empty());
        assert!(w.note.contains("已经挂在别的行"), "{}", w.note);

        // 工资 → 510101 / 660101 / 660201 三个「工资」，最多挂 3 个
        let g = find(8);
        assert_eq!(g.added.len(), 3, "{:?}", g.added);
        assert_eq!(g.added[0].0, "510101");

        // 化工这类标准表里根本没有的名字，不许硬塞一个「其他」
        assert!(find(6).added.is_empty());
        assert!(preview
            .unmatched
            .iter()
            .any(|(s, l)| *s == 6 && l == "化工"));

        // 预览不写库
        assert!(list(&db)
            .unwrap()
            .iter()
            .all(|r| r.seq == 1 || r.accounts.is_empty()));

        // 落地
        let applied = auto_map(&db, true).unwrap();
        assert!(applied.applied);
        assert!(applied.touched >= 5, "{}", applied.touched);
        let rows = list(&db).unwrap();
        let electric = rows.iter().find(|r| r.seq == 4).unwrap();
        assert_eq!(electric.accounts, "510103");
        // 再跑一次：已经挂好的行不动（幂等）
        assert_eq!(auto_map(&db, true).unwrap().touched, 0);

        // 全局不重复
        let mut seen = BTreeSet::new();
        for r in &rows {
            for c in r.codes() {
                assert!(seen.insert(c.clone()), "科目 {c} 挂了两次");
            }
        }
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

    #[test]
    fn 结转过账的月份支出行仍是本月实际花的钱() {
        let db = mem();
        let p = Period::from_ymm(202601);
        create_accounts(&db).unwrap();
        let rows = list(&db).unwrap();
        let rent = rows.iter().find(|r| r.seq == 2).unwrap().codes()[0].clone();
        // 记一笔厂租 3000
        post(&db, p, 6, vec![(&rent, "3000", "0"), ("1001", "0", "3000")]);
        // 月末结转进生产成本：这是做账的分摊动作，不是又花了一笔钱。
        // 老口径取「借方 − 贷方」，这一笔一结转，厂租行就变 0 了——
        // 演示账 202611 就是这样整张表变空、净利等于全部收入。
        post(&db, p, 28, vec![("500103", "3000", "0"), (&rent, "0", "3000")]);
        let s = report(&db, p).unwrap();
        assert_eq!(line(&s, "厂租").amount, m("3000"), "结转过账不等于没花钱");
        assert_eq!(s.expense, m("3000"));
        assert_eq!(s.profit, m("-3000"));
        // 结转目标（生产成本）不算「表外费用」：那笔钱就是厂租这一行
        assert!(
            s.uncovered.is_zero(),
            "体检不该把结转目标当漏项：{:?}",
            s.uncovered_rows
        );
    }

    #[test]
    fn 红字冲销掉的那笔不算支出() {
        let db = mem();
        let p = Period::from_ymm(202601);
        create_accounts(&db).unwrap();
        let rows = list(&db).unwrap();
        let rent = rows.iter().find(|r| r.seq == 2).unwrap().codes()[0].clone();
        post(&db, p, 6, vec![(&rent, "3000", "0"), ("1001", "0", "3000")]);
        // 冲销：反向再记一笔，两笔都在本期 → 净额 0
        post(&db, p, 7, vec![("1001", "3000", "0"), (&rent, "0", "3000")]);
        let s = report(&db, p).unwrap();
        assert_eq!(line(&s, "厂租").amount, m("0"));
        assert_eq!(s.expense, m("0"));
    }

    #[test]
    fn 没落到行上的费用会被体检列出来() {
        let db = mem();
        let p = Period::from_ymm(202601);
        create_accounts(&db).unwrap();
        // 折旧费、手续费：厂里那张表上没有对应行
        post(&db, p, 8, vec![("510102", "6000", "0"), ("1001", "0", "6000")]);
        post(&db, p, 9, vec![("660303", "100", "0"), ("1001", "0", "100")]);
        let s = report(&db, p).unwrap();
        // 不计进支出合计（合计就是各行之和），但要让人看见「还有这些没在表上」
        assert_eq!(s.expense, m("0"));
        assert_eq!(s.uncovered, m("6100"));
        assert_eq!(s.uncovered_rows.len(), 2);
        assert_eq!(s.uncovered_rows[0].code, "510102", "金额大的排前面");
        assert_eq!(s.uncovered_rows[0].amount, m("6000"));
        assert_eq!(s.uncovered_rows[1].code, "660303");
    }
}
