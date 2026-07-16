#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use dashmap::{DashMap, mapref::entry::Entry};
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex, Notify},
};

use crate::layout::ResourceKey;

/// One consumer's contribution to the aggregate demand. `read_pos` is
/// shared with the consumer (advances seen without an update call);
/// `look_ahead = None` means "whole file" and collapses the watermark to
/// `u64::MAX`.
pub(crate) struct DemandEntry {
    read_pos: Arc<AtomicU64>,
    look_ahead: Option<u64>,
}

impl DemandEntry {
    pub(crate) fn new(read_pos: Arc<AtomicU64>, look_ahead: Option<u64>) -> Self {
        Self {
            read_pos,
            look_ahead,
        }
    }

    /// Per-entry watermark: how far this consumer wants bytes fetched.
    fn watermark(&self) -> u64 {
        self.look_ahead.map_or(u64::MAX, |la| {
            self.read_pos.load(Ordering::Acquire).saturating_add(la)
        })
    }
}

/// Shared per-resource demand state, owned by the [`DemandIndex`] map and
/// (for the elected producer) by a [`ProducerHandle`].
pub(crate) struct DemandCell {
    /// `true` once the single producer has been elected for this slot
    /// generation. CAS makes the election exactly-once.
    producer_spawned: AtomicBool,
    /// Attached consumer count. The slot is removed when this hits zero.
    refcount: AtomicUsize,
    /// Cancelled when the last consumer detaches. Child of the store
    /// cancel, so a store/master shutdown also cascades here.
    producer_cancel: CancelToken,
    /// Live consumer contributions. Small N (decks + waveform).
    entries: Mutex<Vec<Arc<DemandEntry>>>,
    /// Woken on attach and on consumer read-advance so the producer
    /// re-checks the aggregate watermark.
    notify: Notify,
}

impl DemandCell {
    fn new(producer_cancel: CancelToken) -> Self {
        Self {
            producer_cancel,
            entries: Mutex::default(),
            refcount: AtomicUsize::new(0),
            producer_spawned: AtomicBool::new(false),
            notify: Notify::default(),
        }
    }

    /// Aggregate demand: the max watermark over all live consumers.
    /// Zero when no consumer is attached.
    fn max_watermark(&self) -> u64 {
        self.entries
            .lock()
            .iter()
            .map(|e| e.watermark())
            .max()
            .unwrap_or(0)
    }
}

/// Producer-side handle returned to the CAS-winning attacher only.
///
/// Carries the shared [`DemandCell`] (for the aggregate watermark and the
/// wake notify) and the slot's `producer_cancel`. Drives the single
/// download task; its lifetime is the elected producer's. Dropping it
/// re-opens the election so a surviving consumer can take over -- see
/// [`DemandLease::try_take_producer`].
#[non_exhaustive]
pub struct ProducerHandle {
    slot: Arc<DemandCell>,
}

impl ProducerHandle {
    delegate::delegate! {
        to self.slot {
            /// Aggregate demand watermark across all live consumers.
            #[must_use]
            pub fn max_watermark(&self) -> u64;
            /// Producer wake notify (woken on attach / read-advance).
            #[must_use]
            #[field(&notify)]
            pub fn notify(&self) -> &Notify;
        }
    }

    /// Producer cancel token (fires when the last consumer detaches).
    #[must_use]
    pub fn producer_cancel(&self) -> CancelToken {
        self.slot.producer_cancel.clone()
    }
}

impl Drop for ProducerHandle {
    fn drop(&mut self) {
        // Re-open the election: a surviving consumer's next
        // `try_take_producer` wins and takes over the download. Harmless
        // when no consumer remains - the slot is removed by the last
        // `DemandLease` drop anyway.
        self.slot.producer_spawned.store(false, Ordering::Release);
        self.slot.notify.notify_one();
    }
}

/// RAII handle for one attached consumer.
///
/// On `Drop` it removes its entry and decrements the slot refcount under
/// the per-key shard lock; on zero it cancels the producer and removes
/// the slot. Both attach and detach serialize through that shard lock,
/// closing the attach-during-last-drop race.
#[non_exhaustive]
pub struct DemandLease {
    entry: Arc<DemandEntry>,
    inner: Arc<DemandInner>,
    slot: Arc<DemandCell>,
    key: ResourceKey,
}

