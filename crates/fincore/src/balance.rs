//! 余额与账簿行模型
//!
//! 统一约定：所有"余额"用 **带符号金额** 表示，正 = 借方余额，负 = 贷方余额。
//! 展示时再结合科目余额方向拆成"方向 + 绝对值"。这样求和与逐级汇总都不需要分支判断。

use serde::{Deserialize, Serialize};

use crate::account::Direction;
use crate::money::Money;
use crate::period::Period;
use crate::voucher::AuxRef;

/// 由带符号金额拆出（方向, 绝对值）
#[inline]
pub fn signed_to_dir_amount(signed: Money) -> (Direction, Money) {
    if signed.is_zero() {
        // 零余额习惯上仍按科目默认方向显示，这里返回"借/0.00"，由调用方按需覆盖
        (Direction::Debit, Money::ZERO)
    } else if signed.is_negative() {
        (Direction::Credit, signed.abs())
    } else {
        (Direction::Debit, signed)
    }
}

/// 由（方向, 绝对值）合成带符号金额
#[inline]
pub fn dir_amount_to_signed(dir: Direction, amount: Money) -> Money {
    match dir {
        Direction::Debit => amount,
        Direction::Credit => -amount,
    }
}

/// 数量账（数量金额式明细账用）
#[derive(Clone, Copy, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct QtyRow {
    /// 期初数量（带符号，正=借方方向）
    pub begin: Money,
    /// 本期收入（借方）数量
    pub in_qty: Money,
    /// 本期发出（贷方）数量
    pub out_qty: Money,
}

impl QtyRow {
    pub fn end(self) -> Money {
        self.begin + self.in_qty - self.out_qty
    }
    pub fn is_zero(self) -> bool {
        self.begin.is_zero() && self.in_qty.is_zero() && self.out_qty.is_zero()
    }
}

/// 科目余额表行（按 科目 + 辅助核算 聚合）
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct BalanceRow {
    pub account_code: String,
    pub account_name: String,
    pub aux: AuxRef,
    /// 期初余额（带符号）
    pub begin: Money,
    /// 本期借方发生额
    pub debit: Money,
    /// 本期贷方发生额
    pub credit: Money,
    /// 本年累计借方
    pub ytd_debit: Money,
    /// 本年累计贷方
    pub ytd_credit: Money,
    /// 数量账
    pub qty: Option<QtyRow>,
}

impl BalanceRow {
    /// 期末余额（带符号）
    pub fn end(&self) -> Money {
        self.begin + self.debit - self.credit
    }
    /// 期末方向与绝对值
    pub fn end_dir_amount(&self) -> (Direction, Money) {
        signed_to_dir_amount(self.end())
    }
    /// 期初方向与绝对值
    pub fn begin_dir_amount(&self) -> (Direction, Money) {
        signed_to_dir_amount(self.begin)
    }
    /// 余额方向是否与该科目应有方向相反（如资产类出现贷方余额）
    pub fn is_abnormal(&self, expected: Direction) -> bool {
        let v = self.end();
        if v.is_zero() {
            return false;
        }
        (v.is_negative() && expected == Direction::Debit)
            || (v.is_positive() && expected == Direction::Credit)
    }
    /// 本期是否无任何发生额
    pub fn is_static(&self) -> bool {
        self.debit.is_zero() && self.credit.is_zero()
    }
    /// 本期与期末是否全为零（用于过滤空行）
    pub fn is_empty_row(&self) -> bool {
        self.begin.is_zero() && self.is_static() && self.end().is_zero()
    }
}

/// 明细账行（一笔分录一行）
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct LedgerRow {
    pub period: Period,
    pub date: chrono::NaiveDate,
    pub voucher_id: i64,
    pub voucher_no: String,
    /// 凭证字
    pub word: String,
    /// 凭证号
    pub no: i32,
    pub line: i32,
    pub summary: String,
    pub account_code: String,
    pub aux: AuxRef,
    pub debit: Money,
    pub credit: Money,
    /// 余额方向
    pub dir: Direction,
    /// 余额绝对值
    pub balance: Money,
    /// 带符号余额（连续滚动）
    pub signed_balance: Money,
    pub qty_in: Option<Money>,
    pub qty_out: Option<Money>,
    pub qty_balance: Option<Money>,
}

/// 总账行（按期间汇总）
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct GeneralLedgerRow {
    pub period: Period,
    pub summary: String,
    pub debit: Money,
    pub credit: Money,
    pub dir: Direction,
    pub balance: Money,
    pub signed_balance: Money,
}

