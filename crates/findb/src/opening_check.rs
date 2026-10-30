//! 期初导入**之后**的对账视图。
//!
//! 导入「成功」不等于导入「对」。`opening_book` 保证的是「这份表自洽且能落库」
//! （借贷平、科目存在、编码不重复）；这一层管的是另一件事：**落库之后，账上是不是
//! 真的变成了表里写的那个样子**。
//!
//! 做法是把四条**互相独立**的线索摆在一起，任何两条差一个数都报出来：
//!
//! | 线索 | 来源 | 它单独看是对的、合起来不对的典型情形 |
//! |------|------|------------------------------------|
//! | ① 往来台账 | `arap_opening` | 只导了台账没导科目 → 账龄表有欠款、报表里应收是 0 |
//! | ② 科目期初 | `begin_balance` | 只导了科目没导台账 → 报表有数、催款单没人 |
//! | ③ 借贷合计 | ② 的正负 | 期初一开局试算就不平 |
//! | ④ 资产负债结构 | ② 按类别汇总 | 漏填实收资本 → 资产 > 负债+权益 |
//!
//! 这四条以前分散在「试算平衡」「资产负债表」「往来核销」三个页面里，用户得自己
//! 心算对齐；期初恰恰是**只做一次、做错要推倒重来**的地方，不该靠心算。

use std::collections::BTreeMap;

use fincore::account::AcctCategory;
use fincore::money::Money;

use crate::{Db, DbResult};

/// 一张纸落库后的样子。
#[derive(Debug, Clone, Default)]
pub struct SheetLine {
    /// 「① 客户应收」
    pub label: String,
    /// 行数（往来是户数 / 科目是科目数）
    pub rows: usize,
    /// 金额合计（一律取绝对值，方便和表里的正数对）
    pub total: Money,
    /// 一句话说明它落到了哪里
    pub hint: String,
}

/// 一条交叉校验。
#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    /// 带数字的说明：`台账 3 家 12,000.00；科目 1122 合计 12,000.00`
    pub detail: String,
}

/// 期初对账结果。
#[derive(Debug, Clone, Default)]
pub struct BeginReconcile {
    pub sheets: Vec<SheetLine>,
    /// 借方合计（资产类期初）
    pub debit_total: Money,
    /// 贷方合计（负债 + 权益）
    pub credit_total: Money,
    pub balanced: bool,
    pub asset_total: Money,
    pub liab_total: Money,
    pub equity_total: Money,
    pub checks: Vec<Check>,
    /// 给页面直接显示的问题清单（红字）
    pub issues: Vec<String>,
    /// 一条期初都没有（还没导）
    pub empty: bool,
}

fn fmt(m: Money) -> String {
    m.fmt_plain()
}

/// 科目编码前缀是否落在往来科目上。
fn is_ar(code: &str) -> bool {
    code.starts_with("1122")
}

fn is_ap(code: &str) -> bool {
    code.starts_with("2202")
}

/// 按账套参数里的往来科目判断更准（可能被改过），退回按标准编码判断。
fn ar_prefix(opts: &fincore::account::BookOptions) -> String {
    let c = &opts.biz_accounts.ar;
    if c.is_empty() {
        "1122".to_string()
    } else {
        c.clone()
    }
}

fn ap_prefix(opts: &fincore::account::BookOptions) -> String {
    let c = &opts.biz_accounts.ap;
    if c.is_empty() {
        "2202".to_string()
    } else {
        c.clone()
    }
}

