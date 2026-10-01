//! 现金流量表
//!
//! 主表采用**直接法**：凭证上现金 / 银行科目的对方科目需标注现金流量项目，
//! 这里有借（流入）贷（流出）两个方向的发生额即可直接汇总成表。
//! 若凭证未标注，则按科目的默认现金流量项目（`Account::cash_flow_item`）归集，
//! 仍无法归集的进入"其他"项目，保证主表与货币资金账面变动始终勾稽得上。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::money::Money;

/// 现金流量大类
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CashFlowGroup {
    /// 经营活动
    Operating,
    /// 投资活动
    Investing,
    /// 筹资活动
    Financing,
}

impl CashFlowGroup {
    pub fn label(self) -> &'static str {
        match self {
            CashFlowGroup::Operating => "经营活动产生的现金流量",
            CashFlowGroup::Investing => "投资活动产生的现金流量",
            CashFlowGroup::Financing => "筹资活动产生的现金流量",
        }
    }
    pub fn all() -> &'static [CashFlowGroup] {
        &[
            CashFlowGroup::Operating,
            CashFlowGroup::Investing,
            CashFlowGroup::Financing,
        ]
    }
}

/// 现金流方向
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CashFlowDirection {
    /// 流入
    In,
    /// 流出
    Out,
}

impl CashFlowDirection {
    pub fn label(self) -> &'static str {
        match self {
            CashFlowDirection::In => "流入",
            CashFlowDirection::Out => "流出",
        }
    }
}

/// 现金流量项目
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CashFlowItem {
    /// 项目编码，如 `0101`
    pub code: String,
    pub name: String,
    pub group: CashFlowGroup,
    pub dir: CashFlowDirection,
    /// 明细项目是否参与汇总
    pub disabled: bool,
}

impl CashFlowItem {
    fn new(code: &str, name: &str, group: CashFlowGroup, dir: CashFlowDirection) -> Self {
        Self {
            code: code.to_string(),
            name: name.to_string(),
            group,
            dir,
            disabled: false,
        }
    }
}

/// 现金流量表标准项目（财政部一般企业报表格式）
pub fn default_cash_flow_items() -> Vec<CashFlowItem> {
    use CashFlowDirection::{In, Out};
    use CashFlowGroup::{Financing, Investing, Operating};
    vec![
        // ---- 经营活动 ----
        CashFlowItem::new("0101", "销售商品、提供劳务收到的现金", Operating, In),
        CashFlowItem::new("0102", "收到的税费返还", Operating, In),
        CashFlowItem::new("0103", "收到其他与经营活动有关的现金", Operating, In),
        CashFlowItem::new("0104", "购买商品、接受劳务支付的现金", Operating, Out),
        CashFlowItem::new("0105", "支付给职工以及为职工支付的现金", Operating, Out),
        CashFlowItem::new("0106", "支付的各项税费", Operating, Out),
        CashFlowItem::new("0107", "支付其他与经营活动有关的现金", Operating, Out),
        // ---- 投资活动 ----
        CashFlowItem::new("0201", "收回投资收到的现金", Investing, In),
        CashFlowItem::new("0202", "取得投资收益收到的现金", Investing, In),
        CashFlowItem::new(
            "0203",
            "处置固定资产、无形资产和其他长期资产收回的现金净额",
            Investing,
            In,
        ),
        CashFlowItem::new(
            "0204",
            "购建固定资产、无形资产和其他长期资产支付的现金",
            Investing,
            Out,
        ),
        CashFlowItem::new("0205", "投资支付的现金", Investing, Out),
        CashFlowItem::new("0206", "取得子公司及其他营业单位支付的现金净额", Investing, Out),
        // ---- 筹资活动 ----
        CashFlowItem::new("0301", "吸收投资收到的现金", Financing, In),
        CashFlowItem::new("0302", "取得借款收到的现金", Financing, In),
        CashFlowItem::new("0303", "偿还债务支付的现金", Financing, Out),
        CashFlowItem::new(
            "0304",
            "分配股利、利润或偿付利息支付的现金",
            Financing,
            Out,
        ),
        CashFlowItem::new("0305", "支付其他与筹资活动有关的现金", Financing, Out),
    ]
}

/// 按项目汇总的发生额：项目编码 → (借方发生额, 贷方发生额)
/// 对现金 / 银行科目而言，借方即流入，贷方即流出。
pub type ItemAmounts = HashMap<String, (Money, Money)>;

/// 现金流量表行
#[derive(Clone, PartialEq, Debug)]
pub struct CashFlowLine {
    pub code: String,
    pub name: String,
    /// 本项目金额（流入为正、流出按正数填列在"流出"列）
    pub inflow: Money,
    pub outflow: Money,
    /// 净额（流入 - 流出）
    pub net: Money,
}

