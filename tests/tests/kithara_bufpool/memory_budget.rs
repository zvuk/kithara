use kithara_bufpool::{BudgetExhausted, internal::*};
use kithara_test_utils::kithara;

// Tier 1: Pool-native memory budget tests

#[kithara::test]
fn test_byte_budget_enforced() {
    let budget = 64 * 1024; // 64 KB
    let pool = SharedPool::<4, Vec<u8>>::with_byte_budget(64, 0, budget);

    // Use ensure_len which respects byte budget (unlike get_with/resize)
    let mut successes = 0;
    let mut failures = 0;
    let mut bufs = Vec::new();

    for _ in 0..100 {
        let mut buf = pool.get();
        match buf.ensure_len(4096) {
            Ok(()) => {
                successes += 1;
                bufs.push(buf);
            }
            Err(BudgetExhausted) => {
                failures += 1;
            }
        }
    }

    // Budget should block some allocations
    assert!(
        failures > 0,
        "some ensure_len calls should fail due to budget"
    );
    assert!(successes > 0, "some ensure_len calls should succeed");

    // allocated_bytes should stay within budget + one buffer margin
    let margin = 4096 * 2;
    assert!(
        pool.allocated_bytes() <= budget + margin,
        "allocated_bytes ({}) should be within budget ({}) + margin ({})",
        pool.allocated_bytes(),
        budget,
        margin,
    );

    drop(bufs);
}

#[kithara::test]
fn test_reuse_rate_after_warmup() {
    let pool = SharedPool::<4, Vec<f32>>::new(128, 200_000);
    pool.pre_warm(16, |v| v.resize(4096, 0.0));

    // Run 100 get/drop cycles
    for _ in 0..100 {
        let _buf = pool.get();
        // buf returned to pool on drop
    }

    let stats = pool.stats();
    let total = stats.home_hits + stats.steal_hits + stats.alloc_misses;
    let reuse = stats.home_hits + stats.steal_hits;

    assert!(total > 0, "should have some operations recorded",);

    // Pre-warm fills home shards, so reuse rate should be very high.
    // Note: 16 pre_warm gets cause 16 alloc_misses, then 100 cycles should mostly hit.
    // We check only the 100-cycle portion by subtracting the warmup misses.
    let warmup_misses = 16u64; // pre_warm internally does get() which counts as miss
    let effective_misses = stats.alloc_misses.saturating_sub(warmup_misses);
    let effective_total = reuse + effective_misses;

    if effective_total > 0 {
        #[expect(clippy::cast_precision_loss, reason = "test counters are small")]
        let rate = reuse as f64 / effective_total as f64;
        assert!(
            rate > 0.95,
            "reuse rate {rate:.2} should be > 0.95 (home_hits={}, steal_hits={}, effective_misses={})",
            stats.home_hits,
            stats.steal_hits,
            effective_misses,
        );
    }
}

#[kithara::test]
fn test_no_unbounded_growth() {
    let pool = SharedPool::<4, Vec<u8>>::with_byte_budget(64, 0, 1024 * 1024);

    // Bursty pattern: allocate 20 buffers, drop all, repeat 10 times
    for _ in 0..10 {
        let bufs: Vec<_> = (0..20)
            .map(|_| pool.get_with(|v| v.resize(4096, 0)))
            .collect();
        drop(bufs);
    }

    let bytes_after = pool.allocated_bytes();

    // One more burst
    for _ in 0..10 {
        let bufs: Vec<_> = (0..20)
            .map(|_| pool.get_with(|v| v.resize(4096, 0)))
            .collect();
        drop(bufs);
    }

    let bytes_final = pool.allocated_bytes();

    // Memory should stabilize — not grow more than 10% between the two measurement points
    let growth = bytes_final.saturating_sub(bytes_after);
    let threshold = bytes_after / 10 + 4096; // 10% + one buffer slack
    assert!(
        growth <= threshold,
        "memory should stabilize: after={bytes_after}, final={bytes_final}, growth={growth}, threshold={threshold}",
    );
}

#[kithara::test]
fn test_pcm_pool_budget_stable() {
    let pool = SharedPool::<8, Vec<f32>>::with_byte_budget(128, 200_000, 2 * 1024 * 1024);

    // Simulate PCM workflow: get buffer, ensure_len, use, drop
    for _ in 0..50 {
        let mut buf = pool.get();
        let _ = buf.ensure_len(4096); // 4096 * 4 = 16KB per buffer
        // use the buffer
        if buf.len() >= 4096 {
            buf[0] = 1.0;
            buf[4095] = 1.0;
        }
    }

    let bytes = pool.allocated_bytes();
    let budget = 2 * 1024 * 1024;
    assert!(
        bytes <= budget + 16384,
        "PCM pool should stay within budget: allocated={bytes}, budget={budget}",
    );
}

#[kithara::test]
fn test_put_drops_when_shard_full() {
    // 4 shards, 4 max_buffers total => 1 per shard
    let pool = Pool::<4, Vec<u8>>::new(4, 1024);
    let shard = shard_index(&pool);

    // Force-put 5 buffers into one shard — at least 4 should be dropped
    for _ in 0..5 {
        let buf = Vec::with_capacity(64);
        put(&pool, buf, shard);
    }

    let stats = pool.stats();
    assert!(
        stats.put_drops > 0,
        "expected put_drops > 0 when shard overflows; got {}",
        stats.put_drops,
    );
}
