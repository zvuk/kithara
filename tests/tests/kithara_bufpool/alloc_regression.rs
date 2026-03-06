use kithara_bufpool::SharedPool;
use kithara_test_utils::kithara;

#[kithara::test(serial)]
fn test_steady_state_get_put_zero_allocs() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);
    pool.pre_warm(4, |v| v.resize(4096, 0));

    for _ in 0..10 {
        let _buf = pool.get();
    }

    let misses_before = pool.stats().alloc_misses;
    for _ in 0..100 {
        let _buf = pool.get();
    }
    let misses_after = pool.stats().alloc_misses;

    assert_eq!(
        misses_after, misses_before,
        "steady-state get/put should reuse pooled buffers without new misses",
    );
}

#[kithara::test(serial)]
fn test_get_with_resize_tracks_growth() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);

    {
        let mut buf = pool.get();
        buf.extend_from_slice(&[0u8; 8192]);
        assert!(
            buf.capacity() >= 8192,
            "first resize must grow buffer capacity",
        );
    }

    let bytes_before = pool.allocated_bytes();
    let misses_before = pool.stats().alloc_misses;
    let buf = pool.get();
    drop(buf);
    let misses_after = pool.stats().alloc_misses;
    let bytes_after = pool.allocated_bytes();

    assert_eq!(
        misses_after, misses_before,
        "re-get of previously grown buffer should not miss the pool",
    );
    assert_eq!(
        bytes_after, bytes_before,
        "re-get of previously grown buffer should not allocate new bytes",
    );
}

#[kithara::test(serial)]
fn test_no_leak_after_many_cycles() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);
    pool.pre_warm(4, |v| v.resize(4096, 0));

    for _ in 0..10 {
        let _buf = pool.get();
    }

    let misses_before = pool.stats().alloc_misses;
    let bytes_before = pool.allocated_bytes();

    for _ in 0..1000 {
        let mut buf = pool.get();
        buf.extend_from_slice(&[0u8; 256]);
    }

    let misses_after = pool.stats().alloc_misses;
    let bytes_after = pool.allocated_bytes();

    assert_eq!(
        misses_after, misses_before,
        "reused buffers should not trigger new allocation misses",
    );
    assert_eq!(
        bytes_after, bytes_before,
        "allocated bytes should stay stable across repeated reuse cycles",
    );
}

#[kithara::test(serial)]
fn test_ensure_len_reuse_avoids_alloc() {
    let pool = SharedPool::<4, Vec<f32>>::new(16, 200_000);
    pool.pre_warm(4, |v| v.resize(4096, 0.0));

    for _ in 0..10 {
        let _buf = pool.get();
    }

    let misses_before = pool.stats().alloc_misses;
    let mut buf = pool.get();
    let capacity_before = buf.capacity();
    let result = buf.ensure_len(2048);
    let capacity_after = buf.capacity();
    drop(buf);
    let misses_after = pool.stats().alloc_misses;

    assert!(result.is_ok(), "ensure_len within capacity should succeed");
    assert!(
        capacity_before >= 4096,
        "pre-warmed buffer should keep configured capacity",
    );
    assert_eq!(
        capacity_after, capacity_before,
        "ensure_len within capacity should not grow buffer capacity",
    );
    assert_eq!(
        misses_after, misses_before,
        "ensure_len within capacity should not cause a new pool miss",
    );
}
