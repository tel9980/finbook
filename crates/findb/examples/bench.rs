//! 性能基线：造一个「小工厂两年真实规模」的账套，然后逐项计时。
//!
//! 用法：
//!   cargo run --release --example bench -p findb -- /tmp/bench/book.fbk [凭证数]
//!
//! 目的不是跑分，是**找出真正慢的那几处**，并留下可复现的数字。小工厂的实际
//! 规模大概是：60 家客户、20 家供应商、两年约 2000 张凭证 / 6000 条分录。
//! 造到这个量级，首页、报表、档案页各自的代价就藏不住了。

use chrono::NaiveDate;
use fincore::chart::default_accounts;
use fincore::{AuxEntity, AuxKind, Entry, Money, Period, Voucher};
use findb::Db;
use std::env;
use std::path::PathBuf;
use std::time::Instant;

fn money(s: &str) -> Money {
    s.parse().unwrap()
}

fn main() {
    let path = PathBuf::from(env::args().nth(1).expect("usage: bench <path> [n]"));
    let target: usize = env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("fbk-wal"));
    let _ = std::fs::remove_file(path.with_extension("fbk-shm"));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }

    let opts = fincore::BookOptions {
        start_period: Period::new(2025, 1).unwrap(),
        company: "性能基准厂".into(),
        enable_audit: false,
        require_cashier: false,
        ..Default::default()
    };
    let db = Db::create_no_admin(&path, &opts).expect("建账");
    findb::accounts::import_many(&db, &default_accounts()).expect("科目");
    findb::reports::reset_cash_flow_items(&db).expect("现金流量项目");

    // 档案：60 客户 + 20 供应商 + 8 存货 + 3 银行
    let mut t = Instant::now();
    let mut customers = Vec::new();
    for i in 1..=60 {
        let code = format!("C{i:03}");
        let e = AuxEntity::new(AuxKind::Customer, &code, format!("客户{i}"));
        findb::auxs::insert(&db, &e).unwrap();
        customers.push(code);
    }
    let mut suppliers = Vec::new();
    for i in 1..=20 {
        let code = format!("S{i:03}");
        let e = AuxEntity::new(AuxKind::Supplier, &code, format!("供应商{i}"));
        findb::auxs::insert(&db, &e).unwrap();
        suppliers.push(code);
    }
    for i in 1..=8 {
        let e = AuxEntity::new(AuxKind::Item, &format!("HG{i:03}"), format!("存货{i}"));
        findb::auxs::insert(&db, &e).unwrap();
    }
    for (code, name) in [("B01", "G银行对公户"), ("B02", "N银行结算户"), ("B03", "微信钱包")] {
        findb::auxs::insert(&db, &AuxEntity::new(AuxKind::Bank, code, name)).unwrap();
    }
    println!("档案 91 条：{:?}", t.elapsed());

    // ── 造凭证 ────────────────────────────────────────────────
    // 每个月固定几张，其余按目标总数平摊；每张 2 条分录（一借一贷），
    // 与手工记账的真实形态一致。
    t = Instant::now();
    let months = 22; // 2025-01 .. 2026-10（跑到「真实当前月」，与网页首页同口径）
    let per_month = (target / months).max(8);
    let mut no = 0;
    let mut made = 0;
    for m in 0..months {
        let year = 2025 + m / 12;
        let month = (m % 12 + 1) as u32;
        let p = Period::new(year as i32, month).unwrap();
        for k in 0..per_month {
            let day = (k % 27 + 1) as u32;
            let date = NaiveDate::from_ymd_opt(year as i32, month, day).unwrap();
            no += 1;
            let mut v = Voucher::new(p, date, "记", no);
            let cust = &customers[(no as usize) % customers.len()];
            let supp = &suppliers[(no as usize) % suppliers.len()];
            match k % 5 {
                // 卖货挂应收
                0 => {
                    let amt = money(&format!("{}.00", 8000 + (no % 40) * 300));
                    let mut a = Entry::new(0, "112201", "卖货");
                    a.debit = amt;
                    a.aux.customer = Some(cust.clone());
                    let mut b = Entry::new(1, "600101", "卖货");
                    b.credit = amt;
                    v.entries = vec![a, b];
                }
                // 客户回款
                1 => {
                    let amt = money(&format!("{}.00", 5000 + (no % 30) * 200));
                    let mut a = Entry::new(0, "100201", "收款");
                    a.debit = amt;
                    a.aux.bank = Some("B01".into());
                    let mut b = Entry::new(1, "112201", "收款");
                    b.credit = amt;
                    b.aux.customer = Some(cust.clone());
                    v.entries = vec![a, b];
                }
                // 采购挂应付
                2 => {
                    let amt = money(&format!("{}.00", 3000 + (no % 20) * 250));
                    let mut a = Entry::new(0, "140301", "进货");
                    a.debit = amt;
                    a.aux.item = Some(format!("HG{:03}", (no % 8) + 1));
                    a.qty = Some(money("10"));
                    a.price = Some(money("500"));
                    let mut b = Entry::new(1, "220201", "进货");
                    b.credit = amt;
                    b.aux.supplier = Some(supp.clone());
                    v.entries = vec![a, b];
                }
                // 付货款
                3 => {
                    let amt = money(&format!("{}.00", 2000 + (no % 25) * 180));
                    let mut a = Entry::new(0, "220201", "付款");
                    a.debit = amt;
                    a.aux.supplier = Some(supp.clone());
                    let mut b = Entry::new(1, "100201", "付款");
                    b.credit = amt;
                    b.aux.bank = Some("B02".into());
                    v.entries = vec![a, b];
                }
                // 房租水电 / 工资记制造费用
                _ => {
                    let amt = money(&format!("{}.00", 1500 + (no % 10) * 120));
                    let mut a = Entry::new(0, "510105", "房租水电");
                    a.debit = amt;
                    let mut b = Entry::new(1, "100201", "房租水电");
                    b.credit = amt;
                    b.aux.bank = Some("B01".into());
                    v.entries = vec![a, b];
                }
            }
            let id = findb::vouchers::save(&db, &mut v).expect("存凭证");
            findb::vouchers::post(&db, id, "bench").expect("记账");
            made += 1;
        }
    }
    let mk = t.elapsed();
    println!(
        "凭证 {made} 张（{} 条分录）：{:?}  —— 每张 {:?}",
        made * 2,
        mk,
        mk / made as u32
    );

    // ── 结账：真实账套此刻一定已经结到上月 ──────────────────────
    // 当前期间 = 最后一个已结账期间的下一期。不结账的话首页永远算启用期间
    // （2025-01），测出来的「一年的趋势」只有一个月，毫无意义。
    t = Instant::now();
    let mut closed = 0;
    let mut p = Period::new(2025, 1).unwrap();
    while p < Period::new(2026, 10).unwrap() {
        match findb::periods::close(&db, p, "bench", false) {
            Ok(_) => closed += 1,
            Err(e) => {
                println!("结账 {p:?} 失败：{e}");
                break;
            }
        }
        p = p.next();
    }
    println!("结账 {closed} 期：{:?}", t.elapsed());

    // ── 计时 ──────────────────────────────────────────────────
    let cur = Period::new(2026, 10).unwrap();
    let as_of = NaiveDate::from_ymd_opt(2026, 10, 31).unwrap();
    let mut t = Instant::now();
    let ft = findb::advanced::financial_totals(&db, cur, cur, None).unwrap();
    println!("financial_totals        {:>10?}  净利 {}", t.elapsed(), ft.net_profit);

    t = Instant::now();
    let fa = findb::advanced::financial_analysis(&db, cur, None).unwrap();
    println!("financial_analysis      {:>10?}  成本动因 {}", t.elapsed(), fa.cost_drivers.len());

    t = Instant::now();
    let mp = findb::advanced::money_position(&db, cur, as_of, None).unwrap();
    println!(
        "money_position          {:>10?}  应收 {} / 应付 {}",
        t.elapsed(),
        mp.receivable,
        mp.payable
    );

    t = Instant::now();
    let ss = findb::vouchers::status_summary(&db, cur).unwrap();
    println!("status_summary          {:>10?}  {:?}", t.elapsed(), ss);

    let chart = findb::accounts::chart(&db).unwrap();
    let snap = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::period(cur),
    )
    .unwrap();
    t = Instant::now();
    let tb = snap.trial_balance(&chart);
    println!("trial_balance           {:>10?}  期末借 {}", t.elapsed(), tb.end_debit);

    t = Instant::now();
    let _s1 = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::period(cur),
    )
    .unwrap();
    println!("snapshot（单期）        {:>10?}", t.elapsed());

    t = Instant::now();
    let all = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::range(Period::new(2025, 1).unwrap(), cur),
    )
    .unwrap();
    println!(
        "snapshot（两年累计）    {:>10?}  科目 {} 行",
        t.elapsed(),
        all.raw_rows().len()
    );

    t = Instant::now();
    let ar = findb::settle::aging(
        &db,
        "112201",
        cur,
        as_of,
        &fincore::engine::aging::buckets_by_days(),
        None,
    )
    .unwrap();
    println!("aging(112201)           {:>10?}  客户 {}", t.elapsed(), ar.len());

    t = Instant::now();
    let open = findb::settle::open_entries(&db, "112201", cur, false).unwrap();
    println!("open_entries(112201)    {:>10?}  未核销 {}", t.elapsed(), open.len());

    t = Instant::now();
    let aux = findb::auxs::list(&db, &fincore::AuxQuery::default()).unwrap();
    println!("auxs::list（全部）      {:>10?}  档案 {}", t.elapsed(), aux.len());

    t = Instant::now();
    let br = findb::balances::list_begin(&db).unwrap();
    println!("list_begin              {:>10?}  期初 {}", t.elapsed(), br.len());

    println!(
        "库文件 {} KB",
        std::fs::metadata(&path).map(|m| m.len() / 1024).unwrap_or(0)
    );

    // ── 导入路径：整批一个事务 ──────────────────────────────────
    // 逐张 save 是「一张一个事务」（每张 3~6ms，多半花在 fsync 上）；
    // 导入走 import_vouchers，整批共用一个事务。两条路必须分开量，
    // 否则会得出「导入 2000 张要 11 秒」的错误结论。
    {
        let imp_path = std::path::Path::new(&path).with_extension("import.fbk");
        let _ = std::fs::remove_file(&imp_path);
        let idb = findb::Db::create_no_admin(&imp_path, &opts).unwrap();
        let mut csv = String::from("日期,凭证字,摘要,科目编码,借方,贷方\n");
        for i in 0..2000 {
            let day = format!("2026-10-{:02}", 1 + (i % 28));
            let v = 100.0 + i as f64;
            csv.push_str(&format!("{day},记,销售,600101,{v:.2},\n"));
            csv.push_str(&format!("{day},记,销售,112201,,{v:.2}\n"));
        }
        t = Instant::now();
        let r = findb::imports::import_vouchers(
            &idb,
            Period::new(2026, 10).unwrap(),
            &csv,
            "bench",
            &std::collections::HashMap::new(),
            findb::imports::ImportTemplate::Generic,
        )
        .unwrap();
        println!(
            "导入 2000 张凭证          {:>10?}  成功 {} / 跳过 {}  每张 {:.2}ms",
            t.elapsed(),
            r.ok,
            r.skipped,
            t.elapsed().as_secs_f64() * 1000.0 / (r.ok.max(1) as f64)
        );
        let _ = std::fs::remove_file(&imp_path);
        let _ = std::fs::remove_file(imp_path.with_extension("fbk-wal"));
        let _ = std::fs::remove_file(imp_path.with_extension("fbk-shm"));
    }
    let _ = snap;
}

