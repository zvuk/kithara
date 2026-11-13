use std::{
    array,
    sync::atomic::{AtomicU64, Ordering},
};

use crossbeam_queue::ArrayQueue;
use kithara_platform::{sync::Arc, thread::current_thread_id};
use kithara_test_macros as kithara;

use super::{shard::PoolShard, stats::PoolStats, storage::Storage};
use crate::{
    PoolConfig, PoolError,
    budget::{IdleReclaimer, RegionBudget, Reservation},
    buffer::OwnedBuffer,
};

pub(crate) struct Core<const SHARDS: usize, B, const OBSERVE: bool>
where
    B: Storage,
{
    stat_alloc_misses: AtomicU64,
    stat_home_hits: AtomicU64,
    stat_put_drops: AtomicU64,
    stat_steal_hits: AtomicU64,
    budget: RegionBudget,
    cold: Option<ArrayQueue<B>>,
    config: PoolConfig,
    shards: [PoolShard<B>; SHARDS],
}

impl<const SHARDS: usize, B, const OBSERVE: bool> Core<SHARDS, B, OBSERVE>
where
    B: Storage,
{
    const MAX_PROBE: usize = 4;

    pub(crate) fn new(config: PoolConfig, budget: RegionBudget) -> Result<Self, PoolError> {
        if SHARDS == 0 {
            return Err(PoolError::InvalidConfig {
                field: "shards",
                reason: "must contain at least one shard",
            });
        }
        if config.max_buffers < SHARDS {
            return Err(PoolError::InvalidConfig {
                field: "max_buffers",
                reason: "must provide at least one retained slot per shard",
            });
        }
        let buffers_per_shard = config.max_buffers / SHARDS;
        let effective_buffers = buffers_per_shard
            .min(PoolShard::<B>::MAX_SLOTS)
            .checked_mul(SHARDS)
            .ok_or(PoolError::InvalidConfig {
                field: "max_buffers",
                reason: "effective shard capacity overflows usize",
            })?;
        if config.initial_buffers > effective_buffers {
            return Err(PoolError::InvalidConfig {
                field: "initial_buffers",
                reason: "exceeds the effective retained-buffer capacity",
            });
        }
        B::bytes_for_capacity(config.initial_capacity)
            .and_then(|bytes| bytes.checked_mul(config.initial_buffers))
            .ok_or(PoolError::InvalidConfig {
                field: "initial_capacity",
                reason: "initial payload byte count overflows usize",
            })?;

        let cold = (config.initial_buffers > 0).then(|| ArrayQueue::new(config.initial_buffers));
        let core = Self {
            cold,
            config,
            budget,
            shards: array::from_fn(|_| PoolShard::new(buffers_per_shard)),
            stat_alloc_misses: AtomicU64::new(0),
            stat_home_hits: AtomicU64::new(0),
            stat_put_drops: AtomicU64::new(0),
            stat_steal_hits: AtomicU64::new(0),
        };

        if let Some(cold) = &core.cold {
            for _ in 0..config.initial_buffers {
                let value = core.allocate(config.initial_capacity, 0)?;
                if let Err(value) = cold.push(value) {
                    let bytes = Self::byte_size(&value)?;
                    drop(value);
                    core.budget.release(bytes);
                    return Err(PoolError::InvalidConfig {
                        field: "initial_buffers",
                        reason: "cold-start queue rejected a validated payload",
                    });
                }
            }
        }
        Ok(core)
    }

    #[kithara::measure]
    pub(crate) fn acquire(self: &Arc<Self>) -> OwnedBuffer<SHARDS, B, OBSERVE> {
        let shard_idx = Self::shard_index();
        let value = self.shards[shard_idx]
            .try_get()
            .map(|value| (value, &self.stat_home_hits))
            .or_else(|| {
                self.try_steal(shard_idx)
                    .map(|value| (value, &self.stat_steal_hits))
            })
            .or_else(|| {
                self.cold
                    .as_ref()
                    .and_then(ArrayQueue::pop)
                    .map(|value| (value, &self.stat_home_hits))
            })
            .map_or_else(
                || {
                    Self::increment(&self.stat_alloc_misses);
                    B::default()
                },
                |(value, counter)| {
                    Self::increment(counter);
                    value
                },
            );
        OwnedBuffer::new(Arc::clone(self), value, shard_idx)
    }

    fn allocate(&self, capacity: usize, old_bytes: usize) -> Result<B, PoolError> {
        let requested_bytes = Self::bytes_for_capacity(capacity)?;
        let requested_delta =
            requested_bytes
                .checked_sub(old_bytes)
                .ok_or(PoolError::InvalidConfig {
                    field: "buffer growth",
                    reason: "new capacity is smaller than the current capacity",
                })?;
        let mut reservation = self.reserve(requested_delta)?;
        let grown = B::try_with_capacity(capacity).map_err(|()| PoolError::AllocationFailed {
            additional_bytes: requested_delta,
            allocated_bytes: self.budget.current(),
            max_bytes: self.budget.limit(),
        })?;
        let actual_bytes = Self::byte_size(&grown)?;
        let actual_delta = actual_bytes
            .checked_sub(old_bytes)
            .ok_or(PoolError::InvalidConfig {
                field: "buffer growth",
                reason: "allocator returned less capacity than the current buffer",
            })?;
        let extra = if actual_delta > requested_delta {
            Some(self.reserve(actual_delta - requested_delta)?)
        } else {
            reservation.reduce(requested_delta - actual_delta);
            None
        };
        if let Some(extra) = extra {
            extra.commit();
        }
        reservation.commit();
        Ok(grown)
    }

    fn byte_size(value: &B) -> Result<usize, PoolError> {
        Self::bytes_for_capacity(value.capacity())
    }

    fn bytes_for_capacity(capacity: usize) -> Result<usize, PoolError> {
        B::bytes_for_capacity(capacity).ok_or_else(|| PoolError::CapacityOverflow {
            elements: capacity,
            element_size: B::bytes_for_capacity(1).unwrap_or(usize::MAX),
        })
    }

    pub(crate) fn grow(
        &self,
        current: &mut B,
        new_len: usize,
        shard_idx: usize,
    ) -> Result<(), PoolError> {
        let old_capacity = current.capacity();
        if new_len <= old_capacity {
            return Ok(());
        }
        let old_bytes = Self::bytes_for_capacity(old_capacity)?;
        let element_bytes = Self::bytes_for_capacity(1)?;
        if element_bytes == 0 {
            return Ok(());
        }
        let available = self.budget.limit().saturating_sub(self.budget.current());
        let affordable_capacity = old_bytes.saturating_add(available) / element_bytes;
        let amortized_capacity = new_len.max(old_capacity.saturating_mul(2));
        let target_capacity = if affordable_capacity >= new_len {
            amortized_capacity.min(affordable_capacity)
        } else {
            new_len
        };
        let mut grown = match self.allocate(target_capacity, old_bytes) {
            Ok(grown) => grown,
            Err(error) => {
                if matches!(
                    &error,
                    PoolError::OverallBudgetExceeded { .. } | PoolError::AllocationFailed { .. }
                ) && self.reuse_for_growth(current, new_len, shard_idx)
                {
                    return Ok(());
                }
                if !self.reclaim_for(&error) {
                    return Err(error);
                }
                self.allocate(target_capacity, old_bytes)?
            }
        };
        grown.move_from(current);
        *current = grown;
        Ok(())
    }

    fn increment(counter: &AtomicU64) {
        if OBSERVE {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn normalize(&self, current: &mut B) {
        if !PoolShard::<B>::normalize(current, &self.config) {
            self.release_value(std::mem::take(current));
            Self::increment(&self.stat_put_drops);
        }
    }

    pub(crate) fn put(&self, value: B, shard_idx: usize) {
        if let Err(value) = self.shards[shard_idx].try_put(value, &self.config) {
            self.release_value(value);
            Self::increment(&self.stat_put_drops);
        }
    }

    fn reclaim_for(&self, error: &PoolError) -> bool {
        let PoolError::OverallBudgetExceeded {
            additional_bytes,
            allocated_bytes,
            max_bytes,
        } = error
        else {
            return false;
        };
        let target = additional_bytes.saturating_sub(max_bytes.saturating_sub(*allocated_bytes));
        self.budget.reclaim(target);
        true
    }

    fn release_idle(&self, target: usize) -> usize {
        if target == 0 {
            return 0;
        }
        let mut released = 0usize;
        for shard in &self.shards {
            let candidates = shard.len();
            for _ in 0..candidates {
                let Some(value) = shard.try_get() else {
                    break;
                };
                released = released.saturating_add(self.release_value(value));
                if released >= target {
                    return released;
                }
            }
        }
        if let Some(cold) = &self.cold {
            let candidates = cold.len();
            for _ in 0..candidates {
                let Some(value) = cold.pop() else {
                    break;
                };
                released = released.saturating_add(self.release_value(value));
                if released >= target {
                    return released;
                }
            }
        }
        released
    }

    fn release_value(&self, value: B) -> usize {
        let bytes = Self::byte_size(&value).unwrap_or(usize::MAX);
        drop(value);
        self.budget.release(bytes);
        bytes
    }

    fn reserve(&self, amount: usize) -> Result<Reservation<'_>, PoolError> {
        self.budget
            .reserve(amount)
            .map_err(|snapshot| PoolError::OverallBudgetExceeded {
                additional_bytes: amount,
                allocated_bytes: snapshot.current,
                max_bytes: snapshot.limit,
            })
    }

    fn reuse_for_growth(&self, current: &mut B, new_len: usize, home: usize) -> bool {
        for offset in 0..SHARDS {
            let shard_idx = (home + offset) % SHARDS;
            let candidates = self.shards[shard_idx].len();
            for _ in 0..candidates {
                let Some(mut value) = self.shards[shard_idx].try_get() else {
                    break;
                };
                if value.capacity() >= new_len {
                    value.move_from(current);
                    self.put(std::mem::replace(current, value), home);
                    return true;
                }
                self.put(value, shard_idx);
            }
        }
        if let Some(cold) = &self.cold {
            let candidates = cold.len();
            for _ in 0..candidates {
                let Some(mut value) = cold.pop() else {
                    break;
                };
                if value.capacity() >= new_len {
                    value.move_from(current);
                    self.put(std::mem::replace(current, value), home);
                    return true;
                }
                if let Err(value) = cold.push(value) {
                    self.put(value, home);
                }
            }
        }
        false
    }

    fn shard_index() -> usize {
        let shards = SHARDS as u64;
        usize::try_from(current_thread_id() % shards).unwrap_or(0)
    }

    pub(crate) fn shrink_to(&self, current: &mut B, min_capacity: usize) {
        let before = Self::byte_size(current).unwrap_or(usize::MAX);
        current.shrink_to(min_capacity);
        let after = Self::byte_size(current).unwrap_or(usize::MAX);
        self.budget.release(before.saturating_sub(after));
    }

    pub(crate) fn stats(&self) -> PoolStats {
        PoolStats {
            alloc_misses: self.stat_alloc_misses.load(Ordering::Relaxed),
            home_hits: self.stat_home_hits.load(Ordering::Relaxed),
            put_drops: self.stat_put_drops.load(Ordering::Relaxed),
            steal_hits: self.stat_steal_hits.load(Ordering::Relaxed),
        }
    }

    fn try_steal(&self, home: usize) -> Option<B> {
        let probes = Self::MAX_PROBE.min(SHARDS.saturating_sub(1));
        (1..=probes).find_map(|offset| self.shards[(home + offset) % SHARDS].try_get())
    }
}

impl<const SHARDS: usize, B, const OBSERVE: bool> IdleReclaimer for Core<SHARDS, B, OBSERVE>
where
    B: Storage + Send + 'static,
{
    fn reclaim(&self, bytes: usize) -> usize {
        self.release_idle(bytes)
    }
}

impl<const SHARDS: usize, B, const OBSERVE: bool> Drop for Core<SHARDS, B, OBSERVE>
where
    B: Storage,
{
    fn drop(&mut self) {
        if let Some(cold) = &self.cold {
            while let Some(value) = cold.pop() {
                let bytes = Self::byte_size(&value).unwrap_or(usize::MAX);
                drop(value);
                self.budget.release(bytes);
            }
        }
        for shard in &self.shards {
            shard.drain(|value| {
                let bytes = Self::byte_size(&value).unwrap_or(usize::MAX);
                drop(value);
                self.budget.release(bytes);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kithara_platform::sync::{Arc, Weak};
    use kithara_test_utils::kithara;

    use super::Core;
    use crate::{
        PoolConfig,
        budget::{IdleReclaimer, RegionBudget},
        pool::storage::Storage,
    };

    type RefillCore = Core<1, RefillStorage, true>;

    #[derive(Default)]
    struct OverallocStorage {
        capacity: usize,
    }

    impl Storage for OverallocStorage {
        fn bytes_for_capacity(capacity: usize) -> Option<usize> {
            Some(capacity)
        }

        fn capacity(&self) -> usize {
            self.capacity
        }

        fn clear(&mut self) {}

        fn move_from(&mut self, _other: &mut Self) {}

        fn shrink_to(&mut self, min_capacity: usize) {
            self.capacity = self.capacity.min(min_capacity);
        }

        fn try_with_capacity(capacity: usize) -> Result<Self, ()> {
            Ok(Self {
                capacity: if capacity < 4 { capacity } else { 2 * capacity },
            })
        }
    }

    struct RefillStorage {
        remaining: Arc<AtomicUsize>,
        core: Weak<RefillCore>,
        capacity: usize,
    }

    impl Default for RefillStorage {
        fn default() -> Self {
            Self {
                capacity: 0,
                core: Weak::new(),
                remaining: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Storage for RefillStorage {
        fn bytes_for_capacity(capacity: usize) -> Option<usize> {
            Some(capacity)
        }

        fn capacity(&self) -> usize {
            self.capacity
        }

        fn clear(&mut self) {}

        fn move_from(&mut self, other: &mut Self) {
            self.capacity = self.capacity.saturating_add(other.capacity);
            other.capacity = 0;
        }

        fn shrink_to(&mut self, min_capacity: usize) {
            self.capacity = self.capacity.min(min_capacity);
        }

        fn try_with_capacity(capacity: usize) -> Result<Self, ()> {
            Ok(Self {
                capacity,
                core: Weak::new(),
                remaining: Arc::new(AtomicUsize::new(0)),
            })
        }
    }

    impl Drop for RefillStorage {
        fn drop(&mut self) {
            let Some(core) = self.core.upgrade() else {
                return;
            };
            if self
                .remaining
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                    left.checked_sub(1)
                })
                .is_err()
            {
                return;
            }
            let Ok(mut replacement) = core.allocate(self.capacity, 0) else {
                return;
            };
            replacement.core = Arc::downgrade(&core);
            replacement.remaining = Arc::clone(&self.remaining);
            core.put(replacement, 0);
        }
    }

    #[kithara::test]
    fn failed_growth_reuses_suitable_buffer_beyond_fast_probe() {
        const CAPACITY: usize = 8;
        const CURRENT_CAPACITY: usize = 4;
        const LIMIT: usize = CAPACITY + CURRENT_CAPACITY;
        const SHARDS: usize = 6;

        let region_budget = RegionBudget::new(LIMIT);
        let core = Core::<SHARDS, Vec<u8>, true>::new(
            PoolConfig::builder().max_buffers(SHARDS).build(),
            region_budget.clone(),
        )
        .unwrap_or_else(|error| panic!("test core: {error}"));
        let home = Core::<SHARDS, Vec<u8>, true>::shard_index();
        let distant = (home + Core::<SHARDS, Vec<u8>, true>::MAX_PROBE + 1) % SHARDS;
        let retained = core
            .allocate(CAPACITY, 0)
            .unwrap_or_else(|error| panic!("retained buffer: {error}"));
        core.put(retained, distant);
        assert!(core.try_steal(home).is_none());
        let mut current = core
            .allocate(CURRENT_CAPACITY, 0)
            .unwrap_or_else(|error| panic!("current buffer: {error}"));
        current.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(region_budget.current(), LIMIT);

        core.grow(&mut current, CAPACITY, home)
            .unwrap_or_else(|error| panic!("reuse distant buffer: {error}"));

        assert!(current.capacity() >= CAPACITY);
        assert_eq!(current, [1, 2, 3, 4]);
        assert_eq!(region_budget.current(), LIMIT);
        core.put(current, home);
        drop(core);
        assert_eq!(region_budget.current(), 0);
    }

    #[kithara::test]
    fn failed_growth_reuses_suitable_cold_start_buffer() {
        const CAPACITY: usize = 8;
        const CURRENT_CAPACITY: usize = 4;
        const LIMIT: usize = CAPACITY + CURRENT_CAPACITY;

        let region_budget = RegionBudget::new(LIMIT);
        let core = Core::<1, Vec<u8>, true>::new(
            PoolConfig::builder()
                .initial_buffers(1)
                .initial_capacity(CAPACITY)
                .max_buffers(1)
                .build(),
            region_budget.clone(),
        )
        .unwrap_or_else(|error| panic!("test core: {error}"));
        let mut current = core
            .allocate(CURRENT_CAPACITY, 0)
            .unwrap_or_else(|error| panic!("current buffer: {error}"));
        current.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(region_budget.current(), LIMIT);

        core.grow(&mut current, CAPACITY, 0)
            .unwrap_or_else(|error| panic!("reuse cold-start buffer: {error}"));

        assert!(current.capacity() >= CAPACITY);
        assert_eq!(current, [1, 2, 3, 4]);
        assert_eq!(region_budget.current(), LIMIT);
        core.put(current, 0);
        drop(core);
        assert_eq!(region_budget.current(), 0);
    }

    #[kithara::test]
    fn overallocated_growth_reclaims_the_failed_extra_reservation() {
        let region_budget = RegionBudget::new(10);
        let donor = Arc::new(
            Core::<1, Vec<u8>, true>::new(
                PoolConfig::builder().max_buffers(1).build(),
                region_budget.clone(),
            )
            .unwrap_or_else(|error| panic!("donor core: {error}")),
        );
        let requester = Arc::new(
            Core::<1, OverallocStorage, true>::new(
                PoolConfig::builder().max_buffers(1).build(),
                region_budget.clone(),
            )
            .unwrap_or_else(|error| panic!("requester core: {error}")),
        );
        let donor_slot: Arc<dyn IdleReclaimer> = donor.clone();
        let requester_slot: Arc<dyn IdleReclaimer> = requester.clone();
        region_budget
            .install_reclaimers(
                [Arc::downgrade(&donor_slot), Arc::downgrade(&requester_slot)].into(),
            )
            .unwrap_or_else(|_| panic!("reclaimer inventory installs once"));
        let idle = donor
            .allocate(4, 0)
            .unwrap_or_else(|error| panic!("donor allocation: {error}"));
        donor.put(idle, 0);
        let mut current = requester
            .allocate(2, 0)
            .unwrap_or_else(|error| panic!("requester allocation: {error}"));
        assert_eq!(region_budget.current(), 6);

        requester
            .grow(&mut current, 4, 0)
            .unwrap_or_else(|error| panic!("growth after reclaim: {error}"));

        assert_eq!(current.capacity(), 8);
        assert_eq!(region_budget.current(), 8);
        requester.put(current, 0);
        drop(requester_slot);
        drop(donor_slot);
        drop(requester);
        drop(donor);
        assert_eq!(region_budget.current(), 0);
    }

    #[kithara::test(miri)]
    fn pressure_reclaim_scans_only_the_idle_snapshot() {
        let region_budget = RegionBudget::new(8);
        let core = Arc::new(
            RefillCore::new(
                PoolConfig::builder().max_buffers(1).build(),
                region_budget.clone(),
            )
            .unwrap_or_else(|error| panic!("test core: {error}")),
        );
        let remaining = Arc::new(AtomicUsize::new(3));
        let mut value = core
            .allocate(1, 0)
            .unwrap_or_else(|error| panic!("initial value: {error}"));
        value.core = Arc::downgrade(&core);
        value.remaining = Arc::clone(&remaining);
        core.put(value, 0);

        assert_eq!(core.release_idle(usize::MAX), 1);
        assert_eq!(remaining.load(Ordering::Relaxed), 2);
        assert_eq!(region_budget.current(), 1);

        drop(core);
        assert_eq!(region_budget.current(), 0);
    }
}
