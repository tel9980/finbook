use fincore::Period;
use findb::Db;
fn main(){
    let p = std::env::args().nth(1).unwrap();
    let db = Db::open(&p).expect("open");
    let o = db.options();
    println!("公司={} 启用期间={} 编码方案={:?}", o.company, o.start_period, o.code_scheme);
    let chart = findb::accounts::chart(&db).expect("chart");
    println!("科目数={}", findb::accounts::list(&db).unwrap().len());
    let q = findb::balances::BalanceQuery::period(Period::parse("2026-11").unwrap());
    let b = findb::balances::BalanceSnapshot::load(&db, &q).expect("load balances");
    let tb = b.trial_balance(&chart);
    println!("试算: 期初借={} 贷={} | 本期借={} 贷={} | 期末借={} 贷={} | 平衡={}",
        tb.begin_debit, tb.begin_credit, tb.period_debit, tb.period_credit,
        tb.end_debit, tb.end_credit,
        tb.begin_balanced() && tb.period_balanced());
    let ov = findb::reports::overview(&db, Period::parse("2026-11").unwrap()).expect("overview");
    println!("概览: {:?}", ov);
    println!("客户={} 供应商={} 银行={}",
        findb::auxs::list(&db,&fincore::AuxQuery::kind(fincore::AuxKind::Customer)).unwrap().len(),
        findb::auxs::list(&db,&fincore::AuxQuery::kind(fincore::AuxKind::Supplier)).unwrap().len(),
        findb::auxs::list(&db,&fincore::AuxQuery::kind(fincore::AuxKind::Bank)).unwrap().len());
    println!("用户数={}", findb::users::count(&db).unwrap());
}
