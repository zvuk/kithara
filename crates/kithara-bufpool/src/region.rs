use std::{cell::RefCell, fmt, marker::PhantomData, ops::DerefMut};

use kithara_platform::sync::{Arc, Weak};
use kithara_ring::RingHalves;

use crate::{
    HasPool, OverallBudget, PoolConfig, PoolError, PoolKey, PoolKeyWithLen, PoolStats,
    budget::{IdleReclaimer, RegionBudget},
    key::PoolAccess,
    pool::{Core, storage::Storage},
};

/// Region-wide byte statistics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RegionStats {
    /// Bytes currently tracked by every pool in the region.
    pub allocated_bytes: usize,
    /// Region-wide hard byte limit.
    pub max_bytes: usize,
    /// Highest region byte count admitted, including in-flight reservations.
    pub peak_allocated_bytes: usize,
}

/// One cloneable facade over a closed typed schema and its shared hard budget.
#[derive_where::derive_where(Clone)]
pub struct PoolRegion<S> {
    inner: Arc<RegionInner<S>>,
}

struct RegionInner<S> {
    budget: RegionBudget,
    schema: S,
}

impl<S> PoolRegion<S> {
    /// Build one region after a generated schema builder has collected every slot.
    ///
    /// # Errors
    ///
    /// Returns an error when a slot configuration is invalid or its eager
    /// payload cannot be admitted and allocated.
    #[doc(hidden)]
    pub fn __build<F>(overall_budget: OverallBudget, build_schema: F) -> Result<Self, PoolError>
    where
        F: FnOnce(&BuildContext) -> Result<S, PoolError>,
    {
        let context = BuildContext {
            budget: RegionBudget::new(overall_budget.0),
            reclaimers: RefCell::new(Vec::new()),
        };
        let schema = build_schema(&context)?;
        context.install_reclaimers()?;
        Ok(Self {
            inner: Arc::new(RegionInner {
                schema,
                budget: context.budget,
            }),
        })
    }

    /// Acquire an empty buffer from the pool registered for `K`.
    #[must_use]
    pub fn get<K>(&self) -> K::Buffer
    where
        K: PoolKey,
        S: HasPool<K>,
    {
        let slot = self.slot::<K>();
        K::__get(&slot.core, PoolAccess::new())
    }

    /// Acquire a buffer whose length is at least `len` elements.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested capacity overflows, exceeds the
    /// region budget, or cannot be allocated.
    #[inline]
    pub fn get_with_len<K>(&self, len: usize) -> Result<K::Buffer, PoolError>
    where
        K: PoolKeyWithLen,
        S: HasPool<K>,
    {
        let slot = self.slot::<K>();
        K::__get_with_len(&slot.core, len, PoolAccess::new())
    }

    /// Acquire a buffer of at least `capacity` elements as the slots of a
    /// single-producer single-consumer ring.
    ///
    /// The buffer returns to its pool once both halves are dropped, and its
    /// slots count against the region budget for the ring's lifetime.
    ///
    /// # Errors
    ///
    /// Returns an error when `capacity` is zero, or when the buffer cannot be
    /// acquired under the region budget.
    pub fn ring<K>(&self, capacity: usize) -> Result<RingHalves<K::Buffer>, PoolError>
    where
        K: PoolKeyWithLen,
        S: HasPool<K>,
        K::Buffer: DerefMut<Target = [K::Item]>,
        K::Item: Copy,
    {
        kithara_ring::split(self.get_with_len::<K>(capacity)?).map_err(|_| PoolError::EmptyRing)
    }

    /// Snapshot reuse counters for the pool registered for `K`.
    #[must_use]
    pub fn pool_stats<K>(&self) -> PoolStats
    where
        K: PoolKey,
        S: HasPool<K>,
    {
        let slot = self.slot::<K>();
        K::__stats(&slot.core, PoolAccess::new())
    }

    fn slot<K>(&self) -> &PoolSlot<K>
    where
        K: PoolKey,
        S: HasPool<K>,
    {
        let slot = <S as HasPool<K>>::__slot(&self.inner.schema);
        debug_assert!(
            self.inner.budget.same_region(&slot.budget),
            "pool slot belongs to a different region"
        );
        slot
    }

    /// Snapshot the shared region budget.
    #[must_use]
    pub fn stats(&self) -> RegionStats {
        RegionStats {
            allocated_bytes: self.inner.budget.current(),
            max_bytes: self.inner.budget.limit(),
            peak_allocated_bytes: self.inner.budget.peak(),
        }
    }
}

impl<S> fmt::Debug for PoolRegion<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PoolRegion")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// Safe construction context used by `pool_schema!` expansions.
#[doc(hidden)]
pub struct BuildContext {
    reclaimers: RefCell<Vec<Weak<dyn IdleReclaimer>>>,
    budget: RegionBudget,
}

impl BuildContext {
    pub(crate) fn core<const SHARDS: usize, B, const OBSERVE: bool>(
        &self,
        config: PoolConfig,
    ) -> Result<Arc<Core<SHARDS, B, OBSERVE>>, PoolError>
    where
        B: Storage + Send + 'static,
    {
        let core = Arc::new(Core::new(config, self.region_budget())?);
        self.reclaimers
            .borrow_mut()
            .push(Arc::downgrade(&core) as Weak<dyn IdleReclaimer>);
        Ok(core)
    }

    fn install_reclaimers(&self) -> Result<(), PoolError> {
        let reclaimers = std::mem::take(&mut *self.reclaimers.borrow_mut()).into_boxed_slice();
        self.budget
            .install_reclaimers(reclaimers)
            .map_err(|_| PoolError::InvalidConfig {
                field: "schema",
                reason: "idle reclaimer inventory was already installed",
            })
    }

    pub(crate) fn region_budget(&self) -> RegionBudget {
        self.budget.clone()
    }

    /// Build one opaque physical slot under this context's shared budget.
    ///
    /// # Errors
    ///
    /// Returns an error when `config` is invalid or its eager payload cannot
    /// be admitted and allocated.
    #[doc(hidden)]
    pub fn slot<K>(&self, config: PoolConfig) -> Result<PoolSlot<K>, PoolError>
    where
        K: PoolKey,
    {
        Ok(PoolSlot {
            core: K::__build(self, config, PoolAccess::new())?,
            budget: self.budget.clone(),
            key: PhantomData,
        })
    }
}

/// Opaque typed slot stored inside a generated schema.
#[doc(hidden)]
pub struct PoolSlot<K>
where
    K: PoolKey,
{
    core: K::Core,
    key: PhantomData<fn() -> K>,
    budget: RegionBudget,
}

#[cfg(test)]
mod tests {
    use kithara_test_utils::kithara;

    use super::*;

    #[kithara::test(miri)]
    fn later_slot_failure_releases_earlier_eager_payloads() {
        let mut observed = None;
        let result: Result<PoolRegion<()>, PoolError> =
            PoolRegion::__build(OverallBudget(64), |context| {
                observed = Some(context.region_budget());
                let _bytes = context.slot::<u8>(
                    PoolConfig::builder()
                        .initial_buffers(1)
                        .initial_capacity(8)
                        .max_buffers(32)
                        .build(),
                )?;
                let _samples = context.slot::<f32>(
                    PoolConfig::builder()
                        .initial_buffers(9)
                        .max_buffers(8)
                        .build(),
                )?;
                Ok(())
            });

        assert!(matches!(result, Err(PoolError::InvalidConfig { .. })));
        assert_eq!(
            observed
                .unwrap_or_else(|| panic!("build context was not observed"))
                .current(),
            0
        );
    }
}
