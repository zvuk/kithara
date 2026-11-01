#![forbid(unsafe_code)]

use std::{
    fmt,
    sync::atomic::{AtomicU64, Ordering},
    task::Waker,
};

use dashmap::{DashMap, mapref::entry::Entry};
use kithara_bufpool::HasPool;
use kithara_platform::{
    CancelToken,
    sync::{Arc, Mutex},
};

use super::pending_resource::{
    PendingResource, PendingResourceSession, RemoveResource, ResourceAttachment, ResourceLease,
    SessionPhase,
};
use crate::{
    error::AssetsResult,
    layout::ResourceKey,
    resource::AcquisitionResult,
    store::{AssetReader, AssetStore, ResourceAcquisition},
};

/// One consumer's contribution to the aggregate demand. `read_pos` is
/// shared with the consumer (advances seen without an update call);
/// `look_ahead = None` means "whole file" and collapses the watermark to
/// `u64::MAX`.
pub(crate) struct DemandEntry {
    read_pos: Arc<AtomicU64>,
    requested_end: AtomicU64,
    peer_waker: Mutex<Option<Waker>>,
    reader_waker: Mutex<Option<Waker>>,
    look_ahead: Option<u64>,
}

impl DemandEntry {
    pub(crate) fn new(read_pos: Arc<AtomicU64>, look_ahead: Option<u64>) -> Self {
        Self {
            read_pos,
            look_ahead,
            requested_end: AtomicU64::new(0),
            peer_waker: Mutex::new(None),
            reader_waker: Mutex::new(None),
        }
    }

    pub(in crate::index) fn clear_peer_waker(&self, waker: &Waker) {
        let old = {
            let mut current = self.peer_waker.lock();
            if current
                .as_ref()
                .is_some_and(|registered| registered.will_wake(waker))
            {
                current.take()
            } else {
                None
            }
        };
        drop(old);
    }

    pub(in crate::index) fn register_peer_waker(&self, waker: &Waker) {
        register_waker(&self.peer_waker, waker);
    }

    pub(in crate::index) fn register_reader_waker(&self, waker: &Waker) {
        register_waker(&self.reader_waker, waker);
    }

    pub(in crate::index) fn request_until(&self, end: u64) -> bool {
        self.requested_end.fetch_max(end, Ordering::AcqRel) < end
    }

    pub(in crate::index) fn take_peer_waker(&self) -> Option<Waker> {
        self.peer_waker.lock().take()
    }

    pub(in crate::index) fn take_reader_waker(&self) -> Option<Waker> {
        self.reader_waker.lock().take()
    }

    /// Per-entry watermark: how far this consumer wants bytes fetched.
    pub(in crate::index) fn watermark(&self) -> u64 {
        let prefetch = self.look_ahead.map_or(u64::MAX, |la| {
            self.read_pos.load(Ordering::Acquire).saturating_add(la)
        });
        prefetch.max(self.requested_end.load(Ordering::Acquire))
    }
}

fn register_waker(slot: &Mutex<Option<Waker>>, waker: &Waker) {
    let replacement = waker.clone();
    let (old, unused) = {
        let mut current = slot.lock();
        if current
            .as_ref()
            .is_none_or(|registered| !registered.will_wake(waker))
        {
            (current.replace(replacement), None)
        } else {
            (None, Some(replacement))
        }
    };
    drop(old);
    drop(unused);
}

pub(in crate::index) struct PendingResourceInner<S> {
    /// Parent of every slot's `writer_cancel` (the store cancel).
    pub(in crate::index) cancel: CancelToken,
    pub(in crate::index) slots: DashMap<ResourceKey, Arc<PendingResource<S>>>,
}

/// Opaque index of resources that are not yet ready in this store.
///
/// Cheap to [`Clone`] (one `Arc` bump); all clones share the same slot
/// map, so consumer demand aggregates across `AssetStore` clones automatically.
#[derive_where::derive_where(Clone)]
pub(crate) struct PendingResourceIndex<S> {
    inner: Arc<PendingResourceInner<S>>,
}

