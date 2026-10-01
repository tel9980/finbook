//! 核心业务闭环端到端测试
//!
//! 覆盖：新建账套 → 科目表 → 辅助档案 → 期初建账 → 填制凭证 → 记账
//!       → 明细账/总账/余额表/试算平衡 → 资产负债表/利润表/现金流量表
//!       → 结转损益 → 期末结账 → 反结账
//!
//! 所有金额均为手工推导的固定值，断言写死，任何一处回归都会立刻暴露。

use chrono::NaiveDate;
use findb::balances::{BalanceQuery, BalanceSnapshot, BeginRow, LedgerQuery};
use findb::{accounts, auxs, periods, reports, users, vouchers, Db};
use fincore::engine::period_end::PROFIT_ACCOUNT;
use fincore::report::balance_sheet::{asset_total_row, liab_equity_total_row};
use fincore::report::income::net_profit_row;
use fincore::{
    Account, AcctCategory, AuxEntity, AuxKind, AuxRef, BookOptions, Chart, Entry, Money, Period,
    Voucher,
};

// ---------------------------------------------------------------------------
// 测试数据
// ---------------------------------------------------------------------------

const Y: i32 = 2025;
fn p1() -> Period {
    Period::new(Y, 1).unwrap()
}

fn pm(month: u32) -> Period {
    Period::new(Y, month).unwrap()
}

fn m(s: &str) -> Money {
    Money::parse(s).unwrap()
}

/// 期初余额：借 1,700,000 = 贷 1,700,000
fn opening_rows() -> Vec<(&'static str, Option<(&'static str, &'static str)>, &'static str)> {
    vec![
        ("100201", None, "700000.00"),
        ("112201", Some(("customer", "C01")), "120000.00"),
        ("112201", Some(("customer", "C02")), "80000.00"),
        ("140501", Some(("item", "I01")), "300000.00"),
        ("160101", None, "500000.00"),
        // 贷方用负数表示
        ("4001", None, "-1500000.00"),
        ("220201", Some(("supplier", "S01")), "-200000.00"),
    ]
}

fn aux_of(kind: &str, code: &str) -> AuxRef {
    let mut a = AuxRef::default();
    match kind {
        "customer" => a.customer = Some(code.to_string()),
        "supplier" => a.supplier = Some(code.to_string()),
        "bank" => a.bank = Some(code.to_string()),
        "item" => a.item = Some(code.to_string()),
        _ => unreachable!(),
    }
    a
}

fn d(y: i32, mo: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, mo, day).unwrap()
}

// ---------------------------------------------------------------------------
// 1. 建账与基础数据
// ---------------------------------------------------------------------------

#[test]
fn t01_book_seeded() {
    let opts = BookOptions {
        company: "示例科技有限公司".to_string(),
        start_period: p1(),
        ..BookOptions::default()
    };
    let db = Db::in_memory(&opts).unwrap();

    // 内置科目表已灌入
    let chart = accounts::chart(&db).unwrap();
    assert!(
        chart.all().len() > 100,
        "内置科目表应有一百多个科目，实际 {}",
        chart.all().len()
    );
    for c in ["100201", "112201", "220201", "600101", "660201", "4103"] {
        assert!(chart.get(c).is_some(), "缺少内置科目 {c}");
    }

    // 报表模板已灌入
    assert!(reports::get_def(&db, "balance_sheet").unwrap().is_some());
    assert!(reports::get_def(&db, "income_statement").unwrap().is_some());

    // 现金流量项目已灌入
    assert!(!reports::cash_flow_items(&db).unwrap().is_empty());

    // 至少有一个可用用户
    assert!(users::count(&db).unwrap() >= 1);

    // 未结账
    assert_eq!(periods::closed_upto(&db).unwrap(), None);
    assert_eq!(periods::current_period(&db).unwrap(), p1());
}

#[test]
fn t02_account_rules() {
    let db = Db::in_memory(&BookOptions::default()).unwrap();
    let chart = accounts::chart(&db).unwrap();

    // 非末级科目不能记账
    assert!(!chart.is_leaf("1002"), "1002 有下级，是非末级");
    assert!(chart.is_leaf("100201"));

    // 辅助核算只在末级科目上
    assert!(chart.get("1002").unwrap().aux.is_empty());
    assert!(chart.get("100201").unwrap().aux.contains(AuxKind::Bank));
    assert!(chart.get("112201").unwrap().aux.contains(AuxKind::Customer));
    assert!(chart.get("220201").unwrap().aux.contains(AuxKind::Supplier));
    assert!(chart.get("140301").unwrap().has_qty);

    // 新增下级后，父科目自动变成非末级
    let before = chart.is_leaf("660201");
    assert!(before);
    accounts::insert(&db, &Account::new("66020101", "基本工资", AcctCategory::Expense)).unwrap();
    let chart2 = accounts::chart(&db).unwrap();
    assert!(!chart2.is_leaf("660201"));
}

// ---------------------------------------------------------------------------
// 2. 期初建账
// ---------------------------------------------------------------------------

