//! 氧化加工厂「一般纳税人」演示账套：来料氧化加工、收加工费的一个完整月份。
//!
//! 用法: cargo run -p findb --example oxide_vat --release -- <输出路径.fbk>
//!
//! 与同目录的 [`oxide`]（小规模纳税人版）的区别：这一套按**一般纳税人**编制，
//! 对齐一家真实小厂的账：年收入 450 万上下、员工 70 人、8 种化工料、电费充值
//! 式扣费、客户与供应商付款都不一一对应。模拟对象 2026-11 全月业务：
//!
//!   - 收入：3 家客户做氧化加工，按完工开票收加工费，开 13% 专票（销项 58,500）
//!   - 进项按票面税率分三档：化工料 13%、电费 13%、水费 9%、小规模供应商 3% 征收率
//!     → 一共只抵到 9,740.34；人工占了成本大头，所以**税负 10.84%** 才是这类厂的真相
//!   - 月末三步：制造费用结转成本 → 转出未交增值税并计提附加税 → 结转损益
//!   - 来料是客户的，不进自己库存（只在摘要里写清 batch / 数量备查）
//!   - 化工料（硫酸/磷酸/硝酸/片碱/亚硝酸钠/染料/除油剂/封闭剂）走 140302 辅助材料，
//!     数量 + 单价核算：月初有存料、月中分批买、月末一次性投产，期末库存刚好为 0
//!   - 电费是**充值式**：先充 20,000（走 112301 预付电费），月末抄表 18,000 再结转
//!   - 喷砂 / 拉丝委外在 13%~3% 之间，抛光没票现付：三种票情形各一笔
//!   - 收付款**不一一对应**：部分收、微信收、承兑汇票收、A 客户代付 B 客户货款、
//!     客户 ↔ 供应商三方抵账、收款抹零 50、付款抹零 20（差异走 660305）
//!   - G银行=对公有票户，N银行+微信=现金户（微信走 1001，摘要注明）
//!   - 70 人工资：计提 → 单位社保 → 代扣个人社保与个税 → 实发 → 缴社保，五步走完
//!   - 月末：**不结账**，留给会计自己点（和界面上的流程一致）
//!
//! 产出可直接打开对照：45 张凭证 + 3 张系统自动生成（代收代付过渡 / 抹零差异 /
//! 往来对冲）全部已记账，试算平衡，报表有数。

use chrono::NaiveDate;
use fincore::engine::period_end::{generate_carry_forward, PROFIT_ACCOUNT};
use fincore::{AuxKind, AuxRef, Entry, Money, Period, Voucher};
use findb::Db;
use std::env;
use std::path::{Path, PathBuf};

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

/// 含税金额按税率拆分：(不含税, 税额)。
///
/// `rate_pct` 传整数百分点（13 / 9 / 3），全程走「分」的整数运算再四舍五入，
/// 保证 `不含税 + 税额 == 含税` 一分不差——这是对账时最容易被挑刺的地方。
fn split(incl: Money, rate_pct: i64) -> (Money, Money) {
    let cents = incl.cents();
    let denom = 100 + rate_pct;
    let net = (cents * 100 + denom / 2) / denom;
    (Money::from_cents(net), Money::from_cents(cents - net))
}

/// 按百分点取税（附加税用）：金额 × p%。
fn pct(amt: Money, p: i64) -> Money {
    Money::from_cents((amt.cents() * p + 50) / 100)
}

/// 参考单价 = 金额 / 数量。数量传「十分位整数」（10 吨传 100、2.5 吨传 25），
/// 这样分数数量也不用浮点数。数量金额核算只按金额入账，单价四舍五入的尾差
/// 正是现实里「数量 × 单价 ≠ 金额」的来源，系统只提醒不拦截。
fn unit_price(amt: Money, qty_tenths: i64) -> Money {
    amt.checked_mul(10_i64)
        .expect("乘溢出")
        .checked_div(qty_tenths)
        .expect("除数为 0")
        .round2()
}

/// 一条分录
struct Line {
    code: &'static str,
    summary: String,
    dr: Money,
    cr: Money,
    aux: Option<(&'static str, &'static str)>,
    qty: Option<(Money, Money)>,
}

fn dr(
    code: &'static str,
    summary: &str,
    amt: Money,
    aux: Option<(&'static str, &'static str)>,
) -> Line {
    Line {
        code,
        summary: summary.to_string(),
        dr: amt,
        cr: Money::ZERO,
        aux,
        qty: None,
    }
}

fn cr(
    code: &'static str,
    summary: &str,
    amt: Money,
    aux: Option<(&'static str, &'static str)>,
) -> Line {
    Line {
        code,
        summary: summary.to_string(),
        dr: Money::ZERO,
        cr: amt,
        aux,
        qty: None,
    }
}

/// 带数量的借方分录（采购入库用）
fn dr_qty(
    code: &'static str,
    summary: &str,
    amt: Money,
    aux: Option<(&'static str, &'static str)>,
    qty: &str,
    price: Money,
) -> Line {
    Line {
        code,
        summary: summary.to_string(),
        dr: amt,
        cr: Money::ZERO,
        aux,
        qty: Some((m(qty), price)),
    }
}

/// 带数量的贷方分录（车间领用出库用）
fn cr_qty(
    code: &'static str,
    summary: &str,
    amt: Money,
    aux: Option<(&'static str, &'static str)>,
    qty: &str,
    price: Money,
) -> Line {
    Line {
        code,
        summary: summary.to_string(),
        dr: Money::ZERO,
        cr: amt,
        aux,
        qty: Some((m(qty), price)),
    }
}

fn cu(code: &'static str) -> Option<(&'static str, &'static str)> {
    Some(("customer", code))
}
fn su(code: &'static str) -> Option<(&'static str, &'static str)> {
    Some(("supplier", code))
}
fn bk(code: &'static str) -> Option<(&'static str, &'static str)> {
    Some(("bank", code))
}
fn it(code: &'static str) -> Option<(&'static str, &'static str)> {
    Some(("item", code))
}

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

/// 一张凭证：(日期, 凭证字, 凭证号, 分录)
struct Spec {
    date: &'static str,
    word: &'static str,
    no: i32,
    lines: Vec<Line>,
}

fn spec(date: &'static str, word: &'static str, no: i32, lines: Vec<Line>) -> Spec {
    Spec {
        date,
        word,
        no,
        lines,
    }
}

