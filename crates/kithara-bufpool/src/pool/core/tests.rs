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
    capacity: usize,
    core: Weak<RefillCore>,
    remaining: Arc<AtomicUsize>,
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
        LIMIT,
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
        LIMIT,
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
            10,
        )
        .unwrap_or_else(|error| panic!("donor core: {error}")),
    );
    let requester = Arc::new(
        Core::<1, OverallocStorage, true>::new(
            PoolConfig::builder().max_buffers(1).build(),
            region_budget.clone(),
            10,
        )
        .unwrap_or_else(|error| panic!("requester core: {error}")),
    );
    let donor_slot: Arc<dyn IdleReclaimer> = donor.clone();
    let requester_slot: Arc<dyn IdleReclaimer> = requester.clone();
    region_budget
        .install_reclaimers([Arc::downgrade(&donor_slot), Arc::downgrade(&requester_slot)].into())
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

#[kithara::test]
fn growth_reclaims_both_region_and_slot_deficits() {
    const REGION_LIMIT: usize = 16;
    const REQUESTER_LIMIT: usize = 8;

    let region_budget = RegionBudget::new(REGION_LIMIT);
    let donor = Arc::new(
        Core::<1, Vec<u8>, true>::new(
            PoolConfig::builder().max_buffers(1).build(),
            region_budget.clone(),
            REGION_LIMIT,
        )
        .unwrap_or_else(|error| panic!("donor core: {error}")),
    );
    let requester = Arc::new(
        Core::<1, Vec<u8>, true>::new(
            PoolConfig::builder().max_buffers(1).build(),
            region_budget.clone(),
            REQUESTER_LIMIT,
        )
        .unwrap_or_else(|error| panic!("requester core: {error}")),
    );
    let donor_slot: Arc<dyn IdleReclaimer> = donor.clone();
    let requester_slot: Arc<dyn IdleReclaimer> = requester.clone();
    region_budget
        .install_reclaimers([Arc::downgrade(&donor_slot), Arc::downgrade(&requester_slot)].into())
        .unwrap_or_else(|_| panic!("reclaimer inventory installs once"));

    let donor_idle = donor
        .allocate(8, 0)
        .unwrap_or_else(|error| panic!("donor allocation: {error}"));
    donor.put(donor_idle, 0);
    let requester_idle = requester
        .allocate(4, 0)
        .unwrap_or_else(|error| panic!("requester idle allocation: {error}"));
    requester.put(requester_idle, 0);
    let mut current = requester
        .allocate(4, 0)
        .unwrap_or_else(|error| panic!("requester active allocation: {error}"));
    current.extend_from_slice(&[1, 2, 3, 4]);
    assert_eq!(region_budget.current(), REGION_LIMIT);

    requester
        .grow(&mut current, REQUESTER_LIMIT, 0)
        .unwrap_or_else(|error| panic!("growth after both reclaims: {error}"));

    assert!(current.capacity() >= REQUESTER_LIMIT);
    assert_eq!(current, [1, 2, 3, 4]);
    assert_eq!(region_budget.current(), REQUESTER_LIMIT);
    requester.put(current, 0);
    drop(requester_slot);
    drop(donor_slot);
    drop(requester);
    drop(donor);
    assert_eq!(region_budget.current(), 0);
}

#[kithara::test]
fn pressure_reclaim_scans_only_the_idle_snapshot() {
    let region_budget = RegionBudget::new(8);
    let core = Arc::new(
        RefillCore::new(
            PoolConfig::builder().max_buffers(1).build(),
            region_budget.clone(),
            8,
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
