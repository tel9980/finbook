use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fincore::{BookOptions, Period};
use findb::balances::{BalanceQuery, BalanceSnapshot};
use findb::Db;

fn bench_opts() -> BookOptions {
    BookOptions {
        start_period: Period::new(2026, 1).unwrap(),
        enable_audit: false,
        require_cashier: false,
        ..Default::default()
    }
}

fn bench_db() -> Db {
    Db::in_memory(&bench_opts()).unwrap()
}

/// 加载余额快照（全账套，无科目范围限制）
fn load_snapshot(c: &mut Criterion) {
    c.bench_function("balance_snapshot_load", |b| {
        b.iter(|| {
            let db = bench_db();
            let q = BalanceQuery::period(Period::new(2026, 12).unwrap());
            let snap = BalanceSnapshot::load(black_box(&db), black_box(&q)).unwrap();
            black_box(snap);
        })
    });
}

/// for_account：前缀预聚合查表（O(log R)）
fn for_account(c: &mut Criterion) {
    let db = bench_db();
    let q = BalanceQuery::period(Period::new(2026, 12).unwrap());
    let snap = BalanceSnapshot::load(&db, &q).unwrap();
    c.bench_function("balance_for_account", |b| {
        b.iter(|| {
            let row = snap.for_account(black_box("1001"), black_box(None));
            black_box(row);
        })
    });
}

criterion_group!(balances, load_snapshot, for_account);
criterion_main!(balances);
