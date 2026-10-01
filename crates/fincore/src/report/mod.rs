//! 会计报表引擎
//!
//! 报表 = 报表项目定义（模板） + 取数公式 + 余额数据源。
//! 公式支持三类原子：科目取数、引用其他行、常量。模板可序列化存库，用户改完即时生效。

pub mod balance_sheet;
pub mod cashflow;
pub mod equity;
pub mod income;

use serde::{Deserialize, Serialize};

use crate::account::Direction;
use crate::money::Money;
use crate::voucher::AuxRef;

/// 取数口径
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmountKind {
    /// 期初余额（带符号）
    Begin,
    /// 期末余额（带符号）
    End,
    /// 本期借方发生额
    PeriodDebit,
    /// 本期贷方发生额
    PeriodCredit,
    /// 本年累计借方
    YearDebit,
    /// 本年累计贷方
    YearCredit,
    /// 期末数量
    EndQty,
}

impl AmountKind {
    pub fn label(self) -> &'static str {
        match self {
            AmountKind::Begin => "期初余额",
            AmountKind::End => "期末余额",
            AmountKind::PeriodDebit => "本期借方",
            AmountKind::PeriodCredit => "本期贷方",
            AmountKind::YearDebit => "累计借方",
            AmountKind::YearCredit => "累计贷方",
            AmountKind::EndQty => "期末数量",
        }
    }
}

/// 余额方向过滤，用于资产负债表的重分类（如应收账款借方余额 + 预收账款借方余额）
///
/// 重要：过滤**只判断方向、不改变符号**。全引擎统一"正=借、负=贷"，
/// 一旦这里把贷方余额翻成正数，它再与裸取数（`Both`）相加做合计时符号就打架了。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalancePick {
    /// 取带符号原值
    Both,
    /// 只取借方余额（贷方余额归零），保持正数
    DebitOnly,
    /// 只取贷方余额（借方余额归零），保持负数
    CreditOnly,
}

/// 公式原子
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Term {
    /// 科目取数（编码可填父级，自动含下级）
    Acct {
        accounts: Vec<String>,
        kind: AmountKind,
        pick: BalancePick,
        sign: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aux: Option<AuxRef>,
    },
    /// 引用本表其他行（0 基索引）
    Line { index: usize, sign: i32 },
    /// 常量
    Const(Money),
    /// 尚未结转的损益净额（收入与费用类科目余额之和，正=净亏损，负=净盈利）
    ///
    /// 期末未结转损益时，本期盈亏还挂在损益类科目上，资产负债表要靠这一项
    /// 把盈亏并入"未分配利润"，否则表会不平。结转后损益科目清零，本项自然归零，
    /// 盈亏改由「本年利润 / 利润分配」科目承接，公式无需改变。
    ProfitLossNet { kind: AmountKind, sign: i32 },
}

impl Term {
    pub fn acct(accounts: &[&str], kind: AmountKind) -> Self {
        Term::Acct {
            accounts: accounts.iter().map(|s| s.to_string()).collect(),
            kind,
            pick: BalancePick::Both,
            sign: 1,
            aux: None,
        }
    }
    pub fn debit_only(mut self) -> Self {
        if let Term::Acct { pick, .. } = &mut self {
            *pick = BalancePick::DebitOnly;
        }
        self
    }
    pub fn credit_only(mut self) -> Self {
        if let Term::Acct { pick, .. } = &mut self {
            *pick = BalancePick::CreditOnly;
        }
        self
    }
    /// 未结转损益净额
    pub fn profit_loss_net(kind: AmountKind) -> Self {
        Term::ProfitLossNet { kind, sign: 1 }
    }
    pub fn neg(mut self) -> Self {
        match &mut self {
            Term::Acct { sign, .. } => *sign = -*sign,
            Term::Line { sign, .. } => *sign = -*sign,
            Term::ProfitLossNet { sign, .. } => *sign = -*sign,
            Term::Const(m) => *m = m.negated(),
        }
        self
    }
    pub fn line(index: usize) -> Self {
        Term::Line { index, sign: 1 }
    }
}

/// 报表行样式
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineStyle {
    Normal,
    /// 小计
    Subtotal,
    /// 合计（加粗）
    Total,
    /// 标题行（无金额）
    Header,
    /// 空行
    Blank,
}

