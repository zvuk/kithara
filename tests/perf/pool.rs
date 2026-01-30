//! Performance tests for buffer pool.
//!
//! Run with: `cargo test --test pool --features perf --release`

#![cfg(feature = "perf")]

use kithara_bufpool::{pcm_pool, PcmPool};
use std::sync::Arc;
use std::thread;

#[hotpath::measure]
fn pool_get_put_cycle(pool: &PcmPool) {
    let buf = pool.get_with(|b| {
        b.clear();
        b.resize(2048, 0.0);
    });
    drop(buf);
}

#[test]
#[ignore]
fn perf_pool_single_thread_get_put() {
    let _guard = hotpath::GuardBuilder::new("pool_single_thread").build();
    let pool = pcm_pool();

    // Warm-up
    for _ in 0..100 {
        pool_get_put_cycle(&pool);
    }

    // Measured run
    for _ in 0..10000 {
        pool_get_put_cycle(&pool);
    }

    println!("\n{:=<60}", "");
    println!("Single-threaded Pool Performance");
    println!("Iterations: 10000");
    println!("{:=<60}\n", "");
}

#[hotpath::measure]
fn pool_thread_worker(pool: Arc<PcmPool>, thread_id: usize, iterations: usize) {
    for i in 0..iterations {
        let buf = pool.get_with(|b| {
            b.clear();
            b.resize(2048, 0.0);
            // Simulate work
            for j in 0..2048 {
                b[j] = (thread_id * iterations + i + j) as f32 * 0.001;
            }
        });
        drop(buf);
    }
}

#[test]
#[ignore]
fn perf_pool_multi_thread_contention() {
    let _guard = hotpath::GuardBuilder::new("pool_multi_thread").build();
    let pool = Arc::new(pcm_pool().clone());
    let num_threads = 8;
    let iterations_per_thread = 1000;

    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let pool_clone = Arc::clone(&pool);
            thread::spawn(move || {
                pool_thread_worker(pool_clone, thread_id, iterations_per_thread);
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    println!("\n{:=<60}", "");
    println!("Multi-threaded Pool Contention ({} threads)", num_threads);
    println!("Total iterations: {}", num_threads * iterations_per_thread);
    println!("{:=<60}\n", "");
}

#[test]
#[ignore]
fn perf_pool_allocation_rate() {
    let _guard = hotpath::GuardBuilder::new("pool_allocation_rate").build();
    let pool = pcm_pool();

    // Measure allocation rate when pool is empty
    hotpath::measure_block!("allocation_from_empty", {
        for _ in 0..1000 {
            let buf = pool.get_with(|b| {
                b.clear();
                b.resize(2048, 0.0);
            });
            // Don't return to pool (force allocation)
            std::mem::forget(buf);
        }
    });

    println!("\n{:=<60}", "");
    println!("Allocation Rate (empty pool)");
    println!("Allocations: 1000");
    println!("{:=<60}\n", "");
}