/// 日记账行（现金/银行日记账）
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct JournalRow {
    pub date: chrono::NaiveDate,
    pub voucher_no: String,
    pub summary: String,
    /// 对方科目（用于日记账展示）
    pub opposite_accounts: String,
    pub debit: Money,
    pub credit: Money,
    pub dir: Direction,
    pub balance: Money,
    pub settle_type: Option<String>,
    pub settle_no: Option<String>,
    /// 出纳签字人（凭证级，出纳日记账签字列展示）
    pub cashier: Option<String>,
}

/// 多栏账的一栏定义
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ColumnDef {
    pub account_code: String,
    pub name: String,
    pub dir: Direction,
}

/// 试算平衡结果
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct TrialBalance {
    pub begin_debit: Money,
    pub begin_credit: Money,
    pub period_debit: Money,
    pub period_credit: Money,
    pub end_debit: Money,
    pub end_credit: Money,
}

impl TrialBalance {
    /// 期初借贷是否平衡
    pub fn begin_balanced(&self) -> bool {
        (self.begin_debit - self.begin_credit).round2().is_zero()
    }
    /// 本期发生额借贷是否平衡
    pub fn period_balanced(&self) -> bool {
        (self.period_debit - self.period_credit).round2().is_zero()
    }
    /// 期末借贷是否平衡
    pub fn end_balanced(&self) -> bool {
        (self.end_debit - self.end_credit).round2().is_zero()
    }
    /// 全部平衡
    pub fn is_balanced(&self) -> bool {
        self.begin_balanced() && self.period_balanced() && self.end_balanced()
    }
    /// 不平衡项描述
    pub fn problems(&self) -> Vec<String> {
        let mut v = Vec::new();
        if !self.begin_balanced() {
            v.push(format!(
                "期初不平衡：借 {} 贷 {}，差 {}",
                self.begin_debit,
                self.begin_credit,
                (self.begin_debit - self.begin_credit).fmt_money()
            ));
        }
        if !self.period_balanced() {
            v.push(format!(
                "本期发生额不平衡：借 {} 贷 {}，差 {}",
                self.period_debit,
                self.period_credit,
                (self.period_debit - self.period_credit).fmt_money()
            ));
        }
        if !self.end_balanced() {
            v.push(format!(
                "期末不平衡：借 {} 贷 {}，差 {}",
                self.end_debit,
                self.end_credit,
                (self.end_debit - self.end_credit).fmt_money()
            ));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_conversion() {
        assert_eq!(signed_to_dir_amount(Money::parse("-100").unwrap()).0, Direction::Credit);
        assert_eq!(signed_to_dir_amount(Money::parse("-100").unwrap()).1, Money::parse("100").unwrap());
        assert_eq!(dir_amount_to_signed(Direction::Credit, Money::parse("5").unwrap()), Money::parse("-5").unwrap());
    }

    #[test]
    fn balance_row_end() {
        let r = BalanceRow {
            begin: Money::parse("100").unwrap(),
            debit: Money::parse("50").unwrap(),
            credit: Money::parse("30").unwrap(),
            ..Default::default()
        };
        assert_eq!(r.end(), Money::parse("120").unwrap());
        assert_eq!(r.end_dir_amount(), (Direction::Debit, Money::parse("120").unwrap()));
        assert!(!r.is_abnormal(Direction::Debit));
        assert!(r.is_abnormal(Direction::Credit));
    }

    #[test]
    fn trial_balance() {
        let t = TrialBalance {
            begin_debit: Money::parse("1000").unwrap(),
            begin_credit: Money::parse("1000").unwrap(),
            period_debit: Money::parse("100").unwrap(),
            period_credit: Money::parse("100").unwrap(),
            end_debit: Money::parse("1100").unwrap(),
            end_credit: Money::parse("1100").unwrap(),
        };
        assert!(t.is_balanced());
        assert!(t.problems().is_empty());
    }

    /// M-15 定案：试算平衡差额按 round2（分）判定——
    /// 不足 1 分的尾差视为平衡，满 1 分即不平衡。
    #[test]
    fn trial_balance_quantized_tolerance() {
        let mk = |bd: &str| TrialBalance {
            begin_debit: Money::parse(bd).unwrap(),
            begin_credit: Money::parse("1000").unwrap(),
            period_debit: Money::parse("100").unwrap(),
            period_credit: Money::parse("100").unwrap(),
            end_debit: Money::parse("1100").unwrap(),
            end_credit: Money::parse("1100").unwrap(),
        };
        // 差 0.004：round2 后为 0 → 平衡
        let t = mk("1000.004");
        assert!(t.begin_balanced(), "不足 1 分的尾差应视为平衡");
        assert!(t.is_balanced());
        // 差 0.01：满 1 分 → 不平衡，且 problems 描述包含期初不平衡
        let t2 = mk("1000.01");
        assert!(!t2.begin_balanced());
        assert!(!t2.is_balanced());
        assert!(
            t2.problems().iter().any(|s| s.contains("期初不平衡")),
            "{:?}",
            t2.problems()
        );
    }
}
