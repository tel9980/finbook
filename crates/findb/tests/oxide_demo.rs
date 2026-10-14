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
    let path = std::env::temp_dir().join(format!("oxide_demo_{}.fbk", std::process::id()));
    let _ = std::fs::remove_file(&path);
    oxide::build(&path);
    path
}

#[test]
fn 演示账28张全已记账且试算平衡() {
    let path = build_book();
    let db = findb::Db::open(&path).expect("打开演示账");

    // 1) 28 张，一个不能少、一张不能是草稿
    let list = findb::vouchers::list(&db, &findb::vouchers::VoucherQuery::default())
        .expect("查凭证");
    assert_eq!(list.len(), 28, "凭证数不对");
    assert!(
        list.iter()
            .all(|v| v.status == fincore::VoucherStatus::Posted),
        "必须全部已记账"
    );

    // 2) 试算平衡（期初/本期/期末三组全平）
    let chart = findb::accounts::chart(&db).expect("读科目表");
    let p = fincore::Period::new(2026, 11).unwrap();
    let snap = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::range(p, p),
    )
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
        "112101", "221101", "600101", "6401", "6403", "660201", "660202", "660204",
        "660303", "510101", "510103", "510105", "510106", "510107", "510108",
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

    let _ = std::fs::remove_file(&path);
}
