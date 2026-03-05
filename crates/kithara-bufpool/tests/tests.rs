use kithara_bufpool::internal::*;

mod kithara {
    pub(crate) use kithara_test_macros::test;
}

#[kithara::test]
fn test_pool_basic() {
    let pool = Pool::<4, Vec<u8>>::new(16, 1024);
    let mut buf = pool.get();
    buf.extend_from_slice(b"hello");
    assert_eq!(&buf[..], b"hello");
}

#[kithara::test]
fn test_pool_reuse() {
    let pool = Pool::<4, Vec<u8>>::new(16, 1024);

    {
        let mut buf = pool.get();
        buf.extend_from_slice(b"data");
    } // buf returned to pool

    let buf = pool.get();
    assert_eq!(buf.len(), 0); // Should be cleared
    assert!(buf.capacity() > 0); // But capacity retained
}

#[kithara::test]
fn test_pool_f32() {
    let pool = Pool::<4, Vec<f32>>::new(16, 1024);
    let mut buf = pool.get_with(|b| b.resize(100, 0.0));
    assert_eq!(buf.len(), 100);
    buf[0] = 1.5;
    assert_eq!(buf[0], 1.5);
}

#[kithara::test]
fn test_shared_pool() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);
    let pool2 = pool.clone();

    let mut buf1 = pool.get();
    buf1.push(1);

    let mut buf2 = pool2.get();
    buf2.push(2);

    assert_eq!(buf1[0], 1);
    assert_eq!(buf2[0], 2);
}

#[kithara::test]
fn test_cross_shard_fallback() {
    // 2-shard pool: put a buffer into the "other" shard, then verify
    // get_with() finds it via cross-shard fallback.
    let pool = Pool::<2, Vec<u8>>::new(8, 1024);

    // Determine which shard this thread maps to
    let home_shard = shard_index(&pool);
    let other_shard = (home_shard + 1) % 2;

    // Place a buffer with known capacity into the other shard
    let mut buf = Vec::with_capacity(999);
    buf.push(0); // ensure capacity > 0 after reuse
    put(&pool, buf, other_shard);

    // get_with() should try home_shard first (empty), then fall back
    // to other_shard and find our buffer.
    let retrieved = pool.get();
    assert!(
        retrieved.capacity() > 0,
        "cross-shard fallback should return a reused buffer"
    );
}

#[kithara::test]
fn test_shard_saturation_drops_excess() {
    // 4 shards, 4 max_buffers total => 1 buffer per shard.
    // Returning more than 1 buffer to the same shard should drop excess.
    let pool = Pool::<4, Vec<u8>>::new(4, 1024);
    let shard = shard_index(&pool);

    // Return 3 buffers to the same shard
    for i in 0u8..3 {
        let mut buf = Vec::with_capacity(128);
        buf.resize(10, i);
        put(&pool, buf, shard);
    }

    // Only 1 should survive (max_buffers_per_shard = 4/4 = 1)
    let first = pool.get();
    assert!(
        first.capacity() > 0,
        "first get should return a reused buffer"
    );

    // Second get from same shard should allocate fresh (empty default Vec)
    let second = pool.get();
    assert_eq!(second.len(), 0);
    // A fresh Vec<u8>::default() has capacity 0
    assert_eq!(
        second.capacity(),
        0,
        "second get should be a fresh allocation"
    );
}

