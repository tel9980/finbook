//! 个人所得税 —— 工资薪金「累计预扣预缴法」
//!
//! 依据《个人所得税扣缴申报管理办法（试行）》（国家税务总局公告 2018 年第 61 号）。
//!
//! 核心公式（按月）：
//! ```text
//! 累计应纳税所得额 = 累计收入
//!                 − 累计减除费用（5000 × 已任职月数）
//!                 − 累计专项扣除（社保 + 公积金个人部分）
//!                 − 累计专项附加扣除
//!                 − 累计依法确定的其他扣除
//!
//! 累计应纳税额     = 累计应纳税所得额 × 预扣率 − 速算扣除数   （年度税率表）
//! 本期应预扣税额   = 累计应纳税额 − 累计已预扣税额
//! ```
//!
//! **注意**：用的是**年度**税率表，不是月度表。这是累计预扣法最容易搞错的地方——
//! 同样 1 万元应纳税所得额，按月税率表算和按年税率表算结果差很多。
//!
//! 本期应预扣为负数（多缴）时按 0 处理，多缴部分在汇算清缴时退税，中途不退。

use crate::money::Money;
use crate::FinError;

/// 基本减除费用标准：5000 元/月
pub const MONTHLY_DEDUCTION: i64 = 5000;

/// 年度税率表：[(下限, 上限, 税率, 速算扣除数)]
///
/// 级数 | 全年应纳税所得额          | 税率 | 速算扣除数
/// -----|---------------------------|------|-----------
/// 1    | ≤ 36,000                  | 3%   | 0
/// 2    | 36,000 ~ 144,000          | 10%  | 2,520
/// 3    | 144,000 ~ 300,000         | 20%  | 16,920
/// 4    | 300,000 ~ 420,000         | 25%  | 31,920
/// 5    | 420,000 ~ 660,000         | 30%  | 52,920
/// 6    | 660,000 ~ 960,000         | 35%  | 85,920
/// 7    | > 960,000                 | 45%  | 181,920
fn brackets() -> Vec<(Money, Money, Money, Money)> {
    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }
    let max = m("999999999999");
    vec![
        (m("0"), m("36000"), m("0.03"), m("0")),
        (m("36000"), m("144000"), m("0.10"), m("2520")),
        (m("144000"), m("300000"), m("0.20"), m("16920")),
        (m("300000"), m("420000"), m("0.25"), m("31920")),
        (m("420000"), m("660000"), m("0.30"), m("52920")),
        (m("660000"), m("960000"), m("0.35"), m("85920")),
        (m("960000"), max, m("0.45"), m("181920")),
    ]
}

/// 按全年应纳税所得额查表，返回（税率, 速算扣除数）
pub fn rate_of(taxable: Money) -> (Money, Money) {
    if taxable <= Money::ZERO {
        return (Money::ZERO, Money::ZERO);
    }
    for (lo, hi, rate, quick) in brackets() {
        if taxable > lo && taxable <= hi {
            return (rate, quick);
        }
    }
    // 超过最高档
    let last = brackets().pop().unwrap();
    (last.2, last.3)
}

/// 累计预扣法的累计输入（截至本期，含本期）
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cumulative {
    /// 累计收入（应发工资合计）
    pub income: Money,
    /// 累计专项扣除（社保 + 公积金个人部分）
    pub special: Money,
    /// 累计专项附加扣除
    pub additional: Money,
    /// 累计依法确定的其他扣除
    pub other: Money,
    /// 累计已预扣税额
    pub withheld: Money,
    /// 在本单位任职的月数（用于算减除费用，1~12）
    pub months: i32,
}

impl Cumulative {
    /// 累计基本减除费用
    pub fn basic_deduction(&self) -> Money {
        Money::from_i64(MONTHLY_DEDUCTION * self.months as i64)
    }

    /// 累计应纳税所得额（可为负，为负则本期不扣税）
    pub fn taxable(&self) -> Money {
        let t = self.income
            - self.basic_deduction()
            - self.special
            - self.additional
            - self.other;
        t.round2()
    }

    /// 累计应纳税额
    pub fn tax_due(&self) -> Money {
        let t = self.taxable();
        if t <= Money::ZERO {
            return Money::ZERO;
        }
        let (rate, quick) = rate_of(t);
        let tax = t * rate - quick;
        tax.round2().max(Money::ZERO)
    }
}

/// 本期应预扣税额
///
/// = 累计应纳税额 − 累计已预扣，为负则取 0（多缴不退，留到汇算清缴）
pub fn current_tax(c: &Cumulative) -> Result<Money, FinError> {
    if c.months < 1 || c.months > 12 {
        return Err(FinError::msg("任职月数必须在 1 ~ 12 之间"));
    }
    let due = c.tax_due();
    let cur = due - c.withheld;
    Ok(cur.round2().max(Money::ZERO))
}

