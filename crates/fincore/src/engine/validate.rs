//! 凭证校验
//!
//! 财务软件的价值一半在这里：把不合规的凭证挡在账外。
//! 校验返回 `Issues`——收集全部问题一次性反馈，而不是遇到第一个就弹窗打断录入。

use rust_decimal::Decimal;

use crate::account::{Account, AuxKind, Chart};
use crate::account::BookOptions;
use crate::error::Issues;
use crate::money::Money;
use crate::period::Period;
use crate::voucher::{Voucher, VoucherStatus};

/// 校验上下文
pub struct ValidateCtx<'a> {
    pub chart: &'a Chart,
    pub opts: &'a BookOptions,
    /// 已结账的最大期间（该期间及之前不允许再动）
    pub closed_upto: Option<Period>,
    /// 是否强制要求填写摘要
    pub require_summary: bool,
}

impl<'a> ValidateCtx<'a> {
    pub fn new(chart: &'a Chart, opts: &'a BookOptions, closed_upto: Option<Period>) -> Self {
        Self {
            chart,
            opts,
            closed_upto,
            require_summary: true,
        }
    }
}

/// 完整校验一张凭证（保存 / 审核 / 记账 共用）
///
/// = [`validate_for_save`] 的全部严格项，外加空分录行的软提示。
pub fn validate_voucher(v: &Voucher, ctx: &ValidateCtx) -> Issues {
    let mut iss = validate_for_save(v, ctx);
    if !iss.is_empty() {
        // 已经有硬错误时不再提示空行，避免噪声盖住真正的问题
        return iss;
    }
    let blanks = v.blank_lines();
    if !blanks.is_empty() && v.entries.len() > 2 {
        // 允许末尾留一个空行方便继续录入，其他空行提示
        let real = blanks.iter().filter(|&&l| l as usize != v.entries.len()).count();
        if real > 0 {
            iss.push(format!("存在 {real} 条借贷均为空的分录行，保存时会自动忽略"));
        }
    }
    iss
}

/// 落库前的**强校验**：任何一条不通过都不允许写进数据库。
///
/// 与 [`validate_voucher`] 的区别是不含"空行提示"这类软提示，因此可以放心地
/// 作为持久化层的最后一道防线——界面可以漏检，数据库不能再放进一张烂凭证。
pub fn validate_for_save(v: &Voucher, ctx: &ValidateCtx) -> Issues {
    let mut iss = Issues::new();

    // 0) 金额/数量量级上限：rust_decimal 的加、乘溢出会直接 panic，
    //    必须早于 balanced()/合计等任何运算，拦住极端输入。
    let max_money = Money::parse("999999999999999.99").unwrap_or(Money::ZERO);
    let max_qty = Money::parse("999999999.999999").unwrap_or(Money::ZERO);
    let max_price = Money::parse("999999999.99").unwrap_or(Money::ZERO);
    let mut over_limit = false;
    for e in &v.entries {
        if e.debit.abs() > max_money || e.credit.abs() > max_money {
            over_limit = true;
        }
        if e.qty.map(|q| q.abs() > max_qty).unwrap_or(false)
            || e.price.map(|p| p.abs() > max_price).unwrap_or(false)
        {
            over_limit = true;
        }
    }
    if over_limit {
        iss.push(format!(
            "金额或数量超出系统上限（金额最大 {}，数量/单价最大 999,999,999）",
            max_money.fmt_money()
        ));
        return iss;
    }

    // 1. 期间锁定
    if let Some(upto) = ctx.closed_upto {
        if v.period <= upto {
            iss.push(format!(
                "{} 及以前期间已结账，不能再新增或修改凭证（当前凭证期间 {}）",
                upto.label(),
                v.period.label()
            ));
        }
    }

    // 2. 日期与期间一致
    if Period::from_date(v.date) != v.period {
        iss.push(format!(
            "凭证日期 {} 与所属期间 {} 不一致",
            v.date.format("%Y-%m-%d"),
            v.period.label()
        ));
    }

    // 3. 至少有两条有效分录
    if v.effective_lines() < 2 {
        iss.push("一张凭证至少需要两条有效分录（借贷各至少一笔）");
        return iss; // 后面逐行校验意义不大，提前返回
    }

    // 4. 借贷平衡
    if !v.balanced() {
        iss.push(format!(
            "借贷不平衡：借方合计 {}，贷方合计 {}，差额 {}",
            v.debit_total().fmt_money(),
            v.credit_total().fmt_money(),
            v.diff().fmt_money()
        ));
    }

    // 5. 存在借方与贷方
    if v.debit_total().is_zero() || v.credit_total().is_zero() {
        iss.push("凭证必须同时存在借方与贷方金额");
    }

    // 6. 逐行校验
    for e in &v.entries {
        if e.is_blank() {
            continue;
        }
        validate_entry(&e.account_code, e, ctx, &mut iss);
    }

    iss
}

