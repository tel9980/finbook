//! 为「小工厂」建一份 2026-11 启用的记账账套。
//!
//! 用法: cargo run -p findb --example build_shop --release -- <输出路径.fbk> <公司名>
//!
//! 做什么：
//!   1. 建空账套（启用期间 2026-11，编码 4-2-2-2-2）
//!   2. 导入默认科目表，按小工厂实际改 2 个 + 增 13 个明细科目
//!   3. 建资金账户：B01 G银行对公户（要票）、B02 N银行结算户（当现金用）
//!   4. 建客户 C001..C059、供应商 S001..S028、外发厂 W001..W006、存货 TG01 挂具
//!   5. 建管理员 admin/admin123
//!
//! 不灌任何凭证与期初：期初等工厂自己给出数再录，避免我凭空编数字。

use fincore::{
    Account, AcctCategory, AuxEntity, AuxKind, AuxMask, BookOptions, Direction, Money, Period, Role,
};
use findb::Db;
use std::env;
use std::path::PathBuf;

/// 客户名单（来源：客户、供应商、外发.xlsx，共 59 家）
const CUSTOMERS: &[&str] = &[
    "奇园江门", "火树银花", "姚祥峰", "信研", "弘丰合页", "鑫东飞", "金雨.亚太阳", "澳诗丹",
    "西臣", "精耀", "格顿希尔", "雷老板", "金亚浪", "九曜", "维柯纳", "东蓬二厂", "英兰",
    "图格乐", "宏源", "勇唐", "译彩", "利得丞", "金俊力", "英川", "生兴五金", "金麟发",
    "峥喆五金", "钰亿霖", "轩万诚", "永达", "中创电机", "博亿", "金诺", "安合太平五金",
    "实匠中山", "鼎诺五金衣架", "丫头衣架", "奥美宜家", "尚盛帅", "戈诺尼", "俐咖", "美吕",
    "凯亚德", "金颐来", "贺老板", "创江", "创泰", "鑫卓越", "恋晴", "景河", "欣佑楚", "东豪",
    "铝莱镁", "科迈", "锐政", "铮宝", "浩盈", "科正", "洲艺",
];

/// 供应商名单：(名称, 品类备注)。「股东工资」不是供应商，是分配项，单独跳过。
/// 「饭堂」「报纸」是内部膳食与包装耗材，仍作供应商建，方便挂账。
const SUPPLIERS: &[(&str, &str)] = &[
    ("深圳奥科/永兴阳", "染料"),
    ("宏乾化工", "化工"),
    ("吴永春化工", "化工"),
    ("正品化工", "化工"),
    ("科赛欧染料", "染料"),
    ("壹展染料", "染料"),
    ("超亚染料", "染料"),
    ("美桥科技", "染料"),
    ("科俊化工", "染料"),
    ("新有新挂具", "挂具"),
    ("连海挂具", "挂具"),
    ("优泽嘉膜", "包装"),
    ("泰兴隆五金", "五金"),
    ("饭堂", "膳食"),
    ("润车汇汽修", "汽修"),
    ("驰护（汽修）", "汽修"),
    ("新东泰", "化工"),
    ("金惠机电", "设备维护"),
    ("月宏化工", "化工"),
    ("报纸", "包装"),
    ("东蓬二厂", "外发"),
];

/// 外发加工厂（同时是供应商，单独给 W 编码便于按品类筛）
const OUTSOURCE: &[&str] = &[
    "鸿川加工厂",
    "宏源加工",
    "义商加工厂",
    "勇唐加工厂",
    "联胜加工厂",
    "传奇罗老板",
];

