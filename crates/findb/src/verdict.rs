//! # 首屏「一句话结论」
//!
//! 打开软件第一眼要回答的只有一个问题：**接下来该先办哪件事**。
//!
//! 桌面端（finui 的 `Dashboard`）和网页版（finweb 的 `/api/dashboard`）都有首屏，
//! 两处各写一份判定，很快就会说成两套话——网页版让先去催款、桌面端让先去记账，
//! 同一本账两个结论，用户只会更糊涂。所以**判定只写这一份**：两端各自把数字
//! 填进 [`Facts`]，拿回同一句 [`Verdict`]。
//!
//! 优先级（顺序本身就是设计，改动前先想清楚为什么）：
//!
//! 1. **有凭证没记账** —— 没记账的凭证不计入利润，此时首屏说的「赚了多少」
//!    是偏小的；拿偏小的数去报价、发工资就会错。这比催款更紧急。
//! 2. **应收账款超 90 天没收回** —— 小厂最常见的死法不是不赚钱，是赚的钱
//!    全压在客户手里，同时房租工资还得按月付。
//! 3. **净欠外面（欠供应商多于别人欠你）** —— 不急着喊，但要提醒看现金。
//! 4. **本月还没有收入凭证** —— 多半是还没开始录，先记第一笔。
//! 5. 都没事 —— 给一句放心话 + 月底的四步。
//!
//! 这里不碰数据库：判定只吃数字。取数口径留在两端各自的 `home_brief` /
//! `Dashboard::reload`，因为它们的权限与数据范围口径本来就不同。

use fincore::{Money, TrialBalance};

/// 结论类型。
///
/// 界面靠它决定颜色和「点进去去哪一页」，**不要**用文字匹配判断——文案会改，
/// 类型不会。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerdictKind {
    /// 有凭证没记账，先记账
    Unposted,
    /// 应收账款超 90 天，先催款
    Overdue,
    /// 净欠外面，留意付款节奏
    NetDebt,
    /// 本月还没有收入凭证
    NoRevenue,
    /// 一切正常
    AllGood,
}

impl VerdictKind {
    /// 稳定的机器标识，用于前端配色与跳转映射（不是文案）
    pub fn key(self) -> &'static str {
        match self {
            VerdictKind::Unposted => "unposted",
            VerdictKind::Overdue => "overdue",
            VerdictKind::NetDebt => "net_debt",
            VerdictKind::NoRevenue => "no_revenue",
            VerdictKind::AllGood => "ok",
        }
    }

    /// 这一句指向的页面（网页版的视图 id）。空串表示不用跳。
    pub fn view(self) -> &'static str {
        match self {
            VerdictKind::Unposted | VerdictKind::NoRevenue => "vouchers",
            VerdictKind::Overdue | VerdictKind::NetDebt => "settle",
            VerdictKind::AllGood => "",
        }
    }

    /// 是不是「要马上处理」的那一类（界面据此决定用警示色还是正常色）
    pub fn is_urgent(self) -> bool {
        matches!(self, VerdictKind::Unposted | VerdictKind::Overdue)
    }
}

/// 判定所需的事实。
///
/// 字段都是首屏本来就要算的数，不额外查库。
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    /// 本期净利润（已记账部分）
    pub net_profit: Money,
    /// 本期营业收入
    pub revenue: Money,
    /// 未记账凭证张数（草稿 + 已审核未记账）
    pub unposted: i64,
    /// 超 90 天未收回的应收合计
    pub overdue_90: Money,
    /// 超 90 天未收回的客户家数
    pub overdue_90_count: usize,
    /// 应付合计
    pub payable: Money,
    /// 手头现金（银行存款 + 现金）
    pub cash: Money,
    /// 净头寸 = 应收 + 预付 − 应付 − 预收；负数说明净欠外面
    pub net: Money,
}

/// 一句话结论：给老板看的原话 + 机器识别用的类型
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    /// 结论类型
    pub kind: VerdictKind,
    /// 屏幕上原样显示的那句话
    pub text: String,
}

impl Verdict {
    /// 结论类型的稳定标识
    pub fn key(&self) -> &'static str {
        self.kind.key()
    }

    /// 点这句话该去哪一页
    pub fn view(&self) -> &'static str {
        self.kind.view()
    }
}

