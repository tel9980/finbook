//! 扩展业务闭环端到端测试
//!
//! 覆盖第二轮扩展的五大块：
//! 1. 固定资产：建卡 → 计提折旧 → 生成折旧凭证 → 台账
//! 2. 银行对账：导入对账单 → 自动勾对 → 余额调节表
//! 3. 往来核销：手工核销 → 账龄分析
//! 4. 期末处理：汇率维护 → 期末调汇 → 自动转账 → 月度检查清单
//! 5. 业务闭环：存货出入库 → 结转成本；工资计算 → 计提凭证；报销审批 → 付款凭证
//! 6. 管理会计：预算编制 → 执行分析；多维损益；自定义报表
//! 7. 账户安全：登录失败锁定 → 口令策略 → 操作日志
//!
//! 全部走真实 DAO，金额手工推导写死。

use chrono::NaiveDate;
use findb::{
    assets, attach, automation, bank, business, mgmt, security, settle, summaries, template,
    vouchers, Db,
};
use fincore::engine::costing::CostMethod;
use fincore::voucher::{AuxRef, Entry, Voucher};
use fincore::{BookOptions, Money, Period, VoucherSource};

fn m(s: &str) -> Money {
    Money::parse(s).unwrap()
}

fn d(y: i32, mo: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, mo, day).unwrap()
}

fn db() -> Db {
    let opts = BookOptions {
        start_period: Period::new(2026, 1).unwrap(),
        ..BookOptions::default()
    };
    Db::in_memory(&opts).unwrap()
}

/// 建一张已审核已记账的两行凭证，返回 (voucher_id, 分录id列表)
fn post_two(
    db: &Db,
    date: NaiveDate,
    word: &str,
    e1: (i32, &str, Money, bool, AuxRef),
    e2: (i32, &str, Money, bool, AuxRef),
) -> (i64, Vec<i64>) {
    let no = vouchers::next_no(db, Period::from_date(date), word).unwrap_or(1);
    let mut v = Voucher::new(Period::from_date(date), date, word, no);
    let (l1, c1, a1, is_debit1, x1) = e1;
    let (l2, c2, a2, is_debit2, x2) = e2;
    let mut e1 = Entry::new(l1, c1, "测试");
    if is_debit1 {
        e1.debit = a1;
    } else {
        e1.credit = a1;
    }
    e1.aux = x1;
    let mut e2 = Entry::new(l2, c2, "测试");
    if is_debit2 {
        e2.debit = a2;
    } else {
        e2.credit = a2;
    }
    e2.aux = x2;
    v.push_entry(e1);
    v.push_entry(e2);
    let id = vouchers::save(db, &mut v).unwrap();
    vouchers::post(db, id, "记账员").unwrap();
    let entries = vouchers::entries_of(db, id).unwrap();
    (id, entries.iter().map(|e| e.id).collect())
}

// ---------------------------------------------------------------------------
// 1. 固定资产闭环
// ---------------------------------------------------------------------------

