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

/// 存货档案：(编码, 名称, 规格型号, 计量单位)
///
/// 小工厂第一线没有盘点制度，仓库实数是跟着单据走的。所以这四类物料
/// 必须按数量记，账上的「还剩多少 kg / 多少套」就是唯一的底子。
/// 品类前缀用来分堆：HG 化工、RL 染料、GJ 挂具、BZ 包装。
const ITEMS: &[(&str, &str, &str, &str)] = &[
    // 化工
    ("HG01", "除油剂", "20kg/桶", "kg"),
    ("HG02", "碱液", "25kg/桶", "kg"),
    ("HG03", "硫酸", "30kg/桶", "kg"),
    ("HG04", "硝酸", "30kg/桶", "kg"),
    ("HG05", "封孔剂", "20kg/桶", "kg"),
    ("HG06", "除膜剂", "20kg/桶", "kg"),
    // 染料
    ("RL01", "黑色染料", "25kg/箱", "kg"),
    ("RL02", "彩色染料", "25kg/箱", "kg"),
    ("RL03", "银白染料", "25kg/箱", "kg"),
    ("RL04", "香槟染料", "25kg/箱", "kg"),
    ("RL05", "钛金染料", "25kg/箱", "kg"),
    ("RL06", "古铜染料", "25kg/箱", "kg"),
    // 挂具
    ("GJ01", "铝挂具", "通用型", "套"),
    ("GJ02", "钛挂具", "通用型", "套"),
    ("GJ03", "挂具夹片", "配套件", "个"),
    // 包装
    ("BZ01", "气泡膜", "1.2m×100m/卷", "卷"),
    ("BZ02", "缠绕膜", "50cm×300m/卷", "卷"),
    ("BZ03", "纸箱", "按客户规格", "个"),
    ("BZ04", "报纸（填充用）", "捆", "捆"),
];

/// 客户名单（来源：客户、供应商、外发.xlsx，共 59 家）
///
/// 与供应商重名的三家（宏源加工 C019、勇唐加工厂 C020、东蓬二厂 C016）
/// 名字必须与 SUPPLIERS 里一字不差，系统才认作「同一家的两面」。
const CUSTOMERS: &[&str] = &[
    "奇园江门",
    "火树银花",
    "姚祥峰",
    "信研",
    "弘丰合页",
    "鑫东飞",
    "金雨.亚太阳",
    "澳诗丹",
    "西臣",
    "精耀",
    "格顿希尔",
    "雷老板",
    "金亚浪",
    "九曜",
    "维柯纳",
    "东蓬二厂",
    "英兰",
    "图格乐",
    "宏源加工",
    "勇唐加工厂",
    "译彩",
    "利得丞",
    "金俊力",
    "英川",
    "生兴五金",
    "金麟发",
    "峥喆五金",
    "钰亿霖",
    "轩万诚",
    "永达",
    "中创电机",
    "博亿",
    "金诺",
    "安合太平五金",
    "实匠中山",
    "鼎诺五金衣架",
    "丫头衣架",
    "奥美宜家",
    "尚盛帅",
    "戈诺尼",
    "俐咖",
    "美吕",
    "凯亚德",
    "金颐来",
    "贺老板",
    "创江",
    "创泰",
    "鑫卓越",
    "恋晴",
    "景河",
    "欣佑楚",
    "东豪",
    "铝莱镁",
    "科迈",
    "锐政",
    "铮宝",
    "浩盈",
    "科正",
    "洲艺",
];

/// 供应商名单：(名称, 品类备注)。
///
/// 不建档案的三类（用户 2026-10-25 决定）：
///   饭堂 → 管理费用-福利费；股东工资 → 应付职工薪酬（是工资，不是往来）；
///   报纸 → 管理费用-办公费。它们不属于「采购买卖」，挂进应付账款会和真供应商混在一起对账。
///
/// 重名单位（既是客户又是供应商）两边写成**完全相同**的名字，
/// 系统靠名称一字不差来识别双向身份，差一个字就当成两家：
///   宏源加工（客户 C019 / 供应商 S007）、勇唐加工厂（C020 / S008）、东蓬二厂（C016 / S006）
const SUPPLIERS: &[(&str, &str)] = &[
    ("深圳奥科", "染料"),
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
    ("宏源加工", "外发"),
    ("鸿川加工厂", "外发"),
    ("义商加工厂", "外发"),
    ("勇唐加工厂", "外发"),
    ("泰兴隆五金", "五金"),
    ("润车汇汽修", "汽修"),
    ("驰护（汽修）", "汽修"),
    ("新东泰", "化工"),
    ("金惠机电", "设备维护"),
    ("月宏化工", "化工"),
    ("联胜加工厂", "外发"),
    ("东蓬二厂", "外发"),
    ("传奇罗老板", "外发"),
];

