use std::{
    io,
    ops::Range,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::{Context, Poll},
};

use kithara_abr::Abr;
use kithara_assets::{AssetReader, ReadSide, ResourceLease, WriterEpoch, WriterHandle};
use kithara_bufpool::HasPool;
use kithara_download::{FetchCmd, Peer, RequestPriority, reject_html_response};
use kithara_net::{Headers, NetError, RangeSpec};
use kithara_platform::{
    CancelToken, CancelWakerGuard,
    sync::{Arc, Mutex, Weak},
};
use kithara_storage::ResourceStatus;

use super::response::{FetchCompletion, FetchWriter, response_contract};
use crate::{config::FileConfigOwnerAccess, coord::FileCoord, session::inner::FileInner};

/// Gap-driven downloader for one remote file session.
/// It emits at most one fetch and waits when finite demand is already present.
pub(crate) struct FilePeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// The fetch this peer has in flight, if any.
    inflight: Arc<Mutex<Option<Inflight>>>,
    session_cancel: CancelToken,
    source_cancel: CancelToken,
    _session_cancel_wake: CancelWakerGuard,
    _source_cancel_wake: CancelWakerGuard,
    /// Current single-writer election handle, if this consumer owns it.
    writer: Mutex<Option<WriterHandle<S>>>,
    inner: Weak<FileInner<S>>,
}

/// The fetch a peer is currently streaming.
struct Inflight {
    /// Cancels exactly this fetch.
    cancel: CancelToken,
    /// End of the span this fetch was planned to deliver, if bounded.
    end_exclusive: Option<u64>,
    /// Where this fetch started streaming; it lands bytes forward from here.
    start: u64,
}

struct WriterSnapshot<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    cancel: CancelToken,
    epoch: WriterEpoch<S>,
    watermark: u64,
}

struct FetchPlan<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    cancel: CancelToken,
    end_exclusive: Option<u64>,
    epoch: WriterEpoch<S>,
    start: u64,
}

enum PeerAction<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    Done,
    Fetch(FetchPlan<S>),
    Pending,
}

impl<S> FilePeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    /// Build the remote session's download peer.
    ///
    /// # Panics
    ///
    /// Panics if `inner` has no resource lease. Local and already-cached files
    /// do not create peers.
    pub(crate) fn new(inner: &Arc<FileInner<S>>, writer: Option<WriterHandle<S>>) -> Self {
        let Some(lease) = inner.resource_lease.as_ref() else {
            panic!("BUG: FilePeer requires a resource lease");
        };
        let session_cancel = lease.session_cancel();
        let source_cancel = inner.source.cancel.clone();
        let source_cancel_wake = wake_peer_on_cancel(&inner.source.cancel, inner);
        let session_cancel_wake = wake_peer_on_cancel(&session_cancel, inner);
        Self {
            session_cancel,
            source_cancel,
            _session_cancel_wake: session_cancel_wake,
            _source_cancel_wake: source_cancel_wake,
            inflight: Arc::new(Mutex::new(None)),
            inner: Arc::downgrade(inner),
            writer: Mutex::new(writer),
        }
    }

    /// A replacement fetch may only start once the peer is woken: it parks on its own waker while a
    /// fetch is in flight, so a superseded fetch that does not wake it would leave it parked with
    /// nothing left to complete it.
    fn build_fetch_cmd(&self, inner: &Arc<FileInner<S>>, plan: FetchPlan<S>) -> FetchCmd {
        let FetchPlan {
            cancel: writer_cancel,
            end_exclusive,
            epoch,
            start,
        } = plan;
        let url = inner.remote_url().clone();
        let headers = inner.headers().clone();
        let source_cancel = inner.source.cancel.clone();
        let fetch_cancel = writer_cancel.child();
        let cancel_from_source = fetch_cancel.clone();
        let source_cancel_guard = source_cancel.on_cancel(move || cancel_from_source.cancel());

        let invalid_response = Arc::new(AtomicBool::new(false));
        let offset = Arc::new(AtomicU64::new(start));
        let writer_state = FetchWriter {
            cancel: fetch_cancel.clone(),
            epoch: epoch.clone(),
            inner: Arc::downgrade(inner),
            invalid_response: Arc::clone(&invalid_response),
            offset: Arc::clone(&offset),
        };
        let writer = Box::new(move |chunk: &[u8]| -> io::Result<()> { writer_state.write(chunk) });

        let weak_for_response = Arc::downgrade(inner);
        let response_epoch = epoch.clone();
        let invalid_for_response = Arc::clone(&invalid_response);
        let on_response = Box::new(move |headers: &Headers| {
            if response_epoch.is_current() {
                let invalid = weak_for_response.upgrade().map_or_else(
                    || response_contract(headers, start, end_exclusive).invalid,
                    |inner| !inner.capture_content_metadata(headers, start, end_exclusive),
                );
                invalid_for_response.store(invalid, Ordering::Release);
            }
        });

        *self.inflight.lock() = Some(Inflight {
            end_exclusive,
            start,
            cancel: fetch_cancel.clone(),
        });

        let weak_for_complete = Arc::downgrade(inner);
        let invalid_for_complete = Arc::clone(&invalid_response);
        let inflight = Arc::clone(&self.inflight);
        let cb_offset = Arc::clone(&offset);
        let on_complete = Box::new(
            move |_reported_total: u64, _headers: Option<&Headers>, err: Option<&NetError>| {
                drop(source_cancel_guard);
                let written = cb_offset.load(Ordering::Acquire).saturating_sub(start);
                let inner = weak_for_complete.upgrade();
                if let Some(inner) = inner.as_ref() {
                    inner.complete_fetch(
                        &epoch,
                        FetchCompletion {
                            end_exclusive,
                            bytes_written: written,
                            error: err,
                            invalid_response: invalid_for_complete.load(Ordering::Acquire),
                            resume_from: start,
                        },
                    );
                }
                *inflight.lock() = None;
                if let Some(lease) = inner
                    .as_ref()
                    .and_then(|inner| inner.resource_lease.as_ref())
                {
                    lease.wake_peer();
                }
            },
        );

        FetchCmd::get(url)
            .cancel(fetch_cancel)
            .writer(writer)
            .validator(reject_html_response)
            .on_response(on_response)
            .maybe_range(fetch_range(start, end_exclusive))
            .maybe_headers(headers)
            .on_complete(on_complete)
            .build()
    }

    fn drop_writer(&self) {
        let writer = self.writer.lock().take();
        drop(writer);
    }

    fn next_action(&self, inner: &Arc<FileInner<S>>, lease: &ResourceLease<S>) -> PeerAction<S> {
        if inner.source.cancel.is_cancelled() || self.session_cancel.is_cancelled() {
            self.drop_writer();
            return PeerAction::Done;
        }
        if inner.observe_committed() {
            return PeerAction::Done;
        }
        if !matches!(inner.asset.reader.status(), ResourceStatus::Active) {
            return PeerAction::Done;
        }

        let Some(writer) = self.writer_snapshot(lease) else {
            return PeerAction::Pending;
        };
        let total = inner.source.coord.total_bytes();
        let upper = total.map_or(writer.watermark, |total| total.min(writer.watermark));
        let gap = match total {
            None if upper > 0 => 0..upper,
            _ => {
                let cursor = steering_cursor(&inner.source.coord);
                let Some(gap) = next_gap_from_cursor(&inner.asset.reader, cursor, upper) else {
                    if total.is_some_and(|total| inner.commit_if_complete(&writer.epoch, total)) {
                        return PeerAction::Done;
                    }
                    return PeerAction::Pending;
                };
                gap
            }
        };
        let end_exclusive = (total.is_some() || writer.watermark != u64::MAX).then_some(gap.end);
        PeerAction::Fetch(FetchPlan {
            end_exclusive,
            cancel: writer.cancel,
            epoch: writer.epoch,
            start: gap.start,
        })
    }

    /// Whether the peer has to park on a fetch that is already running, having first
    /// cancelled that fetch if the reader cursor sits inside its span past the bytes it
    /// has landed. Stored bytes decide, not a write offset the fetch publishes.
    fn park_on_running_fetch(&self, inner: &Arc<FileInner<S>>) -> bool {
        let Some((cancel, end_exclusive, start)) = self
            .inflight
            .lock()
            .as_ref()
            .map(|fetch| (fetch.cancel.clone(), fetch.end_exclusive, fetch.start))
        else {
            return false;
        };
        let frontier = landed_frontier(&inner.asset.reader, start, end_exclusive);
        let overtaken = steering_cursor(&inner.source.coord)
            .filter(|cursor| end_exclusive.is_none_or(|end| *cursor < end))
            .is_some_and(|cursor| cursor > frontier);
        if overtaken {
            cancel.cancel();
        }
        true
    }

    /// Snapshot the current writer without dropping election state under File's lock.
    fn writer_snapshot(&self, lease: &ResourceLease<S>) -> Option<WriterSnapshot<S>> {
        let stale = {
            let mut writer = self.writer.lock();
            if writer.as_ref().is_some_and(|handle| !handle.is_current()) {
                writer.take()
            } else {
                None
            }
        };
        drop(stale);

        let needs_writer = self.writer.lock().is_none();
        let candidate = if needs_writer {
            lease.try_take_writer()
        } else {
            None
        };
        let rejected = candidate.and_then(|candidate| {
            let mut writer = self.writer.lock();
            let rejected = if writer.is_none() {
                *writer = Some(candidate);
                None
            } else {
                Some(candidate)
            };
            drop(writer);
            rejected
        });
        drop(rejected);

        let (snapshot, stale) = {
            let mut writer = self.writer.lock();
            match writer.as_ref() {
                Some(handle) if handle.is_current() => (
                    Some(WriterSnapshot {
                        cancel: handle.writer_cancel(),
                        epoch: handle.epoch(),
                        watermark: handle.max_watermark(),
                    }),
                    None,
                ),
                Some(_) => (None, writer.take()),
                None => (None, None),
            }
        };
        drop(stale);
        snapshot
    }
}

