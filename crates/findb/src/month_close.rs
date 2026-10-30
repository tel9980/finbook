//! 月结向导：把「月末要做哪几件事」摊成一张清单
//!
//! 为什么要有它：`periods::precheck` 已经能告诉用户「现在不能结账，因为 X」，
//! 但它是一串错误文字的平铺——用户看完还是不知道**先做什么、做完了没**。
//! 小厂会计（往往就是老板娘）要的是一个打勾清单：哪几项已经好了、哪一项还差、
//! 差的那项点哪里去补。
//!
//! 所以这里把同一批判定拆成「步骤」：每步有 `ok` / `blocking` / `detail` / `hint`。
//! 判定口径与 `precheck` **必须一致**——`blockers_matches_precheck` 那条测试就是
//! 用来钉住这件事的（多一条少一条都会红）。

use crate::accounts;
use crate::balances::{BalanceQuery, BalanceSnapshot};
use crate::periods;
use crate::vouchers;
use crate::{Db, DbResult};
use fincore::Period;

/// 一个步骤
pub struct CloseStep {
    /// 稳定标识，前端用它决定按钮跳到哪一页
    pub code: &'static str,
    /// 清单上显示的短标题
    pub label: &'static str,
    /// 这一步是否已完成
    pub ok: bool,
    /// 没完成时是否**阻止**结账
    ///
    /// 「现金余额为负」这种是提醒，不是拦截：真的发生了（记错方向），
    /// 但拦住它并不能让用户更好地改正，只会让人去反结账绕开校验。
    pub blocking: bool,
    /// 现状描述，带数字
    pub detail: String,
    /// 该怎么处理
    pub hint: &'static str,
}

/// 整张清单
pub struct MonthClose {
    pub period: Period,
    /// 该期间是否已经结账
    pub closed: bool,
    pub steps: Vec<CloseStep>,
}

impl MonthClose {
    /// 所有「阻止性」步骤都通过，才算可以结账
    pub fn can_close(&self) -> bool {
        !self.closed && self.steps.iter().all(|s| !s.blocking || s.ok)
    }

    /// 还差哪几步（按清单顺序）
    pub fn blockers(&self) -> Vec<&CloseStep> {
        self.steps.iter().filter(|s| s.blocking && !s.ok).collect()
    }
}