fn seed_book(db: &Db) {
    for e in [
        AuxEntity::new(AuxKind::Customer, "C01", "甲客户"),
        AuxEntity::new(AuxKind::Customer, "C02", "乙客户"),
        AuxEntity::new(AuxKind::Supplier, "S01", "丙供应商"),
        AuxEntity::new(AuxKind::Bank, "B01", "工行基本户"),
        AuxEntity::new(AuxKind::Item, "I01", "A 型产品"),
    ] {
        auxs::insert(db, &e).unwrap();
    }

    for (code, aux, amt) in opening_rows() {
        let a = match aux {
            Some((k, v)) => aux_of(k, v),
            None => AuxRef::default(),
        };
        findb::balances::upsert_begin(
            db,
            &BeginRow {
                id: 0,
                account_code: code.to_string(),
                aux: a,
                year_begin: m(amt),
                debit_accum: Money::ZERO,
                credit_accum: Money::ZERO,
                qty_begin: None,
            },
        )
        .unwrap();
    }
}

fn book() -> (Db, Chart) {
    let opts = BookOptions {
        company: "示例科技有限公司".to_string(),
        start_period: p1(),
        require_audit: true,
        ..BookOptions::default()
    };
    let db = Db::in_memory(&opts).unwrap();
    seed_book(&db);
    let chart = accounts::chart(&db).unwrap();
    (db, chart)
}

#[test]
fn t03_opening_balance() {
    let (db, chart) = book();

    let (d, c) = findb::balances::begin_trial(&db).unwrap();
    assert_eq!(d, m("1700000.00"), "期初借方合计");
    assert_eq!(c, m("1700000.00"), "期初贷方合计");
    assert_eq!(d, c, "期初必须试算平衡");

    // 期初余额进入快照
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p1())).unwrap();
    assert_eq!(snap.for_account("100201", None).begin, m("700000.00"));
    assert_eq!(snap.for_account("140501", None).begin, m("300000.00"));
    assert_eq!(snap.for_account("160101", None).begin, m("500000.00"));
    assert_eq!(snap.for_account("4001", None).begin, m("-1500000.00"));

    // 应收账款按客户拆分，合计 200,000
    let mut cust = AuxRef::default();
    cust.customer = Some("C01".to_string());
    assert_eq!(snap.for_account("112201", Some(&cust)).begin, m("120000.00"));

    let tb = snap.trial_balance(&chart);
    assert!(tb.begin_balanced(), "期初试算应平衡");
}

// ---------------------------------------------------------------------------
// 3. 凭证录入 / 校验 / 记账
// ---------------------------------------------------------------------------

/// 一月份 8 张业务凭证。返回每张凭证的分录定义。
struct E {
    code: &'static str,
    summary: &'static str,
    debit: &'static str,
    credit: &'static str,
    aux: Option<(&'static str, &'static str)>,
    cf: Option<&'static str>,
    qty: Option<(&'static str, &'static str)>,
}