impl DemandLease {
    /// Wake the producer so it re-reads the aggregate watermark.
    /// Called by the consumer when its read position advances.
    pub fn note_progress(&self) {
        self.slot.notify.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn producer_cancel_for_test(&self) -> CancelToken {
        self.slot.producer_cancel.clone()
    }

    /// Try to become the slot's producer. Returns `Some(ProducerHandle)`
    /// only when no producer is currently elected (the CAS winner). A
    /// loser whose elected producer has dropped uses this to take over
    /// the download. Returns `None` while another producer holds the
    /// slot.
    #[must_use]
    pub fn try_take_producer(&self) -> Option<ProducerHandle> {
        self.slot
            .producer_spawned
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then(|| ProducerHandle {
                slot: Arc::clone(&self.slot),
            })
    }
}

impl Drop for DemandLease {
    fn drop(&mut self) {
        // Serialize with attach through the per-key shard lock.
        if let Entry::Occupied(occupied) = self.inner.slots.entry(self.key.clone()) {
            let slot = occupied.get();
            slot.entries.lock().retain(|e| !Arc::ptr_eq(e, &self.entry));
            let prev = slot.refcount.fetch_sub(1, Ordering::AcqRel);
            if prev <= 1 {
                slot.producer_cancel.cancel();
                occupied.remove();
            } else {
                slot.notify.notify_one();
            }
        }
    }
}

struct DemandInner {
    /// Parent of every slot's `producer_cancel` (the store cancel).
    cancel: CancelToken,
    slots: DashMap<ResourceKey, Arc<DemandCell>>,
}

/// Opaque handle to the per-resource consumer demand index.
///
/// Cheap to [`Clone`] (one `Arc` bump); all clones share the same slot
/// map, so demand aggregates across `AssetStore` clones automatically.
#[derive(Clone)]
pub(crate) struct DemandIndex {
    inner: Arc<DemandInner>,
}

impl DemandIndex {
    /// Create an empty index. `cancel` is the store cancel; each slot's
    /// `producer_cancel` is a child of it.
    pub(crate) fn new(cancel: CancelToken) -> Self {
        Self {
            inner: Arc::new(DemandInner {
                cancel,
                slots: DashMap::new(),
            }),
        }
    }

    /// Attach a consumer's demand for `key`, returning a [`DemandLease`]
    /// (always) and a [`ProducerHandle`] to the single CAS-winning
    /// attacher only. Runs under the per-key shard lock so it cannot
    /// interleave with a concurrent last-drop.
    pub(crate) fn attach_demand(
        &self,
        key: &ResourceKey,
        entry: Arc<DemandEntry>,
    ) -> (DemandLease, Option<ProducerHandle>) {
        let slot = match self.inner.slots.entry(key.clone()) {
            Entry::Occupied(occupied) => {
                let slot = Arc::clone(occupied.get());
                slot.entries.lock().push(Arc::clone(&entry));
                slot.refcount.fetch_add(1, Ordering::AcqRel);
                slot
            }
            Entry::Vacant(vacant) => {
                let slot = Arc::new(DemandCell::new(self.inner.cancel.child()));
                slot.entries.lock().push(Arc::clone(&entry));
                slot.refcount.store(1, Ordering::Release);
                vacant.insert(Arc::clone(&slot));
                slot
            }
        };

        slot.notify.notify_one();

        let producer = slot
            .producer_spawned
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then(|| ProducerHandle {
                slot: Arc::clone(&slot),
            });

        let lease = DemandLease {
            slot,
            entry,
            inner: Arc::clone(&self.inner),
            key: key.clone(),
        };
        (lease, producer)
    }

    #[cfg(test)]
    pub(crate) fn has_slot_for_test(&self, key: &ResourceKey) -> bool {
        self.inner.slots.contains_key(key)
    }
}

impl std::fmt::Debug for DemandIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DemandIndex")
            .field("tracked_resources", &self.inner.slots.len())
            .finish()
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use kithara_platform::{CancelToken, sync::Arc, time::Duration};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::layout::ResourceKey;

