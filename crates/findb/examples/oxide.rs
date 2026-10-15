//! 氧化加工厂演示账套：来料氧化加工、收加工费的一个完整月份。
//!
//! 用法: cargo run -p findb --example oxide --release -- <输出路径.fbk>
//!
//! 模拟对象：一家小型氧化加工厂（来料加工，只收加工费），2026-11 全月业务。
//!   - 收入：给 3 家客户做氧化加工，按完工开票收加工费（小规模纳税人 3%）
//!   - 来料是客户的，不进自己库存（只在摘要里写清 batch/数量备查）
//!   - 喷砂/拉丝/抛光委外：进制造费用—委外加工费，不走生产模块
//!   - 化工料（三酸/片碱/色粉/除油剂/亚钠）：140302 辅助材料，数量+单价核算
//!   - 收付款不一一对应：部分收、预收、部分付，余额自然轧差
//!   - G银行=对公有票户，N银行+微信=现金户（微信走 1001，摘要注明）
//!   - 承兑汇票收一笔、到期兑付一笔（112101）
//!   - 月末：制造费用结转成本 → 计提附加税 → 结转损益；**不结账**，留给会计自己点
//!   - 往来三层：期初逐单明细（arap_opening）＋ 11 月凭证 ＋ 自动核销配对；
//!     期初单据无分录、配不上是正常的——账龄合并看，余额轧差为准
//!
//! 产出可直接打开对照：28 张凭证全部已记账，试算平衡，报表有数。

use chrono::NaiveDate;
use fincore::{AuxKind, AuxRef, Entry, Money, Period, Voucher};
use findb::Db;
use std::env;
use std::path::{Path, PathBuf};

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

/// 一条分录：(科目, 摘要, 借方, 贷方, 辅助核算, 数量, 单价)
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