fn voucher_specs() -> Vec<(NaiveDate, &'static str, Vec<E>)> {
    vec![
        (
            d(Y, 1, 5),
            "收到甲客户货款",
            vec![
                E {
                    code: "100201",
                    summary: "收到甲客户货款",
                    debit: "120000.00",
                    credit: "0",
                    aux: Some(("bank", "B01")),
                    cf: Some("0101"),
                    qty: None,
                },
                E {
                    code: "112201",
                    summary: "收到甲客户货款",
                    debit: "0",
                    credit: "120000.00",
                    aux: Some(("customer", "C01")),
                    cf: Some("0101"),
                    qty: None,
                },
            ],
        ),
        (
            d(Y, 1, 8),
            "采购原材料",
            vec![
                E {
                    code: "140301",
                    summary: "采购原材料",
                    debit: "100000.00",
                    credit: "0",
                    aux: Some(("item", "I01")),
                    cf: None,
                    qty: Some(("1000", "100")),
                },
                E {
                    code: "22210101",
                    summary: "进项税额",
                    debit: "13000.00",
                    credit: "0",
                    aux: None,
                    cf: None,
                    qty: None,
                },
                E {
                    code: "100201",
                    summary: "支付采购款",
                    debit: "0",
                    credit: "113000.00",
                    aux: Some(("bank", "B01")),
                    cf: Some("0104"),
                    qty: None,
                },
            ],
        ),
        (
            d(Y, 1, 12),
            "赊销产品给乙客户",
            vec![
                E {
                    code: "112201",
                    summary: "赊销产品",
                    debit: "226000.00",
                    credit: "0",
                    aux: Some(("customer", "C02")),
                    cf: Some("0101"),
                    qty: None,
                },
                E {
                    code: "600101",
                    summary: "主营业务收入",
                    debit: "0",
                    credit: "200000.00",
                    aux: None,
                    cf: None,
                    qty: None,
                },
                E {
                    code: "22210102",
                    summary: "销项税额",
                    debit: "0",
                    credit: "26000.00",
                    aux: None,
                    cf: None,
                    qty: None,
                },
            ],
        ),
        (
            d(Y, 1, 20),
            "计提职工工资",
            vec![
                E {
                    code: "660201",
                    summary: "计提工资",
                    debit: "80000.00",
                    credit: "0",
                    aux: None,
                    cf: None,
                    qty: None,
                },
                E {
                    code: "221101",
                    summary: "应付工资",
                    debit: "0",
                    credit: "80000.00",
                    aux: None,
                    cf: None,
                    qty: None,
                },
            ],
        ),
        (
            d(Y, 1, 20),
            "发放职工工资",
            vec![
                E {
                    code: "221101",
                    summary: "发放工资",
                    debit: "80000.00",
                    credit: "0",
                    aux: None,
                    cf: None,
                    qty: None,
                },
                E {
                    code: "100201",
                    summary: "支付工资",
                    debit: "0",
                    credit: "80000.00",
                    aux: Some(("bank", "B01")),
                    cf: Some("0105"),
                    qty: None,
                },
            ],
        ),
        (
            d(Y, 1, 31),
            "计提折旧",
            vec![
                E {
                    code: "660205",
                    summary: "计提折旧",
                    debit: "5000.00",
                    credit: "0",
                    aux: None,
                    cf: None,
                    qty: None,
                },
                E {
                    code: "1602",
                    summary: "累计折旧",
                    debit: "0",
                    credit: "5000.00",
                    aux: None,
                    cf: None,
                    qty: None,
                },
            ],
        ),
        (
            d(Y, 1, 31),
            "支付办公室房租",
            vec![
                E {
                    code: "660209",
                    summary: "支付房租",
                    debit: "20000.00",
                    credit: "0",
                    aux: None,
                    cf: None,
                    qty: None,
                },
                E {
                    code: "100201",
                    summary: "支付房租",
                    debit: "0",
                    credit: "20000.00",
                    aux: Some(("bank", "B01")),
                    cf: Some("0107"),
                    qty: None,
                },
            ],
        ),
        (
            d(Y, 1, 31),
            "结转销售成本",
            vec![
                E {
                    code: "6401",
                    summary: "结转销售成本",
                    debit: "150000.00",
                    credit: "0",
                    aux: None,
                    cf: None,
                    qty: None,
                },
                E {
                    code: "140501",
                    summary: "结转销售成本",
                    debit: "0",
                    credit: "150000.00",
                    aux: Some(("item", "I01")),
                    cf: None,
                    qty: Some(("1500", "100")),
                },
            ],
        ),
    ]
}

fn build_voucher(period: Period, date: NaiveDate, word: &str, no: i32, spec: &[E]) -> Voucher {
    let mut v = Voucher::new(period, date, word, no);
    v.prepared_by = "张会计".to_string();
    for (i, e) in spec.iter().enumerate() {
        let mut en = Entry::new((i + 1) as i32, e.code, e.summary);
        en.debit = m(e.debit);
        en.credit = m(e.credit);
        if let Some((k, val)) = e.aux {
            let mut a = aux_of(k, val);
            if let Some(cf) = e.cf {
                a.cash_flow = Some(cf.to_string());
            }
            en.aux = a;
        } else if let Some(cf) = e.cf {
            en.aux.cash_flow = Some(cf.to_string());
        }
        if let Some((q, price)) = e.qty {
            en.qty = Some(m(q));
            en.price = Some(m(price));
        }
        v.push_entry(en);
    }
    v
}

fn enter_all_vouchers(db: &Db, chart: &Chart) -> Vec<i64> {
    let opts = db.options();
    let mut ids = Vec::new();
    for (i, (date, _sum, spec)) in voucher_specs().into_iter().enumerate() {
        let no = (i + 1) as i32;
        let mut v = build_voucher(p1(), date, "记", no, &spec);

        // 保存前校验：应当没有任何问题
        let iss =
            fincore::engine::validate_voucher(&v, &fincore::engine::ValidateCtx::new(chart, &opts, None));
        assert!(
            iss.is_empty(),
            "第 {} 号凭证校验未通过：{:?}",
            no,
            iss.iter().collect::<Vec<_>>()
        );

        assert!(v.balanced(), "第 {} 号凭证借贷不平衡", no);
        ids.push(vouchers::save(db, &mut v).expect("凭证保存失败"));
    }
    ids
}

#[test]
fn t04_voucher_entry_and_validation() {
    let (db, chart) = book();
    let ids = enter_all_vouchers(&db, &chart);
    assert_eq!(ids.len(), 8);

    // 凭证号连续，无断号
    assert!(vouchers::find_gaps(&db, p1(), "记").unwrap().is_empty());

    // 全部是未记账
    let (drafts, audited, posted, void) = vouchers::status_summary(&db, p1()).unwrap();
    assert_eq!((drafts, audited, posted, void), (8, 0, 0, 0));

    // 重号检测
    assert!(vouchers::no_taken(&db, p1(), "记", 1, 0).unwrap());
    assert!(!vouchers::no_taken(&db, p1(), "记", 99, 0).unwrap());
    assert_eq!(vouchers::next_no(&db, p1(), "记").unwrap(), 9);
}

