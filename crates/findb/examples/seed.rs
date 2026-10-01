//! 把示例科技有限公司常用的科目表 + 几笔凭证导入到账套里
//!
//! 用法: cargo run --example seed --release -- <path>

use findb::Db;
use fincore::chart::default_accounts;
use fincore::{AuxEntity, AuxKind};
use std::env;
use std::path::PathBuf;

fn main() {
    let path = PathBuf::from(env::args().nth(1).expect("usage: seed <path>"));
    let db = Db::open(&path).expect("open");

    // 1) 默认科目表
    let list = default_accounts();
    let n = findb::accounts::import_many(&db, &list).expect("import accounts");
    println!("导入科目 {} 个 (原表 {} 个)", n, list.len());

    // 2) 默认现金流量项目
    findb::reports::reset_cash_flow_items(&db).expect("cash flow items");

    // 3) 辅助档案
    let auxs = [
        (AuxKind::Customer, "C01", "北京星辰科技有限公司"),
        (AuxKind::Customer, "C02", "上海远景贸易公司"),
        (AuxKind::Supplier, "S01", "深圳立讯精密"),
        (AuxKind::Dept, "D01", "销售部"),
        (AuxKind::Dept, "D02", "研发部"),
        (AuxKind::Employee, "E01", "张三"),
        (AuxKind::Employee, "E02", "李四"),
        (AuxKind::Item, "I01", "A 产品"),
        (AuxKind::Item, "I02", "B 服务"),
        (AuxKind::Bank, "B01", "工行基本户"),
        (AuxKind::Bank, "B02", "建行一般户"),
    ];
    let mut n2 = 0;
    for (kind, code, name) in &auxs {
        let e = AuxEntity::new(*kind, *code, *name);
        if findb::auxs::insert(&db, &e).is_ok() { n2 += 1; }
    }
    println!("创建辅助档案 {} 个", n2);

    println!("完成基础数据准备");
    drop(db);
}