/// 按优先级挑出**最该先处理的那一件**，说成一句人话。
///
/// 一次只说一件：同时列五条等于没结论，老板看完还是不知道该先动哪个。
pub fn verdict(f: &Facts) -> Verdict {
    let np = f.net_profit.fmt_money();
    if f.unposted > 0 {
        return Verdict {
            kind: VerdictKind::Unposted,
            text: format!(
                "⚠ 还有 {} 张凭证没记账，下面的 {np} 是「已记账部分」的利润，可能偏小。\
                 去「凭证查询」勾选后批量记账，数字才是完整的。",
                f.unposted
            ),
        };
    }
    if f.overdue_90_count > 0 {
        return Verdict {
            kind: VerdictKind::Overdue,
            text: format!(
                "⚠ 账面这个月{}，但 {} 家客户的钱超过 90 天没收回、共 {}。\
                 小厂最常见的死法就是赚的钱全压在客户手里 —— 先去催这几笔。",
                if f.net_profit.is_negative() {
                    "是亏的"
                } else {
                    "是赚的"
                },
                f.overdue_90_count,
                f.overdue_90.fmt_money()
            ),
        };
    }
    if !f.net.is_zero() && f.net.is_negative() && !f.payable.is_zero() {
        return Verdict {
            kind: VerdictKind::NetDebt,
            text: format!(
                "这个月净利润 {np}，但净欠外面 {}（欠供应商多于别人欠你），\
                 手头现金 {}，留意付款节奏。",
                f.net.abs().fmt_money(),
                f.cash.fmt_money()
            ),
        };
    }
    if f.revenue.is_zero() {
        return Verdict {
            kind: VerdictKind::NoRevenue,
            text: "本月还没有收入凭证 —— 是不是还没开始录？到「填制凭证」记第一笔加工费。"
                .to_string(),
        };
    }
    Verdict {
        kind: VerdictKind::AllGood,
        text: format!(
            "本月净利润 {np}，收支正常，暂无卡住的事。月底记得：记账 → 结转损益 → 结账 → 备份。"
        ),
    }
}

/// 「账有没有记错」的结论
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrialVerdict {
    /// 借贷是否平（期初 / 本期 / 期末三期都平才算）
    pub balanced: bool,
    /// 第一处不平的差额（借 − 贷，带符号）；平的时候是 0
    pub diff: Money,
}