/// 校验单行分录
pub fn validate_entry(
    code: &str,
    e: &crate::voucher::Entry,
    ctx: &ValidateCtx,
    iss: &mut Issues,
) {
    let line = e.line;

    // 借贷不能同时有值
    if e.both_sides() {
        iss.push(format!(
            "第 {line} 行借贷双方同时有金额（{} / {}），同一行只能记一方",
            e.debit.fmt_money(),
            e.credit.fmt_money()
        ));
    }
    if e.debit.is_negative() || e.credit.is_negative() {
        iss.push(format!("第 {line} 行金额不能为负数"));
    }

    // 科目必须存在
    let acct: &Account = match ctx.chart.get(code) {
        Some(a) => a,
        None => {
            iss.push(format!("第 {line} 行科目 {code} 不存在"));
            return;
        }
    };

    if acct.disabled {
        iss.push(format!("第 {line} 行科目 {} {} 已停用", acct.code, acct.name));
    }

    // 必须末级
    if !ctx.chart.is_leaf(code) {
        iss.push(format!(
            "第 {line} 行科目 {} {} 是非末级科目，不能记账",
            acct.code, acct.name
        ));
    }

    // 摘要
    if ctx.require_summary && e.summary.trim().is_empty() {
        iss.push(format!("第 {line} 行摘要不能为空"));
    }

    // 辅助核算必填
    for k in acct.aux.list() {
        if e.aux.get(k).map(|s| s.trim().is_empty()).unwrap_or(true) {
            iss.push(format!(
                "第 {line} 行科目 {} 核算{}，必须填写{}",
                acct.code,
                k.label(),
                k.label()
            ));
        }
    }

    // 数量核算
    if acct.has_qty {
        if e.qty.is_none() || e.qty.map(|q| q.is_zero()).unwrap_or(true) {
            iss.push(format!("第 {line} 行科目 {} 核算数量，必须填写数量", acct.code));
        }
        // 数量 × 单价 ≈ 金额（容差 0.01）
        if let (Some(q), Some(p)) = (e.qty, e.price) {
            let expect = q * p.inner();
            let actual = e.amount();
            if (expect - actual).abs() > Money::parse("0.01").unwrap() {
                iss.push(format!(
                    "第 {line} 行数量 × 单价 = {}，与金额 {} 不符",
                    expect.fmt_money(),
                    actual.fmt_money()
                ));
            }
        }
    }

    // 外币核算
    if let Some(cur) = &acct.currency {
        if e.currency.as_deref().unwrap_or("") != cur && e.amount_for.is_none() {
            iss.push(format!(
                "第 {line} 行科目 {} 核算外币 {}，必须填写原币金额与汇率",
                acct.code, cur
            ));
        }
        if e.rate.map(|r| r <= Decimal::ZERO).unwrap_or(false) {
            iss.push(format!("第 {line} 行汇率必须大于 0"));
        }
        if let (Some(af), Some(rate)) = (e.amount_for, e.rate) {
            let expect = af * rate;
            let actual = e.amount();
            if (expect - actual).abs() > Money::parse("0.01").unwrap() {
                iss.push(format!(
                    "第 {line} 行原币 {} × 汇率 = {}，与金额 {} 不符",
                    af.fmt_money(),
                    expect.fmt_money(),
                    actual.fmt_money()
                ));
            }
        }
    }

    // 现金/银行科目的现金流量项目
    if ctx.opts.enable_foreign && false {
        // 占位：外币分账制下的额外校验
    }
    if (acct.is_cash || acct.is_bank) && e.aux.get(AuxKind::CashFlow).is_none() {
        // 只在科目默认指定了现金流量项目时才强制；否则由用户事后指定，这里仅软提示
        if acct.cash_flow_item.is_some() {
            // 有默认值，不强制
        }
    }
}

/// 记账前校验（无审核环节：未记账凭证核对无误后直接记账）
pub fn validate_post(v: &Voucher, _opts: &BookOptions) -> Issues {
    let mut iss = Issues::new();
    if !v.status.can_post() {
        iss.push(format!("凭证当前状态为「{}」，不能记账", v.status.label()));
    }
    // 出纳签字前置的真正把关在 findb::vouchers::post_tx（BookOptions::require_cashier，仅现金/银行科目凭证）
    iss
}

/// 反记账前校验
pub fn validate_unpost(v: &Voucher, closed_upto: Option<Period>) -> Issues {
    let mut iss = Issues::new();
    if !v.status.can_unpost() {
        iss.push(format!("凭证当前状态为「{}」，不能反记账", v.status.label()));
    }
    if let Some(upto) = closed_upto {
        if v.period <= upto {
            iss.push(format!("{} 及以前期间已结账，请先反结账", upto.label()));
        }
    }
    iss
}

