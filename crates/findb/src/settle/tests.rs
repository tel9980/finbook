use super::*;
use crate::tests::mem;
use fincore::voucher::{AuxRef, Entry, Voucher};

/// 造一张两行凭证并过账，返回凭证 id。
fn post_two(
    db: &Db,
    p: Period,
    day: u32,
    no: i32,
    debit: (&str, Money, Option<(&str, &str)>),
    credit: (&str, Money, Option<(&str, &str)>),
) -> i64 {
    let aux_of = |a: Option<(&str, &str)>| match a {
        None => AuxRef::default(),
        Some(("customer", code)) => AuxRef {
            customer: Some(code.to_string()),
            ..Default::default()
        },
        Some(("supplier", code)) => AuxRef {
            supplier: Some(code.to_string()),
            ..Default::default()
        },
        Some((_, code)) => AuxRef {
            customer: Some(code.to_string()),
            ..Default::default()
        },
    };
    let d = NaiveDate::from_ymd_opt(p.year(), p.month(), day).unwrap();
    let mut v = Voucher::new(p, d, "记", no);
    v.push_entry(Entry {
        debit: debit.1,
        credit: Money::ZERO,
        aux: aux_of(debit.2),
        ..Entry::new(1, debit.0, "借")
    });
    v.push_entry(Entry {
        debit: Money::ZERO,
        credit: credit.1,
        aux: aux_of(credit.2),
        ..Entry::new(2, credit.0, "贷")
    });
    let vid = crate::vouchers::save(db, &mut v).unwrap();
    crate::vouchers::post(db, vid, "poster").unwrap();
    vid
}

fn m(s: &str) -> Money {
    Money::parse(s).unwrap()
}

/// 客户既欠 5000 又预存 3000：催款单上的**催收金额只能是 5000**。
///
/// 回归：`dunning_create` 原来用 `e.open()`（绝对值）累加，预存那笔也被算进
/// 「应向我方支付」—— 客户收到一张 8000 的催款单，其中 3000 是他自己的钱。
/// 账龄页把预收单列在「其中预收」，两处口径打架。
#[test]
fn dunning_counts_only_what_is_actually_owed() {
    let db = mem();
    let p = Period::new(2026, 1).unwrap();
    // 应收 5000
    post_two(
        &db,
        p,
        5,
        1,
        ("112201", m("5000"), Some(("customer", "C01"))),
        ("1001", m("5000"), None),
    );
    // 预收 3000
    post_two(
        &db,
        p,
        8,
        2,
        ("1001", m("3000"), None),
        ("112201", m("3000"), Some(("customer", "C01"))),
    );

    let d = dunning_create(
        &db,
        "ar",
        "1122",
        "C01",
        "客户甲",
        NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
        "",
        "boss",
    )
    .unwrap();
    assert_eq!(d.amount, m("5000"), "催收金额只能是真正欠的 5000");
    assert_eq!(d.item_count, 1, "明细行数只数欠款那一侧");
    assert_eq!(d.owed_total(), m("5000"));
    assert_eq!(d.advance_total(), m("3000"), "预收要单独列出来");
    assert_eq!(d.detail.len(), 2, "两种方向的单据都要能看到");
    assert!(d.detail.iter().any(|i| i.direction == "预收"));
    assert!(d.detail.iter().any(|i| i.direction == "应收"));
}

/// 催款函明细要逐单写「逾期几天」——客户对账时看的就是这一列
///
/// 只给一个总数不行：客户手里有十几张单，得知道是哪一张超期、超了多久。
#[test]
fn dunning_items_carry_overdue_days_from_pay_days() {
    let db = tmpdb("dunning_overdue");
    let _p = Period::new(2026, 10).unwrap();

    // 客户 C01：账期 30 天
    let mut c = fincore::AuxEntity::new(fincore::AuxKind::Customer, "C01", "客户甲");
    c.props.insert(
        fincore::auxiliary::prop::PAY_DAYS.to_string(),
        "30".to_string(),
    );
    crate::auxs::insert(&db, &c).unwrap();

    arap_opening_insert(
        &db,
        &ArapOpening {
            id: 0,
            kind: "ar".into(),
            party_code: "C01".into(),
            party_name: "客户甲".into(),
            doc_no: "XS-OLD".into(),
            doc_date: "2026-09-01".into(),
            amount: Money::parse("6000").unwrap(),
            memo: String::new(),
            created_by: "u".into(),
        },
        "u",
    )
    .unwrap();
    arap_opening_insert(
        &db,
        &ArapOpening {
            id: 0,
            kind: "ar".into(),
            party_code: "C01".into(),
            party_name: "客户甲".into(),
            doc_no: "XS-NEW".into(),
            doc_date: "2026-10-10".into(),
            amount: Money::parse("4000").unwrap(),
            memo: String::new(),
            created_by: "u".into(),
        },
        "u",
    )
    .unwrap();

    // 开函日 10-25：老单 54 天 − 账期 30 = 逾期 24；新单 15 天 < 30 → 0
    let d = dunning_create(
        &db,
        "ar",
        "1122",
        "C01",
        "客户甲",
        NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
        "",
        "boss",
    )
    .unwrap();
    let old = d
        .detail
        .iter()
        .find(|i| i.doc_no.contains("XS-OLD"))
        .expect("老单据要在明细里");
    assert_eq!(old.overdue_days, 24, "54 天账龄 − 30 天账期 = 逾期 24");
    let new = d
        .detail
        .iter()
        .find(|i| i.doc_no.contains("XS-NEW"))
        .expect("新单据也要在明细里");
    assert_eq!(new.overdue_days, 0, "还没到期，不能标逾期");
}

/// 客商档案没填账期：明细的逾期一律 0，不瞎猜
#[test]
fn dunning_overdue_is_zero_without_pay_days() {
    let db = tmpdb("dunning_no_pay_days");
    crate::auxs::insert(
        &db,
        &fincore::AuxEntity::new(fincore::AuxKind::Customer, "C02", "客户乙"),
    )
    .unwrap();
    arap_opening_insert(
        &db,
        &ArapOpening {
            id: 0,
            kind: "ar".into(),
            party_code: "C02".into(),
            party_name: "客户乙".into(),
            doc_no: "XS-2".into(),
            doc_date: "2025-01-05".into(),
            amount: Money::parse("100").unwrap(),
            memo: String::new(),
            created_by: "u".into(),
        },
        "u",
    )
    .unwrap();
    let d = dunning_create(
        &db,
        "ar",
        "1122",
        "C02",
        "客户乙",
        NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
        "",
        "boss",
    )
    .unwrap();
    assert_eq!(
        d.detail[0].overdue_days, 0,
        "没设账期就不能说逾期——说了客户会反问「按什么约定的」"
    );
}

/// 只有预收、一分不欠：不能生成催款单（以前会出一张 0.00 元的单子）。
#[test]
fn dunning_refuses_when_nothing_is_owed() {
    let db = mem();
    let p = Period::new(2026, 1).unwrap();
    post_two(
        &db,
        p,
        8,
        1,
        ("1001", m("3000"), None),
        ("112201", m("3000"), Some(("customer", "C01"))),
    );
    let err = dunning_create(
        &db,
        "ar",
        "1122",
        "C01",
        "客户甲",
        NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
        "",
        "boss",
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("不需要催款"), "错误要说清原因：{err}");
    assert!(err.contains("预收"), "错误要指出那是预收：{err}");
}

/// 应付方向：供应商开票（贷方）是**应付**，不是预付。
///
/// 回归：贷方性质科目不翻号的话，`signed()` 是负数，一张正常的供应商发票
/// 会被判成「预付」—— 对账函上一条应付都不显示。
#[test]
fn dunning_treats_supplier_invoice_as_payable() {
    let db = mem();
    let p = Period::new(2026, 1).unwrap();
    post_two(
        &db,
        p,
        6,
        1,
        ("1001", m("4000"), None),
        ("220201", m("4000"), Some(("supplier", "S01"))),
    );
    let d = dunning_create(
        &db,
        "ap",
        "2202",
        "S01",
        "供应商甲",
        NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
        "",
        "boss",
    )
    .unwrap();
    assert_eq!(d.amount, m("4000"), "供应商开票 4000 应该是对账函上的应付");
    assert_eq!(d.advance_total(), Money::ZERO);
    assert_eq!(d.detail[0].direction, "应付");
}