    fn entry(read_pos: u64, look_ahead: Option<u64>) -> Arc<DemandEntry> {
        Arc::new(DemandEntry::new(
            Arc::new(AtomicU64::new(read_pos)),
            look_ahead,
        ))
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn attach_refcount_and_single_producer_election() {
        let index = DemandIndex::new(CancelToken::never());
        let key = ResourceKey::relative("asset", "file");

        let attach_count = AtomicUsize::new(0);
        let producer_wins = AtomicUsize::new(0);

        let mut leases = Vec::new();
        // Hold every elected handle: a dropped `ProducerHandle` re-opens
        // the election, so the single-winner invariant only holds while
        // the elected producer stays alive.
        let mut producers = Vec::new();
        for _ in 0..8 {
            let (lease, producer) = index.attach_demand(&key, entry(0, Some(64)));
            attach_count.fetch_add(1, Ordering::Relaxed);
            if let Some(handle) = producer {
                producer_wins.fetch_add(1, Ordering::Relaxed);
                producers.push(handle);
            }
            leases.push(lease);
        }

        assert_eq!(attach_count.load(Ordering::Relaxed), 8);
        assert_eq!(
            producer_wins.load(Ordering::Relaxed),
            1,
            "exactly one attacher wins the producer election"
        );
        drop(producers);

        // Last detach cancels the producer and removes the slot.
        let producer_cancel = leases[0].producer_cancel_for_test();
        assert!(!producer_cancel.is_cancelled());
        drop(leases);
        assert!(
            producer_cancel.is_cancelled(),
            "dropping the last lease cancels producer_cancel"
        );
        assert!(
            !index.has_slot_for_test(&key),
            "dropping the last lease removes the slot"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn watermark_is_max_over_entries() {
        let index = DemandIndex::new(CancelToken::never());
        let key = ResourceKey::relative("asset", "file");

        // Bounded entry: read_pos 10 + look_ahead 50 = 60.
        let (_l1, producer) = index.attach_demand(&key, entry(10, Some(50)));
        let producer = producer.expect("first attach wins the producer");
        assert_eq!(producer.max_watermark(), 60);

        // Unbounded entry collapses the aggregate to u64::MAX.
        let (_l2, none) = index.attach_demand(&key, entry(0, None));
        assert!(none.is_none(), "second attach is not the producer");
        assert_eq!(producer.max_watermark(), u64::MAX);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn watermark_tracks_read_pos_advance() {
        let index = DemandIndex::new(CancelToken::never());
        let key = ResourceKey::relative("asset", "file");
        let read_pos = Arc::new(AtomicU64::new(0));

        let (lease, producer) = index.attach_demand(
            &key,
            Arc::new(DemandEntry::new(Arc::clone(&read_pos), Some(100))),
        );
        let producer = producer.expect("first attach wins the producer");
        assert_eq!(producer.max_watermark(), 100);

        read_pos.store(500, Ordering::Release);
        lease.note_progress();
        assert_eq!(producer.max_watermark(), 600);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn detach_one_of_two_keeps_slot_and_producer() {
        let index = DemandIndex::new(CancelToken::never());
        let key = ResourceKey::relative("asset", "file");

        let (l1, producer) = index.attach_demand(&key, entry(0, Some(10)));
        let producer = producer.expect("first attach wins");
        let (l2, _none) = index.attach_demand(&key, entry(0, Some(10)));

        drop(l1);
        assert!(
            !producer.producer_cancel().is_cancelled(),
            "producer survives while one consumer remains"
        );
        assert!(index.has_slot_for_test(&key));

        drop(l2);
        assert!(producer.producer_cancel().is_cancelled());
        assert!(!index.has_slot_for_test(&key));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn reattach_after_last_detach_wins_producer_election() {
        let index = DemandIndex::new(CancelToken::never());
        let key = ResourceKey::relative("asset", "file");

        let (l1, _producer) = index.attach_demand(&key, entry(0, None));
        // Probe: slot is live, second attach must not win the election.
        let (l2, probe) = index.attach_demand(&key, entry(0, None));
        assert!(
            probe.is_none(),
            "second attach while slot is live is not a producer"
        );

        drop(l1);
        drop(l2);
        assert!(
            !index.has_slot_for_test(&key),
            "slot removed after last detach"
        );

        // Fresh attach must win the producer election on the cleared slot.
        let (_l3, new_producer) = index.attach_demand(&key, entry(0, None));
        assert!(
            new_producer.is_some(),
            "reattach after slot removal wins the producer election"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn dropping_producer_lets_a_survivor_take_over() {
        let index = DemandIndex::new(CancelToken::never());
        let key = ResourceKey::relative("asset", "file");

        let (winner_lease, producer) = index.attach_demand(&key, entry(0, None));
        let producer = producer.expect("first attach wins the producer");
        let (survivor_lease, none) = index.attach_demand(&key, entry(0, None));
        assert!(none.is_none(), "second attach is not the producer");

        // While the producer is alive the survivor cannot take over.
        assert!(
            survivor_lease.try_take_producer().is_none(),
            "election stays closed while the producer is alive"
        );

        // Producer abandons the download (its peer/source dropped) but the
        drop(producer);
        drop(winner_lease);
        assert!(
            index.has_slot_for_test(&key),
            "slot survives while one consumer remains"
        );

        let taken = survivor_lease
            .try_take_producer()
            .expect("survivor takes over the abandoned slot");
        assert_eq!(taken.max_watermark(), u64::MAX);
        assert!(
            survivor_lease.try_take_producer().is_none(),
            "only one survivor takes over"
        );
    }
}
