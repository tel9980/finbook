//! 氧化加工厂「一般纳税人」演示账套的验收测试：生成器产出的账必须满足这些断言。
//!
//! 覆盖用户要的效果：48 张全已记账、试算平衡、一般纳税人进销项抵扣算对
//! （销项 58500 − 进项 9740.34 = 应交 48759.66，税负 10.84%）、
//! 往来按单位轧差（宏远 126000 / 精密 34500 / 灯具 0）、70 人工资五步走完、
//! 车间料期末库存为 0、本月利润 17119.18、资产负债表左右相等。
//! 改坏生成器时这里先挂。

#[path = "../examples/oxide_vat.rs"]
mod oxide_vat;

use fincore::{Direction, Money};
use std::path::PathBuf;

fn m(s: &str) -> Money {
    Money::parse_or_zero(s)
}

/// 每个测试各用各的文件名：同一进程里测试是并行跑的，
/// 共用一个 .fbk 会互相删库（UNIQUE constraint failed: account.code 就是这么来的）。
fn build_book_named(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("{}_{}.fbk", name, std::process::id()));
    let _ = std::fs::remove_file(&path);
    oxide_vat::build(&path);
    path
}

#[test]
fn 一般纳税人演示账全已记账且试算平衡() {
    let path = build_book_named("oxide_vat_books");
    let db = findb::Db::open(&path).expect("打开演示账");

    // 1) 48 张（44 张业务 + 结转损益 + 代收代付过渡 + 抹零差异 + 往来对冲），一张不能是草稿
    let list =
        findb::vouchers::list(&db, &findb::vouchers::VoucherQuery::default()).expect("查凭证");
    assert_eq!(list.len(), 48, "凭证数不对");
    assert!(
        list.iter()
            .all(|v| v.status == fincore::VoucherStatus::Posted),
        "必须全部已记账"
    );
    // 结转损益那张必须标 CarryForward（利润表靠它剔除，否则利润算两遍）
    assert_eq!(
        list.iter()
            .filter(|v| v.source == fincore::VoucherSource::CarryForward)
            .count(),
        1,
        "结转损益凭证应恰好一张且标 CarryForward"
    );
    // 三张系统自动生成的凭证必须在：代收代付过渡 / 抹零差异 / 往来对冲
    for memo in ["往来互抵", "核销差异", "三方抵账"] {
        assert!(
            list.iter().any(|v| v.memo.contains(memo)),
            "缺系统自动生成的凭证：{memo}"
        );
    }

    // 2) 试算平衡（期初/本期/期末三组全平）
    let chart = findb::accounts::chart(&db).expect("读科目表");
    let p = fincore::Period::new(2026, 11).unwrap();
    let snap =
        findb::balances::BalanceSnapshot::load(&db, &findb::balances::BalanceQuery::range(p, p))
            .expect("取余额快照");
    let tb = snap.trial_balance(&chart);
    assert!(tb.begin_balanced(), "期初不平");
    assert!(tb.period_balanced(), "本期发生不平");
    assert!(tb.end_balanced(), "期末不平");

    // 3) 所有科目符号余额之和为 0（资产负债表左右相等的另一面）
    let total: Money = snap.raw_rows().iter().map(|r| r.end()).sum();
    assert!(total.is_zero(), "左右不相等：{total:?}");

    // 4) 关键余额（方向 + 金额逐个锁）
    let end_of = |code: &str| snap.for_account(code, None).end_dir_amount();
    // 钱分三处：现金（含微信）、G 银行对公户、N 银行结算户
    assert_eq!(end_of("1001"), (Direction::Debit, m("70620")));
    assert_eq!(end_of("100201"), (Direction::Debit, m("91160")));
    assert_eq!(end_of("100202"), (Direction::Debit, m("10200")));
    // 承兑收 115000 = 到期兑付 100000 + 背书 15000，票据户结平
    assert!(
        snap.for_account("112101", None).end().is_zero(),
        "应收票据应结平"
    );
    // 应收账款 = 宏远 126000 + 精密 34500 + 灯具 0
    assert_eq!(end_of("112201"), (Direction::Debit, m("160500")));
    // 电卡：期初 4000 + 充 20000 − 抄表 18000 = 6000 还躺在卡里
    assert_eq!(end_of("112301"), (Direction::Debit, m("6000")));
    // 8 种化工料：期初 15320 + 购入 53730.99 − 领用 69050.99 = 0（全部投产）
    assert!(
        snap.for_account("140302", None).end().is_zero(),
        "辅助材料应全部投产、库存为 0"
    );
    assert_eq!(end_of("1602"), (Direction::Credit, m("46000")));
    // 应付 = 恒信 0 + 大发 6180 + 亮洁 9950
    assert_eq!(end_of("220201"), (Direction::Credit, m("16130")));
    // 工资与社保五步走完，两个过渡科目归零，只剩代扣个税没交
    assert!(
        snap.for_account("221101", None).end().is_zero(),
        "应付工资应结平"
    );
    assert!(
        snap.for_account("221103", None).end().is_zero(),
        "社保应结平"
    );
    assert_eq!(end_of("222107"), (Direction::Credit, m("1300")));
    // 增值税三段账：进项 9740.34 / 销项 58500 / 转出未交 48759.66 → 未交增值税 48759.66
    assert_eq!(end_of("22210101"), (Direction::Debit, m("9740.34")));
    assert_eq!(end_of("22210102"), (Direction::Credit, m("58500")));
    assert_eq!(end_of("22210104"), (Direction::Debit, m("48759.66")));
    assert_eq!(end_of("222102"), (Direction::Credit, m("48759.66")));
    // 附加税按应交增值税的 7% / 3% / 2% 计提
    assert_eq!(end_of("222103"), (Direction::Credit, m("3413.18")));
    assert_eq!(end_of("222104"), (Direction::Credit, m("1462.79")));
    assert_eq!(end_of("222105"), (Direction::Credit, m("975.19")));
    assert_eq!(end_of("4001"), (Direction::Credit, m("300000")));
    // 本年利润：1-10 月累计 23320 + 本月 17119.18 = 40439.18
    // 本月利润 = 450000 − 371099.66 − 55900 − 30 − 5851.16 = 17119.18
    assert_eq!(end_of("4103"), (Direction::Credit, m("40439.18")));

    // 5) 已结转归零的科目必须真是零
    for code in [
        "112101", "140302", "221101", "221103", "600101", "6401", "6403", "660201", "660202",
        "660204", "660207", "660303", "660305", "510101", "510102", "510103", "510105", "510106",
        "510107", "510108", "510109", "510110",
    ] {
        let e = snap.for_account(code, None).end();
        assert!(e.is_zero(), "{code} 应已结平，实际 {e:?}");
    }

    // 6) 往来按客户/供应商明细（A 付 B 的款、三方抵账、抹零之后余额精准轧差）
    let ar: std::collections::BTreeMap<String, Money> = snap
        .aux_breakdown("112201")
        .into_iter()
        .map(|r| (r.aux.customer.clone().unwrap_or_default(), r.end()))
        .collect();
    assert_eq!(ar.get("C01"), Some(&m("126000")), "宏远应欠 126000");
    assert_eq!(
        ar.get("C02"),
        Some(&m("34500")),
        "精密应欠 34500（票据+代付+抵账后）"
    );
    assert_eq!(ar.get("C03"), Some(&m("0")), "灯具应被代付+抹零全额清掉");
    let ap: std::collections::BTreeMap<String, Money> = snap
        .aux_breakdown("220201")
        .into_iter()
        .map(|r| (r.aux.supplier.clone().unwrap_or_default(), r.end()))
        .collect();
    assert_eq!(
        ap.get("S01"),
        Some(&m("0")),
        "恒信应结平（付款+背书+抵账+抹零）"
    );
    assert_eq!(ap.get("S02"), Some(&m("-6180")), "大发应剩 6180");
    assert_eq!(ap.get("S03"), Some(&m("-9950")), "亮洁应剩 9950");

    // 7) 科目微调生效（银行改名、收入改名、新增预付电费与制造费用五个明细）
    for (code, name) in [
        ("100201", "G银行对公户"),
        ("100202", "N银行结算户"),
        ("600101", "氧化加工费收入"),
        ("112301", "预付电费"),
        ("510106", "房租"),
        ("510107", "化工材料"),
        ("510108", "委外加工费"),
        ("510109", "宿舍租金"),
        ("510110", "社会保险费"),
    ] {
        let a = findb::accounts::get(&db, code)
            .expect("读科目")
            .unwrap_or_else(|| panic!("缺科目 {code}"));
        assert_eq!(a.name, name, "{code} 科目名不对");
    }

    // 8) 核销配对：凭证之间能配的配上，款与单不一一对应也照样轧平
    assert!(
        !findb::settle::list(&db, "112201")
            .expect("查配对")
            .is_empty(),
        "应收应有核销配对"
    );
    assert!(
        !findb::settle::list(&db, "220201")
            .expect("查配对")
            .is_empty(),
        "应付应有核销配对"
    );
    // 未清分录净额恒等于分录净发生（配对只记关系，不改金额）
    let sum_open = |acct: &str| {
        findb::settle::open_entries(&db, acct, p, false)
            .expect("查未清")
            .iter()
            .map(|e| {
                let o = e.open();
                if e.debit > Money::ZERO {
                    o
                } else {
                    Money::ZERO - o
                }
            })
            .fold(Money::ZERO, |a, b| a + b)
    };
    // 分录净额（不含期初，期初欠款没有分录）：160500 − 90000 期初 = 70500
    assert_eq!(sum_open("112201"), m("70500"));
    // 应付分录净额：−16130 − ( −64000 期初 ) = 47870
    assert_eq!(sum_open("220201"), m("47870"));

    // 9) 账龄按单位合计：期初单据（无分录、配不上）也在里面
    let buckets = vec![
        fincore::engine::aging::AgingBucket {
            label: "30天内",
            from: 0,
            to: Some(30),
        },
        fincore::engine::aging::AgingBucket {
            label: "31-60天",
            from: 31,
            to: Some(60),
        },
        fincore::engine::aging::AgingBucket {
            label: "60天以上",
            from: 61,
            to: None,
        },
    ];
    let as_of = chrono::NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
    let aging_ar = findb::settle::aging(&db, "112201", p, as_of, &buckets, None).expect("应收账龄");
    let ar_line = |key: &str| aging_ar.iter().find(|l| l.key == key);
    // 宏远：期初 60000 单据（无分录、永远配不上）+ 本月发票未清 86000 = 146000，
    // 其中 66000 是发票没配上，20000 是**那笔故意留着没认领的微信收款**（credit_total）：
    // 钱到了但还没落到哪张单上——这正是「收款认领」工作台要处理的状态，账龄里必须看得见。
    let c01 = ar_line("C01").expect("C01 账龄行");
    assert_eq!(c01.total, m("146000"));
    assert_eq!(c01.credit_total, m("20000"));
    // 精密：发票 169500 被票据 115000 + 收款 30000 + 抵账 20000 吃掉，剩 34500
    assert_eq!(ar_line("C02").map(|l| l.total), Some(m("34500")));
    // 灯具：本月开票全额收清（15000 代付 + 97950 收款 + 50 抹零），账龄里连行都没有
    assert!(ar_line("C03").is_none(), "灯具已全额收清，账龄里不该有行");

    let aging_ap = findb::settle::aging(&db, "220201", p, as_of, &buckets, None).expect("应付账龄");
    let ap_line = |key: &str| aging_ap.iter().find(|l| l.key == key);
    // 恒信：期初 46000 单据 + 本月 49800 采购，被付 60000、背书 15000、抵账 20000、
    // 尾款 800（抹零 20）正好冲平 —— 账龄行还在（期初单据永远配不上），净额为零
    let s01 = ap_line("S01").expect("S01 账龄行");
    assert_eq!(s01.total, m("46000"));
    assert_eq!(s01.credit_total, m("46000"));
    // 大发：期初 18000 + 喷砂 12000 + 拉丝 6180 − 付 30000 = 6180
    let s02 = ap_line("S02").expect("S02 账龄行");
    assert_eq!(s02.total, m("18000"));
    assert_eq!(s02.credit_total, m("11820"));
    // 亮洁：除油剂 5000 + 封闭剂 4950 全挂着（抛光 3500 是现付，不进应付）
    assert_eq!(ap_line("S03").map(|l| l.total), Some(m("9950")));

    let _ = std::fs::remove_file(&path);
}

