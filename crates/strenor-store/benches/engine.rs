//! Reproducible micro-benchmarks for the pure engine (no NAPI/FFI overhead).
//!
//! Run with `cargo bench -p strenor-store`. These measure the Rust core in
//! isolation, so results reflect the data structures and locking, not the
//! JavaScript boundary. Numbers are only meaningful compared against each other
//! on the same machine — never quote them as absolute throughput without stating
//! the hardware and rustc version.

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use std::hint::black_box;
use strenor_store::Store;

fn bench_kv(c: &mut Criterion) {
    let mut group = c.benchmark_group("kv");

    group.bench_function("set", |b| {
        let store = Store::new();
        let mut i = 0u64;
        b.iter(|| {
            store
                .set(format!("k{i}"), black_box(vec![1, 2, 3, 4]), None)
                .unwrap();
            i += 1;
        });
    });

    group.bench_function("get_hit", |b| {
        let store = Store::new();
        store.set("key".into(), vec![1, 2, 3, 4], None).unwrap();
        b.iter(|| black_box(store.get(black_box("key")).unwrap()));
    });

    group.bench_function("get_miss", |b| {
        let store = Store::new();
        b.iter(|| black_box(store.get(black_box("absent")).unwrap()));
    });

    group.finish();
}

fn bench_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("list");

    group.bench_function("push_back", |b| {
        let store = Store::new();
        b.iter(|| {
            store
                .push_back(black_box("q"), black_box(vec![0u8; 16]))
                .unwrap()
        });
    });

    group.bench_function("enqueue_dequeue", |b| {
        let store = Store::new();
        b.iter(|| {
            store.push_back("q", vec![0u8; 16]).unwrap();
            black_box(store.pop_front("q").unwrap());
        });
    });

    group.finish();
}

fn bench_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("hash");

    group.bench_function("hset", |b| {
        let store = Store::new();
        let mut i = 0u64;
        b.iter(|| {
            store
                .hset("h", format!("f{i}"), black_box(vec![7u8; 8]))
                .unwrap();
            i += 1;
        });
    });

    group.bench_function("hget", |b| {
        let store = Store::new();
        store.hset("h", "field".into(), vec![7u8; 8]).unwrap();
        b.iter(|| black_box(store.hget(black_box("h"), black_box("field")).unwrap()));
    });

    group.finish();
}

fn bench_zset(c: &mut Criterion) {
    let mut group = c.benchmark_group("zset");

    // zadd into a pre-populated set: the interesting cost is the ordered insert.
    group.bench_function("zadd_1k", |b| {
        b.iter_batched(
            || {
                let store = Store::new();
                for i in 0..1000u64 {
                    store
                        .zadd("lb", i as f64, format!("m{i}").into_bytes())
                        .unwrap();
                }
                store
            },
            |store| {
                store
                    .zadd("lb", black_box(500.5), black_box(b"new".to_vec()))
                    .unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("zrange_top10", |b| {
        let store = Store::new();
        for i in 0..1000u64 {
            store
                .zadd("lb", i as f64, format!("m{i}").into_bytes())
                .unwrap();
        }
        b.iter(|| black_box(store.zrange(black_box("lb"), -10, -1).unwrap()));
    });

    group.finish();
}

fn bench_transaction(c: &mut Criterion) {
    let mut group = c.benchmark_group("transaction");

    // Commit cost includes the rollback snapshot taken at begin — this is the
    // O(state size) behaviour we documented, so we measure it at a real size.
    group.bench_function("commit_10_writes_over_1k_state", |b| {
        b.iter_batched(
            || {
                let store = Store::new();
                for i in 0..1000u64 {
                    store.set(format!("k{i}"), vec![0u8; 8], None).unwrap();
                }
                store
            },
            |store| {
                store.tx_begin().unwrap();
                for i in 0..10u64 {
                    store.set(format!("tx{i}"), vec![1u8; 8], None).unwrap();
                }
                store.tx_commit().unwrap();
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_kv,
    bench_list,
    bench_hash,
    bench_zset,
    bench_transaction
);
criterion_main!(benches);
