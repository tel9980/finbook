//! 现金流量项目的「按对方科目归集」规则
//!
//! 现金流量表是直接法：一笔现金收支该归到哪个项目，本该由录凭证的人标。但实际
//! 情况是——没人标。小厂的会计是老板娘，她能照着教程把借贷写对，不会记得给
//! 「100201 借方」再点一个现金流量项目。结果就是报表勾稽得上、内容是一片
//! 「未归集」，老板看到一张等于没用的表。
//!
//! 这一层就是补这个口子：**按对方科目推断**。凭证上「借 100201 / 贷 600102」
//! 里的 `600102` 就是对方科目，它天然说明了这笔钱是什么性质——收加工费。
//!
//! 三条规矩：
//!
//! 1. **从不覆盖人工标注**。凭证上标了就用标的（调用方负责优先）。
//! 2. **只在能确定时猜**。一张凭证的对方科目指向两个不同的项目，就不猜，交回
//!    「未归集」并在界面上报出来——猜错的现金流量表比空表更糟。
//! 3. **内部划转不是现金流量**。提现（借 1001 / 贷 100201）的对方科目也是货币
//!    资金，这条规则表对它一律返回 `None`；是否「整条跳过」由调用方判断。
//!
//! 规则用**最长前缀匹配**，所以「660301 利息支出」能比「6603 财务费用」更具体
//! 地归到筹资活动（0304 偿付利息），不用为此改结构。

use super::cashflow::CashFlowDirection;

/// 一条归集规则：科目编码前缀 → 流入/流出时分别归到哪个现金流量项目
///
/// 一个前缀可以有方向不同的两个项目：「2221 应交税费」收到退税算经营活动流入
/// 0102，交税算流出 0106。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CfRule {
    /// 科目编码前缀（至少两位；末级科目也能直接写满 8 位）
    pub prefix: &'static str,
    /// 现金**流入**（现金科目在借方）时归到哪个项目
    pub inflow: Option<&'static str>,
    /// 现金**流出**（现金科目在贷方）时归到哪个项目
    pub outflow: Option<&'static str>,
}

const fn rule(
    prefix: &'static str,
    inflow: Option<&'static str>,
    outflow: Option<&'static str>,
) -> CfRule {
    CfRule {
        prefix,
        inflow,
        outflow,
    }
}

