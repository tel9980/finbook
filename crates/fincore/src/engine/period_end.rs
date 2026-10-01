//! 期末处理：结转损益、年末结转、结账检查

use chrono::NaiveDate;

use crate::account::Chart;
use crate::balance::{BalanceRow, TrialBalance};
use crate::error::{FinError, Issues};
use crate::money::Money;
use crate::period::Period;
use crate::voucher::{Entry, Voucher, VoucherSource, VoucherStatus};

/// 常用的"本年利润"科目编码（企业会计准则：4103）
pub const PROFIT_ACCOUNT: &str = "4103";
/// 利润分配—未分配利润
pub const UNDISTRIBUTED_ACCOUNT: &str = "410401";

/// 生成结转损益凭证（账结法）
///
/// 规则：把本期所有损益类科目的 **本期净发生额** 反向结转到"本年利润"。
/// 净额为正（借方性质，通常是费用）→ 贷记该科目；净额为负（贷方性质，通常是收入）→ 借记该科目。
/// 差额落在"本年利润"。
///
/// 保留辅助核算维度，这样可以按部门 / 项目出利润表。
pub fn generate_carry_forward(
    period: Period,
    date: NaiveDate,
    word: &str,
    no: i32,
    rows: &[BalanceRow],
    chart: &Chart,
    profit_account: &str,
    user: &str,
) -> Result<Voucher, FinError> {
    if chart.get(profit_account).is_none() {
        return Err(FinError::msg(format!(
            "科目表缺少「本年利润」科目（{profit_account}），无法结转损益"
        )));
    }
    if !chart.is_leaf(profit_account) {
        return Err(FinError::msg(format!(
            "「本年利润」科目（{profit_account}）不是末级科目，无法记账"
        )));
    }

    let mut v = Voucher::new(period, date, word, no);
    v.source = VoucherSource::CarryForward;
    v.prepared_by = user.to_string();
    v.status = VoucherStatus::Draft;

    let mut net_total = Money::ZERO;
    let mut moved = 0usize;

    for r in rows {
        // 只处理损益类、末级、本期有净发生额的行
        let acct = match chart.get(&r.account_code) {
            Some(a) => a,
            None => continue,
        };
        if !acct.category.is_profit_loss() {
            continue;
        }
        if !chart.is_leaf(&r.account_code) {
            continue;
        }
        // 先取整到 2 位再入账：余额聚合可能带出分位以下的尾数，原样入账会把
        // 0.005 级的金额写进账簿，与「金额一律 2 位」的口径冲突。
        let net = (r.debit - r.credit).round2();
        if net.is_zero() {
            continue;
        }

        let mut e = Entry::new(0, r.account_code.clone(), "结转本期损益");
        e.aux = r.aux.clone();
        if net.is_positive() {
            e.credit = net;
        } else {
            e.debit = net.negated();
        }
        v.push_entry(e);
        net_total += net;
        moved += 1;
    }

    if moved == 0 {
        return Err(FinError::msg("本期没有需要结转的损益类科目（损益发生额均为零）"));
    }

    // 各结转分录的合计净额为 -net_total，因此"本年利润"需记 +net_total 才能配平。
    // net_total > 0 表示费用大于收入（亏损），本年利润记借方；反之记贷方。
    let profit = net_total;
    if !profit.round2().is_zero() {
        let mut e = Entry::new(0, profit_account.to_string(), "结转本期损益");
        if profit.is_positive() {
            e.debit = profit;
        } else {
            e.credit = profit.negated();
        }
        v.push_entry(e);
    }

    v.renumber();
    debug_assert!(v.balanced(), "结转损益凭证必须借贷平衡");
    Ok(v)
}

/// 生成年末结转凭证：本年利润 → 利润分配—未分配利润
pub fn generate_year_end_carry(
    period: Period,
    date: NaiveDate,
    word: &str,
    no: i32,
    profit_balance: Money,
    target_account: &str,
    chart: &Chart,
    user: &str,
) -> Result<Voucher, FinError> {
    if chart.get(target_account).is_none() {
        return Err(FinError::msg(format!(
            "缺少科目 {target_account}（利润分配—未分配利润），无法年末结转"
        )));
    }
    if profit_balance.round2().is_zero() {
        return Err(FinError::msg("本年利润余额为零，无需结转"));
    }

    let mut v = Voucher::new(period, date, word, no);
    v.source = VoucherSource::CarryForward;
    v.prepared_by = user.to_string();

    // profit_balance 带符号：正=借方余额（亏损），负=贷方余额（盈利）
    let mut e1 = Entry::new(
        0,
        PROFIT_ACCOUNT.to_string(),
        if profit_balance.is_positive() {
            "年末结转本年亏损"
        } else {
            "年末结转本年利润"
        },
    );
    let mut e2 = Entry::new(0, target_account.to_string(), "年末结转本年利润");
    if profit_balance.is_positive() {
        e1.credit = profit_balance;
        e2.debit = profit_balance;
    } else {
        e1.debit = profit_balance.negated();
        e2.credit = profit_balance.negated();
    }
    v.push_entry(e1);
    v.push_entry(e2);
    v.renumber();
    Ok(v)
}

