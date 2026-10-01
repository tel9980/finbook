//! 会计科目体系
//!
//! 按《企业会计准则》组织：资产 / 负债 / 共同 / 所有者权益 / 成本 / 收入 / 费用。
//! 科目编码采用分级定长，默认 `4-2-2-2`（如 `1001` → `100101` → `10010101`），
//! 与金蝶、用友的编码习惯一致。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::error::{FinError, Issues};
use crate::money::Money;
use crate::period::Period;

/// 余额方向
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// 借
    Debit,
    /// 贷
    Credit,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Direction::Debit => "借",
            Direction::Credit => "贷",
        }
    }
    pub fn opposite(self) -> Direction {
        match self {
            Direction::Debit => Direction::Credit,
            Direction::Credit => Direction::Debit,
        }
    }
    /// 借方为 +1，贷方为 -1。所有余额计算都在这个符号约定下进行。
    pub fn sign(self) -> i32 {
        match self {
            Direction::Debit => 1,
            Direction::Credit => -1,
        }
    }
    pub fn from_sign(s: i32) -> Direction {
        if s < 0 {
            Direction::Credit
        } else {
            Direction::Debit
        }
    }
}

/// 科目类别
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcctCategory {
    /// 资产
    Asset,
    /// 负债
    Liability,
    /// 共同类
    Common,
    /// 所有者权益
    Equity,
    /// 成本
    Cost,
    /// 收入（损益类）
    Income,
    /// 费用（损益类）
    Expense,
}

impl AcctCategory {
    pub fn label(self) -> &'static str {
        match self {
            AcctCategory::Asset => "资产",
            AcctCategory::Liability => "负债",
            AcctCategory::Common => "共同",
            AcctCategory::Equity => "权益",
            AcctCategory::Cost => "成本",
            AcctCategory::Income => "收入",
            AcctCategory::Expense => "费用",
        }
    }

    /// 归属的会计要素（利润表/资产负债表的取数大类）
    pub fn element(self) -> &'static str {
        match self {
            AcctCategory::Asset => "资产",
            AcctCategory::Liability => "负债",
            AcctCategory::Common => "共同",
            AcctCategory::Equity => "所有者权益",
            AcctCategory::Cost => "成本",
            AcctCategory::Income | AcctCategory::Expense => "损益",
        }
    }

    /// 该类别的默认余额方向
    pub fn default_dir(self) -> Direction {
        match self {
            AcctCategory::Asset | AcctCategory::Cost | AcctCategory::Expense | AcctCategory::Common => {
                Direction::Debit
            }
            AcctCategory::Liability | AcctCategory::Equity | AcctCategory::Income => Direction::Credit,
        }
    }

    /// 是否属于损益类（参与期末结转）
    pub fn is_profit_loss(self) -> bool {
        matches!(self, AcctCategory::Income | AcctCategory::Expense)
    }

    /// 是否属于资产负债表科目
    pub fn is_balance_sheet(self) -> bool {
        matches!(
            self,
            AcctCategory::Asset | AcctCategory::Liability | AcctCategory::Common | AcctCategory::Equity
        )
    }

    /// 由首位数字推断类别（用于导入外部科目表）
    pub fn from_code(code: &str) -> Option<AcctCategory> {
        match code.chars().next()? {
            '1' => Some(AcctCategory::Asset),
            '2' => Some(AcctCategory::Liability),
            '3' => Some(AcctCategory::Common),
            '4' => Some(AcctCategory::Equity),
            '5' => Some(AcctCategory::Cost),
            '6' => {
                // 损益类：60xx 收入 / 61xx 投资收益等 / 63xx 营业外收入 为收入，其余为费用
                let n: u32 = code.get(0..2).and_then(|s| s.parse().ok()).unwrap_or(60);
                match n {
                    60 | 61 | 63 => Some(AcctCategory::Income),
                    _ => Some(AcctCategory::Expense),
                }
            }
            _ => None,
        }
    }

    pub fn all() -> &'static [AcctCategory] {
        &[
            AcctCategory::Asset,
            AcctCategory::Liability,
            AcctCategory::Common,
            AcctCategory::Equity,
            AcctCategory::Cost,
            AcctCategory::Income,
            AcctCategory::Expense,
        ]
    }
}

