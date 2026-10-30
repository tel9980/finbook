//! 利润表与首页必须是同一个净利润。
//!
//! 小工厂把房租、水电、煤气、外发加工费直接记在 5101 制造费用，月末一次性
//! 「结转损益」到本年利润——这些支出**永远不经过 6401 主营业务成本**。
//!
//! 于是「营业成本只取 6401/6402」的利润表会说这个月赚了 100,000，而首页
//! （口径含未结转的成本类）说赚了 85,000。同一套账、同一个月、两个净利润，
//! 老板问哪个对，谁也答不上来。
//!
//! 这个文件守三件事，覆盖三种真实记法：
//!   1. 只记 5101、还没结转 → 两边都 85,000
//!   2. 已做「结转损益」 → 两边都 85,000（不能因为结转了就回到 100,000）
//!   3. 手工做「成本结转凭证」5101 → 5001 → 两边仍然一致（不能重复扣）

use fincore::report;
use fincore::{BookOptions, Entry, Money, Period, Voucher};
use findb::balances::{BalanceQuery, BalanceSnapshot};
use findb::Db;
use std::path::PathBuf;

fn m(s: &str) -> Money {
    Money::parse(s).unwrap()
}

fn book(tag: &str) -> (Db, PathBuf) {
    let dir = std::env::temp_dir().join(format!("finbook-income-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.fbk");
    let opts = BookOptions {
        code_scheme: vec![4, 2, 2, 2],
        start_period: Period::parse("2026-01").unwrap(),
        base_currency: "CNY".into(),
        company: "口径一致性厂".into(),
        enable_qty: false,
        enable_audit: false,
        require_audit: false,
        require_cashier: false,
        ..Default::default()
    };
    let db = Db::create_no_admin(&path, &opts).unwrap();
    (db, dir)
}

fn post(db: &Db, v: &mut Voucher) {
    let id = findb::vouchers::save(db, v).unwrap();
    findb::vouchers::post(db, id, "t").unwrap();
}

/// 卖货 100,000（挂应收）+ 房租 15,000 记 510105。
fn seed(db: &Db, p: Period) {
    let date = p.last_day();
    let mut v = Voucher::new(p, date, "记", 1);
    let mut e = Entry::new(1, "112201", "卖货");
    e.debit = m("100000");
    e.aux.customer = Some("C001".into());
    v.entries.push(e);
    let mut e = Entry::new(2, "600101", "卖货");
    e.credit = m("100000");
    v.entries.push(e);
    post(db, &mut v);

    let mut v2 = Voucher::new(p, date, "记", 2);
    let mut e = Entry::new(1, "510105", "厂租房租");
    e.debit = m("15000");
    v2.entries.push(e);
    let mut e = Entry::new(2, "100201", "厂租房租");
    e.credit = m("15000");
    e.aux.bank = Some("B01".into());
    v2.entries.push(e);
    post(db, &mut v2);
}

/// 按界面同一条路径取利润表净利润。
fn income_net(db: &Db, p: Period) -> Money {
    let def = report::income::income_statement_def();
    let bq = BalanceQuery::range(p, p).with_exclude_carry(true);
    let snap = BalanceSnapshot::load(db, &bq).unwrap();
    let table = report::render_single(&def, &snap, "口径一致性厂", "", report::identity);
    let row = table
        .rows
        .iter()
        .find(|r| r.name.contains("净利润"))
        .expect("利润表必须有净利润行");
    row.values.first().copied().unwrap_or(Money::ZERO)
}

fn dash_net(db: &Db, p: Period) -> Money {
    findb::advanced::financial_totals(db, p, p, None)
        .unwrap()
        .net_profit
}

fn assert_same(db: &Db, p: Period, case: &str) {
    let a = income_net(db, p);
    let b = dash_net(db, p);
    assert_eq!(
        a,
        b,
        "{case}：利润表净利润 {} 与首页净利润 {} 不一致——同一套账不能有两个净利润",
        a.fmt_plain(),
        b.fmt_plain()
    );
    assert_eq!(
        a,
        m("85000"),
        "{case}：净利润应为 85,000（收入 100,000 − 房租 15,000），实际 {}",
        a.fmt_plain()
    );
}

/// 只记 5101、还没结转。
#[test]
fn unclosed_manufacturing_cost_reaches_the_income_statement() {
    let (db, dir) = book("unclosed");
    let p = Period::parse("2026-01").unwrap();
    seed(&db, p);
    assert_same(&db, p, "未结转");
    let _ = std::fs::remove_dir_all(dir);
}

/// 做过「结转损益」之后仍然一致（结转凭证不能被算成费用减少）。
#[test]
fn closing_profit_and_loss_keeps_both_sides_equal() {
    let (db, dir) = book("closed");
    let p = Period::parse("2026-01").unwrap();
    seed(&db, p);

    let chart = findb::accounts::chart(&db).unwrap();
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p).with_posted_only(true)).unwrap();
    let rows = snap.carry_forward_rows(&chart);
    let mut cv = fincore::engine::generate_carry_forward(
        p,
        p.last_day(),
        "转",
        1,
        &rows,
        &chart,
        "4103",
        "t",
    )
    .unwrap();
    post(&db, &mut cv);

    assert_same(&db, p, "已结转损益");
    let _ = std::fs::remove_dir_all(dir);
}

