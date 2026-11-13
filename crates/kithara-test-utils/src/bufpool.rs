use kithara_bufpool::PoolConfig;

use crate::consts;

kithara_bufpool::pool_schema! {
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
        overall_budget: kithara_bufpool::OverallBudget,
        bytes: PoolConfig,
        samples: PoolConfig,
    ) -> Result<kithara_bufpool::PoolRegion<Self>, kithara_bufpool::PoolError> {
        Self::builder(overall_budget)
            .bytes(bytes)
            .samples(samples)
            .build()
    }
}

/// Concrete pool facade shared by workspace test harnesses.
pub type Pools = kithara_bufpool::PoolRegion<TestPools>;

/// Build one application-shaped test pool facade.
#[must_use]
pub fn pools() -> Pools {
    pools_with_budget(consts::DEFAULT_OVERALL_BYTES)
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
pub fn pools_with(overall_bytes: usize, bytes: PoolConfig, samples: PoolConfig) -> Pools {
    TestPools::region(
        kithara_bufpool::OverallBudget(overall_bytes),
        bytes,
        samples,
    )
    .unwrap_or_else(|error| panic!("test pool region: {error}"))
}

/// Copy sample values into a buffer from the supplied facade.
///
/// # Panics
///
/// Panics when the region budget cannot accommodate `values`.
#[must_use]
pub fn sample_buffer(pools: &Pools, values: &[f32]) -> kithara_bufpool::SampleBuffer {
    let mut buffer = pools
        .get_with_len::<f32>(values.len())
        .unwrap_or_else(|error| panic!("test sample buffer: {error}"));
    buffer.copy_from_slice(values);
    buffer
}

/// Acquire an empty byte buffer from the supplied facade.
#[must_use]
pub fn byte_buffer(pools: &Pools) -> kithara_bufpool::ByteBuffer {
    pools.get::<u8>()
}

fn byte_config() -> PoolConfig {
    PoolConfig::builder()
        .max_buffers(consts::BYTE_MAX_BUFFERS)
        .max_retained_capacity(consts::BYTE_MAX_RETAINED_CAPACITY)
        .build()
}

fn sample_config() -> PoolConfig {
    PoolConfig::builder()
        .max_buffers(consts::SAMPLE_MAX_BUFFERS)
        .max_retained_capacity(consts::SAMPLE_MAX_RETAINED_CAPACITY)
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
}