fn wake_peer_on_cancel<S>(cancel: &CancelToken, inner: &Arc<FileInner<S>>) -> CancelWakerGuard
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let weak = Arc::downgrade(inner);
    cancel.on_cancel(move || {
        if let Some(inner) = weak.upgrade()
            && let Some(lease) = inner.resource_lease.as_ref()
        {
            lease.wake_peer();
        }
    })
}

/// Where the peer fetches next. The reader cursor comes first: after a seek the
/// listener waits on the bytes under it, and a range request delivers them
/// without walking the span they skipped. Once nothing is missing ahead of the
/// cursor the peer fills the earlier gaps, so the resource still commits.
fn next_gap_from_cursor<S>(
    reader: &AssetReader<S>,
    cursor: Option<u64>,
    upper: u64,
) -> Option<Range<u64>>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    cursor
        .and_then(|cursor| reader.next_gap(cursor, upper))
        .or_else(|| reader.next_gap(0, upper))
}

/// How far a fetch streaming forward from `start` has landed bytes: the first
/// byte still missing at or after `start`, or the end of its span.
fn landed_frontier<S>(reader: &AssetReader<S>, start: u64, end_exclusive: Option<u64>) -> u64
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    let end = end_exclusive.unwrap_or(u64::MAX);
    reader.next_gap(start, end).map_or(end, |gap| gap.start)
}

/// The reader cursor, when it is allowed to steer fetching. A range request
/// needs the resource extent: before the first response has answered how long
/// the resource is, a cursor past the end would anchor a fetch on bytes that do
/// not exist — and the request that would have reported the real length is the
/// one being replaced. A cursor outside a known extent has nothing to fetch
/// either. Both leave the head fetch to establish the resource.
fn steering_cursor(coord: &FileCoord) -> Option<u64> {
    let total = coord.total_bytes()?;
    let cursor = coord.position();
    (cursor < total).then_some(cursor)
}

fn fetch_range(start: u64, end_exclusive: Option<u64>) -> Option<RangeSpec> {
    end_exclusive.map_or_else(
        || (start > 0).then(|| RangeSpec::new(start, None)),
        |end| {
            end.checked_sub(1)
                .filter(|end| *end >= start)
                .map(|end| RangeSpec::new(start, Some(end)))
        },
    )
}

impl<S> Abr for FilePeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn cancel(&self) -> CancelToken {
        self.source_cancel.clone()
    }
}

impl<S> Peer for FilePeer<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Option<Vec<FetchCmd>>> {
        let Some(inner) = self.inner.upgrade() else {
            return Poll::Ready(None);
        };
        let Some(lease) = inner.resource_lease.as_ref() else {
            return Poll::Ready(None);
        };

        lease.register_peer_waker(cx.waker());
        if self.park_on_running_fetch(&inner) {
            return Poll::Pending;
        }
        match self.next_action(&inner, lease) {
            PeerAction::Pending => Poll::Pending,
            PeerAction::Done => {
                lease.clear_peer_waker(cx.waker());
                Poll::Ready(None)
            }
            PeerAction::Fetch(plan) => {
                lease.clear_peer_waker(cx.waker());
                Poll::Ready(Some(vec![self.build_fetch_cmd(&inner, plan)]))
            }
        }
    }

    fn priority(&self) -> RequestPriority {
        if self
            .inner
            .upgrade()
            .is_some_and(|inner| inner.source.coord.activity().is_playing())
        {
            RequestPriority::High
        } else {
            RequestPriority::Low
        }
    }
}

#[cfg(test)]
mod tests {
    mod fixtures {
        #[cfg(not(target_arch = "wasm32"))]
        use std::sync::Barrier;
        use std::{sync::atomic::AtomicU64, task::Wake};

        use kithara_assets::{
            AcquisitionResult, AssetReader, AssetResource, AssetSource, AssetStore, ResourceKey,
            ResourceLease, StorageBackend, WriterHandle,
        };
        use kithara_events::EventBus;
        use kithara_platform::{CancelToken, sync::Arc};
        use kithara_stream::{PlayheadState, WorkerWake};
        use url::Url;

        use super::super::*;
        use crate::{
            File, FileConfig, FileSrc,
            coord::FileCoord,
            session::inner::FileSourceCtx,
            test_pools::{TestPools, pools},
        };

        type TestFile = File<TestPools>;
        type TestInner = FileInner<TestPools>;
        type TestLease = ResourceLease<TestPools>;
        type TestPeer = FilePeer<TestPools>;
        type TestReader = AssetReader<TestPools>;
        type TestStore = AssetStore<TestPools>;
        type TestWriterHandle = WriterHandle<TestPools>;