impl<S> PendingResourceIndex<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Create an empty index. `cancel` is the store cancel; each slot's
    /// `writer_cancel` is a child of it.
    pub(crate) fn new(cancel: CancelToken) -> Self {
        Self {
            inner: Arc::new(PendingResourceInner {
                cancel,
                slots: DashMap::new(),
            }),
        }
    }

    pub(crate) fn attach_pending_resource(
        &self,
        key: &ResourceKey,
        entry: Arc<DemandEntry>,
        store: AssetStore<S>,
        remove: RemoveResource,
        acquire: impl FnOnce() -> AssetsResult<ResourceAcquisition<S>>,
    ) -> AssetsResult<AcquisitionResult<ResourceAttachment<S>, AssetReader<S>>> {
        let (slot, epoch, reader, peer_waker) = match self.inner.slots.entry(key.clone()) {
            Entry::Occupied(occupied) => {
                let slot = Arc::clone(occupied.get());
                let mut state = slot.state.lock();
                match &state.phase {
                    SessionPhase::CleanupFailed(failure) => {
                        return Err(failure.into());
                    }
                    SessionPhase::Committed => {
                        panic!("BUG: committed pending resource remained attachable");
                    }
                    SessionPhase::Active => {}
                    SessionPhase::Finishing => {
                        panic!("BUG: finishing pending resource remained attachable");
                    }
                }
                state.entries.push(Arc::clone(&entry));
                let epoch = slot.elect_writer(&mut state, &entry);
                let Some(reader) = state.reader.as_ref().cloned() else {
                    panic!("BUG: active pending resource lost its reader");
                };
                let peer_waker = state.current_peer_waker();
                drop(state);
                drop(occupied);
                (slot, epoch, reader, peer_waker)
            }
            Entry::Vacant(vacant) => {
                let mut writer = match acquire()? {
                    AcquisitionResult::Pending(writer) => writer,
                    AcquisitionResult::Ready(reader) => {
                        return Ok(AcquisitionResult::Ready(reader));
                    }
                    AcquisitionResult::Joined(reader) => {
                        return Ok(AcquisitionResult::Joined(reader));
                    }
                };
                writer.transfer_cleanup();
                let slot = Arc::new(PendingResource::new(
                    self.inner.cancel.child(),
                    Arc::clone(&entry),
                    writer,
                    remove,
                ));
                let (epoch, reader) = {
                    let mut state = slot.state.lock();
                    let epoch = slot.elect_writer(&mut state, &entry);
                    let Some(reader) = state.reader.as_ref().cloned() else {
                        panic!("BUG: new pending resource lost its reader");
                    };
                    drop(state);
                    (epoch, reader)
                };
                vacant.insert(Arc::clone(&slot));
                (slot, epoch, reader, None)
            }
        };

        if let Some(waker) = peer_waker {
            waker.wake();
        }
        let session = PendingResourceSession::new(&self.inner, key, &slot);
        let lease = ResourceLease::new(entry, session, store);
        let writer = epoch.map(|claim| lease.writer(claim));
        Ok(AcquisitionResult::Pending(ResourceAttachment {
            reader,
            writer,
            lease,
        }))
    }
}

