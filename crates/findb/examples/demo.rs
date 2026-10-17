//! 生成一份**带真实业务数据**的示例账套，供新手第一次打开时照着看。
//!
//! 用法: cargo run -p findb --example demo --release -- <输出路径>
//!
//! 与 `seed` 的区别（这两个经常被搞混）：
//!   - `create` 建空账套（含 admin 账号）
//!   - `seed`  只灌基础档案：科目表 + 现金流量项目 + 辅助档案，**一张凭证都没有**
//!   - `demo`  在 seed 之上再灌一整月的**已记账凭证**，打开就能看到报表上有数
//!
//! 新手第一次打开一份空白账套是学不会东西的 —— 他要看的是「记完一个月账之后
//! 账应该长什么样」。所以发布包里放的是这个工具的产物。
//!
//! 演示数据刻意选一家小商贸公司：一买一卖、发了工资、交了税，够看三大报表了，
//! 但不涉及生产制造（个人记账用不上，也让新人少一层理解成本）。

use chrono::NaiveDate;
use fincore::{AuxKind, AuxRef, Entry, Money, Period, Voucher};
use findb::Db;
use std::env;
use std::path::PathBuf;

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

/// 一条分录：(科目, 摘要, 借方, 贷方, 辅助核算, 数量, 单价)
///
/// 辅助核算用 `("bank"|"customer"|"supplier"|"item", 编码)` 表示；`None` 表示不挂。
/// 银行/存货/往来类科目**必须**挂 —— 记账校验会拒（这正是这个演示要展示的严谨性）。
/// 数量核算科目（存货）还必须给数量与单价，校验同样会拒。
type Aux = Option<(&'static str, &'static str)>;
type Line = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Aux,
    Option<(&'static str, &'static str)>,
);

fn aux_ref(kind: &str, code: &str) -> AuxRef {
    let mut a = AuxRef::default();
    match kind {
        "bank" => a.bank = Some(code.to_string()),
        "customer" => a.customer = Some(code.to_string()),
        "supplier" => a.supplier = Some(code.to_string()),
        "item" => a.item = Some(code.to_string()),
        other => panic!("未知辅助核算类型 {other}"),
    }
    a
}

fn voucher(period: Period, date: &str, no: i32, lines: &[Line]) -> Voucher {
    let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").expect("date");
    let mut v = Voucher::new(period, d, "记", no);
    v.prepared_by = "演示会计".to_string();
    v.attachments = 1;
    for (i, (code, summary, dr, cr, aux, qty)) in lines.iter().enumerate() {
        let mut en = Entry::new((i + 1) as i32, *code, *summary);
        en.debit = m(dr);
        en.credit = m(cr);
        if let Some((k, c)) = aux {
            en.aux = aux_ref(k, c);
        }
        if let Some((q, price)) = qty {
            en.qty = Some(m(q));
            en.price = Some(m(price));
        }
        v.push_entry(en);
    }
    v
}

