//! 报表模板与现金流量取数

use std::collections::HashMap;

use fincore::report::cashflow::{
    build_cash_flow, default_cash_flow_items, CashFlowDirection, CashFlowGroup, CashFlowItem,
    CashFlowStatement, ItemAmounts,
};
use fincore::report::equity::{
    EquityStatement, CAPITAL, CURRENT_PROFIT, RESERVE, SURPLUS, UNDISTRIBUTED,
};
use fincore::report::{balance_sheet, income, AmountKind, ReportDef};
use fincore::Money;

use rusqlite::OptionalExtension;

use crate::balances::{BalanceQuery, BalanceSnapshot};
use crate::{accounts, read_money, Db, DbResult};

/// 写入内置报表模板（已存在则不覆盖，保留用户自定义）
pub fn ensure_defaults(db: &Db) -> DbResult<()> {
    for def in [
        balance_sheet::balance_sheet_def(),
        income::income_statement_def(),
    ] {
        let exists: i64 = db.conn().query_row(
            "SELECT COUNT(*) FROM report_def WHERE key=?1",
            rusqlite::params![def.key],
            |r| r.get(0),
        )?;
        if exists == 0 {
            save_def(db, &def)?;
        }
    }
    Ok(())
}

/// 恢复内置现金流量项目（覆盖同名编码）
pub fn reset_cash_flow_items(db: &Db) -> DbResult<usize> {
    let items = default_cash_flow_items();
    let mut n = 0usize;
    for it in &items {
        let g = match it.group {
            CashFlowGroup::Operating => "operating",
            CashFlowGroup::Investing => "investing",
            CashFlowGroup::Financing => "financing",
        };
        let d = match it.dir {
            CashFlowDirection::In => "in",
            CashFlowDirection::Out => "out",
        };
        db.conn().execute(
            "INSERT OR REPLACE INTO cash_flow_item(code,name,grp,dir,disabled)
             VALUES(?1,?2,?3,?4,0)",
            rusqlite::params![it.code, it.name, g, d],
        )?;
        n += 1;
    }
    Ok(n)
}

