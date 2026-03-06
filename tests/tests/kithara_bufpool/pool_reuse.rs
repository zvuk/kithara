use kithara_bufpool::internal::*;
use kithara_test_utils::kithara;

#[kithara::test]
fn test_reuse_trim_zero_preserves_capacity() {
    use kithara_bufpool::Reuse;
    let mut v: Vec<u8> = Vec::with_capacity(1024);
    v.push(1);
    let accepted = v.reuse(0);
    assert!(accepted);
    assert!(v.capacity() > 0);
}

#[kithara::test]
fn test_reuse_skip_trim_when_within_range() {
    use kithara_bufpool::Reuse;
    let mut v: Vec<f32> = Vec::with_capacity(4096);
    v.resize(100, 0.0);
    let cap_before = v.capacity();
    let accepted = v.reuse(4096);
    assert!(accepted);
    assert_eq!(v.capacity(), cap_before);
}

#[kithara::test]
fn test_reuse_trim_when_oversize() {
    use kithara_bufpool::Reuse;
    let mut v: Vec<f32> = Vec::with_capacity(100_000);
    v.resize(100, 0.0);
    let accepted = v.reuse(4096);
    assert!(accepted);
    assert!(v.capacity() < 100_000);
}

#[kithara::test]
fn test_byte_pool_reuses_buffers() {
    let pool = SharedPool::<4, Vec<u8>>::with_byte_budget(usize::MAX, 0, 256 * 1024 * 1024);

    {
        let mut buf = pool.get();
        buf.resize(64 * 1024, 0xAB);
    }

    let buf = pool.get();
    assert!(buf.capacity() >= 64 * 1024);
}

#[kithara::test]
fn test_byte_pool_budget_tracks_correctly() {
    let pool = SharedPool::<4, Vec<u8>>::with_byte_budget(usize::MAX, 0, 256 * 1024 * 1024);
    let bufs: Vec<_> = (0..10)
        .map(|_| pool.get_with(|v| v.resize(1024, 0)))
        .collect();

    assert!(pool.allocated_bytes() > 0);
    let bytes_before_drop = pool.allocated_bytes();
    drop(bufs);
    assert!(pool.allocated_bytes() <= bytes_before_drop);
}

#[kithara::test]
fn test_pcm_pool_shard_capacity() {
    let pool = Pool::<8, Vec<f32>>::new(128, 200_000);
    let shard = shard_index(&pool);

    for _ in 0..8 {
        let mut buf = Vec::with_capacity(4096);
        buf.resize(1024, 0.0);
        put(&pool, buf, shard);
    }

    let bufs: Vec<_> = (0..8).map(|_| pool.get()).collect();
    let reused = bufs.iter().filter(|buf| buf.capacity() > 0).count();
    assert!(reused >= 8);
}

#[kithara::test]
fn test_pcm_pool_no_cross_shard_needed_typical() {
    let pool = Pool::<8, Vec<f32>>::new(128, 200_000);

    {
        let mut buf = pool.get();
        buf.resize(4096, 0.0);
    }

    let mut fresh_allocs = 0;
    for _ in 0..100 {
        let buf = pool.get();
        if buf.capacity() == 0 {
            fresh_allocs += 1;
        }
    }
    assert_eq!(fresh_allocs, 0);
}

#[kithara::test]
fn test_cross_shard_probe_finds_nearby() {
    let pool = Pool::<8, Vec<u8>>::new(128, 1024);
    let home = shard_index(&pool);
    let target = (home + 2) % 8;

    let mut buf = Vec::with_capacity(512);
    buf.push(0);
    put(&pool, buf, target);

    let retrieved = pool.get();
    assert!(retrieved.capacity() > 0);
}

#[kithara::test]
fn test_cross_shard_fresh_alloc_when_out_of_range() {
    let pool = Pool::<8, Vec<u8>>::new(128, 1024);
    let home = shard_index(&pool);
    let far = (home + 7) % 8;

    let mut buf = Vec::with_capacity(512);
    buf.push(0);
    put(&pool, buf, far);

    let retrieved = pool.get();
    assert_eq!(retrieved.capacity(), 0);
}

#[kithara::test]
fn test_ensure_len_f32_basic() {
    let pool = SharedPool::<4, Vec<f32>>::new(16, 4096);
    let mut buf = pool.get();
    buf.ensure_len(1024).expect("should succeed");
    assert_eq!(buf.len(), 1024);
    assert!(pool.allocated_bytes() > 0);
}

#[kithara::test]
fn test_ensure_len_f32_budget_exceeded() {
    use kithara_bufpool::BudgetExhausted;
    let pool = SharedPool::<4, Vec<f32>>::with_byte_budget(16, 4096, 1024);
    let mut buf = pool.get();
    let result = buf.ensure_len(1000);
    assert_eq!(result, Err(BudgetExhausted));
}

#[kithara::test]
fn test_ensure_len_noop_when_sufficient() {
    let pool = SharedPool::<4, Vec<f32>>::new(16, 4096);
    let mut buf = pool.get_with(|v| v.resize(200, 0.0));
    let bytes_before = pool.allocated_bytes();
    buf.ensure_len(100).expect("should be noop");
    assert_eq!(buf.len(), 200);
    assert_eq!(pool.allocated_bytes(), bytes_before);
}

#[kithara::test]
fn test_ensure_len_u8_still_works() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 4096);
    let mut buf = pool.get();
    buf.ensure_len(512).expect("should succeed");
    assert_eq!(buf.len(), 512);
}