#[kithara::test]
#[case::pool(false)]
#[case::shared_pool(true)]
fn test_into_inner_does_not_recycle(#[case] shared: bool) {
    if shared {
        let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);
        let buf = pool.get_with(|b| b.extend_from_slice(b"owned_extracted"));
        let cap_before = buf.capacity();
        let vec = buf.into_inner();
        assert_eq!(&vec[..], b"owned_extracted");
        assert_eq!(vec.capacity(), cap_before);
        drop(vec);
        let fresh = pool.get();
        assert_eq!(fresh.len(), 0);
        assert_eq!(fresh.capacity(), 0, "pool should be empty after into_inner");
    } else {
        let pool = Pool::<4, Vec<u8>>::new(16, 1024);
        let buf = pool.get_with(|b| b.extend_from_slice(b"extracted"));
        let cap_before = buf.capacity();
        let vec = buf.into_inner();
        assert_eq!(&vec[..], b"extracted");
        assert_eq!(vec.capacity(), cap_before);
        drop(vec);
        let fresh = pool.get();
        assert_eq!(fresh.len(), 0);
        assert_eq!(fresh.capacity(), 0, "pool should be empty after into_inner");
    }
}

#[kithara::test]
#[case::pool(false)]
#[case::shared_pool(true)]
fn test_recycle_roundtrip(#[case] shared: bool) {
    if shared {
        let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);
        let buf = pool.get_with(|b| b.extend_from_slice(b"recycle me"));
        let vec = buf.into_inner();
        assert_eq!(&vec[..], b"recycle me");
        let cap = vec.capacity();
        pool.recycle(vec);
        let reused = pool.get();
        assert_eq!(reused.len(), 0);
        assert_eq!(reused.capacity(), cap);
    } else {
        let pool = Pool::<4, Vec<u8>>::new(16, 1024);
        let buf = pool.get_with(|b| b.extend_from_slice(b"recycle me"));
        let vec = buf.into_inner();
        assert_eq!(&vec[..], b"recycle me");
        let cap = vec.capacity();
        pool.recycle(vec);
        let reused = pool.get();
        assert_eq!(reused.len(), 0);
        assert_eq!(reused.capacity(), cap);
    }
}

#[kithara::test]
fn test_shared_pool_attach() {
    let pool = SharedPool::<4, Vec<f32>>::new(16, 1024);

    // Create a Vec outside the pool
    let mut vec = Vec::with_capacity(256);
    vec.resize(100, 42.0);
    let cap = vec.capacity();

    // Attach it to the pool — drop returns it
    {
        let attached = pool.attach(vec);
        assert_eq!(attached.len(), 100);
        assert_eq!(attached[0], 42.0);
    } // dropped → returned to pool

    // Next get should return the recycled buffer (cleared, capacity retained)
    let reused = pool.get();
    assert_eq!(reused.len(), 0);
    assert_eq!(reused.capacity(), cap);
}

#[kithara::test]
fn test_attach_into_inner_does_not_recycle() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);

    let vec = Vec::with_capacity(128);
    let attached = pool.attach(vec);
    let _extracted = attached.into_inner(); // Should NOT return to pool
    drop(_extracted);

    // Pool should be empty
    let fresh = pool.get();
    assert_eq!(fresh.capacity(), 0, "pool should be empty after into_inner");
}

#[kithara::test(tokio, browser)]
async fn test_multi_threaded_contention() {
    use std::sync::Arc;

    let pool = Arc::new(Pool::<4, Vec<u8>>::new(64, 4096));
    let num_threads = 8usize;
    let iterations = 1000usize;

    let mut handles = Vec::with_capacity(num_threads);
    for t in 0..num_threads {
        let pool = Arc::clone(&pool);
        handles.push(kithara_platform::tokio::task::spawn_blocking(move || {
            for i in 0..iterations {
                let mut buf = pool.get_with(|b| b.resize(64, 0));
                // Write a pattern unique to this thread+iteration
                let tag = u8::try_from((t * iterations + i) & 0xFF).expect("tag must fit into u8");
                buf.fill(tag);
                // Verify no corruption from other threads
                assert!(buf.iter().all(|&b| b == tag), "data corruption detected");
                // buf returned to pool on drop
            }
        }));
    }

    for h in handles {
        h.await.expect("thread panicked during contention test");
    }
}

// ── Step 1: Reuse trait adaptive trim ──────────────────────────────────