// ---------------------------------------------------------------------------
// 辅助核算
// ---------------------------------------------------------------------------

/// 辅助核算维度
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuxKind {
    /// 客户（应收账款往来）
    Customer,
    /// 供应商（应付账款往来）
    Supplier,
    /// 部门
    Dept,
    /// 职员
    Employee,
    /// 项目
    Project,
    /// 存货
    Item,
    /// 现金流量项目
    CashFlow,
    /// 银行账户
    Bank,
}

impl AuxKind {
    pub const ALL: &'static [AuxKind] = &[
        AuxKind::Customer,
        AuxKind::Supplier,
        AuxKind::Dept,
        AuxKind::Employee,
        AuxKind::Project,
        AuxKind::Item,
        AuxKind::CashFlow,
        AuxKind::Bank,
    ];

    /// 参与余额聚合的维度（不含现金流量项目）
    pub const BALANCE_DIMS: &'static [AuxKind] = &[
        AuxKind::Customer,
        AuxKind::Supplier,
        AuxKind::Dept,
        AuxKind::Employee,
        AuxKind::Project,
        AuxKind::Item,
        AuxKind::Bank,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AuxKind::Customer => "客户",
            AuxKind::Supplier => "供应商",
            AuxKind::Dept => "部门",
            AuxKind::Employee => "职员",
            AuxKind::Project => "项目",
            AuxKind::Item => "存货",
            AuxKind::CashFlow => "现金流量",
            AuxKind::Bank => "银行账户",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
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
    pub fn from_code(s: &str) -> Option<AuxKind> {
        AuxKind::ALL.iter().copied().find(|k| k.code() == s)
    }
    pub fn bit(self) -> u32 {
        match self {
            AuxKind::Customer => 1 << 0,
            AuxKind::Supplier => 1 << 1,
            AuxKind::Dept => 1 << 2,
            AuxKind::Employee => 1 << 3,
            AuxKind::Project => 1 << 4,
            AuxKind::Item => 1 << 5,
            AuxKind::CashFlow => 1 << 6,
            AuxKind::Bank => 1 << 7,
        }
    }
}

/// 科目启用的辅助核算维度（位掩码）
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Hash)]
pub struct AuxMask(pub u32);

impl std::fmt::Debug for AuxMask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AuxMask({:#010b})", self.0)
    }
}

impl AuxMask {
    pub const NONE: AuxMask = AuxMask(0);

    pub fn contains(self, k: AuxKind) -> bool {
        self.0 & k.bit() != 0
    }
    pub fn with(mut self, k: AuxKind) -> Self {
        self.0 |= k.bit();
        self
    }
    pub fn without(mut self, k: AuxKind) -> Self {
        self.0 &= !k.bit();
        self
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn list(self) -> Vec<AuxKind> {
        AuxKind::ALL.iter().copied().filter(|k| self.contains(*k)).collect()
    }
    pub fn from_list(list: &[AuxKind]) -> Self {
        let mut m = AuxMask::NONE;
        for k in list {
            m = m.with(*k);
        }
        m
    }
    pub fn set(&mut self, k: AuxKind, on: bool) {
        *self = if on { self.with(k) } else { self.without(k) };
    }
}

// ---------------------------------------------------------------------------
// 科目
// ---------------------------------------------------------------------------

/// 会计科目
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Account {
    /// 科目编码，如 `1001`
    pub code: String,
    /// 科目名称，如 `库存现金`
    pub name: String,
    /// 科目类别
    pub category: AcctCategory,
    /// 余额方向
    pub dir: Direction,
    /// 启用的辅助核算维度
    pub aux: AuxMask,
    /// 数量核算单位（如"吨"、"件"），None 表示不核算数量
    pub unit: Option<String>,
    /// 外币核算币种代码（如 USD），None 表示只核算人民币
    pub currency: Option<String>,
    /// 是否核算数量
    pub has_qty: bool,
    /// 是否为现金科目（现金日记账 / 现金流量表取数）
    pub is_cash: bool,
    /// 是否为银行科目（银行日记账 / 银行对账）
    pub is_bank: bool,
    /// 默认现金流量项目编码
    pub cash_flow_item: Option<String>,
    /// 资产负债表项目（用于报表取数，留空则按科目类别归集）
    pub bs_item: Option<String>,
    /// 利润表项目
    pub pl_item: Option<String>,
    /// 停用
    pub disabled: bool,
    /// 备注
    pub memo: String,
}

impl Account {
    pub fn new(code: impl Into<String>, name: impl Into<String>, category: AcctCategory) -> Self {
        Self {
            code: code.into(),
            name: name.into(),
            category,
            dir: category.default_dir(),
            aux: AuxMask::NONE,
            unit: None,
            currency: None,
            has_qty: false,
            is_cash: false,
            is_bank: false,
            cash_flow_item: None,
            bs_item: None,
            pl_item: None,
            disabled: false,
            memo: String::new(),
        }
    }

