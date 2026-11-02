//! 收款认领：一笔银行流水，认到多个客户、多张单上。
//!
//! 现实里的收款几乎**不一一对应**：客户 A 打过来 45,000，其中 15,000 是替 B 付的；
//! 或者一笔 100,000 同时清掉三张单；或者客户抹了零头。会计在电脑前做的事就是
//! 「这笔钱，认给谁、认多少」—— 然后系统要自动把两边接上。
//!
//! 本模块把这件事拆成三步（界面照这个顺序走）：
//!
//! 1. [`inflows`]：列出「收到的钱还没认完」的分录 —— 应收的**贷方**未清额；
//! 2. [`candidates`]：列出「还没收到钱」的挂账 —— 应收的**借方**未清额，
//!    以及 [`suggest`] 给出的贪心建议（先本客户、后跨客户，按日期先后）；
//! 3. [`apply`]：把选定的组合落库。同一客户走普通核销；不同客户自动出**过渡凭证**
//!    把贷方从「记错的那个人」挪到「真正该清的那个人」——
//!    这部分复用 [`crate::settle::settle_cross`]，口径与本系统别处完全一致。
//!
//! 为什么认领要单独一层而不是让会计直接点「核销」：核销是「两边金额相等」的机械配对，
//! 而认领是**判断**（这笔钱属于谁）。判断错了必须能一眼看见：所以过渡凭证上是
//! 借 [收款记的那个人] / 贷 [真正要清的那个人]，事后查得到。

use chrono::NaiveDate;
use fincore::{FinError, Money, Period};

use crate::settle::{self, CrossSettleResult, OpenEntry};
use crate::{Db, DbResult};

/// 一笔还没认完的收款（应收贷方分录）
#[derive(Clone, Debug)]
pub struct Inflow {
    pub entry_id: i64,
    pub voucher_id: i64,
    pub date: String,
    pub summary: String,
    /// 记账时挂在哪个客户名下（可能记错人，认领就是来纠正它的）
    pub party: String,
    /// 这笔收款总额
    pub amount: Money,
    /// 还没认领的金额
    pub open: Money,
}

/// 一笔还没收到钱的挂账（应收借方分录）
#[derive(Clone, Debug)]
pub struct Candidate {
    pub entry_id: i64,
    pub voucher_id: i64,
    pub date: String,
    pub summary: String,
    pub party: String,
    pub amount: Money,
    pub open: Money,
}

/// 一条认领明细：把这一笔挂账认掉多少
#[derive(Clone, Debug)]
pub struct Pick {
    pub entry_id: i64,
    pub amount: Money,
}

/// 认领结果
#[derive(Clone, Debug, Default)]
pub struct Claimed {
    /// 认掉的金额合计
    pub amount: Money,
    /// 落下的核销记录条数
    pub settles: usize,
    /// 自动生成并记账的过渡凭证（跨客户认领才有）
    pub bridges: Vec<i64>,
    /// 收款还剩多少没认
    pub leftover: Money,
}

/// 从 `aux_key`（形如 `customer=C01`）里取客户编码
fn party_of(e: &OpenEntry) -> String {
    e.aux_key
        .split('\u{1f}')
        .find_map(|seg| seg.split_once('=').map(|(k, v)| (k, v.to_string())))
        .filter(|(k, _)| *k == "customer" || *k == "supplier")
        .map(|(_, v)| v)
        .unwrap_or_default()
}

fn inflow_of(e: &OpenEntry) -> Inflow {
    Inflow {
        entry_id: e.entry_id,
        voucher_id: e.voucher_id,
        date: e.date.format("%Y-%m-%d").to_string(),
        summary: e.summary.clone(),
        party: party_of(e),
        amount: e.credit,
        open: e.open(),
    }
}

fn candidate_of(e: &OpenEntry) -> Candidate {
    Candidate {
        entry_id: e.entry_id,
        voucher_id: e.voucher_id,
        date: e.date.format("%Y-%m-%d").to_string(),
        summary: e.summary.clone(),
        party: party_of(e),
        amount: e.debit,
        open: e.open(),
    }
}

/// 收到的钱还没认完的分录（应收贷方、已记账、未清额 > 0）
pub fn inflows(db: &Db, account: &str, upto: Period) -> DbResult<Vec<Inflow>> {
    let mut out: Vec<Inflow> = settle::open_entries(db, account, upto, false)?
        .iter()
        .filter(|e| e.credit > Money::ZERO && e.open() > Money::ZERO)
        .map(inflow_of)
        .collect();
    // 先认最早的（钱压得越久越容易忘）
    out.sort_by(|a, b| a.date.cmp(&b.date).then(a.entry_id.cmp(&b.entry_id)));
    Ok(out)
}

