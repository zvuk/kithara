use std::num::NonZeroUsize;

use kithara::bufpool::{OverallBudget, PoolConfig, PoolRegion, pool_schema};

use crate::RunError;

pool_schema! {
    pub(crate) AnalyzePools {
        bytes: u8,
        samples: f32,
    }
}

pub(crate) type Pools = PoolRegion<AnalyzePools>;

pub(crate) fn build(jobs: NonZeroUsize) -> Result<Pools, RunError> {
    const PASS_BUDGET_BYTES: usize = 512 * 1024 * 1024;

    let budget = jobs
        .get()
        .checked_mul(PASS_BUDGET_BYTES)
        .ok_or_else(|| std::io::Error::other("job pool budget overflows usize"))?;
    Ok(AnalyzePools::builder(OverallBudget(budget))
        .bytes(
            PoolConfig::builder()
                .max_buffers(32)
                .max_retained_capacity(2 * 1024 * 1024)
                .build(),
        )
        .samples(
            PoolConfig::builder()
                .max_buffers(128)
                .max_retained_capacity(200_000)
                .build(),
        )
        .build()?)
}