    /// 由编码推断类别与方向
    pub fn from_code(code: &str, name: &str) -> Result<Self, FinError> {
        let cat = AcctCategory::from_code(code)
            .ok_or_else(|| FinError::msg(format!("无法由编码 {code} 推断科目类别")))?;
        Ok(Account::new(code, name, cat))
    }

    pub fn level(&self, segs: &[u8]) -> u8 {
        Account::level_of(&self.code, segs)
    }

    /// 根据级长定义推算编码所处级次
    pub fn level_of(code: &str, segs: &[u8]) -> u8 {
        let mut sum = 0usize;
        for (i, s) in segs.iter().enumerate() {
            sum += *s as usize;
            if code.len() == sum {
                return (i + 1) as u8;
            }
            if code.len() < sum {
                return (i + 1) as u8;
            }
        }
        segs.len() as u8
    }

    /// 取父级编码
    pub fn parent_code(&self, segs: &[u8]) -> Option<String> {
        Account::parent_of(&self.code, segs)
    }

    pub fn parent_of(code: &str, segs: &[u8]) -> Option<String> {
        let lvl = Account::level_of(code, segs) as usize;
        if lvl <= 1 {
            return None;
        }
        let len: usize = segs.iter().take(lvl - 1).map(|s| *s as usize).sum();
        // 非法编码（非 ASCII/长度不在级长边界）不 panic，按"无上级"处理
        code.get(..len).map(|s| s.to_string())
    }

    /// 该级次的编码长度
    pub fn code_len_of_level(segs: &[u8], level: u8) -> usize {
        segs.iter().take(level as usize).map(|s| *s as usize).sum()
    }
}

// ---------------------------------------------------------------------------
// 科目表（含树形关系）
// ---------------------------------------------------------------------------

/// 科目编码级长方案，默认 4-2-2-2
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeScheme(pub Vec<u8>);

impl Default for CodeScheme {
    fn default() -> Self {
        CodeScheme(vec![4, 2, 2, 2, 2])
    }
}

impl CodeScheme {
    pub fn max_len(&self) -> usize {
        self.0.iter().map(|s| *s as usize).sum()
    }
    /// 校验编码是否符合级长方案
    pub fn is_valid_code(&self, code: &str) -> bool {
        if code.is_empty() || !code.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        let mut sum = 0usize;
        for s in &self.0 {
            sum += *s as usize;
            if code.len() == sum {
                return true;
            }
        }
        false
    }
    /// 给定父编码，生成新子编码的建议前缀长度
    pub fn child_len(&self, parent: &str) -> usize {
        let lvl = Account::level_of(parent, &self.0) as usize;
        self.0.iter().take(lvl + 1).map(|s| *s as usize).sum()
    }
}

/// 科目表：内存中的科目树，提供查询、增删改与合法性校验
#[derive(Clone, Debug, Default)]
pub struct Chart {
    accounts: BTreeMap<String, Account>,
    scheme: CodeScheme,
}

impl Chart {
    pub fn new(scheme: CodeScheme) -> Self {
        Self {
            accounts: BTreeMap::new(),
            scheme,
        }
    }

