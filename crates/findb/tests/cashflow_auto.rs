//! 现金流量表的「按对方科目自动归集」
//!
//! 每个用例都是一张真实凭证的形态，断言写死项目和金额。这些用例守的是一件事：
//! **没人标现金流量项目时，报表也能填出来；但只要有一点不确定，就必须老实说未归集。**

use chrono::NaiveDate;
use fincore::{AuxEntity, AuxKind, AuxRef, BookOptions, Chart, Entry, Money, Period, Voucher};
use findb::{accounts, auxs, reports, vouchers, Db};

const Y: i32 = 2026;

fn p(n: u32) -> Period {
    Period::new(Y, n).unwrap()
}
fn m(s: &str) -> Money {
    Money::parse(s).unwrap()
}
fn d(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(Y, 11, day).unwrap()
}

fn book() -> Db {
    let db = Db::in_memory(&BookOptions {
        start_period: p(11),
        enable_audit: false,
        require_audit: false,
        require_cashier: false,
        ..BookOptions::default()
    })
    .unwrap();
    for (kind, code, name) in [
        (AuxKind::Bank, "B01", "工行基本户"),
        (AuxKind::Customer, "C01", "华达五金"),
        (AuxKind::Supplier, "S01", "宏源氧化"),
        (AuxKind::Item, "HG001", "氧化辅料"),
    ] {
        auxs::insert(&db, &AuxEntity::new(kind, code, name)).unwrap();
    }
    db
}

/// 一行分录：(科目, 借方, 贷方, 辅助核算, 现金流量项目, 数量, 单价)
struct E {
    code: &'static str,
    debit: &'static str,
    credit: &'static str,
    aux: Option<AuxRef>,
    cf: Option<&'static str>,
    qty: Option<&'static str>,
    price: Option<&'static str>,
}

fn line(code: &'static str, debit: &'static str, credit: &'static str) -> E {
    E {
        code,
        debit,
        credit,
        aux: None,
        cf: None,
        qty: None,
        price: None,
    }
}

fn bank(code: &'static str, debit: &'static str, credit: &'static str) -> E {
    let mut a = AuxRef::default();
    a.bank = Some("B01".to_string());
    E {
        aux: Some(a),
        ..line(code, debit, credit)
    }
}

fn with_cf(mut e: E, cf: &'static str) -> E {
    e.cf = Some(cf);
    e
}

/// 保存并记账一张凭证
fn post(db: &Db, chart: &Chart, no: i32, spec: &[E]) {
    let mut v = Voucher::new(p(11), d(10 + no as u32), "记", no);
    v.prepared_by = "张会计".to_string();
    for (i, e) in spec.iter().enumerate() {
        let mut en = Entry::new((i + 1) as i32, e.code, "测试");
        en.debit = m(e.debit);
        en.credit = m(e.credit);
        if let Some(a) = &e.aux {
            en.aux = a.clone();
        }
        if let Some(cf) = e.cf {
            en.aux.cash_flow = Some(cf.to_string());
        }
        if let Some(q) = e.qty {
            en.qty = Some(m(q));
            en.price = e.price.map(m);
        }
        v.push_entry(en);
    }
    assert!(v.balanced(), "第 {no} 号凭证借贷不平衡");
    let iss = fincore::engine::validate_voucher(
        &v,
        &fincore::engine::ValidateCtx::new(chart, &db.options(), None),
    );
    assert!(
        iss.is_empty(),
        "第 {no} 号凭证校验未通过：{:?}",
        iss.iter().collect::<Vec<_>>()
    );
    let id = vouchers::save(db, &mut v).expect("保存失败");
    vouchers::post_many(db, &[id], "李主管").expect("记账失败");
}

/// 取某个现金流量项目的（借方, 贷方）发生额
fn amount_of(db: &Db, code: &str) -> (Money, Money) {
    let d = reports::cash_flow_amounts_detail(db, p(11), p(11), None).unwrap();
    d.amounts
        .get(code)
        .copied()
        .unwrap_or((Money::ZERO, Money::ZERO))
}

fn detail(db: &Db) -> reports::CashFlowAmounts {
    reports::cash_flow_amounts_detail(db, p(11), p(11), None).unwrap()
}

// ---------------------------------------------------------------------------

/// 收加工费（含销项税）：归「销售商品、提供劳务收到的现金」0101
///
/// 这条守的是「税额科目是伴生行」：一张凭证有 600102 和 22210102 两个对方科目，
/// 不能因为多了一行销项税就说「分不清」。
#[test]
fn sales_with_output_tax_goes_to_operating_inflow() {
    let db = book();
    let chart = accounts::chart(&db).unwrap();
    post(
        &db,
        &chart,
        1,
        &[
            bank("100201", "113000.00", "0"),
            line("600102", "0", "100000.00"),
            line("22210102", "0", "13000.00"),
        ],
    );

    let (dr, cr) = amount_of(&db, "0101");
    assert_eq!(dr, m("113000.00"), "收到的现金应全额进 0101");
    assert_eq!(cr, Money::ZERO);

    let d = detail(&db);
    assert_eq!(d.auto_assigned, 1, "应当有 1 行按对方科目归集");
    assert_eq!(d.ambiguous, 0);
    assert!(d.unassigned.is_zero(), "不该有未归集：{:?}", d.unassigned);
}

