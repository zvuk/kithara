use kithara::{
    self,
    bufpool::{PoolConfig, testing::pools_with},
};

#[kithara::test(serial)]
fn returned_growth_is_accounted_once() {
    let pools = pools_with(
        1024 * 1024,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder().max_buffers(8).build(),
    );
    drop(
        pools
            .get_with_len::<u8>(8_192)
            .expect("test bytes fit the region budget"),
    );
    let before = pools.stats().allocated_bytes;

    drop(pools.get::<u8>());

    assert_eq!(pools.stats().allocated_bytes, before);
}

#[kithara::test(serial)]
fn repeated_reuse_keeps_allocated_bytes_stable() {
    let pools = pools_with(
        1024 * 1024,
        PoolConfig::builder()
            .initial_buffers(4)
            .initial_capacity(4_096)
            .max_buffers(32)
            .build(),
        PoolConfig::builder().max_buffers(8).build(),
    );
    let before = pools.stats().allocated_bytes;

    for _ in 0..1_000 {
        let mut buffer = pools.get::<u8>();
        buffer
            .try_extend_from_slice(&[0; 256])
            .expect("eager capacity is sufficient");
    }

    assert_eq!(pools.stats().allocated_bytes, before);
}

#[kithara::test(serial)]
fn eager_sample_capacity_makes_shorter_ensure_len_a_noop() {
    let pools = pools_with(
        1024 * 1024,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder()
            .initial_buffers(4)
            .initial_capacity(4_096)
            .max_buffers(8)
            .build(),
    );
    let mut buffer = pools.get::<f32>();
    let capacity = buffer.capacity();
    let bytes = pools.stats().allocated_bytes;

    buffer
        .ensure_len(2_048)
        .expect("eager sample capacity is sufficient");

    assert!(capacity >= 4_096);
    assert_eq!(buffer.capacity(), capacity);
    assert_eq!(pools.stats().allocated_bytes, bytes);
}