/// 把试算平衡的六个数变成一句说得出口的结论。
///
/// 差额报「第一处不平的地方」：期末不平报期末差；期末平但本期不平报本期差
/// （期初有反方向差额时会被抵消掉）；三期都平才算真平。
/// 只看期末是不够的——本期记错了、期初又恰好反向，期末看起来就是平的。
pub fn trial(tb: &TrialBalance) -> TrialVerdict {
    let end_diff = (tb.end_debit - tb.end_credit).round2();
    let period_diff = (tb.period_debit - tb.period_credit).round2();
    let begin_diff = (tb.begin_debit - tb.begin_credit).round2();
    let diff = if !end_diff.is_zero() {
        end_diff
    } else if !period_diff.is_zero() {
        period_diff
    } else {
        begin_diff
    };
    TrialVerdict {
        balanced: tb.is_balanced(),
        diff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(s: &str) -> Money {
        Money::parse(s).expect("测试金额必须能解析")
    }

    /// 一份「正常赚钱、没人欠、不欠人」的事实，各用例只改自己关心的字段
    fn ok_facts() -> Facts {
        Facts {
            net_profit: m("8000"),
            revenue: m("120000"),
            unposted: 0,
            overdue_90: Money::ZERO,
            overdue_90_count: 0,
            payable: m("30000"),
            cash: m("50000"),
            net: m("20000"),
        }
    }

    #[test]
    fn 小事都正常时给一句放心话() {
        let v = verdict(&ok_facts());
        assert_eq!(v.kind, VerdictKind::AllGood);
        assert_eq!(v.key(), "ok");
        assert!(v.text.contains("收支正常"));
        assert!(!v.kind.is_urgent());
    }

    #[test]
    fn 有凭证没记账排在最前面() {
        // 三件事同时成立：没记账 + 超 90 天应收 + 本月没收入
        let mut f = ok_facts();
        f.unposted = 3;
        f.overdue_90_count = 2;
        f.overdue_90 = m("45000");
        f.revenue = Money::ZERO;
        let v = verdict(&f);
        assert_eq!(v.kind, VerdictKind::Unposted);
        assert!(v.text.contains("还有 3 张凭证没记账"));
        assert!(v.text.contains("8,000.00"), "要把当前利润带上：{}", v.text);
        assert_eq!(v.view(), "vouchers");
        assert!(v.kind.is_urgent());
    }

    #[test]
    fn 记完账之后才轮到催款() {
        let mut f = ok_facts();
        f.overdue_90_count = 2;
        f.overdue_90 = m("45000");
        let v = verdict(&f);
        assert_eq!(v.kind, VerdictKind::Overdue);
        assert!(v.text.contains("2 家客户"));
        assert!(v.text.contains("45,000.00"));
        assert_eq!(v.view(), "settle");
    }

    #[test]
    fn 亏钱时催款话里说亏() {
        let mut f = ok_facts();
        f.net_profit = m("-3200");
        f.overdue_90_count = 1;
        f.overdue_90 = m("1000");
        assert!(verdict(&f).text.contains("这个月是亏的"));
    }

    #[test]
    fn 净欠外面时提醒付款节奏() {
        let mut f = ok_facts();
        f.net = m("-18000");
        f.payable = m("60000");
        f.cash = m("9000");
        let v = verdict(&f);
        assert_eq!(v.kind, VerdictKind::NetDebt);
        assert!(v.text.contains("净欠外面 18,000.00"));
        assert!(v.text.contains("9,000.00"));
        // 不催不警：黄色不带 ⚠，跳「应收应付」
        assert!(!v.text.starts_with("⚠"));
        assert_eq!(v.view(), "settle");
        assert!(!v.kind.is_urgent());
    }

    #[test]
    fn 没有应付款就不算净欠外面() {
        // net 为负但应付是 0：多半是预收，不是欠供应商，不该喊「留意付款」
        let mut f = ok_facts();
        f.net = m("-5000");
        f.payable = Money::ZERO;
        assert_eq!(verdict(&f).kind, VerdictKind::AllGood);
    }

    #[test]
    fn 本月没收入时提醒去记第一笔() {
        let mut f = ok_facts();
        f.revenue = Money::ZERO;
        f.net_profit = Money::ZERO;
        let v = verdict(&f);
        assert_eq!(v.kind, VerdictKind::NoRevenue);
        assert!(v.text.contains("本月还没有收入凭证"));
        assert_eq!(v.view(), "vouchers");
    }

    #[test]
    fn 类型标识稳定且互不相同() {
        let all = [
            VerdictKind::Unposted,
            VerdictKind::Overdue,
            VerdictKind::NetDebt,
            VerdictKind::NoRevenue,
            VerdictKind::AllGood,
        ];
        let mut keys: Vec<&str> = all.iter().map(|k| k.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all.len(), "每种结论要有自己的标识");
    }

    /// 试算平衡的三个数按「期初借/贷、本期借/贷、期末借/贷」顺序摆好
    fn tb6(v: [&str; 6]) -> TrialBalance {
        TrialBalance {
            begin_debit: m(v[0]),
            begin_credit: m(v[1]),
            period_debit: m(v[2]),
            period_credit: m(v[3]),
            end_debit: m(v[4]),
            end_credit: m(v[5]),
        }
    }

    #[test]
    fn 三期都平时说账是平的() {
        let t = trial(&tb6(["0", "0", "1000", "1000", "1000", "1000"]));
        assert!(t.balanced);
        assert!(
            t.diff.is_zero(),
            "平的账差额必须是 0：{}",
            t.diff.fmt_money()
        );
    }

    #[test]
    fn 期末不平时报期末差() {
        let t = trial(&tb6(["0", "0", "1000", "900", "1000", "900"]));
        assert!(!t.balanced);
        assert_eq!(t.diff.fmt_money(), "100.00");
    }

    #[test]
    fn 期末看着平但本期记错也不能算平() {
        // 期初借多 1000、本期贷多 1000，两边一抵期末正好平 —— 这是真错账，
        // 只看期末会放过它（顺便验一下差额报的是本期那一处）。
        let t = trial(&tb6(["1000", "0", "0", "1000", "1000", "1000"]));
        assert!(!t.balanced, "期间与期初各有差额，不能算平");
        assert_eq!(t.diff.fmt_money(), "-1,000.00", "差额应报本期那一处");
    }
}
