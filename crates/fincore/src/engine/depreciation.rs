//! 固定资产折旧计算
//!
//! 三种方法，全部按**月**计提，结果保留 2 位小数。
//!
//! 会计惯例要点：
//! - 当月增加当月不提，下月起提；当月减少当月照提，下月停提。
//!   本模块只负责"给定第 n 个月该提多少"，起止期间由调用方（卡片）控制。
//! - **最后一期做尾差调整**：把累计折旧拉到"应提总额"，避免逐月四舍五入后
//!   累计折旧 ≠ 原值 − 残值，导致资产清理时净值不为零。
//! - 双倍余额递减法在最后两年（24 个月）改为直线法，这是准则要求，
//!   否则账面净值永远摊不到残值。

use crate::money::Money;
use crate::FinError;

/// 折旧方法
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DepMethod {
    /// 直线法（年限平均法）
    #[default]
    Straight,
    /// 双倍余额递减法
    DoubleDeclining,
    /// 年数总和法
    SumOfYears,
    /// 一次性摊销法（启用当期全额计提，适合低值易耗品/小额资产）
    OneTime,
    /// 五五摊销法（启用期计提 50%，最后一期计提 50%）
    FiftyFifty,
}

impl DepMethod {
    pub fn label(&self) -> &'static str {
        match self {
            DepMethod::Straight => "直线法",
            DepMethod::DoubleDeclining => "双倍余额递减法",
            DepMethod::SumOfYears => "年数总和法",
            DepMethod::OneTime => "一次性摊销法",
            DepMethod::FiftyFifty => "五五摊销法",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "ddb" | "doubledeclining" | "double_declining" => DepMethod::DoubleDeclining,
            "sum" | "sum_of_years" | "sumofyears" | "sy" => DepMethod::SumOfYears,
            "onetime" | "one_time" | "ot" => DepMethod::OneTime,
            "fiftyfifty" | "fifty_fifty" | "ff" => DepMethod::FiftyFifty,
            _ => DepMethod::Straight,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            DepMethod::Straight => "straight",
            DepMethod::DoubleDeclining => "ddb",
            DepMethod::SumOfYears => "sum_of_years",
            DepMethod::OneTime => "one_time",
            DepMethod::FiftyFifty => "fifty_fifty",
        }
    }

    pub const ALL: &'static [DepMethod] = &[
        DepMethod::Straight,
        DepMethod::DoubleDeclining,
        DepMethod::SumOfYears,
        DepMethod::OneTime,
        DepMethod::FiftyFifty,
    ];
}

/// 折旧计算输入
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DepInput {
    /// 原值
    pub original: Money,
    /// 残值率（0.05 表示 5%）
    pub residual_rate: Money,
    /// 预计使用月数（必须 > 0）
    pub life_months: i32,
    /// 折旧方法
    pub method: DepMethod,
}

impl DepInput {
    /// 残值
    pub fn residual(&self) -> Money {
        (self.original * self.residual_rate).round2()
    }

    /// 应提折旧总额 = 原值 − 残值
    pub fn depreciable(&self) -> Money {
        (self.original - self.residual()).round2()
    }

    pub fn validate(&self) -> Result<(), FinError> {
        if self.life_months <= 0 {
            return Err(FinError::msg("预计使用月数必须大于 0"));
        }
        // 上限防止超大年限导致折旧计划表内存暴涨（最长 100 年）
        if self.life_months > 1200 {
            return Err(FinError::msg("预计使用月数不能超过 1200（100 年）"));
        }
        if self.original.is_negative() {
            return Err(FinError::msg("资产原值不能为负"));
        }
        let r = self.residual_rate;
        if r.is_negative() || r > Money::ONE {
            return Err(FinError::msg("残值率必须在 0 ~ 1 之间"));
        }
        if self.residual() >= self.original && !self.original.is_zero() {
            return Err(FinError::msg("残值不能大于等于原值"));
        }
        Ok(())
    }
}

/// 单期折旧明细
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DepRow {
    /// 第几期（从 1 开始）
    pub seq: i32,
    /// 本期折旧额
    pub amount: Money,
    /// 期末累计折旧
    pub accum: Money,
    /// 期末账面净值
    pub net: Money,
}