        pub(super) fn test_key(store: &TestStore) -> ResourceKey {
            let source = AssetSource::Remote {
                url: Url::parse("https://example.com/remote.dat").expect("test URL"),
                discriminator: Some("peer-test".to_string()),
            };
            let scope = store.scope::<TestFile>(&source).expect("test scope");
            scope
                .key(&AssetResource::Source {
                    extension: "dat".to_string(),
                })
                .expect("test resource key")
        }

        pub(super) fn make_coord() -> Arc<FileCoord> {
            Arc::new(FileCoord::new(Arc::new(PlayheadState::new())))
        }

        pub(super) fn attach_pending(
            store: &TestStore,
            key: &ResourceKey,
            coord: &Arc<FileCoord>,
            look_ahead: Option<u64>,
        ) -> (TestReader, TestLease, Option<TestWriterHandle>) {
            let AcquisitionResult::Pending(attachment) = store
                .attach_pending_resource(key, coord.read_pos_handle(), look_ahead)
                .expect("attach pending resource")
            else {
                panic!("fresh session must be pending");
            };
            attachment.into()
        }

        pub(super) fn make_inner(
            reader: TestReader,
            lease: TestLease,
            coord: Arc<FileCoord>,
            bus: EventBus,
        ) -> Arc<TestInner> {
            make_inner_with_cancel(reader, lease, coord, bus, CancelToken::never())
        }

        pub(super) fn make_inner_with_cancel(
            reader: TestReader,
            lease: TestLease,
            coord: Arc<FileCoord>,
            bus: EventBus,
            cancel: CancelToken,
        ) -> Arc<TestInner> {
            let config = Arc::new(
                FileConfig::for_src(FileSrc::Remote(
                    Url::parse("http://127.0.0.1/test.mp3").expect("test url"),
                ))
                .store(
                    AssetStore::builder(pools())
                        .backend(StorageBackend::Memory)
                        .build(),
                )
                .pools(pools())
                .reader_event_capacity(16)
                .build(),
            );
            Arc::new(FileInner::new(
                config,
                FileSourceCtx { coord, cancel, bus },
                crate::session::inner::FileAssetCtx { reader },
                false,
                Some(lease),
            ))
        }

        pub(super) fn make_peer(
            inner: &Arc<TestInner>,
            writer: Option<TestWriterHandle>,
        ) -> TestPeer {
            FilePeer::new(inner, writer)
        }

        pub(super) fn completion(
            resume_from: u64,
            bytes_written: u64,
            end_exclusive: Option<u64>,
            error: Option<&NetError>,
        ) -> FetchCompletion<'_> {
            FetchCompletion {
                bytes_written,
                end_exclusive,
                error,
                resume_from,
                invalid_response: false,
            }
        }

        #[derive(Default)]
        pub(super) struct CountingWake(AtomicU64);

        #[cfg(not(target_arch = "wasm32"))]
        pub(super) struct BlockingWake {
            pub(super) entered: Arc<Barrier>,
            pub(super) release: Arc<Barrier>,
        }

        impl CountingWake {
            pub(super) fn count(&self) -> u64 {
                self.0.load(Ordering::Acquire)
            }
        }

        impl WorkerWake for CountingWake {
            fn defer(&self) {
                self.0.fetch_add(1, Ordering::Release);
            }

            fn wake(&self) {
                self.0.fetch_add(1, Ordering::Release);
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        impl WorkerWake for BlockingWake {
            fn defer(&self) {}

            fn wake(&self) {
                self.entered.wait();
                self.release.wait();
            }
        }

        impl Wake for CountingWake {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::Release);
            }
        }

        pub(super) fn fresh_session(
            look_ahead: Option<u64>,
        ) -> (TestStore, ResourceKey, Arc<TestInner>, TestWriterHandle) {
            let store = AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .cancel(CancelToken::never())
                .build();
            let key = test_key(&store);
            let coord = make_coord();
            let (reader, lease, writer) = attach_pending(&store, &key, &coord, look_ahead);
            let writer = writer.expect("first consumer is writer");
            let inner = make_inner(reader, lease, coord, EventBus::new(16));
            (store, key, inner, writer)
        }