/// 采购辅料（含进项税）：归「购买商品、接受劳务支付的现金」0104
#[test]
fn purchase_with_input_tax_goes_to_operating_outflow() {
    let db = book();
    let chart = accounts::chart(&db).unwrap();
    let mut item = AuxRef::default();
    item.item = Some("HG001".to_string());
    post(
        &db,
        &chart,
        1,
        &[
            E {
                aux: Some(item),
                qty: Some("100"),
                price: Some("100.00"),
                ..line("140302", "10000.00", "0")
            },
            line("22210101", "1300.00", "0"),
            bank("100201", "0", "11300.00"),
        ],
    );

    let (dr, cr) = amount_of(&db, "0104");
    assert_eq!(cr, m("11300.00"), "付出去的现金应全额进 0104");
    assert_eq!(dr, Money::ZERO);
    assert_eq!(detail(&db).auto_assigned, 1);
}

/// 交税：同一科目 2221 按**方向**走不同项目（流出 → 0106）
#[test]
fn paying_tax_goes_to_tax_paid() {
    let db = book();
    let chart = accounts::chart(&db).unwrap();
    post(
        &db,
        &chart,
        1,
        &[
            line("22210103", "5000.00", "0"),
            bank("100201", "0", "5000.00"),
        ],
    );

    let (_, cr) = amount_of(&db, "0106");
    assert_eq!(cr, m("5000.00"));
}

/// 利息支出：660301 比 6603 更具体，归筹资活动 0304
#[test]
fn interest_expense_goes_to_financing_not_operating() {
    let db = book();
    let chart = accounts::chart(&db).unwrap();
    post(
        &db,
        &chart,
        1,
        &[
            line("660301", "1000.00", "0"),
            bank("100201", "0", "1000.00"),
        ],
    );

    let (_, cr) = amount_of(&db, "0304");
    assert_eq!(cr, m("1000.00"), "利息支出属于筹资活动");
    let (_, op) = amount_of(&db, "0107");
    assert!(op.is_zero(), "不该同时进 0107");
}

/// 提现 / 存现：货币资金内部划转，**不是现金流量**
#[test]
fn cash_transfer_is_not_a_cash_flow() {
    let db = book();
    let chart = accounts::chart(&db).unwrap();
    // 借 库存现金 2000 / 贷 银行 2000
    post(
        &db,
        &chart,
        1,
        &[line("1001", "2000.00", "0"), bank("100201", "0", "2000.00")],
    );

    let d = detail(&db);
    assert!(d.amounts.is_empty(), "内部划转不该产生任何现金流量项目");
    assert_eq!(d.transfers, 2, "两行货币资金都被识别为划转");
    assert!(d.unassigned.is_zero(), "内部划转不能算「未归集」");
}

/// 对方科目指向两个不同项目：**不猜**，留在未归集并计数
#[test]
fn conflicting_counter_accounts_are_not_guessed() {
    let db = book();
    let chart = accounts::chart(&db).unwrap();
    // 一笔钱既像收货款（600102）又像卖设备（160101）
    post(
        &db,
        &chart,
        1,
        &[
            bank("100201", "50000.00", "0"),
            line("600102", "0", "30000.00"),
            line("160101", "0", "20000.00"),
        ],
    );

    let d = detail(&db);
    assert_eq!(d.ambiguous, 1, "应当记为「猜不了」而不是随便归一个");
    assert_eq!(d.auto_assigned, 0);
    assert_eq!(d.unassigned, m("50000.00"), "未归集金额要如实反映");
    assert!(d.amounts.is_empty());
}

/// 凭证上标了项目：**标注优先**，规则不许覆盖
#[test]
fn manual_label_wins_over_rules() {
    let db = book();
    let chart = accounts::chart(&db).unwrap();
    post(
        &db,
        &chart,
        1,
        &[
            with_cf(bank("100201", "50000.00", "0"), "0204"),
            line("600102", "0", "50000.00"),
        ],
    );

    let (dr, _) = amount_of(&db, "0204");
    assert_eq!(dr, m("50000.00"), "人工标注的 0204 必须生效");
    let d = detail(&db);
    assert_eq!(d.auto_assigned, 0, "这条不是猜的，不该计入自动归集");
}

/// 一整月的表：三段净额加起来必须等于货币资金的实际变动
#[test]
fn statement_ties_with_cash_balance() {
    let db = book();
    let chart = accounts::chart(&db).unwrap();
    // 期初给银行 100,000
    findb::balances::upsert_begin(
        &db,
        &findb::balances::BeginRow {
            id: 0,
            account_code: "100201".to_string(),
            aux: {
                let mut a = AuxRef::default();
                a.bank = Some("B01".to_string());
                a
            },
            year_begin: m("100000.00"),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        },
    )
    .unwrap();

    post(
        &db,
        &chart,
        1,
        &[
            bank("100201", "113000.00", "0"),
            line("600102", "0", "100000.00"),
            line("22210102", "0", "13000.00"),
        ],
    );
    post(
        &db,
        &chart,
        2,
        &[
            line("510103", "8000.00", "0"),
            bank("100201", "0", "8000.00"),
        ],
    );
    post(
        &db,
        &chart,
        3,
        &[
            line("160101", "30000.00", "0"),
            bank("100201", "0", "30000.00"),
        ],
    );

    let stmt = reports::cash_flow_statement(&db, p(11), p(11), None).unwrap();
    assert!(stmt.ties(), "三段净额 + 未归集 必须等于货币资金变动");
    assert_eq!(stmt.operating_net, m("105000.00"), "经营净额 113000-8000");
    assert_eq!(stmt.investing_net, m("-30000.00"), "投资净额 -30000");
    assert!(stmt.unassigned.is_zero(), "这组凭证应当全部能归集");
    assert_eq!(
        stmt.auto_assigned, 3,
        "三张凭证各一行现金，全部按对方科目归集"
    );
    assert_eq!(stmt.end_cash, m("175000.00"));
}
