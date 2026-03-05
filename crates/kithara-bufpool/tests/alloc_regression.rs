use std::alloc::System;
#[cfg(not(target_arch = "wasm32"))]
use std::thread;

use kithara_bufpool::SharedPool;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

mod kithara {
    pub(crate) use kithara_test_macros::test;
}

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

// Allocation regression tests share a global allocator, so they must not run in parallel.

fn wait_for_allocator_quiet() {
    let mut stable_samples = 0usize;
    let mut prev = GLOBAL.stats();

    while stable_samples < 8 {
        #[cfg(not(target_arch = "wasm32"))]
        thread::yield_now();

        #[cfg(target_arch = "wasm32")]
        std::hint::spin_loop();

        let next = GLOBAL.stats();
        if next == prev {
            stable_samples += 1;
        } else {
            stable_samples = 0;
            prev = next;
        }
    }
}

#[kithara::test(serial)]
fn test_steady_state_get_put_zero_allocs() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);
    pool.pre_warm(4, |v| v.resize(4096, 0));

    // Warmup: stabilize internal state
    for _ in 0..10 {
        let _buf = pool.get();
    }

    wait_for_allocator_quiet();

    // Measure steady-state get/drop cycle
    let reg = Region::new(GLOBAL);
    for _ in 0..100 {
        let _buf = pool.get();
        // buf dropped back to pool
    }
    let change = reg.change();

    assert_eq!(
        change.allocations, 0,
        "steady-state get/put should not allocate; got {} allocations ({} bytes)",
        change.allocations, change.bytes_allocated,
    );
}

#[kithara::test(serial)]
fn test_get_with_resize_tracks_growth() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);

    // First get + resize: allocates
    {
        let mut buf = pool.get();
        buf.extend_from_slice(&[0u8; 8192]);
    } // returned to pool

    // Second get: should reuse the grown buffer without allocating
    wait_for_allocator_quiet();

    let reg = Region::new(GLOBAL);
    {
        let _buf = pool.get();
        // buffer already has capacity >= 8192 from previous cycle
    }
    let change = reg.change();

    assert_eq!(
        change.allocations, 0,
        "re-get of previously grown buffer should not allocate; got {} allocations",
        change.allocations,
    );
}

#[kithara::test(serial)]
fn test_no_leak_after_many_cycles() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);
    pool.pre_warm(4, |v| v.resize(4096, 0));

    // Stabilize
    for _ in 0..10 {
        let _buf = pool.get();
    }

    wait_for_allocator_quiet();

    let reg = Region::new(GLOBAL);
    for _ in 0..1000 {
        let mut buf = pool.get();
        buf.extend_from_slice(&[0u8; 256]);
        // dropped back to pool
    }
    let change = reg.change();

    // Net allocations should be approximately zero (allocs ≈ deallocs)
    let net = change.allocations as i64 - change.deallocations as i64;
    assert!(
        net.unsigned_abs() <= 2,
        "net allocations should be ~0 after many cycles; net={net} (allocs={}, deallocs={})",
        change.allocations,
        change.deallocations,
    );
}

#[kithara::test(serial)]
fn test_ensure_len_reuse_avoids_alloc() {
    let pool = SharedPool::<4, Vec<f32>>::new(16, 200_000);
    pool.pre_warm(4, |v| v.resize(4096, 0.0));

    // Stabilize
    for _ in 0..10 {
        let _buf = pool.get();
    }

    // ensure_len within pre-warmed capacity should not allocate
    wait_for_allocator_quiet();

    let reg = Region::new(GLOBAL);
    {
        let mut buf = pool.get();
        let _ = buf.ensure_len(2048); // 2048 < 4096 (pre-warmed size)
    }
    let change = reg.change();

    assert_eq!(
        change.allocations, 0,
        "ensure_len within capacity should not allocate; got {} allocations",
        change.allocations,
    );
}