fn main() {
    let path = PathBuf::from(env::args().nth(1).expect("usage: demo <输出路径.fbk>"));

    let db = Db::open(&path).expect("打不开账套（先用 create 建好）");

    // ---------- 管理员账号 ----------
    //
    // 必须自己建：`create` 用的是 `create_no_admin`（Web 端「首个登录者即管理员」
    // 的初始化流程），建出来的账套用户表是**空的**。发布包里的说明写着
    // admin/admin123，就得有这个号，否则用户照着做直接登录失败。
    if findb::users::count(&db).unwrap_or(0) == 0 {
        let mut u = fincore::User::new("admin", "演示管理员", fincore::Role::Admin);
        u.set_password("admin123");
        // 不强制改密：这是公开的演示账号，强制改密只会让人以为登录出了问题。
        u.must_change_pwd = false;
        findb::users::insert(&db, &u).expect("创建演示管理员");
        println!("创建演示账号 admin / admin123");
    }

    // ---------- 基础档案（与 seed 同源） ----------
    let list = fincore::chart::default_accounts();
    findb::accounts::import_many(&db, &list).expect("导入科目");
    println!("导入科目 {} 个", list.len());
    findb::reports::reset_cash_flow_items(&db).expect("现金流量项目");

    let auxs = [
        (AuxKind::Customer, "C01", "杭州明远贸易有限公司"),
        (AuxKind::Customer, "C02", "宁波华信电子有限公司"),
        (AuxKind::Supplier, "S01", "苏州精诚原材料有限公司"),
        (AuxKind::Dept, "D01", "销售部"),
        (AuxKind::Dept, "D02", "行政部"),
        (AuxKind::Employee, "E01", "张明"),
        (AuxKind::Employee, "E02", "李红"),
        (AuxKind::Bank, "B01", "工行基本户 6222****8888"),
        (AuxKind::Bank, "B02", "建行一般户 3310****6666"),
    ];
    let mut n = 0;
    for (kind, code, name) in &auxs {
        if findb::auxs::insert(&db, &fincore::AuxEntity::new(*kind, *code, *name)).is_ok() {
            n += 1;
        }
    }
    println!("创建辅助档案 {} 个", n);

    // ---------- 账套参数 ----------
    // 启用期间设为演示当期，免得期初/结账口径对不上。
    let mut opts = db.options();
    opts.company = "示例商贸有限公司".to_string();
    opts.tax_no = "91330100MA2DEMO88".to_string();
    opts.start_period = Period::new(2026, 1).unwrap();
    db.set_options(&opts).expect("写账套参数");

    // ---------- 一整月的已记账凭证 ----------
    //
    // 选这几笔的理由：一买（付供应商货款）、一卖（收客户货款）、一发工资、一交税，
    // 刚好能凑出资产负债表、利润表、现金流量表三张表上的数，而且每笔新手都看得懂
    // 业务含义 —— 不需要懂会计也能明白「买了 5 万的货，付了钱」。
    let p = Period::new(2026, 1).unwrap();
    //
    // 全部用**末级科目**（110202 / 140501 / 222102 …）而不是 1002 / 1122 这种一级码 ——
    // 记账校验会拒非末级科目（Voucher::balanced 之前的 validate 就会报）。
    // 每张凭证借贷必须严格相等，金额自己先算过一遍。
    let specs: Vec<(&str, Vec<Line>)> = vec![
        // 1. 赊购原材料
        (
            "2026-01-05",
            vec![
                (
                    "140501",
                    "采购产成品 100 件",
                    "50000",
                    "0",
                    Some(("item", "I01")),
                    Some(("100", "500")),
                ),
                (
                    "220201",
                    "应付货款-苏州精诚",
                    "0",
                    "50000",
                    Some(("supplier", "S01")),
                    None,
                ),
            ],
        ),
        // 2. 付供应商货款
        (
            "2026-01-08",
            vec![
                (
                    "220201",
                    "支付货款-苏州精诚",
                    "50000",
                    "0",
                    Some(("supplier", "S01")),
                    None,
                ),
                (
                    "100201",
                    "付苏州精诚货款",
                    "0",
                    "50000",
                    Some(("bank", "B01")),
                    None,
                ),
            ],
        ),
        // 3. 赊销开票：含税 120000 = 不含税收入 100000 + 销项税 20000
        (
            "2026-01-12",
            vec![
                (
                    "112201",
                    "应收货款-杭州明远",
                    "120000",
                    "0",
                    Some(("customer", "C01")),
                    None,
                ),
                ("600101", "销售产品-B产品", "0", "100000", None, None),
                ("22210102", "销项税额 20%", "0", "20000", None, None),
            ],
        ),
        // 4. 收到客户货款
        (
            "2026-01-15",
            vec![
                (
                    "100201",
                    "收杭州明远货款",
                    "120000",
                    "0",
                    Some(("bank", "B01")),
                    None,
                ),
                (
                    "112201",
                    "应收货款-杭州明远",
                    "0",
                    "120000",
                    Some(("customer", "C01")),
                    None,
                ),
            ],
        ),
        // 5. 发放工资
        (
            "2026-01-18",
            vec![
                ("660201", "管理费用-工资 1 月", "30000", "0", None, None),
                (
                    "100201",
                    "发工资 1 月",
                    "0",
                    "30000",
                    Some(("bank", "B01")),
                    None,
                ),
            ],
        ),
        // 6. 支付办公室房租
        (
            "2026-01-20",
            vec![
                ("660209", "管理费用-租赁费", "8000", "0", None, None),
                (
                    "100202",
                    "付 1 月房租",
                    "0",
                    "8000",
                    Some(("bank", "B02")),
                    None,
                ),
            ],
        ),
        // 7. 结转已销产品成本
        (
            "2026-01-25",
            vec![
                ("6401", "结转 B 产品销售成本", "45000", "0", None, None),
                (
                    "140501",
                    "结转 B 产品成本",
                    "0",
                    "45000",
                    Some(("item", "I02")),
                    Some(("100", "450")),
                ),
            ],
        ),
        // 8. 缴纳上期增值税
        (
            "2026-01-28",
            vec![
                ("222102", "缴纳增值税", "1800", "0", None, None),
                (
                    "100201",
                    "缴增值税",
                    "0",
                    "1800",
                    Some(("bank", "B01")),
                    None,
                ),
            ],
        ),
        // 9. 销售部门运费
        (
            "2026-01-30",
            vec![
                ("660106", "销售费用-运输费", "3200", "0", None, None),
                ("100201", "付运费", "0", "3200", Some(("bank", "B01")), None),
            ],
        ),
    ];

    let mut ids = Vec::new();
    for (i, (date, lines)) in specs.iter().enumerate() {
        let mut v = voucher(p, date, (i + 1) as i32, lines);
        match findb::vouchers::save(&db, &mut v) {
            Ok(id) => {
                // 直接记成「已记账」，这样打开就能查到余额和报表；
                // 新手看到空白报表会以为软件坏了。
                let _ = findb::vouchers::post(&db, id, "演示会计");
                ids.push(id);
            }
            Err(e) => eprintln!("第 {} 凭证导入失败：{e}", i + 1),
        }
    }
    println!("导入并记账 {} 张凭证", ids.len());

    // 已记账分录即入余额（见 README §5「余额口径」），不需要先结账，
    // 余额表 / 三大报表现在就能查出数。
    let n_acc = findb::accounts::list(&db).map(|v| v.len()).unwrap_or(0);
    println!();
    println!("示例账套已就绪：{}", path.display());
    println!(
        "  科目 {n_acc} 个 · 凭证 {} 张 · 辅助档案 {n} 个",
        ids.len()
    );
    println!("  登录账号 admin / 口令 admin123");
    println!("  建议先看：工作台 → 会计报表（资产负债表 / 利润表 / 现金流量表）");
    drop(db);
}