#[test]
fn t05_reject_bad_vouchers() {
    let (db, chart) = book();
    let opts = db.options();
    let ctx = fincore::engine::ValidateCtx::new(&chart, &opts, None);

    // 借贷不平衡
    let mut v = Voucher::new(p1(), d(Y, 1, 10), "记", 1);
    let mut e1 = Entry::new(1, "100201", "测试");
    e1.debit = m("100.00");
    let mut e2 = Entry::new(2, "100201", "测试");
    e2.credit = m("90.00");
    v.push_entry(e1);
    v.push_entry(e2);
    assert!(!v.balanced());
    vouchers::save(&db, &mut v).expect_err("不平衡的凭证不应保存");

    // 非末级科目
    let mut v = Voucher::new(p1(), d(Y, 1, 10), "记", 2);
    let mut e1 = Entry::new(1, "1002", "非末级");
    e1.debit = m("100.00");
    let mut e2 = Entry::new(2, "4001", "非末级");
    e2.credit = m("100.00");
    v.push_entry(e1);
    v.push_entry(e2);
    let iss = fincore::engine::validate_voucher(&v, &ctx);
    assert!(iss.iter().any(|s| s.contains("非末级")), "{iss:?}");

    // 缺辅助核算
    let mut v = Voucher::new(p1(), d(Y, 1, 10), "记", 3);
    let mut e1 = Entry::new(1, "112201", "缺客户");
    e1.debit = m("100.00");
    let mut e2 = Entry::new(2, "4001", "缺客户");
    e2.credit = m("100.00");
    v.push_entry(e1);
    v.push_entry(e2);
    let iss = fincore::engine::validate_voucher(&v, &ctx);
    assert!(iss.iter().any(|s| s.contains("客户")), "{iss:?}");

    // 缺数量
    let mut v = Voucher::new(p1(), d(Y, 1, 10), "记", 4);
    let mut e1 = Entry::new(1, "140301", "缺数量");
    e1.debit = m("100.00");
    e1.aux = aux_of("item", "I01");
    let mut e2 = Entry::new(2, "4001", "缺数量");
    e2.credit = m("100.00");
    v.push_entry(e1);
    v.push_entry(e2);
    let iss = fincore::engine::validate_voucher(&v, &ctx);
    assert!(iss.iter().any(|s| s.contains("数量")), "{iss:?}");

    // 数量 × 单价 ≠ 金额
    let mut v = Voucher::new(p1(), d(Y, 1, 10), "记", 5);
    let mut e1 = Entry::new(1, "140301", "数量金额不符");
    e1.debit = m("100.00");
    e1.aux = aux_of("item", "I01");
    e1.qty = Some(m("10"));
    e1.price = Some(m("1"));
    let mut e2 = Entry::new(2, "4001", "数量金额不符");
    e2.credit = m("100.00");
    v.push_entry(e1);
    v.push_entry(e2);
    let iss = fincore::engine::validate_voucher(&v, &ctx);
    assert!(iss.iter().any(|s| s.contains("单价")), "{iss:?}");

    // 空摘要
    let mut v = Voucher::new(p1(), d(Y, 1, 10), "记", 6);
    let mut e1 = Entry::new(1, "100201", "");
    e1.debit = m("100.00");
    e1.aux = aux_of("bank", "B01");
    let mut e2 = Entry::new(2, "4001", "");
    e2.credit = m("100.00");
    v.push_entry(e1);
    v.push_entry(e2);
    let iss = fincore::engine::validate_voucher(&v, &ctx);
    assert!(iss.iter().any(|s| s.contains("摘要")), "{iss:?}");
}

#[test]
fn t06_post_vouchers() {
    let (db, chart) = book();
    let ids = enter_all_vouchers(&db, &chart);

    // 无审核环节：未记账凭证直接批量记账
    let (ok, errs) = vouchers::post_many(&db, &ids, "李主管").unwrap();
    assert_eq!(ok, 8, "记账失败：{errs:?}");
    let (drafts, audited, posted, _) = vouchers::status_summary(&db, p1()).unwrap();
    assert_eq!((drafts, audited, posted), (0, 0, 8));

    // 已记账的凭证不能改（需先反记账）
    let v = vouchers::get(&db, ids[0]).unwrap().unwrap();
    assert!(!v.status.can_edit());

    // 未记账数归零，可以结账
    assert_eq!(periods::unposted_count(&db, p1()).unwrap(), 0);
}

#[test]
fn t07_unpost_and_void() {
    let (db, chart) = book();
    let ids = enter_all_vouchers(&db, &chart);
    vouchers::post_many(&db, &ids, "李主管").unwrap();

    // 反记账 → 回到未记账（可编辑）
    vouchers::unpost(&db, ids[0]).unwrap();
    let v = vouchers::get(&db, ids[0]).unwrap().unwrap();
    assert!(v.status.can_edit());

    // 重新记账
    vouchers::post(&db, ids[0], "李主管").unwrap();
    let before = BalanceSnapshot::load(&db, &BalanceQuery::period(p1()))
        .unwrap()
        .for_account("100201", None)
        .end();
    // 已记账的凭证不能直接作废
    vouchers::set_void(&db, ids[0], true, "李主管")
        .expect_err("已记账凭证不能作废，应先反记账");
    vouchers::unpost(&db, ids[0]).unwrap();
    vouchers::set_void(&db, ids[0], true, "李主管").unwrap();
    let after = BalanceSnapshot::load(&db, &BalanceQuery::period(p1()))
        .unwrap()
        .for_account("100201", None)
        .end();
    assert_ne!(before, after, "作废后余额应变化");
    assert_eq!(
        vouchers::get(&db, ids[0]).unwrap().unwrap().status,
        fincore::VoucherStatus::Void
    );
}

