use kithara::{
    self,
    bufpool::{
        PoolConfig, PoolError,
        testing::{pools, pools_with},
    },
};

#[kithara::test]
fn returned_capacity_is_reused() {
    let pools = pools();
    let buffer = pools
        .get_with_len::<u8>(64)
        .expect("test bytes fit the region budget");
    let ptr = buffer.as_ptr();
    drop(buffer);

    let reused = pools.get::<u8>();
    assert!(reused.capacity() >= 64);
    assert_eq!(reused.as_ptr(), ptr);
}

#[kithara::test]
fn configured_initial_payload_is_accounted() {
    let pools = pools_with(
        1024 * 1024,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder()
            .initial_buffers(8)
            .initial_capacity(4_096)
            .max_buffers(128)
            .build(),
    );

    assert_eq!(pools.stats().allocated_bytes, 8 * 4_096 * 4);
}

#[kithara::test]
fn overall_budget_rejection_is_reported() {
    let pools = pools_with(
        1_024,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder().max_buffers(8).build(),
    );

    let error = pools
        .get_with_len::<u8>(2_048)
        .expect_err("request must exceed the shared budget");
    assert!(matches!(error, PoolError::OverallBudgetExceeded { .. }));
}

#[kithara::test]
fn shard_saturation_drops_excess_returns() {
    let pools = pools_with(
        1024 * 1024,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder().max_buffers(8).build(),
    );
    let mut buffers = Vec::new();
    for _ in 0..3 {
        buffers.push(
            pools
                .get_with_len::<u8>(64)
                .expect("test bytes fit the region budget"),
        );
    }
    drop(buffers);

    assert!(pools.stats().allocated_bytes <= 64);
}
