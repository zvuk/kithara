use kithara_bufpool::internal::*;
use kithara_platform::tokio::task::spawn_blocking;
use kithara_test_utils::kithara;

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
    }

    let buf = pool.get();
    assert_eq!(buf.len(), 0);
    assert!(buf.capacity() > 0);
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
    let pool = Pool::<2, Vec<u8>>::new(8, 1024);
    let home_shard = shard_index(&pool);
    let other_shard = (home_shard + 1) % 2;

    let mut buf = Vec::with_capacity(999);
    buf.push(0);
    put(&pool, buf, other_shard);

    let retrieved = pool.get();
    assert!(retrieved.capacity() > 0);
}

#[kithara::test]
fn test_shard_saturation_drops_excess() {
    let pool = Pool::<4, Vec<u8>>::new(4, 1024);
    let shard = shard_index(&pool);

    for i in 0u8..3 {
        let mut buf = Vec::with_capacity(128);
        buf.resize(10, i);
        put(&pool, buf, shard);
    }

    let first = pool.get();
    assert!(first.capacity() > 0);

    let second = pool.get();
    assert_eq!(second.len(), 0);
    assert_eq!(second.capacity(), 0);
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
        assert_eq!(fresh.capacity(), 0);
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
        assert_eq!(fresh.capacity(), 0);
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
    let mut vec = Vec::with_capacity(256);
    vec.resize(100, 42.0);
    let cap = vec.capacity();

    {
        let attached = pool.attach(vec);
        assert_eq!(attached.len(), 100);
        assert_eq!(attached[0], 42.0);
    }

    let reused = pool.get();
    assert_eq!(reused.len(), 0);
    assert_eq!(reused.capacity(), cap);
}

#[kithara::test]
fn test_attach_into_inner_does_not_recycle() {
    let pool = SharedPool::<4, Vec<u8>>::new(16, 1024);

    let vec = Vec::with_capacity(128);
    let attached = pool.attach(vec);
    let extracted = attached.into_inner();
    drop(extracted);

    let fresh = pool.get();
    assert_eq!(fresh.capacity(), 0);
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
        handles.push(spawn_blocking(move || {
            for i in 0..iterations {
                let mut buf = pool.get_with(|b| b.resize(64, 0));
                let tag = u8::try_from((t * iterations + i) & 0xFF).expect("tag must fit into u8");
                buf.fill(tag);
                assert!(buf.iter().all(|&b| b == tag), "data corruption detected");
            }
        }));
    }

    for handle in handles {
        handle
            .await
            .expect("thread panicked during contention test");
    }
}