/// 期末结账前检查
pub struct CloseCheckInput<'a> {
    pub period: Period,
    /// 本期未记账凭证数量
    pub unposted: usize,
    /// 本期草稿凭证数量
    pub drafts: usize,
    /// 试算平衡数据
    pub trial: &'a TrialBalance,
    /// 损益类科目余额行（用于判断是否已结转）
    pub pl_rows: &'a [BalanceRow],
    /// 是否要求结转损益后才能结账
    pub require_carry: bool,
    /// 已结账的最大期间（用于检查跨期结账）
    pub closed_upto: Option<Period>,
    /// 启用期间
    pub start_period: Period,
}

/// 返回问题清单，为空即允许结账
pub fn check_can_close(input: &CloseCheckInput) -> Issues {
    let mut iss = Issues::new();
    let p = input.period;

    // 只能对已启用期间结账
    if p < input.start_period {
        iss.push(format!(
            "{} 早于账套启用期间 {}，无需结账",
            p.label(),
            input.start_period.label()
        ));
    }

    // 必须从最早未结账期间开始，不能跳月结账
    match input.closed_upto {
        Some(upto) if p <= upto => iss.push(format!("{} 已经结账", p.label())),
        Some(upto) if upto.next() != p => iss.push(format!(
            "请先结账 {}，不能跳过中间期间直接结账 {}",
            upto.next().label(),
            p.label()
        )),
        None if p != input.start_period => iss.push(format!(
            "首次结账必须从启用期间 {} 开始",
            input.start_period.label()
        )),
        _ => {}
    }

    if input.unposted > 0 {
        iss.push(format!("本期还有 {} 张凭证未记账", input.unposted));
    }
    if input.drafts > 0 {
        iss.push(format!("本期还有 {} 张草稿凭证，请检查是否需要记账或删除", input.drafts));
    }

    // 用 is_balanced（期初 + 本期发生 + 期末）而不是只看本期发生额：
    // 只看发生额的话，期初就不平的账套（例如期初导入出错）能一路结账下去，
    // 资产负债表恒等式被永久破坏。problems() 三项都会给出来。
    if !input.trial.is_balanced() {
        for s in input.trial.problems() {
            iss.push(s);
        }
    }

    if input.require_carry {
        // 损益类科目本期净发生额应为零（已结转）
        let mut not_carried: Vec<String> = Vec::new();
        for r in input.pl_rows {
            let net = r.debit - r.credit;
            if !net.round2().is_zero() {
                not_carried.push(format!("{} {}", r.account_code, r.account_name));
            }
        }
        if !not_carried.is_empty() {
            if not_carried.len() > 5 {
                iss.push(format!(
                    "有 {} 个损益类科目尚未结转（如 {}），请先执行结转损益",
                    not_carried.len(),
                    not_carried[..5].join("、")
                ));
            } else {
                iss.push(format!(
                    "损益类科目尚未结转：{}，请先执行结转损益",
                    not_carried.join("、")
                ));
            }
        }
    }

    iss
}

/// 反结账检查：只能从最后一个已结账期间开始反
pub fn check_can_unclose(period: Period, closed_upto: Option<Period>) -> Issues {
    let mut iss = Issues::new();
    match closed_upto {
        None => iss.push("当前没有任何期间处于结账状态"),
        Some(upto) => {
            if period != upto {
                iss.push(format!("只能从最后结账的期间 {} 开始反结账", upto.label()));
            }
        }
    }
    iss
}

