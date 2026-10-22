//! 余额与账簿的实时聚合
//!
//! 设计要点：**不物化余额表**。
//! 余额 = 期初 + 已记账凭证发生额，每次查询实时算。单机 SQLite 下十万级分录仍是毫秒级，
//! 换来的是"改一张凭证、账簿立刻正确"，不需要维护缓存一致性——这在财务软件里值这个价。
//!
//! 所有金额累加都在 Rust 侧用 `Decimal` 完成，绕开 SQLite 无十进制类型的短板。

use std::collections::BTreeMap;

use chrono::NaiveDate;
use fincore::report::{AmountKind, BalanceSource};
use fincore::{
    AuxKind, AuxRef, BalanceRow, Chart, GeneralLedgerRow, JournalRow, LedgerRow, Money, Period,
    QtyRow, TrialBalance,
};

use crate::{accounts, read_money, read_money_opt, Db, DbResult};

/// 聚合键：(科目编码, 辅助核算键)
type Key = (String, String);

/// 余额查询条件
#[derive(Clone, Debug)]
pub struct BalanceQuery {
    /// 起始期间（期初余额取该期间月初）
    pub from: Period,
    /// 结束期间（本期发生额 = from..=to 的合计）
    pub to: Period,
    /// 科目范围
    pub code_from: Option<String>,
    pub code_to: Option<String>,
    /// 只看某个辅助核算维度
    pub aux: Option<AuxRef>,
    /// 只保留末级科目
    pub only_leaf: bool,
    /// 只保留有余额或有发生额的行
    pub non_zero_only: bool,
    /// 只显示到第几级科目（None 表示全部级次）
    pub max_level: Option<u8>,
    /// 只统计已记账凭证（**H-3 定案：默认 true**——草稿与已审核未记账一律不入余额；
    /// 显式传 false 时为"含未记账（排除作废）"的临时查看口径）
    pub posted_only: bool,
    /// 剔除结转损益凭证（默认 false —— 试算/余额/资产负债表必须含它）。
    ///
    /// 只给**利润表**开：结转后损益科目借贷相抵，不剔的话结完账利润表
    /// 全是 0。折旧/调汇/自动转账等真实费用不受影响（只认 source='carry_forward'）。
    pub exclude_carry: bool,
    /// 只统计某人填制的凭证（数据范围 own_voucher_only）
    pub prepared_by: Option<String>,
}

impl BalanceQuery {
    pub fn period(p: Period) -> Self {
        Self {
            from: p,
            to: p,
            code_from: None,
            code_to: None,
            aux: None,
            only_leaf: false,
            non_zero_only: false,
            max_level: None,
            posted_only: true,
            prepared_by: None,
            exclude_carry: false,
        }
    }
    pub fn range(from: Period, to: Period) -> Self {
        Self {
            from,
            to,
            ..BalanceQuery::period(from)
        }
    }
    pub fn with_leaf_only(mut self, on: bool) -> Self {
        self.only_leaf = on;
        self
    }
    pub fn with_non_zero(mut self, on: bool) -> Self {
        self.non_zero_only = on;
        self
    }
    pub fn with_max_level(mut self, lv: Option<u8>) -> Self {
        self.max_level = lv;
        self
    }
    /// 只统计已记账凭证（账簿"只含已记账"勾选项；false = 含未记账、排除作废）
    pub fn with_posted_only(mut self, on: bool) -> Self {
        self.posted_only = on;
        self
    }
    /// 利润表专用：剔除结转损益凭证（其它口径一律别开）
    pub fn with_exclude_carry(mut self, on: bool) -> Self {
        self.exclude_carry = on;
        self
    }
    pub fn with_code_range(mut self, from: Option<String>, to: Option<String>) -> Self {
        self.code_from = from;
        self.code_to = to;
        self
    }
}

impl BalanceQuery {
    /// 套用用户的数据范围（科目范围）：与查询已有的科目范围取交集。
    ///
    /// 下界取较大者、上界取较小者（科目编码前缀可比，相同前 4 位时逐级更严）。
    pub fn with_data_scope(mut self, scope: &fincore::user::DataScope) -> Self {
        let lo = scope.account_from.trim();
        let hi = scope.account_to.trim();
        if lo.is_empty() && hi.is_empty() {
            return self;
        }
        let lo = (!lo.is_empty()).then(|| lo.to_string());
        let hi = (!hi.is_empty()).then(|| hi.to_string());
        self.code_from = match (self.code_from.take(), lo) {
            (Some(a), Some(b)) => Some(if a >= b { a } else { b }),
            (a, b) => a.or(b),
        };
        self.code_to = match (self.code_to.take(), hi) {
            (Some(a), Some(b)) => Some(if a <= b { a } else { b }),
            (a, b) => a.or(b),
        };
        self
    }

    /// 套用用户完整数据范围：科目区间 + "仅本人填制的凭证"。
    /// 报表/账簿统一用它，避免"配置了范围却看到全量"的静默越权。
    pub fn with_user_scope(mut self, user: &fincore::user::User) -> Self {
        let own = user.data_scope.own_voucher_only;
        self = self.with_data_scope(&user.data_scope);
        if own {
            self.prepared_by = Some(user.username.clone());
        }
        self
    }
}

/// 某一时点的余额快照。加载一次，多处复用。
#[derive(Clone, Debug, Default)]
pub struct BalanceSnapshot {
    pub from: Period,
    pub to: Period,
    /// (科目, 辅助核算) → 余额行
    rows: BTreeMap<Key, BalanceRow>,
    /// (科目, 辅助核算) → 数量行
    qtys: BTreeMap<Key, QtyRow>,
    /// **按科目编码的预聚合：自身 + 全部下级合计**。
    /// 科目编码是层级前缀（`1002` ⊃ `100201` ⊃ `10020101`），
    /// 在 `load()` 末尾按编码排序后用栈做一次"前缀累加"得到。
    /// 之后 `for_account(code)` 变成 **O(log R) 查表**，而不是 O(R) 线性扫。
    by_code: BTreeMap<String, BalanceRow>,
    /// 加载快照时的科目表。报表需要按科目类别取数（如"未结转损益净额"），
    /// 缓存下来避免调用方到处传 chart。
    chart: Option<Chart>,
}

