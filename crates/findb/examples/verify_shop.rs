use fincore::Period;
use findb::Db;
fn main(){
    let p = std::env::args().nth(1).unwrap();
    let db = Db::open(&p).expect("open");
    let o = db.options();
    println!("公司={} 启用期间={} 数量核算={} 生产模块={} 审核={} 出纳签字={} 外币={}",
        o.company, o.start_period, o.enable_qty, o.enable_production,
        o.enable_audit, o.require_cashier, o.enable_foreign);
    let q = findb::balances::BalanceQuery::period(Period::parse("2026-11").unwrap());
    let b = findb::balances::BalanceSnapshot::load(&db, &q).expect("load");
    let chart = findb::accounts::chart(&db).expect("chart");
    let tb = b.trial_balance(&chart);
    println!("试算: 期初借={} 贷={} | 本期借={} 贷={} | 期末借={} 贷={} | 平衡={}",
        tb.begin_debit, tb.begin_credit, tb.period_debit, tb.period_credit,
        tb.end_debit, tb.end_credit, tb.begin_balanced() && tb.period_balanced());
    let mut hq = vec![];
    for a in findb::accounts::list(&db).unwrap() { if a.has_qty { hq.push((a.code, a.name)); } }
    hq.sort();
    println!("数量核算科目 {} 个: {:?}", hq.len(), hq.iter().map(|(c,_)|c.as_str()).collect::<Vec<_>>());
    println!("科目={} 客户={} 供应商={} 银行={} 用户={}",
        findb::accounts::list(&db).unwrap().len(),
        findb::auxs::list(&db,&fincore::AuxQuery::kind(fincore::AuxKind::Customer)).unwrap().len(),
        findb::auxs::list(&db,&fincore::AuxQuery::kind(fincore::AuxKind::Supplier)).unwrap().len(),
        findb::auxs::list(&db,&fincore::AuxQuery::kind(fincore::AuxKind::Bank)).unwrap().len(),
        findb::users::count(&db).unwrap());
}
