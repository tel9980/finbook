//! 记账凭证模型
//!
//! 一张凭证 = 表头（日期、字号、附件数、制单/审核/记账人）+ 若干分录。
//! 铁律：**有借必有贷，借贷必相等**，且借贷不能同时为零、不能一借多贷混乱（不限制，允许复合分录）。

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::account::{AuxKind, AuxMask, Direction};
use crate::money::Money;
use crate::period::Period;

/// 凭证状态机：`未记账 → 已记账`（另设"作废"终态；历史数据中的"已审核"视同未记账）。
/// 无独立审核环节：未记账凭证核对无误后手动「记账」确认入账。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoucherStatus {
    /// 未记账（刚录入，待确认）
    Draft,
    /// 已审核（历史遗留状态，视同未记账）
    Audited,
    /// 已记账（登记账簿）
    Posted,
    /// 已作废（保留痕迹，不参与任何汇总）
    Void,
}

impl VoucherStatus {
    pub fn label(self) -> &'static str {
        match self {
            VoucherStatus::Draft => "未记账",
            VoucherStatus::Audited => "已审核",
            VoucherStatus::Posted => "已记账",
            VoucherStatus::Void => "已作废",
        }
    }
    /// 是否参与账簿汇总（未记账/已记账均参与，已作废不参与）
    pub fn counts(self) -> bool {
        self != VoucherStatus::Void
    }
    /// 是否可编辑：未记账可改（历史"已审核"视同未记账）；已记账需先反记账
    pub fn can_edit(self) -> bool {
        matches!(self, VoucherStatus::Draft | VoucherStatus::Audited)
    }
    pub fn can_delete(self) -> bool {
        matches!(self, VoucherStatus::Draft | VoucherStatus::Audited)
    }
    pub fn can_post(self) -> bool {
        matches!(self, VoucherStatus::Draft | VoucherStatus::Audited)
    }
    pub fn can_unpost(self) -> bool {
        self == VoucherStatus::Posted
    }
}

/// 凭证来源，用于区分系统自动生成（如期末结转）与手工录入
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoucherSource {
    /// 手工录入
    Manual,
    /// 期末结转损益
    CarryForward,
    /// 期初建账
    Opening,
    /// 外部导入
    Import,
    /// 模板生成
    Template,
    /// 自动转账
    AutoTransfer,
    /// 期末调汇
    FxAdjust,
    /// 业务单据生成（折旧 / 工资 / 报销 / 存货 / 成本结转）
    Business,
}

impl VoucherSource {
    pub fn label(self) -> &'static str {
        match self {
            VoucherSource::Manual => "手工",
            VoucherSource::CarryForward => "结转",
            VoucherSource::Opening => "期初",
            VoucherSource::Import => "导入",
            VoucherSource::Template => "模板",
            VoucherSource::AutoTransfer => "自动转账",
            VoucherSource::FxAdjust => "调汇",
            VoucherSource::Business => "业务单据",
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            VoucherSource::Manual => "manual",
            VoucherSource::CarryForward => "carry",
            VoucherSource::Opening => "opening",
            VoucherSource::Import => "import",
            VoucherSource::Template => "template",
            VoucherSource::AutoTransfer => "auto_transfer",
            VoucherSource::FxAdjust => "fx",
            VoucherSource::Business => "business",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "carry" => VoucherSource::CarryForward,
            "opening" => VoucherSource::Opening,
            "import" => VoucherSource::Import,
            "template" => VoucherSource::Template,
            "auto_transfer" => VoucherSource::AutoTransfer,
            "fx" => VoucherSource::FxAdjust,
            "business" => VoucherSource::Business,
            _ => VoucherSource::Manual,
        }
    }
    /// 系统生成的凭证，不允许在凭证列表里手工修改金额
    pub fn is_generated(self) -> bool {
        !matches!(
            self,
            VoucherSource::Manual | VoucherSource::Opening | VoucherSource::Import
        )
    }
}

/// 分录上的辅助核算取值。未启用的维度保持 `None`。
///
/// 注意：`CashFlow`（现金流量项目）是**分析维度**，不参与余额聚合键——
/// 否则同一笔现金会因标注不同的现金流项目而被拆成多条余额，现金日记账就乱了。
/// 它单独存一列，只用于现金流量表取数。
#[derive(Clone, Default, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AuxRef {
    pub customer: Option<String>,
    pub supplier: Option<String>,
    pub dept: Option<String>,
    pub employee: Option<String>,
    pub project: Option<String>,
    pub item: Option<String>,
    pub cash_flow: Option<String>,
    pub bank: Option<String>,
}

