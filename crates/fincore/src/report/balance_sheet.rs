//! 资产负债表模板（依据《企业会计准则第30号——财务报表列报》的一般企业格式）

use crate::report::{AmountKind, ReportDef, ReportLine, Term};

/// 内置资产负债表模板
pub fn balance_sheet_def() -> ReportDef {
    let mut lines: Vec<ReportLine> = Vec::new();

    // ---------- 资产 ----------
    lines.push(ReportLine::header("流动资产："));

    let i_cash = lines.len();
    lines.push(ReportLine::normal(
        "1",
        "货币资金",
        1,
        vec![Term::acct(&["1001", "1002", "1012", "1015"], AmountKind::End)],
    ));

    let i_fin_asset = lines.len();
    lines.push(ReportLine::normal(
        "2",
        "交易性金融资产",
        1,
        vec![Term::acct(&["1101"], AmountKind::End)],
    ));

    let i_note_recv = lines.len();
    lines.push(ReportLine::normal(
        "3",
        "应收票据",
        1,
        vec![Term::acct(&["1121"], AmountKind::End)],
    ));

    let i_ar = lines.len();
    lines.push(ReportLine::normal(
        "4",
        "应收账款",
        1,
        vec![
            Term::acct(&["1122"], AmountKind::End).debit_only(),
            Term::acct(&["2203"], AmountKind::End).debit_only(),
            // 减去坏账准备（贷方余额为负，直接相加即扣减）
            Term::acct(&["1231"], AmountKind::End),
        ],
    ));

    let i_prepay = lines.len();
    lines.push(ReportLine::normal(
        "5",
        "预付款项",
        1,
        vec![
            Term::acct(&["1123"], AmountKind::End).debit_only(),
            Term::acct(&["2202"], AmountKind::End).debit_only(),
        ],
    ));

    let i_other_recv = lines.len();
    lines.push(ReportLine::normal(
        "6",
        "其他应收款",
        1,
        vec![
            Term::acct(&["1221"], AmountKind::End).debit_only(),
            Term::acct(&["1131", "1132"], AmountKind::End).debit_only(),
        ],
    ));

    let i_inventory = lines.len();
    lines.push(ReportLine::normal(
        "7",
        "存货",
        1,
        vec![
            Term::acct(
                &[
                    "1401", "1402", "1403", "1404", "1405", "1406", "1407", "1408", "1411", "5001",
                    "5101",
                ],
                AmountKind::End,
            ),
            // 减去存货跌价准备（贷方余额为负，直接相加即扣减）
            Term::acct(&["1471"], AmountKind::End),
        ],
    ));

    let i_other_cur = lines.len();
    lines.push(ReportLine::normal(
        "8",
        "其他流动资产",
        1,
        vec![Term::acct(&["1124"], AmountKind::End)],
    ));

    let i_cur_total = lines.len();
    lines.push(ReportLine::subtotal(
        "9",
        "流动资产合计",
        0,
        vec![
            Term::line(i_cash),
            Term::line(i_fin_asset),
            Term::line(i_note_recv),
            Term::line(i_ar),
            Term::line(i_prepay),
            Term::line(i_other_recv),
            Term::line(i_inventory),
            Term::line(i_other_cur),
        ],
    ));

    lines.push(ReportLine::blank());
    lines.push(ReportLine::header("非流动资产："));

    let i_lt_invest = lines.len();
    lines.push(ReportLine::normal(
        "10",
        "长期股权投资",
        1,
        vec![
            Term::acct(&["1511"], AmountKind::End),
            // 长期股权投资减值准备（贷方余额为负，直接相加即扣减）
            Term::acct(&["1512"], AmountKind::End),
        ],
    ));

    let i_fixed = lines.len();
    lines.push(ReportLine::normal(
        "11",
        "固定资产",
        1,
        vec![
            Term::acct(&["1601"], AmountKind::End),
            Term::acct(&["1602"], AmountKind::End),
            Term::acct(&["1603"], AmountKind::End),
        ],
    ));

    let i_construction = lines.len();
    lines.push(ReportLine::normal(
        "12",
        "在建工程",
        1,
        vec![
            Term::acct(&["1604"], AmountKind::End),
            Term::acct(&["1605"], AmountKind::End),
        ],
    ));

    let i_intangible = lines.len();
    lines.push(ReportLine::normal(
        "13",
        "无形资产",
        1,
        vec![
            Term::acct(&["1701"], AmountKind::End),
            Term::acct(&["1702"], AmountKind::End),
        ],
    ));

    let i_lt_prepaid = lines.len();
    lines.push(ReportLine::normal(
        "14",
        "长期待摊费用",
        1,
        vec![Term::acct(&["1801"], AmountKind::End)],
    ));

    let i_deferred_tax_asset = lines.len();
    lines.push(ReportLine::normal(
        "15",
        "递延所得税资产",
        1,
        vec![Term::acct(&["1811"], AmountKind::End)],
    ));

    let i_ncur_total = lines.len();
    lines.push(ReportLine::subtotal(
        "16",
        "非流动资产合计",
        0,
        vec![
            Term::line(i_lt_invest),
            Term::line(i_fixed),
            Term::line(i_construction),
            Term::line(i_intangible),
            Term::line(i_lt_prepaid),
            Term::line(i_deferred_tax_asset),
        ],
    ));

    let _i_asset_total = lines.len();
    lines.push(ReportLine::total(
        "17",
        "资 产 总 计",
        0,
        vec![Term::line(i_cur_total), Term::line(i_ncur_total)],
    ));

    lines.push(ReportLine::blank());

    // ---------- 负债 ----------
    lines.push(ReportLine::header("流动负债："));

    let i_short_loan = lines.len();
    lines.push(ReportLine::normal(
        "18",
        "短期借款",
        1,
        vec![Term::acct(&["2001"], AmountKind::End).neg()],
    ));

    let i_note_pay = lines.len();
    lines.push(ReportLine::normal(
        "19",
        "应付票据",
        1,
        vec![Term::acct(&["2201"], AmountKind::End).neg()],
    ));

    let i_ap = lines.len();
    lines.push(ReportLine::normal(
        "20",
        "应付账款",
        1,
        vec![
            Term::acct(&["2202"], AmountKind::End).credit_only().neg(),
            Term::acct(&["1123"], AmountKind::End).credit_only().neg(),
        ],
    ));

    let i_advance = lines.len();
    lines.push(ReportLine::normal(
        "21",
        "预收款项",
        1,
        vec![
            Term::acct(&["2203"], AmountKind::End).credit_only().neg(),
            Term::acct(&["1122"], AmountKind::End).credit_only().neg(),
        ],
    ));

    let i_payroll = lines.len();
    lines.push(ReportLine::normal(
        "22",
        "应付职工薪酬",
        1,
        vec![Term::acct(&["2211"], AmountKind::End).neg()],
    ));

    let i_tax = lines.len();
    lines.push(ReportLine::normal(
        "23",
        "应交税费",
        1,
        vec![Term::acct(&["2221"], AmountKind::End).neg()],
    ));

    let i_other_pay = lines.len();
    lines.push(ReportLine::normal(
        "24",
        "其他应付款",
        1,
        vec![Term::acct(&["2241", "2231", "2232"], AmountKind::End).neg()],
    ));

    let i_other_cur_liab = lines.len();
    lines.push(ReportLine::normal(
        "25",
        "其他流动负债",
        1,
        vec![Term::acct(&["2401"], AmountKind::End).neg()],
    ));

    let i_cur_liab_total = lines.len();
    lines.push(ReportLine::subtotal(
        "26",
        "流动负债合计",
        0,
        vec![
            Term::line(i_short_loan),
            Term::line(i_note_pay),
            Term::line(i_ap),
            Term::line(i_advance),
            Term::line(i_payroll),
            Term::line(i_tax),
            Term::line(i_other_pay),
            Term::line(i_other_cur_liab),
        ],
    ));

    lines.push(ReportLine::blank());
    lines.push(ReportLine::header("非流动负债："));

    let i_lt_loan = lines.len();
    lines.push(ReportLine::normal(
        "27",
        "长期借款",
        1,
        vec![Term::acct(&["2501"], AmountKind::End).neg()],
    ));

    let i_deferred_tax_liab = lines.len();
    lines.push(ReportLine::normal(
        "28",
        "递延所得税负债",
        1,
        vec![Term::acct(&["2901"], AmountKind::End).neg()],
    ));

    let i_ncur_liab_total = lines.len();
    lines.push(ReportLine::subtotal(
        "29",
        "非流动负债合计",
        0,
        vec![Term::line(i_lt_loan), Term::line(i_deferred_tax_liab)],
    ));

    let i_liab_total = lines.len();
    lines.push(ReportLine::total(
        "30",
        "负 债 合 计",
        0,
        vec![
            Term::line(i_cur_liab_total),
            Term::line(i_ncur_liab_total),
        ],
    ));

    lines.push(ReportLine::blank());

    // ---------- 所有者权益 ----------
    lines.push(ReportLine::header("所有者权益（或股东权益）："));

    let i_capital = lines.len();
    lines.push(ReportLine::normal(
        "31",
        "实收资本（或股本）",
        1,
        vec![Term::acct(&["4001"], AmountKind::End).neg()],
    ));

    let i_reserve = lines.len();
    lines.push(ReportLine::normal(
        "32",
        "资本公积",
        1,
        vec![Term::acct(&["4002"], AmountKind::End).neg()],
    ));

    let i_surplus = lines.len();
    lines.push(ReportLine::normal(
        "33",
        "盈余公积",
        1,
        vec![Term::acct(&["4101"], AmountKind::End).neg()],
    ));

    let i_undistributed = lines.len();
    lines.push(ReportLine::normal(
        "34",
        "未分配利润",
        1,
        vec![
            // 已结转部分：本年利润 + 利润分配（年末结转后落到这里）
            Term::acct(&["4103", "4104"], AmountKind::End).neg(),
            // 未结转部分：本期盈亏还挂在损益类科目上，必须并入，否则资产负债表不平
            Term::profit_loss_net(AmountKind::End).neg(),
        ],
    ));

    let i_equity_total = lines.len();
    lines.push(ReportLine::total(
        "35",
        "所有者权益（或股东权益）合计",
        0,
        vec![
            Term::line(i_capital),
            Term::line(i_reserve),
            Term::line(i_surplus),
            Term::line(i_undistributed),
        ],
    ));

    lines.push(ReportLine::total(
        "36",
        "负债和所有者权益（或股东权益）总计",
        0,
        vec![Term::line(i_liab_total), Term::line(i_equity_total)],
    ));

    ReportDef {
        key: "balance_sheet".to_string(),
        name: "资产负债表".to_string(),
        columns: vec!["期末余额".to_string(), "年初余额".to_string()],
        lines,
    }
}

