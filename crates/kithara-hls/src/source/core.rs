#![forbid(unsafe_code)]

use std::{ops::Range, sync::Arc};

use delegate::delegate;
use kithara_events::EventBus;
use kithara_platform::time::Duration;
use kithara_storage::WaitOutcome;
use kithara_stream::{
    Activity, BoxedEventSink, ByteMap, DeferredWake, MediaInfo, PlayheadRead, PlayheadWrite,
    ReadOutcome, SeekControl, SeekObserve, Source, SourcePhase, StreamResult,
};

use crate::{
    coord::HlsCoord, invalidation::HlsInvalidationGuard, peer::HlsPeer, reader::HlsReaderEventSink,
};

/// HLS source: thin façade over [`HlsCoord`].
///
/// Owns the per-track event bus and the bound peer handle / wake. Every
/// `Source` trait method that touches segments, byte ranges, or timeline
/// state is forwarded to `HlsCoord` via `delegate!` — coord is the
/// single owner of the active-variant atomic, the cross-variant
/// history, and the asset store. `HlsSource` keeps only what coord
/// legitimately should not know about: the [`EventBus`] (for
/// [`HlsReaderEventSink`]) and the [`HlsPeer`] handle (for teardown on
/// drop and for the ABR handle the audio FSM consumes).
pub struct HlsSource {
    coord: Arc<HlsCoord>,
    /// Event bus the track was created against. Forwarded to
    /// [`HlsReaderEventSink`] in [`Source::take_reader_event_sink`] so
    /// the decoder's per-seek / per-chunk signals reach test subscribers
    /// as `HlsEvent::ReaderSeek` / `HlsEvent::ReadProgress`.
    bus: EventBus,
    hls_peer: Option<Arc<HlsPeer>>,
    peer_handle: Option<kithara_stream::dl::PeerHandle>,
    /// Reader→peer wake handle. Cloned from the owning [`HlsPeer`] once it is
    /// bound via [`Self::set_hls_peer`], and returned by [`Source::peer_wake`]
    /// so the reader drivers (`Stream::probe_read` / `read` / `prime_seek_range`
    /// and the audio FSM) can arm or notify the peer themselves.
    peer_wake: Option<Arc<DeferredWake>>,
    /// Registry deregistration guard for the app-wide shared store. `Some`
    /// only when an [`HlsStore`](crate::HlsStore) was injected; dropping it
    /// removes this stream's eviction routing entry. `None` for a
    /// private per-stream store.
    invalidation_guard: Option<HlsInvalidationGuard>,
}

impl HlsSource {
    pub(crate) fn new(coord: Arc<HlsCoord>, bus: EventBus) -> Self {
        Self {
            coord,
            bus,
            hls_peer: None,
            peer_handle: None,
            peer_wake: None,
            invalidation_guard: None,
        }
    }

    /// Pin the shared-store deregistration guard to this source's
    /// lifetime. `None` keeps the private per-stream behaviour.
    pub(crate) fn set_invalidation_guard(&mut self, guard: Option<HlsInvalidationGuard>) {
        self.invalidation_guard = guard;
    }

    pub(crate) fn set_hls_peer(&mut self, peer: Arc<HlsPeer>) {
        self.peer_wake = Some(peer.reader_wake());
        self.hls_peer = Some(peer);
    }

    pub(crate) fn set_peer_handle(&mut self, handle: kithara_stream::dl::PeerHandle) {
        self.peer_handle = Some(handle);
    }
}

impl Drop for HlsSource {
    fn drop(&mut self) {
        if let Some(ref peer) = self.hls_peer {
            peer.teardown();
        }
    }
}

impl Source for HlsSource {
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.peer_handle.as_ref().map(|h| h.abr().clone())
    }

    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        Some(Arc::clone(&self.coord) as Arc<dyn ByteMap>)
    }

    fn peer_wake(&self) -> Option<Arc<DeferredWake>> {
        self.peer_wake.clone()
    }

    fn set_worker_wake(&self, wake: Arc<dyn kithara_stream::WorkerWake>) {
        self.coord.set_worker_wake(wake);
    }

    fn variant_control(&self) -> Option<Arc<dyn kithara_stream::VariantControl>> {
        Some(Arc::clone(&self.coord) as Arc<dyn kithara_stream::VariantControl>)
    }

    fn media_info(&self) -> Option<MediaInfo> {
        Some(self.coord.media_info())
    }

    fn take_reader_event_sink(&mut self) -> Option<BoxedEventSink> {
        let sink = HlsReaderEventSink::new(
            self.bus.clone(),
            Arc::clone(&self.coord),
            self.coord.seek_epoch_handle(),
        );
        Some(Box::new(sink))
    }

    fn playhead_read(&self) -> Arc<dyn PlayheadRead> {
        self.coord.playhead_read()
    }

    fn playhead_write(&self) -> Arc<dyn PlayheadWrite> {
        self.coord.playhead_write()
    }

    fn seek_observe(&self) -> Arc<dyn SeekObserve> {
        self.coord.seek_observe()
    }

    fn seek_control(&self) -> Arc<dyn SeekControl> {
        self.coord.seek_control()
    }

    fn activity(&self) -> Arc<dyn Activity> {
        self.coord.activity()
    }

    delegate! {
        to self.coord {
            fn len(&self) -> Option<u64>;
            fn position(&self) -> u64;
            fn advance(&self, n: u64);
            fn set_position(&self, pos: u64);
            fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome>;
            fn wait_range(
                &mut self,
                range: Range<u64>,
                timeout: Option<Duration>,
            ) -> StreamResult<WaitOutcome>;
        }
    }
}