/// 同一客户既有凭证又有期初挂账时，账龄**必须合并成一行**、金额相加。
///
/// 回归：凭证行的 key 曾用 `e.aux_key.clone()`（`customer=C01`），期初行用
/// `o.party_code`（`C01`）—— 两种格式混用，`analyze()` 把同一个客户分成两行
/// （实测 `["customer=C01", "C01"]`，3000 与 500 分在两行）。界面上看起来是两个
/// 往来单位，按行做的操作（核销/导出/催款）会漏掉一半。
///
/// 而把 `aux_key.clone()` 放回去**不会有任何其他测试变红** —— 所以钉在这里。
#[test]
fn aging_merges_voucher_and_opening_for_same_party() {
    let db = mem();
    let p = Period::new(2026, 1).unwrap();

    arap_opening_insert(
        &db,
        &ArapOpening {
            id: 0,
            kind: "ar".into(),
            party_code: "C01".into(),
            party_name: "客户甲".into(),
            doc_no: "XS-1".into(),
            doc_date: "2025-12-01".into(),
            amount: Money::parse("500").unwrap(),
            memo: String::new(),
            created_by: "u".into(),
        },
        "u",
    )
    .unwrap();

    let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "记", 1);
    v.push_entry(Entry {
        debit: Money::parse("3000").unwrap(),
        credit: Money::ZERO,
        aux: AuxRef {
            customer: Some("C01".into()),
            ..Default::default()
        },
        ..Entry::new(1, "112201", "应收")
    });
    v.push_entry(Entry {
        debit: Money::ZERO,
        credit: Money::parse("3000").unwrap(),
        ..Entry::new(2, "1001", "收")
    });
    let vid = crate::vouchers::save(&db, &mut v).unwrap();
    crate::vouchers::post(&db, vid, "poster").unwrap();

    let lines = aging(
        &db,
        "1122",
        p,
        NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
        &fincore::engine::aging::buckets_by_days(),
        None,
    )
    .unwrap();
    assert_eq!(
        lines.len(),
        1,
        "同一客户必须只有一行：{:?}",
        lines
            .iter()
            .map(|l| (l.key.clone(), l.total.fmt_money()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        lines[0].key, "C01",
        "key 必须是裸客户编码，不能带 customer= 前缀 —— 那是 aux_key 的内部存储格式"
    );
    assert_eq!(
        lines[0].total,
        Money::parse("3500").unwrap(),
        "金额应相加（3000 凭证 + 500 期初），不能分在两行里"
    );
}

/// 账期落地：客户档案里填了 30 天，9/1 的单 10/25 还没给 → 逾期 24 天
///
/// 这是 `pay_days` 第一次真的被读。以前它只是档案里的一个字段，
/// 填什么都一样 —— 而「账期」恰恰是催款时唯一有意义的参照。
#[test]
fn aging_uses_pay_days_to_compute_overdue() {
    let db = tmpdb("pay_days");
    let p = Period::new(2026, 10).unwrap();

    // 客户 C01，账期 30 天
    let mut c = fincore::AuxEntity::new(fincore::AuxKind::Customer, "C01", "客户甲");
    c.props.insert(
        fincore::auxiliary::prop::PAY_DAYS.to_string(),
        "30".to_string(),
    );
    crate::auxs::insert(&db, &c).unwrap();

    arap_opening_insert(
        &db,
        &ArapOpening {
            id: 0,
            kind: "ar".into(),
            party_code: "C01".into(),
            party_name: "客户甲".into(),
            doc_no: "XS-1".into(),
            doc_date: "2026-09-01".into(),
            amount: Money::parse("10000").unwrap(),
            memo: String::new(),
            created_by: "u".into(),
        },
        "u",
    )
    .unwrap();

    let as_of = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
    let lines = aging(
        &db,
        "1122",
        p,
        as_of,
        &fincore::engine::aging::buckets_by_days(),
        None,
    )
    .unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].max_days, 54, "9/1 到 10/25 是 54 天");
    assert_eq!(lines[0].pay_days, Some(30));
    assert_eq!(
        lines[0].overdue_days, 24,
        "54 - 30 = 逾期 24 天，这才是催款要说的数"
    );
}

/// 没填账期 / 账期没到：`overdue_days` 必须是 0，不能拿账龄冒充逾期
#[test]
fn overdue_is_zero_without_pay_days_or_before_due() {
    let db = tmpdb("pay_days_none");
    let p = Period::new(2026, 10).unwrap();
    for (code, days) in [("C01", None), ("C02", Some("60"))] {
        let mut c = fincore::AuxEntity::new(fincore::AuxKind::Customer, code, code);
        if let Some(d) = days {
            c.props.insert(
                fincore::auxiliary::prop::PAY_DAYS.to_string(),
                d.to_string(),
            );
        }
        crate::auxs::insert(&db, &c).unwrap();
        arap_opening_insert(
            &db,
            &ArapOpening {
                id: 0,
                kind: "ar".into(),
                party_code: code.into(),
                party_name: code.into(),
                doc_no: format!("XS-{code}"),
                doc_date: "2026-10-01".into(),
                amount: Money::parse("1000").unwrap(),
                memo: String::new(),
                created_by: "u".into(),
            },
            "u",
        )
        .unwrap();
    }
    let lines = aging(
        &db,
        "1122",
        p,
        NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
        &fincore::engine::aging::buckets_by_days(),
        None,
    )
    .unwrap();
    let c1 = lines.iter().find(|l| l.key == "C01").unwrap();
    assert_eq!(c1.pay_days, None);
    assert_eq!(c1.overdue_days, 0, "没填账期就不能说逾期");
    let c2 = lines.iter().find(|l| l.key == "C02").unwrap();
    assert_eq!(c2.pay_days, Some(60));
    assert_eq!(c2.overdue_days, 0, "账期 60 天，才 19 天没到期");
}

/// 维度按科目的真实辅助核算取，不按首位猜。
/// 2203 预收挂的是客户核算、1123 其他应收挂的是供应商 —— 首位规则下
/// 2203 整页空白（已实锤：5000 余额账龄 0 行），1123 同理。
#[test]
fn party_dim_按科目真实辅助核算() {
    let db = tmpdb("party_dim");
    let chart = crate::accounts::chart(&db).unwrap();
    assert_eq!(party_kind_of(&chart, "2203"), fincore::AuxKind::Customer);
    assert_eq!(party_kind_of(&chart, "1123"), fincore::AuxKind::Supplier);
    // 父级科目无辅助标记，退回首位推断（行为不变）
    assert_eq!(party_kind_of(&chart, "1122"), fincore::AuxKind::Customer);
    assert_eq!(party_kind_of(&chart, "2202"), fincore::AuxKind::Supplier);
    // 账套里没有的科目，退回首位推断
    assert_eq!(party_kind_of(&chart, "9999"), fincore::AuxKind::Customer);
}

/// 2203 预收账款的账龄必须出行（挂客户核算，收预收时贷记）。
#[test]
fn aging_2203_预收按客户出() {
    let db = tmpdb("aging_2203");
    let p = Period::new(2026, 11).unwrap();
    let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(), "收", 1);
    v.push_entry(Entry {
        debit: Money::parse("5000").unwrap(),
        credit: Money::ZERO,
        ..Entry::new(1, "1001", "收预收")
    });
    v.push_entry(Entry {
        debit: Money::ZERO,
        credit: Money::parse("5000").unwrap(),
        aux: AuxRef {
            customer: Some("C01".into()),
            ..Default::default()
        },
        ..Entry::new(2, "2203", "预收甲款")
    });
    let vid = crate::vouchers::save(&db, &mut v).unwrap();
    crate::vouchers::post(&db, vid, "poster").unwrap();

    let lines = aging(
        &db,
        "2203",
        p,
        NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(),
        &fincore::engine::aging::buckets_by_days(),
        None,
    )
    .unwrap();
    assert_eq!(lines.len(), 1, "2203 账龄必须出行");
    assert_eq!(lines[0].key, "C01");
    assert_eq!(lines[0].total, Money::parse("5000").unwrap());
}