impl BalanceSnapshot {
    /// 加载快照
    pub fn load(db: &Db, q: &BalanceQuery) -> DbResult<Self> {
        let start = db.options().start_period;
        // 本年累计只在「报告期与建账期同一年」时才包含建账前的累计发生额。
        // 跨年后，begin_balance 里的 debit_accum/credit_accum 属于上一年，
        // 不能再算进本年的 YTD，否则“本年累计”会从 1 月起就带着上年数。
        let same_year = q.to.year() == start.year();
        let ytd_from = if same_year {
            start.ymm()
        } else {
            q.to.year() * 100 + 1
        };
        let mut rows: BTreeMap<Key, BalanceRow> = BTreeMap::new();
        let mut qtys: BTreeMap<Key, QtyRow> = BTreeMap::new();

        // 1) 期初：启用期之前的累计
        {
            let mut sql = String::from(
                "SELECT account_code, aux_key, aux_json, year_begin, debit_accum, credit_accum, qty_begin
                 FROM begin_balance",
            );
            let mut conds: Vec<String> = Vec::new();
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            if let Some(f) = &q.code_from {
                params.push(Box::new(f.clone()));
                conds.push(format!("account_code >= ?{}", params.len()));
            }
            if let Some(t) = &q.code_to {
                params.push(Box::new(t.clone()));
                conds.push(format!("account_code <= ?{}", params.len()));
            }
            if !conds.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&conds.join(" AND "));
            }
            let mut stmt = db.conn().prepare(&sql)?;
            let refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|b| b.as_ref()).collect();
            let mut r = stmt.query(refs.as_slice())?;
            while let Some(row) = r.next()? {
                let code: String = row.get(0)?;
                let aux_key: String = row.get(1)?;
                let aux_json: String = row.get(2)?;
                let aux: AuxRef = serde_json::from_str(&aux_json).unwrap_or_default();
                let yb = read_money(row, 3)?;
                let ad = read_money(row, 4)?;
                let ac = read_money(row, 5)?;
                let qb = read_money_opt(row, 6)?.unwrap_or(Money::ZERO);
                let key = (code.clone(), aux_key);
                let e = rows.entry(key.clone()).or_insert_with(|| BalanceRow {
                    account_code: code,
                    account_name: String::new(),
                    aux,
                    ..Default::default()
                });
                // 期初 = 年初 + 累计借 - 累计贷
                e.begin = yb + ad - ac;
                if same_year {
                    e.ytd_debit = ad;
                    e.ytd_credit = ac;
                }
                if !qb.is_zero() {
                    qtys.insert(
                        key,
                        QtyRow {
                            begin: qb,
                            ..Default::default()
                        },
                    );
                }
            }
        }

        // 2) 已记账凭证：一次扫描，同时算出期初、本期发生额、本年累计
        {
            let status_filter = if q.posted_only {
                "v.status = 'posted'"
            } else {
                "v.status != 'void'"
            };
            let mut sql = format!(
                "SELECT e.account_code, e.aux_key, e.aux_json, e.period, e.debit, e.credit, e.qty
                 FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
                 WHERE {status_filter} AND e.period <= ?1"
            );
            // 利润表专用：结转损益凭证（source='carry_forward'）不进发生额，
            // 否则结完账利润表全是 0。其它口径一律含它。
            if q.exclude_carry {
                sql.push_str(" AND v.source <> 'carry_forward'");
            }
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(q.to.ymm())];
            if let Some(f) = &q.code_from {
                params.push(Box::new(f.clone()));
                sql.push_str(&format!(" AND e.account_code >= ?{}", params.len()));
            }
            if let Some(t) = &q.code_to {
                params.push(Box::new(t.clone()));
                sql.push_str(&format!(" AND e.account_code <= ?{}", params.len()));
            }
            if let Some(who) = &q.prepared_by {
                params.push(Box::new(who.clone()));
                sql.push_str(&format!(" AND v.prepared_by = ?{}", params.len()));
            }
            let mut stmt = db.conn().prepare(&sql)?;
            let refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|b| b.as_ref()).collect();
            let mut r = stmt.query(refs.as_slice())?;
            while let Some(row) = r.next()? {
                let code: String = row.get(0)?;
                let aux_key: String = row.get(1)?;
                let aux_json: String = row.get(2)?;
                let period = Period::from_ymm(row.get(3)?);
                let d = read_money(row, 4)?;
                let c = read_money(row, 5)?;
                let qty = read_money_opt(row, 6)?.unwrap_or(Money::ZERO);
                let aux: AuxRef = serde_json::from_str(&aux_json).unwrap_or_default();
                let key = (code.clone(), aux_key);

                let e = rows.entry(key.clone()).or_insert_with(|| BalanceRow {
                    account_code: code,
                    account_name: String::new(),
                    aux,
                    ..Default::default()
                });

                if period < q.from {
                    // 期初：累加净额与数量
                    e.begin += d - c;
                } else if period <= q.to {
                    e.debit += d;
                    e.credit += c;
                }
                // 本年累计（仅当期所属年度）
                if period.ymm() >= ytd_from && period <= q.to {
                    e.ytd_debit += d;
                    e.ytd_credit += c;
                }

                if !qty.is_zero() {
                    let qr = qtys.entry(key).or_default();
                    if period < q.from {
                        qr.begin += qty;
                    } else if period <= q.to {
                        if qty.is_positive() {
                            qr.in_qty += qty;
                        } else {
                            qr.out_qty += qty.negated();
                        }
                    }
                }
            }
        }

        let chart = accounts::chart(db).ok();
        // ── 前缀预聚合：按科目编码层级累加（自身 + 全部下级）
        /// 科目编码是层级前缀（`1002` ⊃ `100201` ⊃ `10020101`）。
        /// 做法：先把所有 (code, aux) 的直接值按 code 分组求和（忽略 aux 维度），
        /// 再按 code 降序遍历，把每个 code 的值加到它的父 code 上。
        /// 这样每个 code 最终存的是"自身 + 所有下级"的合计。
        let mut by_code: BTreeMap<String, BalanceRow> = BTreeMap::new();

        // 1) 先聚合同一 code 的所有 aux 维度 → 该 code 的直接值
        for ((code, _aux_key), row) in &rows {
            let e = by_code
                .entry(code.clone())
                .or_insert_with(|| BalanceRow {
                    account_code: code.clone(),
                    account_name: String::new(),
                    aux: AuxRef::default(),
                    ..Default::default()
                });
            e.begin += row.begin;
            e.debit += row.debit;
            e.credit += row.credit;
            e.ytd_debit += row.ytd_debit;
            e.ytd_credit += row.ytd_credit;
        }

        // 2) 按 code 降序遍历（子级先于父级），累加到父级
        /// BTreeMap 的 keys() 是升序；`rev()` 变成降序。
        /// 因为科目编码是前缀关系（父 < 子），降序保证先处理子级。
        let codes: Vec<String> = by_code.keys().cloned().collect();
        for code in codes.into_iter().rev() {
            // 找父 code：当前 code 的最长真前缀，且该前缀存在于 by_code 中
            if let Some(parent) = find_parent_code(&code, &by_code) {
                // 先取出子值，再修改父值（避免同时可变/不可变借用）
                let child_vals = {
                    let child = by_code.get(&code).unwrap();
                    (child.begin, child.debit, child.credit, child.ytd_debit, child.ytd_credit)
                };
                if let Some(parent_row) = by_code.get_mut(&parent) {
                    parent_row.begin += child_vals.0;
                    parent_row.debit += child_vals.1;
                    parent_row.credit += child_vals.2;
                    parent_row.ytd_debit += child_vals.3;
                    parent_row.ytd_credit += child_vals.4;
                }
            }
        }

        Ok(Self {
            from: q.from,
            to: q.to,
            rows,
            qtys,
            by_code,
            chart,
        })
    }

    /// 未结转的损益净额：所有末级损益类科目余额之和（正=净亏损，负=净盈利）
    ///
    /// 资产负债表用它把尚未结转的本期盈亏并入"未分配利润"。
    pub fn profit_loss_net(&self, kind: AmountKind) -> Money {
        let chart = match &self.chart {
            Some(c) => c,
            None => return Money::ZERO,
        };
        let mut sum = Money::ZERO;
        for a in chart.all() {
            if !a.category.is_profit_loss() || !chart.is_leaf(&a.code) {
                continue;
            }
            let r = self.for_account(&a.code, None);
            sum += match kind {
                AmountKind::Begin => r.begin,
                AmountKind::End => r.end(),
                AmountKind::PeriodDebit => r.debit,
                AmountKind::PeriodCredit => r.credit,
                AmountKind::YearDebit => r.ytd_debit,
                AmountKind::YearCredit => r.ytd_credit,
                AmountKind::EndQty => Money::ZERO,
            };
        }
        sum
    }

    /// 未经筛选的全部行
    pub fn raw_rows(&self) -> Vec<BalanceRow> {
        self.rows.values().cloned().collect()
    }

    /// 应用查询条件（科目范围、级次、末级、非零）
    pub fn filtered(&self, chart: &Chart, q: &BalanceQuery) -> Vec<BalanceRow> {
        let mut out = Vec::new();
        for (code, aux_key) in self.rows.keys() {
            if let Some(ref f) = q.code_from {
                if code < f {
                    continue;
                }
            }
            if let Some(ref t) = q.code_to {
                if code > t {
                    continue;
                }
            }
            if let Some(ref want) = q.aux {
                let want_key = want.key();
                if !aux_key_contains(aux_key, &want_key) {
                    continue;
                }
            }
            if q.only_leaf && !chart.is_leaf(code) {
                continue;
            }
            if let Some(lv) = q.max_level {
                if chart.level(code) > lv {
                    continue;
                }
            }
            let mut r = self
                .rows
                .get(&(code.clone(), aux_key.clone()))
                .cloned()
                .unwrap();
            r.account_name = chart.get(code).map(|a| a.name.clone()).unwrap_or_default();
            if let Some(qr) = self.qtys.get(&(code.clone(), aux_key.clone())) {
                if !qr.is_zero() {
                    r.qty = Some(*qr);
                }
            }
            if q.non_zero_only && r.is_empty_row() {
                continue;
            }
            out.push(r);
        }
        out.sort_by(|a, b| {
            a.account_code
                .cmp(&b.account_code)
                .then_with(|| a.aux.key().cmp(&b.aux.key()))
        });
        out
    }

    /// 科目余额表：每个科目一行，金额含所有下级
    pub fn account_table(&self, chart: &Chart, q: &BalanceQuery) -> Vec<BalanceRow> {
        let mut out = Vec::new();
        for a in chart.all() {
            if let Some(ref f) = q.code_from {
                if &a.code < f {
                    continue;
                }
            }
            if let Some(ref t) = q.code_to {
                if &a.code > t {
                    continue;
                }
            }
            if q.only_leaf && !chart.is_leaf(&a.code) {
                continue;
            }
            if let Some(lv) = q.max_level {
                if chart.level(&a.code) > lv {
                    continue;
                }
            }
            let mut r = self.for_account(&a.code, q.aux.as_ref());
            r.account_name = a.name.clone();
            // 默认：只显示有活动的科目（有借方、贷方或期初余额）
            // non_zero_only=true 时保持原有行为（此处保持一致性，均为只显示有数据的科目）
            if r.is_empty_row() {
                continue;
            }
            out.push(r);
        }
        out
    }

    /// 汇总某科目（含所有下级、含所有辅助核算维度）的余额
    pub fn for_account(&self, code: &str, aux: Option<&AuxRef>) -> BalanceRow {
        // Fast path: 无 aux 过滤 → 直接查预聚合表 O(log R)
        if aux.is_none() {
            if let Some(agg) = self.by_code.get(code) {
                let mut r = agg.clone();
                r.account_code = code.to_string(); // 保证返回的 account_code 与查询一致
                // 数量账：by_code 不存 qty，需从 rows 聚合（仅在需要 qty 时）
                if self.qtys.iter().any(|((c, _), _)| c.starts_with(code)) {
                    let mut qr = QtyRow::default();
                    for ((c, ak), q) in &self.qtys {
                        if c.starts_with(code) {
                            qr.begin += q.begin;
                            qr.in_qty += q.in_qty;
                            qr.out_qty += q.out_qty;
                        }
                    }
                    if !qr.is_zero() {
                        r.qty = Some(qr);
                    }
                }
                return r;
            }
        }

        // Slow path: 有 aux 过滤 → 线性扫描 rows（预聚合未含 aux 维度）
        let mut r = BalanceRow {
            account_code: code.to_string(),
            ..Default::default()
        };
        let mut qr = QtyRow::default();
        let want_key = aux.map(|a| a.key());
        for ((c, ak), row) in &self.rows {
            if !c.starts_with(code) {
                continue;
            }
            if let Some(ref wk) = want_key {
                if !aux_key_contains(ak, wk) {
                    continue;
                }
            }
            r.begin += row.begin;
            r.debit += row.debit;
            r.credit += row.credit;
            r.ytd_debit += row.ytd_debit;
            r.ytd_credit += row.ytd_credit;
            if let Some(q) = self.qtys.get(&(c.clone(), ak.clone())) {
                qr.begin += q.begin;
                qr.in_qty += q.in_qty;
                qr.out_qty += q.out_qty;
            }
        }
        if !qr.is_zero() {
            r.qty = Some(qr);
        }
        r
    }

    /// 取某科目的辅助核算明细余额（每个辅助维度一行）
    pub fn aux_breakdown(&self, code: &str) -> Vec<BalanceRow> {
        let mut out: Vec<BalanceRow> = self
            .rows
            .iter()
            .filter(|((c, _), _)| c == code)
            .map(|((_, _), r)| r.clone())
            .collect();
        out.sort_by_key(|r| r.aux.key());
        out
    }

    /// 损益类科目的余额行（结转损益取数用）
    pub fn profit_loss_rows(&self, chart: &Chart) -> Vec<BalanceRow> {
        let mut out = Vec::new();
        for a in chart.all() {
            if !a.category.is_profit_loss() || !chart.is_leaf(&a.code) {
                continue;
            }
            let r = self.for_account(&a.code, None);
            if r.is_empty_row() {
                continue;
            }
            let mut r = r;
            r.account_name = a.name.clone();
            out.push(r);
        }
        out
    }

    /// 试算平衡
    pub fn trial_balance(&self, chart: &Chart) -> TrialBalance {
        let mut t = TrialBalance::default();
        for a in chart.all() {
            if !chart.is_leaf(&a.code) {
                continue;
            }
            let r = self.for_account(&a.code, None);
            if r.begin.is_positive() {
                t.begin_debit += r.begin;
            } else {
                t.begin_credit += r.begin.negated();
            }
            t.period_debit += r.debit;
            t.period_credit += r.credit;
            let end = r.end();
            if end.is_positive() {
                t.end_debit += end;
            } else {
                t.end_credit += end.negated();
            }
        }
        t
    }

    /// 期间标签
    pub fn label(&self) -> String {
        if self.from == self.to {
            self.from.label()
        } else {
            format!("{} 至 {}", self.from.label(), self.to.label())
        }
    }
}