/// 报表行定义
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ReportLine {
    /// 行次，如 "1"
    pub no: String,
    pub name: String,
    /// 缩进层级
    pub indent: u8,
    pub terms: Vec<Term>,
    pub style: LineStyle,
    /// 该行金额的显示方向提示（用于红字显示亏损等）
    #[serde(default)]
    pub show_negative_red: bool,
}

impl ReportLine {
    pub fn normal(no: &str, name: &str, indent: u8, terms: Vec<Term>) -> Self {
        Self {
            no: no.into(),
            name: name.into(),
            indent,
            terms,
            style: LineStyle::Normal,
            show_negative_red: true,
        }
    }
    pub fn total(no: &str, name: &str, indent: u8, terms: Vec<Term>) -> Self {
        Self {
            style: LineStyle::Total,
            ..ReportLine::normal(no, name, indent, terms)
        }
    }
    pub fn subtotal(no: &str, name: &str, indent: u8, terms: Vec<Term>) -> Self {
        Self {
            style: LineStyle::Subtotal,
            ..ReportLine::normal(no, name, indent, terms)
        }
    }
    pub fn header(name: &str) -> Self {
        Self {
            no: String::new(),
            name: name.into(),
            indent: 0,
            terms: vec![],
            style: LineStyle::Header,
            show_negative_red: false,
        }
    }
    pub fn blank() -> Self {
        Self {
            no: String::new(),
            name: String::new(),
            indent: 0,
            terms: vec![],
            style: LineStyle::Blank,
            show_negative_red: false,
        }
    }
}

/// 报表模板
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ReportDef {
    pub key: String,
    pub name: String,
    /// 列标题，第一列固定为项目名
    pub columns: Vec<String>,
    pub lines: Vec<ReportLine>,
}

impl ReportDef {
    /// 每个金额列对应一套取数口径（如资产负债表的"期末余额"和"年初余额"）
    pub fn columns_count(&self) -> usize {
        self.columns.len()
    }
}

/// 余额数据源：由持久化层实现
pub trait BalanceSource {
    /// 取某科目（含所有下级）的汇总余额；无数据返回零行
    fn balance_of(&self, code: &str, aux: Option<&AuxRef>) -> crate::balance::BalanceRow;

    /// 尚未结转的损益净额：收入类与费用类科目余额之和，带符号（正=净亏损，负=净盈利）
    ///
    /// 默认返回 0，适用于不含损益科目或已全部结转的数据源。
    /// 需要按科目类别汇总的实现（如 `findb::balances::BalanceSnapshot`）应覆盖它，
    /// 否则资产负债表的"未分配利润"在结转前会漏掉本期盈亏，导致报表不平。
    fn profit_loss_net(&self, _kind: AmountKind) -> Money {
        Money::ZERO
    }
}

/// 报表计算出的行
#[derive(Clone, PartialEq, Debug)]
pub struct ReportRow {
    pub no: String,
    pub name: String,
    pub indent: u8,
    pub style: LineStyle,
    /// 各金额列的数值
    pub values: Vec<Money>,
    pub show_negative_red: bool,
}

impl ReportRow {
    /// 表格单元格文本：零值显示为空白（财务报表惯例）
    pub fn cell(&self, col: usize) -> String {
        let v = self.values.get(col).copied().unwrap_or(Money::ZERO);
        if v.is_zero() && self.style == LineStyle::Normal {
            String::new()
        } else {
            v.fmt_money()
        }
    }
}

/// 计算出的报表
#[derive(Clone, Debug)]
pub struct ReportTable {
    pub title: String,
    pub subtitle: String,
    pub company: String,
    pub columns: Vec<String>,
    pub rows: Vec<ReportRow>,
}

