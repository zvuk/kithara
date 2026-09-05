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
    budget::{BudgetPair, IdleReclaimer, RegionBudget, ReserveFailure},
    buffer::OwnedBuffer,
};

pub(crate) struct Core<const SHARDS: usize, B, const OBSERVE: bool>
where
    B: Storage,
{
    budgets: BudgetPair,
    cold: Option<ArrayQueue<B>>,
    shards: [PoolShard<B>; SHARDS],
    stat_alloc_misses: AtomicU64,
    stat_home_hits: AtomicU64,
    stat_put_drops: AtomicU64,
    stat_steal_hits: AtomicU64,
}

impl<const SHARDS: usize, B, const OBSERVE: bool> Core<SHARDS, B, OBSERVE>
where
    B: Storage,
{
    const MAX_PROBE: usize = 4;

    pub(crate) fn new(
        config: PoolConfig,
        region_budget: RegionBudget,
        pool_limit: usize,
    ) -> Result<Self, PoolError> {
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
            budgets: BudgetPair::new(region_budget, pool_limit),
            cold,
            shards: array::from_fn(|_| {
                PoolShard::new(
                    buffers_per_shard,
                    config.max_retained_capacity,
                    config.trim_capacity,
                )
            }),
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
                    core.budgets.release(bytes);
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
        let region_available = self
            .budgets
            .region_limit()
            .saturating_sub(self.budgets.region_current());
        let pool_available = self.budgets.limit().saturating_sub(self.budgets.current());
        let affordable_capacity =
            old_bytes.saturating_add(region_available.min(pool_available)) / element_bytes;
        let amortized_capacity = new_len.max(old_capacity.saturating_mul(2));
        let target_capacity = if affordable_capacity >= new_len {
            amortized_capacity.min(affordable_capacity)
        } else {
            new_len
        };
        let mut region_reclaimed = false;
        let mut pool_reclaimed = false;
        let mut first_attempt = true;
        let mut grown = loop {
            match self.allocate(target_capacity, old_bytes) {
                Ok(grown) => break grown,
                Err(error) => {
                    if first_attempt
                        && matches!(
                            &error,
                            PoolError::OverallBudgetExceeded { .. }
                                | PoolError::PoolBudgetExceeded { .. }
                                | PoolError::AllocationFailed { .. }
                        )
                        && self.reuse_for_growth(current, new_len, shard_idx)
                    {
                        return Ok(());
                    }
                    first_attempt = false;
                    let may_reclaim = match &error {
                        PoolError::OverallBudgetExceeded { .. } if !region_reclaimed => {
                            region_reclaimed = true;
                            true
                        }
                        PoolError::PoolBudgetExceeded { .. } if !pool_reclaimed => {
                            pool_reclaimed = true;
                            true
                        }
                        _ => false,
                    };
                    if !may_reclaim || !self.reclaim_for(&error) {
                        return Err(error);
                    }
                }
            }
        };
        grown.move_from(current);
        *current = grown;
        Ok(())
    }

    pub(crate) fn put(&self, value: B, shard_idx: usize) {
        let before = Self::byte_size(&value).unwrap_or(usize::MAX);
        match self.shards[shard_idx].try_put(value) {
            Ok(kept) => self.budgets.release(before.saturating_sub(kept)),
            Err(value) => {
                drop(value);
                self.budgets.release(before);
                Self::increment(&self.stat_put_drops);
            }
        }
    }

    pub(crate) fn normalize(&self, current: &mut B, shard_idx: usize) {
        let before = Self::byte_size(current).unwrap_or(usize::MAX);
        if let Some(kept) = self.shards[shard_idx].normalize(current) {
            self.budgets.release(before.saturating_sub(kept));
        } else {
            drop(std::mem::take(current));
            self.budgets.release(before);
            Self::increment(&self.stat_put_drops);
        }
    }

    pub(crate) fn shrink_to(&self, current: &mut B, min_capacity: usize) {
        let before = Self::byte_size(current).unwrap_or(usize::MAX);
        current.shrink_to(min_capacity);
        let after = Self::byte_size(current).unwrap_or(usize::MAX);
        self.budgets.release(before.saturating_sub(after));
    }

    pub(crate) fn stats(&self) -> PoolStats {
        PoolStats {
            alloc_misses: self.stat_alloc_misses.load(Ordering::Relaxed),
            home_hits: self.stat_home_hits.load(Ordering::Relaxed),
            put_drops: self.stat_put_drops.load(Ordering::Relaxed),
            steal_hits: self.stat_steal_hits.load(Ordering::Relaxed),
        }
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
            allocated_bytes: self.budgets.region_current(),
            max_bytes: self.budgets.region_limit(),
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

    fn increment(counter: &AtomicU64) {
        if OBSERVE {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn reserve(&self, amount: usize) -> Result<crate::budget::Reservation<'_>, PoolError> {
        self.budgets
            .reserve(amount)
            .map_err(|failure| match failure {
                ReserveFailure::Overall { amount, snapshot } => PoolError::OverallBudgetExceeded {
                    additional_bytes: amount,
                    allocated_bytes: snapshot.current,
                    max_bytes: snapshot.limit,
                },
                ReserveFailure::Pool { amount, snapshot } => PoolError::PoolBudgetExceeded {
                    additional_bytes: amount,
                    allocated_bytes: snapshot.current,
                    max_bytes: snapshot.limit,
                },
            })
    }

    fn shard_index() -> usize {
        let shards = SHARDS as u64;
        usize::try_from(current_thread_id() % shards).unwrap_or(0)
    }

    fn try_steal(&self, home: usize) -> Option<B> {
        let probes = Self::MAX_PROBE.min(SHARDS.saturating_sub(1));
        (1..=probes).find_map(|offset| self.shards[(home + offset) % SHARDS].try_get())
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

    fn reclaim_for(&self, error: &PoolError) -> bool {
        match error {
            PoolError::OverallBudgetExceeded {
                additional_bytes,
                allocated_bytes,
                max_bytes,
            } => {
                let target =
                    additional_bytes.saturating_sub(max_bytes.saturating_sub(*allocated_bytes));
                self.budgets.reclaim_region(target);
                true
            }
            PoolError::PoolBudgetExceeded {
                additional_bytes,
                allocated_bytes,
                max_bytes,
            } => {
                let target =
                    additional_bytes.saturating_sub(max_bytes.saturating_sub(*allocated_bytes));
                self.release_idle(target);
                true
            }
            _ => false,
        }
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
        self.budgets.release(bytes);
        bytes
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
                self.budgets.release(bytes);
            }
        }
        for shard in &self.shards {
            shard.drain(|value| {
                let bytes = Self::byte_size(&value).unwrap_or(usize::MAX);
                drop(value);
                self.budgets.release(bytes);
            });
        }
    }
}

#[cfg(test)]
mod tests;