/// 手工做「成本结转凭证」5101 → 5001：两边仍一致，且不能重复扣。
#[test]
fn cost_transfer_voucher_does_not_double_count() {
    let (db, dir) = book("transfer");
    let p = Period::parse("2026-01").unwrap();
    seed(&db, p);

    let mut cv = Voucher::new(p, p.last_day(), "记", 3);
    let mut e = Entry::new(1, "500101", "制造费用转入生产成本");
    e.debit = m("15000");
    cv.entries.push(e);
    let mut e = Entry::new(2, "510105", "制造费用转入生产成本");
    e.credit = m("15000");
    cv.entries.push(e);
    post(&db, &mut cv);

    assert_same(&db, p, "做过成本结转凭证");
    let _ = std::fs::remove_dir_all(dir);
}

/// 完整的成本流转 5101 → 5001 → 1405 → 6401 也不能重复扣：
/// 5101 借 15,000、贷 15,000 相抵，成本最终只在 6401 出现一次。
#[test]
fn full_cost_chain_counts_the_cost_exactly_once() {
    let (db, dir) = book("chain");
    let p = Period::parse("2026-01").unwrap();
    seed(&db, p);
    let date = p.last_day();

    // (凭证号, 借方科目, 贷方科目, 摘要)：制造费用转入生产成本、完工入库
    for (no, debit_code, credit_code, summary) in [
        (3, "500101", "510105", "制造费用转入生产成本"),
        (4, "140501", "500101", "完工入库"),
    ] {
        let mut v = Voucher::new(p, date, "记", no);
        let mut e = Entry::new(1, debit_code, summary);
        e.debit = m("15000");
        if debit_code.starts_with("1405") {
            e.qty = Some(m("1"));
            e.aux.item = Some("HG001".into());
        }
        v.entries.push(e);
        let mut e = Entry::new(2, credit_code, summary);
        e.credit = m("15000");
        if credit_code.starts_with("1405") {
            e.qty = Some(m("1"));
            e.aux.item = Some("HG001".into());
        }
        v.entries.push(e);
        post(&db, &mut v);
    }

    // 卖掉：1405 → 6401
    let mut v = Voucher::new(p, date, "记", 5);
    let mut e = Entry::new(1, "6401", "结转销售成本");
    e.debit = m("15000");
    v.entries.push(e);
    let mut e = Entry::new(2, "140501", "结转销售成本");
    e.credit = m("15000");
    e.qty = Some(m("1"));
    e.aux.item = Some("HG001".into());
    v.entries.push(e);
    post(&db, &mut v);

    assert_same(&db, p, "完整成本流转");
    let _ = std::fs::remove_dir_all(dir);
}
