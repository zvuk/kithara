use std::{sync::Barrier, thread};

use kithara_bufpool::{OverallBudget, PoolConfig, pool_schema};
use kithara_platform::sync::Arc;
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_utils::kithara;

pool_schema! {
    pub BytePools {
        bytes: u8,
    }
}

#[kithara::test]
fn concurrent_get_put_stays_bounded_and_lock_free() {
    const THREADS: usize = 8;
    const ITERS: usize = 4_000;
    const BUFFER_BYTES: usize = 1024;
    const MAX_BUFFERS: usize = 256;

    let pools = BytePools::builder(OverallBudget(MAX_BUFFERS * BUFFER_BYTES))
        .bytes(PoolConfig::builder().max_buffers(MAX_BUFFERS).build())
        .build()
        .unwrap_or_else(|error| panic!("test region: {error}"));
    let barrier = Arc::new(Barrier::new(THREADS));

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let pools = pools.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..ITERS {
                    let mut buffer = pools
                        .get_with_len::<u8>(BUFFER_BYTES)
                        .unwrap_or_else(|error| panic!("buffer: {error}"));
                    buffer[..4].fill(1);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap_or_else(|_| panic!("worker panicked"));
    }

    let stats = pools.stats();
    assert!(stats.allocated_bytes > 0);
    assert!(stats.allocated_bytes <= MAX_BUFFERS * BUFFER_BYTES);
}
