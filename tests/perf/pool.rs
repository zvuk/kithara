#![cfg(feature = "perf")]

use std::{mem, thread};

use hotpath::HotpathGuardBuilder;
use kithara::platform::time::Instant;
use kithara_integration_tests::bufpool_ext::{Pools, pools};

#[hotpath::measure]
fn pool_get_put_cycle(pools: &Pools) {
    let buf = pools.get_with_len::<f32>(2048).expect("perf sample buffer");
    drop(buf);
}

#[hotpath::measure]
fn pool_thread_worker(pools: Pools, thread_id: usize, iterations: usize) {
    for i in 0..iterations {
        let mut buf = pools.get_with_len::<f32>(2048).expect("perf sample buffer");
        for (j, sample) in buf.iter_mut().enumerate() {
            *sample = (thread_id * iterations + i + j) as f32 * 0.001;
        }
        drop(buf);
    }
}

fn run_threaded(pools: Pools, num_threads: usize, iterations_per_thread: usize) {
    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let pools = pools.clone();
            thread::spawn(move || {
                pool_thread_worker(pools, thread_id, iterations_per_thread);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }
}

#[derive(Clone, Copy)]
enum PerfScenario {
    AllocationRate,
    MultiThreadContention,
    Scalability,
    SingleThreadGetPut,
}

#[kithara::test(flash(false))]
#[case("pool_single_thread", PerfScenario::SingleThreadGetPut)]
#[case("pool_multi_thread", PerfScenario::MultiThreadContention)]
#[case("pool_allocation_rate", PerfScenario::AllocationRate)]
#[case("pool_scalability", PerfScenario::Scalability)]
fn perf_pool_scenarios(#[case] label: &'static str, #[case] scenario: PerfScenario) {
    let _guard = HotpathGuardBuilder::new(label).build();
    match scenario {
        PerfScenario::SingleThreadGetPut => {
            let pools = pools();

            for _ in 0..100 {
                pool_get_put_cycle(&pools);
            }
            for _ in 0..10000 {
                pool_get_put_cycle(&pools);
            }

            println!("\n{:=<60}", "");
            println!("Single-threaded Pool Performance");
            println!("Iterations: 10000");
            println!("{:=<60}\n", "");
        }
        PerfScenario::MultiThreadContention => {
            let pools = pools();
            let num_threads = 8;
            let iterations_per_thread = 1000;
            run_threaded(pools, num_threads, iterations_per_thread);

            println!("\n{:=<60}", "");
            println!("Multi-threaded Pool Contention ({} threads)", num_threads);
            println!("Total iterations: {}", num_threads * iterations_per_thread);
            println!("{:=<60}\n", "");
        }
        PerfScenario::AllocationRate => {
            let pools = pools();
            hotpath::measure_block!("allocation_from_empty", {
                for _ in 0..1000 {
                    let buf = pools.get_with_len::<f32>(2048).expect("perf sample buffer");
                    mem::forget(buf);
                }
            });

            println!("\n{:=<60}", "");
            println!("Allocation Rate (empty pool)");
            println!("Allocations: 1000");
            println!("{:=<60}\n", "");
        }
        PerfScenario::Scalability => {
            drop(_guard);
            let thread_counts = [1, 2, 4, 8, 16];
            let iterations_per_thread = 1000;
            for &num_threads in &thread_counts {
                let scenario_label =
                    Box::leak(format!("pool_scalability_{}", num_threads).into_boxed_str());
                let _guard = HotpathGuardBuilder::new(scenario_label).build();
                let pools = pools();
                let start = Instant::now();

                run_threaded(pools, num_threads, iterations_per_thread);

                let elapsed = start.elapsed();
                let total_ops = num_threads * iterations_per_thread;
                let ops_per_sec = total_ops as f64 / elapsed.as_secs_f64();

                println!("\n{:=<60}", "");
                println!(
                    "Threads: {}, Ops/thread: {}",
                    num_threads, iterations_per_thread
                );
                println!("Total ops: {}, Elapsed: {:.2?}", total_ops, elapsed);
                println!("Ops/sec: {:.0}", ops_per_sec);
                println!("{:=<60}\n", "");
            }
        }
    }
}
