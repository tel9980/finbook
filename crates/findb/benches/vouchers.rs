use chrono::NaiveDate;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fincore::voucher::{Entry, Voucher};
use fincore::{BookOptions, Money, Period};
use findb::{vouchers, Db};

/// 构建一张 `line_cnt` 行的平衡凭证（借 1001，贷 1002）
fn make_voucher(line_cnt: usize) -> Voucher {
    let period = Period::new(2026, 1).unwrap();
    let mut v = Voucher::new(
        period,
        NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
        "记",
        1,
    );
    let half = line_cnt / 2;
    for i in 0..half {
        let mut e = Entry::new(
            (i * 2 + 1) as i32,
            "1001".to_string(),
            "bench 借方".to_string(),
        );
        e.debit = Money::parse("100.00").unwrap();
        v.push_entry(e);
    }
    for i in 0..(line_cnt - half) {
        let mut e = Entry::new(
            (i * 2 + 2) as i32,
            "1002".to_string(),
            "bench 贷方".to_string(),
        );
        e.credit = Money::parse("100.00").unwrap();
        v.push_entry(e);
    }
    v
}

/// 测试用账套参数（与 findb::tests::test_opts 保持一致）
fn bench_opts() -> BookOptions {
    BookOptions {
        start_period: Period::new(2026, 1).unwrap(),
        enable_audit: false,
        require_cashier: false,
        ..Default::default()
    }
}

/// 为 bench 准备一个已有基础数据的账套（内置科目、用户等）
fn bench_db() -> Db {
    Db::in_memory(&bench_opts()).unwrap()
}

fn save_small(c: &mut Criterion) {
    c.bench_function("vouchers_save_2行", |b| {
        b.iter(|| {
            let db = bench_db();
            let mut v = make_voucher(2);
            let id = vouchers::save(black_box(&db), black_box(&mut v)).unwrap();
            black_box(id);
        })
    });
}

fn save_medium(c: &mut Criterion) {
    c.bench_function("vouchers_save_20行", |b| {
        b.iter(|| {
            let db = bench_db();
            let mut v = make_voucher(20);
            let id = vouchers::save(black_box(&db), black_box(&mut v)).unwrap();
            black_box(id);
        })
    });
}

fn save_large(c: &mut Criterion) {
    c.bench_function("vouchers_save_100行", |b| {
        b.iter(|| {
            let db = bench_db();
            let mut v = make_voucher(100);
            let id = vouchers::save(black_box(&db), black_box(&mut v)).unwrap();
            black_box(id);
        })
    });
}

criterion_group!(vouchers, save_small, save_medium, save_large);
criterion_main!(vouchers);