        pub(super) fn assert_ready_bytes(store: &TestStore, key: &ResourceKey, expected: &[u8]) {
            let AcquisitionResult::Ready(reader) = store
                .attach_pending_resource(key, Arc::new(AtomicU64::new(0)), None)
                .expect("reopen committed session")
            else {
                panic!("committed session must reopen ready");
            };
            let mut bytes = vec![0; expected.len()];
            let read = reader.read_at(0, &mut bytes).expect("read committed bytes");
            assert_eq!(read, expected.len());
            assert_eq!(bytes, expected);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    use std::{sync::Barrier, thread};
    use std::{sync::atomic::AtomicU64, task::Waker};

    use kithara_assets::{
        AcquisitionResult, AssetResourceState, AssetStore, ResourceLease, StorageBackend,
        WriterOutcome,
    };
    use kithara_download::Peer;
    use kithara_events::{Envelope, EventBus};
    use kithara_platform::{CancelScope, CancelToken, sync::Arc, time::Duration};
    use kithara_stream::WorkerWake;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{FileEvent, session::FileSource, test_pools::pools};

    mod completion {
        use super::*;

        #[kithara::test]
        fn cancelled_full_invalid_response_relinquishes_without_file_error() {
            let (store, key, inner, writer) = fresh_session(None);
            let epoch = writer.epoch();
            let mut events = inner.source.bus.subscribe::<FileEvent>();
            inner.source.coord.set_total_bytes(Some(4));
            assert!(matches!(
                epoch.write_at(0, b"done"),
                WriterOutcome::Current(Ok(()))
            ));

            inner.complete_fetch(
                &epoch,
                FetchCompletion {
                    invalid_response: true,
                    ..completion(0, 4, None, Some(&NetError::Cancelled))
                },
            );

            assert!(!writer.is_current());
            assert!(events.try_recv().is_err());
            assert!(matches!(
                store.resource_state(&key).expect("resource state"),
                AssetResourceState::Active
            ));
        }

        #[kithara::test]
        fn stale_writer_does_not_report_io_failure() {
            let (_store, _key, inner, writer) = fresh_session(None);
            let epoch = writer.epoch();
            let fetch_cancel = writer.writer_cancel().child();
            assert!(matches!(epoch.relinquish(), WriterOutcome::Current(())));
            let writer = FetchWriter {
                cancel: fetch_cancel.clone(),
                epoch: epoch.clone(),
                inner: Arc::downgrade(&inner),
                invalid_response: Arc::new(AtomicBool::new(false)),
                offset: Arc::new(AtomicU64::new(0)),
            };

            let result = writer.write(b"stale");

            assert!(result.is_ok());
            assert!(fetch_cancel.is_cancelled());
            assert!(!inner.asset.reader.contains_range(0..1));
        }

        #[kithara::test]
        fn transient_after_full_advertised_body_commits() {
            let (store, key, inner, writer) = fresh_session(None);
            let epoch = writer.epoch();
            inner.source.coord.set_total_bytes(Some(4));
            assert!(matches!(
                epoch.write_at(0, b"done"),
                WriterOutcome::Current(Ok(()))
            ));

            inner.finalize_fetch(
                &epoch,
                completion(
                    0,
                    4,
                    None,
                    Some(&NetError::Network("tail reset".to_string())),
                ),
            );

            assert_ready_bytes(&store, &key, b"done");
        }

        #[kithara::test]
        fn cancellation_after_full_advertised_body_commits() {
            let (store, key, inner, writer) = fresh_session(None);
            let epoch = writer.epoch();
            inner.source.coord.set_total_bytes(Some(4));
            assert!(matches!(
                epoch.write_at(0, b"done"),
                WriterOutcome::Current(Ok(()))
            ));

            inner.finalize_fetch(&epoch, completion(0, 4, None, Some(&NetError::Cancelled)));

            assert_ready_bytes(&store, &key, b"done");
        }

        #[kithara::test]
        fn cancellation_after_incomplete_body_relinquishes_for_successor() {
            let (store, key, inner, writer) = fresh_session(None);
            let epoch = writer.epoch();
            inner.source.coord.set_total_bytes(Some(4));
            assert!(matches!(
                epoch.write_at(0, b"not"),
                WriterOutcome::Current(Ok(()))
            ));

            inner.finalize_fetch(&epoch, completion(0, 3, None, Some(&NetError::Cancelled)));

            assert!(!writer.is_current());
            assert!(matches!(
                store.resource_state(&key).expect("resource state"),
                AssetResourceState::Active
            ));
            let coord = make_coord();
            let (_reader, _lease, successor) = attach_pending(&store, &key, &coord, None);
            assert!(successor.is_some_and(|writer| writer.is_current()));
        }

        #[kithara::test]
        fn open_ended_resume_without_total_stays_active() {
            let (store, key, inner, writer) = fresh_session(None);
            let epoch = writer.epoch();
            assert!(matches!(
                epoch.write_at(0, b"old"),
                WriterOutcome::Current(Ok(()))
            ));
            assert!(matches!(
                epoch.write_at(3, b"new"),
                WriterOutcome::Current(Ok(()))
            ));

            inner.finalize_fetch(&epoch, completion(3, 3, None, None));

            assert!(writer.is_current());
            assert!(matches!(
                store.resource_state(&key).expect("resource state"),
                AssetResourceState::Active
            ));
        }

        #[kithara::test]
        fn initial_zero_progress_transient_fails_session() {
            let (store, key, inner, writer) = fresh_session(None);
            let epoch = writer.epoch();
            let mut events = inner.source.bus.subscribe::<FileEvent>();

            inner.finalize_fetch(
                &epoch,
                completion(
                    0,
                    0,
                    None,
                    Some(&NetError::Network("connect reset".to_string())),
                ),
            );

            assert!(matches!(
                store.resource_state(&key).expect("resource state"),
                AssetResourceState::Missing
            ));
            assert!(matches!(
                events.try_recv(),
                Ok(Envelope {
                    event: FileEvent::Error { .. },
                    ..
                })
            ));
        }

        #[kithara::test]
        #[case::unknown_extent_without_demand(0, None)]
        #[case::known_extent_with_cached_demand(4, Some(8))]
        fn finite_watermark_already_present_stays_pending(
            #[case] watermark: u64,
            #[case] total: Option<u64>,
        ) {
            let (_store, _key, inner, writer) = fresh_session(Some(watermark));
            inner.source.coord.set_total_bytes(total);
            assert!(matches!(
                writer.epoch().write_at(0, b"data"),
                WriterOutcome::Current(Ok(()))
            ));
            let peer = make_peer(&inner, Some(writer));
            let mut cx = Context::from_waker(Waker::noop());

            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Pending));
        }

        #[kithara::test(native, timeout(Duration::from_secs(2)))]
        fn inflight_clears_after_completion_settlement() {
            let (_store, _key, inner, writer) = fresh_session(None);
            let entered = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            inner.set_worker_wake(Arc::new(BlockingWake {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }));
            inner.arm_reader_waker();
            let peer = FilePeer::new(&inner, Some(writer));
            let mut cx = Context::from_waker(Waker::noop());
            let Poll::Ready(Some(mut fetches)) = Peer::poll_next(&peer, &mut cx) else {
                panic!("missing bytes must start a fetch");
            };
            let mut fetch = fetches.remove(0);
            let on_complete = fetch.take_on_complete().expect("completion callback");

            let completion = thread::spawn(move || {
                let error = NetError::Network("initial fetch failed".to_string());
                on_complete(0, None, Some(&error));
            });
            entered.wait();
            let inflight_during_settlement = peer.inflight.lock().is_some();
            release.wait();
            completion
                .join()
                .expect("completion callback must not panic");

            assert!(
                inflight_during_settlement,
                "a replacement fetch must not start before the prior callback settles"
            );
            assert!(peer.inflight.lock().is_none());
        }

        #[kithara::test(native, timeout(Duration::from_secs(2)))]
        fn terminal_reader_wake_settles_file_before_worker_wake() {
            let (_store, _key, inner, writer) = fresh_session(None);
            let entered = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            let epoch = writer.epoch();
            assert!(matches!(
                epoch.write_at(0, b"done"),
                WriterOutcome::Current(Ok(()))
            ));
            inner.set_worker_wake(Arc::new(BlockingWake {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }));
            inner.arm_reader_waker();
            inner.source.coord.set_total_bytes(Some(4));
            let mut events = inner.source.bus.subscribe::<FileEvent>();

            let completion = thread::spawn(move || epoch.commit(Some(4)));
            entered.wait();
            let mut settled_before_wake = false;
            while let Ok(envelope) = events.try_recv() {
                settled_before_wake |=
                    matches!(envelope.event, FileEvent::CacheComplete { total_bytes: 4 });
            }
            release.wait();
            assert!(matches!(
                completion.join().expect("commit must not panic"),
                WriterOutcome::Current(Ok(()))
            ));

            assert!(
                settled_before_wake,
                "terminal reader wake must settle File state before waking the audio worker"
            );
        }

