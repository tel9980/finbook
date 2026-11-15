use chrono::NaiveDate;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fincore::voucher::{AuxRef, Entry, Voucher};
use fincore::{AuxEntity, AuxKind, BookOptions, Money, Period, Role, User};
use findb::Db;

fn bench_opts() -> BookOptions {
    BookOptions {
        start_period: Period::new(2026, 1).unwrap(),
        enable_audit: false,
        require_cashier: false,
        ..Default::default()
    }
}

/// 造一份「小厂规模」的仿真数据：N 个客户/供应商、N 张已确认且已发货的销售订单
/// （带未核销应收凭证）、N 张已确认采购订单（带未核销应付凭证）。
///
/// 数据规模是慢查询点测的前提 —— 空账套里 N+1 与全表扫都测不出来（几条数据怎么查都快）。
fn seed(db: &Db, n: i64) {
    let p = Period::new(2026, 6).unwrap();
    let d = NaiveDate::from_ymd_opt(2026, 6, 10).unwrap();
    for i in 0..n {
        let code = format!("C{i:04}");
        findb::auxs::insert(db, &AuxEntity::new(AuxKind::Customer, &code, "客户")).unwrap();
        let scode = format!("S{i:04}");
        findb::auxs::insert(db, &AuxEntity::new(AuxKind::Supplier, &scode, "供应商")).unwrap();
    }
    for i in 0..n {
        // 销售订单：确认 + 发货，再挂一张未核销的应收凭证（借 112201 / 贷 600101）
        let code = format!("C{i:04}");
        let mut so = findb::scm::SalesOrder::new(p, d, &code, "客户", "u");
        so.no = findb::scm::so_next_no(db, p).unwrap();
        so.lines.push(findb::scm::SoLine {
            id: 0,
            so_id: 0,
            item_code: "140501".into(),
            item_name: "成品".into(),
            qty_ordered: Money::parse("10").unwrap(),
            qty_shipped: Money::ZERO,
            unit_price: Money::parse("100").unwrap(),
            tax_rate: Money::ZERO,
            amount: Money::parse("1000").unwrap(),
            tax_amount: Money::ZERO,
            memo: String::new(),
        });
        let so_id = findb::scm::so_save(db, &mut so).unwrap();
        findb::scm::so_set_status(db, so_id, findb::scm::SoStatus::Confirmed).unwrap();
        findb::sales::so_shipment_add(db, so_id, p, d, Money::parse("6").unwrap(), "发").unwrap();
        // 销售出库（存货收发存/成本结转的数据源）
        findb::business::stock_insert(
            db,
            &findb::business::StockMove {
                id: 0,
                period: p,
                biz_date: d,
                kind: findb::business::StockKind::Sale,
                item: "140501".into(),
                warehouse: "主仓".into(),
                batch_no: String::new(),
                qty: Money::parse("-6").unwrap(),
                price: Money::ZERO,
                amount: Money::ZERO,
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        // 销项发票（开票汇总的数据源）
        findb::invoices::push_from_so(db, so_id, "u").unwrap();
        let mut v = Voucher::new(p, d, "记", (i + 1) as i32);
        v.push_entry(Entry {
            debit: Money::parse("1000").unwrap(),
            credit: Money::ZERO,
            aux: AuxRef {
                customer: Some(code.clone()),
                ..Default::default()
            },
            ..Entry::new(1, "112201", "应收")
        });
        v.push_entry(Entry {
            debit: Money::ZERO,
            credit: Money::parse("1000").unwrap(),
            aux: AuxRef::default(),
            ..Entry::new(2, "600101", "收入")
        });
        let vid = findb::vouchers::save(db, &mut v).unwrap();
        findb::vouchers::post(db, vid, "u").unwrap();

        // 采购订单：确认，再挂一张未核销的应付凭证（借 660201 / 贷 220201）
        let scode = format!("S{i:04}");
        let mut po = findb::scm::PurchaseOrder::new(p, d, &scode, "供应商", "u");
        po.no = findb::scm::po_next_no(db, p).unwrap();
        po.lines.push(findb::scm::PoLine {
            id: 0,
            po_id: 0,
            item_code: "140301".into(),
            item_name: "原料".into(),
            qty_ordered: Money::parse("10").unwrap(),
            qty_received: Money::ZERO,
            unit_price: Money::parse("80").unwrap(),
            tax_rate: Money::ZERO,
            amount: Money::parse("800").unwrap(),
            tax_amount: Money::ZERO,
            memo: String::new(),
        });
        let po_id = findb::scm::po_save(db, &mut po).unwrap();
        findb::scm::po_set_status(db, po_id, findb::scm::PoStatus::Confirmed).unwrap();
        // 采购入库（存货收发存/成本结转的数据源）
        findb::business::stock_insert(
            db,
            &findb::business::StockMove {
                id: 0,
                period: p,
                biz_date: d,
                kind: findb::business::StockKind::Purchase,
                item: "140301".into(),
                warehouse: "主仓".into(),
                batch_no: String::new(),
                qty: Money::parse("10").unwrap(),
                price: Money::parse("80").unwrap(),
                amount: Money::parse("800").unwrap(),
                voucher_id: None,
                memo: String::new(),
            },
        )
        .unwrap();
        // 进项发票（开票汇总的数据源）
        findb::invoices::push_from_po(db, po_id, "u").unwrap();
        let mut v2 = Voucher::new(p, d, "记", (n + i + 1) as i32);
        v2.push_entry(Entry {
            debit: Money::parse("800").unwrap(),
            credit: Money::ZERO,
            aux: AuxRef::default(),
            ..Entry::new(1, "660201", "采购")
        });
        v2.push_entry(Entry {
            debit: Money::ZERO,
            credit: Money::parse("800").unwrap(),
            aux: AuxRef {
                supplier: Some(scode),
                ..Default::default()
            },
            ..Entry::new(2, "220201", "应付")
        });
        let vid2 = findb::vouchers::save(db, &mut v2).unwrap();
        findb::vouchers::post(db, vid2, "u").unwrap();
    }
}

/// 回款看板（谁还欠我钱）—— 历史 N+1 重灾区，v1.8.104 起收款汇总已批量化。
fn bench_so_receivable(c: &mut Criterion) {
    let db = Db::in_memory(&bench_opts()).unwrap();
    seed(&db, 500);
    c.bench_function("so_receivable_500单", |b| {
        b.iter(|| black_box(findb::sales::so_receivable(black_box(&db)).unwrap()));
    });
}

/// 往来账龄 —— 全量未核销分录 + 账期补算。
fn bench_aging(c: &mut Criterion) {
    let db = Db::in_memory(&bench_opts()).unwrap();
    seed(&db, 500);
    let p = Period::new(2026, 6).unwrap();
    let buckets = fincore::engine::aging::buckets_by_days();
    c.bench_function("aging_500往来", |b| {
        b.iter(|| {
            black_box(
                findb::settle::aging(black_box(&db), "1122", p, p.last_day(), &buckets, None)
                    .unwrap(),
            )
        });
    });
}

/// 按月应收应付明细（到期月表）—— 应收 + 应付两张口径各跑一遍。
fn bench_due_schedule(c: &mut Criterion) {
    let db = Db::in_memory(&bench_opts()).unwrap();
    seed(&db, 500);
    let p = Period::new(2026, 6).unwrap();
    c.bench_function("due_schedule_500往来", |b| {
        b.iter(|| {
            black_box(findb::settle::ar_ap_due_schedule(black_box(&db), "1122", "2202", p).unwrap())
        });
    });
}

/// 工作台全量（卡片 + 待办 + 趋势）—— 一个页面串起所有热读路径。
fn bench_workbench_collect(c: &mut Criterion) {
    let db = Db::in_memory(&bench_opts()).unwrap();
    seed(&db, 500);
    let p = Period::new(2026, 6).unwrap();
    let admin = User::new("boss", "老板", Role::Admin);
    c.bench_function("workbench_collect_500往来", |b| {
        b.iter(|| {
            black_box(findb::workbench::collect(black_box(&db), black_box(&admin), p, 6).unwrap())
        });
    });
}

/// 月结向导（结账检查清单）—— 余额快照 + 试算平衡 + 损益行，月末热读。
fn bench_month_close(c: &mut Criterion) {
    let db = Db::in_memory(&bench_opts()).unwrap();
    seed(&db, 500);
    let p = Period::new(2026, 6).unwrap();
    c.bench_function("month_close_500往来", |b| {
        b.iter(|| black_box(findb::month_close::report(black_box(&db), p).unwrap()));
    });
}

/// 发票台账汇总（进项/销项金额与税额）—— 开票相关的报表热读。
fn bench_invoice_summary(c: &mut Criterion) {
    let db = Db::in_memory(&bench_opts()).unwrap();
    seed(&db, 500);
    c.bench_function("invoice_summary_500往来", |b| {
        b.iter(|| black_box(findb::invoices::summary(black_box(&db)).unwrap()));
    });
}

/// 存货收发存汇总（逐存货重放流水 + 计价）—— 库存报表热读。
fn bench_stock_summary(c: &mut Criterion) {
    let db = Db::in_memory(&bench_opts()).unwrap();
    seed(&db, 500);
    let p = Period::new(2026, 6).unwrap();
    c.bench_function("stock_summary_500往来", |b| {
        b.iter(|| {
            black_box(
                findb::business::stock_summary(
                    black_box(&db),
                    p,
                    fincore::engine::CostMethod::MonthAverage,
                )
                .unwrap(),
            )
        });
    });
}

/// 期末成本结转（逐存货算应结转成本与调整额）—— 成本结转报表热读。
fn bench_period_end_cost(c: &mut Criterion) {
    let db = Db::in_memory(&bench_opts()).unwrap();
    seed(&db, 500);
    let p = Period::new(2026, 6).unwrap();
    c.bench_function("period_end_cost_500往来", |b| {
        b.iter(|| black_box(findb::business::period_end_cost(black_box(&db), p, false).unwrap()));
    });
}

/// 工作台全量在 5000 单规模 —— 专门用来暴露「数据量一大就 O(n²)」的隐性退化。
fn bench_workbench_collect_5000(c: &mut Criterion) {
    let db = Db::in_memory(&bench_opts()).unwrap();
    seed(&db, 5000);
    let p = Period::new(2026, 6).unwrap();
    let admin = User::new("boss", "老板", Role::Admin);
    c.bench_function("workbench_collect_5000往来", |b| {
        b.iter(|| {
            black_box(findb::workbench::collect(black_box(&db), black_box(&admin), p, 6).unwrap())
        });
    });
}

criterion_group!(
    hot_reads,
    bench_so_receivable,
    bench_aging,
    bench_due_schedule,
    bench_workbench_collect,
    bench_month_close,
    bench_invoice_summary,
    bench_stock_summary,
    bench_period_end_cost,
    bench_workbench_collect_5000
);
criterion_main!(hot_reads);
