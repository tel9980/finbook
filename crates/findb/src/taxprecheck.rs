//! 增值税申报前体检。
//!
//! 申报表能取数，不等于数是对的。真出事的场景几乎都是**票和账对不上**：
//! 票录了但凭证没记（或反过来）、进项票忘了认证、跨月的票混进本期、
//! 税率录错档、金额与税额自己都不勾稽。
//!
//! 所以这里不重新算税额（那是 [`crate::taxdecl::vat_main_form`] 的事），
//! 只做**交叉核对**：把「票上的数」和「账上的数」摆在一起，差多少、差在哪张上。
//!
//! 三个判据分工：
//! - `error`：账票**金额**不等，或票自身勾稽不成立 —— 这种数出去就是错的；
//! - `warn`：口径边界（未认证、跨期、作废未处理、税率不可识别）—— 可能是对的，但要人看一眼；
//! - `info`：提醒类（留抵、尚未转出未交增值税）。

use chrono::NaiveDate;
use fincore::{Money, Period};

use crate::invoices::{self, Invoice, InvoiceQuery};
use crate::{Db, DbResult};

/// 体检项级别
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
}

impl Level {
    pub fn label(&self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
        }
    }
}

/// 一条体检结论
#[derive(Clone, Debug)]
pub struct CheckItem {
    pub level: Level,
    /// 稳定编号，前端据此做「已知晓」标记或跳转（如 `invoice-not-verified`）
    pub code: &'static str,
    pub title: String,
    pub detail: String,
    /// 涉及金额（没有金额的项为 `None`）
    pub amount: Option<Money>,
}

impl CheckItem {
    fn new(
        level: Level,
        code: &'static str,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        CheckItem {
            level,
            code,
            title: title.into(),
            detail: detail.into(),
            amount: None,
        }
    }
    fn with_amount(mut self, a: Money) -> Self {
        self.amount = Some(a);
        self
    }
}

/// 体检报告
#[derive(Clone, Debug)]
pub struct PrecheckReport {
    pub period: String,
    pub items: Vec<CheckItem>,
    pub errors: usize,
    pub warns: usize,
    pub infos: usize,
    /// 没有任何 `error` 即可申报（`warn` 允许存在，但界面要摆出来）
    pub can_file: bool,
}

/// 期间首末日
fn month_range(p: Period) -> (NaiveDate, NaiveDate) {
    let (y, m) = (p.year(), p.month());
    let first = NaiveDate::from_ymd_opt(y, m, 1)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2000, 1, 1).unwrap());
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    };
    let last = next.and_then(|d| d.pred_opt()).unwrap_or(first);
    (first, last)
}