impl BalanceSource for BalanceSnapshot {
    fn balance_of(&self, code: &str, aux: Option<&AuxRef>) -> BalanceRow {
        let mut r = self.for_account(code, aux);
        if r.account_name.is_empty() {
            r.account_name = code.to_string();
        }
        r
    }

    fn profit_loss_net(&self, kind: AmountKind) -> Money {
        BalanceSnapshot::profit_loss_net(self, kind)
    }
}

/// 辅助核算键的包含判断：`\u{1f}` 分隔的 `kind=value` 串
///
/// 设为 `pub`：客户视图（customers.rs）要按客户维度过滤与分组，而 aux_key 的
/// 格式**只有这一处定义** —— 各自拼字符串比较迟早对不上。
///
/// 也不要改成整条子串 LIKE：旧实现那样做过，`%customer=C001%` 会把
/// `customer=C0011` 一并命中（vouchers.rs:368 的注释记着这段教训）。
pub fn aux_key_contains(key: &str, want: &str) -> bool {
    if want.is_empty() {
        return true;
    }
    let parts: Vec<&str> = want.split('\u{1f}').collect();
    let have: Vec<&str> = key.split('\u{1f}').collect();
    parts.iter().all(|p| have.contains(p))
}

/// 某一维度的 aux_key 片段（**查询侧**用）：`AuxKind::Customer` → `"customer=C01"`。
///
/// 编码为空时返回空串 —— 而 `aux_key_contains` 对空 want 返回 true，
/// 所以调用方若传空编码，会**匹配到所有带任何辅助核算的分录**。
/// 那不是「没有筛选」，那是「筛出全部」，两者含义不同，别混用。
///
/// 维度是**参数**而不是写死 customer：应付挂的是 `supplier=`（AuxKind::Supplier），
/// 写死 customer 会让应付账龄一行都不出（我第一版就这么写的）。
pub fn aux_key_frag(code: &str, kind: AuxKind) -> String {
    let c = code.trim();
    if c.is_empty() {
        String::new()
    } else {
        format!("{}={}", kind.code(), c)
    }
}

