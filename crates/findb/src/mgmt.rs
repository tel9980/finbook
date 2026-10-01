//! 管理会计：预算、多维损益、自定义报表
//!
//! 与财务会计最大的区别是**没有标准答案**。预算怎么编、部门损益怎么分摊，
//! 每家公司都不一样，所以这里只提供取数与对比的骨架，口径留给用户在界面上定。

use fincore::engine::formula::{self, FormulaSource};
use fincore::account::AuxKind;
use fincore::voucher::AuxRef;
use fincore::{Money, Period};
use rusqlite::OptionalExtension;

use crate::balances::{BalanceQuery, BalanceSnapshot};
use crate::{Db, DbResult};

// ===========================================================================
// 预算
// ===========================================================================

/// 预算行
#[derive(Clone, Debug)]
pub struct Budget {
    pub id: i64,
    pub period: Period,
    pub account_code: String,
    pub dept: String,
    pub amount: Money,
    pub memo: String,
    /// 预算版本（'' = 默认版本，兼容旧数据）
    pub version: String,
}

fn map_budget(r: &rusqlite::Row) -> rusqlite::Result<Budget> {
    Ok(Budget {
        id: r.get(0)?,
        period: Period::from_ymm(r.get(1)?),
        account_code: r.get(2)?,
        dept: r.get(3)?,
        amount: Money::parse_or_zero(&r.get::<_, String>(4)?),
        memo: r.get(5)?,
        version: r.get::<_, String>(6).unwrap_or_default(),
    })
}

const BG_COLS: &str = "id,period,account_code,dept,amount,memo,version";