/// 生成完整折旧计划表
///
/// `life_months` 期，最后一期自动做尾差调整，保证：
/// `最后一期累计折旧 == 应提折旧总额`、`最后一期净值 == 残值`。
pub fn schedule(input: &DepInput) -> Result<Vec<DepRow>, FinError> {
    input.validate()?;
    let total = input.depreciable();
    let n = input.life_months;
    if total.is_zero() {
        // 无可提折旧（例如原值等于残值），返回 n 期全 0
        return Ok((1..=n)
            .map(|i| DepRow {
                seq: i,
                amount: Money::ZERO,
                accum: Money::ZERO,
                net: input.original,
            })
            .collect());
    }

    let mut rows = Vec::with_capacity(n as usize);
    let mut accum = Money::ZERO;

    for i in 1..=n {
        let raw = raw_amount(input, i, accum, total);
        // 不能让累计折旧超过应提总额
        let amount = if accum + raw > total {
            total - accum
        } else {
            raw
        };
        // 最后一期：把累计拉平到应提总额，吃掉所有尾差
        let amount = if i == n { total - accum } else { amount };
        accum += amount;
        rows.push(DepRow {
            seq: i,
            amount,
            accum,
            net: input.original - accum,
        });
    }
    // 兜底：四舍五入可能让最后一期累计差几分，强制拉平
    if let Some(last) = rows.last_mut() {
        if last.accum != total {
            let diff = total - last.accum;
            last.amount += diff;
            last.accum = total;
            last.net = input.original - total;
        }
    }
    Ok(rows)
}

/// 第 `i` 期（1-based）的折旧额（未做尾差与上限处理）
fn raw_amount(input: &DepInput, i: i32, accum: Money, total: Money) -> Money {
    let n = input.life_months;
    match input.method {
        DepMethod::Straight => {
            // 每月等额（life_months 已在 DepInput 校验中要求 > 0）
            total.checked_div(Money::from_i64(n as i64))
                .expect("life_months 已校验 > 0")
                .round2()
        }
        DepMethod::DoubleDeclining => {
            // 前 n-24 期按净值双倍摊销，最后 24 期改直线
            // 当使用期 ≤24 个月时 switch=0，整个使用期内都走直线，符合"最后两年改直线"的准则
            let switch = (n - 24).max(0);
            if i <= switch {
                let net = input.original - accum;
                let rate = Money::from_i64(2)
                    .checked_div(Money::from_i64(n as i64))
                    .expect("life_months 已校验 > 0");
                (net * rate).round2()
            } else {
                // 剩余期数内把剩余可提额摊完（不扣残值之外的部分）
                let remain_months = n - i + 1;
                if remain_months <= 0 {
                    return Money::ZERO;
                }
                let remain = total - accum;
                remain
                    .checked_div(Money::from_i64(remain_months as i64))
                    .expect("remain_months 已判 > 0")
                    .round2()
            }
        }
        DepMethod::SumOfYears => {
            // 年数总和法：按年分档，第 y 年的年折旧 = 剩余年限 / 年数总和 × 应提总额
            let years = ((n + 11) / 12).max(1); // 向上取整到年
            let sum: i64 = (1..=years as i64).sum();
            if sum == 0 {
                return Money::ZERO;
            }
            // 当前处于第几年（1-based）
            let y = ((i - 1) / 12 + 1).min(years);
            let remain_years = years as i64 - y as i64 + 1;
            let year_amount = (total * Money::from_i64(remain_years))
                .checked_div(Money::from_i64(sum))
                .expect("sum 已判非零")
                .round2();
            // 该年内的月数（最后一年可能不满 12 个月）
            let months_in_year = (n - (y - 1) * 12).clamp(1, 12);
            year_amount
                .checked_div(Money::from_i64(months_in_year as i64))
                .expect("months_in_year 已 clamp 到 [1,12]")
                .round2()
        }
        DepMethod::OneTime => {
            // 一次性摊销：启用当期全额计提，其余各期 0
            if i == 1 {
                total
            } else {
                Money::ZERO
            }
        }
        DepMethod::FiftyFifty => {
            // 五五摊销：启用期计提 50%，最后一期（报废前一期）计提 50%
            if n == 1 {
                return total;
            }
            if i == 1 || i == n {
                total.checked_div(Money::from_i64(2))
                    .expect("字面量除数 2 非零")
                    .round2()
            } else {
                Money::ZERO
            }
        }
    }
}