/// 内置归集规则（对应 `fincore::chart::default_accounts()` 的标准科目表）
///
/// 项目编码含义见 `super::cashflow::default_cash_flow_items()`：
/// `0101` 销售商品收到的现金 / `0102` 收到的税费返还 / `0103` 收到其他经营现金 /
/// `0104` 购买商品接受劳务支付的现金 / `0105` 支付职工薪酬 / `0106` 支付的各项税费 /
/// `0107` 支付其他经营现金 / `0201` 收回投资 / `0202` 取得投资收益 /
/// `0203` 处置长期资产收回的现金 / `0204` 购建长期资产支付的现金 /
/// `0205` 投资支付的现金 / `0301` 吸收投资 / `0302` 取得借款 /
/// `0303` 偿还债务 / `0304` 分配股利利润或偿付利息 / `0305` 支付其他筹资现金。
pub fn default_rules() -> &'static [CfRule] {
    static RULES: &[CfRule] = &[
        // ---------------- 经营活动：销售 / 采购 ----------------
        rule("6001", Some("0101"), Some("0101")), // 主营业务收入
        rule("6051", Some("0101"), Some("0101")), // 其他业务收入
        rule("1122", Some("0101"), Some("0101")), // 应收账款（收货款 / 退货款）
        rule("1121", Some("0101"), Some("0101")), // 应收票据
        rule("2203", Some("0101"), Some("0101")), // 预收账款
        rule("1401", Some("0104"), Some("0104")), // 材料采购
        rule("1402", Some("0104"), Some("0104")), // 在途物资
        rule("1403", Some("0104"), Some("0104")), // 原材料
        rule("1405", Some("0104"), Some("0104")), // 库存商品
        rule("1406", Some("0104"), Some("0104")), // 发出商品
        rule("1408", Some("0104"), Some("0104")), // 委托加工物资
        rule("1411", Some("0104"), Some("0104")), // 周转材料
        rule("1123", Some("0104"), Some("0104")), // 预付账款
        rule("2202", Some("0104"), Some("0104")), // 应付账款
        rule("2201", Some("0104"), Some("0104")), // 应付票据
        rule("5001", Some("0104"), Some("0104")), // 生产成本（直接付现的料/工/费）
        rule("5101", Some("0104"), Some("0104")), // 制造费用（水电、外协、房租）
        rule("5201", Some("0104"), Some("0104")), // 劳务成本
        // ---------------- 经营活动：职工 / 税费 / 其他 ----------------
        rule("2211", Some("0105"), Some("0105")), // 应付职工薪酬
        rule("2221", Some("0102"), Some("0106")), // 应交税费（退税 / 交税）
        rule("6403", Some("0106"), Some("0106")), // 税金及附加
        rule("6801", Some("0106"), Some("0106")), // 所得税费用
        rule("6301", Some("0103"), Some("0103")), // 营业外收入
        rule("6601", Some("0103"), Some("0107")), // 销售费用
        rule("6602", Some("0103"), Some("0107")), // 管理费用
        rule("6603", Some("0103"), Some("0107")), // 财务费用
        rule("660301", None, Some("0304")),       // 财务费用-利息支出 → 筹资
        rule("660302", Some("0103"), None),       // 财务费用-利息收入 → 经营流入
        rule("6711", Some("0107"), Some("0107")), // 营业外支出
        rule("1221", Some("0103"), Some("0107")), // 其他应收款（收/退押金、代垫）
        rule("2241", Some("0103"), Some("0107")), // 其他应付款
        // ---------------- 投资活动 ----------------
        rule("1601", Some("0203"), Some("0204")), // 固定资产
        rule("1604", Some("0203"), Some("0204")), // 在建工程
        rule("1605", Some("0203"), Some("0204")), // 工程物资
        rule("1606", Some("0203"), Some("0204")), // 固定资产清理
        rule("1701", Some("0203"), Some("0204")), // 无形资产
        rule("1801", Some("0203"), Some("0204")), // 长期待摊费用
        rule("1511", Some("0201"), Some("0205")), // 长期股权投资
        rule("1101", Some("0201"), Some("0205")), // 交易性金融资产
        rule("1521", Some("0203"), Some("0204")), // 投资性房地产
        rule("6111", Some("0202"), Some("0202")), // 投资收益
        // ---------------- 筹资活动 ----------------
        rule("4001", Some("0301"), None),         // 实收资本
        rule("4002", Some("0301"), None),         // 资本公积
        rule("2001", Some("0302"), Some("0303")), // 短期借款
        rule("2501", Some("0302"), Some("0303")), // 长期借款
        rule("2701", Some("0302"), Some("0303")), // 长期应付款
        rule("2231", None, Some("0304")),         // 应付利息
        rule("2232", None, Some("0304")),         // 应付股利
        rule("4104", None, Some("0304")),         // 利润分配
        rule("4201", None, Some("0305")),         // 库存股（回购）
    ];
    RULES
}

/// 按对方科目推断现金流量项目
///
/// `code` 是**对方科目**（不是现金科目本身）；`dir` 是现金的收付方向。
/// 返回 `None` 表示这条规则表不敢判断——要么对方科目不是现金流量科目
/// （累计折旧、坏账准备这类非现金科目），要么这个方向没有约定。
pub fn item_for(code: &str, dir: CashFlowDirection) -> Option<&'static str> {
    item_for_in(default_rules(), code, dir)
}

