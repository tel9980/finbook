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
    let bytes = finweb::xlsxout::build(&[finweb::sheets::aux_sheet(&rows, "全部")]).unwrap();

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
    let sheet = finweb::sheets::begin_sheet(&saved, &|c| {
        src_chart.get(c).map(|a| a.name.clone()).unwrap_or_default()
    });
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

// ---------------------------------------------------------------------------
// v1.8.22：把「页面上能编辑、导出表里却没有」的字段接上
// ---------------------------------------------------------------------------

/// 客户的「税点%」「开票情况」必须跟着表走。
///
/// 这一条背后是真的要命：小厂 59 个客户，每个客户税点都不一样（13% 专票 / 3%
/// 代开 / 不要票）。原来这两个字段只在桌面端点开档案一个个改，导出表里没有它们，
/// 网页端也改不了 —— 想把税点批量核对一遍，只能点开 59 次。
#[test]
fn 客户税点与开票情况导出后能导回() {
    use fincore::auxiliary::prop;
    let dir = tempfile::tempdir().unwrap();
    let src = db_at(dir.path(), "a.fbk");
    let mut c = fincore::AuxEntity::new(AuxKind::Customer, "C001", "华达五金");
    c.set_prop(prop::TAX_RATE, String::from("13"));
    c.set_prop(prop::TAX_RATE_POLICY, String::from("invoice"));
    c.set_prop(prop::CREDIT_LIMIT, String::from("50000"));
    findb::auxs::insert(&src, &c).unwrap();
    let mut s = fincore::AuxEntity::new(AuxKind::Supplier, "S001", "宏源氧化");
    s.set_prop(prop::TAX_RATE, String::from("3"));
    s.set_prop(prop::TAX_RATE_POLICY, String::from("none"));
    findb::auxs::insert(&src, &s).unwrap();

    let rows = findb::auxs::list(&src, &fincore::AuxQuery::default()).unwrap();
    let bytes = finweb::xlsxout::build(&[finweb::sheets::aux_sheet(&rows, "全部")]).unwrap();

    let dst = db_at(dir.path(), "b.fbk");
    let res = findb::imports::import_aux_bytes(&dst, &bytes, "tester").unwrap();
    assert_eq!((res.ok, res.skipped), (2, 0), "{:?}", res.warnings);

    let got = findb::auxs::get(&dst, AuxKind::Customer, "C001")
        .unwrap()
        .expect("客户没导进去");
    assert_eq!(got.prop_or(prop::TAX_RATE, "-"), "13");
    assert_eq!(
        got.prop_or(prop::TAX_RATE_POLICY, "-"),
        "invoice",
        "开票情况要按中文标签转回存储码"
    );
    assert_eq!(got.prop_or(prop::CREDIT_LIMIT, "-"), "50000");

    let sup = findb::auxs::get(&dst, AuxKind::Supplier, "S001")
        .unwrap()
        .expect("供应商没导进去");
    assert_eq!(sup.prop_or(prop::TAX_RATE, "-"), "3");
    assert_eq!(sup.prop_or(prop::TAX_RATE_POLICY, "-"), "none");
}

/// 存货的「计量单位」「备注」「停用」和「安全库存」要能一起搬过去。
///
/// 网页存货页面一直在显示这几列，导出表里却只有四列：改完导回去，单位没了，
/// 停用的存货又变回启用（录凭证时又能选到）。
#[test]
fn 存货单位与停用状态导出后能导回() {
    use fincore::auxiliary::prop;
    let dir = tempfile::tempdir().unwrap();
    let src = db_at(dir.path(), "a.fbk");
    let mut it = fincore::AuxEntity::new(AuxKind::Item, "HG001", "合页坯");
    it.set_prop(prop::UNIT, String::from("吨"));
    it.memo = "外发前存放".into();
    it.props.insert("shelf_life_days".into(), "365".into());
    findb::auxs::insert(&src, &it).unwrap();
    let mut off = fincore::AuxEntity::new(AuxKind::Item, "OLD01", "淘汰料");
    off.disabled = true;
    findb::auxs::insert(&src, &off).unwrap();
    findb::advanced::item_plan_upsert(
        &src,
        &findb::advanced::ItemPlan {
            item_code: "HG001".into(),
            safety_stock: Money::parse("120").unwrap(),
            lead_days: 0,
            lot_size: Money::ZERO,
        },
    )
    .unwrap();

    let rows = findb::auxs::list(
        &src,
        &fincore::AuxQuery {
            kind: Some(AuxKind::Item),
            include_disabled: true,
            ..Default::default()
        },
    )
    .unwrap();
    let plans: HashMap<String, Money> = findb::advanced::item_plan_all(&src)
        .unwrap()
        .into_iter()
        .map(|p| (p.item_code, p.safety_stock))
        .collect();
    let sheet = finweb::sheets::items_sheet(&rows, &|c| plans.get(c).copied());
    let bytes = finweb::xlsxout::build(&[sheet]).unwrap();

    let dst = db_at(dir.path(), "b.fbk");
    let res = findb::imports::import_items_bytes(&dst, &bytes, "tester").unwrap();
    assert_eq!((res.ok, res.skipped), (2, 0), "{:?}", res.warnings);

    let got = findb::auxs::get(&dst, AuxKind::Item, "HG001")
        .unwrap()
        .expect("存货没导进去");
    assert_eq!(got.prop_or(prop::UNIT, "-"), "吨", "计量单位丢了");
    assert_eq!(got.memo, "外发前存放");
    assert_eq!(
        got.props.get("shelf_life_days").map(|s| s.as_str()),
        Some("365")
    );
    let plan = findb::advanced::item_plan_get(&dst, "HG001")
        .unwrap()
        .expect("安全库存没导进去");
    assert_eq!(plan.safety_stock, Money::parse("120").unwrap());

    let off = findb::auxs::get(&dst, AuxKind::Item, "OLD01")
        .unwrap()
        .expect("停用的存货也要搬过去");
    assert!(off.disabled, "停用状态丢了 —— 淘汰料又能被选进凭证");
}