/// 申报前体检。
///
/// 不写库、不改账，只读。
pub fn precheck(db: &Db, period: Period) -> DbResult<PrecheckReport> {
    let form = crate::taxdecl::vat_main_form(db, period)?;
    let (first, last) = month_range(period);
    let all = invoices::list(db, &InvoiceQuery::default())?;
    let mut items: Vec<CheckItem> = Vec::new();

    // ---------- 1. 票自身勾稽：不含税 + 税额 = 价税合计 ----------
    let mut broken: Vec<&Invoice> = Vec::new();
    for inv in &all {
        if (inv.amount + inv.tax - inv.amount_tax).round2() != Money::ZERO {
            broken.push(inv);
        }
    }
    if !broken.is_empty() {
        let names: Vec<String> = broken
            .iter()
            .take(3)
            .map(|i| format!("{}（{}）", i.number, i.amount_tax.fmt_money()))
            .collect();
        items.push(
            CheckItem::new(
                Level::Error,
                "invoice-sum-mismatch",
                format!("{} 张发票的「金额 + 税额 ≠ 价税合计」", broken.len()),
                format!(
                    "例：{}。这种票无论如何都不能申报，先把票面数字改正。",
                    names.join("、")
                ),
            )
            .with_amount(broken.iter().fold(Money::ZERO, |a, i| {
                a + (i.amount_tax - i.amount - i.tax).round2()
            })),
        );
    }

    // ---------- 2. 税率不可识别 ----------
    let odd: Vec<&Invoice> = all
        .iter()
        .filter(|i| !i.tax_rate.trim().is_empty() && !crate::taxdecl::is_known_rate(&i.tax_rate))
        .collect();
    if !odd.is_empty() {
        items.push(CheckItem::new(
            Level::Warn,
            "invoice-rate-unknown",
            format!("{} 张发票的税率不在常用档位（13/9/6/5/3/1/0）", odd.len()),
            format!(
                "例：{} —— {}%。税率录错会让申报表把它们归进「税率未填」那一行，须人工归类。",
                odd[0].number, odd[0].tax_rate
            ),
        ));
    }

    // ---------- 3. 本期未认证的进项票 ----------
    let unverified: Vec<&Invoice> = all
        .iter()
        .filter(|i| {
            i.kind == "in"
                && i.status != "verified"
                && i.status != "rejected"
                && in_month(i.date.as_str(), first, last)
        })
        .collect();
    if !unverified.is_empty() {
        let tax: Money = unverified.iter().map(|i| i.tax).sum();
        items.push(
            CheckItem::new(
                Level::Warn,
                "invoice-not-verified",
                format!("本期有 {} 张进项票尚未认证", unverified.len()),
                "未认证的进项票**不能抵扣**，申报表里不会出现它们。本月要抵就先认证；不抵则留到以后月份。",
            )
            .with_amount(tax),
        );
    }

    // ---------- 4. 跨期票（本期账上有税，票却不在本期） ----------
    let cross: Vec<&Invoice> = all
        .iter()
        .filter(|i| i.status == "verified" && !in_month(i.date.as_str(), first, last))
        .collect();
    if !cross.is_empty() {
        let tax: Money = cross.iter().map(|i| i.tax).sum();
        items.push(
            CheckItem::new(
                Level::Warn,
                "invoice-other-period",
                format!("有 {} 张已认证的进项/销项票不属于本期", cross.len()),
                format!(
                    "例：{}（{}）。跨期抵扣是否允许按当地税局口径，别自己拍板。",
                    cross[0].number, cross[0].date
                ),
            )
            .with_amount(tax),
        );
    }

    // ---------- 5. 作废/驳回的票仍留在台账里 ----------
    let voided: Vec<&Invoice> = all
        .iter()
        .filter(|i| i.status == "rejected" && in_month(i.date.as_str(), first, last))
        .collect();
    if !voided.is_empty() {
        items.push(CheckItem::new(
            Level::Info,
            "invoice-rejected",
            format!("本期有 {} 张票被标为作废/驳回", voided.len()),
            "已从申报表数值里排除；确认凭证也已冲销，否则账上会多一笔税。",
        ));
    }

    // ---------- 6. 应交增值税与「转出未交增值税」是否一致 ----------
    // 一次装载本期的已记账余额，后面 6/7/8 三节都用它。四节各自 `load` 一遍不仅慢，
    // 更要紧的是同一个判断里用了两个不同时刻的快照，对不上时会让人怀疑是函数写错了。
    let snap = {
        use crate::balances::{BalanceQuery, BalanceSnapshot};
        BalanceSnapshot::load(db, &BalanceQuery::period(period).with_posted_only(true))?
    };
    // 22210104 转出未交增值税：本期借方发生额
    let carry_out = snap.for_account("22210104", None).debit.round2();
    // 222102 未交增值税：期末贷方余额（= 本月应该交的）
    let payable_book = snap.for_account("222102", None).end_dir_amount().1;
    // 6403 税金及附加：本期借方发生额 = 实际计提的附加税
    let booked_surcharge = snap.for_account("6403", None).debit.round2();
    // 22210101 进项税额（借）/ 22210102 销项税额（贷）：本期发生额
    let book_in = snap.for_account("22210101", None).debit.round2();
    let book_out = snap.for_account("22210102", None).credit.round2();
    if form.payable > Money::ZERO && carry_out.is_zero() {
        items.push(
            CheckItem::new(
                Level::Warn,
                "vat-not-transferred",
                "本期有应交增值税，但没有做「转出未交增值税」",
                "月末这一步漏了：222101xx 三栏会挂着余额，222102 也是 0，下月缴税时无处可冲。",
            )
            .with_amount(form.payable),
        );
    } else if !carry_out.is_zero() && (carry_out - form.payable).round2() != Money::ZERO {
        items.push(
            CheckItem::new(
                Level::Error,
                "vat-transfer-mismatch",
                "转出未交增值税的金额与申报表应纳税额不一致",
                format!(
                    "账上转出 {}，申报表算出 {}。差 {}。",
                    carry_out.fmt_money(),
                    form.payable.fmt_money(),
                    (carry_out - form.payable).round2().fmt_money()
                ),
            )
            .with_amount((carry_out - form.payable).round2()),
        );
    } else if !carry_out.is_zero() && (payable_book - form.payable).round2() != Money::ZERO {
        items.push(
            CheckItem::new(
                Level::Warn,
                "vat-payable-balance",
                "222102 未交增值税余额与本期应纳税额不等",
                format!(
                    "余额 {}，本期应纳税额 {}。若上月留抵或上期未缴，属正常，但要能说清差在哪。",
                    payable_book.fmt_money(),
                    form.payable.fmt_money()
                ),
            )
            .with_amount((payable_book - form.payable).round2()),
        );
    }

    // ---------- 7. 附加税是否计提 ----------
    {
        let booked = booked_surcharge;
        let should = form.surcharge.total.round2();
        if should > Money::ZERO && booked.is_zero() {
            items.push(
                CheckItem::new(
                    Level::Warn,
                    "surcharge-not-accrued",
                    "本期没有计提附加税",
                    format!(
                        "按申报表应计提 {}（城建 + 教育 + 地方教育），账上 6403 本期为 0。",
                        should.fmt_money()
                    ),
                )
                .with_amount(should),
            );
        } else if should > Money::ZERO && (booked - should).round2() != Money::ZERO {
            items.push(
                CheckItem::new(
                    Level::Warn,
                    "surcharge-mismatch",
                    "计提的附加税与按应纳税额算出的不一致",
                    format!(
                        "账上 6403 本期 {}，申报表口径 {}。",
                        booked.fmt_money(),
                        should.fmt_money()
                    ),
                )
                .with_amount((booked - should).round2()),
            );
        }
    }

    // ---------- 8. 有票无账 / 有账无票（金额层面） ----------
    if form.input_tax > Money::ZERO && (book_in - form.input_tax).round2() != Money::ZERO {
        items.push(
                CheckItem::new(
                    Level::Error,
                    "input-tax-vs-ledger",
                    "进项税额：账上与票上不一致",
                    format!(
                        "账上 22210101 本期借方 {}，已认证票面合计 {}。差 {} —— 通常是票录了但凭证没记，或凭证记了但票没录。",
                        book_in.fmt_money(),
                        form.input_tax.fmt_money(),
                        (book_in - form.input_tax).round2().fmt_money()
                    ),
                )
                .with_amount((book_in - form.input_tax).round2()),
            );
    }
    if form.output_tax > Money::ZERO && (book_out - form.output_tax).round2() != Money::ZERO {
        items.push(
                CheckItem::new(
                    Level::Error,
                    "output-tax-vs-ledger",
                    "销项税额：账上与票上不一致",
                    format!(
                        "账上 22210102 本期贷方 {}，销项票面合计 {}。差 {}。若是未开票收入，必须手工填在申报表「未开票」两行，否则申报表少报收入。",
                        book_out.fmt_money(),
                        form.output_tax.fmt_money(),
                        (book_out - form.output_tax).round2().fmt_money()
                    ),
                )
                .with_amount((book_out - form.output_tax).round2()),
            );
    }

    // ---------- 9. 留抵提醒 ----------
    if form.payable < Money::ZERO {
        items.push(
            CheckItem::new(
                Level::Info,
                "vat-credit-carry",
                "本期进项大于销项，形成留抵",
                format!("留抵 {}，可结转下期继续抵扣。", (-form.payable).fmt_money()),
            )
            .with_amount(-form.payable),
        );
    }

    // ---------- 10. 取数自身的警告一并带出 ----------
    for w in &form.warnings {
        items.push(CheckItem::new(
            Level::Warn,
            "form-warning",
            "取数提示",
            w.clone(),
        ));
    }

    let errors = items.iter().filter(|i| i.level == Level::Error).count();
    let warns = items.iter().filter(|i| i.level == Level::Warn).count();
    let infos = items.iter().filter(|i| i.level == Level::Info).count();
    Ok(PrecheckReport {
        period: period.to_string(),
        items,
        errors,
        warns,
        infos,
        can_file: errors == 0,
    })
}