/// 应付账龄（2202）必须仍按 **supplier** 维度出行。
///
/// 回归（我自己差点发货的那一个）：key 的取值函数第一版写死了 customer，
/// 而应付挂的是 `supplier=` —— 于是整个应付账龄页**一行都不出，且不报错**。
/// 写死 customer 的版本里，**应收的 3 条测试全部通过**，只有这条会红。
///
/// 「不报错」是它危险的地方：一个只查应收的测试集永远发现不了。
#[test]
fn aging_uses_supplier_dim_for_payable() {
    let db = mem();
    let p = Period::new(2026, 1).unwrap();
    let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "记", 1);
    v.push_entry(Entry {
        debit: Money::parse("2000").unwrap(),
        credit: Money::ZERO,
        aux: AuxRef {
            supplier: Some("S01".into()),
            ..Default::default()
        },
        ..Entry::new(1, "220201", "应付货款")
    });
    v.push_entry(Entry {
        debit: Money::ZERO,
        credit: Money::parse("2000").unwrap(),
        ..Entry::new(2, "1001", "付")
    });
    let vid = crate::vouchers::save(&db, &mut v).unwrap();
    crate::vouchers::post(&db, vid, "poster").unwrap();

    let lines = aging(
        &db,
        "2202",
        p,
        NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
        &fincore::engine::aging::buckets_by_days(),
        None,
    )
    .unwrap();
    assert_eq!(
        lines.len(),
        1,
        "应付账龄必须出行 —— key 取值写死 customer 会让 2202 整页空白：{:?}",
        lines
            .iter()
            .map(|l| (l.key.clone(), l.total.fmt_money()))
            .collect::<Vec<_>>()
    );
    assert_eq!(lines[0].key, "S01", "key 是**裸供应商编码**");
    // 这笔是借方 220201（预付供应商）—— 翻正后落在预付栏，不进合计。
    // （归一化之前它进合计栏，和"欠供应商多少"混在一起；且整张应付账龄的
    // 合计恒 0、最长账龄恒 0。预付就该在预付栏。）
    assert!(lines[0].total.is_zero());
    assert_eq!(lines[0].credit_total, Money::parse("2000").unwrap());
}

/// 没挂客户辅助核算的分录不进往来账龄。
///
/// 否则页面上会出现一行 key 为空的记录：无法解释、也无法操作。
///
/// 场景必须是**自定义的应收科目**（不带 aux 的）—— 预置的 112201/112202
/// 在科目表里就声明了「核算客户」，凭证校验会直接拒（我第一版想用 112201 挂存货，
/// 报的是「第 1 行科目 112201 核算客户，必须填写客户」）。所以这条 filter
/// 守的是**用户自己加的**应收科目那种情形 —— 那才是它能生效的地方。
///
/// 走 `accounts::insert` 而不是手写 INSERT：`account` 表的辅助核算是
/// `aux_mask` 位掩码且**没有 `leaf` 列**（我照抄别处的 `aux`/`leaf` 写法，
/// 报的是 `table account has no column named leaf`）。手写 SQL 会随 schema 漂移。
#[test]
fn aging_skips_entries_without_party() {
    let db = mem();
    let p = Period::new(2026, 1).unwrap();
    crate::accounts::insert(
        &db,
        &fincore::account::Account {
            code: "112299".into(),
            name: "自定义应收(无辅助)".into(),
            category: fincore::account::AcctCategory::Asset,
            dir: fincore::account::Direction::Debit,
            aux: Default::default(),
            unit: None,
            currency: None,
            has_qty: false,
            is_cash: false,
            is_bank: false,
            cash_flow_item: None,
            bs_item: None,
            pl_item: None,
            disabled: false,
            memo: String::new(),
        },
    )
    .unwrap();
    let mut v = Voucher::new(p, NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(), "记", 1);
    v.push_entry(Entry {
        debit: Money::parse("1000").unwrap(),
        credit: Money::ZERO,
        ..Entry::new(1, "112299", "应收")
    });
    v.push_entry(Entry {
        debit: Money::ZERO,
        credit: Money::parse("1000").unwrap(),
        ..Entry::new(2, "1001", "收")
    });
    let vid = crate::vouchers::save(&db, &mut v).unwrap();
    crate::vouchers::post(&db, vid, "poster").unwrap();

    let lines = aging(
        &db,
        "1122",
        p,
        NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
        &fincore::engine::aging::buckets_by_days(),
        None,
    )
    .unwrap();
    assert!(
        lines.is_empty(),
        "没挂往来单位的行不该进往来账龄：{:?}",
        lines.iter().map(|l| l.key.clone()).collect::<Vec<_>>()
    );
}

fn tmpdb(name: &str) -> Db {
    let p = std::env::temp_dir().join(format!("finbook_settle_{name}.fbk"));
    let _ = std::fs::remove_file(&p);
    Db::create(&p, &crate::tests::test_opts()).unwrap()
}

/// 建一张已记账的往来凭证
fn ar_voucher(
    db: &Db,
    period: Period,
    date: NaiveDate,
    no: i32,
    cust: &str,
    amount: &str,
    dir_debit: bool,
    counterpart: &str,
) -> (i64, i64) {
    let mut v = Voucher::new(Period::from_date(date), date, "记", no);
    let a = Money::parse(amount).unwrap();
    let e1 = Entry {
        debit: if dir_debit { a } else { Money::ZERO },
        credit: if dir_debit { Money::ZERO } else { a },
        aux: AuxRef {
            customer: Some(cust.into()),
            ..Default::default()
        },
        ..Entry::new(1, "112201", "往来")
    };
    let e2 = Entry {
        debit: if dir_debit { Money::ZERO } else { a },
        credit: if dir_debit { a } else { Money::ZERO },
        aux: if counterpart == "100201" {
            AuxRef {
                bank: Some("BANK01".into()),
                ..Default::default()
            }
        } else {
            AuxRef::default()
        },
        ..Entry::new(2, counterpart, "往来")
    };
    let _ = period;
    v.push_entry(e1);
    v.push_entry(e2);
    let vid = crate::vouchers::save(db, &mut v).unwrap();
    crate::vouchers::post(db, vid, "poster").unwrap();
    let entries = crate::vouchers::entries_of(db, vid).unwrap();
    (vid, entries[0].id)
}

/// 建一张**未记账**（草稿）的往来凭证，返回 (凭证 id, 112201 分录 id)
fn ar_voucher_draft(
    db: &Db,
    date: NaiveDate,
    no: i32,
    cust: &str,
    amount: &str,
    dir_debit: bool,
    counterpart: &str,
) -> (i64, i64) {
    let mut v = Voucher::new(Period::from_date(date), date, "记", no);
    let a = Money::parse(amount).unwrap();
    let e1 = Entry {
        debit: if dir_debit { a } else { Money::ZERO },
        credit: if dir_debit { Money::ZERO } else { a },
        aux: AuxRef {
            customer: Some(cust.into()),
            ..Default::default()
        },
        ..Entry::new(1, "112201", "往来")
    };
    let e2 = Entry {
        debit: if dir_debit { Money::ZERO } else { a },
        credit: if dir_debit { a } else { Money::ZERO },
        ..Entry::new(2, counterpart, "往来")
    };
    v.push_entry(e1);
    v.push_entry(e2);
    let vid = crate::vouchers::save(db, &mut v).unwrap();
    // 不 post —— 停在草稿
    let entries = crate::vouchers::entries_of(db, vid).unwrap();
    (vid, entries[0].id)
}

/// 「含未记账」只读开关：默认看不到草稿，开了能看到且 `posted=false`。
/// 对标金蝶单据驱动模型——未审核单据在列表里可见，但不参与核销。
#[test]
fn open_entries_with_can_show_drafts_readonly() {
    let db = tmpdb("drafts");
    let d1 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let d2 = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
    // 一张已记账 + 一张草稿
    ar_voucher(
        &db,
        Period::new(2026, 1).unwrap(),
        d1,
        1,
        "C01",
        "1000",
        true,
        "600101",
    );
    ar_voucher_draft(&db, d2, 2, "C01", "500", true, "600101");
    let p = Period::new(2026, 1).unwrap();

    // 默认（核销口径）：只有已记账那笔
    let posted = open_entries(&db, "112201", p, false).unwrap();
    assert_eq!(posted.len(), 1, "默认不应返回未记账行");
    assert!(posted[0].posted);
    assert_eq!(posted[0].open(), Money::parse("1000").unwrap());

    // 打开开关：两笔都在，草稿那笔标 posted=false
    let all = open_entries_with(&db, "112201", p, false, false, None).unwrap();
    assert_eq!(all.len(), 2, "「含未记账」应能看到草稿行");
    let drafts: Vec<&OpenEntry> = all.iter().filter(|e| !e.posted).collect();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].open(), Money::parse("500").unwrap());
    assert!(all.iter().filter(|e| e.posted).count() == 1);
}

