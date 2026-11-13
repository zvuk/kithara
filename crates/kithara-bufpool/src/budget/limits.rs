use std::{
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};

use kithara_platform::sync::{Arc, OnceLock, Weak};

use super::counter::BudgetCounter;

pub(crate) trait IdleReclaimer: Send + Sync {
    fn reclaim(&self, bytes: usize) -> usize;
}

type ReclaimerSlots = Box<[Weak<dyn IdleReclaimer>]>;

struct IdleReclaimers {
    next: AtomicUsize,
    slots: OnceLock<ReclaimerSlots>,
}

/// Hard byte limit shared by every pool in one region.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OverallBudget(pub usize);

#[derive(Clone)]
pub(crate) struct RegionBudget {
    reclaimers: Arc<IdleReclaimers>,
    counter: BudgetCounter,
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

    pub(crate) fn install_reclaimers(
        &self,
        reclaimers: ReclaimerSlots,
    ) -> Result<(), ReclaimerSlots> {
        self.reclaimers.slots.set(reclaimers)
    }

    pub(crate) fn release(&self, amount: usize) {
        if amount > 0 {
            let _ = self.counter.release(amount, "buffer region");
        }
    }

    pub(crate) fn reserve(&self, amount: usize) -> Result<Reservation<'_>, BudgetSnapshot> {
        if amount > 0 {
            self.counter.try_acquire(amount)?;
        }
        Ok(Reservation {
            amount,
            budget: self,
        })
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

    pub(crate) fn same_region(&self, other: &Self) -> bool {
        self.counter.same_counter(&other.counter)
            && Arc::ptr_eq(&self.reclaimers, &other.reclaimers)
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

#[must_use = "dropping an uncommitted reservation rolls it back"]
pub(crate) struct Reservation<'a> {
    budget: &'a RegionBudget,
    amount: usize,
}

impl Reservation<'_> {
    pub(crate) fn commit(mut self) {
        self.amount = 0;
    }

    pub(crate) fn reduce(&mut self, amount: usize) {
        debug_assert!(amount <= self.amount);
        self.budget.release(amount);
        self.amount -= amount;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.budget.release(self.amount);
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

    use super::{BudgetCounter, IdleReclaimer, RegionBudget};

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

    #[kithara::test(miri)]
    fn underflow_release_keeps_the_charge() {
        let counter = BudgetCounter::new(16);
        counter
            .try_acquire(8)
            .unwrap_or_else(|snapshot| panic!("{snapshot:?}"));

        assert!(!counter.release(9, "test"));
        assert_eq!(counter.current(), 8);
    }

    #[kithara::test(miri)]
    fn region_reclaims_the_exact_deficit_and_rotates_the_first_slot() {
        let budget = RegionBudget::new(1);
        let first = Arc::new(RecordingReclaimer::default());
        let second = Arc::new(RecordingReclaimer::default());
        let first_slot: Arc<dyn IdleReclaimer> = first.clone();
        let second_slot: Arc<dyn IdleReclaimer> = second.clone();
        budget
            .install_reclaimers([Arc::downgrade(&first_slot), Arc::downgrade(&second_slot)].into())
            .expect("reclaimer inventory installs once");

        assert_eq!(budget.reclaim(7), 7);
        assert_eq!(budget.reclaim(5), 5);

        assert_eq!(first.calls.load(Ordering::Relaxed), 1);
        assert_eq!(first.requested.load(Ordering::Relaxed), 7);
        assert_eq!(second.calls.load(Ordering::Relaxed), 1);
        assert_eq!(second.requested.load(Ordering::Relaxed), 5);
    }
}