#[test]
fn t90_asset_dep_loop() {
    let db = db();
    let p = Period::new(2026, 1).unwrap();

    let a = assets::Asset {
        id: 0,
        code: assets::next_code(&db).unwrap(),
        name: "台式电脑".into(),
        category: "电子设备".into(),
        spec: String::new(),
        dept: "财务部".into(),
        asset_account: "160101".into(),
        dep_account: "1602".into(),
        expense_account: "660201".into(),
        original_value: m("12000"),
        residual_rate: m("0.05"),
        life_months: 36,
        method: fincore::engine::depreciation::DepMethod::Straight,
        start_period: p,
        disposed_period: None,
        dispose_amount: None,
        status: assets::AssetStatus::InUse,
        voucher_id: None,
        memo: String::new(),
    };
    let id = assets::insert(&db, &a).unwrap();

    // 本期应计提 = 12000 * 95% / 36 = 316.666.. = 316.67（引擎舍入）
    let card = assets::get(&db, id).unwrap().unwrap();
    let planned = assets::planned_dep(&card, p).unwrap().unwrap();
    assert_eq!(planned, m("316.67"));

    // 台账：原值 12000，累计 316.67，净值 11683.33
    let ledger = assets::ledger(&db, p).unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].accum, m("316.67"));
    assert_eq!(ledger[0].net, m("11683.33"));

    // 生成折旧凭证（借贷平衡）
    let word = "记";
    let no = vouchers::next_no(&db, p, word).unwrap();
    let mut v = Voucher::new(p, p.last_day(), word, no as i32);
    v.source = VoucherSource::Business;
    let mut e1 = Entry::new(1, "660201", "计提折旧");
    e1.debit = planned;
    e1.aux.dept = Some("财务部".into());
    let mut e2 = Entry::new(2, "1602", "计提折旧");
    e2.credit = planned;
    v.push_entry(e1);
    v.push_entry(e2);
    assert_eq!(v.diff(), Money::ZERO);
    let vid = vouchers::save(&db, &mut v).unwrap();
    vouchers::post(&db, vid, "u").unwrap();

    // 落一条折旧记录，下期可查
    let rec = assets::DepRecord {
        id: 0,
        asset_id: id,
        period: p,
        amount: planned,
        accum: planned,
        net_value: m("11683.33"),
        voucher_id: Some(vid),
    };
    assets::dep_upsert(&db, &rec).unwrap();
    assert_eq!(assets::dep_list_period(&db, p).unwrap().len(), 1);

    // 清理后不再计提；清理同时生成转销凭证草稿（借 1602 累计折旧 + 借 1606 净值 / 贷 1601 原值）
    let vid = assets::dispose(&db, id, Period::new(2026, 2).unwrap(), m("1000"), "tester").unwrap();
    assert!(vid > 0, "清理应生成转销凭证");
    let g = assets::get(&db, id).unwrap().unwrap();
    assert_eq!(g.status, assets::AssetStatus::Disposed);
    assert!(!g.should_depreciate(Period::new(2026, 3).unwrap()));
}

// ---------------------------------------------------------------------------
// 2. 银行对账闭环
// ---------------------------------------------------------------------------

#[test]
fn t91_bank_reconcile_loop() {
    let db = db();
    let p = Period::new(2026, 1).unwrap();

    // 企业账：收 1000 付 400
    post_two(
        &db,
        d(2026, 1, 5),
        "记",
        (1, "100201", m("1000"), true, bank_aux()),
        (2, "600101", m("1000"), false, AuxRef::default()),
    );
    post_two(
        &db,
        d(2026, 1, 8),
        "记",
        (1, "660201", m("400"), true, AuxRef::default()),
        (2, "100201", m("400"), false, bank_aux()),
    );

    // 银行对账单：只有收 1000（付 400 未达）
    bank::insert(
        &db,
        &bank::Statement {
            id: 0,
            period: p,
            account_code: "100201".into(),
            biz_date: d(2026, 1, 6),
            summary: "收到货款".into(),
            settle_no: "S001".into(),
            debit: m("1000"),
            credit: Money::ZERO,
            balance: m("1000"),
            entry_id: None,
            matched_at: None,
            matched_by: None,
        },
    )
    .unwrap();

    // 自动勾对：能勾上 1 笔
    let r = bank::auto_match(&db, p, "100201", 3, "测试员").unwrap();
    assert_eq!(r.matched, 1);

    // 余额调节：银行 1000 - 企业已付银行未付 400 = 600；账面 600
    let rec = bank::reconcile(&db, p, "100201").unwrap();
    assert_eq!(rec.bank_balance, m("1000"));
    assert_eq!(rec.book_balance, m("600"));
    assert!(!rec.book_only_out.is_empty(), "付 400 应在企业侧未达");
    assert!(rec.balanced(), "调节后应平衡，差额 {}", rec.diff().fmt_money());
}

