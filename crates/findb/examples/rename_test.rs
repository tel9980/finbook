//! 改名后：历史凭证/账簿/首页 显示的是新名还是旧名？
use chrono::NaiveDate;
use findb::Db;
use findb::balances::{BalanceQuery, BalanceSnapshot};
use fincore::{AuxEntity, AuxKind, AuxRef, BookOptions, Entry, Money, Period, Voucher};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = "/tmp/rn.fbk";
    for suf in ["", "-wal", "-shm"] { let _ = std::fs::remove_file(format!("{path}{suf}")); }
    let mut opts = BookOptions::default();
    opts.company = "测试厂".into();
    opts.start_period = Period::new(2026, 11)?;
    opts.enable_audit = false; opts.require_audit = false; opts.require_cashier = false;
    let db = Db::create_no_admin(std::path::Path::new(path), &opts)?;
    findb::accounts::import_many(&db, &fincore::chart::default_accounts())?;

    // 建档：宏源化工
    findb::auxs::insert(&db, &AuxEntity::new(AuxKind::Customer, "C001", "宏源化工"))?;
    let p = Period::new(2026, 11)?;
    let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026,11,10).unwrap(), "记", 1);
    v.memo = "加工费".into();
    let mut e1 = Entry::new(1, "112201", "加工费"); e1.debit = Money::parse("15000")?;
    e1.aux = AuxRef{ customer: Some("C001".into()), ..Default::default() };
    let mut e2 = Entry::new(2, "600101", "加工费"); e2.credit = Money::parse("15000")?;
    v.entries.push(e1); v.entries.push(e2);
    let id = findb::vouchers::save(&db, &mut v)?;
    findb::vouchers::post(&db, id, "admin")?;

    println!("=== 改名前后对比 ===");
    // 改名 C001 → 宏源化工有限公司
    let mut ent = findb::auxs::get(&db, AuxKind::Customer, "C001")?.unwrap();
    ent.name = "宏源化工有限公司".into();
    findb::auxs::update(&db, &ent)?;
    println!("改名成功：宏源化工 → 宏源化工有限公司");

    // 账簿里显示什么
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p))?;
    for r in snap.aux_breakdown("112201") {
        println!("账簿行: aux.key={:?}  期初={} 借={} 贷={} 期末={}",
            r.aux.key(), r.begin.fmt_money(), r.debit.fmt_money(), r.credit.fmt_money(), r.end().fmt_money());
        println!("  ↑ 注意：BalanceRow 只有 aux.key()（C001），没有名称");
    }

    // 首页
    let m = findb::advanced::money_position(&db, p, NaiveDate::from_ymd_opt(2026,11,30).unwrap(), None)?;
    println!("\n首页「欠我最多的客户」:");
    for c in &m.top_customers {
        println!("  {} {} {}", c.code, c.name, c.amount.fmt_money());
    }
    Ok(())
}