/// 现金流量表
#[derive(Clone, Debug)]
pub struct CashFlowStatement {
    pub operating: Vec<CashFlowLine>,
    pub operating_net: Money,
    pub investing: Vec<CashFlowLine>,
    pub investing_net: Money,
    pub financing: Vec<CashFlowLine>,
    pub financing_net: Money,
    /// 现金及现金等价物净增加额
    pub net_increase: Money,
    /// 期初现金及现金等价物余额
    pub begin_cash: Money,
    /// 期末现金及现金等价物余额
    pub end_cash: Money,
    /// 尚未标注现金流量项目的金额（勾稽提示用）
    pub unassigned: Money,
}

impl CashFlowStatement {
    /// 与货币资金账面变动是否勾稽一致
    pub fn ties(&self) -> bool {
        (self.net_increase - (self.end_cash - self.begin_cash))
            .round2()
            .is_zero()
    }
}

/// 生成现金流量表
///
/// `amounts` 为项目编码 → (借方, 贷方) 的汇总发生额；
/// `unassigned` 是无法归集到任何项目的现金收支净额。
pub fn build_cash_flow(
    items: &[CashFlowItem],
    amounts: &ItemAmounts,
    begin_cash: Money,
    end_cash: Money,
    unassigned: Money,
) -> CashFlowStatement {
    let mut groups: HashMap<CashFlowGroup, Vec<CashFlowLine>> = HashMap::new();

    for it in items {
        if it.disabled {
            continue;
        }
        let (dr, cr) = amounts.get(&it.code).copied().unwrap_or((Money::ZERO, Money::ZERO));
        // 保留红字冲回的符号（流入项目出现净流出时为负数），不能用零截断：
        // 截断会让表内净额与货币资金实际变动对不上，红字冲回被凭空抹掉。
        let (inflow, outflow) = match it.dir {
            CashFlowDirection::In => (dr - cr, Money::ZERO),
            CashFlowDirection::Out => (Money::ZERO, cr - dr),
        };
        groups.entry(it.group).or_default().push(CashFlowLine {
            code: it.code.clone(),
            name: it.name.clone(),
            inflow,
            outflow,
            net: inflow - outflow,
        });
    }

    // 先取走分组，再各自求净额，避免同时持有 groups 的可变与不可变借用
    let operating = groups.remove(&CashFlowGroup::Operating).unwrap_or_default();
    let investing = groups.remove(&CashFlowGroup::Investing).unwrap_or_default();
    let financing = groups.remove(&CashFlowGroup::Financing).unwrap_or_default();

    let sum_net = |v: &[CashFlowLine]| -> Money { v.iter().map(|l| l.net).sum::<Money>() };
    let operating_net = sum_net(&operating);
    let investing_net = sum_net(&investing);
    let financing_net = sum_net(&financing);

    CashFlowStatement {
        operating,
        operating_net,
        investing,
        investing_net,
        financing,
        financing_net,
        net_increase: operating_net + investing_net + financing_net + unassigned,
        begin_cash,
        end_cash,
        unassigned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_tie() {
        let items = default_cash_flow_items();
        let mut amt = ItemAmounts::new();
        amt.insert("0101".to_string(), (Money::parse("100000").unwrap(), Money::ZERO));
        amt.insert("0104".to_string(), (Money::ZERO, Money::parse("40000").unwrap()));
        amt.insert("0204".to_string(), (Money::ZERO, Money::parse("20000").unwrap()));
        amt.insert("0302".to_string(), (Money::parse("50000").unwrap(), Money::ZERO));

        let stmt = build_cash_flow(
            &items,
            &amt,
            Money::parse("10000").unwrap(),
            Money::parse("100000").unwrap(),
            Money::ZERO,
        );
        assert_eq!(stmt.operating_net, Money::parse("60000").unwrap());
        assert_eq!(stmt.investing_net, Money::parse("-20000").unwrap());
        assert_eq!(stmt.financing_net, Money::parse("50000").unwrap());
        assert_eq!(stmt.net_increase, Money::parse("90000").unwrap());
        assert!(stmt.ties());
    }

    #[test]
    fn unassigned_reported() {
        let items = default_cash_flow_items();
        let amt = ItemAmounts::new();
        let stmt = build_cash_flow(
            &items,
            &amt,
            Money::ZERO,
            Money::parse("-500").unwrap(),
            Money::parse("-500").unwrap(),
        );
        assert_eq!(stmt.unassigned, Money::parse("-500").unwrap());
        assert!(stmt.ties());
    }
}