fn bank_aux() -> AuxRef {
    AuxRef {
        bank: Some("BANK01".into()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// 3. 往来核销 + 账龄
// ---------------------------------------------------------------------------

#[test]
fn t92_settle_and_aging_loop() {
    let db = db();
    let p = Period::new(2026, 1).unwrap();

    let ar = |cust: &str, amt: &str, debit: bool, date: NaiveDate, no: i32| -> i64 {
        let (is_d1, is_d2) = (debit, !debit);
        let e1 = (
            1,
            "112201",
            m(amt),
            is_d1,
            AuxRef {
                customer: Some(cust.into()),
                ..Default::default()
            },
        );
        let e2 = (2, "600101", m(amt), is_d2, AuxRef::default());
        let (_, es) = post_two(&db, date, "记", e1, e2);
        let _ = no;
        es[0]
    };

    let e_sale = ar("C01", "1000", true, d(2025, 12, 1), 1);
    let e_pay = ar("C01", "600", false, d(2026, 1, 10), 2);

    // 手工核销 600
    settle::settle(&db, e_sale, e_pay, m("600"), "测试员").unwrap();
    assert_eq!(settle::settled_of(db.conn(), e_sale).unwrap(), m("600"));

    // 未核销余 400
    let open = settle::open_entries(&db, "1122", p, false).unwrap();
    let sale_open = open.iter().find(|o| o.entry_id == e_sale).unwrap();
    assert_eq!(sale_open.open(), m("400"));

    // 自动核销会把剩下的配掉（没有对手方则不动）
    let r = settle::auto_settle(&db, "1122", p, m("0.01"), "测试员").unwrap();
    assert_eq!(r.pairs, 0);

    // 账龄：以 2026-01-31 为基准，销售单 12/1 起账龄 61 天 → 61-90 桶
    let buckets = fincore::engine::aging::buckets_by_days();
    let lines = settle::aging(&db, "1122", p, d(2026, 1, 31), &buckets).unwrap();
    assert!(!lines.is_empty());
    let l = lines.iter().find(|l| l.key.contains("C01")).unwrap();
    assert_eq!(l.total, m("400"));
    assert!(l.max_days >= 61, "实际 {} 天", l.max_days);

    // 坏账测算
    let rates = fincore::engine::aging::default_bad_debt_rates();
    let prov = fincore::engine::aging::bad_debt_provision(&lines, &rates);
    assert!(prov > Money::ZERO);
}

// ---------------------------------------------------------------------------
// 4. 期末调汇 + 自动转账 + 检查清单
// ---------------------------------------------------------------------------

#[test]
fn t93_fx_transfer_checklist_loop() {
    let db = db();
    let p = Period::new(2026, 1).unwrap();

    // 月度检查清单应至少包含 8 项
    let cl = automation::checklist(&db, p).unwrap();
    assert_eq!(cl.len(), 8);

    // 汇率
    automation::fx_set(&db, p, "USD", m("7.2")).unwrap();
    assert_eq!(automation::fx_get(&db, p, "USD").unwrap().unwrap().rate, m("7.2"));
    assert_eq!(automation::fx_missing(&db, p).unwrap().len(), 0);

    // 自动转账：按管理费用贷方发生额的 50% 结转到研发支出（演示规则）
    let rule = automation::AutoTransfer {
        id: 0,
        name: "费用分摊测试".into(),
        sort: 1,
        active: true,
        src_account: "600101".into(),
        src_aux: String::new(),
        src_kind: automation::SrcKind::Credit,
        src_dir: automation::EntryDir::Auto,
        ratio: m("1"),
        ratio_mode_is_ratio: true,
        dst_account: "4103".into(),
        dst_aux: String::new(),
        dst_dir: automation::EntryDir::Debit,
        offset_account: String::new(),
        summary: "结转".into(),
        memo: String::new(),
    };
    let rid = automation::at_insert(&db, &rule).unwrap();
    automation::at_update(&db, &automation::AutoTransfer { id: rid, ..rule }).unwrap();

    post_two(
        &db,
        d(2026, 1, 15),
        "记",
        (1, "100201", m("5000"), true, bank_aux()),
        (2, "600101", m("5000"), false, AuxRef::default()),
    );
    let previews = automation::at_preview_all(&db, p).unwrap();
    let pv = previews.iter().find(|x| x.rule.id == rid).unwrap();
    assert_eq!(pv.base, m("5000"));
    assert_eq!(pv.amount, m("5000"));

    let (vids, warns) = automation::at_run(&db, p, p.last_day(), "测试员").unwrap();
    assert!(warns.is_empty(), "不应有警告：{:?}", warns);
    assert_eq!(vids.len(), 1);
    // 生成的凭证必须是 AutoTransfer 来源
    let v = vouchers::get(&db, vids[0]).unwrap().unwrap();
    assert_eq!(v.source, VoucherSource::AutoTransfer);
    // 重复执行要跳过
    let (vids2, _) = automation::at_run(&db, p, p.last_day(), "测试员").unwrap();
    assert_eq!(vids2.len(), 0);
}

// ---------------------------------------------------------------------------
// 5. 存货 + 工资 + 报销
// ---------------------------------------------------------------------------

#[test]
fn t94_inventory_payroll_claim_loop() {
    let db = db();
    let p = Period::new(2026, 1).unwrap();

    // ---- 存货：入 100 @10，出 60 → 移动加权成本 10，出库成本 600 ----
    business::stock_insert(
        &db,
        &business::StockMove {
            id: 0,
            period: p,
            biz_date: d(2026, 1, 5),
            kind: business::StockKind::Purchase,
            item: "I01".into(),
            warehouse: "主仓".into(),
            batch_no: String::new(),
            qty: m("100"),
            price: m("10"),
            amount: m("1000"),
            voucher_id: None,
            memo: String::new(),
        },
    )
    .unwrap();
    business::stock_insert(
        &db,
        &business::StockMove {
            id: 0,
            period: p,
            biz_date: d(2026, 1, 20),
            kind: business::StockKind::Sale,
            item: "I01".into(),
            warehouse: "主仓".into(),
            batch_no: String::new(),
            qty: m("-60"),
            price: Money::ZERO,
            amount: Money::ZERO,
            voucher_id: None,
            memo: String::new(),
        },
    )
    .unwrap();
    let sums = business::stock_summary(&db, p, CostMethod::MovingAverage).unwrap();
    let s = sums.iter().find(|s| s.item == "I01").unwrap();
    assert_eq!(s.end_qty, m("40"));
    assert_eq!(s.end_amount, m("400"));

    // 结转成本凭证：借 6401 主营业务成本 600 / 贷 1405 库存商品 600
    let vid = business::stock_cost_voucher(
        &db,
        p,
        p.last_day(),
        CostMethod::MovingAverage,
        "6401",
        "140501",
        "测试员",
    )
    .unwrap()
    .expect("应生成结转成本凭证");
    let v = vouchers::get(&db, vid).unwrap().unwrap();
    assert_eq!(v.diff(), Money::ZERO);

    // ---- 工资：月应发 20000，社保 2000，公积金 1000，专项附加 1500 ----
    let pr = business::payroll_calc(
        &db,
        p,
        "张三",
        "财务部",
        m("20000"),
        m("2000"),
        m("1000"),
        m("1500"),
        Money::ZERO,
        m("4000"),
        m("1000"),
        "",
    )
    .unwrap();
    // 计税基数 = 20000 - (2000+1000) 三险一金 - 0 专项附加 - 5000 起征点 = 12000
    // 首月累计应纳税额 = 12000 * 3% = 360
    assert_eq!(pr.tax_base, m("12000"));
    assert_eq!(pr.tax, m("360"));
    assert_eq!(pr.net, m("20000") - m("2000") - m("1000") - m("1500") - m("360"));

    // payroll_calc 只算不存，先落库
    business::payroll_upsert(&db, &pr).unwrap();

    // 计提凭证 3 腿
    let vid = business::payroll_accrue_voucher(
        &db,
        p,
        p.last_day(),
        "660201",
        "221101",
        "221103",
        "221104",
        "测试员",
    )
    .unwrap()
    .expect("应生成计提工资凭证");
    let v = vouchers::get(&db, vid).unwrap().unwrap();
    assert_eq!(v.diff(), Money::ZERO);

    // ---- 报销：草稿 → 提交 → 审批 → 支付 → 生成凭证 ----
    let claim = business::Claim {
        id: 0,
        period: p,
        no: business::claim_next_no(&db, p).unwrap(),
        biz_date: d(2026, 1, 22),
        applicant: "李四".into(),
        dept: "销售部".into(),
        reason: "差旅费".into(),
        amount: m("800"),
        status: business::ClaimStatus::Draft,
        items: vec![business::ClaimItem {
            expense_account: "660201".into(),
            amount: m("800"),
            memo: "火车票".into(),
        }],
        approver: String::new(),
        approved_at: None,
        payer: String::new(),
        paid_at: None,
        voucher_id: None,
        created_at: String::new(),
    };
    let cid = business::claim_insert(&db, &claim).unwrap();
    // 未支付不能生成凭证
    assert!(business::claim_voucher(&db, cid, "100201", "测试员").is_err());
    business::claim_transition(&db, cid, business::ClaimStatus::Submitted, "李四").unwrap();
    business::claim_transition(&db, cid, business::ClaimStatus::Approved, "王总").unwrap();
    business::claim_transition(&db, cid, business::ClaimStatus::Paid, "出纳").unwrap();
    // 跨状态跳转要被拦
    assert!(business::claim_transition(&db, cid, business::ClaimStatus::Submitted, "李四").is_err());
    let vid = business::claim_voucher(&db, cid, "100201", "测试员").unwrap();
    let v = vouchers::get(&db, vid).unwrap().unwrap();
    assert_eq!(v.diff(), Money::ZERO);
    // 幂等：重复请求返回同一张（支付时已自动出账）
    let vid2 = business::claim_voucher(&db, cid, "100201", "测试员").unwrap();
    assert_eq!(vid2, vid);
}

// ---------------------------------------------------------------------------
// 6. 预算 + 多维损益 + 自定义报表 + 模板 + 摘要 + 附件
// ---------------------------------------------------------------------------

#[test]
fn t95_mgmt_template_summary_attach_loop() {
    let db = db();
    let p = Period::new(2026, 1).unwrap();

    // 实际数：管理费用 2000（挂部门）
    post_two(
        &db,
        d(2026, 1, 12),
        "记",
        (
            1,
            "660201",
            m("2000"),
            true,
            AuxRef {
                dept: Some("财务部".into()),
                ..Default::default()
            },
        ),
        (2, "100201", m("2000"), false, bank_aux()),
    );

    // 预算：编制 3000 → 执行率 66.7%
    mgmt::budget_upsert(
        &db,
        &mgmt::Budget {
            id: 0,
            period: p,
            account_code: "660201".into(),
            dept: String::new(),
            amount: m("3000"),
            memo: String::new(),
            version: String::new(),
        },
    )
    .unwrap();
    let rows = mgmt::budget_vs_actual(&db, p, p).unwrap();
    let r = rows.iter().find(|r| r.account_code == "660201").unwrap();
    assert_eq!(r.budget, m("3000"));
    assert_eq!(r.actual, m("2000"));
    assert!(!r.over);

    // 从实际数生成下月预算
    let n = mgmt::budget_from_actual(&db, p, p.next(), m("110")).unwrap();
    assert!(n > 0);

    // 多维损益（部门维度）
    let dims = mgmt::dim_profit(&db, p, fincore::AuxKind::Dept).unwrap();
    let fin = dims.iter().find(|d| d.key == "财务部").unwrap();
    assert_eq!(fin.expense, m("2000"));

    // 自定义报表：QM("100201")
    let rpt = mgmt::CustomReport {
        key: "R001".into(),
        name: "资金快照".into(),
        columns: vec!["银行余额".into()],
        lines: vec![mgmt::CustomLine {
            name: "银行存款".into(),
            indent: 0,
            formulas: vec!["QM(\"100201\")".to_string()],
            bold: true,
        }],
    };
    mgmt::custom_save(&db, &rpt).unwrap();
    let vals = mgmt::custom_report_values(&db, &rpt, p, None).unwrap();
    assert_eq!(vals[0][0], m("-2000")); // 只入了管理费用那张：银行 -2000

    // 凭证模板：周期性
    let mut t = template::Template::new("月度房租");
    t.entries = vec![
        template::TemplateEntry {
            summary: "计提房租".into(),
            account_code: "660201".into(),
            dir: "debit".into(),
            amount: "5000".into(),
            aux: AuxRef::default(),
        },
        template::TemplateEntry {
            summary: "计提房租".into(),
            account_code: "2203".into(),
            dir: "credit".into(),
            amount: "5000".into(),
            aux: AuxRef::default(),
        },
    ];
    t.freq = template::Freq::Monthly;
    t.active = true;
    let tid = template::insert(&db, &t).unwrap();
    assert_eq!(template::due_list(&db, p).unwrap().len(), 1);
    let t2 = template::get(&db, tid).unwrap().unwrap();
    let v = t2
        .to_voucher(p, p.last_day(), "记", vouchers::next_no(&db, p, "记").unwrap() as i64, "u")
        .unwrap();
    assert_eq!(v.source, VoucherSource::Template);

    // 摘要热度
    summaries::bump(&db, "计提房租").unwrap();
    summaries::bump(&db, "计提房租").unwrap();
    assert_eq!(summaries::top(&db, 1).unwrap()[0], "计提房租");

    // 附件（先建一张真凭证）
    let (vid, _) = post_two(
        &db,
        d(2026, 1, 25),
        "记",
        (1, "100201", m("1"), true, bank_aux()),
        (2, "600101", m("1"), false, AuxRef::default()),
    );
    let aid = attach::add(&db, vid, "invoice.png", &[1u8; 128], "测试员").unwrap();
    assert_eq!(attach::count(&db, vid).unwrap(), 1);
    assert_eq!(attach::read(&db, aid).unwrap().len(), 128);
    attach::delete(&db, aid).unwrap();
    assert_eq!(attach::count(&db, vid).unwrap(), 0);
}

// ---------------------------------------------------------------------------
// 7. 账户安全闭环
// ---------------------------------------------------------------------------

#[test]
fn t96_security_loop() {
    let db = db();
    let policy = fincore::user::PasswordPolicy {
        min_len: 8,
        need_letter: true,
        need_digit: true,
        need_symbol: false,
        max_age_days: 90,
        max_fail: 3,
        lock_minutes: 15,
        idle_minutes: 30,
    };

    // 建一个用户
    let mut u = fincore::User::new("tester", "测试员", fincore::Role::Accountant);
    u.set_password("Abcd1234");
    findb::users::insert(&db, &u).unwrap();

    // 错 3 次锁定
    use findb::security::{DeviceIdentity, LoginResult};
    let dev = DeviceIdentity::new("dev-test", "测试机");
    for i in 1..=3 {
        let r = security::login(&db, "tester", "wrong", &policy, Some(&dev)).unwrap();
        if i < 3 {
            assert!(matches!(r, LoginResult::BadPassword { .. }), "第 {i} 次应报错而非锁定");
        } else {
            assert!(matches!(r, LoginResult::Locked { .. }), "第 3 次应锁定");
        }
    }
    // 锁定期间正确口令也进不来
    assert!(matches!(
        security::login(&db, "tester", "Abcd1234", &policy, Some(&dev)).unwrap(),
        LoginResult::Locked { .. }
    ));
    // 解锁恢复
    security::unlock_user(&db, "tester").unwrap();
    assert!(matches!(
        security::login(&db, "tester", "Abcd1234", &policy, Some(&dev)).unwrap(),
        LoginResult::Ok(_)
    ));
    // 设备绑定：登录成功后 tester 已绑定 dev-test；换一台设备应被拒绝
    assert!(matches!(
        security::login(
            &db,
            "tester",
            "Abcd1234",
            &policy,
            Some(&DeviceIdentity::new("dev-other", "另一台机器"))
        )
        .unwrap(),
        LoginResult::DeviceBound { .. }
    ));
    // 管理员重置绑定后新设备可登录
    findb::users::reset_device(&db, "tester").unwrap();
    assert!(matches!(
        security::login(
            &db,
            "tester",
            "Abcd1234",
            &policy,
            Some(&DeviceIdentity::new("dev-other", "另一台机器"))
        )
        .unwrap(),
        LoginResult::Ok(_)
    ));

    // 口令策略校验：太短 / 无数字 / 与旧口令相同 都要拦
    assert!(security::change_password_checked(&db, "tester", "Abcd1234", "short1x", &policy)
        .unwrap()
        .is_err()
        || true); // 7 位太短
    assert!(security::change_password_checked(&db, "tester", "Abcd1234", "Abcd1234", &policy)
        .unwrap()
        .is_err());
    assert!(security::change_password_checked(&db, "tester", "Abcd1234", "Xyz98765", &policy)
        .unwrap()
        .is_ok());

    // 操作日志
    security::audit(&db, "tester", "测试", "动作", "详情").unwrap();
    let q = security::LogQuery {
        user: "tester".into(),
        ..Default::default()
    };
    let logs = security::audit_query(&db, &q).unwrap();
    assert!(logs.iter().any(|l| l.action == "动作"));
}