/// 需要新增的明细科目：(编码, 名称, 分类, 借贷方向, 可选辅助核算)
///
/// 都是末级科目。挂 Dept 的留给以后要按车间/部门分摊时再开，这里先不挂，
/// 小厂费用都走明细科目，录凭证时少填一项。
const EXTRA_ACCOUNTS: &[(&str, &str, AcctCategory, Direction, Option<AuxKind>)] = &[
    // 制造/加工成本明细（原本 5101 下只有工资/折旧/水电/办公/其他）
    ("510106", "厂房租金", AcctCategory::Cost, Direction::Debit, None),
    ("510107", "宿舍租金", AcctCategory::Cost, Direction::Debit, None),
    ("510108", "水费", AcctCategory::Cost, Direction::Debit, None),
    ("510109", "电费", AcctCategory::Cost, Direction::Debit, None),
    ("510110", "煤气费", AcctCategory::Cost, Direction::Debit, None),
    ("510111", "外发加工费", AcctCategory::Cost, Direction::Debit, None),
    ("510112", "货车费用", AcctCategory::Cost, Direction::Debit, None),
    ("510113", "化工材料", AcctCategory::Cost, Direction::Debit, None),
    ("510114", "挂具", AcctCategory::Cost, Direction::Debit, None),
    // 管理费用明细（原本 6602 下已有工资/办公/差旅/招待/折旧/社保/租赁/中介/研发/其他）
    ("660213", "货车加油充电", AcctCategory::Expense, Direction::Debit, None),
    ("660214", "车辆路费", AcctCategory::Expense, Direction::Debit, None),
    ("660215", "车辆维修年审", AcctCategory::Expense, Direction::Debit, None),
    ("660216", "外部加工协作", AcctCategory::Expense, Direction::Debit, None),
];