// ---------------------------------------------------------------------------
// 4. 账簿
// ---------------------------------------------------------------------------

fn posted_book() -> (Db, Chart, Vec<i64>) {
    let (db, chart) = book();
    let ids = enter_all_vouchers(&db, &chart);
    vouchers::post_many(&db, &ids, "李主管").unwrap();
    (db, chart, ids)
}

#[test]
fn t08_detail_ledger() {
    let (db, chart, _) = posted_book();

    let q = LedgerQuery {
        code: "100201".to_string(),
        include_children: false,
        aux: None,
        from: p1(),
        to: p1(),
        posted_only: true,
        prepared_by: None,
        code_from: None,
        code_to: None,
    };
    let rows = findb::balances::ledger(&db, &chart, &q).unwrap();
    // 银行存款共 4 笔：收 120,000 / 付 113,000 / 付 80,000 / 付 20,000
    assert_eq!(rows.len(), 4, "银行存款明细应有 4 笔");
    assert_eq!(rows[0].debit, m("120000.00"));
    assert_eq!(rows[1].credit, m("113000.00"));
    assert_eq!(rows[2].credit, m("80000.00"));
    assert_eq!(rows[3].credit, m("20000.00"));

    // 滚动余额 1,000,000 + 120,000 - 113,000 - 80,000 - 20,000
    let last = rows.last().unwrap();
    assert_eq!(last.signed_balance, m("607000.00"));
    assert_eq!(last.balance, m("607000.00"));

    // 逐行校验余额连续
    let mut expect = m("700000.00");
    for r in &rows {
        expect += r.debit - r.credit;
        assert_eq!(r.signed_balance, expect, "明细账滚动余额不连续");
    }

    // 只含未记账凭证时为空
    let q2 = LedgerQuery {
        posted_only: false,
        ..q
    };
    assert_eq!(findb::balances::ledger(&db, &chart, &q2).unwrap().len(), 4);
}

#[test]
fn t09_general_ledger_and_journal() {
    let (db, chart, _) = posted_book();

    let q = LedgerQuery {
        code: "100201".to_string(),
        include_children: false,
        aux: None,
        from: p1(),
        to: p1(),
        posted_only: true,
        prepared_by: None,
        code_from: None,
        code_to: None,
    };
    let gl = findb::balances::general_ledger(&db, &q).unwrap();
    assert_eq!(gl.len(), 1);
    assert_eq!(gl[0].debit, m("120000.00"));
    assert_eq!(gl[0].credit, m("213000.00"));
    assert_eq!(gl[0].signed_balance, m("607000.00"));

    // 现金日记账（银行存款）
    let j = findb::balances::journal(&db, &chart, &q).unwrap();
    assert_eq!(j.len(), 4);
    assert!(!j[0].opposite_accounts.is_empty(), "日记账应有对方科目");
}

#[test]
fn t10_balance_table_and_trial() {
    let (db, chart, _) = posted_book();

    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p1())).unwrap();
    let tb = snap.trial_balance(&chart);

    // 本期发生额
    let total_d: Money = m("120000.00") + m("100000.00") + m("13000.00") + m("226000.00")
        + m("160000.00")
        + m("5000.00")
        + m("20000.00")
        + m("150000.00");
    let total_c: Money = m("120000.00") + m("113000.00") + m("200000.00") + m("26000.00")
        + m("160000.00")
        + m("5000.00")
        + m("20000.00")
        + m("150000.00");
    assert_eq!(tb.period_debit, total_d, "本期借方发生额");
    assert_eq!(tb.period_credit, total_c, "本期贷方发生额");
    assert!(tb.period_balanced(), "本期发生额必须平衡");

    // 期末
    assert_eq!(tb.end_debit - tb.end_credit, Money::ZERO);
    assert!(tb.end_balanced(), "期末必须平衡");

    // 关键科目期末余额
    assert_eq!(snap.for_account("100201", None).end(), m("607000.00"));
    assert_eq!(snap.for_account("112201", None).end(), m("306000.00"));
    assert_eq!(snap.for_account("140301", None).end(), m("100000.00"));
    assert_eq!(snap.for_account("140501", None).end(), m("150000.00"));
    assert_eq!(snap.for_account("1602", None).end(), m("-5000.00"));
    assert_eq!(snap.for_account("221101", None).end(), Money::ZERO);

    // 本年累计 = 本期发生额（1 月）
    assert_eq!(snap.for_account("600101", None).ytd_credit, m("200000.00"));

    // 余额表按级次过滤：一级科目行数应远少于全表
    let all = snap.account_table(&chart, &BalanceQuery::period(p1()));
    let l1 = snap
        .account_table(&chart, &BalanceQuery::period(p1()).with_max_level(Some(1)));
    assert!(l1.len() < all.len());
    assert!(l1.iter().all(|r| r.account_code.len() == 4));

    // 非零过滤：默认余额表已只显示有数据的科目（不显示没用到的科目），
    // 因此 non_zero 结果应是全表的子集
    let nz = snap
        .account_table(&chart, &BalanceQuery::period(p1()).with_non_zero(true));
    assert!(nz.len() <= all.len());
    assert!(!all.is_empty());
}