/// 判断某期间是否为会计年度的最后一个月
pub fn is_year_end(period: Period) -> bool {
    period.month() == 12
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Account, AcctCategory, CodeScheme};

    fn chart() -> Chart {
        let mut c = Chart::new(CodeScheme::default());
        c.insert(Account::new("6001", "主营业务收入", AcctCategory::Income));
        c.insert(Account::new("6601", "销售费用", AcctCategory::Expense));
        c.insert(Account::new("6602", "管理费用", AcctCategory::Expense));
        c.insert(Account::new("4103", "本年利润", AcctCategory::Equity));
        c.insert(Account::new("410401", "未分配利润", AcctCategory::Equity));
        c
    }

    fn row(code: &str, debit: &str, credit: &str) -> BalanceRow {
        BalanceRow {
            account_code: code.into(),
            account_name: code.into(),
            debit: Money::parse(debit).unwrap(),
            credit: Money::parse(credit).unwrap(),
            ..Default::default()
        }
    }

    #[test]
    fn carry_forward_profit() {
        let c = chart();
        let d = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        let p = Period::new(2026, 1).unwrap();
        let rows = vec![
            row("6001", "0", "100000"), // 收入 10 万
            row("6601", "30000", "0"),  // 销售费用 3 万
            row("6602", "20000", "0"),  // 管理费用 2 万
        ];
        let v = generate_carry_forward(p, d, "转", 1, &rows, &c, "4103", "系统").unwrap();
        assert!(v.balanced());
        assert_eq!(v.entries.len(), 4);
        // 收入：借记 6001
        let e0 = &v.entries[0];
        assert_eq!(e0.account_code, "6001");
        assert_eq!(e0.debit, Money::parse("100000").unwrap());
        // 费用：贷记 6601 / 6602
        assert_eq!(v.entries[1].credit, Money::parse("30000").unwrap());
        assert_eq!(v.entries[2].credit, Money::parse("20000").unwrap());
        // 本年利润：净利 5 万，贷记
        let last = v.entries.last().unwrap();
        assert_eq!(last.account_code, "4103");
        assert_eq!(last.credit, Money::parse("50000").unwrap());
        // 借：主营业务收入 10 万；贷：销售费用 3 万、管理费用 2 万、本年利润 5 万
        assert_eq!(v.debit_total(), Money::parse("100000").unwrap());
        assert_eq!(v.credit_total(), Money::parse("100000").unwrap());
    }

    #[test]
    fn carry_forward_loss() {
        let c = chart();
        let d = NaiveDate::from_ymd_opt(2026, 2, 28).unwrap();
        let p = Period::new(2026, 2).unwrap();
        let rows = vec![
            row("6001", "0", "10000"),
            row("6602", "25000", "0"),
        ];
        let v = generate_carry_forward(p, d, "转", 2, &rows, &c, "4103", "系统").unwrap();
        assert!(v.balanced());
        let last = v.entries.last().unwrap();
        // 亏损 1.5 万 → 借记本年利润
        assert_eq!(last.debit, Money::parse("15000").unwrap());
    }

    #[test]
    fn nothing_to_carry() {
        let c = chart();
        let d = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        let p = Period::new(2026, 3).unwrap();
        let rows = vec![row("6001", "0", "0")];
        assert!(generate_carry_forward(p, d, "转", 1, &rows, &c, "4103", "系统").is_err());
    }

    #[test]
    fn year_end_carry() {
        let c = chart();
        let d = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
        let p = Period::new(2026, 12).unwrap();
        // 本年利润贷方余额 8 万（盈利）
        let v = generate_year_end_carry(
            p,
            d,
            "转",
            99,
            Money::parse("-80000").unwrap(),
            "410401",
            &c,
            "系统",
        )
        .unwrap();
        assert!(v.balanced());
        assert_eq!(v.entries[0].account_code, "4103");
        assert_eq!(v.entries[0].debit, Money::parse("80000").unwrap());
        assert_eq!(v.entries[1].account_code, "410401");
        assert_eq!(v.entries[1].credit, Money::parse("80000").unwrap());
    }

    #[test]
    fn close_check_blocks() {
        let trial = TrialBalance::default();
        let rows = vec![row("6001", "0", "1000")];
        // 场景一：从未结账过，且跳过了启用期间
        let input = CloseCheckInput {
            period: Period::new(2026, 3).unwrap(),
            unposted: 2,
            drafts: 1,
            trial: &trial,
            pl_rows: &rows,
            require_carry: true,
            closed_upto: None,
            start_period: Period::new(2026, 1).unwrap(),
        };
        let iss = check_can_close(&input);
        assert!(iss.iter().any(|s| s.contains("未记账")));
        assert!(iss.iter().any(|s| s.contains("首次结账")));
        assert!(iss.iter().any(|s| s.contains("尚未结转")));

        // 场景二：已结账到 1 月，直接结 3 月 → 拦截跳月
        let input2 = CloseCheckInput {
            closed_upto: Some(Period::new(2026, 1).unwrap()),
            ..input
        };
        let iss2 = check_can_close(&input2);
        assert!(
            iss2.iter().any(|s| s.contains("跳过中间期间")),
            "{:?}",
            iss2.iter().collect::<Vec<_>>()
        );

        // 场景三：对已结账期间重复结账
        let input3 = CloseCheckInput {
            period: Period::new(2026, 1).unwrap(),
            closed_upto: Some(Period::new(2026, 1).unwrap()),
            ..input
        };
        assert!(check_can_close(&input3).iter().any(|s| s.contains("已经结账")));
    }

    #[test]
    fn close_check_passes() {
        let trial = TrialBalance::default();
        let rows = vec![row("6001", "0", "0")];
        let input = CloseCheckInput {
            period: Period::new(2026, 1).unwrap(),
            unposted: 0,
            drafts: 0,
            trial: &trial,
            pl_rows: &rows,
            require_carry: true,
            closed_upto: None,
            start_period: Period::new(2026, 1).unwrap(),
        };
        assert!(check_can_close(&input).is_empty());
    }
}
