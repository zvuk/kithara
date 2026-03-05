#![forbid(unsafe_code)]

use std::{thread, time::Duration};

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use kithara_bufpool::{BytePool, PcmPool, Pool, internal};

fn run_threaded_get_put(pool: BytePool, threads: usize, iters_per_thread: usize) {
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let pool_clone = pool.clone();
            thread::spawn(move || {
                for _ in 0..iters_per_thread {
                    let buf = pool_clone.get_with(|b| b.resize(4 * 1024, 0));
                    black_box(buf.len());
                }
            })
        })
        .collect();

    for handle in handles {
        if let Err(e) = handle.join() {
            panic!("threaded get/put worker panicked: {e:?}");
        }
    }
}

fn bench_get_put_single_thread(c: &mut Criterion) {
    let pool = BytePool::with_byte_budget(1024, 0, 256 * 1024 * 1024);
    let mut group = c.benchmark_group("bufpool_get_put");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));

    group.bench_function("single_thread_cycle_u8", |b| {
        b.iter(|| {
            let buf = pool.get_with(|inner| inner.resize(4 * 1024, 0));
            black_box(buf.len());
        });
    });

    group.finish();
}

fn bench_get_put_multi_thread(c: &mut Criterion) {
    let pool = BytePool::with_byte_budget(1024, 0, 256 * 1024 * 1024);
    let mut group = c.benchmark_group("bufpool_get_put");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));

    group.bench_function("multi_thread_contention_u8", |b| {
        b.iter(|| run_threaded_get_put(pool.clone(), 8, 256));
    });

    group.finish();
}

fn bench_ensure_len(c: &mut Criterion) {
    let byte_pool = BytePool::with_byte_budget(1024, 0, 256 * 1024 * 1024);
    let pcm_pool = PcmPool::new(128, 200_000);
    let mut group = c.benchmark_group("bufpool_ensure_len");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));

    group.bench_function("ensure_len_u8_64k", |b| {
        b.iter(|| {
            let mut buf = byte_pool.get();
            if let Err(_e) = buf.ensure_len(64 * 1024) {
                panic!("ensure_len(u8) should not exhaust budget in benchmark");
            }
            black_box(buf.len());
        });
    });

    group.bench_function("ensure_len_f32_16k", |b| {
        b.iter(|| {
            let mut buf = pcm_pool.get();
            if let Err(_e) = buf.ensure_len(16 * 1024) {
                panic!("ensure_len(f32) should not exhaust budget in benchmark");
            }
            black_box(buf.len());
        });
    });

    group.finish();
}

fn bench_cross_shard_steal(c: &mut Criterion) {
    let pool = Pool::<32, Vec<u8>>::with_byte_budget(1024, 0, 256 * 1024 * 1024);
    let home = internal::shard_index(&pool);
    let donor = (home + 1) % 32;

    let mut group = c.benchmark_group("bufpool_steal");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));

    group.bench_function("probe_plus_one", |b| {
        b.iter_batched(
            || {
                let mut seed = Vec::new();
                seed.resize(4 * 1024, 1);
                internal::put(&pool, seed, donor);
            },
            |_| {
                let buf = pool.get();
                black_box(buf.len());
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_pre_warm_get_cycle(c: &mut Criterion) {
    let pool = BytePool::with_byte_budget(1024, 0, 256 * 1024 * 1024);
    pool.pre_warm(256, |b| b.resize(4 * 1024, 0));

    let mut group = c.benchmark_group("bufpool_prewarm");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));

    group.bench_function("prewarm_then_get_u8", |b| {
        b.iter(|| {
            let buf = pool.get_with(|inner| inner.resize(4 * 1024, 0));
            black_box(buf.len());
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_get_put_single_thread,
    bench_get_put_multi_thread,
    bench_ensure_len,
    bench_cross_shard_steal,
    bench_pre_warm_get_cycle
);
criterion_main!(benches);
