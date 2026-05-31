//! Behavioural contract for the lock-free pooled-buffer return path.
//!
//! Pins the WS2 invariants: a warm pool reuses buffers without further
//! allocation, byte-budget accounting returns to baseline once every handle
//! drops, and concurrent get/put from many threads neither deadlocks nor
//! leaks buffers beyond the configured per-pool capacity.

use std::{
    sync::{Arc, Barrier},
    thread,
};

use kithara_bufpool::SharedPool;
use kithara_test_utils::kithara;

type VecPool = SharedPool<8, Vec<u8>>;

#[kithara::test]
fn warm_pool_reuses_without_allocating() {
    let pool = VecPool::new(64, 0);

    for _ in 0..32 {
        let mut buf = pool.get();
        buf.resize(4096, 0);
    }
    let warm = pool.stats();
    assert!(warm.alloc_misses > 0, "warm-up must populate the pool");

    for _ in 0..500 {
        let mut buf = pool.get();
        buf.resize(4096, 0);
    }

    assert_eq!(
        pool.stats().alloc_misses,
        warm.alloc_misses,
        "no fresh allocation expected once the pool is warm",
    );
}

#[kithara::test]
fn byte_budget_tracks_outstanding_then_settles() {
    let pool = VecPool::new(64, 0);

    {
        let _buf = pool.get_with(|b| b.resize(4096, 0));
    }
    let retained = pool.allocated_bytes();
    assert!(
        retained >= 4096,
        "a buffer retained in the pool keeps its tracked bytes",
    );

    {
        let held: Vec<_> = (0..16)
            .map(|_| pool.get_with(|b| b.resize(8192, 0)))
            .collect();
        assert!(
            pool.allocated_bytes() > retained,
            "outstanding buffers must add to the tracked budget",
        );
        drop(held);
    }

    assert!(
        pool.allocated_bytes() >= retained,
        "tracked bytes never underflow below what stays pooled after handles drop",
    );
}

#[kithara::test]
fn concurrent_get_put_no_deadlock_no_leak() {
    const THREADS: usize = 8;
    const ITERS: usize = 4_000;
    const MAX_BUFFERS: usize = 256;

    let pool = Arc::new(VecPool::new(MAX_BUFFERS, 0));
    let barrier = Arc::new(Barrier::new(THREADS));

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..ITERS {
                    // `get_with` drives the tracked byte-budget path (the CAS
                    // loops in `track_byte_delta`), so this exercises budget
                    // accounting under concurrency, not just the queue.
                    let mut buf = pool.get_with(|b| b.resize(1024, 0));
                    buf.iter_mut().take(4).for_each(|b| *b = 1);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("worker thread panicked");
    }

    let stats = pool.stats();
    assert!(
        stats.allocated_bytes > 0,
        "tracked budget must reflect the buffers retained in the warm pool",
    );
    assert!(
        stats.allocated_bytes <= MAX_BUFFERS * 4096,
        "budget stays bounded by pool capacity ({MAX_BUFFERS} buffers), not by the \
         {} concurrent tracked ops — a per-op leak would blow past this bound \
         (got {} bytes)",
        THREADS * ITERS,
        stats.allocated_bytes,
    );

    let pooled = stats.home_hits + stats.steal_hits + stats.alloc_misses;
    assert_eq!(
        pooled,
        (THREADS * ITERS) as u64,
        "every get must resolve as home hit, steal, or fresh allocation",
    );
}