/// 核销的**被核销方**（from_entry）必须已记账——手工核销的 entry id 由前端传入，
/// 绕过了 open_entries 的 posted-only 过滤，只能靠 `settle` 自己兜住。
/// 拿草稿当债权去核销，那张凭证一删核销记录就没了。
/// 而 to 侧（收款单那张还没记账的凭证）允许是草稿——这正是金蝶的做法。
#[test]
fn settle_requires_posted_from_side() {
    let db = tmpdb("unposted_settle");
    let d1 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let d2 = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
    let (_, e_ar_posted) = ar_voucher(
        &db,
        Period::new(2026, 1).unwrap(),
        d1,
        1,
        "C01",
        "1000",
        true,
        "600101",
    );
    // 对方用 1001（库存现金，不核算银行账户），避免 100201 要求 bank 辅助核算
    let (_, e_cash_posted) = ar_voucher(
        &db,
        Period::new(2026, 1).unwrap(),
        d2,
        2,
        "C01",
        "600",
        false,
        "1001",
    );

    // from 侧是草稿 → 拒绝
    let (_, e_ar_draft) = ar_voucher_draft(&db, d2, 3, "C01", "800", true, "600101");
    let err = settle(
        &db,
        e_ar_draft,
        e_ar_posted,
        Money::parse("100").unwrap(),
        "u1",
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("已记账"),
        "草稿债权应被拒绝：{err}"
    );
    assert_eq!(
        settled_of(db.conn(), e_ar_draft).unwrap(),
        Money::ZERO,
        "被拒后不得留下核销记录"
    );

    // to 侧是草稿（收款单凭证未记账）→ 允许，金蝶即此语义
    let (_, e_receipt_draft) = ar_voucher_draft(&db, d2, 4, "C01", "400", false, "1001");
    settle(
        &db,
        e_ar_posted,
        e_receipt_draft,
        Money::parse("400").unwrap(),
        "u1",
    )
    .expect("已记账债权 + 未记账收款单应允许核销（对标金蝶「单据审核→核销→生成凭证」）");
    assert_eq!(
        settled_of(db.conn(), e_ar_posted).unwrap(),
        Money::parse("400").unwrap()
    );

    // 两边都已记账 → 照常
    settle(
        &db,
        e_ar_posted,
        e_cash_posted,
        Money::parse("600").unwrap(),
        "u1",
    )
    .unwrap();
    assert_eq!(
        settled_of(db.conn(), e_ar_posted).unwrap(),
        Money::parse("1000").unwrap()
    );
}

#[test]
fn manual_settle_flow() {
    let db = tmpdb("manual");
    let p = Period::new(2026, 1).unwrap();
    let d1 = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let d2 = NaiveDate::from_ymd_opt(2026, 1, 20).unwrap();
    let (_, e_ar) = ar_voucher(&db, p, d1, 1, "C01", "1000", true, "600101");
    let (_, e_cash) = ar_voucher(&db, p, d2, 2, "C01", "600", false, "100201");

    settle(&db, e_ar, e_cash, Money::parse("600").unwrap(), "u1").unwrap();
    assert_eq!(
        settled_of(db.conn(), e_ar).unwrap(),
        Money::parse("600").unwrap()
    );

    // 超额核销要拦
    assert!(settle(&db, e_ar, e_cash, Money::parse("500").unwrap(), "u1").is_err());
    // 自己核销自己要拦
    assert!(settle(&db, e_ar, e_ar, Money::parse("10").unwrap(), "u1").is_err());

    let opens = open_entries(&db, "1122", p, false).unwrap();
    assert_eq!(opens.len(), 1); // 收款那条已被核完
    assert_eq!(opens[0].open(), Money::parse("400").unwrap());

    // 取消核销后恢复
    let recs = list_for_entry(&db, e_ar).unwrap();
    unsettle(&db, recs[0].id).unwrap();
    assert_eq!(settled_of(db.conn(), e_ar).unwrap(), Money::ZERO);
}

#[test]
fn entry_rewrite_clears_dangling_refs() {
    let db = tmpdb("dangling");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let (vid, e_ar) = ar_voucher(&db, p, d, 1, "C01", "1000", true, "600101");
    // 收款凭证（贷 112201），提供同科目的核销方分录
    let (_, e_pay) = ar_voucher(&db, p, d, 2, "C01", "600", false, "100201");

    settle(&db, e_ar, e_pay, Money::parse("600").unwrap(), "u1").unwrap();
    // 银行勾对也挂在将被重写的这张凭证的分录上
    db.conn()
            .execute(
                "INSERT INTO bank_statement(period,account_code,biz_date,summary,settle_no,debit,credit,balance,entry_id,matched_at,matched_by)
                 VALUES(?1,'112201','2026-01-05','测试','','1000','0','0',?2,'now','u')",
                rusqlite::params![p.ymm(), e_ar],
            )
            .unwrap();
    assert_eq!(list_for_entry(&db, e_ar).unwrap().len(), 1);

    // 反记账后重写凭证（save 会整表 DELETE 旧分录再插入）：
    // 核销记录应级联删除、银行勾对应置空，不能留下指向已删分录的悬空引用
    crate::vouchers::unpost(&db, vid).unwrap();
    let mut v = crate::vouchers::get(&db, vid).unwrap().unwrap();
    v.memo = "改过摘要".to_string();
    crate::vouchers::save(&db, &mut v).unwrap();

    assert_eq!(
        list_for_entry(&db, e_ar).unwrap().len(),
        0,
        "重写分录后核销记录不应悬空"
    );
    let linked: Option<i64> = db
        .conn()
        .query_row(
            "SELECT entry_id FROM bank_statement WHERE period=?1 AND account_code='112201'",
            rusqlite::params![p.ymm()],
            |r| r.get(0),
        )
        .unwrap();
    assert!(linked.is_none(), "银行勾对应被置空");
}

#[test]
fn cross_account_rejected() {
    let db = tmpdb("cross");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let (_, e1) = ar_voucher(&db, p, d, 1, "C01", "100", true, "600101");
    let (_, e2) = ar_voucher(&db, p, d, 2, "C02", "100", false, "100201");
    // 往来单位不同
    assert!(settle(&db, e1, e2, Money::parse("100").unwrap(), "u").is_err());
}

/// 带差异（抹零）的核销：应收 1000，实收 998，差 2 记入差异科目，
/// 被核销方**全额结清**，差异落到损益，账龄不挂尾。
#[test]
fn settle_with_diff_writes_off_and_clears() {
    let db = tmpdb("diff");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let (_, e_ar) = ar_voucher(&db, p, d, 1, "C01", "1000", true, "600101");
    let (_, e_cash) = ar_voucher(&db, p, d, 2, "C01", "998", false, "100201");

    let r = settle_with_diff(
        &db,
        e_ar,
        e_cash,
        Money::parse("998").unwrap(),
        Money::parse("2").unwrap(),
        "660305",
        "u",
    )
    .unwrap();
    let dv_id = r.diff_voucher_id.expect("有差异必须生成差异凭证");
    // 应收全额结清（998 实收 + 2 差异）
    let left = open_entries(&db, "1122", p, false)
        .unwrap()
        .iter()
        .fold(Money::ZERO, |a, e| a + e.open());
    assert_eq!(left, Money::ZERO, "应收应被全额结清");
    // 差异凭证：借 660305 / 贷 112201（带同往来单位），金额 2
    let dv = crate::vouchers::get(&db, dv_id).unwrap().unwrap();
    assert_eq!(dv.entries.len(), 2);
    let debit = dv.entries.iter().find(|e| e.debit > Money::ZERO).unwrap();
    assert_eq!(debit.account_code, "660305");
    assert_eq!(debit.debit, Money::parse("2").unwrap());
    let credit = dv.entries.iter().find(|e| e.credit > Money::ZERO).unwrap();
    assert_eq!(credit.account_code, "112201");
    assert_eq!(
        credit.aux.customer.as_deref(),
        Some("C01"),
        "差异分录必须带同往来单位，否则核销对不上"
    );

    // 差异为 0：退化成普通核销，不生成差异凭证（凭证号跳开，避免与差异凭证撞号）
    let (_, e_ar2) = ar_voucher(&db, p, d, 10, "C02", "500", true, "600101");
    let (_, e_cash2) = ar_voucher(&db, p, d, 11, "C02", "500", false, "100201");
    let r0 = settle_with_diff(
        &db,
        e_ar2,
        e_cash2,
        Money::parse("500").unwrap(),
        Money::ZERO,
        "",
        "u",
    )
    .unwrap();
    assert!(r0.diff_voucher_id.is_none(), "无差异不应生成凭证");
}

