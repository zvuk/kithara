use kithara::{
    self,
    bufpool::{PoolConfig, PoolError, testing::pools_with},
};

#[kithara::test]
fn byte_growth_never_crosses_overall_budget() {
    let budget = 64 * 1024;
    let pools = pools_with(
        budget,
        PoolConfig::builder().max_buffers(64).build(),
        PoolConfig::builder().max_buffers(8).build(),
    );
    let mut buffers = Vec::new();
    let mut rejections = 0;

    for _ in 0..100 {
        match pools.get_with_len::<u8>(4_096) {
            Ok(buffer) => buffers.push(buffer),
            Err(PoolError::OverallBudgetExceeded { .. }) => rejections += 1,
            Err(error) => panic!("unexpected pool error: {error}"),
        }
    }

    assert!(!buffers.is_empty());
    assert!(rejections > 0);
    assert!(pools.stats().allocated_bytes <= budget);
}

#[kithara::test]
fn byte_and_sample_pools_compete_for_one_budget() {
    let budget = 64 * 1024;
    let pools = pools_with(
        budget,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder().max_buffers(8).build(),
    );
    let bytes = pools
        .get_with_len::<u8>(32 * 1024)
        .expect("bytes consume half the shared budget");
    let samples = pools
        .get_with_len::<f32>(8 * 1024)
        .expect("samples consume the other half");

    let error = pools
        .get_with_len::<u8>(1)
        .expect_err("neither pool may exceed the shared cap");
    assert!(matches!(error, PoolError::OverallBudgetExceeded { .. }));
    assert_eq!(pools.stats().allocated_bytes, budget);

    drop((bytes, samples));
}

#[kithara::test]
fn retained_bytes_stabilize_across_cycles() {
    let pools = pools_with(
        1024 * 1024,
        PoolConfig::builder().max_buffers(32).build(),
        PoolConfig::builder().max_buffers(8).build(),
    );

    for _ in 0..10 {
        let buffers: Vec<_> = (0..20)
            .map(|_| {
                pools
                    .get_with_len::<u8>(4_096)
                    .expect("test bytes fit the region budget")
            })
            .collect();
        drop(buffers);
    }
    let bytes_after = pools.stats().allocated_bytes;

    for _ in 0..10 {
        let buffers: Vec<_> = (0..20)
            .map(|_| {
                pools
                    .get_with_len::<u8>(4_096)
                    .expect("test bytes fit the region budget")
            })
            .collect();
        drop(buffers);
    }

    assert_eq!(pools.stats().allocated_bytes, bytes_after);
}