/// 资产总计所在行索引（便于外部做平衡校验）
pub fn asset_total_row(def: &ReportDef) -> Option<usize> {
    def.lines.iter().position(|l| l.no == "17")
}

/// 负债和所有者权益总计所在行索引
pub fn liab_equity_total_row(def: &ReportDef) -> Option<usize> {
    def.lines.iter().position(|l| l.no == "36")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::ReportLine;

    #[test]
    fn def_shape() {
        let d = balance_sheet_def();
        assert_eq!(d.columns.len(), 2);
        // 索引引用不应越界
        for (i, l) in d.lines.iter().enumerate() {
            for t in &l.terms {
                if let Term::Line { index, .. } = t {
                    assert!(*index < d.lines.len(), "line {i} refs {index} out of range");
                }
            }
        }
        assert!(asset_total_row(&d).is_some());
        assert!(liab_equity_total_row(&d).is_some());
        let _ = ReportLine::blank();
    }

    #[test]
    fn no_cycle() {
        let d = balance_sheet_def();
        // 简单检查：合计行只引用比自己小的普通行
        for (i, l) in d.lines.iter().enumerate() {
            for t in &l.terms {
                if let Term::Line { index, .. } = t {
                    assert!(*index < i, "第 {i} 行引用了后面的第 {index} 行");
                }
            }
        }
    }
}