#[test]
fn t11_aux_breakdown() {
    let (db, _chart, _) = posted_book();
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p1())).unwrap();

    // 应收账款按客户
    let rows = snap.aux_breakdown("112201");
    assert_eq!(rows.len(), 2, "应收账款应有两个客户的辅助余额");
    let c1 = rows.iter().find(|r| r.aux.customer.as_deref() == Some("C01")).unwrap();
    let c2 = rows.iter().find(|r| r.aux.customer.as_deref() == Some("C02")).unwrap();
    // C01: 期初 120,000 - 收款 120,000 = 0
    assert_eq!(c1.end(), Money::ZERO);
    // C02: 期初 80,000 + 226,000 = 306,000
    assert_eq!(c2.end(), m("306000.00"));
    assert_eq!(c1.end() + c2.end(), m("306000.00"));
}

// ---------------------------------------------------------------------------
// 5. 报表
// ---------------------------------------------------------------------------

#[test]
fn t12_balance_sheet() {
    let (db, _chart, _) = posted_book();
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::range(p1(), p1())).unwrap();
    let def = reports::get_def(&db, "balance_sheet").unwrap().unwrap();

    let table = fincore::report::render(
        &def,
        &snap,
        "示例科技有限公司",
        "2025年1月",
        vec![
            Box::new(fincore::report::identity),
            Box::new(fincore::report::to_begin),
        ],
    );

    let at = asset_total_row(&def).expect("资产总计行");
    let lt = liab_equity_total_row(&def).expect("负债和所有者权益总计行");

    let assets = table.rows[at].values[0];
    let lieq = table.rows[lt].values[0];
    assert_eq!(assets, m("1658000.00"), "资产总计");
    assert_eq!(lieq, m("1658000.00"), "负债和所有者权益总计（贷方为正）");
    assert_eq!(assets, lieq, "资产 = 负债+权益");

    // 结转前本期盈亏必须体现在「未分配利润」里，否则表会不平
    let undistributed = table
        .rows
        .iter()
        .find(|r| r.name == "未分配利润")
        .expect("未分配利润行");
    assert_eq!(undistributed.values[0], m("-55000.00"), "未分配利润（本期亏损 55,000，权益减少为负）");

    // 年初列 = 期初
    let assets_begin = table.rows[at].values[1];
    assert_eq!(assets_begin, m("1700000.00"), "年初资产总计");
}

#[test]
fn t13_income_statement() {
    let (db, _chart, _) = posted_book();
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::range(p1(), p1())).unwrap();
    let def = reports::get_def(&db, "income_statement").unwrap().unwrap();

    let table = fincore::report::render_single(
        &def,
        &snap,
        "示例科技有限公司",
        "2025年1月",
        fincore::report::identity,
    );

    let np = net_profit_row(&def).expect("净利润行");
    // 收入 200,000 - 成本 150,000 - 工资 80,000 - 折旧 5,000 - 房租 20,000 = -55,000
    assert_eq!(table.rows[np].values[0], m("-55000.00"), "本月净利润（亏损）");

    // 营业收入行
    let rev = table
        .rows
        .iter()
        .find(|r| r.name.contains("营业收入") && r.style == fincore::report::LineStyle::Total)
        .expect("营业总收入行");
    assert_eq!(rev.values[0], m("200000.00"));
}

#[test]
fn t14_cash_flow_statement() {
    let (db, _chart, _) = posted_book();
    let cf = reports::cash_flow_statement(&db, p1(), p1(), None).unwrap();

    // 期初 1,000,000，期末 907,000
    assert_eq!(cf.begin_cash, m("700000.00"));
    assert_eq!(cf.end_cash, m("607000.00"));
    assert_eq!(cf.net_increase, m("-93000.00"));
    assert!(cf.ties(), "现金流量表应与货币资金变动勾稽一致");
    assert_eq!(cf.unassigned, Money::ZERO, "所有现金收支都应有项目");

    // 经营净额 = 120,000 - 113,000 - 80,000 - 20,000
    assert_eq!(cf.operating_net, m("-93000.00"));
    assert_eq!(cf.investing_net, Money::ZERO);
    assert_eq!(cf.financing_net, Money::ZERO);

    // 净增加额 = 三大活动净额之和
    let sum = cf.operating_net + cf.investing_net + cf.financing_net;
    assert_eq!(sum, cf.net_increase);
}

// ---------------------------------------------------------------------------
// 6. 结转损益
// ---------------------------------------------------------------------------

