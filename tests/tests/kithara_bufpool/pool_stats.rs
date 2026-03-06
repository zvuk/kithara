use kithara_bufpool::internal::*;
use kithara_test_utils::kithara;

#[kithara::test]
fn test_pool_stats_tracks_hits() {
    let pool = Pool::<4, Vec<u8>>::new(16, 1024);

    {
        let mut buf = pool.get();
        buf.push(1);
    }

    let _buf = pool.get();
    let stats = pool.stats();
    assert_eq!(stats.home_hits, 1);
}

#[kithara::test]
fn test_pool_stats_tracks_misses() {
    let pool = Pool::<4, Vec<u8>>::new(16, 1024);
    let _buf = pool.get();
    let stats = pool.stats();
    assert_eq!(stats.alloc_misses, 1);
}

#[kithara::test]
fn test_pool_stats_tracks_steals() {
    let pool = Pool::<4, Vec<u8>>::new(128, 1024);
    let home = shard_index(&pool);
    let other = (home + 1) % 4;

    let mut buf = Vec::with_capacity(256);
    buf.push(0);
    put(&pool, buf, other);

    let _buf = pool.get();
    let stats = pool.stats();
    assert_eq!(stats.steal_hits, 1);
}

#[kithara::test]
fn test_pool_stats_tracks_drops() {
    let pool = Pool::<4, Vec<u8>>::new(4, 1024);
    let shard = shard_index(&pool);

    for _ in 0..2 {
        let mut buf = Vec::with_capacity(64);
        buf.push(0);
        put(&pool, buf, shard);
    }

    let stats = pool.stats();
    assert_eq!(stats.put_drops, 1);
}

#[kithara::test]
fn test_pre_warm_fills_pool() {
    let pool = SharedPool::<4, Vec<f32>>::new(128, 200_000);
    pool.pre_warm(8, |v| v.resize(4096, 0.0));

    let bufs: Vec<_> = (0..8).map(|_| pool.get()).collect();
    let warmed = bufs.iter().filter(|buf| buf.capacity() >= 4096).count();
    assert_eq!(warmed, 8);
}

#[kithara::test]
fn test_pre_warm_respects_budget() {
    let pool = SharedPool::<4, Vec<f32>>::with_byte_budget(128, 200_000, 10 * 1024);
    pool.pre_warm(10, |v| v.resize(4096, 0.0));

    let stats = pool.stats();
    assert!(stats.allocated_bytes <= 10 * 1024 + 16384);
}