/// 期初对账：把台账、科目期初、借贷平衡、资产负债结构四条线索摆到一起。
pub fn reconcile_begin(db: &Db) -> DbResult<BeginReconcile> {
    let opts = db.options();
    let ar_p = ar_prefix(&opts);
    let ap_p = ap_prefix(&opts);

    let begins = crate::balances::list_begin(db)?;
    let ar_ledger = crate::settle::arap_opening_list(db, Some("ar"))?;
    let ap_ledger = crate::settle::arap_opening_list(db, Some("ap"))?;

    // ── 科目期初按前缀归堆 ──────────────────────────────────────────
    // 注意：这里**不复用**任何聚合函数，直接从 begin_balance 逐行重算 ——
    // 对账的意义就是拿一条独立路径去验另一条，复用同一条路径等于自己验自己。
    let mut ar_booked = Money::ZERO;
    let mut ap_booked = Money::ZERO;
    let mut acct_rows = 0usize;
    let mut acct_total = Money::ZERO;
    let mut eq_rows = 0usize;
    let mut eq_total = Money::ZERO;
    let mut debit = Money::ZERO;
    let mut credit = Money::ZERO;

    let chart = crate::accounts::chart(db).ok();
    let mut cat_sum: BTreeMap<&'static str, Money> = BTreeMap::new();

    for b in &begins {
        let code = &b.account_code;
        let amt = b.year_begin;
        if code.starts_with(&ar_p) {
            ar_booked += amt;
        } else if code.starts_with(&ap_p) {
            ap_booked += -amt;
        } else if code.starts_with('1') || code.starts_with('2') {
            acct_rows += 1;
            acct_total += if amt < Money::ZERO { -amt } else { amt };
        } else if code.starts_with('4') {
            eq_rows += 1;
            eq_total += if amt < Money::ZERO { -amt } else { amt };
        }
        if amt > Money::ZERO {
            debit += amt;
        } else {
            credit += -amt;
        }

        // 按科目类别汇总（资产/负债/权益），类别取自科目表而不是编码前缀
        let cat = chart.as_ref().and_then(|c| c.get(code)).map(|a| a.category);
        let key = match cat {
            Some(AcctCategory::Asset) | Some(AcctCategory::Common) => "asset",
            Some(AcctCategory::Liability) => "liab",
            Some(AcctCategory::Equity) => "equity",
            _ => {
                // 科目表里找不到（理论上不该：导入时查过），按方向归资产/负债
                if amt >= Money::ZERO {
                    "asset"
                } else {
                    "liab"
                }
            }
        };
        // 资产按「借正贷负」直接累加；负债/权益的余额方向是贷，所以要取反 ——
        // 这样 4103 本年利润 记成借方（亏损）时会变成负数，不会把权益越加越大。
        let add = match key {
            "asset" => amt,
            _ => -amt,
        };
        *cat_sum.entry(key).or_insert(Money::ZERO) += add;
    }

    let asset_total = *cat_sum.get("asset").unwrap_or(&Money::ZERO);
    let liab_total = *cat_sum.get("liab").unwrap_or(&Money::ZERO);
    let equity_total = *cat_sum.get("equity").unwrap_or(&Money::ZERO);

    // ── 四张纸的落库情况 ────────────────────────────────────────────
    let mut sheets = Vec::new();
    let ar_ledger_total = ar_ledger.iter().fold(Money::ZERO, |s, r| s + r.amount);
    let ap_ledger_total = ap_ledger.iter().fold(Money::ZERO, |s, r| s + r.amount);
    let ar_parties = {
        let mut m = std::collections::BTreeSet::new();
        for r in &ar_ledger {
            m.insert(r.party_code.clone());
        }
        m.len()
    };
    let ap_parties = {
        let mut m = std::collections::BTreeSet::new();
        for r in &ap_ledger {
            m.insert(r.party_code.clone());
        }
        m.len()
    };
    sheets.push(SheetLine {
        label: "① 客户应收".into(),
        rows: ar_ledger.len(),
        total: ar_ledger_total,
        hint: format!("{ar_parties} 家客户，落到往来台账 + 科目 {ar_p}"),
    });
    sheets.push(SheetLine {
        label: "② 供应商应付".into(),
        rows: ap_ledger.len(),
        total: ap_ledger_total,
        hint: format!("{ap_parties} 家供应商，落到往来台账 + 科目 {ap_p}"),
    });
    sheets.push(SheetLine {
        label: "③ 账户余额".into(),
        rows: acct_rows,
        total: acct_total,
        hint: "现金/银行/存货等非往来科目".into(),
    });
    sheets.push(SheetLine {
        label: "④ 基本资料".into(),
        rows: eq_rows,
        total: eq_total,
        hint: "实收资本 / 本年利润".into(),
    });

    // ── 交叉校验 ────────────────────────────────────────────────────
    let mut checks = Vec::new();
    let mut issues = Vec::new();

    checks.push(Check {
        name: "往来台账 ↔ 应收科目余额".into(),
        ok: ar_ledger_total == ar_booked,
        detail: format!(
            "台账 {} 笔 {}；科目 {} 合计 {}",
            ar_ledger.len(),
            fmt(ar_ledger_total),
            ar_p,
            fmt(ar_booked)
        ),
    });
    checks.push(Check {
        name: "往来台账 ↔ 应付科目余额".into(),
        ok: ap_ledger_total == ap_booked,
        detail: format!(
            "台账 {} 笔 {}；科目 {} 合计 {}",
            ap_ledger.len(),
            fmt(ap_ledger_total),
            ap_p,
            fmt(ap_booked)
        ),
    });
    checks.push(Check {
        name: "期初借贷相等".into(),
        ok: debit == credit,
        detail: format!("借方 {}；贷方 {}", fmt(debit), fmt(credit)),
    });
    checks.push(Check {
        name: "资产 = 负债 + 权益".into(),
        ok: asset_total == liab_total + equity_total,
        detail: format!(
            "资产 {}；负债 {} + 权益 {} = {}",
            fmt(asset_total),
            fmt(liab_total),
            fmt(equity_total),
            fmt(liab_total + equity_total)
        ),
    });

    // 往来单位编码是否都在档案里（编码笔误在这里才抓得住 —— 导入时只校验科目）
    let mut missing: Vec<String> = Vec::new();
    for (kind, list) in [
        (fincore::AuxKind::Customer, &ar_ledger),
        (fincore::AuxKind::Supplier, &ap_ledger),
    ] {
        for r in list.iter() {
            if crate::auxs::get(db, kind, &r.party_code)?.is_none() {
                missing.push(format!("{}({})", r.party_code, r.party_name));
            }
        }
    }
    missing.sort();
    missing.dedup();
    checks.push(Check {
        name: "往来单位编码都在档案里".into(),
        ok: missing.is_empty(),
        detail: if missing.is_empty() {
            "全部能对上".into()
        } else {
            format!("档案里找不到：{}", missing.join("、"))
        },
    });

    // 数量核算科目：有金额没数量。
    //
    // 这不是格式问题，是**以后一定会算错**的问题：出库按数量结转成本，期初数量是 0 /
    // 空，第一次出库就把结存算成负数。而期初那一栏看起来「金额是对的」，没人会去
    // 数量金额式明细里确认一眼。
    let chart = crate::accounts::chart(db)?;
    let mut no_qty: Vec<String> = Vec::new();
    for r in crate::balances::list_begin(db)?.iter() {
        if r.year_begin.is_zero() {
            continue;
        }
        let Some(a) = chart.get(&r.account_code) else {
            continue;
        };
        if a.has_qty && r.qty_begin.is_none() {
            no_qty.push(format!("{}({})", r.account_code, a.name));
        }
    }
    no_qty.sort();
    no_qty.dedup();
    checks.push(Check {
        name: "数量核算科目都填了数量".into(),
        ok: no_qty.is_empty(),
        detail: if no_qty.is_empty() {
            "有金额的存货科目都带了数量".into()
        } else {
            format!(
                "有金额没数量：{} —— 在 ③ 表填「数量」列；或者改用「期初库存」导入数量流水",
                no_qty.join("、")
            )
        },
    });

    // 已经有期初凭证又录了 begin_balance → 很可能重复
    let dup_voucher = crate::vouchers::list(
        db,
        &crate::vouchers::VoucherQuery {
            from: Some(opts.start_period),
            to: Some(opts.start_period),
            ..Default::default()
        },
    )?
    .into_iter()
    .filter(|v| v.entries.iter().any(|e| e.summary.contains("期初")))
    .count();
    checks.push(Check {
        name: "没有重复的期初凭证".into(),
        ok: dup_voucher == 0,
        detail: if dup_voucher == 0 {
            "未发现摘要含「期初」的凭证".into()
        } else {
            format!(
                "启用期有 {dup_voucher} 张摘要含「期初」的凭证 —— 若期初已在本页录入，请删除凭证，否则双计"
            )
        },
    });

    for c in &checks {
        if !c.ok {
            issues.push(format!("{}：{}", c.name, c.detail));
        }
    }

    let empty = begins.is_empty() && ar_ledger.is_empty() && ap_ledger.is_empty();

    Ok(BeginReconcile {
        sheets,
        debit_total: debit,
        credit_total: credit,
        balanced: debit == credit,
        asset_total,
        liab_total,
        equity_total,
        checks,
        issues,
        empty,
    })
}