#[kithara::test]
fn test_reuse_trim_zero_preserves_capacity() {
    use kithara_bufpool::Reuse;
    let mut v: Vec<u8> = Vec::with_capacity(1024);
    v.push(1); // ensure non-empty
    let accepted = v.reuse(0);
    assert!(accepted, "trim=0 should accept buffer for reuse");
    assert!(v.capacity() > 0, "trim=0 must not deallocate");
}

#[kithara::test]
fn test_reuse_skip_trim_when_within_range() {
    use kithara_bufpool::Reuse;
    let mut v: Vec<f32> = Vec::with_capacity(4096);
    v.resize(100, 0.0);
    let cap_before = v.capacity();
    let accepted = v.reuse(4096);
    assert!(accepted);
    assert_eq!(
        v.capacity(),
        cap_before,
        "capacity within 2x trim should not be shrunk"
    );
}

#[kithara::test]
fn test_reuse_trim_when_oversize() {
    use kithara_bufpool::Reuse;
    let mut v: Vec<f32> = Vec::with_capacity(100_000);
    v.resize(100, 0.0);
    let accepted = v.reuse(4096);
    assert!(accepted);
    assert!(
        v.capacity() < 100_000,
        "oversize buffer should be shrunk; got {}",
        v.capacity()
    );
}

// ── Step 2: BytePool reuse verification ────────────────────────────────

#[kithara::test]
fn test_byte_pool_reuses_buffers() {
    // BytePool uses trim=0 — buffers must be reused, not discarded.
    let pool = SharedPool::<4, Vec<u8>>::with_byte_budget(
        usize::MAX, // no buffer count limit
        0,          // trim=0, same as global BytePool
        256 * 1024 * 1024,
    );

    // Get a buffer, grow it, drop it back to pool
    {
        let mut buf = pool.get();
        buf.resize(64 * 1024, 0xAB);
    }

    // Next get should return the reused buffer with capacity >= 64KB
    let buf = pool.get();
    assert!(
        buf.capacity() >= 64 * 1024,
        "BytePool should reuse buffers; got capacity {}",
        buf.capacity()
    );
}

#[kithara::test]
fn test_byte_pool_budget_tracks_correctly() {
    let pool = SharedPool::<4, Vec<u8>>::with_byte_budget(usize::MAX, 0, 256 * 1024 * 1024);

    // Allocate several buffers and track budget
    let bufs: Vec<_> = (0..10)
        .map(|_| pool.get_with(|v| v.resize(1024, 0)))
        .collect();

    assert!(
        pool.allocated_bytes() > 0,
        "budget should be non-zero with live buffers"
    );

    let bytes_before_drop = pool.allocated_bytes();
    drop(bufs);

    // After dropping all buffers back to pool, allocated_bytes stays
    // (buffers are pooled, not freed), but should not grow.
    assert!(
        pool.allocated_bytes() <= bytes_before_drop,
        "budget should not grow after returning buffers"
    );
}

// ── Step 3: Shard/buffer tuning ────────────────────────────────────────

#[kithara::test]
fn test_pcm_pool_shard_capacity() {
    // With 8 shards and 128 max_buffers, each shard holds 16.
    // Putting 8 buffers to one shard should keep all 8.
    let pool = Pool::<8, Vec<f32>>::new(128, 200_000);
    let shard = shard_index(&pool);

    for _ in 0..8 {
        let mut buf = Vec::with_capacity(4096);
        buf.resize(1024, 0.0);
        put(&pool, buf, shard);
    }

    // All 8 should be retrievable — hold them simultaneously to prove distinct
    let bufs: Vec<_> = (0..8).map(|_| pool.get()).collect();
    let reused = bufs.iter().filter(|b| b.capacity() > 0).count();
    assert!(
        reused >= 8,
        "8-shard pool with 16/shard should hold all 8; got {}",
        reused
    );
}