        #[kithara::test]
        fn repeated_committed_observation_does_not_self_wake() {
            let (_store, _key, inner, writer) = fresh_session(None);
            let wake = Arc::new(CountingWake::default());
            inner.set_worker_wake(Arc::clone(&wake) as Arc<dyn WorkerWake>);
            let epoch = writer.epoch();
            assert!(matches!(
                epoch.write_at(0, b"done"),
                WriterOutcome::Current(Ok(()))
            ));
            assert!(matches!(
                epoch.commit(Some(4)),
                WriterOutcome::Current(Ok(()))
            ));

            assert!(inner.observe_committed());
            let first_count = wake.count();
            assert!(inner.observe_committed());

            assert_eq!(wake.count(), first_count);
        }
    }
    mod metadata {
        use super::*;

        #[kithara::test]
        fn remote_capture_metadata_publishes_opened() {
            let (_store, _key, inner, _writer) = fresh_session(None);
            let mut rx = inner.source.bus.subscribe();
            let mut headers = Headers::default();
            headers.insert("content-type", "audio/mpeg");
            headers.insert("content-length", "12");

            assert!(inner.capture_content_metadata(&headers, 0, None));

            assert!(matches!(
                rx.try_recv(),
                Ok(Envelope {
                    event: FileEvent::TotalBytesResolved { .. },
                    ..
                })
            ));
            assert!(matches!(
                rx.try_recv(),
                Ok(Envelope {
                    event: FileEvent::Opened {
                        cached: false,
                        total_bytes: Some(12),
                        ..
                    },
                    ..
                })
            ));
        }

        #[kithara::test]
        fn conflicting_response_total_is_rejected_before_body() {
            let (_store, _key, inner, _writer) = fresh_session(None);
            inner.source.coord.set_total_bytes(Some(100));
            let mut headers = Headers::default();
            headers.insert("content-range", "bytes 16-31/200");
            headers.insert("content-length", "16");

            assert!(!inner.capture_content_metadata(&headers, 16, Some(32)));
            assert_eq!(inner.source.coord.total_bytes(), Some(100));
            assert!(!inner.asset.reader.contains_range(0..1));
        }

        #[kithara::test]
        fn marking_complete_publishes_cache_complete_once() {
            let (_store, _key, inner, _writer) = fresh_session(None);
            let mut rx = inner.source.bus.subscribe();
            inner.source.coord.set_total_bytes(Some(12));

            inner.mark_complete();

            assert!(matches!(
                rx.try_recv(),
                Ok(Envelope {
                    event: FileEvent::CacheComplete { total_bytes: 12 },
                    ..
                })
            ));
            inner.mark_complete();
            assert!(rx.try_recv().is_err());
        }

        #[kithara::test]
        fn bounded_response_uses_content_range_total() {
            let mut headers = Headers::default();
            headers.insert("content-length", "16");
            headers.insert("content-range", "bytes 0-15/60");

            assert_eq!(response_contract(&headers, 0, Some(16)).total, Some(60));

            let mut length_only = Headers::default();
            length_only.insert("content-length", "16");
            assert_eq!(response_contract(&length_only, 32, Some(48)).total, None);
            assert_eq!(response_contract(&length_only, 32, None).total, None);
            assert!(response_contract(&length_only, 32, None).invalid);
        }

        #[kithara::test]
        fn range_ignored_at_zero_uses_full_length_and_commits() {
            let (store, key, inner, writer) = fresh_session(Some(16));
            let mut headers = Headers::default();
            headers.insert("content-length", "6");
            inner.capture_content_metadata(&headers, 0, Some(16));
            assert_eq!(inner.source.coord.total_bytes(), Some(6));
            let epoch = writer.epoch();
            assert!(matches!(
                epoch.write_at(0, b"all!!!"),
                WriterOutcome::Current(Ok(()))
            ));

            inner.finalize_fetch(&epoch, completion(0, 6, Some(16), None));

            assert_ready_bytes(&store, &key, b"all!!!");
        }

        #[kithara::test]
        fn resumed_response_without_content_range_fails_before_write() {
            let (store, key, inner, writer) = fresh_session(Some(16));
            let epoch = writer.epoch();
            let mut headers = Headers::default();
            headers.insert("content-length", "6");
            let contract = response_contract(&headers, 3, Some(19));
            assert!(contract.invalid);
            let fetch_cancel = writer.writer_cancel().child();
            let writer = FetchWriter {
                cancel: fetch_cancel,
                epoch: epoch.clone(),
                inner: Arc::downgrade(&inner),
                invalid_response: Arc::new(AtomicBool::new(contract.invalid)),
                offset: Arc::new(AtomicU64::new(3)),
            };

            let result = writer.write(b"all!!!");

            assert!(result.is_err());
            assert!(!inner.asset.reader.contains_range(3..4));
            inner.fail_current_epoch(
                &epoch,
                "bounded response did not identify the requested range at offset 3".to_string(),
            );
            assert!(matches!(
                store.resource_state(&key).expect("resource state"),
                AssetResourceState::Missing
            ));
        }

        #[kithara::test]
        fn bounded_response_without_size_is_invalid() {
            assert!(response_contract(&Headers::default(), 0, Some(16)).invalid);
        }

        #[kithara::test]
        fn content_range_without_content_length_is_invalid() {
            let mut headers = Headers::default();
            headers.insert("content-range", "bytes 0-15/60");

            assert!(response_contract(&headers, 0, Some(16)).invalid);
        }

        #[kithara::test]
        fn content_range_without_known_total_is_invalid() {
            let mut headers = Headers::default();
            headers.insert("content-length", "8");
            headers.insert("content-range", "bytes 8-15/*");

            assert!(response_contract(&headers, 8, Some(16)).invalid);
        }

        #[kithara::test]
        fn content_range_end_at_total_is_invalid() {
            let mut headers = Headers::default();
            headers.insert("content-range", "bytes 0-60/60");

            assert!(response_contract(&headers, 0, Some(61)).invalid);
        }

        #[kithara::test]
        fn content_range_must_match_requested_interval() {
            let mut wrong_start = Headers::default();
            wrong_start.insert("content-range", "bytes 1-15/60");
            assert!(response_contract(&wrong_start, 0, Some(16)).invalid);

            let mut past_end = Headers::default();
            past_end.insert("content-range", "bytes 0-16/60");
            assert!(response_contract(&past_end, 0, Some(16)).invalid);

            let mut wrong_length = Headers::default();
            wrong_length.insert("content-range", "bytes 0-15/60");
            wrong_length.insert("content-length", "15");
            assert!(response_contract(&wrong_length, 0, Some(16)).invalid);
        }
    }
    mod ownership {
        use super::*;

        #[kithara::test]
        #[case::fully_cached(4, None)]
        #[case::partially_cached(8, None)]
        #[case::bounded_fully_cached(4, Some(4))]
        #[case::bounded_partially_cached(8, Some(4))]
        fn a_successor_establishes_extent_before_skipping_cached_bytes(
            #[case] total: u64,
            #[case] look_ahead: Option<u64>,
        ) {
            let (store, key, first, first_writer) = fresh_session(look_ahead);
            first.source.coord.set_total_bytes(Some(total));
            let first_epoch = first_writer.epoch();
            assert!(matches!(
                first_epoch.write_at(0, b"done"),
                WriterOutcome::Current(Ok(()))
            ));

            let successor_coord = make_coord();
            let (reader, lease, writer) =
                attach_pending(&store, &key, &successor_coord, look_ahead);
            assert!(writer.is_none());
            assert_eq!(reader.len(), None);
            assert!(reader.contains_range(0..4));
            assert!(matches!(reader.status(), ResourceStatus::Active));
            assert_eq!(successor_coord.total_bytes(), None);
            let successor = make_inner(reader, lease, successor_coord, EventBus::new(16));
            drop(first_writer);
            let peer = make_peer(&successor, writer);
            let lease = successor.resource_lease.as_ref().expect("successor lease");

            let PeerAction::Fetch(head) = peer.next_action(&successor, lease) else {
                panic!("unknown server extent must be established");
            };
            assert_eq!(head.start, 0);
            assert_eq!(head.end_exclusive, look_ahead);
            assert!(!first_epoch.is_current());
            let mut headers = Headers::default();
            headers.insert(
                "content-length",
                head.end_exclusive.unwrap_or(total).to_string(),
            );
            if let Some(end) = head.end_exclusive {
                headers.insert("content-range", format!("bytes 0-{}/{total}", end - 1));
            }
            assert!(successor.capture_content_metadata(&headers, head.start, head.end_exclusive));
            assert_eq!(successor.source.coord.total_bytes(), Some(total));
            successor.complete_fetch(
                &head.epoch,
                completion(
                    head.start,
                    0,
                    head.end_exclusive,
                    Some(&NetError::Cancelled),
                ),
            );

            if total > 4 {
                if look_ahead.is_some() {
                    assert!(matches!(
                        peer.next_action(&successor, lease),
                        PeerAction::Pending
                    ));
                    lease.request_until(total);
                }
                let PeerAction::Fetch(tail) = peer.next_action(&successor, lease) else {
                    panic!("known missing tail must be fetched");
                };
                assert_eq!(tail.start, 4);
                assert_eq!(tail.end_exclusive, Some(total));
                headers.insert("content-range", "bytes 4-7/8");
                headers.insert("content-length", "4");
                assert!(successor.capture_content_metadata(
                    &headers,
                    tail.start,
                    tail.end_exclusive
                ));
                assert!(matches!(
                    tail.epoch.write_at(tail.start, b"tail"),
                    WriterOutcome::Current(Ok(()))
                ));
                successor.complete_fetch(
                    &tail.epoch,
                    completion(tail.start, 4, tail.end_exclusive, None),
                );
                assert_ready_bytes(&store, &key, b"donetail");
            } else {
                assert_ready_bytes(&store, &key, b"done");
            }
            assert!(matches!(
                peer.next_action(&successor, lease),
                PeerAction::Done
            ));
        }

        #[kithara::test]
        fn abr_cancel_observes_the_file_source_scope() {
            let store = AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .cancel(CancelToken::never())
                .build();
            let key = test_key(&store);
            let coord = make_coord();
            let (reader, lease, writer) = attach_pending(&store, &key, &coord, Some(4));
            let source_cancel = CancelToken::never();
            let inner = make_inner_with_cancel(
                reader,
                lease,
                coord,
                EventBus::new(16),
                source_cancel.clone(),
            );
            let peer = make_peer(&inner, writer);
            let observed = Abr::cancel(&peer);

            assert!(!observed.is_cancelled());
            source_cancel.cancel();
            assert!(observed.is_cancelled());
        }

        #[kithara::test]
        fn cancelled_waiting_source_relinquishes_writer() {
            let store = AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .cancel(CancelToken::never())
                .build();
            let key = test_key(&store);
            let first_coord = make_coord();
            first_coord.set_total_bytes(Some(8));
            let (first_reader, first_lease, first_writer) =
                attach_pending(&store, &key, &first_coord, Some(4));
            let scope = CancelScope::new(None);
            let inner = make_inner_with_cancel(
                first_reader,
                first_lease,
                first_coord,
                EventBus::new(16),
                scope.token(),
            );
            let first_writer = first_writer.expect("first consumer is writer");
            assert!(matches!(
                first_writer.epoch().write_at(0, b"data"),
                WriterOutcome::Current(Ok(()))
            ));
            let peer = make_peer(&inner, Some(first_writer));

            let follower_coord = make_coord();
            let (_reader, follower_lease, follower_writer) =
                attach_pending(&store, &key, &follower_coord, Some(4));
            assert!(follower_writer.is_none());
            let wake = Arc::new(CountingWake::default());
            let waker = Waker::from(Arc::clone(&wake));
            let mut cx = Context::from_waker(&waker);
            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Pending));

            scope.cancel();
            assert_eq!(wake.count(), 1, "source cancellation must wake its peer");

            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Ready(None)));
            assert!(peer.writer.lock().is_none());
            assert!(follower_lease.try_take_writer().is_some());
        }

        #[kithara::test]
        fn cancelled_download_session_wakes_peer_and_prevents_reelection() {
            let store_scope = CancelScope::new(None);
            let store = AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .cancel(store_scope.token())
                .build();
            let key = test_key(&store);
            let first_coord = make_coord();
            first_coord.set_total_bytes(Some(8));
            let (first_reader, first_lease, first_writer) =
                attach_pending(&store, &key, &first_coord, Some(4));
            let inner = make_inner(first_reader, first_lease, first_coord, EventBus::new(16));
            let first_writer = first_writer.expect("first consumer is writer");
            assert!(matches!(
                first_writer.epoch().write_at(0, b"data"),
                WriterOutcome::Current(Ok(()))
            ));
            let peer = make_peer(&inner, Some(first_writer));

            let follower_coord = make_coord();
            let (_reader, follower_lease, follower_writer) =
                attach_pending(&store, &key, &follower_coord, Some(4));
            assert!(follower_writer.is_none());
            let wake = Arc::new(CountingWake::default());
            let waker = Waker::from(Arc::clone(&wake));
            let mut cx = Context::from_waker(&waker);
            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Pending));

            store_scope.cancel();
            assert_eq!(wake.count(), 1, "session cancellation must wake its peer");

            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Ready(None)));
            assert!(peer.writer.lock().is_none());
            assert!(follower_lease.try_take_writer().is_none());
        }

        #[kithara::test]
        fn cancelled_active_session_does_not_start_another_fetch() {
            let store_scope = CancelScope::new(None);
            let store = AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .cancel(store_scope.token())
                .build();
            let key = test_key(&store);
            let first_coord = make_coord();
            let (first_reader, first_lease, first_writer) =
                attach_pending(&store, &key, &first_coord, Some(4));
            let inner = make_inner(first_reader, first_lease, first_coord, EventBus::new(16));
            let first_writer = first_writer.expect("first consumer is writer");
            let epoch = first_writer.epoch();
            let peer = make_peer(&inner, Some(first_writer));

            let follower_coord = make_coord();
            let (_reader, follower_lease, follower_writer) =
                attach_pending(&store, &key, &follower_coord, Some(4));
            assert!(follower_writer.is_none());
            let mut cx = Context::from_waker(Waker::noop());
            let Poll::Ready(Some(fetches)) = Peer::poll_next(&peer, &mut cx) else {
                panic!("missing bytes must start the first fetch");
            };
            drop(fetches);

            store_scope.cancel();
            inner.complete_fetch(
                &epoch,
                completion(0, 0, Some(4), Some(&NetError::Cancelled)),
            );
            *peer.inflight.lock() = None;

            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Ready(None)));
            assert!(peer.writer.lock().is_none());
            assert!(follower_lease.try_take_writer().is_none());
        }

        #[kithara::test]
        fn writer_drop_promotes_follower_with_same_partial_bytes() {
            let store = AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .cancel(CancelToken::never())
                .build();
            let key = test_key(&store);
            let first_coord = make_coord();
            let (first_reader, first_lease, first_writer) =
                attach_pending(&store, &key, &first_coord, None);
            let first_writer = first_writer.expect("first writer");
            let first_epoch = first_writer.epoch();
            assert!(matches!(
                first_epoch.write_at(0, b"old"),
                WriterOutcome::Current(Ok(()))
            ));
            let _first_inner =
                make_inner(first_reader, first_lease, first_coord, EventBus::new(16));

            let follower_coord = make_coord();
            let (follower_reader, follower_lease, follower_writer) =
                attach_pending(&store, &key, &follower_coord, None);
            assert!(follower_writer.is_none());
            let follower = make_inner(
                follower_reader,
                follower_lease,
                Arc::clone(&follower_coord),
                EventBus::new(16),
            );

            drop(first_writer);
            let promoted = follower
                .resource_lease
                .as_ref()
                .and_then(ResourceLease::try_take_writer)
                .expect("follower promotion");
            let promoted_epoch = promoted.epoch();
            assert!(matches!(
                first_epoch.write_at(0, b"bad"),
                WriterOutcome::Stale
            ));
            assert!(matches!(
                promoted_epoch.write_at(3, b"new"),
                WriterOutcome::Current(Ok(()))
            ));
            follower_coord.set_total_bytes(Some(6));
            follower.finalize_fetch(&promoted_epoch, completion(3, 3, None, None));

            assert_ready_bytes(&store, &key, b"oldnew");
            drop(promoted);
        }

        #[kithara::test]
        fn late_cancelled_epoch_cannot_poison_successor() {
            let store = AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .cancel(CancelToken::never())
                .build();
            let key = test_key(&store);
            let first_coord = make_coord();
            let (first_reader, first_lease, first_writer) =
                attach_pending(&store, &key, &first_coord, None);
            let first_writer = first_writer.expect("first writer");
            let first_epoch = first_writer.epoch();
            assert!(matches!(
                first_epoch.write_at(0, b"old"),
                WriterOutcome::Current(Ok(()))
            ));
            let first_bus = EventBus::new(16);
            let mut first_events = first_bus.subscribe::<FileEvent>();
            let first = make_inner(first_reader, first_lease, first_coord, first_bus);

            let successor_coord = make_coord();
            let (successor_reader, successor_lease, successor_writer) =
                attach_pending(&store, &key, &successor_coord, None);
            assert!(successor_writer.is_none());
            let successor_bus = EventBus::new(16);
            let mut successor_events = successor_bus.subscribe();
            let successor = make_inner(
                successor_reader,
                successor_lease,
                Arc::clone(&successor_coord),
                successor_bus,
            );

            first.finalize_fetch(
                &first_epoch,
                completion(0, 3, None, Some(&NetError::Cancelled)),
            );
            assert!(!first_writer.is_current());
            let successor_writer = successor
                .resource_lease
                .as_ref()
                .and_then(ResourceLease::try_take_writer)
                .expect("successor promotion");
            let successor_epoch = successor_writer.epoch();

            first.finalize_fetch(
                &first_epoch,
                completion(0, 3, None, Some(&NetError::Cancelled)),
            );
            assert!(first_events.try_recv().is_err());
            assert!(matches!(
                successor_epoch.write_at(3, b"new"),
                WriterOutcome::Current(Ok(()))
            ));
            successor_coord.set_total_bytes(Some(6));
            successor.finalize_fetch(&successor_epoch, completion(3, 3, None, None));
            assert_ready_bytes(&store, &key, b"oldnew");

            while let Ok(event) = successor_events.try_recv() {
                assert!(!matches!(event.event, FileEvent::Error { .. }));
            }
            drop(successor_writer);
            drop(first_writer);
        }

        #[kithara::test]
        fn dropping_last_file_source_clears_active_session_synchronously() {
            let store = AssetStore::builder(pools())
                .backend(StorageBackend::Memory)
                .cancel(CancelToken::never())
                .build();
            let key = test_key(&store);
            let coord = make_coord();
            let (reader, lease, writer) = attach_pending(&store, &key, &coord, None);
            let inner = make_inner(reader, lease, Arc::clone(&coord), EventBus::new(16));
            let peer = make_peer(&inner, writer);
            inner.arm_reader_waker();
            let source = FileSource::with_inner(Arc::clone(&inner), coord);

            drop(inner);
            drop(source);

            assert!(matches!(
                store.resource_state(&key).expect("resource state"),
                AssetResourceState::Missing
            ));
            assert!(peer.inner.upgrade().is_none());
            let successor_coord = make_coord();
            assert!(matches!(
                store.attach_pending_resource(&key, successor_coord.read_pos_handle(), None),
                Ok(AcquisitionResult::Pending(_))
            ));
        }
    }
    mod seek {
        use super::*;

        /// A forward seek moves the reader cursor past everything already stored. The
        /// next fetch has to start there. That is what a range request buys: the
        /// listener waits for the bytes under the cursor, not for the span they
        /// skipped over.
        #[kithara::test]
        fn a_forward_seek_fetches_from_the_new_cursor() {
            let (_store, _key, inner, writer) = fresh_session(None);
            inner.source.coord.set_total_bytes(Some(4096));
            assert!(matches!(
                writer.epoch().write_at(0, &[0u8; 512]),
                WriterOutcome::Current(Ok(()))
            ));

            inner.source.coord.set_position(3072);

            let peer = make_peer(&inner, Some(writer));
            let lease = inner.resource_lease.as_ref().expect("session lease");

            let PeerAction::Fetch(plan) = peer.next_action(&inner, lease) else {
                panic!("a resource missing bytes under the cursor must fetch");
            };

            assert_eq!(plan.start, 3072);
        }

        /// Everything ahead of the cursor is stored, so the peer has nothing to serve
        /// the listener and falls back to filling the span an earlier seek skipped.
        #[kithara::test]
        fn a_cursor_with_no_gap_ahead_backfills_the_skipped_span() {
            let (_store, _key, inner, writer) = fresh_session(None);
            inner.source.coord.set_total_bytes(Some(4096));
            assert!(matches!(
                writer.epoch().write_at(3072, &[0u8; 1024]),
                WriterOutcome::Current(Ok(()))
            ));

            inner.source.coord.set_position(3072);

            let peer = make_peer(&inner, Some(writer));
            let lease = inner.resource_lease.as_ref().expect("session lease");

            let PeerAction::Fetch(plan) = peer.next_action(&inner, lease) else {
                panic!("a resource missing its head must fetch");
            };

            assert_eq!(plan.start, 0);
        }

        /// A running fetch streams forward from where it started, so a seek that lands
        /// past its write cursor cannot be served by it — it would have to deliver the
        /// whole skipped span first. The peer cancels it instead.
        #[kithara::test]
        fn a_seek_past_the_download_cursor_cancels_the_running_fetch() {
            let (_store, _key, inner, writer) = fresh_session(None);
            inner.source.coord.set_total_bytes(Some(4096));
            let peer = FilePeer::new(&inner, Some(writer));
            let mut cx = Context::from_waker(Waker::noop());

            let Poll::Ready(Some(fetches)) = Peer::poll_next(&peer, &mut cx) else {
                panic!("a resource missing every byte must start a fetch");
            };
            let running = fetches[0]
                .cancel()
                .expect("a peer fetch carries its own cancel")
                .clone();

            inner.source.coord.set_position(3072);

            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Pending));
            assert!(running.is_cancelled());
        }

        /// The peer parks on its own waker while a fetch is in flight, so the
        /// completion that clears the in-flight slot is the only thing left that can
        /// bring it back to plan the replacement fetch.
        #[kithara::test]
        fn a_settled_fetch_wakes_the_parked_peer() {
            let (_store, _key, inner, writer) = fresh_session(None);
            inner.source.coord.set_total_bytes(Some(4096));
            let peer = FilePeer::new(&inner, Some(writer));
            let wake = Arc::new(CountingWake::default());
            let waker = Waker::from(Arc::clone(&wake));
            let mut cx = Context::from_waker(&waker);

            let Poll::Ready(Some(mut fetches)) = Peer::poll_next(&peer, &mut cx) else {
                panic!("a resource missing every byte must start a fetch");
            };
            let on_complete = fetches
                .remove(0)
                .take_on_complete()
                .expect("completion callback");

            inner.source.coord.set_position(3072);
            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Pending));
            let parked = wake.count();

            on_complete(0, None, Some(&NetError::Cancelled));

            assert!(wake.count() > parked);
        }

        /// The write offset a fetch publishes lags the bytes it lands: the storage
        /// wakes the reader inside `write_at`, the offset is stored after. A reader
        /// that consumed those bytes in between sits ahead of the offset without being
        /// ahead of the fetch, so the fetch is not overtaken and must keep running.
        #[kithara::test]
        fn a_reader_consuming_landed_bytes_does_not_cancel_the_fetch() {
            let (_store, _key, inner, writer) = fresh_session(None);
            inner.source.coord.set_total_bytes(Some(4096));
            let epoch = writer.epoch();
            let peer = FilePeer::new(&inner, Some(writer));
            let mut cx = Context::from_waker(Waker::noop());

            let Poll::Ready(Some(fetches)) = Peer::poll_next(&peer, &mut cx) else {
                panic!("a resource missing every byte must start a fetch");
            };
            let running = fetches[0]
                .cancel()
                .expect("a peer fetch carries its own cancel")
                .clone();

            assert!(matches!(
                epoch.write_at(0, &[0u8; 512]),
                WriterOutcome::Current(Ok(()))
            ));
            inner.source.coord.set_position(256);

            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Pending));
            assert!(!running.is_cancelled());
        }

        /// A cancelled fetch relinquishes without committing and its replacement is
        /// planned from the resource it left behind. When it had already landed
        /// everything, the plan finds no gap: the writer commits instead of parking,
        /// or every consumer waits on a resource that is complete.
        #[kithara::test]
        fn a_writer_with_nothing_left_to_fetch_commits() {
            let (store, key, inner, writer) = fresh_session(None);
            inner.source.coord.set_total_bytes(Some(4));
            assert!(matches!(
                writer.epoch().write_at(0, b"done"),
                WriterOutcome::Current(Ok(()))
            ));

            let peer = make_peer(&inner, Some(writer));
            let lease = inner.resource_lease.as_ref().expect("session lease");

            assert!(matches!(peer.next_action(&inner, lease), PeerAction::Done));
            assert_ready_bytes(&store, &key, b"done");
        }

        /// A plan bounded by the demand watermark can run out of gaps while bytes
        /// beyond it are still missing. That is a wait for demand, not a complete
        /// resource: committing there would truncate it.
        #[kithara::test]
        fn a_plan_bounded_by_demand_does_not_commit_a_partial_resource() {
            let (store, key, inner, writer) = fresh_session(Some(512));
            inner.source.coord.set_total_bytes(Some(4096));
            assert!(matches!(
                writer.epoch().write_at(0, &[0u8; 512]),
                WriterOutcome::Current(Ok(()))
            ));

            let peer = make_peer(&inner, Some(writer));
            let lease = inner.resource_lease.as_ref().expect("session lease");

            assert!(matches!(
                peer.next_action(&inner, lease),
                PeerAction::Pending
            ));
            assert!(matches!(
                store.resource_state(&key).expect("resource state"),
                AssetResourceState::Active
            ));
        }

        /// A backfill fetch starts behind the cursor by construction — it fills what an
        /// earlier seek skipped. The cursor being ahead of it is the normal case, not a
        /// reason to cancel it, or the peer would kill every backfill it starts.
        #[kithara::test]
        fn a_backfill_fetch_survives_a_cursor_ahead_of_it() {
            let (_store, _key, inner, writer) = fresh_session(None);
            inner.source.coord.set_total_bytes(Some(4096));
            assert!(matches!(
                writer.epoch().write_at(3072, &[0u8; 1024]),
                WriterOutcome::Current(Ok(()))
            ));
            inner.source.coord.set_position(3072);

            let peer = FilePeer::new(&inner, Some(writer));
            let mut cx = Context::from_waker(Waker::noop());

            let Poll::Ready(Some(fetches)) = Peer::poll_next(&peer, &mut cx) else {
                panic!("a resource missing its head must fetch");
            };
            let backfill = fetches[0]
                .cancel()
                .expect("a peer fetch carries its own cancel")
                .clone();

            inner.source.coord.set_position(4096);

            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Pending));
            assert!(!backfill.is_cancelled());
        }

        /// Before the first response has answered how long the resource is, a cursor
        /// past the end is indistinguishable from one inside it. Anchoring a fetch
        /// there would ask for bytes that may not exist and would replace the very
        /// request that reports the real length, so the cursor stays out of it.
        #[kithara::test]
        fn a_cursor_does_not_steer_before_the_extent_is_known() {
            let (_store, _key, inner, writer) = fresh_session(None);
            assert!(matches!(
                writer.epoch().write_at(0, &[0u8; 512]),
                WriterOutcome::Current(Ok(()))
            ));

            inner.source.coord.set_position(3072);

            let peer = make_peer(&inner, Some(writer));
            let lease = inner.resource_lease.as_ref().expect("session lease");

            let PeerAction::Fetch(plan) = peer.next_action(&inner, lease) else {
                panic!("a resource of unknown extent must fetch");
            };

            assert_eq!(plan.start, 0);
        }

        /// A cursor past a known extent has no bytes to ask for, so it steers nothing
        /// and the peer keeps filling the resource it does have.
        #[kithara::test]
        fn a_cursor_past_the_known_extent_does_not_steer() {
            let (_store, _key, inner, writer) = fresh_session(None);
            inner.source.coord.set_total_bytes(Some(4096));
            assert!(matches!(
                writer.epoch().write_at(0, &[0u8; 512]),
                WriterOutcome::Current(Ok(()))
            ));

            inner.source.coord.set_position(9000);

            let peer = make_peer(&inner, Some(writer));
            let lease = inner.resource_lease.as_ref().expect("session lease");

            let PeerAction::Fetch(plan) = peer.next_action(&inner, lease) else {
                panic!("a resource missing bytes must fetch");
            };

            assert_eq!(plan.start, 512);
        }

        /// The fetch that establishes the extent must survive a cursor that cannot
        /// steer: cancelling it would hand the replacement the same anchor it already
        /// has, and the length would never arrive.
        #[kithara::test]
        fn a_fetch_of_unknown_extent_survives_a_seek() {
            let (_store, _key, inner, writer) = fresh_session(None);
            let peer = FilePeer::new(&inner, Some(writer));
            let mut cx = Context::from_waker(Waker::noop());

            let Poll::Ready(Some(fetches)) = Peer::poll_next(&peer, &mut cx) else {
                panic!("a resource missing every byte must start a fetch");
            };
            let head = fetches[0]
                .cancel()
                .expect("a peer fetch carries its own cancel")
                .clone();

            inner.source.coord.set_position(3072);

            assert!(matches!(Peer::poll_next(&peer, &mut cx), Poll::Pending));
            assert!(!head.is_cancelled());
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    use fixtures::BlockingWake;
    use fixtures::{
        CountingWake, assert_ready_bytes, attach_pending, completion, fresh_session, make_coord,
        make_inner, make_inner_with_cancel, make_peer, test_key,
    };
}
