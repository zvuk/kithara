use std::sync::atomic::{AtomicUsize, Ordering};

use kithara_platform::sync::Arc;

use crate::growth::BudgetExhausted;

/// Maximum total bytes a pool may track across all live buffers.
///
/// Newtype so the byte-budget cap is unmistakable at call sites
/// (`with_byte_budget(.., .., ByteBudget(256 * MB))` instead of three
/// adjacent `usize`s where order is easy to swap). Pass
/// `ByteBudget(usize::MAX)` for no cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteBudget(pub usize);

#[derive(Clone, Debug)]
pub(crate) struct RegionBudget {
    inner: Arc<RegionBudgetInner>,
}

#[derive(Debug)]
struct RegionBudgetInner {
    allocated_bytes: AtomicUsize,
    max_bytes: usize,
}

impl RegionBudget {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(RegionBudgetInner {
                allocated_bytes: AtomicUsize::new(0),
                max_bytes,
            }),
        }
    }

    pub(crate) fn allocated_bytes(&self) -> usize {
        self.inner.allocated_bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn max_bytes(&self) -> usize {
        self.inner.max_bytes
    }

    pub(crate) fn release(&self, amount: usize) {
        if amount == 0 {
            return;
        }
        let mut current = self.inner.allocated_bytes.load(Ordering::Relaxed);
        loop {
            let new = current.saturating_sub(amount);
            match self.inner.allocated_bytes.compare_exchange_weak(
                current,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn request(&self, additional: usize) -> Result<(), BudgetExhausted> {
        if additional == 0 {
            return Ok(());
        }
        let mut current = self.inner.allocated_bytes.load(Ordering::Relaxed);
        loop {
            let new = current.checked_add(additional).ok_or(BudgetExhausted)?;
            if new > self.inner.max_bytes {
                return Err(BudgetExhausted);
            }
            match self.inner.allocated_bytes.compare_exchange_weak(
                current,
                new,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn track_byte_delta(&self, before: usize, after: usize) {
        if after > before {
            self.inner
                .allocated_bytes
                .fetch_add(after - before, Ordering::Relaxed);
        } else if before > after {
            self.release(before - after);
        }
    }
}
