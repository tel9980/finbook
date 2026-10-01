//! 往来账龄分析
//!
//! 账龄不是简单的"按日期分桶"，有两点实务细节：
//!
//! 1. **按单据（分录）而不是按余额分桶**。同一客户可能有一笔新的应收和一笔老的应收，
//!    收款先抵老单（或手工指定），所以账龄必须逐笔计算再汇总，不能用"余额 × 平均账龄"。
//! 2. **贷方余额要单独成行**。预收账款性质的客户贷方余额不属于"应收账龄"，
//!    展示时单独列示，否则会把负数混进桶里让人看不懂。

use chrono::NaiveDate;

use crate::money::Money;
use crate::FinError;

/// 账龄区间
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AgingBucket {
    pub label: &'static str,
    /// 起始天数（含）
    pub from: i32,
    /// 结束天数（含），None 表示无上限
    pub to: Option<i32>,
}

impl AgingBucket {
    pub fn contains(&self, days: i32) -> bool {
        if days < self.from {
            return false;
        }
        match self.to {
            Some(t) => days <= t,
            None => true,
        }
    }
}

/// 按天分档（适合账期短、周转快的行业）
pub fn buckets_by_days() -> Vec<AgingBucket> {
    vec![
        AgingBucket { label: "0-30天", from: 0, to: Some(30) },
        AgingBucket { label: "31-60天", from: 31, to: Some(60) },
        AgingBucket { label: "61-90天", from: 61, to: Some(90) },
        AgingBucket { label: "91-180天", from: 91, to: Some(180) },
        AgingBucket { label: "181-365天", from: 181, to: Some(365) },
        AgingBucket { label: "365天以上", from: 366, to: None },
    ]
}

/// 按年分档（坏账准备计提用的口径）
pub fn buckets_by_year() -> Vec<AgingBucket> {
    vec![
        AgingBucket { label: "1年以内", from: 0, to: Some(365) },
        AgingBucket { label: "1-2年", from: 366, to: Some(730) },
        AgingBucket { label: "2-3年", from: 731, to: Some(1095) },
        AgingBucket { label: "3-4年", from: 1096, to: Some(1460) },
        AgingBucket { label: "4-5年", from: 1461, to: Some(1825) },
        AgingBucket { label: "5年以上", from: 1826, to: None },
    ]
}

/// 默认坏账计提比例（与 `buckets_by_year` 对应）
///
/// 企业会计准则不强制比例，这里给一套常见做法，UI 上应允许改。
pub fn default_bad_debt_rates() -> Vec<Money> {
    ["0.05", "0.10", "0.20", "0.30", "0.50", "1.00"]
        .iter()
        .map(|s| Money::parse(s).unwrap())
        .collect()
}

/// 一笔未结清的往来单据
#[derive(Clone, Debug)]
pub struct AgingItem {
    /// 往来单位（辅助核算 key）
    pub key: String,
    /// 单据日期
    pub date: NaiveDate,
    /// 原始金额（带符号：应收为正、应付为正统一用正数，方向由调用方保证）
    pub amount: Money,
    /// 已核销金额
    pub settled: Money,
    /// 单据号，展示用
    pub doc_no: String,
}

impl AgingItem {
    /// 未核销余额
    pub fn open(&self) -> Money {
        let o = self.amount - self.settled;
        if o.abs() < Money::parse("0.005").unwrap() {
            Money::ZERO
        } else {
            o
        }
    }
    pub fn is_open(&self) -> bool {
        !self.open().is_zero()
    }
}

/// 一个往来单位一行
#[derive(Clone, Debug)]
pub struct AgingLine {
    pub key: String,
    /// 各档金额（与 buckets 一一对应）
    pub amounts: Vec<Money>,
    /// 合计（借方性质的未核销额）
    pub total: Money,
    /// 贷方性质的未核销额（预收/预付），单独列示
    pub credit_total: Money,
    /// 最老一笔单据的账龄天数
    pub max_days: i64,
}

impl AgingLine {
    pub fn net(&self) -> Money {
        self.total - self.credit_total
    }
}

/// 账龄分析
///
/// `items` 是全部往来单据（已核销的也要传进来，靠 `settled` 抵扣）。
pub fn analyze(
    items: &[AgingItem],
    as_of: NaiveDate,
    buckets: &[AgingBucket],
) -> Result<Vec<AgingLine>, FinError> {
    let mut map: std::collections::BTreeMap<String, AgingLine> = std::collections::BTreeMap::new();
    for it in items {
        let open = it.open();
        if open.is_zero() {
            continue;
        }
        let line = map
            .entry(it.key.clone())
            .or_insert_with(|| AgingLine {
                key: it.key.clone(),
                amounts: vec![Money::ZERO; buckets.len()],
                total: Money::ZERO,
                credit_total: Money::ZERO,
                max_days: 0,
            });
        // 未来日期的单据（days 为负）按 0 天处理，不能落进"最旧"档虚增长账龄
        let days = (as_of - it.date).num_days().max(0);
        if days > line.max_days {
            line.max_days = days;
        }
        if open.is_negative() {
            line.credit_total += open.abs();
            continue;
        }
        let d = days.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        let idx = buckets.iter().position(|b| b.contains(d));
        match idx {
            Some(i) if i < line.amounts.len() => line.amounts[i] += open,
            _ => {
                // 落在最后一档之外（理论上最后一个桶是 None 兜底）
                if let Some(last) = line.amounts.last_mut() {
                    *last += open;
                }
            }
        }
        line.total += open;
    }
    let mut out: Vec<AgingLine> = map
        .into_values()
        .filter(|l| !l.total.is_zero() || !l.credit_total.is_zero())
        .collect();
    out.sort_by_key(|l| std::cmp::Reverse(l.total));
    Ok(out)
}