/// 从整条 aux_key 里取出**指定维度**的值（**分组侧**用）。
///
/// 为什么不能直接拿整条 aux_key 做分组键：一条分录可能同时挂
/// customer / item / qty / price 多个维度，整条做键会把同一个客户
/// 拆成多组 —— 而客户视图要的是「一个客户一行」。
///
/// 找不到该维度时返回空串 —— 调用方据此判断「这条分录没挂往来单位」，
/// 别把它当成「编码为空串的客户」。
pub fn aux_key_value_of(aux_key: &str, kind: AuxKind) -> String {
    let prefix = format!("{}=", kind.code());
    for seg in aux_key.split('\u{1f}') {
        if let Some(v) = seg.strip_prefix(prefix.as_str()) {
            return v.trim().to_string();
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// 期初余额
// ---------------------------------------------------------------------------

/// 期初余额行（含辅助核算）
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BeginRow {
    pub id: i64,
    pub account_code: String,
    pub aux: AuxRef,
    pub year_begin: Money,
    pub debit_accum: Money,
    pub credit_accum: Money,
    pub qty_begin: Option<Money>,
}

pub fn list_begin(db: &Db) -> DbResult<Vec<BeginRow>> {
    let mut stmt = db.conn().prepare(
        "SELECT id, account_code, aux_json, year_begin, debit_accum, credit_accum, qty_begin
         FROM begin_balance ORDER BY account_code, aux_key",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let aux_json: String = r.get(2)?;
            Ok(BeginRow {
                id: r.get(0)?,
                account_code: r.get(1)?,
                aux: serde_json::from_str(&aux_json).unwrap_or_default(),
                year_begin: read_money(r, 3)?,
                debit_accum: read_money(r, 4)?,
                credit_accum: read_money(r, 5)?,
                qty_begin: read_money_opt(r, 6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// 辅助账行：按辅助核算维度（客户/供应商/部门/职员/项目/存货/银行）汇总
#[derive(Clone, Debug, serde::Serialize)]
pub struct AuxBalanceRow {
    pub key: String,
    pub begin: Money,
    pub debit: Money,
    pub credit: Money,
    pub end: Money,
}

/// 辅助账：某维度下各单位的期初/发生/期末（金额带符号，正=借）
pub fn aux_balance(
    db: &Db,
    kind: fincore::AuxKind,
    from: Period,
    to: Period,
    user: Option<&fincore::user::User>,
) -> DbResult<Vec<AuxBalanceRow>> {
    let mut q = BalanceQuery::range(from, to);
    if let Some(u) = user {
        q = q.with_user_scope(u);
    }
    let snap = BalanceSnapshot::load(db, &q)?;
    let mut map: BTreeMap<String, AuxBalanceRow> = BTreeMap::new();
    for row in snap.raw_rows() {
        let Some(entity) = row.aux.get(kind).filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        let e = map.entry(entity.clone()).or_insert(AuxBalanceRow {
            key: entity.clone(),
            begin: Money::ZERO,
            debit: Money::ZERO,
            credit: Money::ZERO,
            end: Money::ZERO,
        });
        e.begin += row.begin;
        e.debit += row.debit;
        e.credit += row.credit;
        e.end += row.end();
    }
    Ok(map.into_values().collect())
}

/// 存货核算 ↔ 总账 对账行（对标金蝶「存货核算-总账对账表」）
#[derive(Clone, Debug, serde::Serialize)]
pub struct ReconRow {
    pub item: String,
    /// 库存侧：库存流水金额累计（含期末结价写入的调整流水）
    pub stock_value: Money,
    /// 总账侧：存货辅助余额（期末，方向已展开）
    pub gl_value: Money,
    /// 差异 = 库存侧 - 总账侧（≠0 常见于：业务单据未出凭证 / 尚未执行期末结价）
    pub diff: Money,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct ReconReport {
    pub period: i32,
    pub stock_total: Money,
    pub gl_total: Money,
    pub diff_total: Money,
    pub rows: Vec<ReconRow>,
}

/// 存货核算 ↔ 总账 对账：两侧均累计至 period。
/// - 库存侧 = `stock_move.amount` 按存货汇总（领料等 0 价流水由**期末结价调整流水**补齐
///   → 建议结价后对账，口径完整）；
/// - 总账侧 = 存货辅助余额（kind=item，账套首期 → period，方向已展开）。
pub fn gl_reconcile(
    db: &Db,
    period: Period,
    user: Option<&fincore::user::User>,
) -> DbResult<ReconReport> {
    // 库存侧：逐行取文本在 Rust 侧用 Decimal 累加（金额是 TEXT 列，SUM/CAST 会走
    // REAL 丢精度，违反本模块「绝不用 SQL SUM 算钱」的口径）
    let mut stock: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    let mut st = db
        .conn()
        .prepare("SELECT item, amount FROM stock_move WHERE item <> '' AND period <= ?1")?;
    let mv_rows = st
        .query_map([period.ymm()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Money::parse_or_zero(&r.get::<_, String>(1)?),
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(st);
    for (item, v) in mv_rows {
        *stock.entry(item).or_insert(Money::ZERO) += v;
    }
    // 总账侧：存货辅助余额（自账套首期累计至 period）
    let start = db.options().start_period;
    let kind = fincore::AuxKind::from_code("item").expect("item 是合法辅助维度");
    let gl_rows = aux_balance(db, kind, start, period, user)?;
    let mut gl: std::collections::BTreeMap<String, Money> = std::collections::BTreeMap::new();
    for r in gl_rows {
        gl.insert(r.key, r.end);
    }
    // 合并键集（两侧任一有数即出行）
    let mut items: std::collections::BTreeSet<String> = stock.keys().cloned().collect();
    items.extend(gl.keys().cloned());
    let mut out = Vec::new();
    let (mut stock_total, mut gl_total) = (Money::ZERO, Money::ZERO);
    for item in items {
        let s = stock.get(&item).copied().unwrap_or(Money::ZERO);
        let g = gl.get(&item).copied().unwrap_or(Money::ZERO);
        stock_total += s;
        gl_total += g;
        out.push(ReconRow {
            item,
            stock_value: s,
            gl_value: g,
            diff: s - g,
        });
    }
    Ok(ReconReport {
        period: period.ymm(),
        stock_total,
        gl_total,
        diff_total: stock_total - gl_total,
        rows: out,
    })
}

/// 数量金额账行（数量核算科目，数量与金额对照）
#[derive(Clone, Debug, serde::Serialize)]
pub struct QtyBalanceRow {
    pub account_code: String,
    pub account_name: String,
    pub qty_begin: Money,
    pub qty_in: Money,
    pub qty_out: Money,
    pub qty_end: Money,
    pub amount_begin: Money,
    pub amount_debit: Money,
    pub amount_credit: Money,
    pub amount_end: Money,
}

/// 数量金额账：数量核算科目的数量与金额对照（按科目汇总，含下级）
/// 科目明细账行（链7 数字钻取）：分录逐笔 + 运行余额
#[derive(Clone, Debug, serde::Serialize)]
pub struct AcctDetailRow {
    pub date: String,
    pub voucher_id: i64,
    /// 凭证号（word + no，如 记1）
    pub no: String,
    /// 凭证摘要
    pub summary: String,
    /// 分录摘要
    pub line_memo: String,
    pub debit: Money,
    pub credit: Money,
    /// 运行余额（期初起累计，借正贷负）
    pub balance: Money,
    pub status: String,
}

/// 某科目在期初余额表里的期初合计（全部辅助核算行）
///
/// 口径与 [`BalanceSnapshot::load`] 完全一致：`年初 + 累计借 − 累计贷`。
/// 年中建账（`import_begin`）只往 `begin_balance` 写数据，`voucher_entry` 里一条都没有；
/// 期初只从分录算就会显示 0，与余额表、账簿的同一口径自相矛盾。
fn begin_balance_of(db: &Db, account: &str) -> DbResult<Money> {
    let mut st = db.conn().prepare(
        "SELECT year_begin, debit_accum, credit_accum FROM begin_balance WHERE account_code = ?1",
    )?;
    let rows = st
        .query_map([account], |r| {
            Ok((read_money(r, 0)?, read_money(r, 1)?, read_money(r, 2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(st);
    let mut sum = Money::ZERO;
    for (yb, ad, ac) in rows {
        sum += yb + ad - ac;
    }
    Ok(sum)
}

/// 科目明细账：期初（from 之前累计）+ from..to 分录逐笔运行余额。
/// 口径：**status = 'posted' 已记账**（与试算平衡 H-3 完全一致——草稿/已审未记账不进）；
/// 余额 = 借 − 贷 累计（贷余为负）。返回 (期初余额, 明细行)。
pub fn account_detail(
    db: &Db,
    account: &str,
    from: Period,
    to: Period,
) -> DbResult<(Money, Vec<AcctDetailRow>)> {
    // 期初 = 期初余额表（begin_balance）+ from 之前的已记账分录，两者与账簿同口径
    let mut begin = begin_balance_of(db, account)?;
    // 逐行文本汇总，与 batch_balance 同模式（绝不 SQL SUM）
    let mut st = db.conn().prepare(
        "SELECT e.debit, e.credit FROM voucher_entry e
         JOIN voucher v ON v.id = e.voucher_id
         WHERE e.account_code = ?1 AND v.period < ?2 AND v.status = 'posted'",
    )?;
    let begin_rows = st
        .query_map(rusqlite::params![account, from.ymm()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    begin += begin_rows
        .iter()
        .map(|(d, c)| Money::parse_or_zero(d) - Money::parse_or_zero(c))
        .sum::<Money>();
    // 明细
    let mut st = db.conn().prepare(
        "SELECT v.date, v.id, v.no, v.word, v.memo, v.status, e.summary, e.debit, e.credit
         FROM voucher_entry e
         JOIN voucher v ON v.id = e.voucher_id
         WHERE e.account_code = ?1 AND v.period BETWEEN ?2 AND ?3 AND v.status = 'posted'
         ORDER BY v.period, v.date, v.id, e.line",
    )?;
    let raw = st
        .query_map(rusqlite::params![account, from.ymm(), to.ymm()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut bal = begin;
    let mut out = Vec::with_capacity(raw.len());
    for (date, vid, no, word, vsum, status, lsum, debit, credit) in raw {
        let d = Money::parse_or_zero(&debit);
        let c = Money::parse_or_zero(&credit);
        bal += d - c;
        out.push(AcctDetailRow {
            date,
            voucher_id: vid,
            no: format!("{word}{no}"),
            summary: vsum,
            line_memo: lsum,
            debit: d,
            credit: c,
            balance: bal,
            status,
        });
    }
    Ok((begin, out))
}

pub fn qty_balance_sheet(
    db: &Db,
    from: Period,
    to: Period,
    user: Option<&fincore::user::User>,
) -> DbResult<Vec<QtyBalanceRow>> {
    let mut q = BalanceQuery::range(from, to);
    if let Some(u) = user {
        q = q.with_user_scope(u);
    }
    let snap = BalanceSnapshot::load(db, &q)?;
    let chart = crate::accounts::chart(db)?;
    let mut map: BTreeMap<String, QtyBalanceRow> = BTreeMap::new();
    // 数量挂在 filtered() 里合并（raw_rows 只有金额），因此这里走 filtered
    for row in snap.filtered(&chart, &q) {
        let Some(qty) = row.qty else { continue };
        let e = map
            .entry(row.account_code.clone())
            .or_insert(QtyBalanceRow {
                account_code: row.account_code.clone(),
                account_name: chart
                    .get(&row.account_code)
                    .map(|a| a.name.clone())
                    .unwrap_or_default(),
                qty_begin: Money::ZERO,
                qty_in: Money::ZERO,
                qty_out: Money::ZERO,
                qty_end: Money::ZERO,
                amount_begin: Money::ZERO,
                amount_debit: Money::ZERO,
                amount_credit: Money::ZERO,
                amount_end: Money::ZERO,
            });
        e.qty_begin += qty.begin;
        e.qty_in += qty.in_qty;
        e.qty_out += qty.out_qty;
        e.qty_end += qty.end();
        e.amount_begin += row.begin;
        e.amount_debit += row.debit;
        e.amount_credit += row.credit;
        e.amount_end += row.end();
    }
    Ok(map.into_values().collect())
}

/// 写入 / 更新一条期初余额（按 科目+辅助核算 唯一）
pub fn upsert_begin(db: &Db, r: &BeginRow) -> DbResult<()> {
    upsert_begin_on(db.conn(), r)
}

/// 同 `upsert_begin`，但只依赖连接，可在调用方的事务内执行（导入整体原子化）
pub fn upsert_begin_on(conn: &rusqlite::Connection, r: &BeginRow) -> DbResult<()> {
    conn.execute(
        "INSERT INTO begin_balance(account_code,aux_key,aux_json,year_begin,debit_accum,credit_accum,qty_begin)
         VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(account_code,aux_key) DO UPDATE SET
            year_begin=excluded.year_begin, debit_accum=excluded.debit_accum,
            credit_accum=excluded.credit_accum, qty_begin=excluded.qty_begin",
        rusqlite::params![
            r.account_code,
            r.aux.key(),
            serde_json::to_string(&r.aux)?,
            crate::money_param(r.year_begin),
            crate::money_param(r.debit_accum),
            crate::money_param(r.credit_accum),
            r.qty_begin.map(crate::exact_param),
        ],
    )?;
    Ok(())
}

pub fn delete_begin(db: &Db, id: i64) -> DbResult<()> {
    db.conn().execute(
        "DELETE FROM begin_balance WHERE id=?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// 同 `delete_begin`，但只在调用方事务内执行。
///
/// 为什么需要事务版本：期初保存要「先按界面移除的 id 删掉、再 upsert 其余行」
/// 并保证整体原子。原来只有非事务版，删除与写入无法绑在一个事务里 ——
/// 删成功但后续写入失败，就会留下「界面以为删了、库里没删」或反之的半截状态。
pub fn delete_begin_on(conn: &rusqlite::Connection, id: i64) -> DbResult<()> {
    conn.execute(
        "DELETE FROM begin_balance WHERE id=?1",
        rusqlite::params![id],
    )?;
    Ok(())
}

/// 期初试算：借方合计 / 贷方合计
pub fn begin_trial(db: &Db) -> DbResult<(Money, Money)> {
    let mut d = Money::ZERO;
    let mut c = Money::ZERO;
    for r in list_begin(db)? {
        let net = r.year_begin + r.debit_accum - r.credit_accum;
        if net.is_positive() {
            d += net;
        } else {
            c += net.negated();
        }
    }
    Ok((d, c))
}

// ---------------------------------------------------------------------------
// 明细账 / 总账 / 日记账
// ---------------------------------------------------------------------------

/// 明细账查询参数
#[derive(Clone, Debug)]
pub struct LedgerQuery {
    pub code: String,
    /// 是否包含下级科目
    pub include_children: bool,
    pub aux: Option<AuxRef>,
    pub from: Period,
    pub to: Period,
    /// 只显示已记账凭证
    pub posted_only: bool,
    /// 仅本人填制的凭证（own_voucher_only，由 with_user_scope 填充）
    pub prepared_by: Option<String>,
    /// 数据范围科目区间（由 with_user_scope 填充）
    pub code_from: Option<String>,
    pub code_to: Option<String>,
}

impl Default for LedgerQuery {
    /// 基准期间：仅供 `..Default::default()` 的测试/便捷构造使用，
    /// 业务调用点一律显式给 `from` / `to`。
    fn default() -> Self {
        let p = Period::new(2026, 1).expect("基准期间必然合法");
        Self {
            code: String::new(),
            include_children: false,
            aux: None,
            from: p,
            to: p,
            posted_only: true,
            prepared_by: None,
            code_from: None,
            code_to: None,
        }
    }
}

impl LedgerQuery {
    /// 套用用户完整数据范围：科目区间 + "仅本人填制的凭证"。
    /// Web/桌面调用方都应使用，避免配置了范围却看到全量的静默越权。
    pub fn with_user_scope(mut self, user: &fincore::user::User) -> Self {
        let s = &user.data_scope;
        if s.own_voucher_only {
            self.prepared_by = Some(user.username.clone());
        }
        let lo = s.account_from.trim();
        let hi = s.account_to.trim();
        if !lo.is_empty() {
            self.code_from = Some(lo.to_string());
        }
        if !hi.is_empty() {
            self.code_to = Some(hi.to_string());
        }
        self
    }

    /// 数据范围是否为空（无任何限制）
    pub fn scope_unrestricted(&self) -> bool {
        self.prepared_by.is_none() && self.code_from.is_none() && self.code_to.is_none()
    }
}

/// 明细账：逐笔滚动余额
pub fn ledger(db: &Db, chart: &Chart, q: &LedgerQuery) -> DbResult<Vec<LedgerRow>> {
    let mut bq = BalanceQuery {
        from: q.from,
        to: q.to,
        posted_only: q.posted_only,
        ..BalanceQuery::period(q.from)
    };
    bq.prepared_by = q.prepared_by.clone();
    bq.code_from = q.code_from.clone();
    bq.code_to = q.code_to.clone();
    let snap = BalanceSnapshot::load(db, &bq)?;
    let mut running = snap.for_account(&q.code, q.aux.as_ref()).begin;
    let mut qty_running = snap
        .for_account(&q.code, q.aux.as_ref())
        .qty
        .map(|q| q.begin)
        .unwrap_or(Money::ZERO);

    let pattern = if q.include_children {
        format!("{}%", crate::escape_like(&q.code))
    } else {
        crate::escape_like(&q.code)
    };
    // 行集与期初快照同一口径（H-3 定案）：仅已记账 → 只出已记账行；
    // 取消勾选 → "含未记账（排除作废）"。此前行集恒为非作废、期初却按
    // posted_only 算，勾上"只含已记账"后草稿行仍会滚进余额，两边打架。
    let status_cond = if q.posted_only {
        "v.status = 'posted'"
    } else {
        "v.status != 'void'"
    };
    let mut sql = format!(
        "SELECT v.period, v.date, v.id, v.word, v.no, e.line, e.summary, e.account_code,
                e.aux_json, e.debit, e.credit, e.qty, v.status
         FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
         WHERE e.period BETWEEN ?1 AND ?2 AND e.account_code LIKE ?3 ESCAPE '\\'
           AND {status_cond}",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
        Box::new(q.from.ymm()),
        Box::new(q.to.ymm()),
        Box::new(pattern),
    ];
    if let Some(who) = &q.prepared_by {
        params.push(Box::new(who.clone()));
        sql.push_str(&format!(" AND v.prepared_by = ?{}", params.len()));
    }
    if let Some(f) = &q.code_from {
        params.push(Box::new(f.clone()));
        sql.push_str(&format!(" AND e.account_code >= ?{}", params.len()));
    }
    if let Some(t) = &q.code_to {
        params.push(Box::new(t.clone()));
        sql.push_str(&format!(" AND e.account_code <= ?{}", params.len()));
    }
    sql.push_str(" ORDER BY v.date, v.word, v.no, e.line");
    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(refs.as_slice())?;

    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let aux_json: String = r.get(8)?;
        let aux: AuxRef = serde_json::from_str(&aux_json).unwrap_or_default();
        if let Some(ref want) = q.aux {
            let want_key = want.key();
            if !aux_key_contains(&aux.key(), &want_key) {
                continue;
            }
        }
        let debit = read_money(r, 9)?;
        let credit = read_money(r, 10)?;
        let qty = read_money_opt(r, 11)?;

        running += debit - credit;
        if let Some(qv) = qty {
            qty_running += qv;
        }

        let (dir, balance) = if running.is_negative() {
            (fincore::Direction::Credit, running.negated())
        } else {
            (fincore::Direction::Debit, running)
        };
        let date_s: String = r.get(1)?;
        let word: String = r.get(3)?;
        let no: i32 = r.get(4)?;
        let code: String = r.get(7)?;

        out.push(LedgerRow {
            period: Period::from_ymm(r.get(0)?),
            date: NaiveDate::parse_from_str(&date_s, "%Y-%m-%d")
                .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).expect("基准日期")),
            voucher_id: r.get(2)?,
            voucher_no: format!("{word}-{no:04}"),
            word,
            no,
            line: r.get(5)?,
            summary: r.get(6)?,
            account_code: code,
            aux,
            debit,
            credit,
            dir,
            balance,
            signed_balance: running,
            qty_in: qty.filter(|v| v.is_positive()),
            qty_out: qty.and_then(|v| {
                if v.is_negative() {
                    Some(v.negated())
                } else {
                    None
                }
            }),
            qty_balance: if qty.is_some() {
                Some(qty_running)
            } else {
                None
            },
        });
    }
    let _ = chart;
    Ok(out)
}

/// 总账：按期间汇总
pub fn general_ledger(db: &Db, q: &LedgerQuery) -> DbResult<Vec<GeneralLedgerRow>> {
    let mut bq = BalanceQuery {
        from: q.from,
        to: q.to,
        posted_only: q.posted_only,
        ..BalanceQuery::period(q.from)
    };
    bq.prepared_by = q.prepared_by.clone();
    bq.code_from = q.code_from.clone();
    bq.code_to = q.code_to.clone();
    let snap = BalanceSnapshot::load(db, &bq)?;
    let mut running = snap.for_account(&q.code, q.aux.as_ref()).begin;

    let pattern = if q.include_children {
        format!("{}%", crate::escape_like(&q.code))
    } else {
        crate::escape_like(&q.code)
    };
    // 按期间聚合，金额在 Rust 侧累加；行集与期初快照同口径（H-3 定案）
    let mut acc: BTreeMap<i32, (Money, Money)> = BTreeMap::new();
    let status_cond = if q.posted_only {
        "v.status = 'posted'"
    } else {
        "v.status != 'void'"
    };
    let mut sql = format!(
        "SELECT e.period, e.debit, e.credit, e.aux_json
         FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
         WHERE {status_cond} AND e.period BETWEEN ?1 AND ?2
           AND e.account_code LIKE ?3 ESCAPE '\\'",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
        Box::new(q.from.ymm()),
        Box::new(q.to.ymm()),
        Box::new(pattern),
    ];
    if let Some(who) = &q.prepared_by {
        params.push(Box::new(who.clone()));
        sql.push_str(&format!(" AND v.prepared_by = ?{}", params.len()));
    }
    if let Some(f) = &q.code_from {
        params.push(Box::new(f.clone()));
        sql.push_str(&format!(" AND e.account_code >= ?{}", params.len()));
    }
    if let Some(t) = &q.code_to {
        params.push(Box::new(t.clone()));
        sql.push_str(&format!(" AND e.account_code <= ?{}", params.len()));
    }
    let mut stmt = db.conn().prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(refs.as_slice())?;
    while let Some(r) = rows.next()? {
        let aux_json: String = r.get(3)?;
        let aux: AuxRef = serde_json::from_str(&aux_json).unwrap_or_default();
        if let Some(ref want) = q.aux {
            if !aux_key_contains(&aux.key(), &want.key()) {
                continue;
            }
        }
        let p: i32 = r.get(0)?;
        let e = acc.entry(p).or_insert((Money::ZERO, Money::ZERO));
        e.0 += read_money(r, 1)?;
        e.1 += read_money(r, 2)?;
    }

    let mut out = Vec::new();
    for (p, (d, c)) in acc {
        running += d - c;
        let (dir, balance) = if running.is_negative() {
            (fincore::Direction::Credit, running.negated())
        } else {
            (fincore::Direction::Debit, running)
        };
        out.push(GeneralLedgerRow {
            period: Period::from_ymm(p),
            summary: format!("本期发生额（{}）", Period::from_ymm(p).label()),
            debit: d,
            credit: c,
            dir,
            balance,
            signed_balance: running,
        });
    }
    Ok(out)
}

/// 现金 / 银行日记账
pub fn journal(db: &Db, chart: &Chart, q: &LedgerQuery) -> DbResult<Vec<JournalRow>> {
    let rows = ledger(db, chart, q)?;

    // 一次性取出相关凭证的对方科目
    let ids: Vec<i64> = {
        let mut v: Vec<i64> = rows.iter().map(|r| r.voucher_id).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let mut opposite: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    if !ids.is_empty() {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT voucher_id, account_code, debit, credit FROM voucher_entry
             WHERE voucher_id IN ({placeholders})"
        );
        let mut stmt = db.conn().prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|i| i as &dyn rusqlite::types::ToSql)
            .collect();
        let mut rr = stmt.query(params.as_slice())?;
        while let Some(r) = rr.next()? {
            let vid: i64 = r.get(0)?;
            let code: String = r.get(1)?;
            let d = read_money(r, 2)?;
            let c = read_money(r, 3)?;
            // 只保留与本行借贷方向相反的分录，即真正的"对方科目"
            let is_debit = d.is_positive();
            if ((is_debit && c.is_zero()) || (!is_debit && d.is_zero()))
                && !code.starts_with(&q.code)
            {
                let name = chart
                    .get(&code)
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| code.clone());
                opposite.entry(vid).or_default().push(name);
            }
        }
    }

    // 出纳签字人回填（与对方科目同批取，出纳日记账签字列展示）
    let mut cashiers: BTreeMap<i64, Option<String>> = BTreeMap::new();
    if !ids.is_empty() {
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT id, cashier FROM voucher WHERE id IN ({placeholders})");
        let mut stmt = db.conn().prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|i| i as &dyn rusqlite::types::ToSql)
            .collect();
        let mut rr = stmt.query(params.as_slice())?;
        while let Some(r) = rr.next()? {
            let vid: i64 = r.get(0)?;
            let c: Option<String> = r.get(1)?;
            cashiers.insert(vid, c);
        }
    }

    let mut out = Vec::new();
    for r in rows {
        let opp = opposite
            .get(&r.voucher_id)
            .cloned()
            .unwrap_or_default()
            .join("、");
        out.push(JournalRow {
            date: r.date,
            voucher_no: r.voucher_no,
            summary: r.summary,
            opposite_accounts: opp,
            debit: r.debit,
            credit: r.credit,
            dir: r.dir,
            balance: r.balance,
            settle_type: None,
            settle_no: None,
            cashier: cashiers.get(&r.voucher_id).cloned().flatten(),
        });
    }
    Ok(out)
}

/// 多栏账：按指定栏目科目拆借/贷方
pub fn multi_column(
    db: &Db,
    main_code: &str,
    columns: &[String],
    from: Period,
    to: Period,
) -> DbResult<Vec<(Period, String, String, Money)>> {
    let mut stmt = db.conn().prepare(
        "SELECT e.period, v.date, e.summary, e.account_code, e.debit, e.credit
         FROM voucher_entry e JOIN voucher v ON e.voucher_id=v.id
         WHERE v.status = 'posted' AND e.period BETWEEN ?1 AND ?2
           AND e.account_code IN (
               SELECT value FROM json_each(?3)
           )
         ORDER BY v.date, v.id, e.line",
    )?;
    let codes = serde_json::to_string(&columns)?;
    let mut rows = stmt.query(rusqlite::params![from.ymm(), to.ymm(), codes])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let code: String = r.get(3)?;
        // 主科目单独成列，其余栏科目按栏目归集
        if code.starts_with(main_code) {
            continue;
        }
        out.push((
            Period::from_ymm(r.get(0)?),
            r.get(2)?,
            code,
            read_money(r, 4)? + read_money(r, 5)?,
        ));
    }
    Ok(out)
}

/// 在已聚合的科目行中找 `code` 的最长真前缀。
///
/// 必须按**字符边界**切：科目编码正常是 ASCII 数字，但历史库/导入可能混进
/// 非 ASCII 编码；按字节 `&code[..len]` 会切在 UTF-8 字符中间直接 panic，
/// 而且这个 panic 发生在每次报表取数时，等于账套被一条坏数据打崩。
fn find_parent_code(code: &str, map: &BTreeMap<String, BalanceRow>) -> Option<String> {
    for (idx, _) in code.char_indices().rev() {
        if idx == 0 {
            continue;
        }
        let prefix = &code[..idx];
        if map.contains_key(prefix) {
            return Some(prefix.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use crate::vouchers;
    use fincore::{AcctCategory, AuxKind, AuxMask, Entry, Voucher};

    #[test]
    fn find_parent_code_never_splits_utf8() {
        let mut map = BTreeMap::new();
        for code in ["1", "1002", "科目"] {
            map.insert(code.to_string(), BalanceRow::default());
        }
        assert_eq!(find_parent_code("100201", &map).as_deref(), Some("1002"));
        assert_eq!(find_parent_code("科目编码", &map).as_deref(), Some("科目"));
        // 真前缀不包含自身
        assert_eq!(find_parent_code("科目", &map), None);
    }

    /// 按科目表要求补齐辅助核算与数量，让测试用的凭证符合落库校验。
    ///
    /// 现在 `vouchers::save` 会做完整校验（借贷平衡、末级科目、辅助必录、数量必录），
    /// 测试里手写的简写分录必须先补齐这些字段才能存进去。
    fn fill_required(db: &Db, e: &mut Entry) {
        let chart = crate::accounts::chart(db).unwrap();
        let Some(a) = chart.get(&e.account_code).cloned() else {
            return;
        };
        for k in a.aux.list() {
            if e.aux.get(k).is_some() {
                continue;
            }
            let v = match k {
                AuxKind::Bank => "B01",
                AuxKind::Customer => "C01",
                AuxKind::Supplier => "S01",
                AuxKind::Item => "I01",
                AuxKind::Dept => "D01",
                AuxKind::Employee => "E01",
                AuxKind::Project => "P01",
                AuxKind::CashFlow => continue,
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

    /// 保存一张草稿凭证（不记账）——H-3 口径测试需要区分草稿与已记账
    fn save_draft(db: &Db, period: Period, day: u32, entries: Vec<(&str, &str, &str)>) -> i64 {
        let d = NaiveDate::from_ymd_opt(period.year(), period.month(), day).unwrap();
        let mut v = Voucher::new(
            period,
            d,
            "记",
            vouchers::next_no(db, period, "记").unwrap(),
        );
        v.prepared_by = "张三".to_string();
        let mut i = 0;
        for (code, side, amt) in entries {
            i += 1;
            let m = Money::parse(amt).unwrap();
            let mut e = Entry::new(i, code, "测试");
            if side == "借" {
                e.debit = m;
            } else {
                e.credit = m;
            }
            fill_required(db, &mut e);
            v.push_entry(e);
        }
        vouchers::save(db, &mut v).unwrap()
    }

    fn post_voucher(db: &Db, period: Period, day: u32, entries: Vec<(&str, &str, &str)>) -> i64 {
        let id = save_draft(db, period, day, entries);
        vouchers::post(db, id, "王五").unwrap();
        id
    }

    /// H-3 定案：余额默认只统计已记账；显式 posted_only=false 才按
    /// "含未记账（排除作废）"；作废凭证在两种口径下都不入余额。
    #[test]
    fn balance_scope_h3_default_posted_only() {
        let db = mem();
        let p1 = Period::new(2026, 1).unwrap();
        // 草稿 1000（不记账）
        let draft_id = save_draft(
            &db,
            p1,
            5,
            vec![("1001", "借", "1000"), ("100201", "贷", "1000")],
        );
        // 已记账 500
        post_voucher(
            &db,
            p1,
            10,
            vec![("1001", "借", "500"), ("2001", "贷", "500")],
        );
        // 作废只能针对未记账凭证（已记账须先反记账），故用草稿演示作废
        let void_id = save_draft(
            &db,
            p1,
            15,
            vec![("1001", "借", "300"), ("100201", "贷", "300")],
        );
        vouchers::set_void(&db, void_id, true, "王五").unwrap();

        // 默认口径：只有已记账的 500
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p1)).unwrap();
        assert_eq!(
            snap.for_account("1001", None).debit,
            Money::parse("500").unwrap(),
            "H-3 默认口径应只含已记账，草稿与作废都不入余额"
        );
        // 显式含未记账：草稿进（1500），作废仍排除
        let snap2 =
            BalanceSnapshot::load(&db, &BalanceQuery::period(p1).with_posted_only(false)).unwrap();
        assert_eq!(
            snap2.for_account("1001", None).debit,
            Money::parse("1500").unwrap(),
            "含未记账口径应含草稿、排除作废"
        );
        // 草稿记账后进入默认口径
        vouchers::post(&db, draft_id, "王五").unwrap();
        let snap3 = BalanceSnapshot::load(&db, &BalanceQuery::period(p1)).unwrap();
        assert_eq!(
            snap3.for_account("1001", None).debit,
            Money::parse("1500").unwrap(),
            "草稿记账后应进入默认余额"
        );
    }

    /// H-3：账簿行集与期初/余额快照必须同口径——勾"仅已记账"不出草稿行，
    /// 滚动余额末行 = 快照期末。此前行集恒含非作废、期初却按 posted 算，
    /// 勾选后草稿行会把滚动余额滚出快照之外（口径打架的回归防护）。
    #[test]
    fn ledger_rows_follow_posted_flag() {
        let db = mem();
        let p1 = Period::new(2026, 1).unwrap();
        save_draft(
            &db,
            p1,
            5,
            vec![("1001", "借", "1000"), ("100201", "贷", "1000")],
        );
        post_voucher(
            &db,
            p1,
            10,
            vec![("1001", "借", "500"), ("2001", "贷", "500")],
        );
        let chart = crate::accounts::chart(&db).unwrap();
        let mk = |posted: bool| LedgerQuery {
            code: "1001".into(),
            include_children: false,
            aux: None,
            from: p1,
            to: p1,
            posted_only: posted,
            prepared_by: None,
            code_from: None,
            code_to: None,
        };

        // 仅已记账：只出行账，滚动余额与默认快照期末一致
        let rows = ledger(&db, &chart, &mk(true)).unwrap();
        assert_eq!(rows.len(), 1, "仅已记账口径不应出现草稿行");
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p1)).unwrap();
        assert_eq!(
            rows.last().unwrap().signed_balance,
            Money::parse("500").unwrap()
        );
        assert_eq!(
            rows.last().unwrap().signed_balance,
            snap.for_account("1001", None).end(),
            "滚动余额末行必须等于快照期末"
        );

        // 含未记账：两行都在，滚动余额与 posted_only=false 快照一致
        let rows2 = ledger(&db, &chart, &mk(false)).unwrap();
        assert_eq!(rows2.len(), 2, "含未记账口径应出现草稿行");
        let snap2 =
            BalanceSnapshot::load(&db, &BalanceQuery::period(p1).with_posted_only(false)).unwrap();
        assert_eq!(
            rows2.last().unwrap().signed_balance,
            Money::parse("1500").unwrap()
        );
        assert_eq!(
            rows2.last().unwrap().signed_balance,
            snap2.for_account("1001", None).end(),
            "含未记账口径下行集与快照也必须一致"
        );
    }

    #[test]
    fn snapshot_accumulates() {
        let db = mem();
        let p1 = Period::new(2026, 1).unwrap();
        let p2 = Period::new(2026, 2).unwrap();
        // 1 月：借 1001 1000 / 贷 100201 1000
        post_voucher(
            &db,
            p1,
            5,
            vec![("1001", "借", "1000"), ("100201", "贷", "1000")],
        );
        // 2 月：借 1001 500 / 贷 6001 500
        post_voucher(
            &db,
            p2,
            6,
            vec![("1001", "借", "500"), ("600101", "贷", "500")],
        );

        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p2)).unwrap();
        let cash = snap.for_account("1001", None);
        assert_eq!(
            cash.begin,
            Money::parse("1000").unwrap(),
            "2 月期初应是 1 月期末"
        );
        assert_eq!(cash.debit, Money::parse("500").unwrap());
        assert_eq!(cash.end(), Money::parse("1500").unwrap());
        assert_eq!(cash.ytd_debit, Money::parse("1500").unwrap());

        // 银行存款：1 月贷 1000，期末为贷方 1000
        let bank = snap.for_account("100201", None);
        assert_eq!(bank.end(), Money::parse("-1000").unwrap());
    }

    #[test]
    fn trial_balance_balances() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        post_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "1000"), ("100201", "贷", "1000")],
        );
        post_voucher(
            &db,
            p,
            6,
            vec![("660101", "借", "300"), ("1001", "贷", "300")],
        );

        let chart = crate::accounts::chart(&db).unwrap();
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        let t = snap.trial_balance(&chart);
        assert!(t.is_balanced(), "{:?}", t.problems());
        assert_eq!(t.period_debit, Money::parse("1300").unwrap());
        assert_eq!(t.period_credit, Money::parse("1300").unwrap());
    }

    #[test]
    fn trial_balance_detects_imbalance() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // save 会拒绝借贷不平衡的凭证，这里直接写库模拟"历史脏数据"，
        // 用来验证试算平衡确实能把它揪出来。
        let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        db.conn()
            .execute(
                "INSERT INTO voucher(period,date,word,no,status,prepared_by,source)
                 VALUES(?1,?2,'记',1,'posted','张三','manual')",
                rusqlite::params![p.ymm(), d.format("%Y-%m-%d").to_string()],
            )
            .unwrap();
        let vid: i64 = db.conn().last_insert_rowid();
        for (line, code, side, amt) in [(1i32, "1001", "借", "100"), (2i32, "600101", "贷", "90")]
        {
            let (dr, cr) = if side == "借" {
                (amt, "0")
            } else {
                ("0", amt)
            };
            db.conn()
                .execute(
                    "INSERT INTO voucher_entry(voucher_id,period,line,summary,account_code,
                            aux_key,aux_json,debit,credit)
                     VALUES(?1,?2,?3,'不平',?4,'','{}',?5,?6)",
                    rusqlite::params![vid, p.ymm(), line, code, dr, cr],
                )
                .unwrap();
        }

        let chart = crate::accounts::chart(&db).unwrap();
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        let t = snap.trial_balance(&chart);
        assert!(!t.is_balanced());
    }

    #[test]
    fn ledger_rolls_balance() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        post_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "1000"), ("100201", "贷", "1000")],
        );
        post_voucher(
            &db,
            p,
            10,
            vec![("1001", "借", "500"), ("600101", "贷", "500")],
        );
        post_voucher(
            &db,
            p,
            20,
            vec![("660101", "借", "200"), ("1001", "贷", "200")],
        );

        let chart = crate::accounts::chart(&db).unwrap();
        let rows = ledger(
            &db,
            &chart,
            &LedgerQuery {
                code: "1001".into(),
                include_children: false,
                aux: None,
                from: p,
                to: p,
                posted_only: true,
                prepared_by: None,
                code_from: None,
                code_to: None,
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].balance, Money::parse("1000").unwrap());
        assert_eq!(rows[1].balance, Money::parse("1500").unwrap());
        assert_eq!(rows[2].balance, Money::parse("1300").unwrap());
        assert_eq!(rows[2].dir, fincore::Direction::Debit);
    }

    #[test]
    fn ledger_with_children() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        post_voucher(
            &db,
            p,
            5,
            vec![("100201", "借", "800"), ("1001", "贷", "800")],
        );
        post_voucher(
            &db,
            p,
            6,
            vec![("100202", "借", "200"), ("1001", "贷", "200")],
        );

        let chart = crate::accounts::chart(&db).unwrap();
        // 父科目 1002 应汇总两个下级
        let rows = ledger(
            &db,
            &chart,
            &LedgerQuery {
                code: "1002".into(),
                include_children: true,
                aux: None,
                from: p,
                to: p,
                posted_only: true,
                prepared_by: None,
                code_from: None,
                code_to: None,
            },
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].balance, Money::parse("1000").unwrap());
    }

    #[test]
    fn aux_filtered_balance() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "记", 1);
        v.push_entry(Entry {
            debit: Money::parse("600").unwrap(),
            aux: AuxRef {
                dept: Some("D01".into()),
                ..Default::default()
            },
            ..Entry::new(1, "660201", "办公费")
        });
        v.push_entry(Entry {
            debit: Money::parse("400").unwrap(),
            aux: AuxRef {
                dept: Some("D02".into()),
                ..Default::default()
            },
            ..Entry::new(2, "660201", "办公费")
        });
        v.push_entry(Entry {
            credit: Money::parse("1000").unwrap(),
            ..Entry::new(3, "1001", "办公费")
        });
        let id = vouchers::save(&db, &mut v).unwrap();
        vouchers::post(&db, id, "王五").unwrap();

        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        let all = snap.for_account("660201", None);
        assert_eq!(all.debit, Money::parse("1000").unwrap());
        let d01 = snap.for_account(
            "660201",
            Some(&AuxRef {
                dept: Some("D01".into()),
                ..Default::default()
            }),
        );
        assert_eq!(d01.debit, Money::parse("600").unwrap());
        assert_eq!(snap.aux_breakdown("660201").len(), 2);
    }

    #[test]
    fn begin_balance_used() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        upsert_begin(
            &db,
            &BeginRow {
                id: 0,
                account_code: "1001".into(),
                aux: AuxRef::default(),
                year_begin: Money::parse("5000").unwrap(),
                debit_accum: Money::ZERO,
                credit_accum: Money::parse("1000").unwrap(),
                qty_begin: None,
            },
        )
        .unwrap();
        post_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "200"), ("600101", "贷", "200")],
        );

        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        let cash = snap.for_account("1001", None);
        // 5000 - 1000 + 200 = 4200
        assert_eq!(cash.end(), Money::parse("4200").unwrap());
        assert_eq!(cash.begin, Money::parse("4000").unwrap());
        assert_eq!(
            begin_trial(&db).unwrap(),
            (Money::parse("4000").unwrap(), Money::ZERO)
        );
    }

    /// H5：跨年后本年累计必须从 0 重新起算，不能带上建账前的累计发生额。
    #[test]
    fn ytd_resets_across_year_boundary() {
        let mut opts = crate::tests::test_opts();
        opts.start_period = Period::new(2025, 7).unwrap();
        let db = Db::in_memory(&opts).unwrap();
        upsert_begin(
            &db,
            &BeginRow {
                id: 0,
                account_code: "1001".into(),
                aux: AuxRef::default(),
                year_begin: Money::parse("1000").unwrap(),
                debit_accum: Money::parse("200").unwrap(),
                credit_accum: Money::parse("50").unwrap(),
                qty_begin: None,
            },
        )
        .unwrap();
        // 2025 年 12 月借 100；2026 年 1 月借 300
        post_voucher(
            &db,
            Period::new(2025, 12).unwrap(),
            5,
            vec![("1001", "借", "100"), ("600101", "贷", "100")],
        );
        post_voucher(
            &db,
            Period::new(2026, 1).unwrap(),
            5,
            vec![("1001", "借", "300"), ("600101", "贷", "300")],
        );

        let p25 = Period::new(2025, 12).unwrap();
        let s25 = BalanceSnapshot::load(&db, &BalanceQuery::period(p25)).unwrap();
        let r25 = s25.for_account("1001", None);
        // 建账前累计 200 + 2025-12 发生 100 = 300
        assert_eq!(r25.ytd_debit, Money::parse("300").unwrap());
        assert_eq!(r25.begin, Money::parse("1150").unwrap());

        let p26 = Period::new(2026, 1).unwrap();
        let s26 = BalanceSnapshot::load(&db, &BalanceQuery::period(p26)).unwrap();
        let r26 = s26.for_account("1001", None);
        // 2026 年只算 1 月的 300，不能把 2025 的 200+100 再带上
        assert_eq!(r26.ytd_debit, Money::parse("300").unwrap());
        assert_eq!(r26.ytd_credit, Money::ZERO);
        // 期初 = 1000 + 200 - 50 + 100 = 1250；期末 = 1550
        assert_eq!(r26.begin, Money::parse("1250").unwrap());
        assert_eq!(r26.end(), Money::parse("1550").unwrap());
    }

    #[test]
    fn balance_source_impl() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        post_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "1000"), ("600101", "贷", "1000")],
        );
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        let src: &dyn BalanceSource = &snap;
        assert_eq!(
            src.balance_of("1001", None).end(),
            Money::parse("1000").unwrap()
        );
        assert_eq!(
            src.balance_of("600101", None).end(),
            Money::parse("-1000").unwrap()
        );
        assert_eq!(src.balance_of("9999", None).end(), Money::ZERO);
    }

    #[test]
    fn qty_tracked() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "记", 1);
        let mut e1 = Entry {
            debit: Money::parse("1000").unwrap(),
            qty: Some(Money::parse("10").unwrap()),
            price: Some(Money::parse("100").unwrap()),
            ..Entry::new(1, "140501", "购入")
        };
        fill_required(&db, &mut e1);
        let mut e2 = Entry {
            credit: Money::parse("1000").unwrap(),
            ..Entry::new(2, "1001", "购入")
        };
        fill_required(&db, &mut e2);
        v.push_entry(e1);
        v.push_entry(e2);
        let id = vouchers::save(&db, &mut v).unwrap();
        vouchers::post(&db, id, "王五").unwrap();

        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        let r = snap.for_account("140501", None);
        assert_eq!(r.qty.unwrap().end(), Money::parse("10").unwrap());
    }

    #[test]
    fn account_table_shape() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        post_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "1000"), ("600101", "贷", "1000")],
        );
        let chart = crate::accounts::chart(&db).unwrap();
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        let table = snap.account_table(
            &chart,
            &BalanceQuery {
                non_zero_only: true,
                ..BalanceQuery::period(p)
            },
        );
        // 1001 与 6001 应有数据，1002 无数据被过滤
        assert!(table.iter().any(|r| r.account_code == "1001"));
        assert!(table.iter().any(|r| r.account_code == "600101"));
        assert!(!table.iter().any(|r| r.account_code == "140501"));
    }

    #[test]
    fn profit_loss_rows_only_pl() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        post_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "1000"), ("600101", "贷", "1000")],
        );
        post_voucher(
            &db,
            p,
            6,
            vec![("660101", "借", "300"), ("1001", "贷", "300")],
        );
        let chart = crate::accounts::chart(&db).unwrap();
        let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
        let pl = snap.profit_loss_rows(&chart);
        let codes: Vec<&str> = pl.iter().map(|r| r.account_code.as_str()).collect();
        assert!(codes.contains(&"600101"));
        assert!(codes.contains(&"660101"));
        assert!(!codes.contains(&"1001"));
        let _ = AcctCategory::Asset;
        let _ = AuxMask::NONE.with(AuxKind::Dept);
    }

    #[test]
    fn own_voucher_only_scope_filters_balance() {
        use fincore::user::{Role, User};
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        let _id1 = post_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "1000"), ("100201", "贷", "1000")],
        ); // 张三
        let id2 = post_voucher(
            &db,
            p,
            6,
            vec![("1001", "借", "300"), ("600101", "贷", "300")],
        );
        db.conn()
            .execute(
                "UPDATE voucher SET prepared_by='李四' WHERE id=?1",
                rusqlite::params![id2],
            )
            .unwrap();

        // 仅本人过滤（机制保留；默认已放开为多岗位协作，这里显式开启来验证过滤本身）
        let mut u = User::new("张三", "张三", Role::Accountant);
        u.data_scope.own_voucher_only = true;
        let bq = BalanceQuery::period(p).with_user_scope(&u);
        let snap = BalanceSnapshot::load(&db, &bq).unwrap();
        let chart = crate::accounts::chart(&db).unwrap();
        let t = snap.trial_balance(&chart);
        assert_eq!(
            t.period_debit,
            Money::parse("1000").unwrap(),
            "只应统计张三的凭证"
        );
        assert_eq!(t.period_credit, Money::parse("1000").unwrap());

        // 管理员视角不受限
        let admin = User::new("admin", "管理员", Role::Admin);
        let bq2 = BalanceQuery::period(p).with_user_scope(&admin);
        let snap2 = BalanceSnapshot::load(&db, &bq2).unwrap();
        let t2 = snap2.trial_balance(&chart);
        assert_eq!(t2.period_debit, Money::parse("1300").unwrap());
    }

    #[test]
    fn data_scope_merges_account_range() {
        use fincore::user::DataScope;

        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        post_voucher(
            &db,
            p,
            5,
            vec![("1001", "借", "1000"), ("600101", "贷", "1000")],
        );
        post_voucher(
            &db,
            p,
            6,
            vec![("100201", "借", "500"), ("1001", "贷", "500")],
        );

        let chart = crate::accounts::chart(&db).unwrap();

        // 无范围：能看到全部
        let q0 = BalanceQuery::period(p).with_data_scope(&DataScope::default());
        let rows = BalanceSnapshot::load(&db, &q0)
            .unwrap()
            .account_table(&chart, &q0);
        let codes: Vec<&str> = rows.iter().map(|r| r.account_code.as_str()).collect();
        assert!(codes.contains(&"1001"));
        assert!(codes.contains(&"600101"));

        // 范围只允许 1001~1002：不含 6001
        let scope = DataScope {
            account_from: "1001".into(),
            account_to: "1002".into(),
            ..Default::default()
        };
        let q1 = BalanceQuery::period(p).with_data_scope(&scope);
        let rows = BalanceSnapshot::load(&db, &q1)
            .unwrap()
            .account_table(&chart, &q1);
        let codes: Vec<&str> = rows.iter().map(|r| r.account_code.as_str()).collect();
        assert!(codes.contains(&"1001"), "1001 应在范围内：{codes:?}");
        assert!(!codes.contains(&"600101"), "600101 不应在范围内：{codes:?}");

        // 与手动下界取交集：手动 1001→ 与范围 1001→ 一致
        let q2 = BalanceQuery::period(p)
            .with_code_range(Some("1001".into()), None)
            .with_data_scope(&scope);
        let rows = BalanceSnapshot::load(&db, &q2)
            .unwrap()
            .account_table(&chart, &q2);
        assert!(rows.iter().any(|r| r.account_code == "1001"));
        assert!(!rows.iter().any(|r| r.account_code == "600101"));
    }
}