/// 还没收到钱的挂账（应收借方、已记账、未清额 > 0）
pub fn candidates(db: &Db, account: &str, upto: Period) -> DbResult<Vec<Candidate>> {
    let mut out: Vec<Candidate> = settle::open_entries(db, account, upto, false)?
        .iter()
        .filter(|e| e.debit > Money::ZERO && e.open() > Money::ZERO)
        .map(candidate_of)
        .collect();
    out.sort_by(|a, b| a.date.cmp(&b.date).then(a.entry_id.cmp(&b.entry_id)));
    Ok(out)
}

/// 贪心建议：先清同一个客户的（这是「钱属于他」的最常见情况），
/// 再按日期清别人的（代收代付）。只做建议，不落库 —— 认领是判断，机器只排顺序。
pub fn suggest(inflow: &Inflow, cands: &[Candidate]) -> Vec<Pick> {
    let mut same: Vec<&Candidate> = cands.iter().filter(|c| c.party == inflow.party).collect();
    let mut other: Vec<&Candidate> = cands.iter().filter(|c| c.party != inflow.party).collect();
    same.sort_by(|a, b| a.date.cmp(&b.date).then(a.entry_id.cmp(&b.entry_id)));
    other.sort_by(|a, b| a.date.cmp(&b.date).then(a.entry_id.cmp(&b.entry_id)));

    let mut left = inflow.open;
    let mut picks = Vec::new();
    for c in same.into_iter().chain(other) {
        if left <= Money::ZERO {
            break;
        }
        let amount = if c.open <= left { c.open } else { left };
        if amount <= Money::ZERO {
            continue;
        }
        picks.push(Pick {
            entry_id: c.entry_id,
            amount,
        });
        left -= amount;
    }
    picks
}

