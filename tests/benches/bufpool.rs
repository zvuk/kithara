#![forbid(unsafe_code)]

use std::{hint::black_box, thread};

use criterion::{Criterion, criterion_group, criterion_main};
use kithara::{bufpool::PoolConfig, platform::time::Duration};
use kithara_integration_tests::bufpool_ext::{Pools, pools_with};

fn benchmark_pools() -> Pools {
    pools_with(
        256 * 1024 * 1024,
        PoolConfig::builder().max_buffers(1_024).build(),
        PoolConfig::builder()
            .max_buffers(128)
            .trim_capacity(200_000)
            .build(),
    )
}

fn run_threaded_get_put(pools: &Pools, threads: usize, iterations: usize) {
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let pools = pools.clone();
            thread::spawn(move || {
                for _ in 0..iterations {
                    let buffer = pools
                        .get_with_len::<u8>(4 * 1024)
                        .expect("benchmark budget is sufficient");
                    black_box(buffer.len());
                }
            })
        })
        .collect();

    for handle in handles {
        if let Err(error) = handle.join() {
            panic!("threaded get/put worker panicked: {error:?}");
        }
    }
}

fn bench_get_put_single_thread(c: &mut Criterion) {
    let pools = benchmark_pools();
    let mut group = c.benchmark_group("bufpool_get_put");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));

    group.bench_function("single_thread_cycle_u8", |b| {
        b.iter(|| {
            let buffer = pools
                .get_with_len::<u8>(4 * 1024)
                .expect("benchmark budget is sufficient");
            black_box(buffer.len());
        });
    });

    group.finish();
}

fn bench_get_put_multi_thread(c: &mut Criterion) {
    let pools = benchmark_pools();
    let mut group = c.benchmark_group("bufpool_get_put");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(8));

    group.bench_function("multi_thread_contention_u8", |b| {
        b.iter(|| run_threaded_get_put(&pools, 8, 256));
    });

    group.finish();
}

fn bench_ensure_len(c: &mut Criterion) {
    let pools = benchmark_pools();
    let mut group = c.benchmark_group("bufpool_ensure_len");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));

    group.bench_function("ensure_len_u8_64k", |b| {
        b.iter(|| {
            let buffer = pools
                .get_with_len::<u8>(64 * 1024)
                .expect("benchmark budget is sufficient");
            black_box(buffer.len());
        });
    });

    group.bench_function("ensure_len_f32_16k", |b| {
        b.iter(|| {
            let buffer = pools
                .get_with_len::<f32>(16 * 1024)
                .expect("benchmark budget is sufficient");
            black_box(buffer.len());
        });
    });

    group.finish();
}

fn bench_eager_get_cycle(c: &mut Criterion) {
    let pools = pools_with(
        256 * 1024 * 1024,
        PoolConfig::builder()
            .initial_buffers(256)
            .initial_capacity(4 * 1024)
            .max_buffers(1_024)
            .build(),
        PoolConfig::builder().max_buffers(128).build(),
    );
    let mut group = c.benchmark_group("bufpool_eager");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(6));

    group.bench_function("eager_get_u8", |b| {
        b.iter(|| {
            let buffer = pools.get::<u8>();
            black_box(buffer.capacity());
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_get_put_single_thread,
    bench_get_put_multi_thread,
    bench_ensure_len,
    bench_eager_get_cycle
);
criterion_main!(benches);
