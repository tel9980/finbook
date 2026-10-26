//! 期初收集表端到端：造一本《期初收集表》，导入，然后核对台账与科目期初。

use findb::Db;
use fincore::account::BookOptions;
use fincore::money::Money;
use fincore::{AuxEntity, AuxKind};
use rust_xlsxwriter::{Format, Workbook};

fn m(s: &str) -> Money {
    Money::parse(s).expect("金额")
}

/// 造一本和 `examples/opening_sheet.rs` 同构的收集表（四张纸）。
fn make_sheet(ar: &[(&str, &str, f64)], ap: &[(&str, &str, f64)], bal: &[(&str, f64)], eq: &[(&str, f64)]) -> Vec<u8> {
    let mut wb = Workbook::new();
    let f = Format::new();

    // ① 客户应收：标题行 + 表头行 + 数据
    let s = wb.add_worksheet();
    s.set_name("① 客户应收").unwrap();
    s.write_string(0, 0, "客户欠款（应收）· 截止 2026-10-31", &f).unwrap();
    for (c, h) in ["客户编码", "客户名称", "欠款金额", "单据号（可空）", "备注"]
        .iter()
        .enumerate()
    {
        s.write_string(1, c as u16, *h, &f).unwrap();
    }
    for (i, (code, name, amt)) in ar.iter().enumerate() {
        let r = 2 + i as u32;
        s.write_string(r, 0, *code, &f).unwrap();
        s.write_string(r, 1, *name, &f).unwrap();
        s.write_number(r, 2, *amt, &f).unwrap();
    }
    let last = 2 + ar.len() as u32;
    s.write_string(last + 1, 1, "应收合计", &f).unwrap();

    // ② 供应商应付
    let s = wb.add_worksheet();
    s.set_name("② 供应商应付").unwrap();
    s.write_string(0, 0, "欠供应商的钱（应付）· 截止 2026-10-31", &f).unwrap();
    for (c, h) in ["供应商编码", "供应商名称", "品类", "欠款金额", "单据号（可空）", "备注"]
        .iter()
        .enumerate()
    {
        s.write_string(1, c as u16, *h, &f).unwrap();
    }
    for (i, (code, name, amt)) in ap.iter().enumerate() {
        let r = 2 + i as u32;
        s.write_string(r, 0, *code, &f).unwrap();
        s.write_string(r, 1, *name, &f).unwrap();
        s.write_string(r, 2, "化工料", &f).unwrap();
        s.write_number(r, 3, *amt, &f).unwrap();
    }
    let last = 2 + ap.len() as u32;
    s.write_string(last + 1, 2, "应付合计", &f).unwrap();

    // ③ 账户余额
    let s = wb.add_worksheet();
    s.set_name("③ 账户余额").unwrap();
    s.write_string(0, 0, "各账户余额 · 截止 2026-10-31", &f).unwrap();
    for (c, h) in ["科目编码", "账户", "余额", "说明"].iter().enumerate() {
        s.write_string(1, c as u16, *h, &f).unwrap();
    }
    for (i, (code, amt)) in bal.iter().enumerate() {
        let r = 2 + i as u32;
        s.write_string(r, 0, *code, &f).unwrap();
        s.write_string(r, 1, "账户", &f).unwrap();
        s.write_number(r, 2, *amt, &f).unwrap();
    }

    // ④ 基本资料
    let s = wb.add_worksheet();
    s.set_name("④ 基本资料").unwrap();
    s.write_string(0, 0, "基本资料与平衡检查", &f).unwrap();
    for (c, h) in ["项目", "金额", "说明"].iter().enumerate() {
        s.write_string(1, c as u16, *h, &f).unwrap();
    }
    for (i, (k, amt)) in eq.iter().enumerate() {
        let r = 2 + i as u32;
        s.write_string(r, 0, *k, &f).unwrap();
        s.write_number(r, 1, *amt, &f).unwrap();
    }

    wb.save_to_buffer().unwrap()
}

/// 每出一条用例一个独立目录（`tempfile` 不是本仓库的依赖，就用 env::temp_dir 自己拼）。
fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("finbook_ob_{}_{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建临时目录");
    d
}

fn book(dir: &std::path::Path, start: fincore::Period) -> Db {
    let mut o = BookOptions::default();
    o.company = "氧化厂".into();
    o.start_period = start;
    o.enable_audit = false;
    o.require_audit = false;
    o.require_cashier = false;
    let db = Db::create_no_admin(dir.join("t.fbk"), &o).expect("建账");
    for (kind, code, name) in [
        (AuxKind::Customer, "C001", "华达"),
        (AuxKind::Customer, "C002", "鑫盛"),
        (AuxKind::Supplier, "S001", "宏源"),
        (AuxKind::Supplier, "S002", "顺发"),
    ] {
        findb::auxs::insert(&db, &AuxEntity::new(kind, code, name)).expect("档案");
    }
    db
}