/// 落地认领。
///
/// 每条 pick 都用 [`settle::settle_cross`]：同客户时它就是普通核销（不出凭证），
/// 不同客户时自动出过渡凭证并把贷方挪过去。过渡凭证是草稿，这里顺手记账 ——
/// 认领完还留一张草稿在账外，等于「界面说清完了、账上还挂着」。
pub fn apply(
    db: &Db,
    account: &str,
    inflow_entry: i64,
    picks: &[Pick],
    date: NaiveDate,
    who: &str,
) -> DbResult<Claimed> {
    if picks.is_empty() {
        return Err(FinError::msg("请至少勾选一笔要认领的挂账").into());
    }
    let inflow = settle::open_entries(db, account, Period::from_date(date), false)?
        .into_iter()
        .find(|e| e.entry_id == inflow_entry)
        // 认完之后这笔收款就不再有未认额，open_entries 里也就没有它了 ——
        // 报错要说清这一点，否则会计会以为钱丢了。
        .ok_or_else(|| FinError::msg("这笔收款没有可认金额（不存在、不在本账期内，或已经认完）"))?;
    if inflow.credit <= Money::ZERO {
        return Err(FinError::msg("选中分录不是收款（应收贷方）").into());
    }
    let open = inflow.open();
    let total: Money = picks.iter().fold(Money::ZERO, |acc, p| acc + p.amount);
    if total <= Money::ZERO {
        return Err(FinError::msg("认领金额必须大于零").into());
    }
    if total > open {
        return Err(FinError::msg(format!("认领合计 {total} 超过这笔收款的未认额 {open}")).into());
    }
    let mut out = Claimed {
        amount: Money::ZERO,
        settles: 0,
        bridges: Vec::new(),
        leftover: open - total,
    };
    for p in picks {
        let r: CrossSettleResult =
            settle::settle_cross(db, p.entry_id, inflow_entry, p.amount, date, who)?;
        out.amount += p.amount;
        out.settles += r.settle_ids.len();
        if let Some(bid) = r.bridge_voucher_id {
            crate::vouchers::post(db, bid, who)?;
            out.bridges.push(bid);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::mem;
    use fincore::{AuxRef, Entry, Voucher, VoucherSource};

    fn m(s: &str) -> Money {
        Money::parse(s).unwrap()
    }

    /// 记一张已记账凭证（借/贷两行，可带客户辅助）
    fn post(
        db: &Db,
        p: Period,
        day: &str,
        no: i32,
        debit: (&str, Option<&str>, &str),
        credit: (&str, Option<&str>, &str),
    ) -> i64 {
        let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
        let aux = |c: Option<&str>| AuxRef {
            customer: c.map(|s| s.to_string()),
            ..Default::default()
        };
        let mut v = Voucher::new(p, date, "记", no);
        v.source = VoucherSource::Manual;
        v.prepared_by = "u1".into();
        v.push_entry(Entry {
            debit: m(debit.2),
            aux: aux(debit.1),
            ..Entry::new(1, debit.0, "测试")
        });
        v.push_entry(Entry {
            credit: m(credit.2),
            aux: aux(credit.1),
            ..Entry::new(2, credit.0, "测试")
        });
        let id = crate::vouchers::save(db, &mut v).unwrap();
        crate::vouchers::post(db, id, "u1").unwrap();
        id
    }

    /// 一笔钱认给两个客户：同客户走普通核销，跨客户出过渡凭证。
    ///
    /// C01 的 45,000 里有 15,000 是替 C02 付的 —— 这正是演示账里那个场景。
    #[test]
    fn claim_splits_one_receipt_across_customers() {
        let db = mem();
        let p = Period::new(2026, 11).unwrap();
        // 两张应收：C01 45,000、C02 15,000
        post(
            &db,
            p,
            "2026-11-05",
            1,
            ("112201", Some("C01"), "45000"),
            ("600101", None, "45000"),
        );
        post(
            &db,
            p,
            "2026-11-06",
            2,
            ("112201", Some("C02"), "15000"),
            ("600101", None, "15000"),
        );
        // 收款 45,000 记在 C01 名下（现实里会计只知道钱到账了）
        post(
            &db,
            p,
            "2026-11-20",
            3,
            ("1001", None, "45000"),
            ("112201", Some("C01"), "45000"),
        );

        let ins = inflows(&db, "112201", p).unwrap();
        assert_eq!(ins.len(), 1, "只有一笔待认领的收款");
        assert_eq!(ins[0].open.fmt_money(), "45,000.00");
        assert_eq!(ins[0].party, "C01");

        let cands = candidates(&db, "112201", p).unwrap();
        assert_eq!(cands.len(), 2, "两张单都还挂着");

        // 建议：先清 C01 的 45,000（同一客户、日期最早）
        let picks = suggest(&ins[0], &cands);
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].amount.fmt_money(), "45,000.00");

        // 但会计知道其中 15,000 是 C02 的：手工改成 30,000 + 15,000
        let manual = vec![
            Pick {
                entry_id: cands.iter().find(|c| c.party == "C01").unwrap().entry_id,
                amount: m("30000"),
            },
            Pick {
                entry_id: cands.iter().find(|c| c.party == "C02").unwrap().entry_id,
                amount: m("15000"),
            },
        ];
        let date = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let got = apply(&db, "112201", ins[0].entry_id, &manual, date, "u1").unwrap();
        assert_eq!(got.amount.fmt_money(), "45,000.00");
        assert_eq!(got.leftover.fmt_money(), "0.00");
        assert_eq!(got.bridges.len(), 1, "跨客户那一笔要出过渡凭证");

        // 过渡凭证把贷方从 C01 挪到 C02：借 112201(C01) / 贷 112201(C02)
        let bv = crate::vouchers::get(&db, got.bridges[0]).unwrap().unwrap();
        assert_eq!(bv.entries.len(), 2);
        assert_eq!(bv.entries[0].aux.customer.as_deref(), Some("C01"));
        assert_eq!(bv.entries[0].debit.fmt_money(), "15,000.00");
        assert_eq!(bv.entries[1].aux.customer.as_deref(), Some("C02"));
        assert_eq!(bv.entries[1].credit.fmt_money(), "15,000.00");

        // 认领完：C02 那张 15,000 清干净了，收款也认完了；
        // C01 还挂着 15,000 —— 认给的只有 30,000，剩下的是真没收到的钱。
        assert!(inflows(&db, "112201", p).unwrap().is_empty(), "收款该认完");
        let left = candidates(&db, "112201", p).unwrap();
        assert_eq!(left.len(), 1, "只剩 C01 还挂着");
        assert_eq!(left[0].party, "C01");
        assert_eq!(left[0].open.fmt_money(), "15,000.00");
    }

    /// 认领金额不能超过这笔收款的未认额（否则会把别的收款的钱挪过来）
    #[test]
    fn cannot_over_claim() {
        let db = mem();
        let p = Period::new(2026, 11).unwrap();
        post(
            &db,
            p,
            "2026-11-05",
            1,
            ("112201", Some("C01"), "1000"),
            ("600101", None, "1000"),
        );
        post(
            &db,
            p,
            "2026-11-20",
            2,
            ("1001", None, "600"),
            ("112201", Some("C01"), "600"),
        );
        let ins = inflows(&db, "112201", p).unwrap();
        let cands = candidates(&db, "112201", p).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let bad = vec![Pick {
            entry_id: cands[0].entry_id,
            amount: m("800"),
        }];
        let e = apply(&db, "112201", ins[0].entry_id, &bad, date, "u1").unwrap_err();
        // 措辞要落在「这笔收款的未认额」上：认领上限是认领自己的语义，
        // 不能让底层的核销去报「贷方未核销额」——那是另一件事，会计会看糊。
        assert!(
            e.to_string().contains("未认额"),
            "要说清是这笔收款不够认：{e}"
        );
        // 空勾选也要拒绝
        assert!(apply(&db, "112201", ins[0].entry_id, &[], date, "u1").is_err());
    }
}
