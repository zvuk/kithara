use kithara::{self, bufpool::testing::pools, platform::tokio::task::spawn_blocking};

#[kithara::test]
fn byte_buffer_supports_checked_writes() {
    let pools = pools();
    let mut buffer = pools.get::<u8>();
    buffer
        .try_extend_from_slice(b"hello")
        .expect("test bytes fit the region budget");
    assert_eq!(&buffer[..], b"hello");
}

#[kithara::test]
fn returned_buffer_is_reused_empty() {
    let pools = pools();
    {
        let mut buffer = pools
            .get_with_len::<u8>(4)
            .expect("test bytes fit the region budget");
        buffer.copy_from_slice(b"data");
    }

    let buffer = pools.get::<u8>();
    assert!(buffer.is_empty());
    assert!(buffer.capacity() >= 4);
}

#[kithara::test]
fn sample_buffer_has_requested_length() {
    let pools = pools();
    let mut buffer = pools
        .get_with_len::<f32>(100)
        .expect("test samples fit the region budget");
    buffer[0] = 1.5;
    assert_eq!(buffer.len(), 100);
    assert_eq!(buffer[0], 1.5);
}

#[kithara::test]
fn cloned_facade_shares_physical_pools() {
    let pools = pools();
    let clone = pools.clone();
    drop(
        pools
            .get_with_len::<u8>(128)
            .expect("test bytes fit the region budget"),
    );

    let buffer = clone.get::<u8>();
    assert!(buffer.capacity() >= 128);
}

#[kithara::test(tokio, browser)]
async fn multi_threaded_contention_preserves_data() {
    let pools = pools();
    let threads = 8usize;
    let iterations = 1_000usize;

    let mut handles = Vec::with_capacity(threads);
    for thread in 0..threads {
        let pools = pools.clone();
        handles.push(spawn_blocking(move || {
            for iteration in 0..iterations {
                let mut buffer = pools
                    .get_with_len::<u8>(64)
                    .expect("test bytes fit the region budget");
                let tag =
                    u8::try_from((thread * iterations + iteration) & 0xFF).expect("tag fits u8");
                buffer.fill(tag);
                assert!(buffer.iter().all(|&byte| byte == tag));
            }
        }));
    }

    for handle in handles {
        handle.await.expect("contention worker did not panic");
    }
}
