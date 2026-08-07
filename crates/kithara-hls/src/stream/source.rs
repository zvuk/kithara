#![forbid(unsafe_code)]

use std::ops::Range;

use delegate::delegate;
use kithara_assets::EvictionSubscription;
use kithara_events::{DeferredBus, HlsEvent};
use kithara_platform::{CancelScope, sync::Arc, time::Duration};
use kithara_storage::WaitOutcome;
use kithara_stream::{
    Activity, BoxedEventSink, ByteMap, DeferredWake, MediaInfo, PlayheadRead, PlayheadWrite,
    ReadOutcome, SeekControl, SeekObserve, SeekPrepare, Source, SourcePhase, StreamResult,
};

use super::coord::HlsCoord;
use crate::{peer::HlsPeer, reader::HlsReaderEventSink};

/// HLS source: thin façade over [`HlsCoord`].
///
/// Owns the per-track event bus and the bound peer handle / wake. Every `Source` trait method that
/// touches segments, byte ranges, or timeline state is forwarded to `HlsCoord` via `delegate!` —
/// coord is the single owner of the active-variant atomic, the cross-variant history, and the asset
/// store. `HlsSource` keeps only what coord legitimately should not know about: the
/// [`EventBus`](kithara_events::EventBus) (for [`HlsReaderEventSink`]) and the [`HlsPeer`] handle
/// (for teardown on drop and for the ABR handle the audio FSM consumes).
pub struct HlsSource {
    coord: Arc<HlsCoord>,
    /// Deferred HLS event sink shared with the reader hooks and fence-ack path.
    /// [`HlsReaderEventSink`] in [`Source::take_reader_event_sink`] so
    /// the decoder's per-seek / per-chunk signals reach test subscribers
    /// as `HlsEvent::ReaderSeek` / `HlsEvent::ReadProgress`.
    emit: Arc<DeferredBus<HlsEvent>>,
    stream_scope: CancelScope,
    hls_peer: Option<Arc<HlsPeer>>,
    /// Eviction-subscription guard. Dropping it deregisters this stream's
    /// eviction routing entry from the store (shared or private).
    invalidation_guard: Option<EvictionSubscription>,
    peer_handle: Option<kithara_stream::dl::PeerHandle>,
    /// Reader→peer wake handle. Cloned from the owning [`HlsPeer`] once it is
    /// bound via [`Self::set_hls_peer`], and returned by [`Source::peer_wake`]
    /// so the reader drivers (`Stream::probe_read` / `read` / `prime_seek_range`
    /// and the audio FSM) can arm or notify the peer themselves.
    peer_wake: Option<Arc<DeferredWake>>,
}

impl HlsSource {
    pub(crate) fn new(
        coord: Arc<HlsCoord>,
        emit: Arc<DeferredBus<HlsEvent>>,
        stream_scope: CancelScope,
    ) -> Self {
        Self {
            coord,
            stream_scope,
            emit,
            hls_peer: None,
            peer_handle: None,
            peer_wake: None,
            invalidation_guard: None,
        }
    }

    pub(crate) fn set_hls_peer(&mut self, peer: Arc<HlsPeer>) {
        self.peer_wake = Some(peer.reader_wake());
        self.hls_peer = Some(peer);
    }

    /// Pin the eviction-subscription deregistration guard to this
    /// source's lifetime.
    pub(crate) fn set_invalidation_guard(&mut self, guard: EvictionSubscription) {
        self.invalidation_guard = Some(guard);
    }

    pub(crate) fn set_peer_handle(&mut self, handle: kithara_stream::dl::PeerHandle) {
        self.peer_handle = Some(handle);
    }
}

impl Drop for HlsSource {
    fn drop(&mut self) {
        self.stream_scope.cancel();
        if let Some(ref peer) = self.hls_peer {
            peer.teardown();
        }
    }
}

impl Source for HlsSource {
    fn abr_handle(&self) -> Option<kithara_abr::AbrHandle> {
        self.peer_handle.as_ref().map(|h| h.abr().clone())
    }

    /// The reader consumed `n` bytes. `dispatch` anchors the look-ahead window
    /// on the live cursor, so consumption is what makes its decision stale —
    /// and therefore what must re-open it. `take_prefetch_resume` reports the
    /// cursor crossing the byte `dispatch` itself named when it turned a
    /// segment away, so the wake fires once per deferred segment rather than
    /// once per read. Without it the decision taken at the old cursor stands
    /// until the reader hits a missing range, and a bounded look-ahead never
    /// refills *ahead* of playback — it only refills after playback starved.
    ///
    /// Armed, not notified: `advance` runs on the forbid-blocking produce core
    /// (`Stream::probe_read`), so the cross-thread `notify_one` belongs to the
    /// scheduler shell, which flushes this same handle once per pass.
    fn advance(&self, n: u64) {
        self.coord.advance(n);
        if self.coord.take_prefetch_resume()
            && let Some(ref wake) = self.peer_wake
        {
            wake.arm();
        }
    }