    pub fn with_accounts(scheme: CodeScheme, accounts: Vec<Account>) -> Self {
        let mut m = BTreeMap::new();
        for a in accounts {
            m.insert(a.code.clone(), a);
        }
        Self {
            accounts: m,
            scheme,
        }
    }

    #[inline]
    pub fn scheme(&self) -> &CodeScheme {
        &self.scheme
    }
    pub fn set_scheme(&mut self, s: CodeScheme) {
        self.scheme = s;
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.accounts.len()
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    pub fn get(&self, code: &str) -> Option<&Account> {
        self.accounts.get(code)
    }
    pub fn get_mut(&mut self, code: &str) -> Option<&mut Account> {
        self.accounts.get_mut(code)
    }
    pub fn contains(&self, code: &str) -> bool {
        self.accounts.contains_key(code)
    }

    pub fn all(&self) -> Vec<&Account> {
        self.accounts.values().collect()
    }
    pub fn all_owned(&self) -> Vec<Account> {
        self.accounts.values().cloned().collect()
    }
    pub fn codes(&self) -> BTreeSet<String> {
        self.accounts.keys().cloned().collect()
    }

    pub fn level(&self, code: &str) -> u8 {
        Account::level_of(code, &self.scheme.0)
    }

    /// 直接下级（按编码排序）
    pub fn children(&self, code: &str) -> Vec<&Account> {
        let target_lvl = self.level(code) as usize + 1;
        let prefix = code;
        self.accounts
            .values()
            .filter(|a| {
                a.code.starts_with(prefix)
                    && a.code.len() > prefix.len()
                    && self.level(&a.code) as usize == target_lvl
            })
            .collect()
    }

    /// 所有后代（含多级）
    pub fn descendants(&self, code: &str) -> Vec<&Account> {
        self.accounts
            .values()
            .filter(|a| a.code.starts_with(code) && a.code != code)
            .collect()
    }

    /// 自身及所有后代编码（用于余额汇总的 SQL IN 条件）
    pub fn self_and_descendant_codes(&self, code: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .accounts
            .keys()
            .filter(|c| *c == code || (c.starts_with(code)))
            .cloned()
            .collect();
        v.sort();
        v
    }

    /// 是否末级科目（无下级）。末级科目才能直接记账。
    pub fn is_leaf(&self, code: &str) -> bool {
        self.children(code).is_empty()
    }

    /// 全路径名称，如 `银行存款 - 工商银行 - 基本户`
    pub fn full_name(&self, code: &str) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut cur = code.to_string();
        while let Some(a) = self.accounts.get(&cur) {
            parts.push(a.name.clone());
            match self.parent_code(&cur) {
                Some(p) => cur = p,
                None => break,
            }
        }
        parts.reverse();
        parts.join(" / ")
    }

    pub fn parent_code(&self, code: &str) -> Option<String> {
        Account::parent_of(code, &self.scheme.0)
    }