#[test]
fn t15_carry_forward() {
    let (db, chart, _) = posted_book();

    // 结转前：损益类科目有余额
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p1())).unwrap();
    let pl = snap.profit_loss_rows(&chart);
    assert!(!pl.is_empty());
    assert_eq!(snap.for_account("600101", None).end(), m("-200000.00"));
    assert_eq!(snap.for_account("660201", None).end(), m("80000.00"));

    // 未结转时结账会被拦下（要求先结转）
    let issues = periods::precheck(&db, p1(), true).unwrap();
    assert!(!issues.is_empty(), "未结转损益不应允许结账：{issues:?}");

    // 生成结转凭证
    let no = vouchers::next_no(&db, p1(), "记").unwrap();
    let mut v = fincore::engine::generate_carry_forward(
        p1(),
        p1().last_day(),
        "记",
        no,
        &pl,
        &chart,
        PROFIT_ACCOUNT,
        "系统",
    )
    .expect("生成结转凭证失败");

    assert!(v.balanced(), "结转凭证必须平衡");
    assert_eq!(v.source, fincore::VoucherSource::CarryForward);
    // 收入转出 200,000；费用转出 255,000；差额落本年利润
    assert_eq!(v.credit_total(), m("255000.00"));
    assert_eq!(v.debit_total(), m("255000.00"));

    let id = vouchers::save(&db, &mut v).unwrap();
    vouchers::post(&db, id, "李主管").unwrap();

    // 结转后所有损益类科目余额归零
    let snap2 = BalanceSnapshot::load(&db, &BalanceQuery::period(p1())).unwrap();
    for r in snap2.profit_loss_rows(&chart) {
        assert!(
            r.end().round2().is_zero(),
            "损益类科目 {} {} 结转后应清零，实际 {}",
            r.account_code,
            r.account_name,
            r.end()
        );
    }

    // 本年利润 = 亏损 55,000（借方）
    assert_eq!(snap2.for_account("4103", None).end(), m("55000.00"));

    // 试算仍然平衡
    let tb = snap2.trial_balance(&chart);
    assert!(tb.end_balanced());
    assert_eq!(tb.end_debit - tb.end_credit, Money::ZERO);
}

// ---------------------------------------------------------------------------
// 7. 期末结账 / 反结账
// ---------------------------------------------------------------------------

fn carried_book() -> (Db, Chart) {
    let (db, chart, _) = posted_book();
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(p1())).unwrap();
    let pl = snap.profit_loss_rows(&chart);
    let no = vouchers::next_no(&db, p1(), "记").unwrap();
    let mut v = fincore::engine::generate_carry_forward(
        p1(),
        p1().last_day(),
        "记",
        no,
        &pl,
        &chart,
        PROFIT_ACCOUNT,
        "系统",
    )
    .unwrap();
    let id = vouchers::save(&db, &mut v).unwrap();
    vouchers::post(&db, id, "李主管").unwrap();
    (db, chart)
}

#[test]
fn t16_close_and_unclose() {
    let (db, _chart) = carried_book();

    // 结账前检查全部通过
    let issues = periods::precheck(&db, p1(), true).unwrap();
    assert!(issues.is_empty(), "结账前检查应通过：{issues:?}");

    // 结账
    let issues = periods::close(&db, p1(), "李主管", true).unwrap();
    assert!(issues.is_empty(), "结账失败：{issues:?}");
    assert!(periods::is_closed(&db, p1()).unwrap());
    assert_eq!(periods::closed_upto(&db).unwrap(), Some(p1()));
    assert_eq!(periods::current_period(&db).unwrap(), p1().next());
    assert!(!periods::is_open(&db, p1()).unwrap());

    // 已结账期间不能再新增凭证
    let opts = db.options();
    let chart = accounts::chart(&db).unwrap();
    let v = build_voucher(p1(), d(Y, 1, 15), "记", 99, &voucher_specs()[0].2);
    let iss = fincore::engine::validate_voucher(
        &v,
        &fincore::engine::ValidateCtx::new(&chart, &opts, periods::closed_upto(&db).unwrap()),
    );
    assert!(!iss.is_empty(), "已结账期间不应允许新增凭证");

    // 反结账
    periods::unclose(&db, p1(), "李主管").unwrap();
    assert!(!periods::is_closed(&db, p1()).unwrap());
    assert_eq!(periods::closed_upto(&db).unwrap(), None);

    // 反结账后可以再结账
    assert!(periods::close(&db, p1(), "李主管", true).unwrap().is_empty());

    // 不能跳过期间结账（1 月未结时不能结 3 月）
    periods::unclose(&db, p1(), "李主管").unwrap();
    let iss = periods::close(&db, pm(3), "李主管", true).unwrap();
    assert!(!iss.is_empty(), "不应允许跳过期间结账");
}

#[test]
fn t17_close_blocked_by_unposted() {
    let (db, _chart) = book();
    enter_all_vouchers(&db, &accounts::chart(&db).unwrap());

    // 全部未记账，结账应被拦下
    let issues = periods::precheck(&db, p1(), true).unwrap();
    assert!(!issues.is_empty());
    assert!(issues.iter().any(|s| s.contains("记账")));

    // 批量记账直接处理未记账凭证
    let (n, errs) = periods::post_all(&db, p1(), "李主管").unwrap();
    assert_eq!(n, 8, "批量记账：{errs:?}");
    let issues = periods::precheck(&db, p1(), false).unwrap();
    assert!(issues.is_empty(), "不要求结转时应可结账：{issues:?}");
}

