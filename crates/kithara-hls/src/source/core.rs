#![forbid(unsafe_code)]

use std::{ops::Range, sync::Arc};

use delegate::delegate;
use kithara_events::EventBus;
use kithara_platform::{time::Duration, tokio::sync::Notify};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    BoxedHooks, MediaInfo, ReadOutcome, SegmentLayout, Source, SourcePhase, SourceSeekAnchor,
    StreamResult, Timeline,
};

use crate::{
    coord::HlsCoord, invalidation::HlsInvalidationGuard, peer::HlsPeer, reader::HlsReaderHooks,
};

/// HLS source: thin façade over [`HlsCoord`].
///
/// Owns the per-track event bus and the bound peer handle / wake. Every
/// `Source` trait method that touches segments, byte ranges, or timeline
/// state is forwarded to `HlsCoord` via `delegate!` — coord is the
/// single owner of the active-variant atomic, the cross-variant
/// history, and the asset store. `HlsSource` keeps only what coord
/// legitimately should not know about: the [`EventBus`] (for
/// [`HlsReaderHooks`]) and the [`HlsPeer`] handle (for teardown on drop
/// and for the ABR handle the audio FSM consumes).
pub struct HlsSource {
    coord: Arc<HlsCoord>,
    /// Event bus the track was created against. Forwarded to
    /// [`HlsReaderHooks`] in [`Source::take_reader_hooks`] so the
    /// decoder's per-seek / per-chunk signals reach test subscribers
    /// as `HlsEvent::ReaderSeek` / `HlsEvent::ReadProgress`.
    bus: EventBus,
    hls_peer: Option<Arc<HlsPeer>>,
    peer_handle: Option<kithara_stream::dl::PeerHandle>,
    /// Reader→peer wake handle. Cloned from the owning [`HlsPeer`] once
    /// it is bound via [`Self::set_hls_peer`]. Also installed on
    /// [`HlsCoord::set_peer_wake`] so coord-side methods can wake the
    /// peer directly.
    peer_wake: Option<Arc<Notify>>,
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
        let wake = peer.reader_wake();
        self.coord.set_peer_wake(&wake);
        self.peer_wake = Some(wake);
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

    fn as_segment_layout(&self) -> Option<Arc<dyn SegmentLayout>> {
        Some(Arc::clone(&self.coord) as Arc<dyn SegmentLayout>)
    }

    fn current_variant(&self) -> Option<kithara_events::VariantInfo> {
        self.abr_handle()?.current_variant()
    }

    fn make_notify_fn(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        let notify = self.peer_wake.clone()?;
        Some(Box::new(move || notify.notify_one()))
    }

    fn media_info(&self) -> Option<MediaInfo> {
        Some(self.coord.media_info())
    }

    fn take_reader_hooks(&mut self) -> Option<BoxedHooks> {
        let hooks = HlsReaderHooks::new(
            self.bus.clone(),
            Arc::clone(&self.coord),
            self.coord.timeline.seek_epoch_handle(),
        );
        Some(Box::new(hooks))
    }

    delegate! {
        to self.coord {
            fn len(&self) -> Option<u64>;
            fn position(&self) -> u64;
            fn advance(&self, n: u64);
            fn set_position(&self, pos: u64);
            fn timeline(&self) -> Timeline;
            fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            fn format_change_segment_range(&self) -> StreamResult<Range<u64>>;
            fn notify_waiting(&self);
            fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome>;
            fn wait_range(
                &mut self,
                range: Range<u64>,
                timeout: Option<Duration>,
            ) -> StreamResult<WaitOutcome>;
            fn seek_time_anchor(
                &mut self,
                position: Duration,
            ) -> StreamResult<Option<SourceSeekAnchor>>;
            fn set_seek_epoch(&mut self, seek_epoch: u64);
            fn clear_variant_fence(&mut self);
            fn has_variant_change_pending(&self) -> bool;
        }
    }
}