/// 需要新增的明细科目：(编码, 名称, 分类, 借贷方向, 可选辅助核算)
///
/// 都是末级科目。挂 Dept 的留给以后要按车间/部门分摊时再开，这里先不挂，
/// 小厂费用都走明细科目，录凭证时少填一项。
const EXTRA_ACCOUNTS: &[(&str, &str, AcctCategory, Direction, Option<AuxKind>)] = &[
    // 制造/加工成本明细（原本 5101 下只有工资/折旧/水电/办公/其他）
    (
        "510106",
        "厂房租金",
        AcctCategory::Cost,
        Direction::Debit,
        None,
    ),
    (
        "510107",
        "宿舍租金",
        AcctCategory::Cost,
        Direction::Debit,
        None,
    ),
    ("510108", "水费", AcctCategory::Cost, Direction::Debit, None),
    ("510109", "电费", AcctCategory::Cost, Direction::Debit, None),
    (
        "510110",
        "煤气费",
        AcctCategory::Cost,
        Direction::Debit,
        None,
    ),
    (
        "510111",
        "外发加工费",
        AcctCategory::Cost,
        Direction::Debit,
        None,
    ),
    (
        "510112",
        "货车费用",
        AcctCategory::Cost,
        Direction::Debit,
        None,
    ),
    (
        "510113",
        "化工材料",
        AcctCategory::Cost,
        Direction::Debit,
        None,
    ),
    ("510114", "挂具", AcctCategory::Cost, Direction::Debit, None),
    // 管理费用明细（原本 6602 下已有工资/办公/差旅/招待/折旧/社保/租赁/中介/研发/其他）
    (
        "660213",
        "货车加油充电",
        AcctCategory::Expense,
        Direction::Debit,
        None,
    ),
    (
        "660214",
        "车辆路费",
        AcctCategory::Expense,
        Direction::Debit,
        None,
    ),
    (
        "660215",
        "车辆维修年审",
        AcctCategory::Expense,
        Direction::Debit,
        None,
    ),
    (
        "660216",
        "外部加工协作",
        AcctCategory::Expense,
        Direction::Debit,
        None,
    ),
];

fn main() {
    let mut args = env::args().skip(1);
    let path = PathBuf::from(
        args.next()
            .expect("usage: build_shop <输出路径.fbk> [公司名]"),
    );
    let company = args.next().unwrap_or_else(|| "小工厂".to_string());

    let _ = std::fs::remove_file(&path);

    // ---------- 1. 建账套 ----------
    let mut opts = BookOptions {
        code_scheme: vec![4, 2, 2, 2, 2],
        start_period: Period::parse("2026-11").expect("period"),
        base_currency: "CNY".into(),
        company: company.clone(),
        tax_no: String::new(),
        // 数量核算开着：化工、染料、挂具、包装按「数量 × 单价」记，
        // 第一线不盘点，账上的数量进出就是唯一的实数底子，全靠它查账。
        enable_qty: true,
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
        print: Default::default(),
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

    // ---------- 7. 存货档案：化工 / 染料 / 挂具 / 包装 ----------
    //
    // 编码分四段，第 3 位是品类，方便按前缀筛：
    //   HG 化工、RL 染料、GJ 挂具、BZ 包装
    // 单位随品类走（kg / 套 / 个 / 卷），填错了数量金额对不上，validate 会拦。
    let mut ni = 0;
    for (code, name, spec, unit) in ITEMS {
        let mut e = AuxEntity::new(AuxKind::Item, *code, *name);
        e.set_prop(fincore::auxiliary::prop::SPEC, *spec);
        e.set_prop(fincore::auxiliary::prop::UNIT, *unit);
        if findb::auxs::insert(&db, &e).is_ok() {
            ni += 1;
        }
    }
    println!("建存货档案 {ni} 个（化工/染料/挂具/包装，带规格与计量单位）");

    // ---------- 7.5 存货科目：确保「按数量核算 + 挂存货档案 + 单位」三件套齐备 ----------
    //
    // 默认科目表里 1403x/1405x/1411x 本来就挂着 AuxKind::Item 且 has_qty=true，
    // 只是单位一律写成"个"。这里按品类改成真实单位，并给新增的明细科目补上标记。
    // 非末级科目不记账，保持不挂辅助（与 chart.rs 的口径一致）。
    let qty_accounts: &[(&str, &str)] = &[
        ("1401", "批"),   // 材料采购 —— 在途归集，按批次记
        ("140301", "kg"), // 原料及主要材料
        ("140302", "kg"), // 辅助材料 —— 化工料、染料的落脚点
        ("140303", "kg"), // 外购半成品
        ("140304", "个"), // 修理用备件
        ("140305", "卷"), // 包装材料
        ("140306", "kg"), // 燃料
        ("140501", "个"), // 产成品
        ("140502", "个"), // 自制半成品
        ("141101", "卷"), // 包装物
        ("141102", "个"), // 低值易耗品（挂具）
        ("141103", "个"), // 其他周转材料
    ];
    let mut nq = 0;
    for (code, unit) in qty_accounts {
        if let Some(mut a) = findb::accounts::get(&db, code).expect("查科目") {
            let mut changed = false;
            if !a.has_qty {
                a.has_qty = true;
                changed = true;
            }
            if !a.aux.contains(AuxKind::Item) {
                a.aux = a.aux.with(AuxKind::Item);
                changed = true;
            }
            if a.unit.as_deref() != Some(unit) {
                a.unit = Some((*unit).to_string());
                changed = true;
            }
            if changed {
                findb::accounts::update(&db, &a).expect("更新存货科目");
                nq += 1;
            }
        }
    }
    println!("存货科目按数量金额式配置 {nq} 个（挂存货档案 + 真实计量单位）");

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
    println!(
        "完成。合计：科目 {} 个（含新增 {}），客户 {}，供应商 {}（含外发厂），资金账户 {}",
        list.len() + added,
        added,
        nc,
        ns,
        nb
    );
    let _ = Money::ZERO;
}