/// 差异核销的守卫：负差异 / 空或不存在或同往来的差异科目 / 超额，全部拦下且整单回滚。
#[test]
fn settle_with_diff_guards() {
    let db = tmpdb("diffguard");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let (_, e_ar) = ar_voucher(&db, p, d, 1, "C01", "1000", true, "600101");
    let (_, e_cash) = ar_voucher(&db, p, d, 2, "C01", "998", false, "100201");
    let (a, dv) = (Money::parse("998").unwrap(), Money::parse("2").unwrap());
    assert!(settle_with_diff(
        &db,
        e_ar,
        e_cash,
        a,
        Money::parse("-1").unwrap(),
        "660305",
        "u"
    )
    .is_err());
    assert!(settle_with_diff(&db, e_ar, e_cash, a, dv, "", "u").is_err());
    assert!(settle_with_diff(&db, e_ar, e_cash, a, dv, "999999", "u").is_err());
    assert!(settle_with_diff(&db, e_ar, e_cash, a, dv, "112201", "u").is_err());
    // 998 + 100 = 1098 > 未核销 1000
    assert!(settle_with_diff(
        &db,
        e_ar,
        e_cash,
        a,
        Money::parse("100").unwrap(),
        "660305",
        "u"
    )
    .is_err());
    // 全部失败：一条核销记录都不该留下（整单回滚，不残留差异凭证与核销）
    assert!(
        list(&db, "1122").unwrap().is_empty(),
        "失败应整单回滚，不得残留核销记录"
    );
}

/// 建一张已记账的应付凭证：贷 220201（供应商），借 counterpart。返回 (凭证 id, 220201 分录 id)。
fn ap_voucher(
    db: &Db,
    date: NaiveDate,
    no: i32,
    sup: &str,
    amount: &str,
    counterpart: &str,
) -> (i64, i64) {
    let mut v = Voucher::new(Period::from_date(date), date, "记", no);
    let a = Money::parse(amount).unwrap();
    v.push_entry(Entry {
        debit: a,
        ..Entry::new(1, counterpart, "付款")
    });
    v.push_entry(Entry {
        credit: a,
        aux: AuxRef {
            supplier: Some(sup.into()),
            ..Default::default()
        },
        ..Entry::new(2, "220201", "应付挂账")
    });
    let vid = crate::vouchers::save(db, &mut v).unwrap();
    crate::vouchers::post(db, vid, "u").unwrap();
    let ids = crate::vouchers::entries_of(db, vid).unwrap();
    (vid, ids[1].id)
}

/// 代收代付：A 打来的钱清的是 B 的货款——**同科目（应收）跨往来单位**。
///
/// 普通核销要求同科目同辅助，这里两边客户不同（B vs A），只能靠过渡凭证接上：
/// 过渡 `借 应收-A / 贷 应收-B`，与 A 那张收款合起来净额就是 `借 银行 / 贷 应收-B`。
#[test]
fn settle_cross_跨往来单位代收代付() {
    let db = tmpdb("cross_pay");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
    // B 的货款：借 应收-B 1000
    let e_b = ar_voucher(&db, p, d, 1, "B01", "1000", true, "600101").1;
    // A 打来的钱记在 A 名下：贷 应收-A 1000
    let e_a = ar_voucher(&db, p, d, 2, "A01", "1000", false, "1001").1;

    let r = settle_cross(&db, e_b, e_a, Money::parse("1000").unwrap(), d, "u").unwrap();
    assert_eq!(r.settle_ids.len(), 2, "两侧各落一条核销记录");
    let vid = r.bridge_voucher_id.expect("跨往来单位必须出过渡凭证");

    // 过渡凭证：借 应收-A / 贷 应收-B
    let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
    assert_eq!(v.entries.len(), 2);
    let debit = v.entries.iter().find(|e| e.debit > Money::ZERO).unwrap();
    assert_eq!(debit.account_code, "112201");
    assert_eq!(debit.aux.customer.as_deref(), Some("A01"));
    let credit = v.entries.iter().find(|e| e.credit > Money::ZERO).unwrap();
    assert_eq!(credit.account_code, "112201");
    assert_eq!(credit.aux.customer.as_deref(), Some("B01"));

    // 两边同时清
    let left = open_entries(&db, "1122", p, false).unwrap();
    assert!(
        left.is_empty(),
        "互抵后应收应全清：{:?}",
        left.iter()
            .map(|e| (e.entry_id, e.open().fmt_money()))
            .collect::<Vec<_>>()
    );
}

/// 三方抵账：客户 A 欠我们、我们欠供应商 B，A 直接抵掉 B——**跨科目**。
/// 过渡 `借 应付-B / 贷 应收-A`（与往来对冲单同口径，只是这里按分录精确挑）。
#[test]
fn settle_cross_跨科目三方抵账() {
    let db = tmpdb("cross_three");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    let e_ar = ar_voucher(&db, p, d, 1, "A01", "1000", true, "600101").1;
    let e_ap = ap_voucher(&db, d, 2, "S01", "600", "1001").1;

    let r = settle_cross(&db, e_ar, e_ap, Money::parse("600").unwrap(), d, "u").unwrap();
    let vid = r.bridge_voucher_id.expect("跨科目必须出过渡凭证");
    let v = crate::vouchers::get(&db, vid).unwrap().unwrap();
    let debit = v.entries.iter().find(|e| e.debit > Money::ZERO).unwrap();
    assert_eq!(debit.account_code, "220201");
    assert_eq!(debit.aux.supplier.as_deref(), Some("S01"));
    let credit = v.entries.iter().find(|e| e.credit > Money::ZERO).unwrap();
    assert_eq!(credit.account_code, "112201");
    assert_eq!(credit.aux.customer.as_deref(), Some("A01"));

    // 应收剩 400、应付全清
    let ar = open_entries(&db, "1122", p, false)
        .unwrap()
        .iter()
        .fold(Money::ZERO, |a, e| a + e.open());
    assert_eq!(ar, Money::parse("400").unwrap(), "三方抵账后应收应剩 400");
    let ap = open_entries(&db, "2202", p, false).unwrap();
    assert!(ap.is_empty(), "应付应被全额冲销，实际 {} 笔", ap.len());
}

/// 同科目同辅助：本就是普通核销，不该出过渡凭证（省一张多余凭证）。
#[test]
fn settle_cross_同往来不需要过渡凭证() {
    let db = tmpdb("cross_same");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
    let e_ar = ar_voucher(&db, p, d, 1, "C01", "1000", true, "600101").1;
    let e_pre = ar_voucher(&db, p, d, 2, "C01", "300", false, "1001").1;

    let r = settle_cross(&db, e_ar, e_pre, Money::parse("300").unwrap(), d, "u").unwrap();
    assert!(r.bridge_voucher_id.is_none(), "同往来不该生成过渡凭证");
    assert_eq!(r.settle_ids.len(), 1);
    let left = open_entries(&db, "1122", p, false)
        .unwrap()
        .iter()
        .fold(Money::ZERO, |a, e| a + e.open());
    assert_eq!(left, Money::parse("700").unwrap(), "应收应剩 700");
}