    /// 新增科目，返回校验问题
    pub fn validate_new(&self, a: &Account) -> Issues {
        let mut iss = Issues::new();
        iss.check(!a.code.is_empty(), "科目编码不能为空");
        iss.check(!a.name.trim().is_empty(), "科目名称不能为空");
        iss.check(
            self.scheme.is_valid_code(&a.code),
            format!(
                "科目编码 {} 不符合级长方案（可用长度：{}）",
                a.code,
                self.scheme
                    .0
                    .iter()
                    .scan(0usize, |st, s| {
                        *st += *s as usize;
                        Some(*st)
                    })
                    .map(|x| x.to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            ),
        );
        iss.check(!self.contains(&a.code), format!("科目编码 {} 已存在", a.code));
        if let Some(p) = self.parent_code(&a.code) {
            match self.get(&p) {
                None => iss.push(format!("上级科目 {p} 不存在，请先建立上级科目")),
                Some(pa) => {
                    // 上级若已停用，仍允许新增下级，但给出提醒，避免用户误以为建成功了却用不上
                    if pa.disabled {
                        iss.push(format!("上级科目 {p} 已停用，新增的下级科目同样不可用"));
                    }
                }
            }
        }
        iss
    }

    /// 删除科目前的可行性检查
    pub fn validate_delete(&self, code: &str) -> Issues {
        let mut iss = Issues::new();
        match self.get(code) {
            None => iss.push(format!("科目 {code} 不存在")),
            Some(_) => {
                if !self.children(code).is_empty() {
                    iss.push("存在下级科目，不能删除");
                }
            }
        }
        iss
    }

    pub fn insert(&mut self, a: Account) {
        self.accounts.insert(a.code.clone(), a);
    }

    pub fn remove(&mut self, code: &str) -> Option<Account> {
        self.accounts.remove(code)
    }
}

// ---------------------------------------------------------------------------
// 期初余额
// ---------------------------------------------------------------------------

/// 期初余额行（按科目 + 辅助核算维度）
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BeginBalance {
    pub account_code: String,
    /// 辅助核算值组合（与分录同一结构）
    pub aux: crate::voucher::AuxRef,
    /// 年初余额（带符号：正=借，负=贷）
    pub year_begin: Money,
    /// 启用期之前的累计借方发生额
    pub debit_accum: Money,
    /// 启用期之前的累计贷方发生额
    pub credit_accum: Money,
    /// 期初数量（带符号）
    pub qty_begin: Option<Money>,
}

impl BeginBalance {
    /// 启用期期初余额 = 年初 + 累计借 - 累计贷
    pub fn period_begin(&self) -> Money {
        self.year_begin + self.debit_accum - self.credit_accum
    }
    /// 本年累计借方（利润表取数用）
    pub fn year_debit(&self) -> Money {
        self.debit_accum
    }
    pub fn year_credit(&self) -> Money {
        self.credit_accum
    }
}

/// 账套参数
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookOptions {
    /// 科目编码级长
    pub code_scheme: Vec<u8>,
    /// 启用期间
    pub start_period: Period,
    /// 本位币
    pub base_currency: String,
    /// 企业名称
    pub company: String,
    /// 纳税识别号
    pub tax_no: String,
    /// 启用数量核算
    pub enable_qty: bool,
    /// 启用外币核算
    pub enable_foreign: bool,
    /// 启用审核环节（未记账 → 已审核 → 已记账）。默认关闭：录入核对后直接记账。
    /// 旧账套没有该字段时按关闭处理。
    #[serde(default)]
    pub enable_audit: bool,
    /// 出纳签字前置（可选，默认关闭）：开启后**涉及现金/银行科目**的凭证须出纳签字才能记账。
    /// 旧账套没有该字段时按关闭处理。
    #[serde(default)]
    pub require_cashier: bool,
    /// （已废弃）审核环节已移除：未记账凭证核对后直接记账。
    /// 字段仅为兼容旧账套序列化而保留，不再生效。
    pub require_audit: bool,
    /// 业务凭证默认科目（收付款单 / 发货收入 / 暂估等自动生成用）
    #[serde(default)]
    pub biz_accounts: BizAccounts,
    /// 预算控制：off/空=关闭（默认）、warn=超预算仅提醒放行、strong=超预算拒绝保存
    #[serde(default)]
    pub budget_control: String,
    /// 单据号前缀自定义（键：po/so/req/quo/prod；空/缺省用内置默认 CG/XS/QG/BJ/SC）
    #[serde(default)]
    pub doc_prefixes: std::collections::BTreeMap<String, String>,
    /// 凭证字号方案
    pub voucher_words: Vec<String>,
}

/// 业务凭证默认科目配置（须为末级科目编码；空值生成时回退到本默认表）
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BizAccounts {
    /// 应收账款（客户辅助）
    pub ar: String,
    /// 应付账款（供应商辅助）
    pub ap: String,
    /// 主营业务收入
    pub income: String,
    /// 销项税额
    pub tax_sales: String,
    /// 暂估借方（材料/库存商品）默认科目：存货编码不在科目表时的回退
    pub material: String,
    /// 默认资金账户（银行/现金）
    pub fund: String,
}

impl Default for BizAccounts {
    fn default() -> Self {
        Self {
            ar: "112201".to_string(),
            ap: "220201".to_string(),
            income: "600101".to_string(),
            tax_sales: "22210102".to_string(),
            material: "140301".to_string(),
            fund: "100201".to_string(),
        }
    }
}

impl Default for BookOptions {
    fn default() -> Self {
        Self {
            code_scheme: vec![4, 2, 2, 2, 2],
            start_period: Period::default(),
            base_currency: "CNY".to_string(),
            company: String::new(),
            tax_no: String::new(),
            enable_qty: false,
            enable_foreign: false,
            enable_audit: false,
            require_cashier: false,
            require_audit: true,
            biz_accounts: BizAccounts::default(),
            budget_control: String::new(),
            doc_prefixes: std::collections::BTreeMap::new(),
            voucher_words: vec!["记".to_string()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_chart() -> Chart {
        let mut c = Chart::new(CodeScheme::default());
        c.insert(Account::new("1001", "库存现金", AcctCategory::Asset));
        c.insert(Account::new("1002", "银行存款", AcctCategory::Asset));
        c.insert(Account::new("100201", "工行基本户", AcctCategory::Asset));
        c.insert(Account::new("100202", "建行一般户", AcctCategory::Asset));
        c.insert(Account::new("6001", "主营业务收入", AcctCategory::Income));
        c
    }

    #[test]
    fn tree_ops() {
        let c = demo_chart();
        assert_eq!(c.level("1001"), 1);
        assert_eq!(c.level("100201"), 2);
        assert!(c.is_leaf("1001"));
        assert!(!c.is_leaf("1002"));
        assert_eq!(c.children("1002").len(), 2);
        assert_eq!(c.descendants("1002").len(), 2);
        assert_eq!(c.parent_code("100201"), Some("1002".into()));
        assert_eq!(c.full_name("100201"), "银行存款 / 工行基本户");
        assert_eq!(c.self_and_descendant_codes("1002"), vec!["1002", "100201", "100202"]);
    }

    #[test]
    fn code_scheme() {
        let s = CodeScheme::default();
        assert!(s.is_valid_code("1001"));
        assert!(s.is_valid_code("100201"));
        assert!(!s.is_valid_code("10020"));
        assert!(!s.is_valid_code("10a1"));
        assert_eq!(s.child_len("1002"), 6);
    }

    #[test]
    fn category_from_code() {
        assert_eq!(AcctCategory::from_code("1001"), Some(AcctCategory::Asset));
        assert_eq!(AcctCategory::from_code("2202"), Some(AcctCategory::Liability));
        assert_eq!(AcctCategory::from_code("4001"), Some(AcctCategory::Equity));
        assert_eq!(AcctCategory::from_code("5001"), Some(AcctCategory::Cost));
        assert_eq!(AcctCategory::from_code("6001"), Some(AcctCategory::Income));
        assert_eq!(AcctCategory::from_code("6601"), Some(AcctCategory::Expense));
        assert_eq!(AcctCategory::from_code("6602"), Some(AcctCategory::Expense));
    }

    #[test]
    fn aux_mask() {
        let m = AuxMask::NONE.with(AuxKind::Customer).with(AuxKind::Dept);
        assert!(m.contains(AuxKind::Customer));
        assert!(!m.contains(AuxKind::Supplier));
        assert_eq!(m.list(), vec![AuxKind::Customer, AuxKind::Dept]);
        assert_eq!(AuxMask::from_list(&m.list()), m);
    }
}
