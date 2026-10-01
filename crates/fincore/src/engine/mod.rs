//! 会计引擎：凭证校验、过账规则、期末处理

pub mod aging;
pub mod costing;
pub mod formula;
pub mod depreciation;
pub mod period_end;
pub mod tax;
pub mod validate;

pub use aging::{
    analyze as aging_analyze, bad_debt_provision, buckets_by_days, buckets_by_year,
    column_totals as aging_column_totals, default_bad_debt_rates, AgingBucket, AgingItem,
    AgingLine,
};
pub use formula::{
    check as check_formula, eval as eval_formula, referenced_accounts, FormulaSource,
};
pub use costing::{run as run_costing, CostMethod, Lot, Move, StockState};
pub use depreciation::{
    amount_at as dep_amount_at, schedule as dep_schedule, DepInput, DepMethod, DepRow,
};
pub use period_end::{
    check_can_close, check_can_unclose, generate_carry_forward, generate_year_end_carry,
    is_year_end, CloseCheckInput, PROFIT_ACCOUNT, UNDISTRIBUTED_ACCOUNT,
};
pub use tax::{
    current_tax, rate_of as tax_rate_of, year_schedule as tax_year_schedule, Cumulative,
    MONTHLY_DEDUCTION,
};
pub use validate::{
    validate_delete, validate_entry, validate_for_save, validate_post, validate_unpost,
    validate_void, validate_voucher, ValidateCtx,
};

use crate::account::{Chart, Direction};
use crate::money::Money;
use crate::voucher::{Entry, Voucher};

/// 把凭证分录按科目拆成"借方 / 贷方"两栏，用于打印与预览
pub fn entries_for_print(v: &Voucher, chart: &Chart) -> Vec<PrintEntry> {
    v.entries
        .iter()
        .filter(|e| !e.is_blank())
        .map(|e| PrintEntry {
            line: e.line,
            summary: e.summary.clone(),
            account_display: match chart.get(&e.account_code) {
                Some(a) => format!("{}-{}", a.code, a.name),
                None => e.account_code.clone(),
            },
            aux_display: e.aux.display(&std::collections::HashMap::new()),
            debit: e.debit,
            credit: e.credit,
        })
        .collect()
}

/// 打印用分录
#[derive(Clone, Debug)]
pub struct PrintEntry {
    pub line: i32,
    pub summary: String,
    pub account_display: String,
    pub aux_display: String,
    pub debit: Money,
    pub credit: Money,
}

/// 计算凭证的合计（借贷合计 + 大写）
pub fn voucher_totals(v: &Voucher) -> (Money, Money, String) {
    let d = v.debit_total();
    let c = v.credit_total();
    (d, c, d.to_capital())
}

/// 依据科目余额方向，把带符号金额转成界面显示用的"借 / 贷 + 正数"
pub fn display_amount(signed: Money, expected: Direction) -> (Direction, Money) {
    if signed.is_zero() {
        return (expected, Money::ZERO);
    }
    if signed.is_negative() {
        (Direction::Credit, signed.abs())
    } else {
        (Direction::Debit, signed)
    }
}

/// 生成一条分录的镜像（红字冲销用）：借贷互换
pub fn negate_entry(e: &Entry) -> Entry {
    let mut n = e.clone();
    std::mem::swap(&mut n.debit, &mut n.credit);
    if let Some(q) = n.qty {
        n.qty = Some(q.negated());
    }
    n
}

/// 红字冲销整张凭证（保留科目、辅助核算、摘要加"冲销"前缀）
pub fn reverse_voucher(v: &Voucher) -> Voucher {
    let mut r = v.clone();
    r.id = 0;
    r.entries = v
        .entries
        .iter()
        .filter(|e| !e.is_blank())
        .map(|e| {
            let mut n = negate_entry(e);
            n.id = 0;
            n.summary = format!("冲销：{}", e.summary);
            n
        })
        .collect();
    r.renumber();
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Account, AcctCategory, CodeScheme};
    use crate::period::Period;
    use crate::voucher::{AuxRef, VoucherSource};

    fn v() -> Voucher {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        let mut v = Voucher::new(Period::new(2026, 1).unwrap(), d, "记", 1);
        v.source = VoucherSource::Manual;
        v.push_entry(Entry {
            debit: Money::parse("500").unwrap(),
            ..Entry::new(1, "1001", "报销差旅费")
        });
        v.push_entry(Entry {
            credit: Money::parse("500").unwrap(),
            aux: AuxRef {
                dept: Some("D01".into()),
                ..Default::default()
            },
            ..Entry::new(2, "660203", "报销差旅费")
        });
        v
    }

    fn chart() -> Chart {
        let mut c = Chart::new(CodeScheme::default());
        c.insert(Account::new("1001", "库存现金", AcctCategory::Asset));
        c.insert(Account::new("660203", "差旅费", AcctCategory::Expense));
        c
    }

    #[test]
    fn reversal_balanced() {
        let v = v();
        let r = reverse_voucher(&v);
        assert!(r.balanced());
        assert_eq!(r.entries[0].credit, Money::parse("500").unwrap());
        assert_eq!(r.entries[1].debit, Money::parse("500").unwrap());
        assert!(r.entries[0].summary.starts_with("冲销"));
        // 辅助核算要跟着冲销过去
        assert_eq!(r.entries[1].aux.dept.as_deref(), Some("D01"));
    }

    #[test]
    fn print_view() {
        let c = chart();
        let v = v();
        let rows = entries_for_print(&v, &c);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].account_display, "1001-库存现金");
        assert_eq!(rows[0].debit, Money::parse("500").unwrap());
    }

    #[test]
    fn totals_capital() {
        let v = v();
        let (d, c, cap) = voucher_totals(&v);
        assert_eq!(d, c);
        assert_eq!(cap, "伍佰元整");
    }
}
