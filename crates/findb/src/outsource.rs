//! 委外加工的三种票：13% 专票 / 3% 征收率 / 无票。
//!
//! 加工厂把钱付给外面的喷砂、拉丝、抛光、电镀厂，能拿到的票就这三种。
//! 三种票的**税后实际成本完全不同**，但账面「付了多少钱」是同一个数 ——
//! 这是新会计最容易漏的一层：付 10,000 元，
//!
//! | 票种 | 不含税成本 | 可抵进项 | 真实现金成本 |
//! |------|-----------|---------|-------------|
//! | 13% 专票 | 8,849.56 | 1,150.44 | **8,849.56** |
//! | 3% 征收率 | 9,708.74 | 291.26 | **9,708.74** |
//! | 无票 | 10,000.00 | 0 | **10,000.00** |
//!
//! 所以「要不要票」不是合规问题，先是算术问题：同一笔加工费，
//! 13% 专票比无票少花 1,150.44 元。而**无票的 10,000 元连企业所得税前都不能扣**，
//! 真正差的比这张表还多 25%。
//!
//! 本模块只做算术和口径判定，不落库、不出凭证：出凭证在
//! [`crate::manufacturing::outsource_fee_with_ticket`]，因为凭证要挂订单和供应商辅助。

use fincore::Money;

/// 委外加工费的三种票
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ticket {
    /// 一般纳税人开的 13% 专用发票，进项可全额抵扣
    Special13,
    /// 小规模纳税人 / 简易计税项目开的 3% 征收率发票
    Rate3,
    /// 没票（现金付、对方不开票）
    Plain,
}

impl Ticket {
    /// 前端/接口用的短码
    pub fn key(&self) -> &'static str {
        match self {
            Ticket::Special13 => "13",
            Ticket::Rate3 => "3",
            Ticket::Plain => "none",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Ticket::Special13 => "13% 专用发票",
            Ticket::Rate3 => "3% 征收率发票",
            Ticket::Plain => "无票",
        }
    }

    /// 税率字符串（无票为空）
    pub fn rate(&self) -> &'static str {
        match self {
            Ticket::Special13 => "0.13",
            Ticket::Rate3 => "0.03",
            Ticket::Plain => "",
        }
    }

    /// 由短码解析。宽松兜底到「无票」是不行的 —— 少抵的税是真金白银，
    /// 所以拼错必须报错，让人显式选。
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim() {
            "13" | "0.13" | "special13" => Ok(Ticket::Special13),
            "3" | "0.03" | "rate3" => Ok(Ticket::Rate3),
            "none" | "" | "plain" | "no" => Ok(Ticket::Plain),
            other => Err(format!(
                "不认识的委外票种「{other}」，只支持 13（专用发票）/ 3（征收率）/ none（无票）"
            )),
        }
    }
}

/// 一次「票种 × 含税金额」的拆解结果
#[derive(Clone, Debug)]
pub struct Split {
    pub ticket: Ticket,
    /// 付出去的钱（价税合计 / 无票时即全部）
    pub gross: Money,
    /// 不含税金额：进成本的数
    pub net: Money,
    /// 可抵扣的进项税额
    pub tax: Money,
    /// 一个提示：这张票还能不能抵、要不要补票
    pub note: String,
}

impl Split {
    /// 现金成本 = 不含税金额（进项能抵，所以不构成成本）
    pub fn real_cost(&self) -> Money {
        self.net
    }
}