fn voucher(period: Period, date: &str, word: &'static str, no: i32, lines: &[Line]) -> Voucher {
    let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").expect("date");
    let mut v = Voucher::new(period, d, word, no);
    v.prepared_by = "李会计".to_string();
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

/// 生成演示账套（幂等覆盖：输出文件已存在先删除）。全程 expect：哪一步失败
/// 直接 panic，测试里能立刻定位，而不是悄悄少几张凭证。
pub fn build(path: &Path) {
    let _ = std::fs::remove_file(path);
    let opts = fincore::BookOptions {
        code_scheme: vec![4, 2, 2, 2],
        start_period: Period::new(2026, 11).unwrap(),
        base_currency: "CNY".into(),
        company: "恒亮氧化加工厂".into(),
        tax_no: "91330100MAOXIDE66".into(),
        enable_qty: true,
        enable_foreign: false,
        enable_audit: false,
        require_cashier: false,
        biz_accounts: Default::default(),
        budget_control: String::new(),
        doc_prefixes: Default::default(),
        require_audit: false,
        voucher_words: fincore::chart::default_voucher_words(),
    };
    let db = Db::create_no_admin(path, &opts).expect("create");

    // ---------- 管理员账号 ----------
    let mut u = fincore::User::new("admin", "演示管理员", fincore::Role::Admin);
    u.set_password("admin123");
    u.must_change_pwd = false;
    findb::users::insert(&db, &u).expect("创建演示管理员");

    // ---------- 科目表 + 微调 ----------
    let list = fincore::chart::default_accounts();
    findb::accounts::import_many(&db, &list).expect("导入科目");
    findb::reports::reset_cash_flow_items(&db).expect("现金流量项目");
    // 银行改成厂里的叫法；收入改成加工费口径
    for (code, name) in [
        ("100201", "G银行对公户"),
        ("100202", "N银行结算户"),
        ("600101", "氧化加工费收入"),
    ] {
        let mut a = findb::accounts::get(&db, code)
            .expect("读科目")
            .unwrap_or_else(|| panic!("缺科目 {code}"));
        a.name = name.to_string();
        findb::accounts::update(&db, &a).expect("改科目名");
    }
    // 制造费用加三个明细：房租 / 化工材料 / 委外加工费（类别照抄 5101）
    let p5101 = findb::accounts::get(&db, "5101")
        .expect("读5101")
        .expect("缺5101");
    for (code, name) in [
        ("510106", "房租"),
        ("510107", "化工材料"),
        ("510108", "委外加工费"),
    ] {
        let a = fincore::Account::new(code.to_string(), name.to_string(), p5101.category);
        findb::accounts::insert(&db, &a).expect("增明细科目");
    }

    // ---------- 辅助档案 ----------
    let auxs = [
        (AuxKind::Customer, "C01", "宏远汽配厂"),
        (AuxKind::Customer, "C02", "精密电子厂"),
        (AuxKind::Customer, "C03", "美丽灯具厂"),
        (AuxKind::Supplier, "S01", "恒信化工商行"),
        (AuxKind::Supplier, "S02", "大发外发加工厂"),
        (AuxKind::Supplier, "S03", "亮洁抛光厂"),
        (AuxKind::Bank, "B01", "G银行对公户"),
        (AuxKind::Bank, "B02", "N银行结算户"),
        (AuxKind::Item, "HG01", "三酸"),
        (AuxKind::Item, "PJ01", "片碱"),
        (AuxKind::Item, "SF01", "色粉"),
        (AuxKind::Item, "CY01", "除油剂"),
        (AuxKind::Item, "YN01", "亚硝酸钠"),
    ];
    for (kind, code, name) in &auxs {
        findb::auxs::insert(&db, &fincore::AuxEntity::new(*kind, *code, *name))
            .expect("建辅助档案");
    }

    // ---------- 期初（10 月末余额：借 288000 = 贷 288000） ----------
    // 期初用借正贷负记：借方科目记正数，贷方科目记负数。
    let begins: Vec<(&str, Aux, &str, Option<&str>)> = vec![
        ("1001", None, "8000", None),
        ("100201", Some(("bank", "B01")), "153000", None),
        ("100202", Some(("bank", "B02")), "42000", None),
        ("112201", Some(("customer", "C01")), "68000", None),
        ("140302", Some(("item", "HG01")), "12000", Some("4")),
        ("140302", Some(("item", "PJ01")), "5000", Some("2")),
        ("220201", Some(("supplier", "S01")), "-23000", None),
        ("220201", Some(("supplier", "S02")), "-15000", None),
        ("4001", None, "-250000", None),
    ];
    for (code, aux, amt, qty) in begins {
        let mut aux_ref_v = AuxRef::default();
        if let Some((k, c)) = aux {
            aux_ref_v = aux_ref(k, c);
        }
        findb::balances::upsert_begin(
            &db,
            &findb::balances::BeginRow {
                id: 0,
                account_code: code.to_string(),
                aux: aux_ref_v,
                year_begin: m(amt),
                debit_accum: Money::ZERO,
                credit_accum: Money::ZERO,
                qty_begin: qty.map(m),
            },
        )
        .expect("写期初");
    }

    // ---------- 11 月凭证（全部末级科目；借贷先手算平） ----------
    let p = Period::new(2026, 11).unwrap();
    // (日期, 凭证字, 凭证号, 分录)
    let specs: Vec<(&str, &'static str, i32, Vec<Line>)> = vec![
        // 收1：收回 10 月加工费（G银行，有票）
        ("2026-11-03", "收", 1, vec![
            ("100201", "收宏远汽配10月加工费", "68000", "0", Some(("bank", "B01")), None),
            ("112201", "收宏远汽配10月加工费", "0", "68000", Some(("customer", "C01")), None),
        ]),
        // 转1：A客户11月第一批完工开票（含税92700=90000+2700）
        ("2026-11-04", "转", 1, vec![
            ("112201", "宏远汽配氧化加工费11-A批", "92700", "0", Some(("customer", "C01")), None),
            ("600101", "氧化加工费-A批", "0", "90000", None, None),
            ("222102", "销项税额3%（小规模）", "0", "2700", None, None),
        ]),
        // 转2：B客户完工开票（含税57680=56000+1680）
        ("2026-11-05", "转", 2, vec![
            ("112201", "精密电子氧化加工费", "57680", "0", Some(("customer", "C02")), None),
            ("600101", "氧化加工费-B批", "0", "56000", None, None),
            ("222102", "销项税额3%（小规模）", "0", "1680", None, None),
        ]),
        // 转3：购三酸8吨（挂恒信账，有票）
        ("2026-11-06", "转", 3, vec![
            ("140302", "购三酸8吨", "24000", "0", Some(("item", "HG01")), Some(("8", "3000"))),
            ("220201", "欠恒信化工三酸款", "0", "24000", Some(("supplier", "S01")), None),
        ]),
        // 转4：购片碱+色粉（N银行付，散户无票）
        ("2026-11-07", "转", 4, vec![
            ("140302", "购片碱2吨（无票）", "5000", "0", Some(("item", "PJ01")), Some(("2", "2500"))),
            ("140302", "购色粉130公斤（无票）", "6500", "0", Some(("item", "SF01")), Some(("130", "50"))),
            ("100202", "付辅料款", "0", "11500", Some(("bank", "B02")), None),
        ]),
        // 付1：付恒信化工30000（欠53800，不一一对应）
        ("2026-11-08", "付", 1, vec![
            ("220201", "付恒信化工货款", "30000", "0", Some(("supplier", "S01")), None),
            ("100201", "付恒信化工货款", "0", "30000", Some(("bank", "B01")), None),
        ]),
        // 转5：喷砂委外（大发厂，挂账）
        ("2026-11-10", "转", 5, vec![
            ("510108", "喷砂委外加工费", "18000", "0", None, None),
            ("220201", "欠大发外发喷砂费", "0", "18000", Some(("supplier", "S02")), None),
        ]),
        // 转6：收B客户银行承兑汇票
        ("2026-11-11", "转", 6, vec![
            ("112101", "收精密电子承兑汇票", "57680", "0", None, None),
            ("112201", "精密电子票据结算", "0", "57680", Some(("customer", "C02")), None),
        ]),
        // 付2：交厂房房租（G银行，房东无票）
        ("2026-11-12", "付", 2, vec![
            ("510106", "付11月厂房房租（无票）", "25000", "0", None, None),
            ("100201", "付厂房房租", "0", "25000", Some(("bank", "B01")), None),
        ]),
        // 付3：交水电费（G银行）
        ("2026-11-13", "付", 3, vec![
            ("510103", "交11月水电费", "13800", "0", None, None),
            ("100201", "交水电费", "0", "13800", Some(("bank", "B01")), None),
        ]),
        // 转7：车间领用化工料
        ("2026-11-14", "转", 7, vec![
            ("510107", "车间领用化工料", "28000", "0", None, None),
            ("140302", "领用三酸6吨", "0", "18000", Some(("item", "HG01")), Some(("6", "3000"))),
            ("140302", "领用片碱2吨", "0", "5000", Some(("item", "PJ01")), Some(("2", "2500"))),
            ("140302", "领用色粉100公斤", "0", "5000", Some(("item", "SF01")), Some(("100", "50"))),
        ]),
        // 收2：A客户微信付款50000（欠92700，部分收）
        ("2026-11-15", "收", 2, vec![
            ("1001", "微信收宏远汽配加工费", "50000", "0", None, None),
            ("112201", "宏远汽配部分回款", "0", "50000", Some(("customer", "C01")), None),
        ]),
        // 付4：抛光委外（亮洁厂，N银行付现）
        ("2026-11-16", "付", 4, vec![
            ("510108", "抛光委外加工费", "9600", "0", None, None),
            ("100202", "付抛光委外费", "0", "9600", Some(("bank", "B02")), None),
        ]),
        // 付5：购挂具一批（现金）
        ("2026-11-17", "付", 5, vec![
            ("510105", "购氧化挂具一批", "3200", "0", None, None),
            ("1001", "付挂具款", "0", "3200", None, None),
        ]),
        // 转8：C客户完工开票（含税39140=38000+1140）
        ("2026-11-18", "转", 8, vec![
            ("112201", "美丽灯具氧化加工费", "39140", "0", Some(("customer", "C03")), None),
            ("600101", "氧化加工费-C批", "0", "38000", None, None),
            ("222102", "销项税额3%（小规模）", "0", "1140", None, None),
        ]),
        // 收3：B客户N银行转账30000（账上已结清，成预收）
        ("2026-11-19", "收", 3, vec![
            ("100202", "收精密电子预付款", "30000", "0", Some(("bank", "B02")), None),
            ("112201", "精密电子预收下批加工费", "0", "30000", Some(("customer", "C02")), None),
        ]),
        // 付6：微信付大发外发20000（欠40200，部分付）
        ("2026-11-20", "付", 6, vec![
            ("220201", "付大发外发加工费", "20000", "0", Some(("supplier", "S02")), None),
            ("1001", "微信付外发费", "0", "20000", None, None),
        ]),
        // 转9：计提11月工资（车间12000+办公室8000）
        ("2026-11-21", "转", 9, vec![
            ("510101", "计提车间工资", "12000", "0", None, None),
            ("660201", "计提办公室工资", "8000", "0", None, None),
            ("221101", "计提11月工资", "0", "20000", None, None),
        ]),
        // 付7：发工资（N银行，代扣个税500）
        ("2026-11-22", "付", 7, vec![
            ("221101", "发11月工资", "20000", "0", None, None),
            ("100202", "发工资", "0", "19500", Some(("bank", "B02")), None),
            ("222107", "代扣代缴个税", "0", "500", None, None),
        ]),
        // 转10：购除油剂+亚钠（挂恒信账）
        ("2026-11-23", "转", 10, vec![
            ("140302", "购除油剂20桶", "4800", "0", Some(("item", "CY01")), Some(("20", "240"))),
            ("140302", "购亚硝酸钠1吨", "2000", "0", Some(("item", "YN01")), Some(("1", "2000"))),
            ("220201", "欠恒信化工化料款", "0", "6800", Some(("supplier", "S01")), None),
        ]),
        // 转11：拉丝委外（大发厂，挂账）
        ("2026-11-24", "转", 11, vec![
            ("510108", "拉丝委外加工费", "7200", "0", None, None),
            ("220201", "欠大发外发拉丝费", "0", "7200", Some(("supplier", "S02")), None),
        ]),
        // 收4：承兑汇票到期兑付进G银行
        ("2026-11-25", "收", 4, vec![
            ("100201", "承兑汇票到期兑付", "57680", "0", Some(("bank", "B01")), None),
            ("112101", "汇票兑付", "0", "57680", None, None),
        ]),
        // 付8：日常费用（微信：办公2000+招待2600）
        ("2026-11-25", "付", 8, vec![
            ("660202", "购办公用品", "2000", "0", None, None),
            ("660204", "业务招待费", "2600", "0", None, None),
            ("1001", "微信付日常费用", "0", "4600", None, None),
        ]),
        // 收5：A客户G银行付款40000（92700-50000-40000=2700收尾）
        ("2026-11-27", "收", 5, vec![
            ("100201", "收宏远汽配加工费", "40000", "0", Some(("bank", "B01")), None),
            ("112201", "宏远汽配回款", "0", "40000", Some(("customer", "C01")), None),
        ]),
        // 付9：银行手续费
        ("2026-11-29", "付", 9, vec![
            ("660303", "G银行手续费", "120", "0", None, None),
            ("100201", "付手续费", "0", "120", Some(("bank", "B01")), None),
        ]),
        // 转12：制造费用结转成本（34800+25000+13800+28000+3200+12000=116800）
        ("2026-11-30", "转", 12, vec![
            ("6401", "结转11月制造费用", "116800", "0", None, None),
            ("510101", "结转工资", "0", "12000", None, None),
            ("510103", "结转水电费", "0", "13800", None, None),
            ("510105", "结转其他", "0", "3200", None, None),
            ("510106", "结转房租", "0", "25000", None, None),
            ("510107", "结转化工材料", "0", "28000", None, None),
            ("510108", "结转委外加工费", "0", "34800", None, None),
        ]),
        // 转13：结转损益（收入184000；成本费用130182.40；利润53817.60）
        ("2026-11-30", "转", 13, vec![
            ("600101", "结转加工费收入", "184000", "0", None, None),
            ("4103", "结转净支出", "130182.40", "0", None, None),
            ("6401", "结转主营业务成本", "0", "116800", None, None),
            ("660201", "结转管理工资", "0", "8000", None, None),
            ("660202", "结转办公费", "0", "2000", None, None),
            ("660204", "结转招待费", "0", "2600", None, None),
            ("660303", "结转手续费", "0", "120", None, None),
            ("6403", "结转附加税", "0", "662.40", None, None),
            ("4103", "结转净利润", "0", "184000", None, None),
        ]),
    ];
    // 注：附加税 662.40 = 城建 386.40 + 教育 165.60 + 地方 110.40，
    // 在转13之前先计提（转12之后、转13之前加一张）：
    let mut specs = specs;
    specs.insert(
        specs.len() - 1,
        ("2026-11-30", "转", 13, vec![
            ("6403", "计提11月附加税", "662.40", "0", None, None),
            ("222103", "计提城建税7%", "0", "386.40", None, None),
            ("222104", "计提教育费附加3%", "0", "165.60", None, None),
            ("222105", "计提地方教育附加2%", "0", "110.40", None, None),
        ]),
    );
    // 插入后转13顺延为转14
    let last = specs.len() - 1;
    specs[last].2 = 14;

    let mut n = 0;
    for (date, word, no, lines) in &specs {
        let mut v = voucher(p, date, word, *no, lines);
        let id = findb::vouchers::save(&db, &mut v)
            .unwrap_or_else(|e| panic!("{date} 凭证保存失败：{e}"));
        findb::vouchers::post(&db, id, "李会计")
            .unwrap_or_else(|e| panic!("{date} 凭证记账失败：{e}"));
        n += 1;
    }
    println!("导入并记账 {n} 张凭证");

    // ---------- 往来期初明细（启用前的逐单欠款：这就是"基础对应表"的期初部分） ----------
    //
    // 为什么需要它：核销配对只认凭证分录，期初余额没有分录、配不上。
    // 期初欠款的逐单台账记在这里，进账龄分析与未清口径。
    for (kind, party, name, doc_no, doc_date, amount) in [
        ("ar", "C01", "宏远汽配厂", "XF-202610-001", "2026-10-28", "68000"),
        ("ap", "S01", "恒信化工商行", "CL-202610-001", "2026-10-25", "23000"),
        ("ap", "S02", "大发外发加工厂", "WW-202610-001", "2026-10-26", "15000"),
    ] {
        findb::settle::arap_opening_insert(
            &db,
            &findb::settle::ArapOpening {
                id: 0,
                kind: kind.to_string(),
                party_code: party.to_string(),
                party_name: name.to_string(),
                doc_no: doc_no.to_string(),
                doc_date: doc_date.to_string(),
                amount: m(amount),
                memo: "10月遗留".to_string(),
                created_by: "李会计".to_string(),
            },
            "李会计",
        )
        .expect("写往来期初");
    }

    // ---------- 自动核销（FIFO：同单位内凭证之间能配的配上，尾数留给手工） ----------
    //
    // 配得上：A 的 50000+40000 吃掉 92700 中的 90000（剩 2700 尾）；
    // B 的票据 57680 与发票等额配平；供应商两笔付款 FIFO 吃掉最早单据。
    // 配不上：10 月三笔期初单据（无分录）——它们在账龄表里，收到钱后余额自动轧平。
    let ar_settle = findb::settle::auto_settle(&db, "112201", p, Money::ZERO, "李会计")
        .expect("应收自动核销");
    let ap_settle = findb::settle::auto_settle(&db, "220201", p, Money::ZERO, "李会计")
        .expect("应付自动核销");
    println!(
        "自动核销：应收配对 {} 笔、应付配对 {} 笔",
        ar_settle.pairs, ap_settle.pairs
    );

    let n_acc = findb::accounts::list(&db).map(|v| v.len()).unwrap_or(0);
    println!();
    println!("氧化厂演示账套已就绪：{}", path.display());
    println!("  科目 {n_acc} 个 · 凭证 {n} 张 · 11月未结账（留给会计自己点）");
    println!("  登录账号 admin / 口令 admin123");
    drop(db);
}

/// `cargo test` 把本文件当模块引入时会多出一个用不上的 main，允许。
#[allow(dead_code)]
fn main() {
    let path = PathBuf::from(env::args().nth(1).expect("usage: oxide <输出路径.fbk>"));
    build(&path);
}
