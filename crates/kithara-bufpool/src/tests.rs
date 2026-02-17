use super::*;

#[test]
fn test_pool_basic() {
    let pool = Pool::<4, Vec<u8>>::new(16, 1024);
    let mut buf = pool.get();
    buf.extend_from_slice(b"hello");
    assert_eq!(&buf[..], b"hello");
}

#[test]
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

#[test]
fn test_pool_f32() {
    let pool = Pool::<4, Vec<f32>>::new(16, 1024);
    let mut buf = pool.get_with(|b| b.resize(100, 0.0));
    assert_eq!(buf.len(), 100);
    buf[0] = 1.5;
    assert_eq!(buf[0], 1.5);
}

#[test]
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

#[test]
fn test_cross_shard_fallback() {
    // 2-shard pool: put a buffer into the "other" shard, then verify
    // get_with() finds it via cross-shard fallback.
    let pool = Pool::<2, Vec<u8>>::new(8, 1024);

    // Determine which shard this thread maps to
    let home_shard = pool.shard_index();
    let other_shard = (home_shard + 1) % 2;

    // Place a buffer with known capacity into the other shard
    let mut buf = Vec::with_capacity(999);
    buf.push(0); // ensure capacity > 0 after reuse
    pool.put(buf, other_shard);

    // get_with() should try home_shard first (empty), then fall back
    // to other_shard and find our buffer.
    let retrieved = pool.get();
    assert!(
        retrieved.capacity() > 0,
        "cross-shard fallback should return a reused buffer"
    );
}

#[test]
fn test_shard_saturation_drops_excess() {
    // 4 shards, 4 max_buffers total => 1 buffer per shard.
    // Returning more than 1 buffer to the same shard should drop excess.
    let pool = Pool::<4, Vec<u8>>::new(4, 1024);
    let shard = pool.shard_index();

    // Return 3 buffers to the same shard
    for i in 0..3 {
        let mut buf = Vec::with_capacity(128);
        buf.resize(10, i as u8);
        pool.put(buf, shard);
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

#[test]
fn test_pooled_into_inner_not_returned() {
    let pool = Pool::<4, Vec<u8>>::new(16, 1024);

    // Get a buffer, write data, extract via into_inner
    let buf = pool.get_with(|b| b.extend_from_slice(b"extracted"));
    let cap_before = buf.capacity();
    let vec = buf.into_inner();
    assert_eq!(&vec[..], b"extracted");
    assert_eq!(vec.capacity(), cap_before);
    drop(vec); // dropped without returning to pool

    // Next get should be a fresh allocation (pool is empty)
    let fresh = pool.get();
    assert_eq!(fresh.len(), 0);
    assert_eq!(fresh.capacity(), 0, "pool should be empty after into_inner");
}

#[test]
fn test_pooled_owned_into_inner_not_returned() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);

    // Get an owned buffer, write data, extract via into_inner
    let buf = pool.get_with(|b| b.extend_from_slice(b"owned_extracted"));
    let cap_before = buf.capacity();
    let vec = buf.into_inner();
    assert_eq!(&vec[..], b"owned_extracted");
    assert_eq!(vec.capacity(), cap_before);
    drop(vec); // dropped without returning to pool

    // Next get should be a fresh allocation (pool is empty)
    let fresh = pool.get();
    assert_eq!(fresh.len(), 0);
    assert_eq!(
        fresh.capacity(),
        0,
        "shared pool should be empty after into_inner"
    );
}

#[test]
fn test_pool_recycle() {
    let pool = Pool::<4, Vec<u8>>::new(16, 1024);

    // Get a buffer, extract it, and recycle manually
    let buf = pool.get_with(|b| b.extend_from_slice(b"recycle me"));
    let vec = buf.into_inner(); // Extracted — not returned to pool
    assert_eq!(&vec[..], b"recycle me");

    let cap = vec.capacity();
    pool.recycle(vec);

    // Next get should return the recycled buffer (cleared, capacity retained)
    let reused = pool.get();
    assert_eq!(reused.len(), 0);
    assert_eq!(reused.capacity(), cap);
}

#[test]
fn test_shared_pool_recycle() {
    let pool = SharedPool::<4, Vec<f32>>::new(16, 1024);

    let buf = pool.get_with(|b| b.resize(100, 1.0));
    let vec = buf.into_inner();
    assert_eq!(vec.len(), 100);

    let cap = vec.capacity();
    pool.recycle(vec);

    // Next get should return the recycled buffer
    let reused = pool.get();
    assert_eq!(reused.len(), 0);
    assert_eq!(reused.capacity(), cap);
}

#[test]
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

#[test]
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

#[test]
fn test_multi_threaded_contention() {
    use std::{sync::Arc, thread};

    let pool = Arc::new(Pool::<4, Vec<u8>>::new(64, 4096));
    let num_threads = 8;
    let iterations = 1000;

    let handles: Vec<_> = (0..num_threads)
        .map(|t| {
            let pool = Arc::clone(&pool);
            thread::spawn(move || {
                for i in 0..iterations {
                    let mut buf = pool.get_with(|b| b.resize(64, 0));
                    // Write a pattern unique to this thread+iteration
                    let tag = ((t * iterations + i) & 0xFF) as u8;
                    buf.fill(tag);
                    // Verify no corruption from other threads
                    assert!(buf.iter().all(|&b| b == tag), "data corruption detected");
                    // buf returned to pool on drop
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked during contention test");
    }
}