/// 拆分一张委外加工费发票。
///
/// 含税金额除以 `1 + 税率`，税额用减法反推 —— 与账套里所有含税拆分
/// （`split()` 系列）保持同一口径：**税额 = 含税 − 不含税**，
/// 除出来的分位四舍五入到不含税金额上，保证 不含税 + 税额 永远等于含税。
pub fn split(ticket: Ticket, gross: Money) -> Result<Split, String> {
    if gross.is_negative() {
        return Err("委外加工费不能是负数（红字冲销请用红字凭证，不要用负数走模板）".into());
    }
    let rate = ticket.rate();
    let (net, tax) = if rate.is_empty() {
        (gross.round2(), Money::ZERO)
    } else {
        let divisor = Money::parse(rate)
            .ok()
            .and_then(|r| Money::parse("1").ok().and_then(|one| one.checked_add(r)))
            .ok_or_else(|| format!("税率 {rate} 无法解析"))?;
        let n = gross
            .checked_div(divisor)
            .ok_or_else(|| "含税金额除税率失败".to_string())?
            .round2();
        (n, (gross - n).round2())
    };
    let note = match ticket {
        Ticket::Special13 => "进项可全额抵扣，成本按不含税金额进。".to_string(),
        Ticket::Rate3 => {
            "只抵 3%。注意：一般纳税人如果开 3%，说明这笔业务是简易计税或对方是小规模 —— \
             别指望按 13% 抵。"
                .to_string()
        }
        Ticket::Plain => "没有进项可抵，而且这笔支出企业所得税前不能扣除（要纳税调增）。\
             让对方开票通常比省下的税划算：同一笔钱，13% 专票比无票少花一截。"
            .to_string(),
    };
    Ok(Split {
        ticket,
        gross: gross.round2(),
        net,
        tax,
        note,
    })
}

/// 三种票并排比一笔金额：给「要不要票」一个能拿去跟老板谈的数。
///
/// 返回顺序固定 13% → 3% → 无票，方便界面直接铺表。
pub fn compare(gross: Money) -> Result<Vec<Split>, String> {
    [Ticket::Special13, Ticket::Rate3, Ticket::Plain]
        .into_iter()
        .map(|t| split(t, gross))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    /// 一万块钱的三种票：这是本模块存在的理由，数字必须钉死
    #[test]
    fn ten_thousand_three_tickets() {
        let rows = compare(m("10000")).unwrap();
        assert_eq!(rows[0].net.fmt_money(), "8,849.56");
        assert_eq!(rows[0].tax.fmt_money(), "1,150.44");
        assert_eq!(rows[1].net.fmt_money(), "9,708.74");
        assert_eq!(rows[1].tax.fmt_money(), "291.26");
        assert_eq!(rows[2].net.fmt_money(), "10,000.00");
        assert_eq!(rows[2].tax.fmt_money(), "0.00");
        // 不含税 + 税额 必须回到含税，一分不差
        for r in &rows {
            assert_eq!(
                (r.net + r.tax).fmt_money(),
                "10,000.00",
                "{}",
                r.ticket.label()
            );
        }
        // 「要不要票」的价差
        assert_eq!(
            (rows[2].net - rows[0].net).fmt_money(),
            "1,150.44",
            "无票比 13% 专票多花的钱"
        );
    }

    /// 除不尽的时候（10000 / 1.13 = 8849.5575…）不能出现「不含税 + 税 ≠ 含税」
    #[test]
    fn rounding_never_loses_a_cent() {
        for gross in ["1", "99.99", "1234.56", "0.01", "123456.78"] {
            let g = m(gross);
            for t in [Ticket::Special13, Ticket::Rate3, Ticket::Plain] {
                let s = split(t, g).unwrap();
                assert_eq!(
                    (s.net + s.tax).fmt_money(),
                    g.round2().fmt_money(),
                    "{gross} / {}",
                    t.label()
                );
            }
        }
    }

    /// 拼错的票种必须报错，不能悄悄按「无票」处理
    #[test]
    fn unknown_ticket_is_rejected() {
        let e = Ticket::parse("9").unwrap_err();
        assert!(e.contains("9"), "要把错的输入带出来：{e}");
        assert_eq!(Ticket::parse("13").unwrap(), Ticket::Special13);
        assert_eq!(Ticket::parse("none").unwrap(), Ticket::Plain);
        // 空串当无票：老接口不传 ticket 时就是「没票」，这是唯一安全的默认
        assert_eq!(Ticket::parse("").unwrap(), Ticket::Plain);
    }

    /// 负数直接拒：红字要用红字凭证，用负数走模板会把进项抵成负的
    #[test]
    fn negative_is_rejected() {
        assert!(split(Ticket::Special13, m("-100")).is_err());
    }
}