/// 一次性算全年的逐月税额（用于工资表批量计算 / 汇算预估）
///
/// `monthly[i]` 为第 i+1 个月的（应发, 专项扣除, 专项附加扣除, 其他扣除）
pub fn year_schedule(
    monthly: &[(Money, Money, Money, Money)],
) -> Vec<Money> {
    let mut income = Money::ZERO;
    let mut special = Money::ZERO;
    let mut additional = Money::ZERO;
    let mut other = Money::ZERO;
    let mut withheld = Money::ZERO;
    let mut out = Vec::with_capacity(monthly.len());

    // 全年最多 12 个月：多余入参直接截断，否则 months>12 会让 current_tax 返回
    // Err 被吞成 0，后续月份税额全变成 0（静默错数）。
    for (i, (inc, sp, ad, ot)) in monthly.iter().take(12).enumerate() {
        income += *inc;
        special += *sp;
        additional += *ad;
        other += *ot;
        let c = Cumulative {
            income,
            special,
            additional,
            other,
            withheld,
            months: (i + 1) as i32,
        };
        let t = match current_tax(&c) {
            Ok(v) => v,
            Err(_) => {
                debug_assert!(false, "months 已限制在 1..=12，current_tax 不应失败");
                Money::ZERO
            }
        };
        withheld += t;
        out.push(t);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    #[test]
    fn rate_brackets() {
        let (r, q) = rate_of(m("36000"));
        assert_eq!(r, m("0.03"));
        assert_eq!(q, m("0"));

        let (r, q) = rate_of(m("36001"));
        assert_eq!(r, m("0.10"));
        assert_eq!(q, m("2520"));

        let (r, _q) = rate_of(m("144000"));
        assert_eq!(r, m("0.10"));
        let (r2, q2) = rate_of(m("144001"));
        assert_eq!(r2, m("0.20"));
        assert_eq!(q2, m("16920"));

        let (r, q) = rate_of(m("1000000"));
        assert_eq!(r, m("0.45"));
        assert_eq!(q, m("181920"));

        // 负数与零不征税
        assert_eq!(rate_of(m("-100")), (Money::ZERO, Money::ZERO));
        assert_eq!(rate_of(Money::ZERO), (Money::ZERO, Money::ZERO));
    }

    #[test]
    fn low_income_no_tax() {
        // 月薪 5000，减除费用也是 5000，永远不交税
        let mut out = vec![];
        for i in 1..=6 {
            let c = Cumulative {
                income: m("5000") * Money::from_i64(i),
                special: Money::ZERO,
                additional: Money::ZERO,
                other: Money::ZERO,
                withheld: Money::ZERO,
                months: i as i32,
            };
            out.push(current_tax(&c).unwrap());
        }
        assert!(out.iter().all(|t| t.is_zero()));
    }

    #[test]
    fn classical_monthly_increasing() {
        // 月薪 15000，社保公积金个人部分 2000，无专项附加扣除
        // 第1月：累计应税 = 15000 - 5000 - 2000 = 8000 → 3% → 240
        // 第2月：累计应税 = 30000 - 10000 - 4000 = 16000 → 3% → 480，本期 480-240=240
        // 第5月：累计应税 = 75000 - 25000 - 10000 = 40000 → 10% - 2520 = 1480
        //        前4月已扣：第4月累计应税 32000 → 3% → 960；故本期 1480-960 = 520
        let monthly: Vec<_> = (0..12)
            .map(|_| (m("15000"), m("2000"), Money::ZERO, Money::ZERO))
            .collect();
        let taxes = year_schedule(&monthly);

        assert_eq!(taxes[0], m("240"));
        assert_eq!(taxes[1], m("240"));
        assert_eq!(taxes[4], m("520"));
        // 累计预扣法的特点：前期税率低，跳档后单月税额上升
        assert!(taxes[11] >= taxes[0]);
        // 全年总税额 = 第12月累计应税 96000 → 10% - 2520 = 7080
        let total: Money = taxes.iter().fold(Money::ZERO, |a, b| a + *b);
        assert_eq!(total, m("7080"));
    }

    #[test]
    fn additional_deduction_reduces_tax() {
        // 同样月薪 15000/社保 2000，其中有 4000/月 专项附加扣除（如房贷+赡养老人）
        let without: Vec<_> = (0..12)
            .map(|_| (m("15000"), m("2000"), Money::ZERO, Money::ZERO))
            .collect();
        let with: Vec<_> = (0..12)
            .map(|_| (m("15000"), m("2000"), m("4000"), Money::ZERO))
            .collect();
        let a: Money = year_schedule(&without).iter().fold(Money::ZERO, |x, y| x + *y);
        let b: Money = year_schedule(&with).iter().fold(Money::ZERO, |x, y| x + *y);
        assert!(b < a);
    }

    #[test]
    fn over_withheld_never_negative() {
        // 累计已预扣 10000，但累计应税为负 → 本期应为 0，不是负数
        let c = Cumulative {
            income: m("30000"),
            special: Money::ZERO,
            additional: m("30000"),
            other: Money::ZERO,
            withheld: m("10000"),
            months: 6,
        };
        assert_eq!(current_tax(&c).unwrap(), Money::ZERO);
    }

    #[test]
    fn months_must_be_1_to_12() {
        let c = Cumulative {
            months: 0,
            ..Default::default()
        };
        assert!(current_tax(&c).is_err());
        let c = Cumulative {
            months: 13,
            ..Default::default()
        };
        assert!(current_tax(&c).is_err());
    }

    #[test]
    fn high_income_crosses_brackets() {
        // 月薪 100000，社保 5000，无附加扣除
        let monthly: Vec<_> = (0..12)
            .map(|_| (m("100000"), m("5000"), Money::ZERO, Money::ZERO))
            .collect();
        let taxes = year_schedule(&monthly);
        // 第12月累计应税 = 1200000 - 60000 - 60000 = 1080000 → 45% - 181920 = 304080
        let total: Money = taxes.iter().fold(Money::ZERO, |a, b| a + *b);
        assert_eq!(total, m("304080"));
    }
}