/// 同 [`item_for`]，但可以指定规则表（测试与将来的「自定义规则」用）
///
/// 最长前缀优先；最长前缀没给这个方向的项目时，继续退到更短的前缀上找，
/// 这样「660301 利息支出」只管流出、流入仍可退回「6603 财务费用」的约定。
pub fn item_for_in(
    rules: &'static [CfRule],
    code: &str,
    dir: CashFlowDirection,
) -> Option<&'static str> {
    let code = code.trim();
    if code.is_empty() {
        return None;
    }
    // 把命中的规则按前缀长度降序排好，从最长的一条开始找这个方向的项目
    let mut candidates: Vec<&CfRule> = rules
        .iter()
        .filter(|r| code.starts_with(r.prefix))
        .collect();
    candidates.sort_by_key(|r| std::cmp::Reverse(r.prefix.len()));
    for r in candidates {
        let hit = match dir {
            CashFlowDirection::In => r.inflow,
            CashFlowDirection::Out => r.outflow,
        };
        if hit.is_some() {
            return hit;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::cashflow::{default_cash_flow_items, CashFlowDirection::*};

    #[test]
    fn sales_of_services_is_operating_inflow() {
        assert_eq!(item_for("600102", In), Some("0101"));
        assert_eq!(item_for("600101", In), Some("0101"));
        assert_eq!(item_for("6001", In), Some("0101"));
    }

    #[test]
    fn buying_material_is_operating_outflow() {
        assert_eq!(item_for("140302", Out), Some("0104"));
        assert_eq!(item_for("140501", Out), Some("0104"));
        assert_eq!(item_for("220201", Out), Some("0104"));
        assert_eq!(item_for("510105", Out), Some("0104"));
    }

    #[test]
    fn tax_payable_goes_by_direction() {
        // 收到退税 → 0102；交税 → 0106。同一科目两个方向不同项目。
        assert_eq!(item_for("2221", In), Some("0102"));
        assert_eq!(item_for("2221", Out), Some("0106"));
        assert_eq!(item_for("22210103", Out), Some("0106"));
    }

    #[test]
    fn longer_prefix_wins_over_shorter() {
        // 6603 财务费用整体是 0107，但利息支出（660301）属于筹资活动 0304
        assert_eq!(item_for("660301", Out), Some("0304"));
        assert_eq!(item_for("660305", Out), Some("0107"));
        // 利息支出没有约定流入项目，退回 6603 的「0103」
        assert_eq!(item_for("660301", In), Some("0103"));
    }

    #[test]
    fn cash_and_non_cash_accounts_get_no_rule() {
        // 货币资金之间划转不是现金流量；累计折旧这类非现金科目也不该有规则
        assert_eq!(item_for("1001", In), None);
        assert_eq!(item_for("100201", Out), None);
        assert_eq!(item_for("1602", Out), None);
        assert_eq!(item_for("1231", Out), None);
        assert_eq!(item_for("", In), None);
    }

    #[test]
    fn investing_and_financing_sides() {
        assert_eq!(item_for("160101", Out), Some("0204")); // 买设备
        assert_eq!(item_for("160101", In), Some("0203")); // 卖设备
        assert_eq!(item_for("4001", In), Some("0301")); // 实收资本
        assert_eq!(item_for("2001", In), Some("0302")); // 借入
        assert_eq!(item_for("2001", Out), Some("0303")); // 还本
        assert_eq!(item_for("410403", Out), Some("0304")); // 分红
    }

    /// 规则表里写的项目编码必须真的存在 —— 手打错一个字（`0101` 写成 `0110`）
    /// 的后果是这一整类现金收支静默落到「未归集」，报表上没有金额也没有报错。
    #[test]
    fn every_rule_points_at_a_real_cash_flow_item() {
        let items = default_cash_flow_items();
        for r in default_rules() {
            for code in [r.inflow, r.outflow].into_iter().flatten() {
                assert!(
                    items.iter().any(|it| it.code == code),
                    "规则 {} 指向不存在的现金流量项目 {code}",
                    r.prefix
                );
            }
            assert!(
                r.inflow.is_some() || r.outflow.is_some(),
                "规则 {} 两个方向都没配，是死规则",
                r.prefix
            );
        }
    }

    /// 前缀不能互相「吞掉」：如果两个前缀一个以另一个开头，长的那条必须写得更具体，
    /// 否则短规则的金额会被静默吃掉。这里只检查没有完全重复的前缀。
    #[test]
    fn no_duplicate_prefixes() {
        let rules = default_rules();
        for (i, a) in rules.iter().enumerate() {
            for b in &rules[i + 1..] {
                assert_ne!(a.prefix, b.prefix, "前缀 {} 重复定义", a.prefix);
            }
        }
    }

    /// 方向必须真的影响结果，否则「按方向给不同项目」这层就是假的
    #[test]
    fn direction_matters() {
        let mut differ = 0;
        for code in ["2221", "1601", "2001", "6601"] {
            if item_for(code, In) != item_for(code, Out) {
                differ += 1;
            }
        }
        assert!(differ >= 3, "至少三类科目应当按方向归到不同项目");
    }
}
