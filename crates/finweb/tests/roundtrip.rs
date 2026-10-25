//! 导出 → 改 → 导回 的闭环测试。
//!
//! 这些用例存在的理由：`sheets.rs` 的列头与 `imports.rs` 的解析器是**两处独立**
//! 的代码，靠「列下标」对齐。任何一边改动而另一边没跟上，产品都不会报错 ——
//! 用户拿到的是一份「能导出来、导回去少了几条」或「金额整体错位」的文件，
//! 而这种 bug 只有在对账时才会被发现。
//!
//! 所以这里不用「断言导出的字符串长什么样」（那种测试只证明代码没变），
//! 而是真的把导出的字节喂回导入器，断言**数据回到库里且数值一致**。
//! 三个基础资料表（档案 / 科目 / 期初）各一条。

use std::collections::HashMap;

use fincore::account::{AcctCategory, Direction};
use fincore::{AuxKind, BookOptions, Money, Period};
use findb::balances::{list_begin, BeginRow};
use findb::imports::ImportTemplate;

fn opts() -> BookOptions {
    BookOptions {
        start_period: Period::new(2026, 1).unwrap(),
        company: "闭环测试".into(),
        ..Default::default()
    }
}

fn db_at(dir: &std::path::Path, name: &str) -> findb::Db {
    let db = findb::Db::create_no_admin(dir.join(name), &opts()).expect("建账套失败");
    db
}

/// 客户 + 供应商 + 现金流量项目三张档案：导出成 xlsx，导进另一个空账套，
/// 断言类型/编码/名称/备注全部还原。
#[test]
fn 辅助档案导出后能导回另一个账套() {
    let dir = tempfile::tempdir().unwrap();
    let src = db_at(dir.path(), "a.fbk");
    let mut c = fincore::AuxEntity::new(AuxKind::Customer, "C001", "客户甲");
    c.memo = "月结 30 天".into();
    findb::auxs::insert(&src, &c).unwrap();
    findb::auxs::insert(
        &src,
        &fincore::AuxEntity::new(AuxKind::Supplier, "S001", "供应商甲"),
    )
    .unwrap();
    findb::auxs::insert(
        &src,
        &fincore::AuxEntity::new(AuxKind::CashFlow, "CF01", "销售商品收到的现金"),
    )
    .unwrap();

    let rows = findb::auxs::list(
        &src,
        &fincore::AuxQuery {
            include_disabled: true,
            ..Default::default()
        },
    )
    .unwrap();
    let sheet = finweb::sheets::aux_sheet(&rows, "全部");
    let bytes = finweb::xlsxout::build(&[sheet]).unwrap();

    let dst = db_at(dir.path(), "b.fbk");
    let res = findb::imports::import_aux_bytes(&dst, &bytes, "tester").unwrap();
    assert_eq!(
        (res.ok, res.skipped),
        (3, 0),
        "三条档案应全部导入：{:?}",
        res.warnings
    );

    let got = findb::auxs::get(&dst, AuxKind::Customer, "C001")
        .unwrap()
        .expect("客户没导进去");
    assert_eq!(got.name, "客户甲");
    assert_eq!(got.memo, "月结 30 天");
    assert!(findb::auxs::get(&dst, AuxKind::Supplier, "S001")
        .unwrap()
        .is_some());
    assert!(
        findb::auxs::get(&dst, AuxKind::CashFlow, "CF01")
            .unwrap()
            .is_some(),
        "现金流量项目也要能搬过去（原来 aux_kind_of 不认这个类型）"
    );
}

/// 停用状态与上级编码也要能搬过去。
///
/// 这两列是 v1.8.21 才补进导出表的。原来导出只有「类型,编码,名称,备注」四列，
/// 于是搬完账套之后：停用的档案全变回启用（凭证选客商时又冒出来），
/// 层级的部门全变成一级（树状结构塌平）。数据没丢，但**结构丢了**，
/// 而界面上看不出任何异常 —— 比丢几条数据更难发现。
#[test]
fn 停用与上级编码导出后能导回() {
    let dir = tempfile::tempdir().unwrap();
    let src = db_at(dir.path(), "a.fbk");
    findb::auxs::insert(
        &src,
        &fincore::AuxEntity::new(AuxKind::Dept, "D01", "生产部"),
    )
    .unwrap();
    let mut sub = fincore::AuxEntity::new(AuxKind::Dept, "D0101", "氧化车间");
    sub.parent_code = Some("D01".into());
    findb::auxs::insert(&src, &sub).unwrap();
    let mut old = fincore::AuxEntity::new(AuxKind::Customer, "C099", "已停用的老客户");
    old.disabled = true;
    findb::auxs::insert(&src, &old).unwrap();

    let rows = findb::auxs::list(
        &src,
        &fincore::AuxQuery {
            include_disabled: true,
            ..Default::default()
        },
    )
    .unwrap();
    let bytes =
        finweb::xlsxout::build(&[finweb::sheets::aux_sheet(&rows, "全部")]).unwrap();

    let dst = db_at(dir.path(), "b.fbk");
    let res = findb::imports::import_aux_bytes(&dst, &bytes, "tester").unwrap();
    assert_eq!(res.ok, 3, "三条档案应全部导入：{:?}", res.warnings);

    let got_sub = findb::auxs::get(&dst, AuxKind::Dept, "D0101")
        .unwrap()
        .expect("子部门没导进去");
    assert_eq!(
        got_sub.parent_code.as_deref(),
        Some("D01"),
        "上级编码丢了 —— 层级树会塌平"
    );
    let got_old = findb::auxs::get(&dst, AuxKind::Customer, "C099")
        .unwrap()
        .expect("停用客户没导进去（停用的也要导出，否则历史凭证的辅助核算断了）");
    assert!(got_old.disabled, "停用状态丢了 —— 停掉的客户又冒出来了");
}