/// 日期字符串（`YYYY-MM-DD`）是否落在 `[first, last]` 内
fn in_month(date: &str, first: NaiveDate, last: NaiveDate) -> bool {
    NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map(|d| d >= first && d <= last)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invoices::Invoice;
    use crate::tests::mem;

    fn p() -> Period {
        Period::new(2026, 1).unwrap()
    }

    /// 造一张票。`net`/`tax`/`gross` 分开给，是为了造得出「金额 + 税额 ≠ 价税合计」的坏票。
    fn add_inv(
        db: &Db,
        kind: &str,
        date: &str,
        rate: &str,
        net: &str,
        tax: &str,
        gross: &str,
        status: &str,
    ) {
        let seq: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM invoice", [], |r| r.get::<_, i64>(0))
            .unwrap()
            + 1;
        crate::invoices::insert(
            db,
            &Invoice {
                id: 0,
                kind: kind.into(),
                code: "044001900111".into(),
                number: format!("{}{seq:04}", if kind == "in" { "IN" } else { "OUT" }),
                date: date.into(),
                buyer: "本公司".into(),
                seller: "对方".into(),
                amount_tax: Money::parse(gross).unwrap(),
                amount: Money::parse(net).unwrap(),
                tax: Money::parse(tax).unwrap(),
                tax_rate: rate.into(),
                status: status.into(),
                memo: String::new(),
                attach_id: 0,
                created_by: "李会计".into(),
                created_at: "2026-01-01 00:00:00".into(),
                updated_at: "2026-01-01 00:00:00".into(),
            },
            "李会计",
        )
        .unwrap();
    }

    fn post_voucher(db: &Db, day: u32, lines: Vec<(&str, Money, Money)>) -> i64 {
        let mut v = fincore::Voucher::new(
            p(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, day).unwrap(),
            "记",
            0,
        );
        v.prepared_by = "李会计".into();
        for (i, (code, dr, cr)) in lines.into_iter().enumerate() {
            let mut e = fincore::Entry::new((i + 1) as i32, code, "测试");
            e.debit = dr;
            e.credit = cr;
            v.entries.push(e);
        }
        let id = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, id, "李会计").unwrap();
        id
    }

    fn codes(r: &PrecheckReport) -> Vec<&'static str> {
        r.items.iter().map(|i| i.code).collect()
    }

    /// 空账套：不该凭空生出「错误」，只允许「提醒」
    #[test]
    fn empty_book_has_no_error() {
        let db = mem();
        let r = precheck(&db, p()).unwrap();
        assert_eq!(r.errors, 0, "空账套不该报错：{:?}", codes(&r));
        assert!(r.can_file, "没有错误项即可申报");
    }

    /// 未认证的进项票：提醒 + 带出税额，但不能算错 —— 会计可能就是不想本月抵
    #[test]
    fn unverified_input_invoice_is_warned_not_errored() {
        let db = mem();
        add_inv(
            &db,
            "in",
            "2026-01-08",
            "0.13",
            "1000",
            "130",
            "1130",
            "pending",
        );

        let r = precheck(&db, p()).unwrap();
        assert!(
            codes(&r).contains(&"invoice-not-verified"),
            "要提示未认证：{:?}",
            codes(&r)
        );
        let item = r
            .items
            .iter()
            .find(|i| i.code == "invoice-not-verified")
            .unwrap();
        assert_eq!(item.amount.unwrap().fmt_money(), "130.00");
        assert_eq!(item.level, Level::Warn);
        assert_eq!(r.errors, 0);
    }

    /// 票录了、凭证没记 —— 这是申报表最危险的一种「数字都对但互相矛盾」
    #[test]
    fn invoice_without_voucher_is_error() {
        let db = mem();
        add_inv(
            &db,
            "out",
            "2026-01-05",
            "0.13",
            "1000",
            "130",
            "1130",
            "verified",
        );
        // 账上什么都没记

        let r = precheck(&db, p()).unwrap();
        let item = r
            .items
            .iter()
            .find(|i| i.code == "output-tax-vs-ledger")
            .unwrap_or_else(|| panic!("应报销项账票不符：{:?}", codes(&r)));
        assert_eq!(item.level, Level::Error);
        assert_eq!(item.amount.unwrap().fmt_money(), "-130.00", "账 0 − 票 130");
        assert!(!r.can_file, "有错误项就不该放行申报");
    }

    /// 票与凭证金额一致时不该误报；但漏做「转出未交增值税」要提醒
    #[test]
    fn matched_voucher_passes_but_missing_transfer_is_warned() {
        let db = mem();
        add_inv(
            &db,
            "out",
            "2026-01-05",
            "0.13",
            "1000",
            "130",
            "1130",
            "verified",
        );
        post_voucher(
            &db,
            5,
            vec![
                ("1001", Money::parse("1130").unwrap(), Money::ZERO),
                ("600101", Money::ZERO, Money::parse("1000").unwrap()),
                ("22210102", Money::ZERO, Money::parse("130").unwrap()),
            ],
        );

        let r = precheck(&db, p()).unwrap();
        assert_eq!(r.errors, 0, "票账一致不该报错：{:?}", codes(&r));
        assert!(
            codes(&r).contains(&"vat-not-transferred"),
            "漏转出未交要提醒：{:?}",
            codes(&r)
        );
        assert!(r.can_file);
    }

    /// 票面自己就不勾稽的坏票，必须是「错误」而不是「提醒」
    #[test]
    fn broken_invoice_sum_is_error() {
        let db = mem();
        // 价税合计写成 1000，但 1000 + 130 应该是 1130
        add_inv(
            &db,
            "out",
            "2026-01-05",
            "0.13",
            "1000",
            "130",
            "1000",
            "verified",
        );

        let r = precheck(&db, p()).unwrap();
        let item = r
            .items
            .iter()
            .find(|i| i.code == "invoice-sum-mismatch")
            .unwrap_or_else(|| panic!("应报票面不勾稽：{:?}", codes(&r)));
        assert_eq!(item.level, Level::Error);
        assert!(!r.can_file);
    }

    /// 跨期的已认证票要单独点名：抵扣期次是税务上最容易出问题的地方
    #[test]
    fn cross_period_verified_invoice_is_warned() {
        let db = mem();
        add_inv(
            &db,
            "in",
            "2025-12-20",
            "0.13",
            "1000",
            "130",
            "1130",
            "verified",
        );

        let r = precheck(&db, p()).unwrap();
        assert!(
            codes(&r).contains(&"invoice-other-period"),
            "跨期票要点名：{:?}",
            codes(&r)
        );
        // 提醒不等于错
        assert_eq!(r.errors, 0);
    }
}