/// 三大报表：资产负债表左右相等、利润表净利润勾稽、现金流量表自平。
#[test]
fn 一般纳税人演示账三大报表勾稽() {
    let path = build_book_named("oxide_vat_reports");
    let db = findb::Db::open(&path).expect("打开演示账");
    let from = fincore::Period::new(2026, 1).unwrap();
    let to = fincore::Period::new(2026, 11).unwrap();
    let snap = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::range(from, to),
    )
    .expect("取余额快照");

    // 资产负债表：资产合计 == 负债和所有者权益合计（期末列）
    let bs_def = fincore::report::balance_sheet::balance_sheet_def();
    let bs = fincore::report::render(
        &bs_def,
        &snap,
        "恒亮氧化加工厂",
        "2026-11 期末",
        vec![
            Box::new(fincore::report::identity),
            Box::new(fincore::report::to_begin),
        ],
    );
    let end_of = |idx: usize| bs.rows[idx].values[0];
    let assets =
        end_of(fincore::report::balance_sheet::asset_total_row(&bs_def).expect("资产合计行"));
    let liab_eq = end_of(
        fincore::report::balance_sheet::liab_equity_total_row(&bs_def).expect("负债权益合计行"),
    );
    assert_eq!(assets, m("412480"), "资产合计");
    assert_eq!(liab_eq, m("412480"), "负债和所有者权益合计");
    assert_eq!(assets, liab_eq, "资产负债表左右必须相等");

    // 利润表：本期净利润 == 17119.18（快照口径与界面一致：剔除结转损益凭证）
    fn to_ytd(k: fincore::report::AmountKind) -> fincore::report::AmountKind {
        match k {
            fincore::report::AmountKind::PeriodDebit => fincore::report::AmountKind::YearDebit,
            fincore::report::AmountKind::PeriodCredit => fincore::report::AmountKind::YearCredit,
            other => other,
        }
    }
    let snap_is = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::range(from, to).with_exclude_carry(true),
    )
    .expect("取利润表口径快照");
    let is_def = fincore::report::income::income_statement_def();
    let is = fincore::report::render(
        &is_def,
        &snap_is,
        "恒亮氧化加工厂",
        "2026-11",
        vec![Box::new(fincore::report::identity), Box::new(to_ytd)],
    );
    let profit =
        is.rows[fincore::report::income::net_profit_row(&is_def).expect("净利润行")].values[0];
    assert_eq!(profit, m("17119.18"), "本期净利润");

    // 现金流量表：自带勾稽（净增加 == 期末 − 期初）+ 首尾数
    // 期初现金 18000+126000+54000 = 198000；期末 70620+91160+10200 = 171980
    let cf = findb::reports::cash_flow_statement(&db, from, to, None).expect("现金流量表");
    assert!(cf.ties(), "现金流量表必须自平");
    assert_eq!(cf.begin_cash, m("198000"), "期初现金");
    assert_eq!(cf.end_cash, m("171980"), "期末现金");
    assert_eq!(cf.net_increase, m("-26020"), "现金净增加");

    // 一般纳税人口径：税负 = 应交增值税 / 不含税收入 = 48759.66 / 450000 = 10.84%
    let vat = m("48759.66");
    let revenue = m("450000");
    let burden = vat.checked_div(revenue).expect("除数").round2();
    assert_eq!(burden, m("0.11"), "税负率应在 11% 量级");

    println!(
        "资产 {assets:?} = 负债权益 {liab_eq:?}；净利润 {profit:?}；现金 {:?} -> {:?}（净增加 {:?}）；税负 {burden:?}",
        cf.begin_cash, cf.end_cash, cf.net_increase
    );
    let _ = std::fs::remove_file(&path);
}