    fn byte_map(&self) -> Option<Arc<dyn ByteMap>> {
        Some(Arc::clone(&self.coord) as Arc<dyn ByteMap>)
    }

    fn peer_wake(&self) -> Option<Arc<DeferredWake>> {
        self.peer_wake.clone()
    }

    fn seek_prepare(&self) -> Option<Arc<dyn SeekPrepare>> {
        Some(Arc::clone(&self.coord) as Arc<dyn SeekPrepare>)
    }

    fn take_reader_event_sink(&mut self) -> Option<BoxedEventSink> {
        let sink = HlsReaderEventSink::new(
            Arc::clone(&self.emit),
            Arc::clone(&self.coord),
            self.coord.seek_epoch_handle(),
        );
        Some(Box::new(sink))
    }

    fn variant_control(&self) -> Option<Arc<dyn kithara_stream::VariantControl>> {
        Some(Arc::clone(&self.coord) as Arc<dyn kithara_stream::VariantControl>)
    }

    delegate! {
        to self.coord {
            fn len(&self) -> Option<u64>;
            fn position(&self) -> u64;
            fn set_position(&self, pos: u64);
            fn phase_at(&self, range: Range<u64>) -> SourcePhase;
            fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> StreamResult<ReadOutcome>;
            fn wait_range(
                &mut self,
                range: Range<u64>,
                timeout: Option<Duration>,
            ) -> StreamResult<WaitOutcome>;
            fn activity(&self) -> Arc<dyn Activity>;
            #[expr(Some($))]
            fn media_info(&self) -> Option<MediaInfo>;
            fn playhead_read(&self) -> Arc<dyn PlayheadRead>;
            fn playhead_write(&self) -> Arc<dyn PlayheadWrite>;
            fn seek_control(&self) -> Arc<dyn SeekControl>;
            fn seek_observe(&self) -> Arc<dyn SeekObserve>;
            fn set_worker_wake(&self, wake: Arc<dyn kithara_stream::WorkerWake>);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use kithara_abr::{Abr, AbrController, AbrSettings, AbrState};
    use kithara_assets::{AssetResource, AssetSource, AssetStore, StorageBackend};
    use kithara_events::{AbrMode, EventBus, VariantIndex};
    use kithara_platform::{CancelToken, sync::ThreadGate, time::Duration as PlatformDuration};
    use kithara_stream::{AudioCodec, ContainerFormat, PlayheadState, SeekState};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        config::SizeProbeMethod,
        peer::HlsPeer,
        playlist::{PlaylistState, SegmentState, VariantState},
        segment::{MediaSegment, Segment, SegmentContent, SegmentSize, SegmentSlotState},
        signal::SizeSignal,
        stream::HlsCoordEnv,
        variant::{HlsVariant, PlanCtx, VariantParts},
    };

    struct TestAbrPeer {
        state: Arc<AbrState>,
    }

    impl Abr for TestAbrPeer {
        fn state(&self) -> Option<Arc<AbrState>> {
            Some(Arc::clone(&self.state))
        }
    }

    /// One HLS track of equal-sized segments behind a real `HlsSource`, with the
    /// peer wake the source arms exposed so a test can observe it.
    struct Fixture {
        variant: Arc<HlsVariant>,
        wake: Arc<DeferredWake>,
        cancel: CancelToken,
        source: HlsSource,
        ctx: PlanCtx,
    }

    impl Fixture {
        const SEGMENT_BYTES: u64 = 100;
        const SEGMENT_COUNT: u32 = 4;

        fn coord(bus: &EventBus, ctx: &PlanCtx, cancel: CancelToken) -> Arc<HlsCoord> {
            let playlist = Self::playlist_state();
            let segments: Vec<Segment> = (0..Self::SEGMENT_COUNT)
                .map(|idx| {
                    Segment::Media(MediaSegment {
                        url: Self::segment_url(idx),
                        resource_id: ctx
                            .scope
                            .key(&AssetResource::Url(Self::segment_url(idx)))
                            .expect("segment key"),
                        state: SegmentSlotState::missing(),
                        size: SegmentSize::seed(Self::SEGMENT_BYTES),
                        content: SegmentContent::Plain,
                        decode_time: PlatformDuration::from_secs(u64::from(idx) * 2),
                        duration: PlatformDuration::from_secs(2),
                    })
                })
                .collect();
            let variant = VariantParts {
                segments,
                init: None,
                seek_obs: Arc::new(SeekState::new()) as Arc<dyn SeekObserve>,
                codec: playlist.variant_codec(0),
                container: playlist.variant_container(0),
            }
            .into_variant(0, ctx);
            let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
            let publisher = state.publisher();
            let peer: Arc<dyn Abr> = Arc::new(TestAbrPeer { state });
            let controller = AbrController::new(AbrSettings::default());
            Arc::new(HlsCoord::new(
                HlsCoordEnv {
                    cancel,
                    scope: ctx.scope.clone(),
                    headers: None,
                    emit: Arc::new(DeferredBus::new(bus.clone(), 8)),
                    signal: ctx.signal.clone(),
                },
                Arc::new(PlayheadState::new()),
                Arc::new(SeekState::new()),
                controller.register(&peer),
                publisher,
                Arc::from(vec![variant]),
            ))
        }

        fn plan_ctx(bus: &EventBus, look_ahead_bytes: u64, cancel: &CancelToken) -> PlanCtx {
            let store = Arc::new(
                AssetStore::builder()
                    .backend(StorageBackend::Memory)
                    .cancel(cancel.clone())
                    .build(),
            );
            PlanCtx {
                bus: bus.clone(),
                prefetch_budget: 8,
                scope: store
                    .scope::<crate::Hls>(&AssetSource::Remote {
                        url: "https://example.com/master.m3u8"
                            .parse()
                            .expect("master url"),
                        discriminator: Some("source-test".to_owned()),
                    })
                    .expect("source asset scope"),
                seek_epoch: 0,
                look_ahead_bytes: Some(look_ahead_bytes),
                look_ahead_segments: None,
                headers: None,
                size_probe_method: SizeProbeMethod::Head,
                signal: SizeSignal::new(Arc::new(ThreadGate::default()), Arc::new(OnceLock::new())),
            }
        }

        fn playlist_state() -> Arc<PlaylistState> {
            Arc::new(PlaylistState::new(vec![VariantState {
                codec: Some(AudioCodec::AacLc),
                container: Some(ContainerFormat::Fmp4),
                init_url: None,
                segments: (0..Self::SEGMENT_COUNT)
                    .map(|idx| SegmentState {
                        url: Self::segment_url(idx),
                        duration: PlatformDuration::from_secs(2),
                        byte_range_len: Some(Self::SEGMENT_BYTES),
                    })
                    .collect(),
            }]))
        }

        fn segment_url(idx: u32) -> url::Url {
            format!("https://example.com/seg{idx}.m4s")
                .parse()
                .expect("segment url")
        }

        fn with_look_ahead(look_ahead_bytes: u64) -> Self {
            let bus = EventBus::new(8);
            let cancel = CancelToken::never();
            let ctx = Self::plan_ctx(&bus, look_ahead_bytes, &cancel);
            let coord = Self::coord(&bus, &ctx, cancel.clone());
            let variant = coord.active();
            let mut source = HlsSource::new(
                Arc::clone(&coord),
                Arc::new(DeferredBus::new(bus, 8)),
                CancelScope::new(Some(cancel.clone())),
            );
            let peer = Arc::new(HlsPeer::new(
                coord.seek_observe(),
                coord.activity(),
                AbrMode::Auto(Some(VariantIndex::new(0))),
            ));
            let wake = peer.reader_wake();
            source.set_hls_peer(peer);
            Self {
                variant,
                wake,
                cancel,
                source,
                ctx,
            }
        }
    }

    /// The look-ahead window is anchored on the reader cursor, so the reader
    /// consuming bytes is the only thing that can make it stale - and therefore
    /// the only thing that can make it advance. Both halves of that contract are
    /// asserted here: a segment the window turned away becomes dispatchable once
    /// the cursor moves past the first one, and the cursor move itself wakes the
    /// peer, so the re-dispatch needs no external event (no seek, no ABR tick, no
    /// stalled fetch, and above all no timer).
    #[kithara::test]
    fn reader_consumption_advances_the_prefetch_window() {
        // One and a half segments: wide enough for segments 0 and 1 from the
        // stream start, narrow enough that segment 2 stays outside the window
        // until the reader drains the first one.
        let look_ahead = Fixture::SEGMENT_BYTES + Fixture::SEGMENT_BYTES / 2;
        let fixture = Fixture::with_look_ahead(look_ahead);

        fixture.variant.rebuild(&fixture.ctx, 0);
        let opening = fixture.variant.dispatch_from(
            &fixture.ctx,
            8,
            fixture.source.position(),
            None,
            true,
            fixture.cancel.clone(),
        );
        assert_eq!(
            opening.len(),
            2,
            "the window holds segments 0 and 1; segment 2 starts beyond the cap"
        );
        assert!(
            !fixture.wake.flush(),
            "no reader progress yet: nothing may have woken the peer"
        );

        fixture.source.advance(Fixture::SEGMENT_BYTES);

        assert!(
            fixture.wake.flush(),
            "the reader consumed a segment: that progress must wake the peer on its own"
        );
        let after_consumption = fixture.variant.dispatch_from(
            &fixture.ctx,
            8,
            fixture.source.position(),
            None,
            true,
            fixture.cancel.clone(),
        );
        assert_eq!(
            after_consumption.len(),
            1,
            "the window slid with the cursor and now reaches segment 2"
        );
        assert_eq!(after_consumption[0].url, Fixture::segment_url(2));
    }
}