impl AuxRef {
    pub fn get(&self, k: AuxKind) -> Option<&String> {
        match k {
            AuxKind::Customer => self.customer.as_ref(),
            AuxKind::Supplier => self.supplier.as_ref(),
            AuxKind::Dept => self.dept.as_ref(),
            AuxKind::Employee => self.employee.as_ref(),
            AuxKind::Project => self.project.as_ref(),
            AuxKind::Item => self.item.as_ref(),
            AuxKind::CashFlow => self.cash_flow.as_ref(),
            AuxKind::Bank => self.bank.as_ref(),
        }
    }
    pub fn set(&mut self, k: AuxKind, v: Option<String>) {
        match k {
            AuxKind::Customer => self.customer = v,
            AuxKind::Supplier => self.supplier = v,
            AuxKind::Dept => self.dept = v,
            AuxKind::Employee => self.employee = v,
            AuxKind::Project => self.project = v,
            AuxKind::Item => self.item = v,
            AuxKind::CashFlow => self.cash_flow = v,
            AuxKind::Bank => self.bank = v,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.customer.is_none()
            && self.supplier.is_none()
            && self.dept.is_none()
            && self.employee.is_none()
            && self.project.is_none()
            && self.item.is_none()
            && self.cash_flow.is_none()
            && self.bank.is_none()
    }
    /// 只保留科目实际启用的维度，避免脏数据进入聚合键
    pub fn masked(&self, m: AuxMask) -> AuxRef {
        let mut r = AuxRef::default();
        for k in AuxKind::ALL {
            if m.contains(*k) {
                r.set(*k, self.get(*k).cloned());
            }
        }
        r
    }
    /// 稳定分组键，用于按"科目 + 辅助核算"聚合余额（不含现金流量项目）
    pub fn key(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for k in AuxKind::BALANCE_DIMS {
            if let Some(v) = self.get(*k) {
                parts.push(format!("{}={}", k.code(), v));
            }
        }
        parts.join("\u{1f}")
    }
    /// [`AuxRef::key`] 的逆运算，把分组键还原成 AuxRef
    ///
    /// 无法识别的维度直接忽略——分组键可能来自旧版本，宁可少还原也不要崩。
    pub fn from_key(s: &str) -> AuxRef {
        let mut r = AuxRef::default();
        if s.is_empty() {
            return r;
        }
        for part in s.split('\u{1f}') {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            if v.is_empty() {
                continue;
            }
            if let Some(kind) = AuxKind::from_code(k) {
                r.set(kind, Some(v.to_string()));
            }
        }
        r
    }
    /// 人类可读描述，如 `客户:华东商贸 部门:销售部`
    pub fn display(&self, names: &std::collections::HashMap<String, String>) -> String {
        let mut parts: Vec<String> = Vec::new();
        for k in AuxKind::ALL {
            if let Some(v) = self.get(*k) {
                let shown = names.get(&format!("{}:{}", k.code(), v)).cloned().unwrap_or_else(|| v.clone());
                parts.push(format!("{}:{}", k.label(), shown));
            }
        }
        parts.join(" ")
    }
    /// 返回实际有值的维度编码列表（用于界面高亮必填项）
    pub fn filled(&self) -> Vec<AuxKind> {
        AuxKind::ALL.iter().copied().filter(|k| self.get(*k).is_some()).collect()
    }
}

/// 凭证分录
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: i64,
    /// 行号，从 1 开始
    pub line: i32,
    /// 摘要
    pub summary: String,
    /// 科目编码（必须末级）
    pub account_code: String,
    /// 辅助核算
    pub aux: AuxRef,
    /// 借方金额（非负）
    pub debit: Money,
    /// 贷方金额（非负）
    pub credit: Money,
    /// 数量（数量核算科目）
    pub qty: Option<Money>,
    /// 单价
    pub price: Option<Money>,
    /// 原币币种
    pub currency: Option<String>,
    /// 汇率
    pub rate: Option<rust_decimal::Decimal>,
    /// 原币金额
    pub amount_for: Option<Money>,
    /// 结算方式
    pub settle_type: Option<String>,
    /// 结算号（支票号、合同号等，用于往来核销与银行对账）
    pub settle_no: Option<String>,
    /// 业务日期（票面日期，用于账龄分析）
    pub biz_date: Option<NaiveDate>,
}