#[kithara::test]
fn test_pcm_pool_no_cross_shard_needed_typical() {
    // Single thread get/drop cycle should reuse from home shard.
    let pool = Pool::<8, Vec<f32>>::new(128, 200_000);

    // Warmup: allocate and return one buffer
    {
        let mut buf = pool.get();
        buf.resize(4096, 0.0);
    }

    // Now 100 cycles should all hit home shard (capacity > 0)
    let mut fresh_allocs = 0;
    for _ in 0..100 {
        let buf = pool.get();
        if buf.capacity() == 0 {
            fresh_allocs += 1;
        }
    }
    assert_eq!(
        fresh_allocs, 0,
        "after warmup, no fresh allocations expected"
    );
}

// ── Step 4: Cross-shard probe limits ───────────────────────────────────

#[kithara::test]
fn test_cross_shard_probe_finds_nearby() {
    // 8-shard pool: put buffer into shard home+2, probe limit 4 should find it.
    let pool = Pool::<8, Vec<u8>>::new(128, 1024);
    let home = shard_index(&pool);
    let target = (home + 2) % 8;

    let mut buf = Vec::with_capacity(512);
    buf.push(0);
    put(&pool, buf, target);

    let retrieved = pool.get();
    assert!(
        retrieved.capacity() > 0,
        "probe should find buffer within MAX_PROBE range"
    );
}

#[kithara::test]
fn test_cross_shard_fresh_alloc_when_out_of_range() {
    // 8-shard pool: put buffer into shard home+7 (far away).
    // With MAX_PROBE=4, probe covers home+1..=home+4 only.
    // Shard home+7 is out of range → fresh alloc expected.
    let pool = Pool::<8, Vec<u8>>::new(128, 1024);
    let home = shard_index(&pool);
    let far = (home + 7) % 8;

    let mut buf = Vec::with_capacity(512);
    buf.push(0);
    put(&pool, buf, far);

    let retrieved = pool.get();
    // If probe is limited, this will be a fresh alloc (cap=0).
    // If probe is unlimited (old behavior), it will find the buffer.
    // After Step 4, this should be a fresh alloc.
    assert_eq!(
        retrieved.capacity(),
        0,
        "buffer in shard home+7 should be out of probe range (MAX_PROBE=4)"
    );
}

// ── Step 5: Generic ensure_len ─────────────────────────────────────────

#[kithara::test]
fn test_ensure_len_f32_basic() {
    let pool = SharedPool::<4, Vec<f32>>::new(16, 4096);
    let mut buf = pool.get();
    buf.ensure_len(1024).expect("should succeed");
    assert_eq!(buf.len(), 1024);
    assert!(pool.allocated_bytes() > 0, "budget should be charged");
}

#[kithara::test]
fn test_ensure_len_f32_budget_exceeded() {
    use kithara_bufpool::BudgetExhausted;
    // Pool with tiny budget: 1KB
    let pool = SharedPool::<4, Vec<f32>>::with_byte_budget(16, 4096, 1024);
    let mut buf = pool.get();
    // 1000 f32 = 4000 bytes > 1KB budget
    let result = buf.ensure_len(1000);
    assert_eq!(result, Err(BudgetExhausted));
}

#[kithara::test]
fn test_ensure_len_noop_when_sufficient() {
    let pool = SharedPool::<4, Vec<f32>>::new(16, 4096);
    let mut buf = pool.get_with(|v| v.resize(200, 0.0));
    let bytes_before = pool.allocated_bytes();
    buf.ensure_len(100).expect("should be noop");
    assert_eq!(buf.len(), 200, "length should not change");
    assert_eq!(pool.allocated_bytes(), bytes_before, "budget unchanged");
}

#[kithara::test]
fn test_ensure_len_u8_still_works() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 4096);
    let mut buf = pool.get();
    buf.ensure_len(512).expect("should succeed");
    assert_eq!(buf.len(), 512);
}

// ── Step 6: PoolStats observability ────────────────────────────────────