/// 供页面用的一行摘要（「四张纸 + 七项校验」都过时说一句话）。
pub fn one_liner(r: &BeginReconcile) -> String {
    if r.empty {
        return "还没有期初数据 —— 先下载期初收集表填好导入".into();
    }
    if r.issues.is_empty() {
        format!(
            "期初对上了：借方 {} = 贷方 {}，{} 项校验全过",
            fmt(r.debit_total),
            fmt(r.credit_total),
            r.checks.len()
        )
    } else {
        format!("期初有 {} 处不对，见下方红字", r.issues.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_follow_book_options() {
        let mut o = fincore::account::BookOptions::default();
        o.biz_accounts.ar = "112201".into();
        o.biz_accounts.ap = "220201".into();
        assert_eq!(ar_prefix(&o), "112201");
        assert_eq!(ap_prefix(&o), "220201");
        // 空串退回标准前缀，不能变成「所有科目都算往来」
        o.biz_accounts.ar = String::new();
        assert_eq!(ar_prefix(&o), "1122");
    }

    #[test]
    fn ar_ap_prefix_matching() {
        assert!(is_ar("112201"));
        assert!(!is_ar("112301"));
        assert!(is_ap("220201"));
        assert!(!is_ap("220301"));
    }

    #[test]
    fn empty_book_says_so() {
        let r = BeginReconcile {
            empty: true,
            ..Default::default()
        };
        assert!(one_liner(&r).contains("还没有期初数据"));
    }
}