/// 互抵守卫：自核自 / 零额 / 方向反 / 超额 / 草稿，全部拦下，且失败不残留任何东西。
#[test]
fn settle_cross_guards() {
    let db = tmpdb("crossguard");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 10).unwrap();
    let e_b = ar_voucher(&db, p, d, 1, "B01", "1000", true, "600101").1;
    let e_a = ar_voucher(&db, p, d, 2, "A01", "1000", false, "1001").1;
    let e_dr = ar_voucher(&db, p, d, 3, "C01", "1000", true, "600101").1;
    let e_cr = ar_voucher(&db, p, d, 4, "D01", "1000", false, "1001").1;
    let full = Money::parse("1000").unwrap();
    let over = Money::parse("1000.01").unwrap();

    // 自核自
    assert!(settle_cross(&db, e_b, e_b, full, d, "u").is_err());
    // 零额 / 负额
    assert!(settle_cross(&db, e_b, e_a, Money::ZERO, d, "u").is_err());
    // 方向反了：把贷方当借方、借方当贷方
    assert!(settle_cross(&db, e_a, e_b, full, d, "u").is_err());
    // 两边同向（都是借方 / 都是贷方）
    assert!(settle_cross(&db, e_b, e_dr, full, d, "u").is_err());
    assert!(settle_cross(&db, e_a, e_cr, full, d, "u").is_err());
    // 超额
    assert!(settle_cross(&db, e_b, e_a, over, d, "u").is_err());
    // 草稿（未记账）不能互抵
    let (_, e_draft) = ar_voucher_draft(&db, d, 9, "E01", "1000", true, "600101");
    assert!(settle_cross(&db, e_draft, e_a, full, d, "u").is_err());

    // 全部失败：一条核销记录都不该留下，也不该留下过渡凭证
    assert!(
        list(&db, "1122").unwrap().is_empty(),
        "失败应整单回滚，不得残留核销记录"
    );
    let draft_cnt: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM voucher WHERE memo LIKE '往来互抵%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(draft_cnt, 0, "失败不得留下过渡凭证");
}

#[test]
fn auto_settle_exact_then_fifo() {
    let db = tmpdb("auto");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    // 两笔应收各 500，一笔收款 800
    ar_voucher(&db, p, d, 1, "C01", "500", true, "600101");
    ar_voucher(&db, p, d, 2, "C01", "500", true, "600101");
    ar_voucher(&db, p, d, 3, "C01", "800", false, "100201");

    let res = auto_settle(&db, "1122", p, Money::parse("0.01").unwrap(), "u").unwrap();
    assert!(res.pairs >= 2, "{res:?}");
    let opens = open_entries(&db, "1122", p, false).unwrap();
    let total: Money = opens.iter().map(|o| o.open()).sum();
    assert_eq!(total, Money::parse("200").unwrap());
}

/// 回归：尾数抹平失败时不得计数。
///
/// 旧写法 `let _ = settle(...); res.pairs += 1; res.written_off += 1;` 把错误丢掉
/// 照样计数，界面显示"已核销 N 笔 / 金额 X"，库里却没有那条核销记录
/// （本例 X 甚至超过实际核销总额）。
#[test]
fn auto_settle_does_not_count_failed_writeoff() {
    let db = tmpdb("auto_wo");
    let p = Period::new(2026, 1).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
    // 应收 100；收款 99.99；再一笔 0.005 的收款（尾数比应收的 0.01 尾差还小）
    let (_, e_ar) = ar_voucher(&db, p, d, 1, "C01", "100", true, "600101");
    ar_voucher(&db, p, d, 2, "C01", "99.99", false, "100201");
    ar_voucher(&db, p, d, 3, "C01", "0.005", false, "100201");

    let res = auto_settle(&db, "1122", p, Money::parse("0.01").unwrap(), "u").unwrap();
    assert_eq!(res.written_off, 0, "抹平失败不得计入 written_off：{res:?}");
    assert!(
        res.warnings.iter().any(|w| w.contains("抹平失败")),
        "抹平失败必须进 warnings：{res:?}"
    );
    // 计数必须与库里的核销记录一致
    let really = settled_of(db.conn(), e_ar).unwrap();
    assert_eq!(
        res.amount, really,
        "统计金额必须等于实际核销额（回归前会把失败的抹平也算进去）：{res:?}"
    );
    assert_eq!(
        res.pairs,
        list(&db, "1122").unwrap().len(),
        "核销笔数必须等于实际记录数：{res:?}"
    );
}

/// settled_map 参数曾经是 [id1,id1,id2,id2…] 交错排列，而 SQL 里两个 IN
/// 各吃连续 N 个 —— from 侧永远漏后一半、to 侧永远漏前一半。
/// 两行以内碰巧全对（N=1 时两边吃到同一个 id），三行以上已配平的分录
/// 在循环视图里还是"未清"，自动核销第二轮就去重复配，被超额校验拦下
/// （"核销金额 24700.00 超过核销方未核销额 0.00"）。
/// 触发条件是 to 侧分录的 id 排在第一位 —— 现实里就是"先收款、后开票"
/// （氧化厂演示账：11-03 收 68000，11-04 才开 92700 发票），稳定复现。
#[test]
fn settled_map_三行以上双向都计数() {
    let db = tmpdb("settle_map_3rows");
    let p = Period::new(2026, 1).unwrap();
    // 先收款 68000、再开票 92700、再收款 50000，同客户
    let (_, e_r68) = ar_voucher(
        &db,
        p,
        NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(),
        1,
        "C01",
        "68000",
        false,
        "1001",
    );
    let (_, e_inv) = ar_voucher(
        &db,
        p,
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
        2,
        "C01",
        "92700",
        true,
        "600101",
    );
    let (_, e_r50) = ar_voucher(
        &db,
        p,
        NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
        3,
        "C01",
        "50000",
        false,
        "1001",
    );

    // 手工配第一对：发票 → 68000 收款
    settle(&db, e_inv, e_r68, Money::parse("68000").unwrap(), "u").unwrap();
    // 三个 id 必须都有数：修之前 to 侧的 e_r68 会漏掉（还是 0）
    let sm = settled_map(&db, &[e_r68, e_inv, e_r50]).unwrap();
    assert_eq!(sm.get(&e_inv), Some(&Money::parse("68000").unwrap()));
    assert_eq!(sm.get(&e_r68), Some(&Money::parse("68000").unwrap()));
    assert!(!sm.contains_key(&e_r50));

    // 自动核销必须一口气吃完发票剩下的 24700，而不是报错
    let res = auto_settle(&db, "112201", p, Money::ZERO, "u").unwrap();
    assert_eq!(res.amount, Money::parse("24700").unwrap());
    assert_eq!(
        settled_of(db.conn(), e_inv).unwrap(),
        Money::parse("92700").unwrap(),
        "发票应全额配平"
    );
}

#[test]
fn aging_report() {
    let db = tmpdb("aging");
    let p = Period::new(2026, 3).unwrap();
    let d1 = NaiveDate::from_ymd_opt(2026, 3, 20).unwrap();
    let d2 = NaiveDate::from_ymd_opt(2025, 6, 1).unwrap();
    ar_voucher(&db, p, d1, 1, "C01", "1000", true, "600101");
    ar_voucher(&db, p, d2, 2, "C01", "2000", true, "600101");

    let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
    let b = fincore::engine::aging::buckets_by_days();
    let lines = aging(&db, "1122", p, as_of, &b, None).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].total, Money::parse("3000").unwrap());
    assert_eq!(lines[0].amounts[0], Money::parse("1000").unwrap()); // 11 天
                                                                    // 2025-06-01 → 2026-03-31 约 303 天，落在 181-365 档
    assert_eq!(lines[0].amounts[4], Money::parse("2000").unwrap());
}

/// 到期月表：把未核销往来按「单据日期 + 账期」归到月份，应收应付一起出。
#[test]
fn ar_ap_due_schedule_groups_open_entries_by_due_month() {
    let db = tmpdb("ar_ap_due");
    let p = Period::new(2026, 3).unwrap();

    // 客户 C01 账期 30 天；C02 没设账期（货到即付）；供应商 S01 账期 60 天
    for (kind, code, name, days) in [
        (fincore::AuxKind::Customer, "C01", "客户甲", Some("30")),
        (fincore::AuxKind::Customer, "C02", "客户乙", None),
        (fincore::AuxKind::Supplier, "S01", "供应商丙", Some("60")),
    ] {
        let mut c = fincore::AuxEntity::new(kind, code, name);
        if let Some(d) = days {
            c.props.insert(
                fincore::auxiliary::prop::PAY_DAYS.to_string(),
                d.to_string(),
            );
        }
        crate::auxs::insert(&db, &c).unwrap();
    }

    // 应收 C01：1/5 卖 1000，账期 30 天 → 2/4 到期（落在 2026-02）
    ar_voucher(
        &db,
        p,
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
        1,
        "C01",
        "1000",
        true,
        "600101",
    );
    // 应收 C02：1/20 卖 500，没账期 → 1/20 到期（落在 2026-01）
    ar_voucher(
        &db,
        p,
        NaiveDate::from_ymd_opt(2026, 1, 20).unwrap(),
        2,
        "C02",
        "500",
        true,
        "600101",
    );
    // 应付 S01：1/10 买 800，账期 60 天 → 3/11 到期（落在 2026-03）
    ap_voucher(
        &db,
        NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
        3,
        "S01",
        "800",
        "1001",
    );

    let rows = ar_ap_due_schedule(&db, "1122", "2202", p).unwrap();
    assert_eq!(rows.len(), 3, "三笔未核销往来都要进月表");
    // 排序：按月，再按 kind（ar 在 ap 前），再按 party
    assert_eq!(rows[0].kind, "ar");
    assert_eq!(rows[0].party, "C02");
    assert_eq!(rows[0].month, "2026-01");
    assert_eq!(rows[0].due_date, "2026-01-20");
    assert_eq!(rows[0].amount, m("500"));

    assert_eq!(rows[1].kind, "ar");
    assert_eq!(rows[1].party, "C01");
    assert_eq!(rows[1].month, "2026-02");
    assert_eq!(rows[1].due_date, "2026-02-04");
    assert_eq!(rows[1].amount, m("1000"));

    assert_eq!(rows[2].kind, "ap");
    assert_eq!(rows[2].party, "S01");
    assert_eq!(rows[2].month, "2026-03");
    assert_eq!(rows[2].due_date, "2026-03-11");
    assert_eq!(rows[2].amount, m("800"));
}