/// 取单个 Term 的值
pub fn eval_term(
    term: &Term,
    src: &dyn BalanceSource,
    kind_map: &dyn Fn(AmountKind) -> AmountKind,
    cache: &mut Vec<Option<Money>>,
    lines: &[ReportLine],
    stack: &mut Vec<usize>,
) -> Money {
    match term {
        Term::Const(m) => *m,
        Term::Acct {
            accounts,
            kind,
            pick,
            sign,
            aux,
        } => {
            let target = kind_map(*kind);
            let mut sum = Money::ZERO;
            for code in accounts {
                let row = src.balance_of(code, aux.as_ref());
                let raw = match target {
                    AmountKind::Begin => row.begin,
                    AmountKind::End => row.end(),
                    AmountKind::PeriodDebit => row.debit,
                    AmountKind::PeriodCredit => row.credit,
                    AmountKind::YearDebit => row.ytd_debit,
                    AmountKind::YearCredit => row.ytd_credit,
                    AmountKind::EndQty => row.qty.map(|q| q.end()).unwrap_or(Money::ZERO),
                };
                let v = match pick {
                    BalancePick::Both => raw,
                    BalancePick::DebitOnly => {
                        if raw.is_positive() {
                            raw
                        } else {
                            Money::ZERO
                        }
                    }
                    BalancePick::CreditOnly => {
                        if raw.is_negative() {
                            raw
                        } else {
                            Money::ZERO
                        }
                    }
                };
                sum += v;
            }
            if *sign < 0 {
                sum.negated()
            } else {
                sum
            }
        }
        Term::ProfitLossNet { kind, sign } => {
            let v = src.profit_loss_net(kind_map(*kind));
            if *sign < 0 {
                v.negated()
            } else {
                v
            }
        }
        Term::Line { index, sign } => {
            let v = eval_line(*index, src, kind_map, cache, lines, stack);
            if *sign < 0 {
                v.negated()
            } else {
                v
            }
        }
    }
}

/// 计算某行的值（对每列取同一套公式，列差异通过 kind_map 切换）
pub fn eval_line(
    index: usize,
    src: &dyn BalanceSource,
    kind_map: &dyn Fn(AmountKind) -> AmountKind,
    cache: &mut Vec<Option<Money>>,
    lines: &[ReportLine],
    stack: &mut Vec<usize>,
) -> Money {
    if let Some(v) = cache.get(index).copied().flatten() {
        return v;
    }
    if stack.contains(&index) {
        return Money::ZERO; // 循环引用保护
    }
    let line = match lines.get(index) {
        Some(l) => l,
        None => return Money::ZERO,
    };
    stack.push(index);
    let mut sum = Money::ZERO;
    for t in &line.terms {
        sum += eval_term(t, src, kind_map, cache, lines, stack);
    }
    stack.pop();
    if index < cache.len() {
        cache[index] = Some(sum);
    }
    sum
}

/// 渲染整张报表。`kind_maps` 每个元素对应一个金额列，负责把模板里的取数口径映射到实际口径。
pub fn render(
    def: &ReportDef,
    src: &dyn BalanceSource,
    company: &str,
    subtitle: &str,
    kind_maps: Vec<Box<dyn Fn(AmountKind) -> AmountKind>>,
) -> ReportTable {
    let n = def.lines.len();
    let mut rows = Vec::with_capacity(n);
    for (i, line) in def.lines.iter().enumerate() {
        let mut values = Vec::new();
        for map in &kind_maps {
            let mut cache = vec![None; n];
            let mut stack = Vec::new();
            let v = eval_line(i, src, map.as_ref(), &mut cache, &def.lines, &mut stack);
            values.push(v.round2());
        }
        rows.push(ReportRow {
            no: line.no.clone(),
            name: line.name.clone(),
            indent: line.indent,
            style: line.style,
            values,
            show_negative_red: line.show_negative_red,
        });
    }
    ReportTable {
        title: def.name.clone(),
        subtitle: subtitle.to_string(),
        company: company.to_string(),
        columns: def.columns.clone(),
        rows,
    }
}

/// 便捷函数：单列口径（如科目余额表）
pub fn render_single(
    def: &ReportDef,
    src: &dyn BalanceSource,
    company: &str,
    subtitle: &str,
    map: fn(AmountKind) -> AmountKind,
) -> ReportTable {
    render(def, src, company, subtitle, vec![Box::new(map)])
}

/// 常见的口径映射：身份映射
pub fn identity(k: AmountKind) -> AmountKind {
    k
}

/// 把"期末"映射为"期初"（用于资产负债表的年初列）
pub fn to_begin(k: AmountKind) -> AmountKind {
    match k {
        AmountKind::End => AmountKind::Begin,
        AmountKind::PeriodDebit => AmountKind::YearDebit,
        AmountKind::PeriodCredit => AmountKind::YearCredit,
        other => other,
    }
}

/// 余额方向辅助：判断某金额应显示为红字
pub fn should_red(v: Money) -> bool {
    v.is_negative()
}

/// 方向枚举未使用时的占位，保持导出稳定
pub fn dir_label(d: Direction) -> &'static str {
    d.label()
}