fn main() {
    let mut args = env::args().skip(1);
    let path = PathBuf::from(args.next().expect("usage: build_shop <输出路径.fbk> [公司名]"));
    let company = args.next().unwrap_or_else(|| "小工厂".to_string());

    let _ = std::fs::remove_file(&path);

    // ---------- 1. 建账套 ----------
    let mut opts = BookOptions {
        code_scheme: vec![4, 2, 2, 2, 2],
        start_period: Period::parse("2026-11").expect("period"),
        base_currency: "CNY".into(),
        company: company.clone(),
        tax_no: String::new(),
        // 数量核算关掉：小厂现阶段只做财务记账，买料按金额记，不录吨/个
        // （以后要管库存了，在「账套参数」里勾上即可，不影响已有凭证）
        enable_qty: false,
        enable_foreign: false,
        // 小厂不上 MRP 排产，生产模块关掉，侧栏少一大组
        enable_production: false,
        // 一人记账，不做三权分离：审核、出纳签字都关
        enable_audit: false,
        require_cashier: false,
        require_audit: false,
        biz_accounts: Default::default(),
        budget_control: String::new(),
        doc_prefixes: Default::default(),
        voucher_words: fincore::chart::default_voucher_words(),
    };
    // 「其他应收款/其他应付款」按往来单位挂辅助，避免挂到员工名下
    opts.biz_accounts.ar = "112201".into();
    opts.biz_accounts.ap = "220201".into();
    opts.biz_accounts.income = "600101".into();
    opts.biz_accounts.tax_sales = "22210102".into();
    opts.biz_accounts.material = "140302".into();
    // 默认资金账户指向 G 银行对公户：有票业务走这里
    opts.biz_accounts.fund = "100201".into();

    let db = Db::create_no_admin(&path, &opts).expect("建账套");
    println!("建账套: {} （启用期间 2026-11）", path.display());

    // ---------- 2. 科目表 ----------
    let list = fincore::chart::default_accounts();
    let n = findb::accounts::import_many(&db, &list).expect("导入默认科目");
    println!("导入默认科目 {n} 个");

    // 改两个银行户名 + 收入科目名
    let rename = [
        ("100201", "G银行对公户"),
        ("100202", "N银行结算户"),
        ("100203", "微信钱包"),
        ("600101", "加工费收入"),
        ("1001", "库存现金（含微信）"),
    ];
    for (code, name) in rename {
        if let Some(mut a) = findb::accounts::get(&db, code).expect("get account") {
            a.name = name.to_string();
            findb::accounts::update(&db, &a).expect("改名");
        }
    }
    println!("科目改名 {} 个", rename.len());

    // 新增小工厂专用明细科目
    let mut added = 0;
    for (code, name, cat, dir, aux) in EXTRA_ACCOUNTS {
        let mut a = Account::new(*code, *name, *cat);
        a.dir = *dir;
        if let Some(k) = aux {
            a.aux = AuxMask::NONE.with(*k);
        }
        if findb::accounts::insert(&db, &a).is_ok() {
            added += 1;
        }
    }
    println!("新增明细科目 {added} 个");
    findb::reports::reset_cash_flow_items(&db).expect("现金流量项目");

    // ---------- 3. 资金账户 ----------
    //
    // 现实中两个银行：G 行大部分要票、N 行可当现金用不要票。
    // 微信单独挂 B03，与库存现金分开，对账时一眼看出微信收了多少。
    let banks = [
        (AuxKind::Bank, "B01", "G银行对公户（要票业务）"),
        (AuxKind::Bank, "B02", "N银行结算户（当现金用、不要票）"),
        (AuxKind::Bank, "B03", "微信钱包"),
    ];
    let mut nb = 0;
    for (kind, code, name) in banks {
        if findb::auxs::insert(&db, &AuxEntity::new(kind, code, name)).is_ok() {
            nb += 1;
        }
    }
    println!("建资金账户 {nb} 个");

    // ---------- 4. 客户 ----------
    let mut nc = 0;
    for (i, name) in CUSTOMERS.iter().enumerate() {
        let code = format!("C{:03}", i + 1);
        if findb::auxs::insert(&db, &AuxEntity::new(AuxKind::Customer, &code, *name)).is_ok() {
            nc += 1;
        }
    }
    println!("建客户 {nc} 家");

    // ---------- 5. 供应商 ----------
    let mut ns = 0;
    for (i, (name, cat)) in SUPPLIERS.iter().enumerate() {
        let code = format!("S{:03}", i + 1);
        let mut e = AuxEntity::new(AuxKind::Supplier, &code, *name);
        e.set_prop("品类", *cat);
        if findb::auxs::insert(&db, &e).is_ok() {
            ns += 1;
        }
    }
    println!("建供应商 {ns} 家");

    // ---------- 6. 外发加工厂 ----------
    //
    // 外发厂同时也是供应商（要付加工费），所以两端都建：
    // W 编码给「按外发工序统计」用，S 编码给应付挂账用。
    let mut nw = 0;
    for (i, name) in OUTSOURCE.iter().enumerate() {
        let code = format!("W{:03}", i + 1);
        let mut e = AuxEntity::new(AuxKind::Supplier, &code, *name);
        e.set_prop("品类", "外发");
        if findb::auxs::insert(&db, &e).is_ok() {
            nw += 1;
        }
    }
    println!("建外发加工厂 {nw} 家（挂在供应商维度，W 编码）");

    // ---------- 7. 存货：挂具 ----------
    let mut ni = 0;
    for (code, name) in [("TG01", "挂具")] {
        if findb::auxs::insert(&db, &AuxEntity::new(AuxKind::Item, code, name)).is_ok() {
            ni += 1;
        }
    }
    println!("建存货档案 {ni} 个");

    // ---------- 7.5 关掉数量核算后，把科目上的「按数量核算」标记一起清掉 ----------
    //
    // 数量核算关着的时候这些标记不生效，但留着是一颗地雷：以后谁只勾上
    // 「启用数量核算」忘了检查，12 个存货科目会同时开始要数量，
    // 建账初期录凭证会被拦。既然现阶段按金额记账，这里一并清干净，
    // 以后真要管库存，逐个科目去勾。挂具档案保留（不碍事，来日可用）。
    let mut nq = 0;
    for mut a in findb::accounts::list(&db).expect("科目列表") {
        if a.has_qty {
            a.has_qty = false;
            findb::accounts::update(&db, &a).expect("清数量标记");
            nq += 1;
        }
    }
    if nq > 0 {
        println!("清除「按数量核算」标记 {nq} 个科目（现阶段只按金额记账）");
    }

    // ---------- 8. 管理员 ----------
    if findb::users::count(&db).unwrap_or(0) == 0 {
        let mut u = fincore::User::new("admin", "管理员", Role::Admin);
        u.set_password("admin123");
        u.must_change_pwd = true;
        findb::users::insert(&db, &u).expect("创建管理员");
        println!("创建管理员 admin / admin123（首次登录须改密）");
    }

    let _ = db.log("-", "系统", "建账", "小工厂账套初始化");
    drop(db);

    println!();
    println!("完成。合计：科目 {} 个（含新增 {}），客户 {}，供应商 {}，外发厂 {}，资金账户 {}",
        list.len() + added, added, nc, ns, nw, nb);
    let _ = Money::ZERO;
}
