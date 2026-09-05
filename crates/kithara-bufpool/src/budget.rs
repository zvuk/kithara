mod counter;
mod pair;

use std::{
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};

use counter::BudgetCounter;
use kithara_platform::sync::{Arc, OnceLock, Weak};
pub(crate) use pair::{BudgetPair, Reservation, ReserveFailure};

pub(crate) trait IdleReclaimer: Send + Sync {
    fn reclaim(&self, bytes: usize) -> usize;
}

type ReclaimerSlots = Box<[Weak<dyn IdleReclaimer>]>;

struct IdleReclaimers {
    slots: OnceLock<ReclaimerSlots>,
    next: AtomicUsize,
}

/// Hard byte limit shared by every pool in one region.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverallBudget(pub usize);

/// Percentage of the overall budget available to one physical pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Percent(pub u8);

impl Percent {
    /// The selected pool may compete for the entire region budget.
    pub const FULL: Self = Self(100);

    pub(crate) const fn is_valid(self) -> bool {
        self.0 <= Self::FULL.0
    }
}

#[derive(Clone)]
pub(crate) struct RegionBudget {
    counter: BudgetCounter,
    reclaimers: Arc<IdleReclaimers>,
}

impl RegionBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            counter: BudgetCounter::new(limit),
            reclaimers: Arc::new(IdleReclaimers {
                slots: OnceLock::new(),
                next: AtomicUsize::new(0),
            }),
        }
    }

    pub(crate) fn same_region(&self, other: &Self) -> bool {
        self.counter.same_counter(&other.counter)
            && Arc::ptr_eq(&self.reclaimers, &other.reclaimers)
    }

    pub(crate) fn install_reclaimers(
        &self,
        reclaimers: ReclaimerSlots,
    ) -> Result<(), ReclaimerSlots> {
        self.reclaimers.slots.set(reclaimers)
    }

    pub(crate) fn reclaim(&self, target: usize) -> usize {
        if target == 0 {
            return 0;
        }
        let Some(reclaimers) = self.reclaimers.slots.get() else {
            return 0;
        };
        if reclaimers.is_empty() {
            return 0;
        }
        let start = self.reclaimers.next.fetch_add(1, Ordering::Relaxed) % reclaimers.len();
        let mut released = 0usize;
        for offset in 0..reclaimers.len() {
            let reclaimer = &reclaimers[start.wrapping_add(offset) % reclaimers.len()];
            let Some(reclaimer) = reclaimer.upgrade() else {
                continue;
            };
            released = released.saturating_add(reclaimer.reclaim(target.saturating_sub(released)));
            if released >= target {
                break;
            }
        }
        released
    }

    delegate::delegate! {
        to self.counter {
            pub(crate) fn current(&self) -> usize;
            pub(crate) fn limit(&self) -> usize;
            pub(crate) fn peak(&self) -> usize;
        }
    }
}

impl fmt::Debug for RegionBudget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegionBudget")
            .field("current", &self.current())
            .field("limit", &self.limit())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PoolBudget(BudgetCounter);

impl PoolBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self(BudgetCounter::new(limit))
    }

    delegate::delegate! {
        to self.0 {
            pub(crate) fn current(&self) -> usize;
            pub(crate) fn limit(&self) -> usize;
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct BudgetSnapshot {
    pub(crate) current: usize,
    pub(crate) limit: usize,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::{BudgetCounter, BudgetPair, IdleReclaimer, RegionBudget};

    #[derive(Default)]
    struct RecordingReclaimer {
        calls: AtomicUsize,
        requested: AtomicUsize,
    }

    impl IdleReclaimer for RecordingReclaimer {
        fn reclaim(&self, bytes: usize) -> usize {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.requested.store(bytes, Ordering::Relaxed);
            bytes
        }
    }

    #[kithara::test]
    fn uncommitted_reservation_rolls_back_both_counters() {
        let pair = BudgetPair::new(RegionBudget::new(16), 16);
        let reservation = pair.reserve(8).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(pair.region_current(), 8);
        assert_eq!(pair.current(), 8);

        drop(reservation);

        assert_eq!(pair.region_current(), 0);
        assert_eq!(pair.current(), 0);
    }

    #[kithara::test]
    fn underflow_release_keeps_the_charge() {
        let counter = BudgetCounter::new(16);
        counter
            .try_acquire(8)
            .unwrap_or_else(|snapshot| panic!("{snapshot:?}"));

        assert!(!counter.release(9, "test"));
        assert_eq!(counter.current(), 8);
    }

    #[kithara::test]
    fn region_reclaims_the_exact_deficit_and_rotates_the_first_slot() {
        let budget = RegionBudget::new(1);
        let first = Arc::new(RecordingReclaimer::default());
        let second = Arc::new(RecordingReclaimer::default());
        let first_slot: Arc<dyn IdleReclaimer> = first.clone();
        let second_slot: Arc<dyn IdleReclaimer> = second.clone();
        budget
            .install_reclaimers([Arc::downgrade(&first_slot), Arc::downgrade(&second_slot)].into())
            .unwrap_or_else(|_| panic!("reclaimer inventory installs once"));

        assert_eq!(budget.reclaim(7), 7);
        assert_eq!(budget.reclaim(5), 5);

        assert_eq!(first.calls.load(Ordering::Relaxed), 1);
        assert_eq!(first.requested.load(Ordering::Relaxed), 7);
        assert_eq!(second.calls.load(Ordering::Relaxed), 1);
        assert_eq!(second.requested.load(Ordering::Relaxed), 5);
    }
}
