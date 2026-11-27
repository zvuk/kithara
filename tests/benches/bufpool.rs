#![forbid(unsafe_code)]

use std::{fs::OpenOptions, io::Write, thread};

use criterion::Criterion;
use kithara::{bufpool::PoolConfig, platform::time::Duration};
use kithara_integration_tests::bufpool_ext::{Pools, pools_with};

fn benchmark_pools() -> Pools {
    pools_with(
        256 * 1024 * 1024,
        PoolConfig::builder().max_buffers(1_024).build(),
        PoolConfig::builder().max_buffers(128).build(),
    )
}

fn eager_pools() -> Pools {
    pools_with(
        256 * 1024 * 1024,
        PoolConfig::builder()
            .initial_buffers(256)
            .initial_capacity(4 * 1024)
            .max_buffers(1_024)
            .build(),
        PoolConfig::builder().max_buffers(128).build(),
    )
}

fn run_threaded_get_put(pools: &Pools, threads: usize, iterations: usize) -> usize {
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let pools = pools.clone();
            thread::spawn(move || {
                for _ in 0..iterations {
                    let buffer = pools
                        .get_with_len::<u8>(4 * 1024)
                        .expect("benchmark budget is sufficient");
                    std::hint::black_box(buffer.len());
                }
            })
        })
        .collect();

    let joined = handles.len();
    for handle in handles {
        if let Err(error) = handle.join() {
            panic!("threaded get/put worker panicked: {error:?}");
        }
    }
    joined
}

fn warm_pools() -> Pools {
    let pools = benchmark_pools();
    let held: Vec<_> = (0..8)
        .map(|_| {
            pools
                .get_with_len::<u8>(4 * 1024)
                .expect("warm worker buffer")
        })
        .collect();
    std::hint::black_box(held.len());
    drop(held);
    get_u8(&pools, 64 * 1024);
    let buffer = pools
        .get_with_len::<f32>(16 * 1024)
        .expect("warm f32 buffer");
    std::hint::black_box(buffer.len());
    pools
}

struct MultiWork {
    pools: Pools,
    joined: usize,
}

fn multi_prepare() -> MultiWork {
    let pools = warm_pools();
    run_threaded_get_put(&pools, 8, 1);
    MultiWork { pools, joined: 0 }
}

fn get_u8(pools: &Pools, bytes: usize) {
    let buffer = pools
        .get_with_len::<u8>(bytes)
        .expect("benchmark budget is sufficient");
    std::hint::black_box(buffer.len());
}

#[metabench::benchmark(BUFPOOL_SINGLE, "bufpool_get_put", "single_thread_cycle_u8", gungraun_setup = warm_pools)]
fn single_case(pools: Pools) -> Pools {
    get_u8(&pools, 4 * 1024);
    pools
}

#[metabench::benchmark(BUFPOOL_MULTI, "bufpool_get_put", "multi_thread_contention_u8", gungraun_setup = multi_prepare, gungraun_teardown = observe_workers)]
fn multi_case(mut work: MultiWork) -> MultiWork {
    work.joined = run_threaded_get_put(&work.pools, 8, 256);
    work
}

fn observe_workers(pools: MultiWork) {
    let Some(path) = std::env::var_os("KITHARA_BENCH_OBSERVATIONS") else {
        return;
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open worker observations");
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "identity": "bufpool_get_put/multi_thread_contention_u8",
            "observation": {"worker": {"joined": pools.joined, "escaped": 0}},
        }),
    )
    .expect("write worker observation");
    file.write_all(b"\n").expect("terminate worker observation");
    drop(pools);
}

#[metabench::benchmark(BUFPOOL_ENSURE_U8, "bufpool_ensure_len", "ensure_len_u8_64k", gungraun_setup = warm_pools)]
fn ensure_u8_case(pools: Pools) -> Pools {
    get_u8(&pools, 64 * 1024);
    pools
}

#[metabench::benchmark(BUFPOOL_ENSURE_F32, "bufpool_ensure_len", "ensure_len_f32_16k", gungraun_setup = warm_pools)]
fn ensure_f32_case(pools: Pools) -> Pools {
    let buffer = pools
        .get_with_len::<f32>(16 * 1024)
        .expect("benchmark budget is sufficient");
    std::hint::black_box(buffer.len());
    pools
}

#[metabench::benchmark(BUFPOOL_EAGER, "bufpool_eager", "eager_get_u8", gungraun_setup = eager_pools)]
fn eager_case(pools: Pools) -> Pools {
    let buffer = pools.get::<u8>();
    std::hint::black_box(buffer.capacity());
    pools
}

fn criterion_benchmarks(c: &mut Criterion) {
    let pools = benchmark_pools();
    {
        let mut group = c.benchmark_group("bufpool_get_put");
        group.sample_size(20);
        group.measurement_time(Duration::from_secs(6));
        group.bench_function("single_thread_cycle_u8", |b| {
            b.iter(|| get_u8(&pools, 4 * 1024));
        });
        group.finish();
    }

    {
        let mut group = c.benchmark_group("bufpool_get_put");
        group.sample_size(10);
        group.measurement_time(Duration::from_secs(8));
        group.bench_function("multi_thread_contention_u8", |b| {
            b.iter(|| run_threaded_get_put(&pools, 8, 256));
        });
        group.finish();
    }

    {
        let mut group = c.benchmark_group("bufpool_ensure_len");
        group.sample_size(20);
        group.measurement_time(Duration::from_secs(6));
        group.bench_function("ensure_len_u8_64k", |b| {
            b.iter(|| get_u8(&pools, 64 * 1024));
        });
        group.bench_function("ensure_len_f32_16k", |b| {
            b.iter(|| {
                let buffer = pools
                    .get_with_len::<f32>(16 * 1024)
                    .expect("benchmark budget is sufficient");
                std::hint::black_box(buffer.len());
            });
        });
        group.finish();
    }

    let pools = eager_pools();
    {
        let mut group = c.benchmark_group("bufpool_eager");
        group.sample_size(20);
        group.measurement_time(Duration::from_secs(6));
        group.bench_function("eager_get_u8", |b| {
            b.iter(|| {
                let buffer = pools.get::<u8>();
                std::hint::black_box(buffer.capacity());
            });
        });
        group.finish();
    }
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [
        BUFPOOL_SINGLE,
        BUFPOOL_MULTI,
        BUFPOOL_ENSURE_U8,
        BUFPOOL_ENSURE_F32,
        BUFPOOL_EAGER,
    ],
    allocator = std::alloc::System,
);