impl Entry {
    pub fn new(line: i32, account_code: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            id: 0,
            line,
            summary: summary.into(),
            account_code: account_code.into(),
            aux: AuxRef::default(),
            debit: Money::ZERO,
            credit: Money::ZERO,
            qty: None,
            price: None,
            currency: None,
            rate: None,
            amount_for: None,
            settle_type: None,
            settle_no: None,
            biz_date: None,
        }
    }

    /// 本行净额（正=借，负=贷）
    pub fn signed(&self) -> Money {
        self.debit - self.credit
    }

    /// 本行方向；借贷都为零时按借
    pub fn dir(&self) -> Direction {
        if self.credit > self.debit {
            Direction::Credit
        } else {
            Direction::Debit
        }
    }

    /// 借贷是否同时为零
    pub fn is_blank(&self) -> bool {
        self.debit.is_zero() && self.credit.is_zero()
    }

    /// 借贷是否同时有值（不合法）
    pub fn both_sides(&self) -> bool {
        self.debit.is_positive() && self.credit.is_positive()
    }

    /// 发生额（取非零一侧）
    pub fn amount(&self) -> Money {
        if self.debit.is_positive() {
            self.debit
        } else {
            self.credit
        }
    }
}

/// 记账凭证
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Voucher {
    pub id: i64,
    /// 所属会计期间
    pub period: Period,
    /// 业务日期
    pub date: NaiveDate,
    /// 凭证字，如"记"、"收"、"付"、"转"
    pub word: String,
    /// 凭证号（同一期间 + 同一凭证字内唯一，连续编号）
    pub no: i32,
    /// 附件张数
    pub attachments: i32,
    pub entries: Vec<Entry>,
    pub status: VoucherStatus,
    /// 制单人
    pub prepared_by: String,
    /// 审核人
    pub audited_by: Option<String>,
    /// 记账人
    pub posted_by: Option<String>,
    /// 出纳签字人
    pub cashier: Option<String>,
    pub source: VoucherSource,
    pub memo: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl Voucher {
    pub fn new(period: Period, date: NaiveDate, word: impl Into<String>, no: i32) -> Self {
        Self {
            id: 0,
            period,
            date,
            word: word.into(),
            no,
            attachments: 0,
            entries: Vec::new(),
            status: VoucherStatus::Draft,
            prepared_by: String::new(),
            audited_by: None,
            posted_by: None,
            cashier: None,
            source: VoucherSource::Manual,
            memo: String::new(),
            created_at: None,
            updated_at: None,
        }
    }

    /// 字号显示，如 `记-0001`
    pub fn voucher_no(&self) -> String {
        format!("{}-{:04}", self.word, self.no)
    }

    pub fn debit_total(&self) -> Money {
        self.entries.iter().map(|e| e.debit).sum()
    }
    pub fn credit_total(&self) -> Money {
        self.entries.iter().map(|e| e.credit).sum()
    }
    /// 借贷差额，应为 0
    pub fn diff(&self) -> Money {
        self.debit_total() - self.credit_total()
    }
    /// 借贷是否平衡（按落库口径：每条分录先量化到 2 位再比较）。
    ///
    /// 不能只用 `diff().round2().is_zero()`：写库时每条分录各自 round2，
    /// 借 100.015 / 贷 100.010 的差额 0.005 会被容差放过，但入库后变成
    /// 100.02 / 100.01，账套里就出现了一张不平的凭证。这里与 `money_param`
    /// 保持完全一致的量化口径。
    pub fn balanced(&self) -> bool {
        let debit: Money = self.entries.iter().map(|e| e.debit.round2()).sum();
        let credit: Money = self.entries.iter().map(|e| e.credit.round2()).sum();
        debit == credit
    }

    /// 空行（借贷均为零）行号
    pub fn blank_lines(&self) -> Vec<i32> {
        self.entries.iter().filter(|e| e.is_blank()).map(|e| e.line).collect()
    }

    /// 去掉首尾空行后的有效行数
    pub fn effective_lines(&self) -> usize {
        self.entries.iter().filter(|e| !e.is_blank()).count()
    }

    /// 重排行号，令其从 1 连续递增
    pub fn renumber(&mut self) {
        for (i, e) in self.entries.iter_mut().enumerate() {
            e.line = i as i32 + 1;
        }
    }

    /// 追加一行分录（行号自动递增）
    pub fn push_entry(&mut self, mut e: Entry) {
        e.line = self.entries.len() as i32 + 1;
        self.entries.push(e);
    }

    /// 首行摘要（列表展示用）
    pub fn first_summary(&self) -> String {
        self.entries
            .iter()
            .find(|e| !e.summary.trim().is_empty())
            .map(|e| e.summary.clone())
            .unwrap_or_default()
    }

    /// 涉及科目数（列表展示用）
    pub fn account_count(&self) -> usize {
        let mut set = std::collections::BTreeSet::new();
        for e in &self.entries {
            if !e.is_blank() {
                set.insert(e.account_code.clone());
            }
        }
        set.len()
    }
}

impl Default for Voucher {
    fn default() -> Self {
        let today = chrono::Local::now().date_naive();
        Voucher::new(Period::from_date(today), today, "记", 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v() -> Voucher {
        let d = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        let mut v = Voucher::new(Period::new(2026, 1).unwrap(), d, "记", 1);
        v.push_entry(Entry {
            debit: Money::parse("100.00").unwrap(),
            ..Entry::new(1, "1001", "提现")
        });
        v.push_entry(Entry {
            credit: Money::parse("100.00").unwrap(),
            ..Entry::new(2, "100201", "提现")
        });
        v
    }

    #[test]
    fn balance() {
        let v = v();
        assert_eq!(v.debit_total(), Money::parse("100").unwrap());
        assert!(v.balanced());
        assert_eq!(v.voucher_no(), "记-0001");
        assert_eq!(v.entries[0].dir(), Direction::Debit);
        assert_eq!(v.entries[1].dir(), Direction::Credit);
    }

    #[test]
    fn unbalanced() {
        let mut v = v();
        v.entries[1].credit = Money::parse("90").unwrap();
        assert!(!v.balanced());
        assert_eq!(v.diff(), Money::parse("10").unwrap());
    }

    /// M-15 定案：逐条量化到分（2 位）后严格判平，与落库 `money_param` 口径一致。
    /// 两侧同值的分位尾差同进同出 → 判平；异值尾差 100.015 / 100.010 → 判不平，
    /// 不能用"差额再 round2"的容差放过（入库后会真的差 0.01）。
    #[test]
    fn balanced_quantizes_per_entry() {
        // 同值尾差：两侧逐条 round2 后相等 → 判平
        let mut same = v();
        same.entries[0].debit = Money::parse("100.015").unwrap();
        same.entries[1].credit = Money::parse("100.015").unwrap();
        assert!(same.balanced(), "两侧同值的分位尾差应判平");
        // 异值尾差：原始差额只有 0.005，但量化后借 100.02 ≠ 贷 100.01 → 判不平
        let mut diff = v();
        diff.entries[0].debit = Money::parse("100.015").unwrap();
        diff.entries[1].credit = Money::parse("100.010").unwrap();
        assert!(!diff.balanced(), "量化后差 0.01 必须判不平");
        // 整分级差异一律判不平
        let mut big = v();
        big.entries[0].debit = Money::parse("100.01").unwrap();
        big.entries[1].credit = Money::parse("100.02").unwrap();
        assert!(!big.balanced());
    }

    #[test]
    fn aux_key_stable() {
        let mut a = AuxRef::default();
        a.customer = Some("C001".into());
        a.dept = Some("D01".into());
        let k1 = a.key();
        let mut b = AuxRef::default();
        b.dept = Some("D01".into());
        b.customer = Some("C001".into());
        assert_eq!(k1, b.key());
        assert!(AuxRef::default().key().is_empty());
    }

    #[test]
    fn aux_masking() {
        let mut a = AuxRef::default();
        a.customer = Some("C001".into());
        a.project = Some("P1".into());
        let m = AuxMask::NONE.with(AuxKind::Customer);
        let r = a.masked(m);
        assert_eq!(r.customer.as_deref(), Some("C001"));
        assert_eq!(r.project, None);
    }
}
