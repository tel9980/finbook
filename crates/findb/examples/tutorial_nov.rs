//! 教程示例账套：恒亮氧化加工厂 2026 年 11 月一整个月，从录凭证到出报表。
//!
//! 用法: cargo run --release --example tutorial_nov -p findb 2>/dev/null
//!
//! 输出就是教程里那些「你应该看到」的数字，全部来自真实引擎，不是手算。

use fincore::report;
use fincore::{BookOptions, Entry, Money, Period, Voucher};
use findb::balances::{BalanceQuery, BalanceSnapshot};
use findb::Db;

fn m(s: &str) -> Money {
    Money::parse(s).unwrap()
}

fn main() {
    let dir = std::env::temp_dir().join("finbook-tutorial-nov");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.fbk");
    let opts = BookOptions {
        code_scheme: vec![4, 2, 2, 2],
        start_period: Period::parse("2026-11").unwrap(),
        base_currency: "CNY".into(),
        company: "恒亮氧化加工厂".into(),
        enable_qty: false,
        enable_audit: false,
        require_audit: false,
        require_cashier: false,
        ..Default::default()
    };
    let db = Db::create_no_admin(&path, &opts).unwrap();
    let p = Period::parse("2026-11").unwrap();

    // 档案：银行账户、客户、供应商、存货（正常建账时由「建账向导 / 批量粘贴」建好）
    findb::auxs::insert_all_presets(&db).unwrap();
    for (kind, code, name) in [
        (fincore::account::AuxKind::Customer, "C001", "华达五金"),
        (fincore::account::AuxKind::Supplier, "S001", "宏源氧化"),
        (fincore::account::AuxKind::Item, "RL001", "氧化辅料"),
    ] {
        let _ = findb::auxs::insert(&db, &fincore::AuxEntity::new(kind, code, name));
    }

    // 期初：银行 200,000、生产设备 40,000、实收资本 240,000
    for (code, yb) in [
        ("100201", "200000"),
        ("160101", "40000"),
        ("4001", "-240000"),
    ] {
        let mut r = findb::balances::BeginRow {
            id: 0,
            account_code: code.into(),
            aux: Default::default(),
            year_begin: m(yb),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        };
        if code.starts_with("1002") {
            r.aux.bank = Some("B01".into());
        }
        findb::balances::upsert_begin(&db, &r).unwrap();
    }

    // ── 11 月的 9 张凭证 ────────────────────────────────────────────
    // 1) 11-05 采购辅助材料，专票含税 11,300（13%）
    let mut v = Voucher::new(p, date(2026, 11, 5), "记", 1);
    // 存货科目必须填数量和存货档案：100 kg × 100 元/kg
    let mut buy = dr("140302", "采购辅助材料 100kg", m("10000"));
    buy.qty = Some(m("100"));
    buy.aux.item = Some("RL001".into());
    v.entries.push(buy);
    v.entries.push(dr("22210101", "进项税额", m("1300")));
    v.entries.push(cr_bank("付材料款", m("11300")));
    post(&db, &mut v);

    // 2) 11-08 付厂租房租 15,000
    let mut v = Voucher::new(p, date(2026, 11, 8), "记", 2);
    v.entries.push(dr("510105", "厂租房租", m("15000")));
    v.entries.push(cr_bank("付厂租房租", m("15000")));
    post(&db, &mut v);

    // 3) 11-10 水电费 8,000（没票）
    let mut v = Voucher::new(p, date(2026, 11, 10), "记", 3);
    v.entries.push(dr("510103", "生产水电费", m("8000")));
    v.entries.push(cr_bank("付水电费", m("8000")));
    post(&db, &mut v);

    // 4) 11-15 领用辅料投入生产 10,000（数量 100 kg）
    let mut v = Voucher::new(p, date(2026, 11, 15), "记", 4);
    let mut e = dr("500101", "领用辅助材料", m("10000"));
    e.qty = Some(m("100"));
    e.aux.item = Some("RL001".into());
    v.entries.push(e);
    let mut e = cr("140302", "领用辅助材料", m("10000"));
    e.qty = Some(m("100"));
    e.aux.item = Some("RL001".into());
    v.entries.push(e);
    post(&db, &mut v);

    // 5) 11-20 外发加工费 12,000（宏源，没票，挂应付）
    let mut v = Voucher::new(p, date(2026, 11, 20), "记", 5);
    v.entries.push(dr("510105", "外发加工费-宏源", m("12000")));
    v.entries.push(cr_supplier("外发加工费-宏源", m("12000")));
    post(&db, &mut v);

    // 6) 11-25 完工交货，开票收加工费含税 113,000（13%）
    let mut v = Voucher::new(p, date(2026, 11, 25), "记", 6);
    v.entries.push(dr_customer("华达五金加工费", m("113000")));
    v.entries.push(cr("600102", "氧化加工费", m("100000")));
    v.entries.push(cr("22210102", "销项税额", m("13000")));
    post(&db, &mut v);

    // 7) 11-26 收到华达货款 113,000
    let mut v = Voucher::new(p, date(2026, 11, 26), "记", 7);
    v.entries.push(dr_bank("收到华达货款", m("113000")));
    v.entries.push(cr_customer("收到华达货款", m("113000")));
    post(&db, &mut v);

    // 8) 11-28 付宏源外发加工费 12,000
    let mut v = Voucher::new(p, date(2026, 11, 28), "记", 8);
    v.entries.push(dr_supplier("付宏源加工费", m("12000")));
    v.entries.push(cr_bank("付宏源加工费", m("12000")));
    post(&db, &mut v);

    // 9) 11-30 计提 11 月工资：生产工人 15,000、管理人员 5,000（一张凭证）
    let mut v = Voucher::new(p, date(2026, 11, 30), "记", 9);
    v.entries.push(dr("510101", "生产工人工资", m("15000")));
    v.entries.push(dr("660201", "管理人员工资", m("5000")));
    v.entries.push(cr("221101", "计提 11 月工资", m("20000")));
    post(&db, &mut v);

    // 10) 11-30 计提折旧：生产 3,000、管理 1,000
    let mut v = Voucher::new(p, date(2026, 11, 30), "记", 10);
    v.entries.push(dr("510102", "生产设备折旧", m("3000")));
    v.entries.push(dr("660205", "管理折旧", m("1000")));
    v.entries.push(cr("1602", "计提折旧", m("4000")));
    post(&db, &mut v);

    println!("=== 结转前：试算平衡 ===");
    trial(&db, p);

    println!("\n=== 结转损益凭证 ===");
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
        "会计",
    )
    .unwrap();
    for e in &cv.entries {
        println!(
            "  {} {:<14} 借 {:>12} 贷 {:>12}",
            e.account_code,
            e.summary,
            e.debit.fmt_plain(),
            e.credit.fmt_plain()
        );
    }
    post(&db, &mut cv);

    println!("\n=== 利润表（2026-11）===");
    let def = report::income::income_statement_def();
    let bq = BalanceQuery::range(p, p).with_exclude_carry(true);
    let s2 = BalanceSnapshot::load(&db, &bq).unwrap();
    let t = report::render_single(
        &def,
        &s2,
        "恒亮氧化加工厂",
        "2026-11 至 2026-11",
        report::identity,
    );
    for r in &t.rows {
        let v = r.values.first().copied().unwrap_or(Money::ZERO);
        if !v.is_zero() {
            println!("  {:<40} {:>14}", r.name.trim(), v.fmt_plain());
        }
    }

    println!("\n=== 资产负债表（期末）===");
    let bdef = report::balance_sheet::balance_sheet_def();
    let s3 =
        BalanceSnapshot::load(&db, &BalanceQuery::range(Period::new(2026, 1).unwrap(), p)).unwrap();
    let bt = report::render(
        &bdef,
        &s3,
        "恒亮氧化加工厂",
        "2026-11 期末",
        vec![Box::new(report::identity), Box::new(report::to_begin)],
    );
    for r in &bt.rows {
        let v = r.values.first().copied().unwrap_or(Money::ZERO);
        if !v.is_zero() {
            println!("  {:<40} {:>14}", r.name.trim(), v.fmt_plain());
        }
    }

    println!("\n=== 首页「本期经营结果」口径 ===");
    let ft = findb::advanced::financial_totals(&db, p, p, None).unwrap();
    println!(
        "  营业收入 {}   营业成本 {}   净利润 {}",
        ft.revenue.fmt_plain(),
        ft.cost.fmt_plain(),
        ft.net_profit.fmt_plain()
    );
    println!("\n=== 库存（数量核算）===");
    let s4 = BalanceSnapshot::load(&db, &BalanceQuery::period(p)).unwrap();
    for code in ["140302", "500101"] {
        let r = s4.for_account(code, None);
        println!("  {code} 期末余额 {}", r.end().fmt_plain());
    }

    println!("\n=== 钱在哪 ===");
    let mp = findb::advanced::money_position(&db, p, p.last_day(), None).unwrap();
    println!(
        "  别人欠我 {}   我欠别人 {}   手头现金 {}   净头寸 {}",
        mp.receivable.fmt_plain(),
        mp.payable.fmt_plain(),
        mp.cash.fmt_plain(),
        mp.net().fmt_plain()
    );

    let _ = std::fs::remove_dir_all(dir);
}