/// 造一张发票（税务模块用）：`kind` = "in"/"out"，`incl` 含税金额，
/// `rate_pct` 传 13/9/3。进项票一律登记为「已认证」，否则申报表取不到数。
fn invoice(
    kind: &str,
    number: &str,
    date: &str,
    party: &str,
    incl: Money,
    rate_pct: i64,
    memo: &str,
) -> findb::invoices::Invoice {
    let (net, tax) = split(incl, rate_pct);
    let (buyer, seller) = if kind == "out" {
        (party.to_string(), "恒亮氧化加工厂".to_string())
    } else {
        ("恒亮氧化加工厂".to_string(), party.to_string())
    };
    findb::invoices::Invoice {
        id: 0,
        kind: kind.into(),
        code: "4400190000".into(),
        number: number.into(),
        date: date.into(),
        buyer,
        seller,
        amount_tax: incl,
        amount: net,
        tax,
        tax_rate: match rate_pct {
            13 => "0.13".into(),
            9 => "0.09".into(),
            _ => "0.03".into(),
        },
        status: "verified".into(),
        memo: memo.into(),
        attach_id: 0,
        created_by: "李会计".into(),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

/// 把发票存进税务模块
fn put_invoice(db: &Db, inv: &findb::invoices::Invoice) {
    findb::invoices::insert(db, inv, "李会计").expect("录发票");
}

/// 期初一行：(科目, 辅助核算, 金额（借正贷负）, 数量)
type BegRow = (
    &'static str,
    Option<(&'static str, &'static str)>,
    &'static str,
    Option<&'static str>,
);

fn build_voucher(p: Period, s: &Spec, source: fincore::VoucherSource) -> Voucher {
    let d = NaiveDate::parse_from_str(s.date, "%Y-%m-%d").expect("date");
    let mut v = Voucher::new(p, d, s.word, s.no);
    v.prepared_by = "李会计".to_string();
    v.source = source;
    for (i, l) in s.lines.iter().enumerate() {
        let mut en = Entry::new((i + 1) as i32, l.code, l.summary.as_str());
        en.debit = l.dr;
        en.credit = l.cr;
        if let Some((k, c)) = l.aux {
            en.aux = aux_ref(k, c);
        }
        if let Some((q, price)) = l.qty {
            en.qty = Some(q);
            en.price = Some(price);
        }
        v.push_entry(en);
    }
    v
}

/// 保存并记账一张业务凭证。失败直接把凭证日期与号数带出来——
/// 生成器里 40 多张凭证，报错只说「保存失败」等于没说。
fn post_spec(db: &findb::Db, p: Period, s: &Spec) {
    let mut v = build_voucher(p, s, fincore::VoucherSource::Manual);
    let id = findb::vouchers::save(db, &mut v)
        .unwrap_or_else(|e| panic!("{} {} 号凭证保存失败：{e}", s.date, s.no));
    findb::vouchers::post(db, id, "李会计")
        .unwrap_or_else(|e| panic!("{} {} 号凭证记账失败：{e}", s.date, s.no));
}

/// 在未清往来分录里精确定位一条：往来单位 + 摘要关键字 + 借方/贷方原额。
///
/// 系统自动生成的三张凭证（代收代付过渡 / 抹零差异 / 往来对冲）要拿分录 id 才能做，
/// 而分录 id 是记账后才有的，所以只能回查。找不到就 panic——宁可生成失败，
/// 也不能悄悄漏一笔核销。
fn entry_id(
    db: &Db,
    acct: &str,
    p: Period,
    aux_tag: &str,
    summary_key: &str,
    amount: Money,
    debit_side: bool,
) -> i64 {
    let list = findb::settle::open_entries(db, acct, p, false).expect("读未清往来分录");
    let hit = list.iter().find(|e| {
        e.aux_key.contains(aux_tag)
            && e.summary.contains(summary_key)
            && (if debit_side {
                e.debit == amount
            } else {
                e.credit == amount
            })
    });
    match hit {
        Some(e) => e.entry_id,
        None => panic!(
            "找不到未清分录：{acct} / {aux_tag} / {summary_key} / {}",
            amount.fmt_money()
        ),
    }
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
        tax_no: "91330100MAOXVAT888".into(),
        enable_qty: true,
        enable_foreign: false,
        enable_production: false,
        enable_audit: false,
        require_cashier: false,
        biz_accounts: Default::default(),
        budget_control: String::new(),
        doc_prefixes: Default::default(),
        require_audit: false,
        voucher_words: fincore::chart::default_voucher_words(),
        print: Default::default(),
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

    // 银行改成厂里的叫法（G 银行=对公有票户、N 银行=结算户）；收入按加工费口径
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

    // 预付电费：1123 预付账款下面挂一个明细（充值时进这里，抄表后结转）
    let p1123 = findb::accounts::get(&db, "1123")
        .expect("读1123")
        .expect("缺1123");
    findb::accounts::insert(
        &db,
        &fincore::Account::new("112301".to_string(), "预付电费".to_string(), p1123.category),
    )
    .expect("增 112301 预付电费");

    // 制造费用加五个明细：房租 / 化工材料 / 委外加工费 / 宿舍租金 / 社会保险费
    let p5101 = findb::accounts::get(&db, "5101")
        .expect("读5101")
        .expect("缺5101");
    for (code, name) in [
        ("510106", "房租"),
        ("510107", "化工材料"),
        ("510108", "委外加工费"),
        ("510109", "宿舍租金"),
        ("510110", "社会保险费"),
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
        (AuxKind::Item, "LS01", "硫酸"),
        (AuxKind::Item, "LS02", "磷酸"),
        (AuxKind::Item, "LS03", "硝酸"),
        (AuxKind::Item, "PJ01", "片碱"),
        (AuxKind::Item, "YN01", "亚硝酸钠"),
        (AuxKind::Item, "RL01", "染料"),
        (AuxKind::Item, "CY01", "除油剂"),
        (AuxKind::Item, "FB01", "封闭剂"),
    ];
    for (kind, code, name) in &auxs {
        findb::auxs::insert(&db, &fincore::AuxEntity::new(*kind, *code, *name))
            .expect("建辅助档案");
    }

    // ---------- 期初（2026-10 月末余额：借 387,320 = 贷 387,320） ----------
    // 期初用借正贷负记：借方科目记正数，贷方科目记负数。
    let begins: Vec<BegRow> = vec![
        ("1001", None, "18000", None),               // 现金（含微信零钱）
        ("100201", bk("B01"), "126000", None),       // G银行
        ("100202", bk("B02"), "54000", None),        // N银行
        ("112301", None, "4000", None),              // 预付电费（电卡里还有 4,000）
        ("112201", cu("C01"), "60000", None),        // 宏远汽配 10 月欠款
        ("112201", cu("C02"), "30000", None),        // 精密电子 10 月欠款
        ("140302", it("LS01"), "2000", Some("2")),   // 硫酸 2 吨
        ("140302", it("LS02"), "4600", Some("1")),   // 磷酸 1 吨
        ("140302", it("LS03"), "1240", Some("0.5")), // 硝酸 0.5 吨
        ("140302", it("PJ01"), "2300", Some("1")),   // 片碱 1 吨
        ("140302", it("YN01"), "975", Some("0.5")),  // 亚钠 0.5 吨
        ("140302", it("RL01"), "2000", Some("50")),  // 染料 50 公斤
        ("140302", it("CY01"), "1065", Some("5")),   // 除油剂 5 桶
        ("140302", it("FB01"), "1140", Some("4")),   // 封闭剂 4 桶
        ("160102", None, "120000", None),            // 固定资产-机器设备原值
        ("1602", None, "-40000", None),              // 累计折旧
        ("220201", su("S01"), "-46000", None),       // 欠恒信化工
        ("220201", su("S02"), "-18000", None),       // 欠大发外发
        ("4001", None, "-300000", None),             // 实收资本
        ("4103", None, "-23320", None),              // 本年利润（1-10 月累计）
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

    let p = Period::new(2026, 11).unwrap();

    // ---------- 先把金额算清楚（含税拆不含税、进项、领用、附加税） ----------
    //
    // 全部按票面来：3 张 13% 专票销项；进项分 13% / 9% / 3% 三档。
    let (s1_net, s1_tax) = split(m("226000"), 13); // 宏远汽配 C01
    let (s2_net, s2_tax) = split(m("169500"), 13); // 精密电子 C02
    let (s3_net, s3_tax) = split(m("113000"), 13); // 美丽灯具 C03

    // 8 种化工料（含税金额按「数量 × 含税单价」算好写死，方便对照采购单）
    let (ls01_net, ls01_tax) = split(m("9600"), 13); // 硫酸 8 吨 × 1,200
    let (ls02_net, ls02_tax) = split(m("15600"), 13); // 磷酸 3 吨 × 5,200
    let (ls03_net, ls03_tax) = split(m("5600"), 13); // 硝酸 2 吨 × 2,800
    let (pj01_net, pj01_tax) = split(m("7800"), 13); // 片碱 3 吨 × 2,600
    let (yn01_net, yn01_tax) = split(m("2200"), 13); // 亚钠 1 吨 × 2,200
    let (rl01_net, rl01_tax) = split(m("9000"), 13); // 染料 200 公斤 × 45
    let (cy01_net, cy01_tax) = split(m("5000"), 3); // 除油剂 20 桶 × 250（小规模 3%）
    let (fb01_net, fb01_tax) = split(m("4950"), 3); // 封闭剂 15 桶 × 330（小规模 3%）

    let (ele_net, ele_tax) = split(m("18000"), 13); // 电费（充值后按抄表结转）
    let (wat_net, wat_tax) = split(m("1090"), 9); // 水费
    let (sand_net, sand_tax) = split(m("12000"), 13); // 喷砂委外
    let (wire_net, wire_tax) = split(m("6180"), 3); // 拉丝委外（小规模 3%）
    let polish = m("3500"); // 抛光委外（没票，全额进成本，不能抵扣）

    let input_tax = ls01_tax
        + ls02_tax
        + ls03_tax
        + pj01_tax
        + yn01_tax
        + rl01_tax
        + cy01_tax
        + fb01_tax
        + ele_tax
        + wat_tax
        + sand_tax
        + wire_tax;
    let output_tax = s1_tax + s2_tax + s3_tax;
    let vat = output_tax - input_tax; // 本月应交增值税
    let ct = pct(vat, 7); // 城市维护建设税 7%
    let edu = pct(vat, 3); // 教育费附加 3%
    let lc = pct(vat, 2); // 地方教育附加 2%
    let surtax = ct + edu + lc; // 税金及附加合计

    // 工资：车间 62 人 198,400 + 管理 8 人 44,000 = 242,400
    let wage_shop = m("198400");
    let wage_admin = m("44000");
    let wage_total = wage_shop + wage_admin; // 242,400
    let si_shop = m("32000"); // 车间单位社保
    let si_admin = m("7800"); // 管理单位社保
    let si_self = m("11800"); // 代扣个人社保
    let itax = m("1300"); // 代扣个人所得税
    let si_total = si_shop + si_admin + si_self; // 应缴社保合计 51,600
    let paid_wage = wage_total - si_self - itax; // 实发 229,300

    // 车间领用：期初 + 本月购入，全部投产，期末库存 0
    let use_ls01 = m("2000") + ls01_net;
    let use_ls02 = m("4600") + ls02_net;
    let use_ls03 = m("1240") + ls03_net;
    let use_pj01 = m("2300") + pj01_net;
    let use_yn01 = m("975") + yn01_net;
    let use_rl01 = m("2000") + rl01_net;
    let use_cy01 = m("1065") + cy01_net;
    let use_fb01 = m("1140") + fb01_net;
    let chem_use =
        use_ls01 + use_ls02 + use_ls03 + use_pj01 + use_yn01 + use_rl01 + use_cy01 + use_fb01;

    // 制造费用九项 → 结转主营业务成本
    let mfg_wage = wage_shop; // 510101
    let mfg_dep = m("6000"); // 510102
    let mfg_util = ele_net + wat_net; // 510103
    let mfg_labor = m("2600"); // 510105 劳保用品
    let mfg_rent = m("20000"); // 510106 厂房房租（无票）
    let mfg_chem = chem_use; // 510107 化工材料
    let mfg_out = sand_net + wire_net + polish; // 510108 委外加工费
    let mfg_dorm = m("6000"); // 510109 宿舍租金（无票）
    let mfg_si = si_shop; // 510110 社会保险费
    let mfg_total = mfg_wage
        + mfg_dep
        + mfg_util
        + mfg_labor
        + mfg_rent
        + mfg_chem
        + mfg_out
        + mfg_dorm
        + mfg_si; // 371,099.66 → 主营业务成本

    // ---------- 11 月凭证（全部末级科目；借贷先手算平） ----------
    let specs: Vec<Spec> = vec![
        // 付1：电卡充值 20,000——充值不是费用，先挂预付电费
        spec(
            "2026-11-02",
            "付",
            1,
            vec![
                dr("112301", "充N银行结算户电卡", m("20000"), None),
                cr("100202", "充N银行结算户电卡", m("20000"), bk("B02")),
            ],
        ),
        // 付2：厂房房租（个人房东，没有票，全额进制造费用）
        spec(
            "2026-11-03",
            "付",
            2,
            vec![
                dr("510106", "付11月厂房房租（无票）", m("20000"), None),
                cr("100201", "付11月厂房房租（无票）", m("20000"), bk("B01")),
            ],
        ),
        // 付3：宿舍租金（员工宿舍，也是无票支出）
        spec(
            "2026-11-03",
            "付",
            3,
            vec![
                dr("510109", "付11月员工宿舍租金（无票）", m("6000"), None),
                cr("100202", "付11月员工宿舍租金（无票）", m("6000"), bk("B02")),
            ],
        ),
        // 转1：宏远汽配完工开票（含税 226,000 = 200,000 + 26,000 销项）
        spec(
            "2026-11-04",
            "转",
            1,
            vec![
                dr("112201", "宏远汽配氧化加工费11-A批", m("226000"), cu("C01")),
                cr("600101", "氧化加工费-A批", s1_net, None),
                cr("22210102", "销项税额13%", s1_tax, None),
            ],
        ),
        // 转2：购硫酸 8 吨（恒信化工，13% 专票）
        spec(
            "2026-11-05",
            "转",
            2,
            vec![
                dr_qty(
                    "140302",
                    "购硫酸8吨",
                    ls01_net,
                    it("LS01"),
                    "8",
                    unit_price(ls01_net, 80),
                ),
                dr("22210101", "进项税额13%（硫酸）", ls01_tax, None),
                cr("220201", "欠恒信化工硫酸款", m("9600"), su("S01")),
            ],
        ),
        // 转3：购磷酸 3 吨
        spec(
            "2026-11-05",
            "转",
            3,
            vec![
                dr_qty(
                    "140302",
                    "购磷酸3吨",
                    ls02_net,
                    it("LS02"),
                    "3",
                    unit_price(ls02_net, 30),
                ),
                dr("22210101", "进项税额13%（磷酸）", ls02_tax, None),
                cr("220201", "欠恒信化工磷酸款", m("15600"), su("S01")),
            ],
        ),
        // 转4：精密电子完工开票（含税 169,500 = 150,000 + 19,500）
        spec(
            "2026-11-06",
            "转",
            4,
            vec![
                dr("112201", "精密电子氧化加工费11月", m("169500"), cu("C02")),
                cr("600101", "氧化加工费-精密电子", s2_net, None),
                cr("22210102", "销项税额13%", s2_tax, None),
            ],
        ),
        // 转5：购硝酸 2 吨
        spec(
            "2026-11-07",
            "转",
            5,
            vec![
                dr_qty(
                    "140302",
                    "购硝酸2吨",
                    ls03_net,
                    it("LS03"),
                    "2",
                    unit_price(ls03_net, 20),
                ),
                dr("22210101", "进项税额13%（硝酸）", ls03_tax, None),
                cr("220201", "欠恒信化工硝酸款", m("5600"), su("S01")),
            ],
        ),
        // 转6：购片碱 3 吨
        spec(
            "2026-11-07",
            "转",
            6,
            vec![
                dr_qty(
                    "140302",
                    "购片碱3吨",
                    pj01_net,
                    it("PJ01"),
                    "3",
                    unit_price(pj01_net, 30),
                ),
                dr("22210101", "进项税额13%（片碱）", pj01_tax, None),
                cr("220201", "欠恒信化工片碱款", m("7800"), su("S01")),
            ],
        ),
        // 收1：收宏远汽配货款 100,000（G银行，有票）
        spec(
            "2026-11-08",
            "收",
            1,
            vec![
                dr("100201", "收宏远汽配货款", m("100000"), bk("B01")),
                cr("112201", "收宏远汽配货款", m("100000"), cu("C01")),
            ],
        ),
        // 转7：美丽灯具完工开票（含税 113,000 = 100,000 + 13,000）
        spec(
            "2026-11-10",
            "转",
            7,
            vec![
                dr("112201", "美丽灯具厂氧化加工费", m("113000"), cu("C03")),
                cr("600101", "氧化加工费-美丽灯具", s3_net, None),
                cr("22210102", "销项税额13%", s3_tax, None),
            ],
        ),
        // 转8：购亚硝酸钠 1 吨
        spec(
            "2026-11-11",
            "转",
            8,
            vec![
                dr_qty(
                    "140302",
                    "购亚硝酸钠1吨",
                    yn01_net,
                    it("YN01"),
                    "1",
                    unit_price(yn01_net, 10),
                ),
                dr("22210101", "进项税额13%（亚钠）", yn01_tax, None),
                cr("220201", "欠恒信化工亚钠款", m("2200"), su("S01")),
            ],
        ),
        // 转9：购染料 200 公斤
        spec(
            "2026-11-11",
            "转",
            9,
            vec![
                dr_qty(
                    "140302",
                    "购染料200公斤",
                    rl01_net,
                    it("RL01"),
                    "200",
                    unit_price(rl01_net, 2000),
                ),
                dr("22210101", "进项税额13%（染料）", rl01_tax, None),
                cr("220201", "欠恒信化工染料款", m("9000"), su("S01")),
            ],
        ),
        // 转10：购除油剂 20 桶（亮洁是小规模，只开 3% 征收率专票）
        spec(
            "2026-11-12",
            "转",
            10,
            vec![
                dr_qty(
                    "140302",
                    "购除油剂20桶",
                    cy01_net,
                    it("CY01"),
                    "20",
                    unit_price(cy01_net, 200),
                ),
                dr("22210101", "进项税额3%（除油剂）", cy01_tax, None),
                cr("220201", "欠亮洁抛光厂除油剂款", m("5000"), su("S03")),
            ],
        ),
        // 转11：购封闭剂 15 桶（同上 3%）
        spec(
            "2026-11-12",
            "转",
            11,
            vec![
                dr_qty(
                    "140302",
                    "购封闭剂15桶",
                    fb01_net,
                    it("FB01"),
                    "15",
                    unit_price(fb01_net, 150),
                ),
                dr("22210101", "进项税额3%（封闭剂）", fb01_tax, None),
                cr("220201", "欠亮洁抛光厂封闭剂款", m("4950"), su("S03")),
            ],
        ),
        // 收2：微信收宏远汽配 40,000（微信当现金，走 1001）
        spec(
            "2026-11-13",
            "收",
            2,
            vec![
                dr("1001", "微信收宏远汽配货款", m("40000"), None),
                cr("112201", "微信收宏远汽配货款", m("40000"), cu("C01")),
            ],
        ),
        // 转12：喷砂委外 12,000（大发开 13% 专票，挂账）
        spec(
            "2026-11-14",
            "转",
            12,
            vec![
                dr("510108", "喷砂委外加工费", sand_net, None),
                dr("22210101", "进项税额13%（喷砂）", sand_tax, None),
                cr("220201", "欠大发外发喷砂款", m("12000"), su("S02")),
            ],
        ),
        // 付4：抛光委外 3,500（对方不开发票，全额进成本，不能抵扣）
        spec(
            "2026-11-14",
            "付",
            4,
            vec![
                dr("510108", "抛光委外加工费（无票）", polish, None),
                cr("100202", "抛光委外加工费（无票）", polish, bk("B02")),
            ],
        ),
        // 付5：水费 1,090（自来水公司 9% 专票，含税 1,090 → 不含税 1,000 + 税 90）
        spec(
            "2026-11-15",
            "付",
            5,
            vec![
                dr("510103", "付11月水费", wat_net, None),
                dr("22210101", "进项税额9%（水费）", wat_tax, None),
                cr("100201", "付11月水费", m("1090"), bk("B01")),
            ],
        ),
        // 转13：拉丝委外 6,180（小规模 3% 专票，只能抵 180）
        spec(
            "2026-11-16",
            "转",
            13,
            vec![
                dr("510108", "拉丝委外加工费", wire_net, None),
                dr("22210101", "进项税额3%（拉丝）", wire_tax, None),
                cr("220201", "欠大发外发拉丝款", m("6180"), su("S02")),
            ],
        ),
        // 收3：精密电子打 45,000，其中 15,000 是替美丽灯具付的（A 付 B 的货款）
        spec(
            "2026-11-17",
            "收",
            3,
            vec![
                dr(
                    "100202",
                    "收精密电子货款（含代付美丽灯具15000）",
                    m("45000"),
                    bk("B02"),
                ),
                cr(
                    "112201",
                    "收精密电子货款（含代付美丽灯具15000）",
                    m("45000"),
                    cu("C02"),
                ),
            ],
        ),
        // 转14：收精密电子银行承兑汇票 115,000（先挂应收票据，别直接冲应收）
        spec(
            "2026-11-18",
            "转",
            14,
            vec![
                dr("112101", "收精密电子银行承兑汇票", m("115000"), None),
                cr("112201", "收精密电子银行承兑汇票", m("115000"), cu("C02")),
            ],
        ),
        // 付6：劳保用品 2,600（手套、口罩、工作服，现金付）
        spec(
            "2026-11-19",
            "付",
            6,
            vec![
                dr("510105", "买车间劳保用品（无票）", m("2600"), None),
                cr("1001", "买车间劳保用品（无票）", m("2600"), None),
            ],
        ),
        // 付7：办公用品 1,200
        spec(
            "2026-11-20",
            "付",
            7,
            vec![
                dr("660202", "买办公用品（无票）", m("1200"), None),
                cr("1001", "买办公用品（无票）", m("1200"), None),
            ],
        ),
        // 付8：付恒信化工 60,000（欠 49,800 也照付 60,000——付款和单据本来就不一一对应）
        spec(
            "2026-11-20",
            "付",
            8,
            vec![
                dr("220201", "付恒信化工货款", m("60000"), su("S01")),
                cr("100201", "付恒信化工货款", m("60000"), bk("B01")),
            ],
        ),
        // 付9：业务招待费 2,800（请客户吃饭，现金付）
        spec(
            "2026-11-22",
            "付",
            9,
            vec![
                dr("660204", "请客户吃饭（无票）", m("2800"), None),
                cr("1001", "请客户吃饭（无票）", m("2800"), None),
            ],
        ),
        // 付10：付大发外发 30,000（欠 18,000+12,000 也没对上）
        spec(
            "2026-11-24",
            "付",
            10,
            vec![
                dr("220201", "付大发外发加工费", m("30000"), su("S02")),
                cr("100202", "付大发外发加工费", m("30000"), bk("B02")),
            ],
        ),
        // 付11：G银行转 N银行 200,000（自己的两个户互转，不是收入也不是费用）
        spec(
            "2026-11-24",
            "付",
            11,
            vec![
                dr("100202", "G银行转N银行备发工资", m("200000"), bk("B02")),
                cr("100201", "G银行转N银行备发工资", m("200000"), bk("B01")),
            ],
        ),
        // 转15：计提 11 月工资（70 人：车间 62 人 198,400 + 管理 8 人 44,000）
        spec(
            "2026-11-25",
            "转",
            15,
            vec![
                dr("510101", "计提车间工资62人", wage_shop, None),
                dr("660201", "计提管理人员工资8人", wage_admin, None),
                cr("221101", "计提11月工资", wage_total, None),
            ],
        ),
        // 转16：计提单位承担的社保（车间进制造费用，管理进管理费用）
        spec(
            "2026-11-25",
            "转",
            16,
            vec![
                dr("510110", "计提车间社保（单位部分）", si_shop, None),
                dr("660207", "计提管理社保（单位部分）", si_admin, None),
                cr("221103", "计提11月单位社保", si_shop + si_admin, None),
            ],
        ),
        // 收4：收美丽灯具 97,950（开票 113,000，抹零 50，后面用差异核销清掉）
        spec(
            "2026-11-25",
            "收",
            4,
            vec![
                dr(
                    "100201",
                    "收美丽灯具厂货款（抹零50）",
                    m("97950"),
                    bk("B01"),
                ),
                cr(
                    "112201",
                    "收美丽灯具厂货款（抹零50）",
                    m("97950"),
                    cu("C03"),
                ),
            ],
        ),
        // 付12：发工资（银行代发实发数，同时代扣个人社保和个税）
        spec(
            "2026-11-26",
            "付",
            12,
            vec![
                dr("221101", "发11月工资", wage_total, None),
                cr("100202", "发11月工资（实发）", paid_wage, bk("B02")),
                cr("221103", "代扣个人社保", si_self, None),
                cr("222107", "代扣个人所得税", itax, None),
            ],
        ),
        // 付13：缴社保（单位 39,800 + 个人 11,800）
        spec(
            "2026-11-27",
            "付",
            13,
            vec![
                dr("221103", "缴11月社保", si_total, None),
                cr("100201", "缴11月社保", si_total, bk("B01")),
            ],
        ),
        // 收5：微信收宏远汽配 20,000
        spec(
            "2026-11-27",
            "收",
            5,
            vec![
                dr("1001", "微信收宏远汽配货款", m("20000"), None),
                cr("112201", "微信收宏远汽配货款", m("20000"), cu("C01")),
            ],
        ),
        // 转17：把承兑汇票背书给恒信化工 15,000（拿票抵应付，不经过银行）
        spec(
            "2026-11-28",
            "转",
            17,
            vec![
                dr("220201", "承兑汇票背书给恒信化工", m("15000"), su("S01")),
                cr("112101", "承兑汇票背书给恒信化工", m("15000"), None),
            ],
        ),
        // 收6：承兑汇票到期兑付 100,000 进 G银行
        spec(
            "2026-11-28",
            "收",
            6,
            vec![
                dr("100201", "承兑汇票到期兑付", m("100000"), bk("B01")),
                cr("112101", "承兑汇票到期兑付", m("100000"), None),
            ],
        ),
        // 付14：付恒信化工尾款 800，实付 780（抹零 20，差额进 660305 贷方）
        spec(
            "2026-11-29",
            "付",
            14,
            vec![
                dr("220201", "付恒信化工尾款（抹零20）", m("800"), su("S01")),
                cr("1001", "付恒信化工尾款（抹零20）", m("780"), None),
                cr("660305", "付款抹零收益", m("20"), None),
            ],
        ),
        // 付15：银行手续费 100
        spec(
            "2026-11-29",
            "付",
            15,
            vec![
                dr("660303", "付银行手续费", m("100"), None),
                cr("100201", "付银行手续费", m("100"), bk("B01")),
            ],
        ),
        // 转18：车间领用化工料（8 种料全部投产，数量按「期初+本月购入」出库）
        spec(
            "2026-11-30",
            "转",
            18,
            vec![
                dr("510107", "车间领用化工料", chem_use, None),
                cr_qty(
                    "140302",
                    "出库：硫酸",
                    use_ls01,
                    it("LS01"),
                    "10",
                    unit_price(use_ls01, 100),
                ),
                cr_qty(
                    "140302",
                    "出库：磷酸",
                    use_ls02,
                    it("LS02"),
                    "4",
                    unit_price(use_ls02, 40),
                ),
                cr_qty(
                    "140302",
                    "出库：硝酸",
                    use_ls03,
                    it("LS03"),
                    "2.5",
                    unit_price(use_ls03, 25),
                ),
                cr_qty(
                    "140302",
                    "出库：片碱",
                    use_pj01,
                    it("PJ01"),
                    "4",
                    unit_price(use_pj01, 40),
                ),
                cr_qty(
                    "140302",
                    "出库：亚硝酸钠",
                    use_yn01,
                    it("YN01"),
                    "1.5",
                    unit_price(use_yn01, 15),
                ),
                cr_qty(
                    "140302",
                    "出库：染料",
                    use_rl01,
                    it("RL01"),
                    "250",
                    unit_price(use_rl01, 2500),
                ),
                cr_qty(
                    "140302",
                    "出库：除油剂",
                    use_cy01,
                    it("CY01"),
                    "25",
                    unit_price(use_cy01, 250),
                ),
                cr_qty(
                    "140302",
                    "出库：封闭剂",
                    use_fb01,
                    it("FB01"),
                    "19",
                    unit_price(use_fb01, 190),
                ),
            ],
        ),
        // 转19 的空缺是有意的：电费凭证不由手写，改为「电费台账 → 结转」生成
        // （见下面 `电费台账` 那段）。台账上的用量、单价、含税金额与凭证一一对应，
        // 会计不用两头对——这是充值式电费唯一不容易记错的做法。
        // 转19：计提折旧
        spec(
            "2026-11-30",
            "转",
            19,
            vec![
                dr("510102", "计提11月设备折旧", mfg_dep, None),
                cr("1602", "计提11月设备折旧", mfg_dep, None),
            ],
        ),
        // 转20：制造费用结转主营业务成本（九项一起转，一分不留）
        spec(
            "2026-11-30",
            "转",
            20,
            vec![
                dr("6401", "结转11月加工成本", mfg_total, None),
                cr("510101", "结转车间工资", mfg_wage, None),
                cr("510102", "结转折旧", mfg_dep, None),
                cr("510103", "结转水电", mfg_util, None),
                cr("510105", "结转劳保", mfg_labor, None),
                cr("510106", "结转厂房房租", mfg_rent, None),
                cr("510107", "结转化工材料", mfg_chem, None),
                cr("510108", "结转委外加工费", mfg_out, None),
                cr("510109", "结转宿舍租金", mfg_dorm, None),
                cr("510110", "结转车间社保", mfg_si, None),
            ],
        ),
        // 转21：转出未交增值税（销项 - 进项 = 48,759.66，别把进项销项对冲到 0 就不管了）
        spec(
            "2026-11-30",
            "转",
            21,
            vec![
                dr("22210104", "转出本月未交增值税", vat, None),
                cr("222102", "转出本月未交增值税", vat, None),
            ],
        ),
        // 转22：按应交增值税计提附加税（城建 7% + 教育 3% + 地方教育 2%）
        spec(
            "2026-11-30",
            "转",
            22,
            vec![
                dr("6403", "计提11月附加税", surtax, None),
                cr("222103", "城市维护建设税7%", ct, None),
                cr("222104", "教育费附加3%", edu, None),
                cr("222105", "地方教育附加2%", lc, None),
            ],
        ),
    ];

    // 先按顺序记账（结转损益留到核销做完再生成：抹零差异会动 660305）。
    // 分两段是给「电费台账结转」腾位置：它的凭证落在 510103（制造费用），
    // 必须排在制造费用结转之前，否则这个月的电费进不了成本。
    let mfg_at = specs
        .iter()
        .position(|s| s.word == "转" && s.no == 20)
        .expect("specs 里必须有制造费用结转（转-20）");
    let mut n = 0;
    for s in &specs[..mfg_at] {
        post_spec(&db, p, s);
        n += 1;
    }

    // ---------- 电费台账：充值（付1 已记）→ 抄表 → 结转 ----------
    //
    // 电费的凭证不是手写出来的，是从台账「结转」出来的。三步各就各位：
    // 充值只动 112301（付-1 已经记了 20,000），抄表只是登记一行，
    // 结转才把「本期用量」变成费用并拆出进项——这正是充值式电费唯一不容易记错的走法。
    let mut reading = findb::meter::Reading {
        id: 0,
        kind: findb::meter::KIND_ELECTRIC.to_string(),
        meter_no: "E-01".to_string(),
        period: p.ymm(),
        read_date: "2026-11-30".to_string(),
        qty: m("15000"),
        unit_price: m("1.2"),
        amount_incl: m("18000"),
        tax_rate: "0.13".to_string(),
        voucher_id: 0,
        memo: "11 月车间总表".to_string(),
        created_by: String::new(),
        created_at: String::new(),
    };
    findb::meter::add(&db, &mut reading, "李会计").expect("登记 11 月抄表");
    let elec_vid = findb::meter::close(
        &db,
        p,
        Some(findb::meter::KIND_ELECTRIC),
        Some("112301"),
        None,
        NaiveDate::parse_from_str("2026-11-30", "%Y-%m-%d").unwrap(),
        "李会计",
    )
    .expect("电费结转")
    .expect("应有一行待结转的抄表记录");
    n += 1;
    println!(
        "电费台账：抄表 15000 度 × 1.20，结转凭证 #{elec_vid}（借 510103 15,929.20 + 进项 2,070.80 / 贷 112301 18,000）"
    );

    for s in &specs[mfg_at..] {
        post_spec(&db, p, s);
        n += 1;
    }
    println!("导入并记账 {n} 张业务凭证");

    // ---------- 往来期初明细（启用前的逐单欠款：这就是"基础对应表"的期初部分） ----------
    //
    // 为什么需要它：核销配对只认凭证分录，期初余额没有分录、配不上。
    // 期初欠款的逐单台账记在这里，进账龄分析与未清口径。
    for (kind, party, name, doc_no, doc_date, amount) in [
        (
            "ar",
            "C01",
            "宏远汽配厂",
            "JF-202610-001",
            "2026-10-26",
            "60000",
        ),
        (
            "ar",
            "C02",
            "精密电子厂",
            "JF-202610-002",
            "2026-10-28",
            "30000",
        ),
        (
            "ap",
            "S01",
            "恒信化工商行",
            "CL-202610-001",
            "2026-10-22",
            "46000",
        ),
        (
            "ap",
            "S02",
            "大发外发加工厂",
            "WW-202610-001",
            "2026-10-25",
            "18000",
        ),
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

    // ---------- 三笔「不一一对应」的往来：代收代付 / 抹零 / 三方抵账 ----------
    //
    // 这三笔是这套账的核心教学点：现实里 A 付的是 B 的货款、收钱时抹个零、
    // 客户欠我的直接去顶我欠供应商的。系统各有专门入口，不能靠硬调账。

    // ① 代收代付：精密电子打来的 45,000 里有 15,000 是替美丽灯具付的。
    //    互抵时系统自动出过渡凭证：借 应收-精密电子 15,000 / 贷 应收-美丽灯具 15,000，
    //    两边同时清，跟那张收款合起来净额就是「借 银行 45,000 / 贷 应收-精密电子 30,000」。
    let inv_c03 = entry_id(
        &db,
        "112201",
        p,
        "C03",
        "美丽灯具厂氧化加工费",
        m("113000"),
        true,
    );
    let recv_c02 = entry_id(
        &db,
        "112201",
        p,
        "C02",
        "收精密电子货款（含代付美丽灯具15000）",
        m("45000"),
        false,
    );
    //
    //    这件事在系统里叫「收款认领」：一笔钱认给几个客户。走的是 `claim::apply`，
    //    它内部对**同一客户**走普通核销（不出凭证），对**跨客户**自动出上面那张过渡凭证。
    //    认领是把「钱属于谁」这个判断落到纸上，不是金额配对——所以它单列一层。
    let inv_c02 = entry_id(
        &db,
        "112201",
        p,
        "C02",
        "精密电子氧化加工费11月",
        m("169500"),
        true,
    );
    let claimed = findb::claim::apply(
        &db,
        "112201",
        recv_c02,
        &[
            findb::claim::Pick {
                entry_id: inv_c02,
                amount: m("30000"),
            },
            findb::claim::Pick {
                entry_id: inv_c03,
                amount: m("15000"),
            },
        ],
        NaiveDate::parse_from_str("2026-11-17", "%Y-%m-%d").unwrap(),
        "李会计",
    )
    .expect("收款认领");
    // 过渡凭证由认领自己记账（它知道这张票是哪个判断引出来的），这里不再补记
    println!(
        "收款认领：{} 认给 2 个客户（同客户 30,000 + 跨客户 15,000），过渡凭证 {} 张",
        claimed.amount.fmt_money(),
        claimed.bridges.len()
    );

    // ② 三方抵账：精密电子欠我 20,000，我欠恒信化工 20,000，三家说好互顶。
    //    往来对冲单审核时生成凭证「借 220201-恒信 / 贷 112201-精密电子」，两端同时核销。
    let off_id = findb::offset::offset_create(
        &db,
        NaiveDate::parse_from_str("2026-11-24", "%Y-%m-%d").unwrap(),
        "C02",
        "S01",
        "112201",
        "220201",
        m("20000"),
        "精密电子 ↔ 恒信化工 三方抵账",
        "李会计",
    )
    .expect("建往来对冲单");
    let (off_vid, off_pairs) =
        findb::offset::offset_audit(&db, off_id, "李会计").expect("审核对冲单");
    findb::vouchers::post(&db, off_vid, "李会计").expect("对冲凭证记账");
    println!("往来对冲单已审核：凭证 #{off_vid}，自动核销 {off_pairs} 笔");

    // ③ 收款抹零：美丽灯具开票 113,000，实收 97,950（另 15,000 已由精密电子代付），
    //    差额 50 走差异核销 → 系统自动出一张「借 660305 / 贷 应收-美丽灯具 50」，
    //    应收全额结清，抹零落到财务费用，账龄里不会剩 50 挂着烂。
    let recv_c03 = entry_id(
        &db,
        "112201",
        p,
        "C03",
        "收美丽灯具厂货款（抹零50）",
        m("97950"),
        false,
    );
    let diff = findb::settle::settle_with_diff(
        &db,
        inv_c03,
        recv_c03,
        m("97950"),
        m("50"),
        "660305",
        "李会计",
    )
    .expect("收款抹零差异核销");
    if let Some(vid) = diff.diff_voucher_id {
        // 系统把差异凭证开在发票日期上；抹零其实是收款那天松的口，
        // 改到收款日期，会计翻开凭证一眼就能和银行流水对上。
        if let Some(mut v) = findb::vouchers::get(&db, vid).expect("读差异凭证") {
            v.date = NaiveDate::parse_from_str("2026-11-25", "%Y-%m-%d").unwrap();
            findb::vouchers::save(&db, &mut v).expect("改差异凭证日期");
        }
        findb::vouchers::post(&db, vid, "李会计").expect("抹零差异凭证记账");
    }

    // ---------- 录发票（一般纳税人的命门：进项只认「已认证」的专票） ----------
    //
    // 账上的 22210101 / 22210102 是「会计按票据算的税」，增值税申报表是「按发票算的税」，
    // 两边必须一分不差。所以每张**有票**的采购、费用、销售都在这里登记；没票的一律不登
    // （厂房房租、宿舍、抛光、劳保、办公、招待、工资——这正是本月进项只有 9,740.34 的原因）。
    // 进项票 status 必须是 "verified"（已认证）才进申报表，待认证的票不算数。
    let outs = [
        (
            "26880001",
            "2026-11-04",
            "宏远汽配厂",
            "226000",
            "氧化加工费 11-A 批",
        ),
        (
            "26880002",
            "2026-11-06",
            "精密电子厂",
            "169500",
            "氧化加工费 11 月",
        ),
        (
            "26880003",
            "2026-11-10",
            "美丽灯具厂",
            "113000",
            "氧化加工费 11 月",
        ),
    ];
    for (number, date, buyer, incl, memo) in outs {
        put_invoice(&db, &invoice("out", number, date, buyer, m(incl), 13, memo));
    }
    let ins = [
        ("2026-11-05", "恒信化工商行", "9600", 13, "硫酸 8 吨"),
        ("2026-11-05", "恒信化工商行", "15600", 13, "磷酸 3 吨"),
        ("2026-11-07", "恒信化工商行", "5600", 13, "硝酸 2 吨"),
        ("2026-11-07", "恒信化工商行", "7800", 13, "片碱 3 吨"),
        ("2026-11-11", "恒信化工商行", "2200", 13, "亚硝酸钠 1 吨"),
        ("2026-11-11", "恒信化工商行", "9000", 13, "染料 200 公斤"),
        (
            "2026-11-12",
            "亮洁抛光厂",
            "5000",
            3,
            "除油剂 20 桶（3% 征收率）",
        ),
        (
            "2026-11-12",
            "亮洁抛光厂",
            "4950",
            3,
            "封闭剂 15 桶（3% 征收率）",
        ),
        ("2026-11-14", "大发外发加工厂", "12000", 13, "喷砂委外加工"),
        ("2026-11-15", "市政自来水公司", "1090", 9, "11 月水费"),
        (
            "2026-11-16",
            "大发外发加工厂",
            "6180",
            3,
            "拉丝委外加工（3% 征收率）",
        ),
        (
            "2026-11-30",
            "国网供电公司",
            "18000",
            13,
            "11 月电费（按抄表）",
        ),
    ];
    for (i, (date, seller, incl, rate, memo)) in ins.iter().enumerate() {
        let number = format!("{:08}", 51_000_000 + i);
        put_invoice(
            &db,
            &invoice("in", &number, date, seller, m(incl), *rate, memo),
        );
    }
    println!("录入发票：销项 3 张、进项 12 张（进项全部已认证）");

    // ---------- 结转损益（最后一步：把收入费用全部转进本年利润） ----------
    //
    // 用引擎现成的生成器：它按损益类科目净额出凭证，标 CarryForward 来源，
    // 利润表靠这个标记把结转分录剔除，才不会被"结转"重复计一遍。
    let chart = findb::accounts::chart(&db).expect("科目表");
    let snap = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::period(p).with_posted_only(true),
    )
    .expect("科目余额");
    let rows: Vec<_> = snap
        .carry_forward_rows(&chart)
        .into_iter()
        .filter(|r| !(r.debit - r.credit).round2().is_zero())
        .collect();
    let no = findb::vouchers::next_no(&db, p, "转").expect("取凭证号");
    let mut cv = generate_carry_forward(
        p,
        NaiveDate::parse_from_str("2026-11-30", "%Y-%m-%d").unwrap(),
        "转",
        no,
        &rows,
        &chart,
        PROFIT_ACCOUNT,
        "李会计",
    )
    .expect("生成结转损益凭证");
    let cid = findb::vouchers::save(&db, &mut cv).expect("保存结转凭证");
    findb::vouchers::post(&db, cid, "李会计").expect("结转凭证记账");
    n += 1;

    // ---------- 自动核销（FIFO：同单位内凭证之间能配的配上，尾数留给手工） ----------
    //
    // 配得上：宏远的 100,000+40,000+20,000 吃掉 226,000 发票的一部分；
    // 精密的票据 115,000 + 收款 30,000 吃掉 169,500；
    // 恒信两笔付款和背书吃掉采购款。
    // 配不上：10 月四笔期初单据（无分录）——它们在账龄表里，收到钱后余额自动轧平。
    // 故意留一笔**没认领**的收款（收-5 微信收宏远 20,000）：自动核销之后把它撤回来，
    // 于是账龄里就有「钱到了、还没落到哪张单上」的状态。打开「往来核销 → 收款认领」
    // 就能看到它躺在待认列表里，拿来练手正好——这是现实里最常见的起点。
    let idle_recv = entry_id(
        &db,
        "112201",
        p,
        "C01",
        "微信收宏远汽配货款",
        m("20000"),
        false,
    );
    let ar_settle =
        findb::settle::auto_settle(&db, "112201", p, Money::ZERO, "李会计").expect("应收自动核销");
    let ap_settle =
        findb::settle::auto_settle(&db, "220201", p, Money::ZERO, "李会计").expect("应付自动核销");
    println!(
        "自动核销：应收配对 {} 笔、应付配对 {} 笔",
        ar_settle.pairs, ap_settle.pairs
    );
    let undone = findb::settle::unsettle_entry(&db, idle_recv).expect("撤销那笔待认领的收款");
    println!("留了 1 笔 20,000 待认领（撤销 {undone} 条核销记录），往来核销 → 收款认领 可直接试");

    let n_acc = findb::accounts::list(&db).map(|v| v.len()).unwrap_or(0);
    let n_all = findb::vouchers::list(&db, &findb::vouchers::VoucherQuery::default())
        .map(|v| v.len())
        .unwrap_or(0);
    let revenue = s1_net + s2_net + s3_net;
    // 税负 = 应交增值税 / 不含税收入。人工占成本大头、进项只有 2% 的时候，
    // 一般纳税人的税负就是这么高——这个数字比「能抵多少」更值得老板记住。
    let burden = (vat * 100_i64)
        .checked_div(revenue)
        .unwrap_or(Money::ZERO)
        .round2();
    println!();
    println!("一般纳税人演示账套已就绪：{}", path.display());
    println!(
        "  科目 {n_acc} 个 · 凭证 {n_all} 张（业务 {n} 张 + 系统自动生成 3 张） · 11 月未结账（留给会计自己点）"
    );
    println!(
        "  销项 {} − 进项 {} = 应交增值税 {}（税负 {}%，附加税 {}）",
        output_tax.fmt_money(),
        input_tax.fmt_money(),
        vat.fmt_money(),
        burden.fmt_money(),
        surtax.fmt_money()
    );
    // 委外加工费三票对比：同样的钱，三种票进成本差 1,150 元/万元
    if let Ok(rows) = findb::outsource::compare(m("10000")) {
        println!();
        println!("委外加工费 10,000 元，三种票各进多少成本：");
        for r in rows {
            println!(
                "  {:<14} 不含税 {} · 进项 {}",
                r.ticket.label(),
                r.net.fmt_money(),
                r.tax.fmt_money()
            );
        }
    }
    // 申报前体检：这本账是干净的，应该 0 个错误；提醒项正好当教学材料
    if let Ok(rep) = findb::taxprecheck::precheck(&db, p) {
        println!();
        println!(
            "申报前体检：{} 个错误 / {} 个待确认 / {} 个提醒（{}）",
            rep.errors,
            rep.warns,
            rep.infos,
            if rep.can_file {
                "可以申报"
            } else {
                "先改再报"
            }
        );
        for it in &rep.items {
            // `Level::label()` 给的是前端用的英文类名（error/warn/info），控制台说人话。
            let tag = match it.level {
                findb::taxprecheck::Level::Error => "错误",
                findb::taxprecheck::Level::Warn => "待确认",
                findb::taxprecheck::Level::Info => "提醒",
            };
            println!("  [{tag}] {}", it.title);
        }
    }
    println!("  登录账号 admin / 口令 admin123");
    drop(db);
}

/// `cargo test` 把本文件当模块引入时会多出一个用不上的 main，允许。
#[allow(dead_code)]
fn main() {
    let path = PathBuf::from(env::args().nth(1).expect("usage: oxide_vat <输出路径.fbk>"));
    build(&path);
}
