use kithara_platform::sync::Arc;

use crate::{
    BytePool, PcmPool,
    budget::RegionBudget,
    global::{BYTE_MAX_BUFFERS, BYTE_TRIM_CAPACITY, PCM_MAX_BUFFERS, PCM_TRIM_CAPACITY},
};

const DEFAULT_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Configuration for a shared buffer-pool region.
///
/// Pool sizing policies follow the workspace defaults in `global`; the one
/// product knob is the total byte budget shared by both pools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionConfig {
    max_bytes: usize,
}

impl RegionConfig {
    /// Set the total byte budget shared by byte and PCM pools.
    #[must_use]
    pub fn max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl Default for RegionConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// Statistics for both pools sharing a region budget.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RegionStats {
    /// Current bytes tracked across both pools.
    pub allocated_bytes: usize,
    /// Post-initialization growth events that exceeded the shared budget.
    pub budget_overshoots: u64,
    /// Total home and steal hits for the byte pool.
    pub byte_pool_hits: u64,
    /// Fresh allocations by the byte pool.
    pub byte_pool_misses: u64,
    /// Maximum bytes available to both pools.
    pub max_bytes: usize,
    /// Total home and steal hits for the PCM pool.
    pub pcm_pool_hits: u64,
    /// Fresh allocations by the PCM pool.
    pub pcm_pool_misses: u64,
}

/// Canonical owner of byte and PCM pools sharing one byte budget.
#[derive(Clone)]
pub struct Region {
    inner: Arc<RegionInner>,
}

struct RegionInner {
    budget: RegionBudget,
    byte_pool: BytePool,
    pcm_pool: PcmPool,
}

impl Region {
    /// Create a region with the supplied shared-budget configuration.
    #[must_use]
    pub fn new(config: RegionConfig) -> Self {
        let budget = RegionBudget::new(config.max_bytes);
        let byte_pool =
            BytePool::with_region_budget(BYTE_MAX_BUFFERS, BYTE_TRIM_CAPACITY, budget.clone());
        let pcm_pool =
            PcmPool::with_region_budget(PCM_MAX_BUFFERS, PCM_TRIM_CAPACITY, budget.clone());
        Self {
            inner: Arc::new(RegionInner {
                budget,
                byte_pool,
                pcm_pool,
            }),
        }
    }

    /// Get the region's byte-buffer pool.
    #[must_use]
    pub fn byte_pool(&self) -> BytePool {
        self.inner.byte_pool.clone()
    }

    /// Get the region's PCM-buffer pool.
    #[must_use]
    pub fn pcm_pool(&self) -> PcmPool {
        self.inner.pcm_pool.clone()
    }

    /// Get combined budget and per-pool hit/miss statistics.
    #[must_use]
    pub fn stats(&self) -> RegionStats {
        let byte = self.inner.byte_pool.stats();
        let pcm = self.inner.pcm_pool.stats();
        RegionStats {
            allocated_bytes: self.inner.budget.allocated_bytes(),
            budget_overshoots: byte.budget_overshoots + pcm.budget_overshoots,
            byte_pool_hits: byte.home_hits + byte.steal_hits,
            byte_pool_misses: byte.alloc_misses,
            max_bytes: self.inner.budget.max_bytes(),
            pcm_pool_hits: pcm.home_hits + pcm.steal_hits,
            pcm_pool_misses: pcm.alloc_misses,
        }
    }
}

impl Default for Region {
    /// Create a top-level convenience region.
    ///
    /// Library code should receive an injected region-derived pool instead.
    fn default() -> Self {
        Self::new(RegionConfig::default())
    }
}