/// 科目：类别与方向必须原样回来。方向错了报表会整体反号，
/// 而余额表看着仍然是平的 —— 最容易蒙混过关的一种错。
///
/// 注意「已存在就跳过」是**有意**的幂等设计：新账套自带 199 个预置科目，
/// 所以把整张科目表导进另一个新账套时，预置的那些会全部 skipped、只有自建的
/// 科目真正落库。这条用例正因此才要造一个自己加的科目 —— 否则断言的
/// 「条数对上了」只能证明两边预置表一样，证明不了搬运能力。
#[test]
fn 科目导出后类别与方向能导回() {
    let dir = tempfile::tempdir().unwrap();
    let src = db_at(dir.path(), "a.fbk");
    // 造两个预置里没有的科目：一个费用类借方、一个负债类却配借方方向
    // （「共同」类科目就是这种两边都可能的方向，导回时最容易被类别默认值盖掉）
    let mut expense = fincore::account::Account::new("660901", "办公费", AcctCategory::Expense);
    expense.memo = "行政办公".into();
    findb::accounts::insert(&src, &expense).unwrap();
    let mut odd = fincore::account::Account::new("219901", "待确认往来", AcctCategory::Liability);
    odd.dir = Direction::Debit;
    findb::accounts::insert(&src, &odd).unwrap();

    let chart = findb::accounts::chart(&src).unwrap();
    let all: Vec<&fincore::account::Account> = chart.all();
    let sheet = finweb::sheets::accounts_sheet(&all);
    let bytes = finweb::xlsxout::build(&[sheet]).unwrap();

    let dst = db_at(dir.path(), "b.fbk");
    let res = findb::imports::import_accounts_bytes(&dst, &bytes, "tester").unwrap();
    assert_eq!(res.ok, 2, "两个自建科目应导入：{:?}", res.warnings);
    assert!(res.skipped >= 199, "预置科目应被幂等跳过");

    let dst_chart = findb::accounts::chart(&dst).unwrap();
    let e = dst_chart.get("660901").expect("自建费用科目没导进去");
    assert_eq!(e.name, "办公费");
    assert_eq!(e.category, AcctCategory::Expense);
    assert_eq!(e.dir, Direction::Debit);
    assert_eq!(e.memo, "行政办公");

    let o = dst_chart.get("219901").expect("自建负债科目没导进去");
    assert_eq!(o.category, AcctCategory::Liability);
    assert_eq!(
        o.dir,
        Direction::Debit,
        "方向被类别默认值盖掉了 —— 导出必须写方向列，导回必须认它"
    );

    // 预置科目不能被这次导入搞乱
    let ap = dst_chart.get("220201").expect("应付账款明细");
    assert_eq!(ap.category, AcctCategory::Liability);
    assert_eq!(ap.dir, Direction::Credit);
}

/// 期初：借贷方向与金额必须一一还原（贷方在库里是负数，
/// 导出写成「贷 3000」，导回要回到 -3000）。
#[test]
fn 期初余额导出后能导回且方向不变() {
    let dir = tempfile::tempdir().unwrap();
    let src = db_at(dir.path(), "a.fbk");
    let src_chart = findb::accounts::chart(&src).unwrap();
    let rows = vec![
        BeginRow {
            id: 0,
            account_code: "100201".into(),
            aux: Default::default(),
            year_begin: Money::parse("12000").unwrap(),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        },
        // 贷方一笔：与上一笔合计必须平（12,000 借 = 3,000 + 9,000 贷），
        // 否则最后那条「导回后仍要试算平衡」断言会假失败
        BeginRow {
            id: 0,
            account_code: "220201".into(),
            aux: Default::default(),
            year_begin: Money::parse("-3000").unwrap(),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        },
        BeginRow {
            id: 0,
            account_code: "4001".into(),
            aux: Default::default(),
            year_begin: Money::parse("-9000").unwrap(),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        },
    ];
    for r in &rows {
        findb::balances::upsert_begin(&src, r).unwrap();
    }
    let saved = list_begin(&src).unwrap();
    let sheet =
        finweb::sheets::begin_sheet(&saved, &|c| src_chart.get(c).map(|a| a.name.clone()).unwrap_or_default());
    let bytes = finweb::xlsxout::build(&[sheet]).unwrap();

    let dst = db_at(dir.path(), "b.fbk");
    let res = findb::imports::import_begin_bytes(
        &dst,
        &bytes,
        "tester",
        &HashMap::new(),
        ImportTemplate::Generic,
    )
    .unwrap();
    assert_eq!((res.ok, res.skipped), (3, 0), "{:?}", res.warnings);

    let back = list_begin(&dst).unwrap();
    let bank = back
        .iter()
        .find(|r| r.account_code == "100201")
        .expect("银行存款期初没导回");
    let ap = back
        .iter()
        .find(|r| r.account_code == "220201")
        .expect("应付账款期初没导回");
    assert_eq!(bank.year_begin, Money::parse("12000").unwrap());
    assert_eq!(
        ap.year_begin,
        Money::parse("-3000").unwrap(),
        "贷方期初必须回来成负数，导成借方就会凭空多出 6000 资产"
    );
    // 导回后仍要试算平衡
    let snap = findb::balances::BalanceSnapshot::load(
        &dst,
        &findb::balances::BalanceQuery::range(
            Period::new(2026, 1).unwrap(),
            Period::new(2026, 1).unwrap(),
        ),
    )
    .unwrap();
    let tb = snap.trial_balance(&findb::accounts::chart(&dst).unwrap());
    assert!(tb.begin_balanced(), "导回后期初借贷不平：{tb:?}");
}