#[cfg(test)]
mod drill_tests {
    use super::*;
    use crate::tests::mem;
    use fincore::account::AuxKind;
    use fincore::voucher::Entry;

    #[test]
    fn account_detail_smoke() {
        let db = mem();
        db.conn()
            .execute(
                "INSERT INTO voucher(period,date,word,no,status,prepared_by,created_at,updated_at)
                 VALUES(202601,'2026-01-15','记',1,'posted','u','x','x')",
                [],
            )
            .unwrap();
        let vid = db.conn().last_insert_rowid();
        db.conn()
            .execute(
                "INSERT INTO voucher_entry(voucher_id,period,line,summary,account_code,debit,credit)
                 VALUES(?1,202601,1,'t','1001','100','0')",
                [vid],
            )
            .unwrap();
        let (begin, rows) = account_detail(
            &db,
            "1001",
            Period::from_ymm(202601),
            Period::from_ymm(202601),
        )
        .unwrap();
        assert_eq!(begin, Money::ZERO);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].balance, Money::parse("100").unwrap());
    }

    /// 回归：`account_detail` 的期初必须与 `ledger()` 同源，都含期初余额表。
    ///
    /// 年中建账时 `import_begin` 只写 `begin_balance`、`voucher_entry` 一条都没有，
    /// 期初只从分录算就会显示 0：余额表有金额、钻取进去期初却是 0，两边打架。
    #[test]
    fn account_detail_begin_matches_ledger() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        // 年中建账：只有期初余额表，没有历史凭证
        crate::imports::import_begin(
            &db,
            "\u{feff}科目,方向,金额\n100201,借,50000\n",
            "u1",
            &std::collections::HashMap::new(),
            crate::imports::ImportTemplate::Generic,
        )
        .unwrap();
        // 当期再记一张已记账凭证：借管理费用 / 贷银行
        let mut v = fincore::Voucher::new(p, p.first_day(), "记", 1);
        v.push_entry(Entry {
            debit: Money::parse("1000").unwrap(),
            ..Entry::new(1, "660201", "付办公费")
        });
        let mut e2 = Entry::new(2, "100201", "付办公费");
        e2.aux.set(AuxKind::Bank, Some("B01".into()));
        e2.credit = Money::parse("1000").unwrap();
        v.push_entry(e2);
        let vid = crate::vouchers::save(&db, &mut v).unwrap();
        crate::vouchers::post(&db, vid, "u").unwrap();

        let chart = accounts::chart(&db).unwrap();
        let lq = LedgerQuery {
            code: "100201".into(),
            from: p,
            to: p,
            posted_only: true,
            ..LedgerQuery::default()
        };
        let led = ledger(&db, &chart, &lq).unwrap();
        let (begin, rows) = account_detail(&db, "100201", p, p).unwrap();
        assert_eq!(
            begin,
            Money::parse("50000").unwrap(),
            "期初必须取期初余额表（回归前恒为 0）"
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].balance, Money::parse("49000").unwrap());
        // 与账簿末行运行余额必须一致
        assert_eq!(
            rows.last().unwrap().balance,
            led.last().unwrap().balance,
            "钻取运行余额必须与账簿一致（回归前差 50000）"
        );
    }

    /// 回归：金额不得用 `SUM(CAST(amount AS REAL))` 汇总。
    /// REAL 是二进制浮点，`0.1 + 0.2 != 0.3`，而 4 位格式化还会把 TEXT 里
    /// 更长的小数位直接截断；逐行在 Rust 侧用 Decimal 累加才可控。
    #[test]
    fn gl_reconcile_sums_stock_amount_exactly() {
        let db = mem();
        let p = Period::new(2026, 1).unwrap();
        crate::auxs::insert(
            &db,
            &fincore::auxiliary::AuxEntity::new(AuxKind::Item, "I01", "存货A"),
        )
        .unwrap();
        // 三笔 1.00005 的入库流水：Decimal 合计 3.00015
        // （直接写 SQL：金额列是 TEXT，仓储层的 money_param 会按 2 位显示精度存，
        //   这里要验证的是"聚合"这一步对长小数的处理）
        for _ in 0..3 {
            db.conn()
                .execute(
                    "INSERT INTO stock_move(period, biz_date, kind, item, qty, price, amount, memo)
                     VALUES(?1, ?2, 'purchase', 'I01', '1', '1.00005', '1.00005', '')",
                    rusqlite::params![p.ymm(), p.first_day().format("%Y-%m-%d").to_string()],
                )
                .unwrap();
        }
        let r = gl_reconcile(&db, p, None).unwrap();
        let row = r.rows.iter().find(|x| x.item == "I01").unwrap();
        assert_eq!(
            row.stock_value,
            Money::parse("3.00015").unwrap(),
            "库存侧金额必须精确求和（回归前 REAL + 4 位格式化会截成 3.0001）"
        );
        assert_eq!(r.stock_total, Money::parse("3.00015").unwrap());
    }
}
