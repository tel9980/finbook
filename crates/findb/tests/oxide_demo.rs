//! 氧化加工厂演示账套的验收测试：生成器产出的账必须满足这些断言。
//!
//! 覆盖用户要的全部效果：28 张全已记账、试算平衡、往来余额符合设计
//! （A 剩 2700 / B 预收 30000 / C 欠 39140）、本月利润 53817.60、
//! 资产负债表左右相等。改坏生成器时这里先挂。

#[path = "../examples/oxide.rs"]
mod oxide;

use fincore::{Direction, Money};
use std::path::PathBuf;

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

fn build_book() -> PathBuf {
    build_book_named("oxide_demo")
}

fn build_book_named(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{}_{}.fbk", name, std::process::id()));
    let _ = std::fs::remove_file(&path);
    oxide::build(&path);
    path
}

#[test]
fn 演示账28张全已记账且试算平衡() {
    let path = build_book();
    let db = findb::Db::open(&path).expect("打开演示账");

    // 1) 28 张，一个不能少、一张不能是草稿
    let list =
        findb::vouchers::list(&db, &findb::vouchers::VoucherQuery::default()).expect("查凭证");
    assert_eq!(list.len(), 28, "凭证数不对");
    assert!(
        list.iter()
            .all(|v| v.status == fincore::VoucherStatus::Posted),
        "必须全部已记账"
    );

    // 2) 试算平衡（期初/本期/期末三组全平）
    let chart = findb::accounts::chart(&db).expect("读科目表");
    let p = fincore::Period::new(2026, 11).unwrap();
    let snap =
        findb::balances::BalanceSnapshot::load(&db, &findb::balances::BalanceQuery::range(p, p))
            .expect("取余额快照");
    let tb = snap.trial_balance(&chart);
    assert!(tb.begin_balanced(), "期初不平");
    assert!(tb.period_balanced(), "本期发生不平");
    assert!(tb.end_balanced(), "期末不平");

    // 3) 所有科目符号余额之和为 0（资产负债表左右相等的另一面）
    let total: Money = snap.raw_rows().iter().map(|r| r.end()).sum();
    assert!(total.is_zero(), "左右不相等：{total:?}");

    // 4) 关键余额（方向 + 金额逐个锁）
    let end_of = |code: &str| snap.for_account(code, None).end_dir_amount();
    assert_eq!(end_of("1001"), (Direction::Debit, m("30200")));
    assert_eq!(end_of("100201"), (Direction::Debit, m("249760")));
    assert_eq!(end_of("100202"), (Direction::Debit, m("31400")));
    assert_eq!(end_of("112201"), (Direction::Debit, m("11840")));
    assert_eq!(end_of("140302"), (Direction::Debit, m("31300")));
    assert_eq!(end_of("220201"), (Direction::Credit, m("44000")));
    assert_eq!(end_of("222102"), (Direction::Credit, m("5520")));
    assert_eq!(end_of("222103"), (Direction::Credit, m("386.40")));
    assert_eq!(end_of("222104"), (Direction::Credit, m("165.60")));
    assert_eq!(end_of("222105"), (Direction::Credit, m("110.40")));
    assert_eq!(end_of("222107"), (Direction::Credit, m("500")));
    assert_eq!(end_of("4001"), (Direction::Credit, m("250000")));
    // 本月利润：184000 − (116800 + 12600 + 120 + 662.40) = 53817.60
    assert_eq!(end_of("4103"), (Direction::Credit, m("53817.60")));

    // 5) 已结转归零的科目必须真是零
    for code in [
        "112101", "221101", "600101", "6401", "6403", "660201", "660202", "660204", "660303",
        "510101", "510103", "510105", "510106", "510107", "510108",
    ] {
        let e = snap.for_account(code, None).end();
        assert!(e.is_zero(), "{code} 应已结平，实际 {e:?}");
    }

    // 6) 往来按客户/供应商明细（不一一对应的收付，余额自然轧差）
    let ar: std::collections::BTreeMap<String, Money> = snap
        .aux_breakdown("112201")
        .into_iter()
        .map(|r| (r.aux.customer.clone().unwrap_or_default(), r.end()))
        .collect();
    assert_eq!(ar.get("C01"), Some(&m("2700")), "A 宏远应剩 2700");
    assert_eq!(ar.get("C02"), Some(&m("-30000")), "B 精密应预收 30000");
    assert_eq!(ar.get("C03"), Some(&m("39140")), "C 灯具应欠 39140");
    let ap: std::collections::BTreeMap<String, Money> = snap
        .aux_breakdown("220201")
        .into_iter()
        .map(|r| (r.aux.supplier.clone().unwrap_or_default(), r.end()))
        .collect();
    assert_eq!(ap.get("S01"), Some(&m("-23800")), "恒信应剩 23800");
    assert_eq!(ap.get("S02"), Some(&m("-20200")), "大发应剩 20200");

    // 7) 科目微调生效（银行改名、收入改名、制造费用三个明细）
    assert_eq!(
        findb::accounts::get(&db, "100201")
            .expect("读科目")
            .expect("缺100201")
            .name,
        "G银行对公户"
    );
    assert_eq!(
        findb::accounts::get(&db, "600101")
            .expect("读科目")
            .expect("缺600101")
            .name,
        "氧化加工费收入"
    );
    assert!(
        findb::accounts::get(&db, "510108")
            .expect("读科目")
            .is_some(),
        "缺 510108 委外加工费"
    );

    // 8) 核销配对：凭证之间能配的配上（A 发票全额配平、B 票据等额、供应商 FIFO）；
    //    期初三笔无分录、配不上——留在账龄里，余额照样轧平
    let sum_open = |acct: &str| {
        findb::settle::open_entries(&db, acct, p, false)
            .expect("查未清")
            .iter()
            .map(|e| {
                // open() 是绝对值；净额要按原方向还原符号（与配对顺序无关）
                let o = e.open();
                if e.debit > Money::ZERO {
                    o
                } else {
                    Money::ZERO - o
                }
            })
            .fold(Money::ZERO, |a, b| a + b)
    };
    // 未清分录净额恒等于分录净发生（配对只标配对关系，不改金额，顺序无关）
    assert_eq!(sum_open("112201"), m("-56160"));
    assert_eq!(sum_open("220201"), m("-6000"));
    assert!(
        !findb::settle::list(&db, "112201")
            .expect("查配对")
            .is_empty(),
        "应收应有核销配对"
    );
    assert!(
        !findb::settle::list(&db, "220201")
            .expect("查配对")
            .is_empty(),
        "应付应有核销配对"
    );

    // 9) 账龄按单位合计（期初明细 + 凭证未清合并看）
    let buckets = vec![
        fincore::engine::aging::AgingBucket {
            label: "30天内",
            from: 0,
            to: Some(30),
        },
        fincore::engine::aging::AgingBucket {
            label: "31-60天",
            from: 31,
            to: Some(60),
        },
        fincore::engine::aging::AgingBucket {
            label: "60天以上",
            from: 61,
            to: None,
        },
    ];
    let as_of = chrono::NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
    let aging_ar = findb::settle::aging(&db, "112201", p, as_of, &buckets).expect("应收账龄");
    let ar_line = |key: &str| aging_ar.iter().find(|l| l.key == key);
    // C01：期初单据 68000（配不上）躺在账龄里；本月发票全额配平，
    // 两笔收款尾 25300＋40000 躺在预收栏；净额 2700＝余额
    let c01 = ar_line("C01").expect("C01 账龄行");
    assert_eq!(c01.total, m("68000"));
    assert_eq!(c01.credit_total, m("65300"));
    // C02：预收 30000 不进账龄桶，只进预收栏
    let c02 = ar_line("C02").expect("C02 账龄行");
    assert!(c02.total.is_zero());
    assert_eq!(c02.credit_total, m("30000"));
    // C03：本月开票未收，全额在账龄里
    assert_eq!(ar_line("C03").map(|l| l.total), Some(m("39140")));
    // 供应商两家：期初单据＋本月未清＝余额，预收栏为零
    let aging_ap = findb::settle::aging(&db, "220201", p, as_of, &buckets).expect("应付账龄");
    let ap_line = |key: &str| aging_ap.iter().find(|l| l.key == key);
    assert_eq!(ap_line("S01").map(|l| l.total), Some(m("23800")));
    assert_eq!(ap_line("S02").map(|l| l.total), Some(m("20200")));

    let _ = std::fs::remove_file(&path);
}