#[test]
fn collection_sheet_lands_in_ledger_and_opening_balances() {
    let dir = scratch("ok");
    let start = fincore::Period::new(2026, 11).unwrap();
    let db = book(&dir, start);

    let bytes = make_sheet(
        &[("C001", "华达", 50000.0), ("C002", "鑫盛", 20000.0)],
        &[("S001", "宏源", 30000.0)],
        &[("100201", 20000.0)],
        &[("实收资本 4001", 50000.0)],
    );
    let res = findb::opening_book::import_collection_sheet(&db, &bytes, "admin").expect("导入");

    // 台账：两笔应收、一笔应付
    let ar = findb::settle::arap_opening_list(&db, Some("ar")).unwrap();
    assert_eq!(ar.len(), 2, "应收台账应有 2 笔：{ar:?}");
    assert_eq!(ar.iter().map(|o| o.amount).sum::<Money>(), m("70000"));
    let ap = findb::settle::arap_opening_list(&db, Some("ap")).unwrap();
    assert_eq!(ap.len(), 1);
    assert_eq!(ap[0].amount, m("30000"));
    // 单据号是自动补的（表里空着），日期是启用期间前一天
    assert!(ap[0].doc_no.starts_with("QC-"), "单据号应自动补：{}", ap[0].doc_no);
    assert_eq!(ap[0].doc_date, "2026-10-31", "期初日期应是 10-31");

    // 科目期初：应收/应付合计 + 银行 + 实收资本 + 轧差的本年利润
    let begin = findb::balances::list_begin(&db).unwrap();
    let get = |code: &str| {
        begin
            .iter()
            .find(|r| r.account_code == code)
            .map(|r| r.year_begin)
            .unwrap_or(Money::ZERO)
    };
    assert_eq!(get("112201"), m("70000"), "应收科目期初应等于台账合计");
    assert_eq!(get("220201"), m("-30000"), "应付记贷方（负）");
    assert_eq!(get("100201"), m("20000"));
    assert_eq!(get("4001"), m("-50000"));
    // 资产 90000 − 负债 30000 − 实收 50000 = 10000 的窟窿，由轧差补进 4103
    assert_eq!(get("4103"), m("-10000"), "差额应补到本年利润");
    assert_eq!(res.auto_profit, m("10000"));

    // 试算平衡必须开账就是平的
    let chart = findb::accounts::chart(&db).unwrap();
    let snap = findb::balances::BalanceSnapshot::load(&db, &findb::balances::BalanceQuery::period(start)).unwrap();
    let tb = snap.trial_balance(&chart);
    assert!(tb.begin_balanced(), "期初不平：{tb:?}");
}

#[test]
fn unbalanced_sheet_is_rejected_and_nothing_is_written() {
    let dir = scratch("bad");
    let start = fincore::Period::new(2026, 11).unwrap();
    let db = book(&dir, start);

    // 应收 70000、银行 20000、应付 30000、实收资本 50000，且**明确填了**本年利润 1000
    // → 借 90000 对 贷 81000，差 9000，应该整批拒绝
    let bytes = make_sheet(
        &[("C001", "华达", 50000.0), ("C002", "鑫盛", 20000.0)],
        &[("S001", "宏源", 30000.0)],
        &[("100201", 20000.0)],
        &[("实收资本 4001", 50000.0), ("其中：本年利润 4103", 1000.0)],
    );
    let err = findb::opening_book::import_collection_sheet(&db, &bytes, "admin")
        .expect_err("不平必须报错");
    let msg = format!("{err}");
    assert!(msg.contains("不平"), "错误信息要讲清是不平：{msg}");
    assert!(msg.contains("9,000.00"), "要把差额带出来（带千分位）：{msg}");

    // 一笔都没落库
    assert!(
        findb::settle::arap_opening_list(&db, None).unwrap().is_empty(),
        "拒绝时不能写进半个账套"
    );
    assert!(
        findb::balances::list_begin(&db).unwrap().is_empty(),
        "拒绝时期初必须干净"
    );
}

#[test]
fn non_collection_workbook_is_rejected_with_a_readable_reason() {
    let dir = scratch("notform");
    let start = fincore::Period::new(2026, 11).unwrap();
    let db = book(&dir, start);

    // 只有一张纸的普通表格
    let mut wb = Workbook::new();
    let f = Format::new();
    let s = wb.add_worksheet();
    s.set_name("Sheet1").unwrap();
    s.write_string(0, 0, "随便什么", &f).unwrap();
    let bytes = wb.save_to_buffer().unwrap();

    let err = findb::opening_book::import_collection_sheet(&db, &bytes, "admin")
        .expect_err("不是收集表要拒绝");
    assert!(
        format!("{err}").contains("期初收集表"),
        "错误里要提到期初收集表：{err}"
    );
}