fn date(y: i32, mo: u32, d: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, mo, d).unwrap()
}

fn dr(code: &str, summary: &str, amount: Money) -> Entry {
    let mut e = Entry::new(1, code, summary);
    e.debit = amount;
    e
}

fn cr(code: &str, summary: &str, amount: Money) -> Entry {
    let mut e = Entry::new(1, code, summary);
    e.credit = amount;
    e
}

fn dr_bank(summary: &str, amount: Money) -> Entry {
    let mut e = Entry::new(1, "100201", summary);
    e.debit = amount;
    e.aux.bank = Some("B01".into());
    e
}

fn cr_bank(summary: &str, amount: Money) -> Entry {
    let mut e = Entry::new(1, "100201", summary);
    e.credit = amount;
    e.aux.bank = Some("B01".into());
    e
}

fn dr_customer(summary: &str, amount: Money) -> Entry {
    let mut e = Entry::new(1, "112201", summary);
    e.debit = amount;
    e.aux.customer = Some("C001".into());
    e
}

fn cr_customer(summary: &str, amount: Money) -> Entry {
    let mut e = Entry::new(1, "112201", summary);
    e.credit = amount;
    e.aux.customer = Some("C001".into());
    e
}

fn cr_supplier(summary: &str, amount: Money) -> Entry {
    let mut e = Entry::new(1, "220201", summary);
    e.credit = amount;
    e.aux.supplier = Some("S001".into());
    e
}

fn dr_supplier(summary: &str, amount: Money) -> Entry {
    let mut e = Entry::new(1, "220201", summary);
    e.debit = amount;
    e.aux.supplier = Some("S001".into());
    e
}

fn post(db: &Db, v: &mut Voucher) {
    let id = findb::vouchers::save(db, v).unwrap();
    findb::vouchers::post(db, id, "会计").unwrap();
}

fn trial(db: &Db, p: Period) {
    let chart = findb::accounts::chart(db).unwrap();
    let snap = BalanceSnapshot::load(db, &BalanceQuery::period(p).with_posted_only(true)).unwrap();
    let tb = snap.trial_balance(&chart);
    println!(
        "  期初 借 {} 贷 {}   本期 借 {} 贷 {}",
        tb.begin_debit.fmt_plain(),
        tb.begin_credit.fmt_plain(),
        tb.period_debit.fmt_plain(),
        tb.period_credit.fmt_plain()
    );
}
