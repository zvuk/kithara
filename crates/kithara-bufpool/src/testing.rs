//! Shared application-shaped pools for test composition roots.

struct Consts;

impl Consts {
    const BYTE_MAX_BUFFERS: usize = 32;
    const BYTE_MAX_RETAINED_CAPACITY: usize = 2 * 1024 * 1024;
    const DEFAULT_OVERALL_BYTES: usize = 256 * 1024 * 1024;
    const SAMPLE_MAX_BUFFERS: usize = 128;
    const SAMPLE_MAX_RETAINED_CAPACITY: usize = 200_000;
}

crate::pool_schema! {
    /// Byte and sample pools available to one isolated test harness.
    pub TestPools {
        bytes: u8,
        samples: f32,
    }
}

impl TestPools {
    /// Build one byte-and-sample pool facade for an isolated test harness.
    ///
    /// # Errors
    /// Returns an error when either pool config or eager allocation is invalid.
    pub fn region(
        overall_budget: crate::OverallBudget,
        bytes: crate::PoolConfig,
        samples: crate::PoolConfig,
    ) -> Result<crate::PoolRegion<Self>, crate::PoolError> {
        Self::builder(overall_budget)
            .bytes(bytes)
            .samples(samples)
            .build()
    }
}

/// Concrete pool facade shared by workspace test harnesses.
pub type Pools = crate::PoolRegion<TestPools>;

/// Build one application-shaped test pool facade.
#[must_use]
pub fn pools() -> Pools {
    pools_with_budget(Consts::DEFAULT_OVERALL_BYTES)
}

/// Build one application-shaped test pool facade with a custom hard budget.
#[must_use]
pub fn pools_with_budget(overall_bytes: usize) -> Pools {
    pools_with(overall_bytes, byte_config(), sample_config())
}

/// Build one test pool facade with explicit per-pool policies.
///
/// # Panics
///
/// Panics when the region cannot satisfy the requested initial allocation.
#[must_use]
pub fn pools_with(
    overall_bytes: usize,
    bytes: crate::PoolConfig,
    samples: crate::PoolConfig,
) -> Pools {
    TestPools::region(crate::OverallBudget(overall_bytes), bytes, samples)
        .unwrap_or_else(|error| panic!("test pool region: {error}"))
}

/// Copy sample values into a buffer from the supplied facade.
///
/// # Panics
///
/// Panics when the region budget cannot accommodate `values`.
#[must_use]
pub fn sample_buffer(pools: &Pools, values: &[f32]) -> crate::SampleBuffer {
    let mut buffer = pools
        .get_with_len::<f32>(values.len())
        .unwrap_or_else(|error| panic!("test sample buffer: {error}"));
    buffer.copy_from_slice(values);
    buffer
}

/// Acquire an empty byte buffer from the supplied facade.
#[must_use]
pub fn byte_buffer(pools: &Pools) -> crate::ByteBuffer {
    pools.get::<u8>()
}

fn byte_config() -> crate::PoolConfig {
    crate::PoolConfig::builder()
        .max_buffers(Consts::BYTE_MAX_BUFFERS)
        .max_retained_capacity(Consts::BYTE_MAX_RETAINED_CAPACITY)
        .max_share(crate::Percent::FULL)
        .build()
}

fn sample_config() -> crate::PoolConfig {
    crate::PoolConfig::builder()
        .max_buffers(Consts::SAMPLE_MAX_BUFFERS)
        .max_retained_capacity(Consts::SAMPLE_MAX_RETAINED_CAPACITY)
        .max_share(crate::Percent::FULL)
        .build()
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::{byte_buffer, pools, sample_buffer};

    #[kithara::test]
    fn buffers_use_the_supplied_region() {
        let pools = pools();
        let samples = sample_buffer(&pools, &[1.0, 2.0]);
        let bytes = byte_buffer(&pools);

        assert_eq!(&*samples, &[1.0, 2.0]);
        assert!(bytes.is_empty());
        assert!(pools.stats().allocated_bytes >= 2 * size_of::<f32>());
    }

    #[kithara::test]
    fn returned_capacity_is_bounded() {
        const BYTE_GUARDS: usize = 33;
        const BYTE_COUNT_CAPACITY: usize = 1;
        const BYTE_RETAINED_CAPACITY: usize = 2 * 1024 * 1024;
        const OVERALL_BYTES: usize = 256 * 1024 * 1024;
        const SAMPLE_RETAINED_CAPACITY: usize = 200_000;

        let retained_pools = pools();
        let baseline = retained_pools.stats().allocated_bytes;
        assert_eq!(retained_pools.stats().max_bytes, OVERALL_BYTES);

        let buffers = (0..BYTE_GUARDS)
            .map(|_| {
                retained_pools
                    .get_with_len::<u8>(BYTE_COUNT_CAPACITY)
                    .unwrap_or_else(|error| panic!("byte buffer: {error}"))
            })
            .collect::<Vec<_>>();
        drop(buffers);
        assert!(
            retained_pools.stats().allocated_bytes <= baseline + BYTE_COUNT_CAPACITY,
            "same-thread byte returns exceed one retained buffer"
        );

        let strict_pools = pools();
        let baseline = strict_pools.stats().allocated_bytes;
        let bytes = strict_pools
            .get_with_len::<u8>(BYTE_RETAINED_CAPACITY + 1)
            .unwrap_or_else(|error| panic!("oversized byte buffer: {error}"));
        drop(bytes);
        assert_eq!(strict_pools.stats().allocated_bytes, baseline);

        let samples = strict_pools
            .get_with_len::<f32>(SAMPLE_RETAINED_CAPACITY + 1)
            .unwrap_or_else(|error| panic!("oversized sample buffer: {error}"));
        drop(samples);
        assert!(
            strict_pools.stats().allocated_bytes <= baseline,
            "oversized sample return exceeds baseline"
        );
    }
}
