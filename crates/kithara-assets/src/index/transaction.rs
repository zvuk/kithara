#![forbid(unsafe_code)]

use std::{
    future::Future,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use dashmap::{DashMap, mapref::entry::Entry};
use kithara_platform::sync::{Arc, Notify};

use crate::ResourceKey;

struct TransactionCell {
    held: AtomicBool,
    participants: AtomicUsize,
    notify: Notify,
}

impl Default for TransactionCell {
    fn default() -> Self {
        Self {
            held: AtomicBool::new(false),
            participants: AtomicUsize::new(1),
            notify: Notify::default(),
        }
    }
}

#[derive(Default)]
struct TransactionInner {
    cells: DashMap<ResourceKey, Arc<TransactionCell>>,
}

struct ResourceTransaction {
    cell: Arc<TransactionCell>,
    inner: Arc<TransactionInner>,
    key: ResourceKey,
    held: bool,
}

impl Drop for ResourceTransaction {
    fn drop(&mut self) {
        let should_notify = if self.held {
            self.cell.held.store(false, Ordering::Release);
            true
        } else {
            // A cancelled waiter may have consumed the only unlock notification.
            !self.cell.held.load(Ordering::Acquire)
        };
        if should_notify {
            self.cell.notify.notify_one();
        }

        if let Entry::Occupied(entry) = self.inner.cells.entry(self.key.clone())
            && Arc::ptr_eq(entry.get(), &self.cell)
            && self.cell.participants.fetch_sub(1, Ordering::AcqRel) == 1
        {
            entry.remove();
        }
    }
}

/// Shared process-local registry for per-resource cache transactions.
#[derive(Clone, Default)]
pub(crate) struct ResourceTransactionIndex {
    inner: Arc<TransactionInner>,
}

impl ResourceTransactionIndex {
    async fn acquire(&self, key: &ResourceKey) -> ResourceTransaction {
        let cell = match self.inner.cells.entry(key.clone()) {
            Entry::Occupied(entry) => {
                entry.get().participants.fetch_add(1, Ordering::AcqRel);
                Arc::clone(entry.get())
            }
            Entry::Vacant(entry) => {
                let cell = Arc::new(TransactionCell::default());
                entry.insert(Arc::clone(&cell));
                cell
            }
        };
        let mut transaction = ResourceTransaction {
            cell,
            inner: Arc::clone(&self.inner),
            key: key.clone(),
            held: false,
        };

        loop {
            if transaction
                .cell
                .held
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                transaction.held = true;
                return transaction;
            }
            transaction.cell.notify.notified().await;
        }
    }

    pub(crate) async fn run<T, F, Fut>(&self, key: &ResourceKey, operation: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let _transaction = self.acquire(key).await;
        operation().await
    }
}

impl std::fmt::Debug for ResourceTransactionIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceTransactionIndex")
            .field("active_resources", &self.inner.cells.len())
            .finish()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use kithara_platform::{
        time::Duration,
        tokio::{
            join,
            task::{self, yield_now},
        },
    };
    use kithara_test_utils::kithara;

    use super::*;

    async fn observe_concurrency(active: &AtomicUsize, max_active: &AtomicUsize) {
        let current = active.fetch_add(1, Ordering::AcqRel) + 1;
        max_active.fetch_max(current, Ordering::AcqRel);
        yield_now().await;
        active.fetch_sub(1, Ordering::AcqRel);
    }

    async fn wait_for_participants(
        index: &ResourceTransactionIndex,
        key: &ResourceKey,
        expected: usize,
    ) {
        loop {
            let participants = index
                .inner
                .cells
                .get(key)
                .map_or(0, |cell| cell.participants.load(Ordering::Acquire));
            if participants == expected {
                return;
            }
            yield_now().await;
        }
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(1)))]
    async fn same_key_operations_are_serialized_and_released() {
        let index = ResourceTransactionIndex::default();
        let key = ResourceKey::relative("asset", "master.m3u8");
        let active = AtomicUsize::new(0);
        let max_active = AtomicUsize::new(0);

        join!(
            index.run(&key, || observe_concurrency(&active, &max_active)),
            index.run(&key, || observe_concurrency(&active, &max_active)),
        );

        assert_eq!(max_active.load(Ordering::Acquire), 1);
        assert!(index.inner.cells.is_empty());
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(1)))]
    async fn different_keys_remain_parallel() {
        let index = ResourceTransactionIndex::default();
        let first = ResourceKey::relative("asset", "master.m3u8");
        let second = ResourceKey::relative("asset", "media.m3u8");
        let active = AtomicUsize::new(0);
        let max_active = AtomicUsize::new(0);

        join!(
            index.run(&first, || observe_concurrency(&active, &max_active)),
            index.run(&second, || observe_concurrency(&active, &max_active)),
        );

        assert_eq!(max_active.load(Ordering::Acquire), 2);
        assert!(index.inner.cells.is_empty());
    }

    #[kithara::test(tokio, timeout(Duration::from_secs(1)))]
    async fn cancelled_notified_waiter_hands_off_and_cleans_up() {
        let index = ResourceTransactionIndex::default();
        let key = ResourceKey::relative("asset", "master.m3u8");
        let holder = index.acquire(&key).await;

        let first = {
            let index = index.clone();
            let key = key.clone();
            task::spawn(async move { index.run(&key, || async {}).await })
        };
        wait_for_participants(&index, &key, 2).await;

        let second = {
            let index = index.clone();
            let key = key.clone();
            task::spawn(async move { index.run(&key, || async {}).await })
        };
        wait_for_participants(&index, &key, 3).await;

        drop(holder);
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());

        kithara_platform::time::timeout(Duration::from_millis(100), second)
            .await
            .expect("remaining waiter must receive the released transaction")
            .expect("remaining waiter task must complete");
        assert!(index.inner.cells.is_empty());
    }
}