pub fn list_defs(db: &Db) -> DbResult<Vec<ReportDef>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT key,name,columns_json,lines_json FROM report_def ORDER BY key")?;
    let rows = stmt
        .query_map([], |r| {
            let key: String = r.get(0)?;
            let name: String = r.get(1)?;
            let cols: String = r.get(2)?;
            let lines: String = r.get(3)?;
            Ok(ReportDef {
                key,
                name,
                columns: serde_json::from_str(&cols).unwrap_or_default(),
                lines: serde_json::from_str(&lines).unwrap_or_default(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_def(db: &Db, key: &str) -> DbResult<Option<ReportDef>> {
    db.conn()
        .query_row(
            "SELECT key,name,columns_json,lines_json FROM report_def WHERE key=?1",
            rusqlite::params![key],
            |r| {
                let cols: String = r.get(2)?;
                let lines: String = r.get(3)?;
                Ok(ReportDef {
                    key: r.get(0)?,
                    name: r.get(1)?,
                    columns: serde_json::from_str(&cols).unwrap_or_default(),
                    lines: serde_json::from_str(&lines).unwrap_or_default(),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub fn save_def(db: &Db, def: &ReportDef) -> DbResult<()> {
    db.conn().execute(
        "INSERT INTO report_def(key,name,columns_json,lines_json) VALUES(?1,?2,?3,?4)
         ON CONFLICT(key) DO UPDATE SET name=excluded.name,
             columns_json=excluded.columns_json, lines_json=excluded.lines_json",
        rusqlite::params![
            def.key,
            def.name,
            serde_json::to_string(&def.columns)?,
            serde_json::to_string(&def.lines)?
        ],
    )?;
    Ok(())
}

/// 恢复内置模板
pub fn reset_def(db: &Db, key: &str) -> DbResult<()> {
    let def = match key {
        "balance_sheet" => balance_sheet::balance_sheet_def(),
        "income_statement" => income::income_statement_def(),
        _ => return Err(fincore::FinError::msg(format!("未知报表 {key}")).into()),
    };
    save_def(db, &def)
}

// ---------------------------------------------------------------------------
// 现金流量
// ---------------------------------------------------------------------------

pub fn cash_flow_items(db: &Db) -> DbResult<Vec<CashFlowItem>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT code,name,grp,dir,disabled FROM cash_flow_item ORDER BY code")?;
    let rows = stmt
        .query_map([], |r| {
            let g: String = r.get(2)?;
            let d: String = r.get(3)?;
            Ok(CashFlowItem {
                code: r.get(0)?,
                name: r.get(1)?,
                group: match g.as_str() {
                    "investing" => CashFlowGroup::Investing,
                    "financing" => CashFlowGroup::Financing,
                    _ => CashFlowGroup::Operating,
                },
                dir: match d.as_str() {
                    "out" => CashFlowDirection::Out,
                    _ => CashFlowDirection::In,
                },
                disabled: r.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 现金及现金等价物科目（末级）
pub fn cash_accounts(db: &Db) -> DbResult<Vec<String>> {
    let chart = accounts::chart(db)?;
    Ok(chart
        .all()
        .into_iter()
        .filter(|a| (a.is_cash || a.is_bank) && chart.is_leaf(&a.code))
        .map(|a| a.code.clone())
        .collect())
}

/// 现金流量归集结果
///
/// 除了「项目 → 发生额」，还带四项审计信息：界面要能诚实地说清
/// 「这张表有多少是猜出来的、多少猜不了」——不然用户以为报表是完整的。
#[derive(Debug, Default)]
pub struct CashFlowAmounts {
    pub amounts: ItemAmounts,
    /// 无法归集的现金收支净额
    pub unassigned: Money,
    /// 按对方科目自动归集的行数
    pub auto_assigned: usize,
    /// 对方科目指向不同项目、拒绝猜测的行数
    pub ambiguous: usize,
    /// 纯货币资金内部划转（提现 / 存现）而跳过的行数
    pub transfers: usize,
}

/// 按现金流量项目汇总发生额（只统计现金/银行科目的已记账分录）
///
/// 归集顺序：**凭证上标的项目 → 按对方科目推断**。推断只在一件事上猜：
/// 这张凭证的对方科目全都指向同一个现金流量项目。指向不同项目时宁可留在
/// 「未归集」也不猜——猜错的现金流量表比空表更糟。
pub fn cash_flow_amounts_detail(
    db: &Db,
    from: fincore::Period,
    to: fincore::Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<CashFlowAmounts> {
    let codes = cash_accounts(db)?;
    if codes.is_empty() {
        return Ok(CashFlowAmounts::default());
    }
    let cash_set: std::collections::HashSet<&str> = codes.iter().map(|c| c.as_str()).collect();
    let placeholders = codes.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    // 取「本期所有含现金科目的凭证」的**全部分录**，按凭证分组后再判断对方科目。
    // 不能只查现金行：对方科目在同一个凭证的另一行上。
    let mut sql = format!(
        "SELECT e.voucher_id, e.account_code, e.cf_item, e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON e.voucher_id = v.id
         WHERE v.status = 'posted' AND e.period BETWEEN ?1 AND ?2
           AND e.voucher_id IN (
               SELECT e2.voucher_id FROM voucher_entry e2
               JOIN voucher v2 ON e2.voucher_id = v2.id
               WHERE v2.status = 'posted' AND e2.period BETWEEN ?1 AND ?2
                 AND e2.account_code IN ({placeholders})
           )"
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(from.ymm()), Box::new(to.ymm())];
    for c in &codes {
        params.push(Box::new(c.clone()));
    }
    if let Some(u) = scope {
        crate::push_report_scope(&mut sql, &mut params, &u.data_scope, &u.username);
    }
    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(refs.as_slice())?;

    // voucher_id -> [(科目, 标注项目, 借方, 贷方)]
    let mut by_voucher: HashMap<i64, Vec<(String, Option<String>, Money, Money)>> = HashMap::new();
    while let Some(r) = rows.next()? {
        let vid: i64 = r.get(0)?;
        let code: String = r.get(1)?;
        let cf: Option<String> = r.get(2)?;
        let d = read_money(r, 3)?;
        let c = read_money(r, 4)?;
        by_voucher.entry(vid).or_default().push((code, cf, d, c));
    }

    let mut out = CashFlowAmounts::default();
    // 固定遍历顺序，保证 auto_assigned / ambiguous 的计数可复现
    let mut vids: Vec<i64> = by_voucher.keys().copied().collect();
    vids.sort_unstable();
    for vid in vids {
        let lines = &by_voucher[&vid];
        // 这张凭证的全部对方科目（非货币资金科目）
        let counters: Vec<&str> = lines
            .iter()
            .map(|(c, ..)| c.as_str())
            .filter(|c| !cash_set.contains(c))
            .collect();

        for (code, cf, d, c) in lines {
            if !cash_set.contains(code.as_str()) {
                continue;
            }
            if d.round2().is_zero() && c.round2().is_zero() {
                continue;
            }
            let dir = if *d >= *c {
                CashFlowDirection::In
            } else {
                CashFlowDirection::Out
            };
            let net = *d - *c;

            // ① 凭证上标了，就听凭证的
            let assigned = match cf {
                Some(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
                _ => {
                    // ② 对方科目全是货币资金 → 提现/存现，不是现金流量
                    if counters.is_empty() {
                        out.transfers += 1;
                        continue;
                    }
                    // ③ 按对方科目推断。税额科目是主业务的伴生行：
                    //    「借 银行 113,000 / 贷 收入 100,000 / 贷 销项 13,000」
                    //    该归「销售商品收到的现金」，不能因为多了个 2221 就说分不清。
                    let votes: std::collections::BTreeSet<&'static str> = counters
                        .iter()
                        .filter(|c| !(counters.len() > 1 && c.starts_with("2221")))
                        .filter_map(|c| fincore::report::cashflow_rules::item_for(c, dir))
                        .collect();
                    match votes.len() {
                        1 => {
                            out.auto_assigned += 1;
                            Some(votes.into_iter().next().unwrap_or_default().to_string())
                        }
                        0 => None,
                        _ => {
                            // 指向多个项目：不猜
                            out.ambiguous += 1;
                            None
                        }
                    }
                }
            };

            match assigned {
                Some(code) => {
                    let e = out
                        .amounts
                        .entry(code)
                        .or_insert((Money::ZERO, Money::ZERO));
                    e.0 += *d;
                    e.1 += *c;
                }
                None => out.unassigned += net,
            }
        }
    }
    Ok(out)
}

/// 只要「项目 → 发生额」的旧接口（未归集额用内部键 `__unassigned__` 带出）
pub fn cash_flow_amounts(
    db: &Db,
    from: fincore::Period,
    to: fincore::Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<ItemAmounts> {
    let d = cash_flow_amounts_detail(db, from, to, scope)?;
    let mut amt = d.amounts;
    if !d.unassigned.is_zero() {
        amt.insert("__unassigned__".to_string(), (d.unassigned, Money::ZERO));
    }
    Ok(amt)
}

/// 现金及现金等价物的期初 / 期末余额
pub fn cash_begin_end(
    db: &Db,
    from: fincore::Period,
    to: fincore::Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<(Money, Money)> {
    let codes = cash_accounts(db)?;
    let mut bq = BalanceQuery::range(from, to);
    if let Some(u) = scope {
        bq = bq.with_user_scope(u);
    }
    let snap = BalanceSnapshot::load(db, &bq)?;
    let mut begin = Money::ZERO;
    let mut end = Money::ZERO;
    for c in codes {
        let r = snap.for_account(&c, None);
        begin += r.begin;
        end += r.end();
    }
    Ok((begin, end))
}

/// 生成现金流量表
/// 现金流量表：本期 + 上期（同样长度的上一段期间）
///
/// 「这个月经营现金流 5 万」这句话本身没有信息量——是比上月多了还是少了才有。
/// 上期长度取与本期**相同的月数**（本期 3 个月就取上一个 3 个月），
/// 这样两列才可比；固定取「上月」会让季度报里出现「3 个月 vs 1 个月」。
/// 与 `from..to` 等长的上一段期间
///
/// 「等长」是关键：本期是 3 个月，上期就得取 3 个月，否则两列不可比
/// （季度报里出现「3 个月 vs 1 个月」，同比变成月比季）。
pub fn prior_range(
    from: fincore::Period,
    to: fincore::Period,
) -> (fincore::Period, fincore::Period) {
    let months = ((to.ymm() / 100 - from.ymm() / 100) * 12 + (to.ymm() % 100) as i32
        - (from.ymm() % 100) as i32
        + 1)
    .max(1) as usize;
    // 末尾是本期起始月的前一个月，再往前推 months-1 个月就是上期的起点。
    // 曾经的写法把 `prev_to` 也往前推了 months-1，于是 3 个月的对比实际取的是
    // 「再往前两个月的那 3 个月」——差两个月，看着像数字都对，其实是另一段时间。
    let prev_to = from.prev();
    let mut prev_from = prev_to;
    for _ in 1..months {
        prev_from = prev_from.prev();
    }
    (prev_from, prev_to)
}

pub fn cash_flow_compare(
    db: &Db,
    from: fincore::Period,
    to: fincore::Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<(CashFlowStatement, CashFlowStatement)> {
    let cur = cash_flow_statement(db, from, to, scope)?;
    let (prev_from, prev_to) = prior_range(from, to);
    let prev = cash_flow_statement(db, prev_from, prev_to, scope)?;
    Ok((cur, prev))
}

pub fn cash_flow_statement(
    db: &Db,
    from: fincore::Period,
    to: fincore::Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<CashFlowStatement> {
    let detail = cash_flow_amounts_detail(db, from, to, scope)?;
    let amounts = detail.amounts;
    let (begin, end) = cash_begin_end(db, from, to, scope)?;
    let items = cash_flow_items(db)?;
    let items = if items.is_empty() {
        default_cash_flow_items()
    } else {
        items
    };
    let mut stmt = build_cash_flow(&items, &amounts, begin, end, detail.unassigned);
    stmt.auto_assigned = detail.auto_assigned;
    stmt.ambiguous = detail.ambiguous;
    stmt.transfers = detail.transfers;
    Ok(stmt)
}

// ---------------------------------------------------------------------------
// 所有者权益变动表
// ---------------------------------------------------------------------------

/// 生成所有者权益变动表。
///
/// 取数口径：权益类科目贷方余额为正。年初 = 本年 1 月的期初余额，
/// 本年增减 = 本年累计贷 − 本年累计借（贷方增加为正），年末 = 年初 + 本年增减。
/// `from` 应为本会计年度首个期间（通常 1 月），`to` 为报告期末。
pub fn equity_statement(
    db: &Db,
    from: fincore::Period,
    to: fincore::Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<EquityStatement> {
    let mut bq = BalanceQuery::range(from, to);
    if let Some(u) = scope {
        bq = bq.with_user_scope(u);
    }
    let snap = BalanceSnapshot::load(db, &bq)?;

    // 与资产负债表“未分配利润”同一口径：4103 本年利润 + 4104 利润分配，
    // 再加上尚未结转的损益净额。只取 410401 会漏掉 4103 和 4104 的其它明细，
    // 年中未结转时权益合计也会比资产负债表少一块。
    let names: [(&str, &[&str]); 4] = [
        ("实收资本", &[CAPITAL]),
        ("资本公积", &[RESERVE]),
        ("盈余公积", &[SURPLUS]),
        ("未分配利润", &[CURRENT_PROFIT, UNDISTRIBUTED]),
    ];
    let mut inputs = Vec::with_capacity(4);
    for (name, codes) in names {
        let (mut begin, mut change, mut end) = (Money::ZERO, Money::ZERO, Money::ZERO);
        for code in codes {
            let r = snap.for_account(code, None);
            // 权益类贷方正：年初/年末 = -余额（带符号余额为借正贷负）
            begin += r.begin.negated();
            change += r.credit - r.debit;
            end += r.end().negated();
        }
        if name == "未分配利润" {
            let pl_begin = snap.profit_loss_net(AmountKind::Begin);
            let pl_end = snap.profit_loss_net(AmountKind::End);
            // 资产负债表把未结转损益并入未分配利润：正=净亏损，所以取负。
            begin += pl_begin.negated();
            end += pl_end.negated();
            change += pl_begin - pl_end;
        }
        inputs.push((name.to_string(), begin, change, end));
    }
    Ok(EquityStatement::build(&inputs))
}

// ---------------------------------------------------------------------------
// 报表对比分析
// ---------------------------------------------------------------------------

/// 报表对比行：同一报表项目在两个期间的取值 + 差额 + 变动率
#[derive(Clone, Debug, serde::Serialize)]
pub struct CompareRow {
    pub no: String,
    pub name: String,
    pub indent: u8,
    pub style: fincore::report::LineStyle,
    /// 当前期值
    pub current: Money,
    /// 对比期值
    pub previous: Money,
    /// 差额 = 当前 − 对比
    pub diff: Money,
    /// 变动率（%），对比期为 0 时记为 0
    pub rate: Money,
}

/// 对同一张内置/自定义报表做两期对比。
/// `key` 为 report_def 的 key，`current_from/to` 与 `prev_from/to` 分别为两期的取数区间。
/// 每个金额列（取第一列）对比；其余列忽略。
pub fn report_compare(
    db: &Db,
    key: &str,
    current_from: fincore::Period,
    current_to: fincore::Period,
    prev_from: fincore::Period,
    prev_to: fincore::Period,
    scope: Option<&fincore::user::User>,
) -> DbResult<Vec<CompareRow>> {
    let def = get_def(db, key)?.unwrap_or_else(|| match key {
        "balance_sheet" => balance_sheet::balance_sheet_def(),
        "income_statement" => income::income_statement_def(),
        _ => fincore::report::ReportDef {
            key: key.to_string(),
            name: key.to_string(),
            columns: vec![],
            lines: vec![],
        },
    });
    let mut cur_bq = BalanceQuery::range(current_from, current_to);
    let mut prev_bq = BalanceQuery::range(prev_from, prev_to);
    if let Some(u) = scope {
        cur_bq = cur_bq.with_user_scope(u);
        prev_bq = prev_bq.with_user_scope(u);
    }
    let cur_snap = BalanceSnapshot::load(db, &cur_bq)?;
    let prev_snap = BalanceSnapshot::load(db, &prev_bq)?;
    let cur_table =
        fincore::report::render_single(&def, &cur_snap, "", "", fincore::report::identity);
    let prev_table =
        fincore::report::render_single(&def, &prev_snap, "", "", fincore::report::identity);
    let mut out = Vec::with_capacity(cur_table.rows.len());
    for (i, r) in cur_table.rows.iter().enumerate() {
        let cur = r.values.first().copied().unwrap_or(Money::ZERO);
        let prev = prev_table
            .rows
            .get(i)
            .and_then(|x| x.values.first())
            .copied()
            .unwrap_or(Money::ZERO);
        let diff = cur - prev;
        let rate = if prev.is_zero() {
            Money::ZERO
        } else {
            // 分母取绝对值只用于统一比例基准；分子必须保留方向，否则收入下降
            // 会被显示成“+20%”，和差额的负号自相矛盾。
            (diff * Money::from_i64(100))
                .checked_div(prev.abs().inner())
                .expect("prev 已判非零")
                .round2()
        };
        out.push(CompareRow {
            no: r.no.clone(),
            name: r.name.clone(),
            indent: r.indent,
            style: r.style,
            current: cur,
            previous: prev,
            diff,
            rate,
        });
    }
    Ok(out)
}

/// 科目日报表：按日期汇总某科目的借贷发生额与日末余额
#[derive(Clone, Debug, serde::Serialize)]
pub struct DailyRow {
    pub date: String,
    pub debit: Money,
    pub credit: Money,
    pub balance: Money,
}

/// 科目日报表：期间内按天汇总指定科目（含下级）的借贷发生额，逐日滚动余额。
pub fn account_daily_report(
    db: &Db,
    code: &str,
    from: fincore::Period,
    to: fincore::Period,
    scope: Option<&fincore::user::User>,
    posted_only: bool,
) -> DbResult<Vec<DailyRow>> {
    // 期初快照与逐日行集必须同口径（H-3）：都按 posted_only 过滤
    let mut bq = BalanceQuery::range(from, to).with_posted_only(posted_only);
    if let Some(u) = scope {
        bq = bq.with_user_scope(u).with_dept_scope(u);
    }
    let snap = BalanceSnapshot::load(db, &bq)?;
    let begin = snap.for_account(code, None).begin;
    let pattern = format!("{}%", crate::escape_like(code));
    let status_cond = if posted_only {
        "v.status = 'posted'"
    } else {
        "v.status != 'void'"
    };
    let mut sql = format!(
        "SELECT v.date, e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
         WHERE {status_cond} AND e.period BETWEEN ?1 AND ?2
           AND e.account_code LIKE ?3 ESCAPE '\\'",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(from.ymm()), Box::new(to.ymm()), Box::new(pattern)];
    if let Some(u) = scope {
        crate::push_report_scope(&mut sql, &mut params, &u.data_scope, &u.username);
        // 日报表是账簿口径：与期初快照一致地按部门收窄（凭证列表同一条判定）
        if let Some(c) = crate::balances::dept_scope_cond("e", &u.data_scope.depts, &mut params) {
            sql.push_str(&format!(" AND {c}"));
        }
    }
    sql.push_str(" ORDER BY v.date");
    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(refs.as_slice())?;
    // 按日期聚合
    let mut map: std::collections::BTreeMap<String, (Money, Money)> =
        std::collections::BTreeMap::new();
    while let Some(r) = rows.next()? {
        let d: String = r.get(0)?;
        let dr = read_money(r, 1)?;
        let cr = read_money(r, 2)?;
        let e = map.entry(d).or_insert((Money::ZERO, Money::ZERO));
        e.0 += dr;
        e.1 += cr;
    }
    let mut running = begin;
    let mut out = Vec::with_capacity(map.len());
    for (date, (debit, credit)) in map {
        running += debit - credit;
        out.push(DailyRow {
            date,
            debit,
            credit,
            balance: running,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 期末对账
// ---------------------------------------------------------------------------

/// 期末对账检查项
#[derive(Clone, Debug, serde::Serialize)]
pub struct ReconcileItem {
    /// 检查项名称
    pub name: String,
    /// 是否通过
    pub ok: bool,
    /// 明细说明
    pub detail: String,
}

/// 统计"还有多少笔没处理干净"。
///
/// **查询失败绝不能当成 0**：结账前的控制项在 `SQLITE_BUSY` / 表损坏 / 表缺失时
/// 报"0 笔未处理"，对账照样通过——一个会在出错时静默放行的控制比没有控制更糟。
/// 这里把错误一并带回，由调用方出一项 `ok: false` 的检查。
fn loose_count(db: &Db, sql: &str, period: fincore::Period) -> Result<i64, String> {
    db.conn()
        .query_row(sql, rusqlite::params![period.ymm()], |r| r.get(0))
        .map_err(|e| e.to_string())
}

/// 统一的检查项：计数成功就按 0 判定，失败则 `ok: false` 并说明原因
fn loose_item(
    db: &Db,
    name: &str,
    sql: &str,
    period: fincore::Period,
    ok_detail: impl Fn(i64) -> String,
) -> ReconcileItem {
    match loose_count(db, sql, period) {
        Ok(n) => ReconcileItem {
            name: name.into(),
            ok: n == 0,
            detail: ok_detail(n),
        },
        Err(e) => ReconcileItem {
            name: name.into(),
            ok: false,
            detail: format!("统计失败：{e}（不能据此判断是否已处理干净）"),
        },
    }
}

/// 期末对账：试算平衡 + 总账/明细账一致 + 银行未达账项 + 未生成凭证的业务单据。
/// 返回检查项清单，全部 ok 即对账通过。
pub fn period_reconcile(db: &Db, period: fincore::Period) -> DbResult<Vec<ReconcileItem>> {
    use fincore::balance::TrialBalance;
    let mut out = Vec::new();
    let snap = BalanceSnapshot::load(db, &BalanceQuery::period(period))?;
    let chart = accounts::chart(db)?;

    // 1. 试算平衡
    let trial: TrialBalance = snap.trial_balance(&chart);
    let balanced = trial.period_balanced();
    out.push(ReconcileItem {
        name: "试算平衡".into(),
        ok: balanced,
        detail: if balanced {
            format!(
                "借方 {} = 贷方 {}",
                trial.period_debit.fmt_money(),
                trial.period_credit.fmt_money()
            )
        } else {
            let diff = (trial.period_debit - trial.period_credit).abs();
            format!(
                "借方 {} ≠ 贷方 {}，差 {}",
                trial.period_debit.fmt_money(),
                trial.period_credit.fmt_money(),
                diff.fmt_money()
            )
        },
    });

    // 2. 期末余额方向合法性：资产负债科目余额方向不匹配则列出
    let mut wrong_dir = Vec::new();
    for a in chart.all() {
        if !chart.is_leaf(&a.code) {
            continue;
        }
        let r = snap.for_account(&a.code, None);
        let end = r.end();
        if end.is_zero() {
            continue;
        }
        let expected_credit = a.category.default_dir() == fincore::account::Direction::Credit;
        let is_credit = end.is_negative();
        if expected_credit != is_credit {
            wrong_dir.push(format!("{}({})", a.code, a.name));
        }
    }
    if wrong_dir.is_empty() {
        out.push(ReconcileItem {
            name: "余额方向检查".into(),
            ok: true,
            detail: "全部科目余额方向正常".into(),
        });
    } else {
        out.push(ReconcileItem {
            name: "余额方向检查".into(),
            ok: false,
            detail: format!(
                "{} 个科目余额方向异常：{}",
                wrong_dir.len(),
                wrong_dir
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("、")
            ),
        });
    }

    // 3. 银行未达账项（本期银行对账单未勾对的笔数）
    out.push(loose_item(
        db,
        "银行未达账项",
        "SELECT COUNT(*) FROM bank_statement WHERE period=?1 AND entry_id IS NULL",
        period,
        |n| format!("本期银行对账单未勾对 {n} 笔"),
    ));

    // 4. 未生成凭证的业务单据（存货流水/工资/报销 缺 voucher_id）
    const BIZ_SQLS: [&str; 3] = [
        "SELECT COUNT(*) FROM stock_move WHERE period=?1 AND voucher_id IS NULL",
        "SELECT COUNT(*) FROM payroll WHERE period=?1 AND voucher_id IS NULL",
        "SELECT COUNT(*) FROM expense_claim WHERE period=?1 AND status='paid' AND voucher_id IS NULL",
    ];
    let mut loose = 0i64;
    let mut biz_errs: Vec<String> = Vec::new();
    for sql in BIZ_SQLS {
        match loose_count(db, sql, period) {
            Ok(n) => loose += n,
            Err(e) => biz_errs.push(e),
        }
    }
    out.push(if biz_errs.is_empty() {
        ReconcileItem {
            name: "业务单据生成凭证".into(),
            ok: loose == 0,
            detail: format!("存货/工资/报销尚有 {loose} 笔未生成凭证"),
        }
    } else {
        ReconcileItem {
            name: "业务单据生成凭证".into(),
            ok: false,
            detail: format!(
                "统计失败：{}（不能据此判断是否已生成凭证）",
                biz_errs.join("；")
            ),
        }
    });

    Ok(out)
}

// ---------------------------------------------------------------------------
// 管理员「账目总览」：只读视角的账目全貌
// ---------------------------------------------------------------------------

/// 总览数据（口径与仪表盘 / 资产负债表 / 利润表一致）
#[derive(Clone, Debug)]
pub struct Overview {
    pub company: String,
    pub period: fincore::Period,
    pub closed_upto: Option<fincore::Period>,
    /// 凭证总数（全账套）
    pub vouchers: i64,
    /// 分录总数（全账套）
    pub entries: i64,
    /// 科目数（全账套）
    pub accounts: i64,
    /// 当期未记账张数（含历史"已审核"）
    pub unposted: i64,
    /// 当期已记账张数
    pub posted: i64,
    /// 财务总量（年初至今）
    pub totals: crate::advanced::FinTotals,
    /// 进项发票：（价税合计, 张数）
    pub invoice_in: (Money, i64),
    /// 销项发票：（价税合计, 张数）
    pub invoice_out: (Money, i64),
    /// 最近 10 张凭证（全账套，含分录摘要与借贷合计）
    pub recent: Vec<fincore::Voucher>,
}

/// 汇总账目全貌，供管理员只读查看（不做任何写操作）
pub fn overview(db: &Db, period: fincore::Period) -> DbResult<Overview> {
    let company = db.options().company.clone();
    let closed_upto = crate::periods::closed_upto(db)?;
    let (vouchers, entries, accounts) = db.stats()?;
    let (draft, audited, posted, _void) = crate::vouchers::status_summary(db, period)?;
    let from = fincore::Period::new(period.year(), 1).unwrap_or(period);
    let totals = crate::advanced::financial_totals(db, period, from, None)?;
    let mut invoice_in = (Money::ZERO, 0i64);
    let mut invoice_out = (Money::ZERO, 0i64);
    for (kind, amount_tax, _tax, count) in crate::invoices::summary(db)? {
        match kind.as_str() {
            "in" => invoice_in = (amount_tax, count),
            "out" => invoice_out = (amount_tax, count),
            _ => {}
        }
    }
    let q = crate::vouchers::VoucherQuery {
        asc: false,
        limit: Some(10),
        ..Default::default()
    };
    let mut recent = crate::vouchers::list(db, &q)?;
    crate::vouchers::fill_entries(db, &mut recent)?;
    Ok(Overview {
        company,
        period,
        closed_upto,
        vouchers,
        entries,
        accounts,
        unposted: draft + audited,
        posted,
        totals,
        invoice_in,
        invoice_out,
        recent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invoices;
    use crate::tests::mem;
    use crate::vouchers;
    use chrono::NaiveDate;
    use fincore::{AuxRef, Entry, Period, Voucher};

    #[test]
    fn default_defs_seeded() {
        let db = mem();
        let bs = get_def(&db, "balance_sheet").unwrap().unwrap();
        assert_eq!(bs.name, "资产负债表");
        let inc = get_def(&db, "income_statement").unwrap().unwrap();
        assert_eq!(inc.name, "利润表");
        assert!(!list_defs(&db).unwrap().is_empty());
    }

    #[test]
    fn def_roundtrip() {
        let db = mem();
        let mut def = get_def(&db, "income_statement").unwrap().unwrap();
        def.name = "自定义利润表".into();
        save_def(&db, &def).unwrap();
        assert_eq!(
            get_def(&db, "income_statement").unwrap().unwrap().name,
            "自定义利润表"
        );
        reset_def(&db, "income_statement").unwrap();
        assert_eq!(
            get_def(&db, "income_statement").unwrap().unwrap().name,
            "利润表"
        );
    }

    fn cash_voucher(db: &Db, p: Period, day: u32, entries: Vec<(&str, &str, &str, Option<&str>)>) {
        let d = NaiveDate::from_ymd_opt(p.year(), p.month(), day).unwrap();
        let mut v = Voucher::new(p, d, "记", vouchers::next_no(db, p, "记").unwrap());
        v.prepared_by = "张三".to_string();
        let mut i = 0;
        for (code, side, amt, cf) in entries {
            i += 1;
            let m = Money::parse(amt).unwrap();
            let mut e = Entry::new(i, code, "现金流测试");
            if side == "借" {
                e.debit = m;
            } else {
                e.credit = m;
            }
            if let Some(c) = cf {
                e.aux.cash_flow = Some(c.to_string());
            }
            fill_required(db, &mut e);
            v.push_entry(e);
        }
        let id = vouchers::save(db, &mut v).unwrap();
        vouchers::post(db, id, "王五").unwrap();
    }

    /// 按科目表补齐辅助核算与数量（save 会强校验，测试分录必须先合规）
    fn fill_required(db: &Db, e: &mut Entry) {
        let Ok(chart) = crate::accounts::chart(db) else {
            return;
        };
        let Some(a) = chart.get(&e.account_code).cloned() else {
            return;
        };
        for k in a.aux.list() {
            if e.aux.get(k).is_some() {
                continue;
            }
            let v = match k {
                fincore::AuxKind::Bank => "B01",
                fincore::AuxKind::Customer => "C01",
                fincore::AuxKind::Supplier => "S01",
                fincore::AuxKind::Item => "I01",
                fincore::AuxKind::Dept => "D01",
                fincore::AuxKind::Employee => "E01",
                fincore::AuxKind::Project => "P01",
                fincore::AuxKind::CashFlow => continue,
            };
            e.aux.set(k, Some(v.to_string()));
        }
        if a.has_qty && e.qty.is_none() {
            let amt = if e.debit.is_positive() {
                e.debit
            } else {
                e.credit
            };
            e.qty = Some(Money::ONE);
            e.price = Some(amt);
        }
    }

    #[test]
    fn cash_flow_ties() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 销售收款 10 万（流入 0101）
        cash_voucher(
            &db,
            p,
            5,
            vec![
                ("1001", "借", "100000", Some("0101")),
                ("600101", "贷", "100000", None),
            ],
        );
        // 采购付款 4 万（流出 0104）
        cash_voucher(
            &db,
            p,
            6,
            vec![
                ("140501", "借", "40000", None),
                ("1001", "贷", "40000", Some("0104")),
            ],
        );

        let stmt = cash_flow_statement(&db, p, p, None).unwrap();
        assert_eq!(stmt.operating_net, Money::parse("60000").unwrap());
        assert_eq!(stmt.net_increase, Money::parse("60000").unwrap());
        assert_eq!(stmt.begin_cash, Money::ZERO);
        assert_eq!(stmt.end_cash, Money::parse("60000").unwrap());
        assert!(stmt.ties(), "现金流量表净增加额应与货币资金变动勾稽");
        assert_eq!(stmt.unassigned, Money::ZERO);
    }

    /// 未归集只出现在「猜不了」的时候
    ///
    /// 以前是「没标就一律未归集」；现在没标时先按对方科目推断（见
    /// `fincore::report::cashflow_rules`）。所以这条用例改成真正猜不了的情形：
    /// 一笔 5,000 的收款，对方科目一半像收入、一半像卖设备。
    #[test]
    fn unassigned_reported_only_when_ambiguous() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        cash_voucher(
            &db,
            p,
            5,
            vec![
                ("1001", "借", "5000", None),
                ("600101", "贷", "3000", None),
                ("160101", "贷", "2000", None),
            ],
        );
        let stmt = cash_flow_statement(&db, p, p, None).unwrap();
        assert_eq!(stmt.unassigned, Money::parse("5000").unwrap());
        assert_eq!(stmt.ambiguous, 1, "对方科目指向两个项目，不能猜");
        assert_eq!(stmt.auto_assigned, 0);
        assert!(stmt.ties());
        assert!(stmt.assignment_note().contains("未归集"));
    }

    /// 没标注但对方科目单一：按规则归集到 0101，不再算未归集
    #[test]
    fn unannotated_but_inferable_is_auto_assigned() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        cash_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "5000", None), ("600101", "贷", "5000", None)],
        );
        let stmt = cash_flow_statement(&db, p, p, None).unwrap();
        assert_eq!(stmt.unassigned, Money::ZERO, "对方科目是收入，能推出来");
        assert_eq!(stmt.auto_assigned, 1);
        assert_eq!(stmt.operating_net, Money::parse("5000").unwrap());
        assert!(stmt.ties());
    }

    #[test]
    fn cash_accounts_found() {
        let db = mem();
        let codes = cash_accounts(&db).unwrap();
        assert!(codes.contains(&"1001".to_string()));
        assert!(codes.contains(&"100201".to_string()));
        assert!(!codes.contains(&"1002".to_string()), "非末级不应计入");
    }

    #[test]
    fn items_seeded() {
        let db = mem();
        let items = cash_flow_items(&db).unwrap();
        assert!(items.len() >= 15);
        assert!(items.iter().any(|i| i.code == "0101"));
    }

    #[test]
    fn aux_unused_placeholder() {
        // 保持 AuxRef 引用，避免未使用告警影响整洁
        let _ = AuxRef::default().key();
    }

    #[test]
    fn equity_statement_computed() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 实收资本 100 万（贷方）
        cash_voucher(
            &db,
            p,
            5,
            vec![
                ("1001", "借", "1000000", Some("0301")),
                ("4001", "贷", "1000000", None),
            ],
        );
        let stmt = equity_statement(&db, p, p, None).unwrap();
        let capital = stmt.lines.iter().find(|l| l.name == "实收资本").unwrap();
        assert_eq!(capital.begin, Money::ZERO);
        assert_eq!(capital.change, Money::parse("1000000").unwrap());
        assert_eq!(capital.end, Money::parse("1000000").unwrap());
        assert!(stmt.ties());
    }

    /// H4：权益表的未分配利润要与资产负债表同口径 —— 4103 + 4104 + 未结转损益。
    #[test]
    fn equity_statement_matches_balance_sheet_equity() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 实收资本 100 万
        cash_voucher(
            &db,
            p,
            1,
            vec![
                ("1001", "借", "1000000", None),
                ("4001", "贷", "1000000", None),
            ],
        );
        // 收入 500、税金 200 → 未结转净利 300
        cash_voucher(
            &db,
            p,
            2,
            vec![("1001", "借", "500", None), ("600101", "贷", "500", None)],
        );
        cash_voucher(
            &db,
            p,
            3,
            vec![("6403", "借", "200", None), ("1001", "贷", "200", None)],
        );
        // 本年利润 700、利润分配-未分配利润 100
        cash_voucher(
            &db,
            p,
            4,
            vec![("1001", "借", "700", None), ("4103", "贷", "700", None)],
        );
        cash_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "100", None), ("410401", "贷", "100", None)],
        );

        let stmt = equity_statement(&db, p, p, None).unwrap();
        let na = stmt.lines.iter().find(|l| l.name == "未分配利润").unwrap();
        // 300（未结转）+ 700（4103）+ 100（4104）= 1100
        assert_eq!(na.end, Money::parse("1100").unwrap());
        assert!(stmt.ties());

        let t = crate::advanced::financial_totals(&db, p, p, None).unwrap();
        assert_eq!(
            stmt.total.end, t.equity,
            "权益表合计必须等于资产负债表权益（资产-负债）"
        );
    }

    #[test]
    fn daily_report_rolls_balance() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        cash_voucher(
            &db,
            p,
            5,
            vec![
                ("1001", "借", "1000", Some("0101")),
                ("600101", "贷", "1000", None),
            ],
        );
        cash_voucher(
            &db,
            p,
            9,
            vec![
                ("1001", "借", "500", Some("0103")),
                ("6301", "贷", "500", None),
            ],
        );
        let rows = account_daily_report(&db, "1001", p, p, None, true).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].balance, Money::parse("1000").unwrap());
        assert_eq!(rows[1].balance, Money::parse("1500").unwrap());
    }

    #[test]
    fn report_compare_diff() {
        let db = mem();
        let p1 = Period::new(2026, 1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        cash_voucher(
            &db,
            p1,
            5,
            vec![
                ("1001", "借", "1000", Some("0101")),
                ("600101", "贷", "1000", None),
            ],
        );
        cash_voucher(
            &db,
            p2,
            5,
            vec![
                ("1001", "借", "3000", Some("0101")),
                ("600101", "贷", "3000", None),
            ],
        );
        let rows = report_compare(&db, "income_statement", p2, p2, p1, p1, None).unwrap();
        // 营业收入行：本期 3000 vs 上期 1000 → 差额 2000
        let rev = rows.iter().find(|r| r.no == "1").unwrap();
        assert_eq!(rev.current, Money::parse("3000").unwrap());
        assert_eq!(rev.previous, Money::parse("1000").unwrap());
        assert_eq!(rev.diff, Money::parse("2000").unwrap());
        assert_eq!(rev.rate, Money::parse("200").unwrap());
    }

    #[test]
    fn report_compare_rate_keeps_negative_sign() {
        let db = mem();
        let p1 = Period::new(2026, 1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        cash_voucher(
            &db,
            p1,
            5,
            vec![
                ("1001", "借", "1000", Some("0101")),
                ("600101", "贷", "1000", None),
            ],
        );
        cash_voucher(
            &db,
            p2,
            5,
            vec![
                ("1001", "借", "800", Some("0101")),
                ("600101", "贷", "800", None),
            ],
        );
        let rows = report_compare(&db, "income_statement", p2, p2, p1, p1, None).unwrap();
        let rev = rows.iter().find(|r| r.no == "1").unwrap();
        assert_eq!(rev.diff, Money::parse("-200").unwrap());
        assert_eq!(rev.rate, Money::parse("-20").unwrap());
    }

    #[test]
    fn period_reconcile_lists_checks() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        cash_voucher(
            &db,
            p,
            5,
            vec![
                ("1001", "借", "500", Some("0101")),
                ("600101", "贷", "500", None),
            ],
        );
        let items = period_reconcile(&db, p).unwrap();
        assert!(items.len() >= 3);
        assert!(
            items.iter().all(|i| i.ok),
            "空账套+平衡凭证应全部通过：{:?}",
            items.iter().map(|i| &i.name).collect::<Vec<_>>()
        );
    }

    /// 回归：COUNT 查询失败绝不能当成"0 笔未处理"让对账通过。
    /// 旧的三个 `unwrap_or(0)` 在表缺失 / SQLITE_BUSY 时报"未勾对 0 笔"，
    /// 结账前的控制项就在出错的情况下静默放行。
    #[test]
    fn period_reconcile_reports_query_failure_instead_of_zero() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        cash_voucher(
            &db,
            p,
            5,
            vec![
                ("1001", "借", "500", Some("0101")),
                ("600101", "贷", "500", None),
            ],
        );
        // 正常时全部通过
        assert!(period_reconcile(&db, p).unwrap().iter().all(|i| i.ok));

        // 制造真实的查询失败：把银行对账单表改名
        db.conn()
            .execute_batch("ALTER TABLE bank_statement RENAME TO bank_statement_x;")
            .unwrap();
        let items = period_reconcile(&db, p).unwrap();
        assert!(
            !items.iter().all(|i| i.ok),
            "查询失败时对账不得通过：{:?}",
            items
        );
        let bank = items.iter().find(|i| i.name == "银行未达账项").unwrap();
        assert!(!bank.ok, "银行未达账项必须报失败");
        assert!(
            bank.detail.contains("统计失败"),
            "必须说明是统计失败而非 0 笔：{}",
            bank.detail
        );

        // 业务单据那一项同理
        db.conn()
            .execute_batch("ALTER TABLE expense_claim RENAME TO expense_claim_x;")
            .unwrap();
        let items = period_reconcile(&db, p).unwrap();
        let biz = items.iter().find(|i| i.name == "业务单据生成凭证").unwrap();
        assert!(!biz.ok, "业务单据项必须报失败");
        assert!(biz.detail.contains("统计失败"), "{}", biz.detail);
    }

    #[test]
    fn overview_aggregates() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        cash_voucher(
            &db,
            p,
            5,
            vec![
                ("1001", "借", "1000", Some("0101")),
                ("600101", "贷", "1000", None),
            ],
        );

        let inv = invoices::Invoice {
            id: 0,
            kind: "in".into(),
            code: "044001900111".into(),
            number: "INV001".into(),
            date: "2026-01-10".into(),
            buyer: "甲公司".into(),
            seller: "乙公司".into(),
            amount_tax: Money::parse("1130").unwrap(),
            amount: Money::parse("1000").unwrap(),
            tax: Money::parse("130").unwrap(),
            tax_rate: "0.13".into(),
            status: "pending".into(),
            memo: String::new(),
            attach_id: 0,
            created_by: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        invoices::insert(&db, &inv, "张三").unwrap();

        let o = overview(&db, p).unwrap();
        assert_eq!(o.vouchers, 1, "凭证总数=1");
        assert_eq!(o.entries, 2, "分录总数=2");
        assert_eq!(
            o.accounts,
            fincore::chart::default_accounts().len() as i64,
            "科目数=内置科目数"
        );
        assert_eq!(o.unposted, 0);
        assert_eq!(o.posted, 1);
        assert_eq!(o.totals.total_asset, Money::parse("1000").unwrap());
        assert_eq!(o.totals.revenue, Money::parse("1000").unwrap());
        assert_eq!(o.invoice_in, (Money::parse("1130").unwrap(), 1));
        assert_eq!(o.invoice_out, (Money::ZERO, 0));
        assert_eq!(o.recent.len(), 1);
        assert_eq!(o.recent[0].debit_total(), Money::parse("1000").unwrap());
    }
}

#[cfg(test)]
mod compare_window_tests {
    use super::prior_range;
    use fincore::Period;

    fn p(s: &str) -> Period {
        Period::parse(s).expect("期间")
    }

    #[test]
    fn single_month_takes_the_month_before() {
        let (f, t) = prior_range(p("2026-03"), p("2026-03"));
        assert_eq!(
            (f.code(), t.code()),
            ("2026-02".to_string(), "2026-02".to_string())
        );
    }

    #[test]
    fn quarter_takes_the_previous_quarter_not_one_month() {
        let (f, t) = prior_range(p("2026-04"), p("2026-06"));
        assert_eq!(f.code(), "2026-01", "3 个月就取上一个 3 个月");
        assert_eq!(t.code(), "2026-03");
    }

    #[test]
    fn window_crosses_the_year_boundary() {
        let (f, t) = prior_range(p("2026-01"), p("2026-02"));
        assert_eq!(
            (f.code(), t.code()),
            ("2025-11".to_string(), "2025-12".to_string())
        );
    }
}