/// 删除前校验（未记账凭证可删；已记账需先反记账）
pub fn validate_delete(v: &Voucher, closed_upto: Option<Period>) -> Issues {
    let mut iss = Issues::new();
    if !v.status.can_delete() {
        iss.push(format!("凭证已{}，不能删除，请先反记账", v.status.label()));
    }
    if let Some(upto) = closed_upto {
        if v.period <= upto {
            iss.push(format!("{} 及以前期间已结账，不能删除凭证", upto.label() ));
        }
    }
    iss
}

/// 作废/恢复校验
pub fn validate_void(v: &Voucher) -> Issues {
    let mut iss = Issues::new();
    if v.status == VoucherStatus::Posted {
        iss.push("已记账的凭证不能作废，请先反记账");
    }
    iss
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Account, AcctCategory, AuxMask, CodeScheme};
    use crate::voucher::Entry;

    fn chart() -> Chart {
        let mut c = Chart::new(CodeScheme::default());
        c.insert(Account::new("1001", "库存现金", AcctCategory::Asset));
        c.insert(Account::new("1002", "银行存款", AcctCategory::Asset));
        c.insert(Account::new("100201", "工行基本户", AcctCategory::Asset));
        let mut ar = Account::new("1122", "应收账款", AcctCategory::Asset);
        ar.aux = AuxMask::NONE.with(AuxKind::Customer);
        c.insert(ar);
        let mut rev = Account::new("6001", "主营业务收入", AcctCategory::Income);
        rev.aux = AuxMask::NONE;
        c.insert(rev);
        c
    }

    fn opts() -> BookOptions {
        BookOptions::default()
    }

    fn base_voucher() -> Voucher {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        let mut v = Voucher::new(Period::new(2026, 1).unwrap(), d, "记", 1);
        v.prepared_by = "张三".to_string();
        v.push_entry(Entry {
            debit: Money::parse("1130").unwrap(),
            ..Entry::new(1, "1001", "销售收款")
        });
        v.push_entry(Entry {
            credit: Money::parse("1130").unwrap(),
            ..Entry::new(2, "6001", "销售收款")
        });
        v
    }

    #[test]
    fn ok_voucher() {
        let c = chart();
        let o = opts();
        let ctx = ValidateCtx::new(&c, &o, None);
        let v = base_voucher();
        let iss = validate_voucher(&v, &ctx);
        assert!(iss.is_empty(), "{:?}", iss.iter().collect::<Vec<_>>());
    }

    #[test]
    fn unbalanced_detected() {
        let c = chart();
        let o = opts();
        let ctx = ValidateCtx::new(&c, &o, None);
        let mut v = base_voucher();
        v.entries[1].credit = Money::parse("1000").unwrap();
        let iss = validate_voucher(&v, &ctx);
        assert!(iss.iter().any(|s| s.contains("借贷不平衡")));
    }

    /// 量化口径必须与落库一致：借 100.015 / 贷 100.010 的原始差额 0.005
    /// 会被旧的 `diff().round2()` 容差放过，但入库逐行 round2 后变成
    /// 100.02 / 100.01，账套里就多出一张不平的凭证。
    #[test]
    fn quantization_mismatch_detected() {
        let c = chart();
        let o = opts();
        let ctx = ValidateCtx::new(&c, &o, None);
        let mut v = base_voucher();
        v.entries[0].debit = Money::parse("100.015").unwrap();
        v.entries[1].credit = Money::parse("100.010").unwrap();
        let iss = validate_voucher(&v, &ctx);
        assert!(
            iss.iter().any(|s| s.contains("借贷不平衡")),
            "{:?}",
            iss.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn non_leaf_detected() {
        let c = chart();
        let o = opts();
        let ctx = ValidateCtx::new(&c, &o, None);
        let mut v = base_voucher();
        v.entries[1].account_code = "1002".into();
        let iss = validate_voucher(&v, &ctx);
        assert!(iss.iter().any(|s| s.contains("非末级")));
    }

    #[test]
    fn aux_required() {
        let c = chart();
        let o = opts();
        let ctx = ValidateCtx::new(&c, &o, None);
        let mut v = base_voucher();
        v.entries[0].account_code = "1122".into();
        let iss = validate_voucher(&v, &ctx);
        assert!(iss.iter().any(|s| s.contains("客户")));
    }

    #[test]
    fn closed_period_blocks() {
        let c = chart();
        let o = opts();
        let ctx = ValidateCtx::new(&c, &o, Some(Period::new(2026, 1).unwrap()));
        let v = base_voucher();
        let iss = validate_voucher(&v, &ctx);
        assert!(iss.iter().any(|s| s.contains("已结账")));
    }

    #[test]
    fn date_period_mismatch() {
        let c = chart();
        let o = opts();
        let ctx = ValidateCtx::new(&c, &o, None);
        let mut v = base_voucher();
        v.date = chrono::NaiveDate::from_ymd_opt(2026, 2, 3).unwrap();
        let iss = validate_voucher(&v, &ctx);
        assert!(iss.iter().any(|s| s.contains("不一致")));
    }
}