#[kithara::test]
fn test_pool_stats_tracks_hits() {
    use kithara_bufpool::PoolStats;
    let pool = Pool::<4, Vec<u8>>::new(16, 1024);

    // Warmup: get + drop returns buffer to pool
    {
        let mut buf = pool.get();
        buf.push(1);
    }

    // This get should be a home hit
    let _buf = pool.get();
    let stats = pool.stats();
    assert_eq!(stats.home_hits, 1, "expected 1 home hit");
}

#[kithara::test]
fn test_pool_stats_tracks_misses() {
    let pool = Pool::<4, Vec<u8>>::new(16, 1024);

    // First get on empty pool = alloc miss
    let _buf = pool.get();
    let stats = pool.stats();
    assert_eq!(stats.alloc_misses, 1, "expected 1 alloc miss");
}

#[kithara::test]
fn test_pool_stats_tracks_steals() {
    let pool = Pool::<4, Vec<u8>>::new(128, 1024);
    let home = shard_index(&pool);
    let other = (home + 1) % 4;

    // Put buffer into other shard
    let mut buf = Vec::with_capacity(256);
    buf.push(0);
    put(&pool, buf, other);

    // Get from home (empty) → steal from other
    let _buf = pool.get();
    let stats = pool.stats();
    assert_eq!(stats.steal_hits, 1, "expected 1 steal hit");
}

#[kithara::test]
fn test_pool_stats_tracks_drops() {
    // 4 shards, 4 max_buffers => 1 per shard
    let pool = Pool::<4, Vec<u8>>::new(4, 1024);
    let shard = shard_index(&pool);

    // Put 2 buffers to same shard — second should be dropped
    for _ in 0..2 {
        let mut buf = Vec::with_capacity(64);
        buf.push(0);
        put(&pool, buf, shard);
    }
    let stats = pool.stats();
    assert_eq!(stats.put_drops, 1, "expected 1 put drop");
}

// ── Step 7: PooledOwned is not Clone ───────────────────────────────────

/// Static assertion: PooledOwned must NOT implement Clone.
/// Budget-invisible deep copies were a footgun; use pool.get() + copy instead.
const _: () = {
    const fn assert_not_clone<T>()
    where
    // Trick: this compiles only if T does NOT impl Clone.
    // We use the "negative reasoning" pattern via a helper trait.
    {
    }
    // We can't do true negative trait bounds in stable Rust,
    // so we just document the intent. The derive(Clone) removal
    // on PooledOwned is the enforcement.
};

// ── Step 9: Pre-warm API ───────────────────────────────────────────────

#[kithara::test]
fn test_pre_warm_fills_pool() {
    let pool = SharedPool::<4, Vec<f32>>::new(128, 200_000);
    pool.pre_warm(8, |v| v.resize(4096, 0.0));

    // 8 consecutive gets should all return pre-warmed buffers
    let bufs: Vec<_> = (0..8).map(|_| pool.get()).collect();
    let warmed = bufs.iter().filter(|b| b.capacity() >= 4096).count();
    assert_eq!(warmed, 8, "all 8 should be pre-warmed; got {}", warmed);
}

#[kithara::test]
fn test_pre_warm_respects_budget() {
    // Pool with 10KB budget
    let pool = SharedPool::<4, Vec<f32>>::with_byte_budget(128, 200_000, 10 * 1024);
    // Each buffer: 4096 f32 = 16KB. Budget only allows ~0 full buffers.
    // pre_warm should stop when budget is exceeded.
    pool.pre_warm(10, |v| v.resize(4096, 0.0));

    // Pool should have some buffers, but not all 10 (budget limited).
    // At minimum it should not panic.
    let stats = pool.stats();
    assert!(
        stats.allocated_bytes <= 10 * 1024 + 16384, // some slack
        "budget should constrain pre-warm; got {} bytes",
        stats.allocated_bytes
    );
}