/// 生成月结清单
pub fn report(db: &Db, p: Period) -> DbResult<MonthClose> {
    let opts = db.options();
    let closed_upto = periods::closed_upto(db)?;
    let closed = matches!(closed_upto, Some(u) if p <= u);

    let (drafts, audited, posted, _void) = vouchers::status_summary(db, p)?;
    let chart = accounts::chart(db)?;
    let snap = BalanceSnapshot::load(
        db,
        &BalanceQuery {
            from: p,
            to: p,
            ..BalanceQuery::period(p)
        },
    )?;
    let trial = snap.trial_balance(&chart);
    let pl_rows = snap.profit_loss_rows(&chart);

    let mut steps: Vec<CloseStep> = Vec::new();

    // ① 期间顺序：不能跳月，也不能对已结账的月份再结一次
    let (seq_ok, seq_detail) = match closed_upto {
        Some(u) if p <= u => (true, format!("{} 及以前期间已结账", p.label())),
        Some(u) if u.next() == p => (true, format!("上一期间 {} 已结账", u.label())),
        Some(u) => (
            false,
            format!(
                "{} 还没结账，不能跳过中间期间直接结 {}",
                u.next().label(),
                p.label()
            ),
        ),
        None if p == opts.start_period => (true, format!("首次结账从启用期间 {} 开始", p.label())),
        None => (
            false,
            format!("首次结账必须从启用期间 {} 开始", opts.start_period.label()),
        ),
    };
    steps.push(CloseStep {
        code: "sequence",
        label: "期间顺序",
        ok: seq_ok,
        blocking: true,
        detail: seq_detail,
        hint: "按月顺序结账；要改已结账的月份，先反结账再改。",
    });

    // ② 未记账凭证：结账后这些凭证会永远挂在该期间之外
    let unposted = drafts + audited;
    steps.push(CloseStep {
        code: "unposted",
        label: "凭证全部记账",
        ok: unposted == 0,
        blocking: true,
        detail: if unposted == 0 {
            format!("本期已记账 {posted} 张")
        } else {
            format!("还有 {unposted} 张未记账（草稿 {drafts} 张 / 已审核未记账 {audited} 张）")
        },
        hint: "到「凭证」页全选后点「记账」；草稿要先生成完整凭证。",
    });

    // ③ 试算平衡：期初 + 发生 + 期末三者都平
    let problems = trial.problems();
    steps.push(CloseStep {
        code: "trial",
        label: "试算平衡",
        ok: trial.is_balanced(),
        blocking: true,
        detail: if problems.is_empty() {
            "期初、本期发生、期末借贷均相等".to_string()
        } else {
            problems.join("；")
        },
        hint: "多半是某张凭证借贷不等或期初导入有错，先看「科目余额表」的合计行。",
    });

    // ④ 损益已结转
    let not_carried: Vec<String> = pl_rows
        .iter()
        .filter(|r| !(r.debit - r.credit).round2().is_zero())
        .map(|r| format!("{} {}", r.account_code, r.account_name))
        .collect();
    steps.push(CloseStep {
        code: "carry",
        label: "损益已结转",
        ok: not_carried.is_empty(),
        blocking: true,
        detail: if not_carried.is_empty() {
            "损益类科目本期发生额已结平".to_string()
        } else if not_carried.len() > 4 {
            format!(
                "{} 个损益类科目还没结转（如 {}）",
                not_carried.len(),
                not_carried[..3].join("、")
            )
        } else {
            format!("还没结转：{}", not_carried.join("、"))
        },
        hint: "点「结转损益」自动生成结转凭证。结转后利润表才有数。",
    });

    // ⑤ 成本类（5101 制造费用 / 5201 劳务成本）已转出
    //
    // 这一项原来漏了：成本类科目既不在 `is_profit_loss()` 里，也不进资产负债表
    // 的存货项时，会在账上永久挂着，而利润表看得见、余额表看不见它被结转。
    let mut cost_left: Vec<String> = Vec::new();
    for a in chart.all() {
        if a.category != fincore::AcctCategory::Cost || !chart.is_leaf(&a.code) {
            continue;
        }
        let r = snap.for_account(&a.code, None);
        if !(r.debit - r.credit).round2().is_zero() {
            cost_left.push(format!("{} {}", a.code, a.name));
        }
    }
    steps.push(CloseStep {
        code: "cost",
        label: "成本类已转出",
        ok: cost_left.is_empty(),
        blocking: true,
        detail: if cost_left.is_empty() {
            "制造费用等成本类科目已随损益一并转出".to_string()
        } else {
            format!("还挂在账上：{}", cost_left.join("、"))
        },
        hint: "「结转损益」会连成本类一起转出（房租水电外发加工费都该进当月成本）。",
    });

    // ⑥ 现金/银行余额为负：不拦，但一定要说
    let mut neg: Vec<String> = Vec::new();
    for a in chart.all() {
        if !(a.is_cash || a.is_bank) || !chart.is_leaf(&a.code) {
            continue;
        }
        let r = snap.for_account(&a.code, None);
        if r.end() < fincore::Money::ZERO {
            neg.push(format!(
                "{} {} 期末 {}",
                a.code,
                a.name,
                r.end().fmt_money()
            ));
        }
    }
    steps.push(CloseStep {
        code: "negative_cash",
        label: "现金不为负",
        ok: neg.is_empty(),
        blocking: false,
        detail: if neg.is_empty() {
            "现金与银行存款期末均为非负".to_string()
        } else {
            neg.join("；")
        },
        hint: "负余额说明有收支记错了方向或缺了收款凭证，结账前先补上更省事。",
    });

    Ok(MonthClose {
        period: p,
        closed,
        steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;

    fn p(y: i32, m: u32) -> Period {
        Period::new(y, m).unwrap()
    }

    /// 空账套的第一期：只有「未记账/试算/结转」以外的东西都该通过
    #[test]
    fn empty_first_period_is_ready_to_close() {
        let db = mem();
        let mc = report(&db, p(2026, 1)).unwrap();
        assert!(!mc.closed);
        assert!(mc.can_close(), "空账套应该可以结账");
        let codes: Vec<&str> = mc.steps.iter().map(|s| s.code).collect();
        assert_eq!(
            codes,
            vec![
                "sequence",
                "unposted",
                "trial",
                "carry",
                "cost",
                "negative_cash"
            ]
        );
    }

    /// 清单的「阻止项」必须与 `precheck` 的空/非空完全一致
    ///
    /// 两条判定路径各自演化是这类清单最典型的腐烂方式：向导说「可以结了」，
    /// 点下去结账却报一串错。
    #[test]
    fn blockers_matches_precheck() {
        let db = mem();
        let p1 = p(2026, 1);
        let mc = report(&db, p1).unwrap();
        let pre = periods::precheck(&db, p1, true).unwrap();
        assert_eq!(
            mc.can_close(),
            pre.is_empty(),
            "向导与 precheck 判定不一致：blockers={:?} precheck={:?}",
            mc.blockers().iter().map(|s| s.code).collect::<Vec<_>>(),
            pre
        );
    }

    /// 记了账但没结转损益：向导必须拦住（与 web/桌面预检同口径 require_carry=true）
    #[test]
    fn uncarried_profit_blocks_closing() {
        let db = mem();
        let p1 = p(2026, 1);
        let mut e1 = fincore::voucher::Entry::new(1, "100201", "收款");
        e1.debit = fincore::Money::parse("100").unwrap();
        e1.aux.bank = Some("B01".into());
        let mut e2 = fincore::voucher::Entry::new(2, "600102", "收入");
        e2.credit = fincore::Money::parse("100").unwrap();
        let mut v = fincore::Voucher::new(
            p1,
            chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
            "记",
            1,
        );
        v.entries = vec![e1, e2];
        v.memo = "test".to_string();
        v.status = fincore::voucher::VoucherStatus::Posted;
        crate::vouchers::save(&db, &mut v).unwrap();

        let mc = report(&db, p1).unwrap();
        let carried = mc.steps.iter().find(|s| s.code == "carry").unwrap();
        assert!(!carried.ok, "收入未结转时应报未结转");
        assert!(!mc.can_close(), "未结转损益不能结账");
        assert!(mc.blockers().iter().any(|s| s.code == "carry"));
    }

    /// 已结账的期间：向导只报告状态，不能再结一次
    #[test]
    fn closed_period_cannot_be_closed_again() {
        let db = mem();
        let p1 = p(2026, 1);
        periods::close(&db, p1, "t", false).unwrap();
        let mc = report(&db, p1).unwrap();
        assert!(mc.closed);
        assert!(!mc.can_close());
        let seq = mc.steps.iter().find(|s| s.code == "sequence").unwrap();
        assert!(seq.ok, "已结账本身不算「顺序错」");
        assert!(seq.detail.contains("已结账"));
    }
}