pub fn budget_list(db: &Db, period: Period) -> DbResult<Vec<Budget>> {
    let mut st = db.conn().prepare(&format!(
        "SELECT {BG_COLS} FROM budget WHERE period=?1 ORDER BY account_code, dept"
    ))?;
    let rows = st
        .query_map(rusqlite::params![period.ymm()], map_budget)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 按版本列预算（period 为 None 时列全部期间）
pub fn budget_list_version(db: &Db, period: Option<Period>, version: &str) -> DbResult<Vec<Budget>> {
    let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match period {
        Some(p) => (
            format!("SELECT {BG_COLS} FROM budget WHERE period=?1 AND version=?2 ORDER BY account_code, dept"),
            vec![Box::new(p.ymm()), Box::new(version.to_string())],
        ),
        None => (
            format!("SELECT {BG_COLS} FROM budget WHERE version=?1 ORDER BY period, account_code"),
            vec![Box::new(version.to_string())],
        ),
    };
    let mut st = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = st
        .query_map(refs.as_slice(), map_budget)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 带版本 upsert（UNIQUE(period,account_code,dept,version)）
pub fn budget_upsert_version(db: &Db, b: &Budget) -> DbResult<i64> {
    budget_upsert_version_on(db.conn(), b)
}

/// 同 `budget_upsert_version`，但只依赖连接，可在调用方的事务内执行
pub fn budget_upsert_version_on(conn: &rusqlite::Connection, b: &Budget) -> DbResult<i64> {
    conn.execute(
        "INSERT INTO budget(period,account_code,dept,amount,memo,version) VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(period,account_code,dept,version) DO UPDATE SET amount=excluded.amount, memo=excluded.memo",
        rusqlite::params![
            b.period.ymm(),
            b.account_code,
            b.dept,
            crate::money_param(b.amount),
            b.memo,
            b.version
        ],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM budget WHERE period=?1 AND account_code=?2 AND dept=?3 AND version=?4",
        rusqlite::params![b.period.ymm(), b.account_code, b.dept, b.version],
        |r| r.get(0),
    )?;
    Ok(id)
}

/// 全年预算（12 个月）
pub fn budget_year(db: &Db, year: i32) -> DbResult<Vec<Budget>> {
    // 年份可能来自外部输入，非法年份返回错误而不是 panic
    let from = Period::new(year, 1)
        .map_err(|e| crate::DbError::Fin(fincore::FinError::msg(format!("非法年份 {year}：{e}"))))?
        .ymm();
    let to = Period::new(year, 12)
        .map_err(|e| crate::DbError::Fin(fincore::FinError::msg(format!("非法年份 {year}：{e}"))))?
        .ymm();
    let mut st = db.conn().prepare(&format!(
        "SELECT {BG_COLS} FROM budget WHERE period BETWEEN ?1 AND ?2 ORDER BY period, account_code"
    ))?;
    let rows = st
        .query_map(rusqlite::params![from, to], map_budget)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn budget_get(db: &Db, id: i64) -> DbResult<Option<Budget>> {
    db.conn()
        .query_row(
            &format!("SELECT {BG_COLS} FROM budget WHERE id=?1"),
            rusqlite::params![id],
            map_budget,
        )
        .optional()
        .map_err(Into::into)
}

pub fn budget_upsert(db: &Db, b: &Budget) -> DbResult<i64> {
    budget_upsert_version(db, b)
}

pub fn budget_delete(db: &Db, id: i64) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM budget WHERE id=?1", rusqlite::params![id])?;
    Ok(())
}

/// 按上期实际（或同期实际）批量生成预算，`ratio` 用于上浮下浮
pub fn budget_from_actual(
    db: &Db,
    from_period: Period,
    to_period: Period,
    ratio: Money,
) -> DbResult<usize> {
    let snap = BalanceSnapshot::load(db, &BalanceQuery::period(from_period))?;
    let accounts = crate::accounts::list(db)?;
    // 整批同事务：中途失败整体回滚，避免生成一半预算
    let tx = db.write_tx()?;
    let mut n = 0;
    for a in accounts.iter().filter(|a| a.category.is_profit_loss()) {
        let row = snap.for_account(&a.code, None);
        let actual = row.debit + row.credit;
        if actual.is_zero() {
            continue;
        }
        budget_upsert_version_on(
            &tx,
            &Budget {
                id: 0,
                period: to_period,
                account_code: a.code.clone(),
                dept: String::new(),
                amount: (actual * ratio).round2(),
                memo: format!("按 {} 实际生成", from_period.label()),
                version: String::new(),
            },
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// 预算执行分析行
#[derive(Clone, Debug, serde::Serialize)]
pub struct BudgetRow {
    pub account_code: String,
    pub account_name: String,
    pub dept: String,
    pub budget: Money,
    pub actual: Money,
    /// 实际 - 预算（费用类正数=超支）
    pub diff: Money,
    /// 执行率（%），无预算时为 0
    pub rate: Money,
    /// 是否超支
    pub over: bool,
}

impl BudgetRow {
    /// 进度百分比，超支为 100+
    pub fn rate_pct(&self) -> String {
        format!("{}%", self.rate.round2())
    }
}

/// 预算 vs 实际
///
/// `expense_like` 决定"超支"的判定方向：费用类科目实际 > 预算算超支，
/// 收入类则相反。用一个闭包交给调用方判断，避免把行业惯例写死。
pub fn budget_vs_actual(
    db: &Db,
    period: Period,
    from: Period,
) -> DbResult<Vec<BudgetRow>> {
    let budgets = budget_list(db, period)?;
    let snap = BalanceSnapshot::load(db, &BalanceQuery::range(from, period))?;
    let chart = crate::accounts::chart(db)?;
    let mut out = Vec::new();
    for b in budgets {
        let aux = if b.dept.is_empty() {
            None
        } else {
            Some(AuxRef {
                dept: Some(b.dept.clone()),
                ..Default::default()
            })
        };
        let row = snap.for_account(&b.account_code, aux.as_ref());
        let actual = row.debit - row.credit;
        let is_expense = chart
            .get(&b.account_code)
            .map(|a| {
                !matches!(
                    a.category,
                    fincore::account::AcctCategory::Income
                ) && (a.code.starts_with('6') || a.code.starts_with('5'))
            })
            .unwrap_or(true);
        let diff = if is_expense {
            actual - b.amount
        } else {
            b.amount - actual
        };
        let rate = if b.amount.is_zero() {
            Money::ZERO
        } else {
            ((actual.abs() * Money::parse("100").unwrap()))
                .checked_div(b.amount.abs().inner())
                .expect("b.amount 已判非零")
                .round2()
        };
        out.push(BudgetRow {
            account_name: chart
                .get(&b.account_code)
                .map(|a| a.name.clone())
                .unwrap_or_default(),
            account_code: b.account_code,
            dept: b.dept,
            budget: b.amount,
            actual,
            diff,
            rate,
            over: diff.is_positive() && !b.amount.is_zero(),
        });
    }
    out.sort_by(|a, b| b.over.cmp(&a.over).then(a.account_code.cmp(&b.account_code)));
    Ok(out)
}

/// 预算超支明细（凭证保存前的硬控制检查结果）
#[derive(Clone, Debug, serde::Serialize)]
pub struct BudgetOver {
    pub account_code: String,
    pub account_name: String,
    pub dept: String,
    pub budget: Money,
    pub actual: Money,
    pub add: Money,
    pub over: Money,
}

/// 预算控制检查（凭证保存前）：对费用/成本类借方分录（本次发生额 `add`）比对**当前激活版本**的预算。
/// 口径：执行额 = 会计年度 1 月至该期间的**已记账**发生额（与预算执行报表一致）+ 本次；
/// 无预算行 / 预算为 0 / 非费用类分录不拦；返回全部超支行（空 = 未超）。
pub fn budget_check(
    db: &Db,
    period: Period,
    adds: &[(String, String, Money)],
) -> DbResult<Vec<BudgetOver>> {
    if adds.is_empty() {
        return Ok(Vec::new());
    }
    let ver = crate::advanced::bversion_current(db)?;
    let budgets: Vec<Budget> = budget_list(db, period)?
        .into_iter()
        .filter(|b| b.version == ver)
        .collect();
    if budgets.is_empty() {
        return Ok(Vec::new());
    }
    let chart = crate::accounts::chart(db)?;
    let from = Period::new(period.year(), 1).unwrap_or(period);
    let snap = BalanceSnapshot::load(db, &BalanceQuery::range(from, period))?;
    let mut out = Vec::new();
    for (code, dept, add) in adds {
        if !add.is_positive() {
            continue;
        }
        let is_expense = chart
            .get(code)
            .map(|a| {
                !matches!(a.category, fincore::account::AcctCategory::Income)
                    && (code.starts_with('6') || code.starts_with('5'))
            })
            .unwrap_or(false);
        if !is_expense {
            continue;
        }
        for b in budgets
            .iter()
            .filter(|b| b.account_code == *code && (b.dept.is_empty() || b.dept == *dept))
        {
            if b.amount.is_zero() {
                continue;
            }
            let aux = if b.dept.is_empty() {
                None
            } else {
                Some(AuxRef {
                    dept: Some(b.dept.clone()),
                    ..Default::default()
                })
            };
            let row = snap.for_account(code, aux.as_ref());
            let actual = row.debit - row.credit;
            let after = actual + *add;
            if after > b.amount {
                out.push(BudgetOver {
                    account_code: code.clone(),
                    account_name: chart.get(code).map(|a| a.name.clone()).unwrap_or_default(),
                    dept: b.dept.clone(),
                    budget: b.amount,
                    actual,
                    add: *add,
                    over: after - b.amount,
                });
            }
        }
    }
    Ok(out)
}

/// 预算预警：执行率超过阈值（默认 100% = 超支）的科目
#[derive(Clone, Debug, serde::Serialize)]
pub struct BudgetAlert {
    pub account_code: String,
    pub account_name: String,
    pub dept: String,
    pub budget: Money,
    pub actual: Money,
    /// 执行率（%）
    pub rate: Money,
    /// 超支金额（费用类 actual-budget）
    pub over_amount: Money,
}

/// 预算预警：执行率 >= threshold（百分比，如 90）的科目，按超支额降序。
pub fn budget_alerts(
    db: &Db,
    period: Period,
    from: Period,
    threshold_pct: i64,
) -> DbResult<Vec<BudgetAlert>> {
    let rows = budget_vs_actual(db, period, from)?;
    let mut out = Vec::new();
    for r in rows {
        // 只有预算的科目才谈得上预警
        if r.budget.is_zero() {
            continue;
        }
        let pct = ((r.actual.abs() * Money::from_i64(100)))
            .checked_div(r.budget.abs().inner())
            .expect("budget 已判非零")
            .round2();
        if pct.to_f64() >= threshold_pct as f64 {
            out.push(BudgetAlert {
                account_code: r.account_code,
                account_name: r.account_name,
                dept: r.dept,
                budget: r.budget,
                actual: r.actual,
                rate: pct,
                over_amount: if r.diff.is_positive() { r.diff } else { Money::ZERO },
            });
        }
    }
    out.sort_by(|a, b| b.over_amount.cmp(&a.over_amount));
    Ok(out)
}

// ===========================================================================
// 预算分析（年度趋势 + 部门维度）
// ===========================================================================

/// 预算分析行：某科目 × 某部门在某一期间的预算与执行
#[derive(Clone, Debug, serde::Serialize)]
pub struct BudgetAnalysisRow {
    pub period: Period,
    pub account_code: String,
    pub account_name: String,
    pub dept: String,
    pub budget: Money,
    pub actual: Money,
    /// 执行率（%）
    pub rate: Money,
}

/// 预算分析：返回指定版本、某年 1 月 ~ 12 月（或截至 `upto`）的逐月预算 vs 实际，
/// 并按 科目 × 部门 展开（多维度预算：dept 维度）。
///
/// `upto` 为空表示整个年度；`rate_only_with_budget` 为 true 时只输出有预算的行。
pub fn budget_analysis(
    db: &Db,
    year: i32,
    version: &str,
    upto: Option<Period>,
) -> DbResult<Vec<BudgetAnalysisRow>> {
    let chart = crate::accounts::chart(db)?;
    // 年份来自外部输入（Web 查询参数），必须校验后再构造期间：
    // `Period::new(...).unwrap()` 会在非法年份时 panic，abort 配置下等于打挂进程。
    let end = match upto {
        Some(p) => p,
        None => Period::new(year, 12).map_err(|e| {
            crate::DbError::Fin(fincore::FinError::msg(format!("非法年份 {year}：{e}")))
        })?,
    };
    let from = Period::new(year, 1).map_err(|e| {
        crate::DbError::Fin(fincore::FinError::msg(format!("非法年份 {year}：{e}")))
    })?;
    let budgets = budget_list_version(db, None, version)?;

    // 预算按 (期间, 科目, 部门) 聚合成映射
    let mut bmap: std::collections::BTreeMap<(i32, String, String), Money> =
        std::collections::BTreeMap::new();
    for b in budgets {
        if b.period.year() != year {
            continue;
        }
        *bmap.entry((b.period.ymm(), b.account_code.clone(), b.dept.clone()))
            .or_insert(Money::ZERO) += b.amount;
    }
    if bmap.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    let mut period = from;
    while period <= end {
        // 逐期快照：BalanceRow 的 debit/credit 是"该期发生额"
        let snap = BalanceSnapshot::load(db, &BalanceQuery::period(period))?;
        for ((p, code, dept), budget) in &bmap {
            if *p != period.ymm() {
                continue;
            }
            let aux = if dept.is_empty() {
                None
            } else {
                Some(AuxRef {
                    dept: Some(dept.clone()),
                    ..Default::default()
                })
            };
            let row = snap.for_account(code, aux.as_ref());
            let actual = row.debit - row.credit;
            let rate = if budget.is_zero() {
                Money::ZERO
            } else {
                ((actual.abs() * Money::from_i64(100)))
                    .checked_div(budget.abs().inner())
                    .expect("budget 已判非零")
                    .round2()
            };
            out.push(BudgetAnalysisRow {
                period,
                account_code: code.clone(),
                account_name: chart.get(code).map(|a| a.name.clone()).unwrap_or_default(),
                dept: dept.clone(),
                budget: *budget,
                actual,
                rate,
            });
        }
        period = period.next();
    }
    out.sort_by(|a, b| {
        a.period
            .ymm()
            .cmp(&b.period.ymm())
            .then(a.account_code.cmp(&b.account_code))
            .then(a.dept.cmp(&b.dept))
    });
    Ok(out)
}

/// 预算分析汇总：某科目 × 部门全年预算/实际合计
#[derive(Clone, Debug, serde::Serialize)]
pub struct BudgetAnalysisSummary {
    pub account_code: String,
    pub account_name: String,
    pub dept: String,
    pub budget: Money,
    pub actual: Money,
    /// 执行率（%）
    pub rate: Money,
}

/// 预算分析的年度汇总（用于部门维度对比与排行）
pub fn budget_analysis_summary(
    db: &Db,
    year: i32,
    version: &str,
) -> DbResult<Vec<BudgetAnalysisSummary>> {
    let rows = budget_analysis(db, year, version, None)?;
    let mut map: std::collections::BTreeMap<(String, String), (Money, Money)> =
        std::collections::BTreeMap::new();
    for r in rows {
        let e = map
            .entry((r.account_code.clone(), r.dept.clone()))
            .or_insert((Money::ZERO, Money::ZERO));
        e.0 += r.budget;
        e.1 += r.actual;
    }
    let chart = crate::accounts::chart(db)?;
    let mut out = Vec::new();
    for ((code, dept), (budget, actual)) in map {
        let rate = if budget.is_zero() {
            Money::ZERO
        } else {
            ((actual.abs() * Money::from_i64(100)))
                .checked_div(budget.abs().inner())
                .expect("budget 已判非零")
                .round2()
        };
        out.push(BudgetAnalysisSummary {
            account_code: code.clone(),
            account_name: chart.get(&code).map(|a| a.name.clone()).unwrap_or_default(),
            dept,
            budget,
            actual,
            rate,
        });
    }
    out.sort_by(|a, b| a.account_code.cmp(&b.account_code).then(a.dept.cmp(&b.dept)));
    Ok(out)
}

// ===========================================================================
// 多维损益
// ===========================================================================

/// 某个维度（部门 / 项目 / 客户）的损益
#[derive(Clone, Debug)]
pub struct DimProfit {
    /// 维度值编码
    pub key: String,
    pub name: String,
    /// 收入类科目（6xxx 中的收入，即 6001/6051 等）
    pub revenue: Money,
    /// 成本
    pub cost: Money,
    /// 费用
    pub expense: Money,
    /// 税金及附加 + 所得税等
    pub tax: Money,
    pub profit: Money,
}

/// 按辅助核算维度出损益表
///
/// 分摊规则很公司特化，这里只做**直接归属**：分录上带了某部门/项目就算它的，
/// 共同费用不摊——宁可显示"未分配"，也不要编一个假的分摊率出来。
pub fn dim_profit(
    db: &Db,
    period: Period,
    dim: AuxKind,
) -> DbResult<Vec<DimProfit>> {
    let mut q = BalanceQuery::period(period).with_leaf_only(true);
    q.aux = None;
    let snap = BalanceSnapshot::load(db, &q)?;
    let chart = crate::accounts::chart(db)?;
    let names = crate::auxs::full_name_map(db)?;

    let mut map: std::collections::BTreeMap<String, DimProfit> =
        std::collections::BTreeMap::new();
    for a in crate::accounts::list(db)?.iter().filter(|a| a.category.is_profit_loss()) {
        for row in snap.aux_breakdown(&a.code) {
            let Some(v) = row.aux.get(dim) else {
                continue;
            };
            let signed = row.debit - row.credit;
            if signed.is_zero() {
                continue;
            }
            let e = map
                .entry(v.clone())
                .or_insert_with(|| DimProfit {
                    key: v.clone(),
                    name: names
                        .get(&format!("{}:{}", dim.code(), v))
                        .cloned()
                        .unwrap_or_else(|| v.clone()),
                    revenue: Money::ZERO,
                    cost: Money::ZERO,
                    expense: Money::ZERO,
                    tax: Money::ZERO,
                    profit: Money::ZERO,
                });
            // 损益科目：收入类贷方为正，成本费用类借方为正
            let amount = if a.category == fincore::account::AcctCategory::Income {
                -signed
            } else {
                signed
            };
            match a.code.as_str() {
                c if c.starts_with("6001") || c.starts_with("6051") || c.starts_with("6301") => {
                    e.revenue += amount
                }
                c if c.starts_with("6401") || c.starts_with("6402") => e.cost += amount,
                c if c.starts_with("6403") || c.starts_with("6801") => e.tax += amount,
                _ => e.expense += amount,
            }
        }
    }
    let mut out: Vec<DimProfit> = map.into_values().collect();
    for d in out.iter_mut() {
        d.profit = d.revenue - d.cost - d.expense - d.tax;
    }
    out.sort_by_key(|l| std::cmp::Reverse(l.profit));
    let _ = chart;
    Ok(out)
}

// ===========================================================================
// 自定义报表
// ===========================================================================

/// 报表取数上下文：把 BalanceSnapshot 接到公式引擎上
pub struct ReportCtx<'a> {
    db: &'a Db,
    /// 数据范围用户（None = 不限制）：快照取数按科目区间 + 本人凭证过滤
    user: Option<&'a fincore::user::User>,
    cache: std::cell::RefCell<std::collections::HashMap<i32, BalanceSnapshot>>,
}

impl<'a> ReportCtx<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self {
            db,
            user: None,
            cache: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }
    /// 套用用户数据范围（科目区间 / 仅本人凭证）
    pub fn with_user(mut self, u: &'a fincore::user::User) -> Self {
        self.user = Some(u);
        self
    }
    fn snap(&self, period: Period) -> DbResult<BalanceSnapshot> {
        let mut c = self.cache.borrow_mut();
        if let Some(s) = c.get(&period.ymm()) {
            return Ok(s.clone());
        }
        let mut bq = BalanceQuery::period(period);
        if let Some(u) = self.user {
            bq = bq.with_user_scope(u);
        }
        let s = BalanceSnapshot::load(self.db, &bq)?;
        c.insert(period.ymm(), s.clone());
        Ok(s)
    }
}

impl<'a> FormulaSource for ReportCtx<'a> {
    fn qc(&self, code: &str, period: Period, _dir: Option<&str>) -> Money {
        self.snap(period)
            .map(|s| s.for_account(code, None).begin)
            .unwrap_or(Money::ZERO)
    }
    fn qm(&self, code: &str, period: Period, _dir: Option<&str>) -> Money {
        self.snap(period)
            .map(|s| s.for_account(code, None).end())
            .unwrap_or(Money::ZERO)
    }
    fn fs(&self, code: &str, period: Period, dir: Option<&str>) -> Money {
        match self.snap(period) {
            Ok(s) => {
                let r = s.for_account(code, None);
                match dir {
                    Some(d) if d.starts_with('借') || d.eq_ignore_ascii_case("J") => r.debit,
                    Some(d) if d.starts_with('贷') || d.eq_ignore_ascii_case("D") => r.credit,
                    _ => r.debit - r.credit,
                }
            }
            Err(_) => Money::ZERO,
        }
    }
    fn lfs(&self, code: &str, period: Period, dir: Option<&str>) -> Money {
        match self.snap(period) {
            Ok(s) => {
                let r = s.for_account(code, None);
                match dir {
                    Some(d) if d.starts_with('借') || d.eq_ignore_ascii_case("J") => r.ytd_debit,
                    Some(d) if d.starts_with('贷') || d.eq_ignore_ascii_case("D") => r.ytd_credit,
                    _ => r.ytd_debit - r.ytd_credit,
                }
            }
            Err(_) => Money::ZERO,
        }
    }
}

/// 用户自定义报表＝一张"行 × 列"的网格，每个单元格一条公式
///
/// 沿用 `ReportDef` 的结构（列标题 + 行定义），但行的取数方式换成公式，
/// 这样既能复用现有的报表渲染，又给了用户 UFO 式的表达力。
#[derive(Clone, Debug)]
pub struct CustomReport {
    pub key: String,
    pub name: String,
    /// 列标题（不含第一列"项目"）
    pub columns: Vec<String>,
    /// 每行：名称 + 各列公式
    pub lines: Vec<CustomLine>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CustomLine {
    pub name: String,
    pub indent: u8,
    /// 与 `columns` 一一对应
    pub formulas: Vec<String>,
    pub bold: bool,
}

impl CustomReport {
    pub fn new(key: &str, name: &str, columns: Vec<String>) -> Self {
        Self {
            key: key.to_string(),
            name: name.to_string(),
            columns,
            lines: Vec::new(),
        }
    }
}

/// 求值整个自定义报表，返回 `行 × 列` 的金额矩阵（`user` = 数据范围，None 不限制）
pub fn custom_report_values(
    db: &Db,
    r: &CustomReport,
    period: Period,
    user: Option<&fincore::user::User>,
) -> DbResult<Vec<Vec<Money>>> {
    let mut ctx = ReportCtx::new(db);
    if let Some(u) = user {
        ctx = ctx.with_user(u);
    }
    let mut out = Vec::with_capacity(r.lines.len());
    for l in &r.lines {
        let mut row = Vec::with_capacity(r.columns.len());
        for f in &l.formulas {
            row.push(formula::eval(f, &ctx, period).unwrap_or(Money::ZERO));
        }
        out.push(row);
    }
    Ok(out)
}

/// 列出全部自定义报表
pub fn custom_list(db: &Db) -> DbResult<Vec<CustomReport>> {
    let mut st = db.conn().prepare(
        "SELECT key,name,columns_json,lines_json FROM custom_report ORDER BY key",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok(CustomReport {
                key: r.get(0)?,
                name: r.get(1)?,
                columns: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                lines: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn custom_get(db: &Db, key: &str) -> DbResult<Option<CustomReport>> {
    db.conn()
        .query_row(
            "SELECT key,name,columns_json,lines_json FROM custom_report WHERE key=?1",
            rusqlite::params![key],
            |r| {
                Ok(CustomReport {
                    key: r.get(0)?,
                    name: r.get(1)?,
                    columns: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                    lines: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub fn custom_save(db: &Db, r: &CustomReport) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO custom_report(key,name,columns_json,lines_json,updated_at)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(key) DO UPDATE SET name=excluded.name,
            columns_json=excluded.columns_json, lines_json=excluded.lines_json,
            updated_at=excluded.updated_at",
        rusqlite::params![
            r.key,
            r.name,
            serde_json::to_string(&r.columns)?,
            serde_json::to_string(&r.lines)?,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
        ],
    )?;
    Ok(())
}

pub fn custom_delete(db: &Db, key: &str) -> DbResult<()> {
    db.conn()
        .execute("DELETE FROM custom_report WHERE key=?1", rusqlite::params![key])?;
    Ok(())
}

/// 新建自定义报表时自动分配 key
pub fn custom_next_key(db: &Db) -> DbResult<String> {
    let n: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM custom_report", [], |r| r.get(0))
        .unwrap_or(0);
    Ok(format!("R{:03}", n + 1))
}

/// 校验整张报表的公式语法，返回（行, 列, 错误）
pub fn custom_check(r: &CustomReport) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    for (li, l) in r.lines.iter().enumerate() {
        for (ci, f) in l.formulas.iter().enumerate() {
            if f.trim().is_empty() {
                continue;
            }
            if let Err(e) = formula::check(f) {
                out.push((li, ci, e.to_string()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fincore::account::AuxKind;
    use fincore::voucher::{Entry, Voucher};

    fn tmpdb(name: &str) -> Db {
        let p = std::env::temp_dir().join(format!("finbook_mgmt_{name}.fbk"));
        let _ = std::fs::remove_file(&p);
        Db::create(&p, &fincore::BookOptions::default()).unwrap()
    }
    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    fn post(db: &Db, period: Period, no: i32, lines: &[(&str, &str, &str, Option<&str>)]) -> i64 {
        let date = period.first_day();
        let mut v = Voucher::new(period, date, "记", no);
        for (i, (acc, d, c, dept)) in lines.iter().enumerate() {
            let mut aux = AuxRef::default();
            if let Some(x) = dept {
                aux.dept = Some(x.to_string());
            }
            if acc.starts_with("1002") {
                aux.bank = Some("BANK01".to_string());
            }
            v.push_entry(Entry {
                debit: m(d),
                credit: m(c),
                aux,
                ..Entry::new(i as i32 + 1, *acc, "测试")
            });
        }
        let id = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, id, "p").unwrap();
        id
    }

    #[test]
    fn budget_crud_and_compare() {
        let db = tmpdb("budget");
        let p = Period::new(2026, 1).unwrap();
        budget_upsert(
            &db,
            &Budget {
                id: 0,
                period: p,
                account_code: "660201".into(),
                dept: String::new(),
                amount: m("10000"),
                memo: String::new(),
                version: String::new(),
            },
        )
        .unwrap();
        // 重复 upsert 覆盖而不是新增
        budget_upsert(
            &db,
            &Budget {
                id: 0,
                period: p,
                account_code: "660201".into(),
                dept: String::new(),
                amount: m("12000"),
                memo: "调整".into(),
                version: String::new(),
            },
        )
        .unwrap();
        assert_eq!(budget_list(&db, p).unwrap().len(), 1);
        assert_eq!(budget_list(&db, p).unwrap()[0].amount, m("12000"));

        // 实际发生 15000 → 超支 3000
        post(&db, p, 1, &[("660201", "15000", "0", None), ("100201", "0", "15000", None)]);
        let rows = budget_vs_actual(&db, p, p).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].actual, m("15000"));
        assert_eq!(rows[0].diff, m("3000"));
        assert!(rows[0].over);
        assert_eq!(rows[0].rate, m("125"));

        budget_delete(&db, rows[0].account_code.parse().unwrap_or(1)).ok();
    }

    #[test]
    fn budget_from_actual_copies() {
        let db = tmpdb("bfa");
        let p1 = Period::new(2026, 1).unwrap();
        post(&db, p1, 1, &[("660201", "8000", "0", None), ("100201", "0", "8000", None)]);
        let n = budget_from_actual(&db, p1, Period::new(2026, 2).unwrap(), m("1.1")).unwrap();
        assert!(n > 0);
        let b = budget_list(&db, Period::new(2026, 2).unwrap())
            .unwrap()
            .into_iter()
            .find(|x| x.account_code == "660201")
            .unwrap();
        assert_eq!(b.amount, m("8800"));
    }

    #[test]
    fn dim_profit_by_dept() {
        let db = tmpdb("dim");
        let p = Period::new(2026, 1).unwrap();
        crate::auxs::insert(
            &db,
            &fincore::auxiliary::AuxEntity::new(AuxKind::Dept, "D01", "销售部"),
        )
        .unwrap();
        crate::auxs::insert(
            &db,
            &fincore::auxiliary::AuxEntity::new(AuxKind::Dept, "D02", "研发部"),
        )
        .unwrap();
        // 销售部：收入 100000，费用 20000
        post(
            &db,
            p,
            1,
            &[("100201", "100000", "0", Some("D01")), ("600101", "0", "100000", Some("D01"))],
        );
        post(
            &db,
            p,
            2,
            &[("660201", "20000", "0", Some("D01")), ("100201", "0", "20000", Some("D01"))],
        );
        // 研发部：只有费用 50000
        post(
            &db,
            p,
            3,
            &[("660201", "50000", "0", Some("D02")), ("100201", "0", "50000", Some("D02"))],
        );

        let rows = dim_profit(&db, p, AuxKind::Dept).unwrap();
        assert_eq!(rows.len(), 2);
        let d01 = rows.iter().find(|r| r.key == "D01").unwrap();
        assert_eq!(d01.name, "销售部");
        assert_eq!(d01.revenue, m("100000"));
        assert_eq!(d01.expense, m("20000"));
        assert_eq!(d01.profit, m("80000"));
        let d02 = rows.iter().find(|r| r.key == "D02").unwrap();
        assert_eq!(d02.profit, m("-50000"));
    }

    #[test]
    fn custom_report_eval() {
        let db = tmpdb("custom");
        let p = Period::new(2026, 1).unwrap();
        post(&db, p, 1, &[("100201", "50000", "0", None), ("600101", "0", "50000", None)]);
        let mut r = CustomReport::new("custom.r1", "资金情况", vec!["本月".into(), "占比".into()]);
        r.lines.push(CustomLine {
            name: "银行存款".into(),
            indent: 0,
            formulas: vec!["QM(\"1002\")".into(), "QM(\"1002\")/QM(\"1001\")*100".into()],
            bold: false,
        });
        r.lines.push(CustomLine {
            name: "营业收入".into(),
            indent: 0,
            formulas: vec!["FS(\"6001\",,\"贷\")".into(), "100".into()],
            bold: false,
        });
        custom_save(&db, &r).unwrap();
        let vals = custom_report_values(&db, &r, p, None).unwrap();
        assert_eq!(vals[0][0], m("50000"));
        assert_eq!(vals[1][0], m("50000"));
        let list = custom_list(&db).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].lines.len(), 2);
    }

    #[test]
    fn budget_analysis_yearly_and_by_dept() {
        let db = tmpdb("anly");
        // 2026 年 1~2 月，销售部 660201 预算各 10000
        for mo in [1u32, 2] {
            budget_upsert_version(
                &db,
                &Budget {
                    id: 0,
                    period: Period::new(2026, mo).unwrap(),
                    account_code: "660201".into(),
                    dept: "D01".into(),
                    amount: m("10000"),
                    memo: String::new(),
                    version: "v1".into(),
                },
            )
            .unwrap();
        }
        // 1 月实际 12000（超支），2 月实际 8000
        post(&db, Period::new(2026, 1).unwrap(), 1, &[("660201", "12000", "0", Some("D01")), ("100201", "0", "12000", Some("D01"))]);
        post(&db, Period::new(2026, 2).unwrap(), 2, &[("660201", "8000", "0", Some("D01")), ("100201", "0", "8000", Some("D01"))]);

        let rows = budget_analysis(&db, 2026, "v1", None).unwrap();
        assert_eq!(rows.len(), 2);
        let jan = rows.iter().find(|r| r.period.month() == 1).unwrap();
        assert_eq!(jan.budget, m("10000"));
        assert_eq!(jan.actual, m("12000"));
        assert_eq!(jan.rate, m("120"));
        let feb = rows.iter().find(|r| r.period.month() == 2).unwrap();
        assert_eq!(feb.actual, m("8000"));
        assert_eq!(feb.rate, m("80"));

        // 年度汇总：预算 20000，实际 20000，执行率 100
        let sums = budget_analysis_summary(&db, 2026, "v1").unwrap();
        assert_eq!(sums.len(), 1);
        assert_eq!(sums[0].dept, "D01");
        assert_eq!(sums[0].budget, m("20000"));
        assert_eq!(sums[0].actual, m("20000"));
        assert_eq!(sums[0].rate, m("100"));
    }
}
