use kithara::{
    self,
    bufpool::{
        PoolConfig, PoolError,
        testing::{pools, pools_with},
    },
};

#[kithara::test]
fn oversized_sample_buffer_is_trimmed_on_return() {
    let pools = pools_with(
        1024 * 1024,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder()
            .max_buffers(8)
            .trim_capacity(4_096)
            .build(),
    );
    let oversized_capacity = {
        let buffer = pools
            .get_with_len::<f32>(100_000)
            .expect("test samples fit the region budget");
        buffer.capacity()
    };

    let reused = pools.get::<f32>();
    assert!(reused.capacity() < oversized_capacity);
    assert!(reused.capacity() >= 4_096);
}

#[kithara::test]
fn ensure_len_within_capacity_does_not_change_accounting() {
    let pools = pools();
    let mut buffer = pools
        .get_with_len::<f32>(200)
        .expect("test samples fit the region budget");
    let bytes_before = pools.stats().allocated_bytes;
    let capacity_before = buffer.capacity();

    buffer
        .ensure_len(100)
        .expect("shorter request needs no growth");

    assert_eq!(buffer.len(), 200);
    assert_eq!(buffer.capacity(), capacity_before);
    assert_eq!(pools.stats().allocated_bytes, bytes_before);
}

#[kithara::test]
fn sample_growth_respects_hard_budget() {
    let pools = pools_with(
        1_024,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder().max_buffers(8).build(),
    );
    let error = pools
        .get_with_len::<f32>(1_000)
        .expect_err("sample request must exceed the region budget");
    assert!(matches!(error, PoolError::OverallBudgetExceeded { .. }));
}