/// 只算某一期的折旧额（用于 UI 预览单月）
pub fn amount_at(input: &DepInput, seq: i32) -> Result<Money, FinError> {
    let rows = schedule(input)?;
    Ok(rows
        .iter()
        .find(|r| r.seq == seq)
        .map(|r| r.amount)
        .unwrap_or(Money::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    #[test]
    fn straight_line_sum_to_depreciable() {
        // 原值 120000，残值率 5% → 应提 114000，10 年 = 120 期
        let input = DepInput {
            original: m("120000"),
            residual_rate: m("0.05"),
            life_months: 120,
            method: DepMethod::Straight,
        };
        let rows = schedule(&input).unwrap();
        assert_eq!(rows.len(), 120);
        // 每月 950
        assert_eq!(rows[0].amount, m("950"));
        // 累计必须精确等于应提总额
        assert_eq!(rows.last().unwrap().accum, m("114000"));
        // 期末净值必须等于残值
        assert_eq!(rows.last().unwrap().net, m("6000"));
        // 逐期净值单调递减
        for w in rows.windows(2) {
            assert!(w[1].net <= w[0].net);
        }
    }

    #[test]
    fn straight_line_rounding_no_drift() {
        // 10000 / 36 个月除不尽，验证尾差被最后一期吃掉
        let input = DepInput {
            original: m("10000"),
            residual_rate: m("0.1"),
            life_months: 36,
            method: DepMethod::Straight,
        };
        let rows = schedule(&input).unwrap();
        assert_eq!(rows.last().unwrap().accum, m("9000"));
        assert_eq!(rows.last().unwrap().net, m("1000"));
    }

    #[test]
    fn ddb_ends_at_residual() {
        let input = DepInput {
            original: m("100000"),
            residual_rate: m("0.05"),
            life_months: 60,
            method: DepMethod::DoubleDeclining,
        };
        let rows = schedule(&input).unwrap();
        // 双倍余额递减前期计提快
        assert!(rows[0].amount > rows[59].amount);
        // 但终点一样落到残值
        assert_eq!(rows.last().unwrap().accum, m("95000"));
        assert_eq!(rows.last().unwrap().net, m("5000"));
    }

    #[test]
    fn sum_of_years_ends_at_residual() {
        let input = DepInput {
            original: m("120000"),
            residual_rate: m("0"),
            life_months: 60,
            method: DepMethod::SumOfYears,
        };
        let rows = schedule(&input).unwrap();
        // 年数总和法前期多后期少
        assert!(rows[0].amount > rows[59].amount);
        assert_eq!(rows.last().unwrap().accum, m("120000"));
        assert_eq!(rows.last().unwrap().net, Money::ZERO);
    }

    #[test]
    fn validation_rejects_bad_input() {
        assert!(DepInput {
            original: m("1000"),
            residual_rate: m("0"),
            life_months: 0,
            method: DepMethod::Straight,
        }
        .validate()
        .is_err());
        assert!(DepInput {
            original: m("1000"),
            residual_rate: m("1.5"),
            life_months: 12,
            method: DepMethod::Straight,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn method_parse_roundtrip() {
        for mth in DepMethod::ALL {
            assert_eq!(DepMethod::parse(mth.code()), *mth);
        }
        assert_eq!(DepMethod::parse("ddb"), DepMethod::DoubleDeclining);
        assert_eq!(DepMethod::parse("未知"), DepMethod::Straight);
    }

    #[test]
    fn one_time_amortizes_in_first_period() {
        let input = DepInput {
            original: m("3000"),
            residual_rate: m("0"),
            life_months: 12,
            method: DepMethod::OneTime,
        };
        let rows = schedule(&input).unwrap();
        assert_eq!(rows.len(), 12);
        // 第 1 期全额计提
        assert_eq!(rows[0].amount, m("3000"));
        assert_eq!(rows[0].accum, m("3000"));
        // 其余各期 0
        assert_eq!(rows[1].amount, Money::ZERO);
        assert_eq!(rows.last().unwrap().amount, Money::ZERO);
        // 累计 = 原值，净值 0
        assert_eq!(rows.last().unwrap().accum, m("3000"));
        assert_eq!(rows.last().unwrap().net, Money::ZERO);
    }

    #[test]
    fn fifty_fifty_splits_whole_life_periods() {
        let input = DepInput {
            original: m("6000"),
            residual_rate: m("0"),
            life_months: 10,
            method: DepMethod::FiftyFifty,
        };
        let rows = schedule(&input).unwrap();
        assert_eq!(rows.len(), 10);
        // 第 1 期 50%
        assert_eq!(rows[0].amount, m("3000"));
        // 中间各期 0
        assert_eq!(rows[4].amount, Money::ZERO);
        // 最后一期 50%（尾差调整后为剩余全部）
        assert_eq!(rows.last().unwrap().amount, m("3000"));
        assert_eq!(rows.last().unwrap().accum, m("6000"));
        assert_eq!(rows.last().unwrap().net, Money::ZERO);
    }

    #[test]
    fn fifty_fifty_single_period_takes_all() {
        // 生命周期仅 1 期时，五五摊销=一次全额
        let input = DepInput {
            original: m("1000"),
            residual_rate: m("0.1"),
            life_months: 1,
            method: DepMethod::FiftyFifty,
        };
        let rows = schedule(&input).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].amount, m("900"));
        assert_eq!(rows[0].accum, m("900"));
        assert_eq!(rows[0].net, m("100"));
    }
}