#[test]
fn bad_debt_provision_generates_voucher() {
    let db = tmpdb("baddebt");
    let p = Period::new(2026, 3).unwrap();
    // 一笔 3 年以上的老应收 10000，一笔 1 年以内 2000
    ar_voucher(
        &db,
        p,
        NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        1,
        "C01",
        "2000",
        true,
        "600101",
    );
    ar_voucher(
        &db,
        p,
        NaiveDate::from_ymd_opt(2023, 1, 5).unwrap(),
        2,
        "C01",
        "10000",
        true,
        "600101",
    );

    let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
    let id = bad_debt_provision_voucher(&db, p, as_of, "u1")
        .unwrap()
        .expect("应生成坏账准备凭证");
    let v = crate::vouchers::get(&db, id).unwrap().unwrap();
    // 借 资产减值损失 / 贷 坏账准备，借贷平衡
    assert!(v.balanced());
    assert!(v.entries.iter().any(|e| e.account_code == "6701"));
    assert!(v.entries.iter().any(|e| e.account_code == "1231"));
    // 3-4 年老账 10000 × 30% + 1 年内新账 2000 × 5% = 3000 + 100 = 3100
    let credit: Money = v
        .entries
        .iter()
        .filter(|e| e.account_code == "1231")
        .map(|e| e.credit)
        .sum();
    assert_eq!(credit, Money::parse("3100").unwrap());
    assert_eq!(v.source, fincore::voucher::VoucherSource::Business);
}

#[test]
fn bad_debt_provision_adjusts_target_and_reverses() {
    let db = tmpdb("baddebt_diff");
    let p1 = Period::new(2026, 3).unwrap();
    let (_, e_new) = ar_voucher(
        &db,
        p1,
        NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        1,
        "C01",
        "2000",
        true,
        "600101",
    );
    let (_, e_old) = ar_voucher(
        &db,
        p1,
        NaiveDate::from_ymd_opt(2023, 1, 5).unwrap(),
        2,
        "C01",
        "10000",
        true,
        "600101",
    );
    let as_of = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
    let first = bad_debt_provision_voucher(&db, p1, as_of, "u1")
        .unwrap()
        .expect("首次应计提");
    // 计提出来的 1231 分录**必须挂上往来对象** —— 这是幂等性的机械保证：
    // 下一轮 existing 从 `e.aux_key` 读回上一轮的数，aux 没挂上就取不到，
    // 于是每轮都当成「还没提过」而全额重复计提（而且不报错）。
    //
    // 「为什么这条重要」不写在断言消息里，因为上面那句注释就是原因；
    // 这里只钉住事实：aux 存在，且值是**裸编码经 aux_key_frag 还原**的
    // `customer=C01` 形式，而不是反过来（`from_key` 吃裸编码会得到空 aux）。
    {
        let v = crate::vouchers::get(&db, first).unwrap().unwrap();
        let e1231 = v
            .entries
            .iter()
            .find(|e| e.account_code == "1231")
            .expect("应有 1231 分录");
        assert_eq!(
            e1231
                .aux
                .get(fincore::AuxKind::Customer)
                .map(|s| s.as_str()),
            Some("C01"),
            "计提的 1231 分录必须挂客户辅助核算，否则下一轮读不回来、幂等性断裂"
        );
    }
    // 目标未变：重复执行差额为 0，不再生成
    assert!(
        bad_debt_provision_voucher(&db, p1, as_of, "u1")
            .unwrap()
            .is_none(),
        "目标不变时不应重复计提"
    );

    // 期后全部收回 → 应提额降为 0，应自动冲回
    let p2 = Period::new(2026, 4).unwrap();
    let (_, e_pay) = ar_voucher(
        &db,
        p2,
        NaiveDate::from_ymd_opt(2026, 4, 10).unwrap(),
        3,
        "C01",
        "12000",
        false,
        "100201",
    );
    settle(&db, e_new, e_pay, Money::parse("2000").unwrap(), "u1").unwrap();
    settle(&db, e_old, e_pay, Money::parse("10000").unwrap(), "u1").unwrap();

    let rev =
        bad_debt_provision_voucher(&db, p2, NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(), "u1")
            .unwrap()
            .expect("应生成冲回凭证");
    let v = crate::vouchers::get(&db, rev).unwrap().unwrap();
    assert!(v.balanced());
    let debit: Money = v
        .entries
        .iter()
        .filter(|e| e.account_code == "1231")
        .map(|e| e.debit)
        .sum();
    let credit: Money = v
        .entries
        .iter()
        .filter(|e| e.account_code == "6701")
        .map(|e| e.credit)
        .sum();
    assert_eq!(debit, Money::parse("3100").unwrap());
    assert_eq!(credit, Money::parse("3100").unwrap());
    // 已到目标余额：再执行不再生成
    assert!(bad_debt_provision_voucher(
        &db,
        p2,
        NaiveDate::from_ymd_opt(2026, 4, 30).unwrap(),
        "u1"
    )
    .unwrap()
    .is_none());
}

#[test]
fn bad_debt_provision_skip_when_none() {
    let db = tmpdb("baddebt0");
    let p = Period::new(2026, 3).unwrap();
    let r = bad_debt_provision_voucher(&db, p, NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(), "u1")
        .unwrap();
    assert!(r.is_none(), "无应收时不应生成凭证");
}
/// 取某科目下一条未核销分录的 id（按借贷方向挑）。
fn open_id(db: &Db, upto: Period, want_debit: bool) -> i64 {
    let rows = open_entries(db, "1122", upto, false).unwrap();
    rows.iter()
        .find(|r| want_debit && r.debit > Money::ZERO || !want_debit && r.credit > Money::ZERO)
        .unwrap_or_else(|| {
            panic!(
                "找不到{}方的未核销分录",
                if want_debit { "借" } else { "贷" }
            )
        })
        .entry_id
}

/// 核销记录记在**核销发生的期间**，不是被核销单据的期间。
///
/// 回归：原来写的是 `f.period`，于是 2 月收 1 月的货款，台账里那笔记在 202601 ——
/// 而 1 月已经结账。既看不出钱是哪个月收的，又等于往已结账期间里塞了记录。
#[test]
fn settle_record_is_dated_by_the_settlement_period_not_the_invoice() {
    let db = mem();
    let p1 = Period::new(2026, 1).unwrap();
    let p2 = Period::new(2026, 2).unwrap();
    // 1 月：卖货挂应收 5000
    post_two(
        &db,
        p1,
        5,
        1,
        ("112201", m("5000"), Some(("customer", "C01"))),
        ("1001", m("5000"), None),
    );
    crate::periods::close(&db, p1, "tester", false).unwrap();
    // 2 月：客户还钱
    post_two(
        &db,
        p2,
        8,
        2,
        ("1001", m("5000"), None),
        ("112201", m("5000"), Some(("customer", "C01"))),
    );

    let ar = open_id(&db, p2, true);
    let cash = open_id(&db, p2, false);
    settle(&db, ar, cash, m("5000"), "tester").unwrap();

    let recs = list(&db, "112201").unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(
        recs[0].period, p2,
        "核销发生在 2 月，记录就该记 202602；记成 202601 等于往已结账期间塞记录"
    );
}