impl<S> fmt::Debug for PendingResourceIndex<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingResourceIndex")
            .field("tracked_resources", &self.inner.slots.len())
            .finish()
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    mod cleanup {
        use super::*;

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn cleanup_failure_blocks_successor_publication() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "cleanup-error");
            let remove_calls = Arc::new(AtomicUsize::new(0));
            let counted_remove = Arc::clone(&remove_calls);
            let remove: RemoveResource = Arc::new(move |_| {
                counted_remove.fetch_add(1, Ordering::SeqCst);
                Err(AssetsError::InvalidKey)
            });
            let acquisition = index
                .attach_pending_resource(
                    &key,
                    entry(0, None),
                    store.clone(),
                    Arc::clone(&remove),
                    || store.acquire_resource(&key, None),
                )
                .expect("initial attachment");
            let AcquisitionResult::Pending(attachment) = acquisition else {
                panic!("fresh resource must start an acquisition session");
            };
            let (reader, lease, writer) = attachment.into();
            let writer = writer.expect("initial attachment owns the writer");
            writer
                .epoch()
                .write_at(0, b"live")
                .current()
                .expect("writer epoch remains current")
                .expect("writer write");
            assert!(store.contains_range(&key, 0..4));
            drop(writer);
            drop(reader);
            drop(lease);

            assert!(
                index.inner.slots.contains_key(&key),
                "failed cleanup must keep an exact tombstone"
            );
            assert_eq!(
                remove_calls.load(Ordering::SeqCst),
                1,
                "the acquisition session is the sole physical cleanup owner"
            );
            assert!(matches!(
                store.resource_state(&key).expect("resource state"),
                AssetResourceState::Active
            ));
            assert!(
                store.contains_range(&key, 0..4),
                "failed canonical cleanup must not hide an earlier physical remove"
            );
            let successor =
                index.attach_pending_resource(&key, entry(0, None), store.clone(), remove, || {
                    store.acquire_resource(&key, None)
                });
            let Err(AssetsError::Storage(StorageError::Io(error))) = successor else {
                panic!("successor must receive typed cleanup failure");
            };
            let cleanup = error
                .get_ref()
                .and_then(|source| source.downcast_ref::<PendingResourceCleanupError>())
                .expect("io error retains typed cleanup carrier");
            assert_eq!(cleanup.key(), &key);
            assert!(cleanup.to_string().contains("cleanup-error"));
            let source = StdError::source(cleanup).expect("cleanup source");
            assert!(matches!(
                source.downcast_ref::<AssetsError>(),
                Some(AssetsError::InvalidKey)
            ));
            fn assert_send_sync<T: Send + Sync>() {}
            assert_send_sync::<PendingResourceCleanupError>();
        }
    }
    mod fixtures {
        use std::{
            sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
            task::{Wake, Waker},
        };

        use kithara_platform::{CancelToken, sync::Arc};

        use super::super::*;
        use crate::{
            AcquisitionResult, AssetStore, StorageBackend, WriterHandle, layout::ResourceKey,
        };

        type TestAssetStore = AssetStore<crate::test_pools::TestPools>;
        type TestPendingResourceIndex = PendingResourceIndex<crate::test_pools::TestPools>;
        type TestResourceLease = ResourceLease<crate::test_pools::TestPools>;
        type TestWriterHandle = WriterHandle<crate::test_pools::TestPools>;

        #[derive(Default)]
        pub(super) struct WakeCount(pub(super) AtomicUsize);

        pub(super) struct RearmReaderOnDrop {
            pub(super) dropped: Arc<AtomicBool>,
            pub(super) lease: Arc<TestResourceLease>,
            pub(super) replacement: Waker,
        }

        impl Wake for WakeCount {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        impl Wake for RearmReaderOnDrop {
            fn wake(self: Arc<Self>) {
                self.dropped.store(true, Ordering::SeqCst);
            }
        }

        impl Drop for RearmReaderOnDrop {
            fn drop(&mut self) {
                self.lease.register_reader_waker(&self.replacement);
                self.dropped.store(true, Ordering::SeqCst);
            }
        }

        pub(super) fn counting_waker() -> (Arc<WakeCount>, Waker) {
            let count = Arc::new(WakeCount::default());
            let waker = Waker::from(Arc::clone(&count));
            (count, waker)
        }

        pub(super) fn entry(read_pos: u64, look_ahead: Option<u64>) -> Arc<DemandEntry> {
            Arc::new(DemandEntry::new(
                Arc::new(AtomicU64::new(read_pos)),
                look_ahead,
            ))
        }

        pub(super) fn test_store() -> TestAssetStore {
            AssetStore::builder(crate::test_pools::pools())
                .backend(StorageBackend::Memory)
                .cancel(CancelToken::never())
                .build()
        }

        pub(super) fn attach(
            index: &TestPendingResourceIndex,
            store: &TestAssetStore,
            key: &ResourceKey,
            entry: Arc<DemandEntry>,
        ) -> (TestResourceLease, Option<TestWriterHandle>) {
            let remove_store = store.clone();
            let remove: RemoveResource = Arc::new(move |key| remove_store.remove_resource(key));
            let acquisition = index
                .attach_pending_resource(key, entry, store.clone(), remove, || {
                    store.acquire_resource(key, None)
                })
                .expect("test attachment");
            let AcquisitionResult::Pending(attachment) = acquisition else {
                panic!("test resource must be active");
            };
            let (_reader, lease, writer) = attachment.into();
            (lease, writer)
        }
    }
    mod lifecycle {
        use super::*;

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn attach_refcount_and_single_writer_election() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "file");

            let attach_count = AtomicUsize::new(0);
            let writer_wins = AtomicUsize::new(0);

            let mut leases = Vec::new();
            // Hold every elected handle: a dropped `WriterHandle` re-opens
            // the election, so the single-winner invariant only holds while
            // the elected writer stays alive.
            let mut writers = Vec::new();
            for _ in 0..8 {
                let (lease, writer) = attach(&index, &store, &key, entry(0, Some(64)));
                attach_count.fetch_add(1, Ordering::Relaxed);
                if let Some(handle) = writer {
                    writer_wins.fetch_add(1, Ordering::Relaxed);
                    writers.push(handle);
                }
                leases.push(lease);
            }

            assert_eq!(attach_count.load(Ordering::Relaxed), 8);
            assert_eq!(
                writer_wins.load(Ordering::Relaxed),
                1,
                "exactly one attacher wins the writer election"
            );
            drop(writers);

            // Last detach cancels the writer and removes the slot.
            let writer_cancel = leases[0].session_cancel();
            assert!(!writer_cancel.is_cancelled());
            drop(leases);
            assert!(
                writer_cancel.is_cancelled(),
                "dropping the last lease cancels writer_cancel"
            );
            assert!(
                !index.inner.slots.contains_key(&key),
                "dropping the last lease removes the slot"
            );
        }

        #[kithara::test]
        fn cancelled_session_does_not_reopen_writer_election() {
            let scope = CancelScope::new(None);
            let index = PendingResourceIndex::new(scope.token());
            let store = test_store();
            let key = ResourceKey::relative("asset", "file");
            let (lease, writer) = attach(&index, &store, &key, entry(0, Some(64)));
            drop(writer);

            scope.cancel();

            assert!(lease.try_take_writer().is_none());
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn watermark_is_max_over_entries() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "file");

            // Bounded entry: read_pos 10 + look_ahead 50 = 60.
            let (_l1, writer) = attach(&index, &store, &key, entry(10, Some(50)));
            let writer = writer.expect("first attach wins the writer");
            assert_eq!(writer.max_watermark(), 60);

            // Unbounded entry collapses the aggregate to u64::MAX.
            let (_l2, none) = attach(&index, &store, &key, entry(0, None));
            assert!(none.is_none(), "second attach is not the writer");
            assert_eq!(writer.max_watermark(), u64::MAX);
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn watermark_tracks_read_pos_advance() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "file");
            let read_pos = Arc::new(AtomicU64::new(0));

            let (lease, writer) = attach(
                &index,
                &store,
                &key,
                Arc::new(DemandEntry::new(Arc::clone(&read_pos), Some(100))),
            );
            let writer = writer.expect("first attach wins the writer");
            assert_eq!(writer.max_watermark(), 100);

            read_pos.store(500, Ordering::Release);
            lease.note_progress();
            assert_eq!(writer.max_watermark(), 600);
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn immediate_request_extends_bounded_watermark() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "file");
            let (lease, writer) = attach(&index, &store, &key, entry(0, Some(0)));
            let writer = writer.expect("first attach wins the writer");
            assert_eq!(writer.max_watermark(), 0);

            lease.request_until(32);

            assert_eq!(writer.max_watermark(), 32);
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn detach_one_of_two_keeps_slot_and_writer() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "file");

            let (l1, writer) = attach(&index, &store, &key, entry(0, Some(10)));
            let writer = writer.expect("first attach wins");
            let (l2, _none) = attach(&index, &store, &key, entry(0, Some(10)));

            drop(l1);
            assert!(
                !writer.writer_cancel().is_cancelled(),
                "writer survives while one consumer remains"
            );
            assert!(index.inner.slots.contains_key(&key));

            drop(l2);
            assert!(writer.writer_cancel().is_cancelled());
            assert!(!index.inner.slots.contains_key(&key));
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn reattach_after_last_detach_wins_writer_election() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "file");

            let (l1, _writer) = attach(&index, &store, &key, entry(0, None));
            // Probe: slot is live, second attach must not win the election.
            let (l2, probe) = attach(&index, &store, &key, entry(0, None));
            assert!(
                probe.is_none(),
                "second attach while slot is live is not a writer"
            );

            drop(l1);
            drop(l2);
            assert!(
                !index.inner.slots.contains_key(&key),
                "slot removed after last detach"
            );

            // Fresh attach must win the writer election on the cleared slot.
            let (_l3, new_writer) = attach(&index, &store, &key, entry(0, None));
            assert!(
                new_writer.is_some(),
                "reattach after slot removal wins the writer election"
            );
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn dropping_writer_lets_a_survivor_take_over() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "file");

            let (winner_lease, writer) = attach(&index, &store, &key, entry(0, None));
            let writer = writer.expect("first attach wins the writer");
            let (survivor_lease, none) = attach(&index, &store, &key, entry(0, None));
            assert!(none.is_none(), "second attach is not the writer");

            // While the writer is alive the survivor cannot take over.
            assert!(
                survivor_lease.try_take_writer().is_none(),
                "election stays closed while the writer is alive"
            );

            drop(writer);
            drop(winner_lease);
            assert!(
                index.inner.slots.contains_key(&key),
                "slot survives while one consumer remains"
            );

            let taken = survivor_lease
                .try_take_writer()
                .expect("survivor takes over the abandoned slot");
            assert_eq!(taken.max_watermark(), u64::MAX);
            assert!(
                survivor_lease.try_take_writer().is_none(),
                "only one survivor takes over"
            );
        }
    }
    mod session {
        use super::*;

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn follower_uses_the_elected_writers_session() {
            let store = AssetStore::builder(crate::test_pools::pools())
                .backend(StorageBackend::Memory)
                .cancel(CancelToken::never())
                .build();
            let key = ResourceKey::relative("asset", "file");

            let AcquisitionResult::Pending(first) = store
                .attach_pending_resource(&key, Arc::new(AtomicU64::new(0)), None)
                .expect("first attachment")
            else {
                panic!("fresh resource must start an acquisition session");
            };
            let (first_reader, _first_lease, writer) = first.into();
            let writer = writer.expect("first consumer owns the writer epoch");

            let AcquisitionResult::Pending(second) = store
                .attach_pending_resource(&key, Arc::new(AtomicU64::new(0)), Some(64))
                .expect("follower attachment")
            else {
                panic!("follower must join the active acquisition session");
            };
            let (second_reader, _second_lease, follower_writer) = second.into();
            assert!(follower_writer.is_none(), "follower must not own a writer");

            let epoch = writer.epoch();
            epoch
                .write_at(0, b"shared")
                .current()
                .expect("writer epoch remains current")
                .expect("writer write");
            epoch
                .commit(Some(6))
                .current()
                .expect("writer epoch remains current")
                .expect("writer commit");

            let mut first_bytes = [0; 6];
            let mut second_bytes = [0; 6];
            first_reader
                .read_at(0, &mut first_bytes)
                .expect("first reader");
            second_reader
                .read_at(0, &mut second_bytes)
                .expect("second reader");
            assert_eq!(&first_bytes, b"shared");
            assert_eq!(&second_bytes, b"shared");
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn committed_session_reopens_ready_after_consumers_detach() {
            let store = test_store();
            let key = ResourceKey::relative("asset", "committed");
            let AcquisitionResult::Pending(attachment) = store
                .attach_pending_resource(&key, Arc::new(AtomicU64::new(0)), None)
                .expect("initial attachment")
            else {
                panic!("fresh resource must start an acquisition session");
            };
            let (reader, lease, writer) = attachment.into();
            let writer = writer.expect("first consumer owns the writer epoch");
            let epoch = writer.epoch();
            epoch
                .write_at(0, b"ready")
                .current()
                .expect("writer epoch remains current")
                .expect("writer write");
            epoch
                .commit(Some(5))
                .current()
                .expect("writer epoch remains current")
                .expect("writer commit");
            drop(writer);
            drop(reader);
            drop(lease);

            let reopened = store
                .attach_pending_resource(&key, Arc::new(AtomicU64::new(0)), None)
                .expect("reopen committed resource");
            let AcquisitionResult::Ready(reader) = reopened else {
                panic!("committed session must reopen Ready");
            };
            let mut bytes = [0; 5];
            reader.read_at(0, &mut bytes).expect("committed read");
            assert_eq!(&bytes, b"ready");
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn writer_handoff_keeps_writer_and_rejects_old_epoch() {
            let store = test_store();
            let key = ResourceKey::relative("asset", "handoff");
            let AcquisitionResult::Pending(first) = store
                .attach_pending_resource(&key, Arc::new(AtomicU64::new(0)), None)
                .expect("first attachment")
            else {
                panic!("fresh resource must start an acquisition session");
            };
            let (first_reader, first_lease, first_writer) = first.into();
            let first_writer = first_writer.expect("first consumer owns the writer epoch");
            let first_epoch = first_writer.epoch();

            let AcquisitionResult::Pending(follower) = store
                .attach_pending_resource(&key, Arc::new(AtomicU64::new(0)), None)
                .expect("follower attachment")
            else {
                panic!("follower must join the active session");
            };
            let (follower_reader, follower_lease, follower_writer) = follower.into();
            assert!(follower_writer.is_none());

            first_epoch
                .write_at(0, b"old")
                .current()
                .expect("first epoch is current")
                .expect("first write");
            drop(first_writer);
            let next_writer = follower_lease
                .try_take_writer()
                .expect("surviving follower takes writer ownership");
            let next_epoch = next_writer.epoch();

            assert!(
                first_epoch.write_at(3, b"stale").current().is_none(),
                "old epoch write must be stale after handoff"
            );
            assert!(
                first_epoch.commit(Some(5)).current().is_none(),
                "old epoch commit must be stale after handoff"
            );
            assert!(
                first_epoch.relinquish().current().is_none(),
                "old epoch relinquish must be stale after handoff"
            );
            assert!(
                first_epoch
                    .fail("late writer failure".to_string())
                    .current()
                    .is_none(),
                "old epoch fail must be stale after handoff"
            );
            next_epoch
                .write_at(3, b"new")
                .current()
                .expect("new epoch is current")
                .expect("handoff write");
            next_epoch
                .commit(Some(6))
                .current()
                .expect("new epoch is current")
                .expect("handoff commit");

            let mut first_bytes = [0; 6];
            let mut follower_bytes = [0; 6];
            first_reader
                .read_at(0, &mut first_bytes)
                .expect("first reader");
            follower_reader
                .read_at(0, &mut follower_bytes)
                .expect("follower reader");
            assert_eq!(&first_bytes, b"oldnew");
            assert_eq!(&follower_bytes, b"oldnew");
            drop(first_lease);
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn follower_attach_clones_reader_before_current_failure() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "attach-fail");
            let (first_lease, first_writer) = attach(&index, &store, &key, entry(0, None));
            let first_writer = first_writer.expect("first writer");
            let first_epoch = first_writer.epoch();
            let slot = Arc::clone(
                index
                    .inner
                    .slots
                    .get(&key)
                    .expect("first attachment opened a slot")
                    .value(),
            );
            let state = slot.state.lock();
            let follower_index = index.clone();
            let follower_store = store.clone();
            let follower_key = key.clone();
            let follower = thread::spawn(move || {
                let remove_store = follower_store.clone();
                let acquire_store = follower_store.clone();
                let acquire_key = follower_key.clone();
                follower_index.attach_pending_resource(
                    &follower_key,
                    entry(0, None),
                    follower_store,
                    Arc::new(move |key| remove_store.remove_resource(key)),
                    move || acquire_store.acquire_resource(&acquire_key, None),
                )
            });

            while !matches!(index.inner.slots.try_get(&key), TryResult::Locked) {
                thread::yield_now();
            }
            let (failed_tx, failed_rx) = mpsc::channel();
            let failure = thread::spawn(move || {
                failed_tx
                    .send(first_epoch.fail("current failure".to_string()))
                    .expect("failure result receiver");
            });
            assert!(failed_rx.try_recv().is_err());

            drop(state);
            let follower = follower.join().expect("follower attach thread");
            assert!(matches!(follower, Ok(AcquisitionResult::Pending(_))));
            failure.join().expect("failure thread");
            failed_rx
                .recv()
                .expect("failure result")
                .current()
                .expect("first epoch remains current until follower attach completes")
                .expect("failure cleanup");
            drop(first_lease);
        }
    }
    mod wake {
        use super::*;

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn write_and_terminal_wake_every_attached_reader_and_peer() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "reader-wake");
            let (first_lease, writer) = attach(&index, &store, &key, entry(0, None));
            let (follower_lease, none) = attach(&index, &store, &key, entry(0, None));
            assert!(none.is_none());
            let writer = writer.expect("first writer");
            let writer_cancel = writer.writer_cancel();
            let epoch = writer.epoch();
            let (first_reader_count, first_reader_waker) = counting_waker();
            let (follower_reader_count, follower_reader_waker) = counting_waker();
            let (first_peer_count, first_peer_waker) = counting_waker();
            let (follower_peer_count, follower_peer_waker) = counting_waker();
            first_lease.register_reader_waker(&first_reader_waker);
            follower_lease.register_reader_waker(&follower_reader_waker);
            first_lease.register_peer_waker(&first_peer_waker);
            follower_lease.register_peer_waker(&follower_peer_waker);

            epoch
                .write_at(0, b"wake")
                .current()
                .expect("current write")
                .expect("write succeeds");
            assert_eq!(first_reader_count.0.load(Ordering::SeqCst), 1);
            assert_eq!(follower_reader_count.0.load(Ordering::SeqCst), 1);
            assert_eq!(first_peer_count.0.load(Ordering::SeqCst), 0);
            assert_eq!(follower_peer_count.0.load(Ordering::SeqCst), 0);

            epoch
                .write_at(0, b"wake")
                .current()
                .expect("current repeat write")
                .expect("repeat write succeeds");
            assert_eq!(first_reader_count.0.load(Ordering::SeqCst), 1);
            assert_eq!(follower_reader_count.0.load(Ordering::SeqCst), 1);

            first_lease.register_reader_waker(&first_reader_waker);
            follower_lease.register_reader_waker(&follower_reader_waker);
            epoch
                .commit(Some(4))
                .current()
                .expect("current commit")
                .expect("commit succeeds");
            assert_eq!(first_reader_count.0.load(Ordering::SeqCst), 2);
            assert_eq!(follower_reader_count.0.load(Ordering::SeqCst), 2);
            assert_eq!(first_peer_count.0.load(Ordering::SeqCst), 1);
            assert_eq!(follower_peer_count.0.load(Ordering::SeqCst), 1);
            assert!(writer_cancel.is_cancelled());
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn follower_attach_and_progress_wake_the_current_writer_peer() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "writer-wake");
            let (writer_lease, writer) = attach(&index, &store, &key, entry(0, None));
            let (count, waker) = counting_waker();
            writer_lease.register_peer_waker(&waker);
            writer_lease.register_peer_waker(&waker);

            let (follower_lease, none) = attach(&index, &store, &key, entry(0, None));
            assert!(none.is_none());
            assert_eq!(count.0.load(Ordering::SeqCst), 1);

            follower_lease.note_progress();
            assert_eq!(count.0.load(Ordering::SeqCst), 1);

            writer_lease.register_peer_waker(&waker);
            follower_lease.note_progress();
            assert_eq!(count.0.load(Ordering::SeqCst), 2);
            drop(writer);
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn writer_drop_wakes_a_registered_follower_peer_once() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "handoff-wake");
            let (_writer_lease, writer) = attach(&index, &store, &key, entry(0, None));
            let (follower_lease, none) = attach(&index, &store, &key, entry(0, None));
            assert!(none.is_none());
            let (count, waker) = counting_waker();
            follower_lease.register_peer_waker(&waker);
            follower_lease.register_peer_waker(&waker);

            drop(writer.expect("first writer"));

            assert_eq!(count.0.load(Ordering::SeqCst), 1);
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn cleared_peer_registration_is_not_woken_by_progress() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "clear-peer-wake");
            let (lease, writer) = attach(&index, &store, &key, entry(0, None));
            let _writer = writer.expect("first writer");
            let (count, waker) = counting_waker();
            lease.register_peer_waker(&waker);

            lease.clear_peer_waker(&waker);
            lease.note_progress();

            assert_eq!(count.0.load(Ordering::SeqCst), 0);
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn clearing_stale_peer_waker_preserves_its_replacement() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "replace-peer-wake");
            let (lease, writer) = attach(&index, &store, &key, entry(0, None));
            let _writer = writer.expect("first writer");
            let (old_count, old_waker) = counting_waker();
            let (new_count, new_waker) = counting_waker();
            lease.register_peer_waker(&old_waker);
            lease.register_peer_waker(&new_waker);

            lease.clear_peer_waker(&old_waker);
            lease.note_progress();

            assert_eq!(old_count.0.load(Ordering::SeqCst), 0);
            assert_eq!(new_count.0.load(Ordering::SeqCst), 1);
        }

        #[kithara::test(timeout(Duration::from_secs(1)))]
        fn waker_replacement_drops_old_after_slot_unlock() {
            let index = PendingResourceIndex::new(CancelToken::never());
            let store = test_store();
            let key = ResourceKey::relative("asset", "reentrant-waker-drop");
            let (lease, writer) = attach(&index, &store, &key, entry(0, None));
            let lease = Arc::new(lease);
            let writer = writer.expect("first writer");
            let epoch = writer.epoch();
            let (discarded_count, discarded_waker) = counting_waker();
            let (replacement_count, replacement_waker) = counting_waker();
            let dropped = Arc::new(AtomicBool::new(false));
            let reentrant = Waker::from(Arc::new(RearmReaderOnDrop {
                dropped: Arc::clone(&dropped),
                lease: Arc::clone(&lease),
                replacement: replacement_waker,
            }));
            lease.register_reader_waker(&reentrant);
            drop(reentrant);

            lease.register_reader_waker(&discarded_waker);

            assert!(dropped.load(Ordering::SeqCst));
            epoch
                .write_at(0, b"wake")
                .current()
                .expect("current write")
                .expect("write succeeds");
            assert_eq!(discarded_count.0.load(Ordering::SeqCst), 0);
            assert_eq!(replacement_count.0.load(Ordering::SeqCst), 1);
        }
    }

    use std::{
        error::Error as StdError,
        sync::{
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
            mpsc,
        },
        task::Waker,
        thread,
    };

    use dashmap::try_result::TryResult;
    use fixtures::{RearmReaderOnDrop, attach, counting_waker, entry, test_store};
    use kithara_platform::{CancelScope, CancelToken, sync::Arc, time::Duration};
    use kithara_storage::StorageError;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        AcquisitionResult, AssetResourceState, AssetStore, AssetsError,
        PendingResourceCleanupError, ReadSide, StorageBackend, layout::ResourceKey,
    };
}