/// 科目的「辅助核算维度」「数量单位」「现金/银行标志」「停用」都要能搬。
///
/// 这几项原来导出表里写着一句「只读提示，导回时不生效」。搬账套后科目是齐的，
/// 但余额表按客户拆不开（科目上没挂客户维度），数量金额式没数量（没设数量核算），
/// 报表取现金流取不到（现金科目标志丢了）。
#[test]
fn 科目辅助核算与数量单位导出后能导回() {
    use fincore::account::{AuxKind as _AuxKindUnused, AuxMask};
    let _ = std::marker::PhantomData::<_AuxKindUnused>;
    let dir = tempfile::tempdir().unwrap();
    let src = db_at(dir.path(), "a.fbk");

    let mut ar = fincore::account::Account::new("112299", "应收账款-样品", AcctCategory::Asset);
    ar.aux = AuxMask::NONE.with(AuxKind::Customer);
    findb::accounts::insert(&src, &ar).unwrap();

    let mut stock = fincore::account::Account::new("140599", "原材料-辅料", AcctCategory::Asset);
    stock.aux = AuxMask::NONE.with(AuxKind::Item);
    stock.unit = Some("吨".into());
    stock.has_qty = true;
    findb::accounts::insert(&src, &stock).unwrap();

    let mut gone = fincore::account::Account::new("660999", "停用费用", AcctCategory::Expense);
    gone.disabled = true;
    findb::accounts::insert(&src, &gone).unwrap();

    let chart = findb::accounts::chart(&src).unwrap();
    let all: Vec<&fincore::account::Account> = chart.all();
    let bytes = finweb::xlsxout::build(&[finweb::sheets::accounts_sheet(&all)]).unwrap();

    let dst = db_at(dir.path(), "b.fbk");
    let res = findb::imports::import_accounts_bytes(&dst, &bytes, "tester").unwrap();
    assert_eq!(res.ok, 3, "三个自建科目应导入：{:?}", res.warnings);

    let dc = findb::accounts::chart(&dst).unwrap();
    let a = dc.get("112299").expect("应收样品科目没导进去");
    assert!(
        a.aux.contains(AuxKind::Customer),
        "辅助核算维度丢了 —— 余额表按客户拆不开"
    );
    let s = dc.get("140599").expect("原材料辅料科目没导进去");
    assert!(s.aux.contains(AuxKind::Item));
    assert_eq!(s.unit.as_deref(), Some("吨"), "数量单位丢了");
    assert!(s.has_qty, "数量核算标志丢了");
    let g = dc.get("660999").expect("停用科目没导进去");
    assert!(g.disabled, "停用状态丢了 —— 新账里又多出一个能选的费用科目");
}

/// 期初的「数量」和「分户」（客户/存货）要能搬过去。
///
/// 小厂真实场景：开工第一天，「华达还欠我 5 万」记在 112201 上、客户靠辅助核算
/// 分。原来金额搬得过去、分户搬不过去 —— 期初总额对得上，按客户一查是空的。
#[test]
fn 期初数量与分户导出后能导回() {
    let dir = tempfile::tempdir().unwrap();
    let src = db_at(dir.path(), "a.fbk");
    let oa = fincore::AuxRef {
        customer: Some("C001".into()),
        ..Default::default()
    };
    let oi = fincore::AuxRef {
        item: Some("HG001".into()),
        ..Default::default()
    };
    let rows = vec![
        BeginRow {
            id: 0,
            account_code: "112201".into(),
            aux: oa,
            year_begin: Money::parse("50000").unwrap(),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        },
        BeginRow {
            id: 0,
            account_code: "140501".into(),
            aux: oi,
            year_begin: Money::parse("10000").unwrap(),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: Some(Money::parse("5").unwrap()),
        },
        BeginRow {
            id: 0,
            account_code: "4001".into(),
            aux: Default::default(),
            year_begin: Money::parse("-60000").unwrap(),
            debit_accum: Money::ZERO,
            credit_accum: Money::ZERO,
            qty_begin: None,
        },
    ];
    for r in &rows {
        findb::balances::upsert_begin(&src, r).unwrap();
    }
    let chart = findb::accounts::chart(&src).unwrap();
    let saved = list_begin(&src).unwrap();
    let bytes = finweb::xlsxout::build(&[finweb::sheets::begin_sheet(&saved, &|c| {
        chart.get(c).map(|a| a.name.clone()).unwrap_or_default()
    })])
    .unwrap();

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
    let ar = back
        .iter()
        .find(|r| r.account_code == "112201")
        .expect("应收账款期初没导回");
    assert_eq!(ar.year_begin, Money::parse("50000").unwrap());
    assert_eq!(
        ar.aux.customer.as_deref(),
        Some("C001"),
        "分户丢了 —— 期初总额对得上，按客户查是空的"
    );
    let raw = back
        .iter()
        .find(|r| r.account_code == "140501")
        .expect("原材料期初没导回");
    assert_eq!(
        raw.qty_begin,
        Some(Money::parse("5").unwrap()),
        "数量期初丢了 —— 数量金额式明细里数量是 0"
    );
    assert_eq!(raw.aux.item.as_deref(), Some("HG001"));
}