/// 基础报表：资产负债表左右相等、利润表净利润勾稽、现金流量表自平。
#[test]
fn 演示账三大报表勾稽() {
    let path = build_book_named("oxide_reports");
    let db = findb::Db::open(&path).expect("打开演示账");
    let from = fincore::Period::new(2026, 1).unwrap();
    let to = fincore::Period::new(2026, 11).unwrap();
    let snap = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::range(from, to),
    )
    .expect("取余额快照");

    // 资产负债表：资产合计 == 负债和所有者权益合计（期末列）
    let bs_def = fincore::report::balance_sheet::balance_sheet_def();
    let bs = fincore::report::render(
        &bs_def,
        &snap,
        "恒亮氧化加工厂",
        "2026-11 期末",
        vec![
            Box::new(fincore::report::identity),
            Box::new(fincore::report::to_begin),
        ],
    );
    let end_of = |idx: usize| bs.rows[idx].values[0];
    let assets =
        end_of(fincore::report::balance_sheet::asset_total_row(&bs_def).expect("资产合计行"));
    let liab_eq = end_of(
        fincore::report::balance_sheet::liab_equity_total_row(&bs_def).expect("负债权益合计行"),
    );
    assert_eq!(assets, m("354500"), "资产合计");
    assert_eq!(liab_eq, m("354500"), "负债和所有者权益合计");
    assert_eq!(assets, liab_eq, "资产负债表左右必须相等");

    // 利润表：本期净利润 == 53817.60（快照口径与界面一致：剔除结转损益凭证）
    fn to_ytd(k: fincore::report::AmountKind) -> fincore::report::AmountKind {
        match k {
            fincore::report::AmountKind::PeriodDebit => fincore::report::AmountKind::YearDebit,
            fincore::report::AmountKind::PeriodCredit => fincore::report::AmountKind::YearCredit,
            other => other,
        }
    }
    let snap_is = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::range(from, to).with_exclude_carry(true),
    )
    .expect("取利润表口径快照");
    let is_def = fincore::report::income::income_statement_def();
    let is = fincore::report::render(
        &is_def,
        &snap_is,
        "恒亮氧化加工厂",
        "2026-11",
        vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
    );
    let profit =
        is.rows[fincore::report::income::net_profit_row(&is_def).expect("净利润行")].values[0];
    assert_eq!(profit, m("53817.60"), "本期净利润");

    // 现金流量表：自带勾稽（净增加 == 期末 − 期初）+ 首尾数
    let cf = findb::reports::cash_flow_statement(&db, from, to, None).expect("现金流量表");
    assert!(cf.ties(), "现金流量表必须自平");
    assert_eq!(cf.begin_cash, m("203000"), "期初现金");
    assert_eq!(cf.end_cash, m("311360"), "期末现金");
    assert_eq!(cf.net_increase, m("108360"), "现金净增加");

    println!(
        "资产 {assets:?} = 负债权益 {liab_eq:?}；净利润 {profit:?}；现金 {:?} -> {:?}（净增加 {:?}）",
        cf.begin_cash, cf.end_cash, cf.net_increase
    );
    let _ = std::fs::remove_file(&path);
}