/// 增值税申报表与账上增值税必须一分不差：这是「账表一致」的底线。
/// 申报表按**发票**取数（进项只认已认证的专票），账按**凭证**取数，
/// 两边对不上，要么票没录全，要么凭证里的税算错了。
#[test]
fn 一般纳税人申报表与账上增值税一致() {
    let path = build_book_named("oxide_vat_tax");
    let db = findb::Db::open(&path).expect("打开演示账");
    let p = fincore::Period::from_ymm(202611);

    // 发票都录好了：销项 3 张、进项 12 张（房租/宿舍/抛光/劳保/办公/招待没有票，不录）
    let outs = findb::invoices::list(
        &db,
        &findb::invoices::InvoiceQuery {
            kind: Some("out".into()),
            ..Default::default()
        },
    )
    .expect("查销项票");
    let ins = findb::invoices::list(
        &db,
        &findb::invoices::InvoiceQuery {
            kind: Some("in".into()),
            ..Default::default()
        },
    )
    .expect("查进项票");
    assert_eq!(outs.len(), 3, "销项发票张数");
    assert_eq!(ins.len(), 12, "进项发票张数");
    assert!(
        ins.iter().all(|i| i.status == "verified"),
        "进项票必须全部已认证，否则申报表取不到进项"
    );

    // 申报表（主表 + 附加税费）取数
    let form = findb::taxdecl::vat_main_form(&db, p).expect("增值税申报表");
    assert_eq!(form.sales_general, m("450000"), "一般计税销售额");
    assert_eq!(form.output_tax, m("58500"), "销项税额");
    assert_eq!(form.input_tax, m("9740.34"), "进项税额");
    assert_eq!(form.payable, m("48759.66"), "应纳税额");
    assert_eq!(form.surcharge.city_tax, m("3413.18"), "城建税 7%");
    assert_eq!(form.surcharge.edu, m("1462.79"), "教育费附加 3%");
    assert_eq!(form.surcharge.local_edu, m("975.19"), "地方教育附加 2%");
    assert_eq!(form.surcharge.total, m("5851.16"), "附加税费合计");

    // 账表一致：凭证里的销项、进项、未交增值税、税金及附加，与申报表逐个对。
    // 注意 6403 / 22210102 期末余额已被结转踢平，看「本期发生额」才是本月真实数。
    let snap = findb::balances::BalanceSnapshot::load(
        &db,
        &findb::balances::BalanceQuery::range(p, p).with_posted_only(true),
    )
    .expect("科目余额");
    let row = |code: &str| snap.for_account(code, None);
    assert_eq!(row("22210102").credit, m("58500"), "账上销项税额");
    assert_eq!(row("22210101").debit, m("9740.34"), "账上进项税额");
    assert_eq!(row("22210104").debit, m("48759.66"), "账上转出未交增值税");
    assert_eq!(row("222102").credit, m("48759.66"), "账上未交增值税");
    assert_eq!(
        row("22210102").credit - row("22210101").debit,
        form.payable,
        "账上（销项 − 进项）必须等于申报表应纳税额"
    );
    assert_eq!(
        row("6403").debit,
        form.surcharge.total,
        "账上税金及附加必须等于申报表附加税费合计"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn 一般纳税人演示账里四个新功能都有样例数据() {
    // 演示账不能只有「借贷平衡」——会计打开它是要**照着做**的。
    // 这一条钉住：电费台账、收款认领、委外三票、申报体检四件事在账套里都摸得到实物。
    let path = build_book_named("oxide_vat_samples");
    let db = findb::Db::open(&path).expect("打开演示账");
    let p = fincore::Period::from_ymm(202611);

    // 1) 电费台账：一行抄表记录，已结转并挂着凭证；结转额一分不差
    let rows = findb::meter::list(&db, Some(p.ymm()), Some(findb::meter::KIND_ELECTRIC))
        .expect("读电费台账");
    assert_eq!(rows.len(), 1, "11 月应有一行抄表记录");
    assert_eq!(rows[0].qty, m("15000"), "抄表用量");
    assert_eq!(rows[0].amount_incl, m("18000"), "含税金额");
    assert!(rows[0].is_closed(), "这行应已结转");
    let (net, tax) = rows[0].split().expect("拆分");
    assert_eq!((net, tax), (m("15929.20"), m("2070.80")), "13% 电费拆进项");

    // 2) 收款认领：留了一笔 20,000 待认领，建议正好指向宏远那张没清完的发票
    let inflows = findb::claim::inflows(&db, "112201", p).expect("待认收款");
    assert_eq!(inflows.len(), 1, "应留 1 笔没认领的收款");
    assert_eq!(inflows[0].open, m("20000"), "待认金额");
    assert_eq!(inflows[0].party, "C01", "这笔钱记在宏远名下");
    let cands = findb::claim::candidates(&db, "112201", p).expect("待清单据");
    let sug = findb::claim::suggest(&inflows[0], &cands);
    assert_eq!(sug.len(), 1, "建议应能配掉这笔钱");
    assert_eq!(sug[0].amount, m("20000"));

    // 3) 委外三票：同样 10,000 元，进成本差 1,150 元
    let cmp = findb::outsource::compare(m("10000")).expect("三票对比");
    assert_eq!(cmp.len(), 3);
    assert_eq!(cmp[0].net, m("8849.56"), "13% 专票进成本");
    assert_eq!(cmp[1].net, m("9708.74"), "3% 征收率进成本");
    assert_eq!(cmp[2].net, m("10000.00"), "无票进成本");

    // 4) 申报体检：这本账是干净的，0 个错误、可以申报
    let rep = findb::taxprecheck::precheck(&db, p).expect("申报体检");
    assert_eq!(rep.errors, 0, "演示账不该有申报错误");
    assert!(rep.can_file, "没有错误就应该可以申报");

    drop(db);
    let _ = std::fs::remove_file(&path);
}