/// 已结账期间的核销记录不许撤销 —— 账龄是「截至某期」的数，
/// 结账后改掉它，等于让已经出过的报表对不上账。
#[test]
fn unsettle_refuses_records_in_a_closed_period() {
    let db = mem();
    let p = Period::new(2026, 1).unwrap();
    post_two(
        &db,
        p,
        5,
        1,
        ("112201", m("5000"), Some(("customer", "C01"))),
        ("1001", m("5000"), None),
    );
    post_two(
        &db,
        p,
        8,
        2,
        ("1001", m("5000"), None),
        ("112201", m("5000"), Some(("customer", "C01"))),
    );
    let ar = open_id(&db, p, true);
    let cash = open_id(&db, p, false);
    settle(&db, ar, cash, m("5000"), "tester").unwrap();
    let id = list(&db, "112201").unwrap()[0].id;

    crate::periods::close(&db, p, "tester", false).unwrap();
    let err = unsettle(&db, id).unwrap_err().to_string();
    assert!(err.contains("已结账"), "期望提示已结账，实际：{err}");
    assert_eq!(
        list(&db, "112201").unwrap().len(),
        1,
        "拒绝之后记录必须还在"
    );

    // 反结账之后可以撤
    crate::periods::unclose(&db, p, "tester").unwrap();
    unsettle(&db, id).unwrap();
    assert!(list(&db, "112201").unwrap().is_empty());
}

/// 部分撤销：5 万撤 2 万，剩 3 万，未核销额同步回来。
#[test]
fn unsettle_part_reduces_the_record_and_reopens_the_amount() {
    let db = mem();
    let p = Period::new(2026, 1).unwrap();
    post_two(
        &db,
        p,
        5,
        1,
        ("112201", m("50000"), Some(("customer", "C01"))),
        ("1001", m("50000"), None),
    );
    post_two(
        &db,
        p,
        8,
        2,
        ("1001", m("50000"), None),
        ("112201", m("50000"), Some(("customer", "C01"))),
    );
    let ar = open_id(&db, p, true);
    let cash = open_id(&db, p, false);
    settle(&db, ar, cash, m("50000"), "tester").unwrap();
    let id = list(&db, "112201").unwrap()[0].id;

    let left = unsettle_part(&db, id, m("20000")).unwrap();
    assert_eq!(left, m("30000"));
    assert_eq!(settled_of(db.conn(), ar).unwrap(), m("30000"));
    // 应收单重新露出 2 万
    let open = open_entries(&db, "1122", p, false).unwrap();
    let ar_row = open.iter().find(|r| r.entry_id == ar).unwrap();
    assert_eq!(ar_row.debit - ar_row.settled, m("20000"));

    // 撤到 0 就整条删掉
    let left = unsettle_part(&db, id, m("30000")).unwrap();
    assert_eq!(left, Money::ZERO);
    assert!(list(&db, "112201").unwrap().is_empty(), "撤到 0 应整条删除");
}

/// 撤销金额不能超过这条记录本身。
#[test]
fn unsettle_part_rejects_more_than_the_record() {
    let db = mem();
    let p = Period::new(2026, 1).unwrap();
    post_two(
        &db,
        p,
        5,
        1,
        ("112201", m("5000"), Some(("customer", "C01"))),
        ("1001", m("5000"), None),
    );
    post_two(
        &db,
        p,
        8,
        2,
        ("1001", m("5000"), None),
        ("112201", m("5000"), Some(("customer", "C01"))),
    );
    let ar = open_id(&db, p, true);
    let cash = open_id(&db, p, false);
    settle(&db, ar, cash, m("3000"), "tester").unwrap();
    let id = list(&db, "112201").unwrap()[0].id;
    let err = unsettle_part(&db, id, m("5000")).unwrap_err().to_string();
    assert!(err.contains("超过"), "期望提示超过记录金额，实际：{err}");
    assert_eq!(list(&db, "112201").unwrap()[0].amount, m("3000"));
}

/// v1.8.83 往来期初明细「修改」：改完要真的改对，且不能把导入校验绕过去。
///
/// 盯的失效形态：只改了金额没改别的（或反过来）、把原始录入人冲掉、
/// 允许把单据号改成撞上另一行（下次 CSV 重导会被幂等判重误跳过）、
/// 允许空客商/坏日期/负数金额（和导入口径不一致）、改不存在的行却报成功。
#[test]
fn arap_opening_update_validates_and_keeps_creator() {
    let db = tmpdb("arap_opening_update");
    let mk = |kind: &str, party: &str, doc: &str, amt: &str| ArapOpening {
        id: 0,
        kind: kind.to_string(),
        party_code: party.to_string(),
        party_name: "甲客户".to_string(),
        doc_no: doc.to_string(),
        doc_date: "2025-12-31".to_string(),
        amount: Money::parse(amt).unwrap(),
        memo: "老账".to_string(),
        created_by: String::new(),
    };
    let id = arap_opening_insert(&db, &mk("ar", "C01", "Q-001", "5000"), "importer").unwrap();
    let other = arap_opening_insert(&db, &mk("ar", "C01", "Q-002", "3000"), "importer").unwrap();

    // 正常修改：类型/客商/名称/单据号/日期/金额/备注全都落库
    let mut o = mk("ap", "S02", "Q-003", "1234.56");
    o.party_name = "乙供应商".to_string();
    o.doc_date = "2025-11-30".to_string();
    o.memo = "改过了".to_string();
    arap_opening_update(&db, id, &o).unwrap();
    let rows = arap_opening_list(&db, None).unwrap();
    let got = rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!(got.kind, "ap");
    assert_eq!(got.party_code, "S02");
    assert_eq!(got.party_name, "乙供应商");
    assert_eq!(got.doc_no, "Q-003");
    assert_eq!(got.doc_date, "2025-11-30");
    assert_eq!(got.amount, Money::parse("1234.56").unwrap());
    assert_eq!(got.memo, "改过了");
    assert_eq!(got.created_by, "importer", "原始录入人不该被表单冲掉");
    // 另一行不受影响
    assert_eq!(rows.iter().find(|r| r.id == other).unwrap().doc_no, "Q-002");

    // 改单据号不能撞上另一行（导入按 kind+客商+单据号幂等判重）
    let mut dup = mk("ap", "S02", "Q-003", "1234.56");
    dup.doc_no = "Q-002".to_string();
    dup.party_code = "C01".to_string();
    dup.kind = "ar".to_string();
    let err = arap_opening_update(&db, id, &dup).unwrap_err().to_string();
    assert!(err.contains("单据号"), "期望提示单据号重复，实际：{err}");

    // 逐条拒绝坏输入（与 CSV 导入同口径）
    let bad_kind = mk("AR", "C01", "Q-001", "5000");
    assert!(
        arap_opening_update(&db, id, &bad_kind).is_err(),
        "kind 只允许 ar/ap"
    );
    let mut bad_party = mk("ar", "C01", "Q-001", "5000");
    bad_party.party_code = "  ".to_string();
    assert!(
        arap_opening_update(&db, id, &bad_party).is_err(),
        "客商编码不能空"
    );
    let mut bad_doc = mk("ar", "C01", "Q-001", "5000");
    bad_doc.doc_no = String::new();
    assert!(
        arap_opening_update(&db, id, &bad_doc).is_err(),
        "单据号不能空"
    );
    let mut bad_date = mk("ar", "C01", "Q-001", "5000");
    bad_date.doc_date = "2025/12/31".to_string();
    assert!(
        arap_opening_update(&db, id, &bad_date).is_err(),
        "日期须 YYYY-MM-DD"
    );
    let bad_amount = mk("ar", "C01", "Q-001", "0");
    assert!(
        arap_opening_update(&db, id, &bad_amount).is_err(),
        "金额须正数"
    );
    let bad_negative = mk("ar", "C01", "Q-001", "-5");
    assert!(
        arap_opening_update(&db, id, &bad_negative).is_err(),
        "负数金额不允许"
    );

    // 坏输入不能把原来那行改坏
    let after = arap_opening_list(&db, None).unwrap();
    let still = after.iter().find(|r| r.id == id).unwrap();
    assert_eq!(still.amount, Money::parse("1234.56").unwrap());
    assert_eq!(still.doc_no, "Q-003");

    // 不存在的行要报错，不能静默成功
    let err = arap_opening_update(&db, 999_999, &mk("ar", "C09", "Q-999", "1"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("未找到"), "期望提示未找到，实际：{err}");
}