/// 按坏账计提比例算应计提的坏账准备
pub fn bad_debt_provision(lines: &[AgingLine], rates: &[Money]) -> Money {
    let mut sum = Money::ZERO;
    for l in lines {
        for (i, a) in l.amounts.iter().enumerate() {
            let r = rates.get(i).copied().unwrap_or(Money::ZERO);
            sum += (*a * r).round2();
        }
    }
    sum.round2()
}

/// 列合计（用于表尾）
pub fn column_totals(lines: &[AgingLine], buckets: &[AgingBucket]) -> Vec<Money> {
    let mut v = vec![Money::ZERO; buckets.len()];
    for l in lines {
        for (i, a) in l.amounts.iter().enumerate() {
            if i < v.len() {
                v[i] += *a;
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, dd: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, dd).unwrap()
    }
    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    fn item(key: &str, date: NaiveDate, amount: &str, settled: &str) -> AgingItem {
        AgingItem {
            key: key.into(),
            date,
            amount: m(amount),
            settled: m(settled),
            doc_no: String::new(),
        }
    }

    #[test]
    fn buckets_partition() {
        let b = buckets_by_days();
        assert!(b[0].contains(0));
        assert!(b[0].contains(30));
        assert!(b[1].contains(31));
        assert!(b[5].contains(400));
        assert!(!b[0].contains(31));
    }

    #[test]
    fn analyze_groups_by_counterparty() {
        let as_of = d(2026, 3, 31);
        let items = vec![
            item("C01", d(2026, 3, 20), "1000", "0"), // 11 天 → 0-30
            item("C01", d(2025, 12, 1), "500", "0"),  // 120 天 → 91-180
            item("C02", d(2025, 1, 1), "800", "0"),   // 454 天 → 365以上
        ];
        let b = buckets_by_days();
        let lines = analyze(&items, as_of, &b).unwrap();
        assert_eq!(lines.len(), 2);
        let c01 = lines.iter().find(|l| l.key == "C01").unwrap();
        assert_eq!(c01.amounts[0], m("1000"));
        assert_eq!(c01.amounts[3], m("500")); // 91-180
        assert_eq!(c01.total, m("1500"));
        let c02 = lines.iter().find(|l| l.key == "C02").unwrap();
        assert_eq!(c02.amounts[5], m("800"));
        assert_eq!(c02.max_days, 454);
    }

    #[test]
    fn settled_reduces_open() {
        let as_of = d(2026, 3, 31);
        let items = vec![
            item("C01", d(2026, 3, 1), "1000", "1000"), // 已结清 → 不出现
            item("C01", d(2026, 3, 5), "600", "100"),   // 余额 500
        ];
        let lines = analyze(&items, as_of, &buckets_by_days()).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].total, m("500"));
    }

    #[test]
    fn credit_balance_shown_separately() {
        let as_of = d(2026, 3, 31);
        let items = vec![
            item("C01", d(2026, 3, 5), "1000", "0"),
            item("C01", d(2026, 3, 10), "-1500", "0"), // 预收 > 应收
        ];
        let lines = analyze(&items, as_of, &buckets_by_days()).unwrap();
        assert_eq!(lines[0].total, m("1000"));
        assert_eq!(lines[0].credit_total, m("1500"));
        assert_eq!(lines[0].net(), m("-500"));
    }

    #[test]
    fn bad_debt_uses_rates() {
        let as_of = d(2026, 3, 31);
        let items = vec![
            item("C01", d(2026, 3, 20), "1000", "0"),
            item("C01", d(2024, 1, 1), "2000", "0"),
        ];
        let by = buckets_by_year();
        let lines = analyze(&items, as_of, &by).unwrap();
        // 1000 → 1年以内 5%；2000 → 1-2年? 2024-01-01 到 2026-03-31 = 820 天 → 2-3年? 
        // 820 天落在 731~1095 → 索引 2（2-3年），比例 20%
        assert_eq!(lines[0].amounts[0], m("1000"));
        assert_eq!(lines[0].amounts[2], m("2000"));
        let prov = bad_debt_provision(&lines, &default_bad_debt_rates());
        assert_eq!(prov, m("450")); // 1000*0.05 + 2000*0.20
        let totals = column_totals(&lines, &by);
        assert_eq!(totals[0], m("1000"));
    }
}
