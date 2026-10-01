//! 利润表模板（一般企业格式）

use crate::report::{AmountKind, ReportDef, ReportLine, Term};

/// 收入类科目取数：贷方 - 借方
fn revenue(accounts: &[&str]) -> Vec<Term> {
    vec![
        Term::acct(accounts, AmountKind::PeriodCredit),
        Term::acct(accounts, AmountKind::PeriodDebit).neg(),
    ]
}

/// 费用类科目取数：借方 - 贷方
fn expense(accounts: &[&str]) -> Vec<Term> {
    vec![
        Term::acct(accounts, AmountKind::PeriodDebit),
        Term::acct(accounts, AmountKind::PeriodCredit).neg(),
    ]
}

/// 内置利润表模板
pub fn income_statement_def() -> ReportDef {
    let mut lines: Vec<ReportLine> = Vec::new();

    let i_revenue = lines.len();
    lines.push(ReportLine::total(
        "1",
        "一、营业收入",
        0,
        revenue(&["6001", "6051"]),
    ));

    let i_cost = lines.len();
    lines.push(ReportLine::normal("2", "减：营业成本", 1, expense(&["6401", "6402"])));

    let i_tax = lines.len();
    lines.push(ReportLine::normal(
        "3",
        "　　税金及附加",
        1,
        expense(&["6403"]),
    ));

    let i_sell = lines.len();
    lines.push(ReportLine::normal("4", "　　销售费用", 1, expense(&["6601"])));

    let i_admin = lines.len();
    lines.push(ReportLine::normal("5", "　　管理费用", 1, expense(&["6602"])));

    let i_fin = lines.len();
    lines.push(ReportLine::normal("6", "　　财务费用", 1, expense(&["6603"])));

    let i_impair = lines.len();
    lines.push(ReportLine::normal(
        "7",
        "　　资产减值损失",
        1,
        expense(&["6701"]),
    ));

    let i_fair = lines.len();
    lines.push(ReportLine::normal(
        "8",
        "加：公允价值变动收益（损失以“-”号填列）",
        0,
        revenue(&["6101"]),
    ));

    let i_invest = lines.len();
    lines.push(ReportLine::normal(
        "9",
        "　　投资收益（损失以“-”号填列）",
        1,
        revenue(&["6111"]),
    ));

    let i_operating = lines.len();
    lines.push(ReportLine::total(
        "10",
        "二、营业利润（亏损以“-”号填列）",
        0,
        vec![
            Term::line(i_revenue),
            Term::line(i_cost).neg(),
            Term::line(i_tax).neg(),
            Term::line(i_sell).neg(),
            Term::line(i_admin).neg(),
            Term::line(i_fin).neg(),
            Term::line(i_impair).neg(),
            Term::line(i_fair),
            Term::line(i_invest),
        ],
    ));

    let i_nonop_inc = lines.len();
    lines.push(ReportLine::normal(
        "11",
        "加：营业外收入",
        0,
        revenue(&["6301"]),
    ));

    let i_nonop_exp = lines.len();
    lines.push(ReportLine::normal(
        "12",
        "减：营业外支出",
        0,
        expense(&["6711"]),
    ));

    let i_total_profit = lines.len();
    lines.push(ReportLine::total(
        "13",
        "三、利润总额（亏损总额以“-”号填列）",
        0,
        vec![
            Term::line(i_operating),
            Term::line(i_nonop_inc),
            Term::line(i_nonop_exp).neg(),
        ],
    ));

    let i_income_tax = lines.len();
    lines.push(ReportLine::normal(
        "14",
        "减：所得税费用",
        0,
        expense(&["6801"]),
    ));

    lines.push(ReportLine::total(
        "15",
        "四、净利润（净亏损以“-”号填列）",
        0,
        vec![
            Term::line(i_total_profit),
            Term::line(i_income_tax).neg(),
        ],
    ));

    ReportDef {
        key: "income_statement".to_string(),
        name: "利润表".to_string(),
        columns: vec!["本期金额".to_string(), "本年累计金额".to_string()],
        lines,
    }
}

/// 净利润所在行索引
pub fn net_profit_row(def: &ReportDef) -> Option<usize> {
    def.lines.iter().position(|l| l.no == "15")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn def_shape() {
        let d = income_statement_def();
        assert_eq!(d.columns.len(), 2);
        for (i, l) in d.lines.iter().enumerate() {
            for t in &l.terms {
                if let Term::Line { index, .. } = t {
                    assert!(*index < i, "第 {i} 行引用了后面的第 {index} 行");
                }
            }
        }
        assert!(net_profit_row(&d).is_some());
    }

    #[test]
    fn revenue_terms() {
        let t = revenue(&["6001"]);
        assert_eq!(t.len(), 2);
        match &t[0] {
            Term::Acct { kind, sign, .. } => {
                assert_eq!(*kind, AmountKind::PeriodCredit);
                assert_eq!(*sign, 1);
            }
            _ => panic!("应为科目取数"),
        }
        match &t[1] {
            Term::Acct { kind, sign, .. } => {
                assert_eq!(*kind, AmountKind::PeriodDebit);
                assert_eq!(*sign, -1);
            }
            _ => panic!("应为科目取数"),
        }
    }
}
