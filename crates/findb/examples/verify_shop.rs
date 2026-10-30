//! 账套自检：科目/档案/数量金额式配置/试算平衡。
use fincore::Period;
use findb::Db;

fn main() {
    let p = std::env::args().nth(1).unwrap();
    let db = Db::open(&p).expect("open");
    let o = db.options();
    println!(
        "公司={} 启用期间={} 数量核算={} 生产模块={} 审核={} 出纳签字={} 外币={}",
        o.company,
        o.start_period,
        o.enable_qty,
        o.enable_production,
        o.enable_audit,
        o.require_cashier,
        o.enable_foreign
    );

    let q = findb::balances::BalanceQuery::period(Period::parse("2026-11").unwrap());
    let b = findb::balances::BalanceSnapshot::load(&db, &q).expect("load");
    let chart = findb::accounts::chart(&db).expect("chart");
    let tb = b.trial_balance(&chart);
    println!(
        "试算: 期初借={} 贷={} | 本期借={} 贷={} | 期末借={} 贷={} | 平衡={}",
        tb.begin_debit,
        tb.begin_credit,
        tb.period_debit,
        tb.period_credit,
        tb.end_debit,
        tb.end_credit,
        tb.begin_balanced() && tb.period_balanced()
    );

    let all = findb::accounts::list(&db).unwrap();
    let hq: Vec<_> = all.iter().filter(|a| a.has_qty).collect();
    println!("--- 数量金额式科目 {} 个 ---", hq.len());
    for a in &hq {
        let unit = a.unit.clone().unwrap_or_default();
        let item = if a.aux.contains(fincore::AuxKind::Item) {
            "挂存货"
        } else {
            "未挂存货!"
        };
        println!("  {} {:<10} 单位={:<3} {}", a.code, a.name, unit, item);
    }

    let mut items: Vec<_> =
        findb::auxs::list(&db, &fincore::AuxQuery::kind(fincore::AuxKind::Item))
            .unwrap()
            .into_iter()
            .map(|e| {
                (
                    e.code.clone(),
                    e.name.clone(),
                    e.props
                        .get(fincore::auxiliary::prop::SPEC)
                        .cloned()
                        .unwrap_or_default(),
                    e.props
                        .get(fincore::auxiliary::prop::UNIT)
                        .cloned()
                        .unwrap_or_default(),
                )
            })
            .collect();
    items.sort();
    println!("--- 存货档案 {} 个 ---", items.len());
    for (c, n, s, u) in &items {
        println!("  {c} {n:<10} 规格={s:<16} 单位={u}");
    }

    println!(
        "科目={} 客户={} 供应商={} 银行={} 用户={}",
        all.len(),
        findb::auxs::list(&db, &fincore::AuxQuery::kind(fincore::AuxKind::Customer))
            .unwrap()
            .len(),
        findb::auxs::list(&db, &fincore::AuxQuery::kind(fincore::AuxKind::Supplier))
            .unwrap()
            .len(),
        findb::auxs::list(&db, &fincore::AuxQuery::kind(fincore::AuxKind::Bank))
            .unwrap()
            .len(),
        findb::users::count(&db).unwrap()
    );
}