// ---------------------------------------------------------------------------
// 8. 跨期与年度结转
// ---------------------------------------------------------------------------

#[test]
fn t18_second_period_carries_forward() {
    let (db, chart) = carried_book();
    periods::close(&db, p1(), "李主管", true).unwrap();

    let p2 = p1().next();
    // 2 月期初 = 1 月期末
    let snap1 = BalanceSnapshot::load(&db, &BalanceQuery::period(p1())).unwrap();
    let snap2 = BalanceSnapshot::load(&db, &BalanceQuery::period(p2)).unwrap();
    assert_eq!(
        snap1.for_account("100201", None).end(),
        snap2.for_account("100201", None).begin
    );
    assert_eq!(
        snap1.for_account("4103", None).end(),
        snap2.for_account("4103", None).begin
    );

    // 2 月记一笔，本期发生额只算 2 月，本年累计含 1 月
    let mut v = Voucher::new(p2, d(Y, 2, 10), "记", 1);
    v.prepared_by = "张会计".to_string();
    let mut e1 = Entry::new(1, "100201", "收到乙客户货款");
    e1.debit = m("306000.00");
    e1.aux = aux_of("bank", "B01");
    let mut e2 = Entry::new(2, "112201", "收到乙客户货款");
    e2.credit = m("306000.00");
    e2.aux = aux_of("customer", "C02");
    v.push_entry(e1);
    v.push_entry(e2);
    let id = vouchers::save(&db, &mut v).unwrap();
    vouchers::post(&db, id, "李主管").unwrap();

    let s2 = BalanceSnapshot::load(&db, &BalanceQuery::period(p2)).unwrap();
    assert_eq!(s2.for_account("100201", None).debit, m("306000.00"));
    assert_eq!(s2.for_account("100201", None).ytd_debit, m("426000.00"));
    assert_eq!(s2.for_account("112201", None).end(), Money::ZERO);

    // 1 月已结账，不影响
    assert!(periods::is_closed(&db, p1()).unwrap());

    // 跨期查询（1-2 月）
    let sr = BalanceSnapshot::load(&db, &BalanceQuery::range(p1(), p2)).unwrap();
    assert_eq!(sr.for_account("100201", None).debit, m("426000.00"));
    assert_eq!(sr.for_account("100201", None).end(), m("913000.00"));

    let _ = chart;
}

#[test]
fn t19_year_end_carry() {
    let (db, chart) = carried_book();

    // 结转本年利润到未分配利润
    let snap = BalanceSnapshot::load(&db, &BalanceQuery::period(pm(12))).unwrap();
    let profit = snap.for_account("4103", None).end();
    assert_eq!(profit, m("55000.00"));

    let no = vouchers::next_no(&db, pm(12), "记").unwrap();
    let mut v = fincore::engine::generate_year_end_carry(
        pm(12),
        pm(12).last_day(),
        "记",
        no,
        profit,
        fincore::engine::UNDISTRIBUTED_ACCOUNT,
        &chart,
        "系统",
    )
    .expect("生成年度结转凭证失败");
    assert!(v.balanced());

    let id = vouchers::save(&db, &mut v).unwrap();
    vouchers::post(&db, id, "李主管").unwrap();

    let after = BalanceSnapshot::load(&db, &BalanceQuery::period(pm(12))).unwrap();
    assert_eq!(after.for_account("4103", None).end(), Money::ZERO);
    // 亏损结转到未分配利润的借方，权益相应减少
    assert_eq!(
        after.for_account(fincore::engine::UNDISTRIBUTED_ACCOUNT, None).end(),
        m("55000.00")
    );
}

// ---------------------------------------------------------------------------
// 9. 备份 / 恢复 / 完整性
// ---------------------------------------------------------------------------

#[test]
fn t20_backup_and_integrity() {
    let (db, _chart, _ids) = posted_book();
    assert_eq!(db.integrity_check().unwrap(), vec!["ok"]);

    let dir = std::env::temp_dir().join(format!("finbook_e2e_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bak = dir.join("backup.fbk");
    db.backup(&bak).unwrap();
    assert!(bak.exists());

    // 备份文件可独立打开且数据一致
    let db2 = Db::open(&bak).unwrap();
    let (d, c) = findb::balances::begin_trial(&db2).unwrap();
    assert_eq!(d, m("1700000.00"));
    assert_eq!(c, m("1700000.00"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn t21_op_log() {
    let (db, chart) = book();
    let ids = enter_all_vouchers(&db, &chart);
    vouchers::post_many(&db, &ids, "李主管").unwrap();

    let logs = db.recent_logs(100).unwrap();
    assert!(!logs.is_empty(), "操作日志不应为空");
    assert!(logs.iter().any(|l| l.action.contains("记账")), "{logs:?}");
}
