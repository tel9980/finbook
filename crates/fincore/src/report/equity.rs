//! 所有者权益变动表
//!
//! 依据《企业会计准则第30号——财务报表列报》的一般企业格式（简式）：
//! 按「实收资本 / 资本公积 / 盈余公积 / 未分配利润」四栏，逐项列示
//! 本年年初余额、本年增减变动、本年年末余额。
//!
//! 金额全部带符号（正=贷方余额增加，负=借方），由取数方决定最终显示方向。

use serde::{Deserialize, Serialize};

use crate::money::Money;

/// 权益项目编码（企业内部键，与科目编码对应）
pub const CAPITAL: &str = "4001"; // 实收资本
pub const RESERVE: &str = "4002"; // 资本公积
pub const SURPLUS: &str = "4101"; // 盈余公积
pub const UNDISTRIBUTED: &str = "410401"; // 未分配利润

/// 一行权益变动
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EquityLine {
    /// 项目名称（实收资本 / 资本公积 / …）
    pub name: String,
    /// 本年年初余额（贷方为正）
    pub begin: Money,
    /// 本年增减变动（贷方增加为正、借方减少为负）
    pub change: Money,
    /// 本年年末余额（贷方为正）
    pub end: Money,
}

/// 所有者权益变动表
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EquityStatement {
    /// 各权益项目行
    pub lines: Vec<EquityLine>,
    /// 所有者权益合计
    pub total: EquityLine,
}

impl EquityStatement {
    /// 构建：`inputs` 为 (项目名, 年初, 本年增减, 年末)，顺序即展示顺序
    pub fn build(inputs: &[(String, Money, Money, Money)]) -> Self {
        let mut lines = Vec::with_capacity(inputs.len());
        let mut t_begin = Money::ZERO;
        let mut t_change = Money::ZERO;
        let mut t_end = Money::ZERO;
        for (name, b, c, e) in inputs {
            lines.push(EquityLine {
                name: name.clone(),
                begin: *b,
                change: *c,
                end: *e,
            });
            t_begin += *b;
            t_change += *c;
            t_end += *e;
        }
        Self {
            lines,
            total: EquityLine {
                name: "所有者权益合计".to_string(),
                begin: t_begin,
                change: t_change,
                end: t_end,
            },
        }
    }

    /// 勾稽：年初 + 本年增减 == 年末
    pub fn ties(&self) -> bool {
        (self.total.begin + self.total.change - self.total.end)
            .round2()
            .is_zero()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    #[test]
    fn build_and_tie() {
        // 实收资本 1000 万，本年无变动；未分配利润年初 200 万，本年 +50 万
        let stmt = EquityStatement::build(&[
            ("实收资本".into(), m("10000000"), m("0"), m("10000000")),
            ("资本公积".into(), m("500000"), m("0"), m("500000")),
            ("盈余公积".into(), m("100000"), m("50000"), m("150000")),
            ("未分配利润".into(), m("2000000"), m("500000"), m("2500000")),
        ]);
        assert_eq!(stmt.lines.len(), 4);
        assert_eq!(stmt.total.begin, m("12600000"));
        assert_eq!(stmt.total.change, m("550000"));
        assert_eq!(stmt.total.end, m("13150000"));
        assert!(stmt.ties());
    }

    #[test]
    fn build_empty() {
        let stmt = EquityStatement::build(&[]);
        assert!(stmt.lines.is_empty());
        assert_eq!(stmt.total.begin, Money::ZERO);
        assert!(stmt.ties());
    }
}
