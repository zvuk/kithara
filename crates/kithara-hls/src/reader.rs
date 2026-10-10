#![forbid(unsafe_code)]

#[cfg(test)]
use kithara_abr::{AbrMode, VariantIndex};
use kithara_bufpool::HasPool;
use kithara_events::DeferredBus;
use kithara_platform::sync::Arc;
use kithara_stream::{PrerollHint, ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal};

use crate::{
    HlsEvent,
    stream::{HlsCoord, HlsSession},
};

enum HlsReaderRoute<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    Active(Arc<HlsCoord<S>>),
    Session(Arc<HlsSession<S>>),
}

impl<S> HlsReaderRoute<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn find_at_offset(&self, cursor: u64) -> Option<(u32, u64, u64)> {
        self.map(
            |coord| coord.find_at_offset(cursor),
            |session| session.find_at_offset(cursor),
        )
    }

    fn len(&self) -> Option<u64> {
        self.map(HlsCoord::len, HlsSession::len)
    }

    fn map<T>(
        &self,
        on_active: impl FnOnce(&HlsCoord<S>) -> T,
        on_session: impl FnOnce(&HlsSession<S>) -> T,
    ) -> T {
        match self {
            Self::Active(coord) => on_active(coord),
            Self::Session(session) => on_session(session),
        }
    }

    fn position(&self) -> u64 {
        self.map(HlsCoord::position, HlsSession::position)
    }

    fn variant_index(&self) -> usize {
        self.map(HlsCoord::variant_index, HlsSession::variant_index)
    }
}

/// Decoder→HLS reader hook bridge: turns the decoder's per-chunk and
/// per-seek signals into [`HlsEvent`]s on the track's [`EventBus`].
///
/// Mirrors `kithara-file`'s `FileReaderEventSink` but resolves the landed byte
/// against the active or fixed incoming session captured at construction.
pub(crate) struct HlsReaderEventSink<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    bus: Arc<DeferredBus<HlsEvent>>,
    route: HlsReaderRoute<S>,
    /// `(variant_index, segment_index)` of the last segment the
    /// reader was observed in. A change between `on_chunk` calls
    /// drives [`HlsEvent::SegmentReadStart`]; the same pair held
    /// across a seek is intentionally treated as a no-op for the
    /// boundary event (the seek itself was already announced).
    last_segment: Option<(usize, usize)>,
    /// Last observed cursor, used as the origin of a landed reader seek.
    last_cursor: u64,
    last_progress_cursor: u64,
    last_segment_start_cursor: u64,
}

impl<S> HlsReaderEventSink<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    pub(crate) fn new(bus: Arc<DeferredBus<HlsEvent>>, coord: Arc<HlsCoord<S>>) -> Self {
        Self::with_route(bus, HlsReaderRoute::Active(coord))
    }

    pub(crate) fn for_session(
        bus: Arc<DeferredBus<HlsEvent>>,
        session: Arc<HlsSession<S>>,
    ) -> Self {
        Self::with_route(bus, HlsReaderRoute::Session(session))
    }

    fn maybe_publish_end_of_stream(&mut self) {
        if self.last_segment.take().is_none() {
            return;
        }
        self.bus.enqueue(HlsEvent::EndOfStream);
    }

    fn maybe_publish_read_progress(&mut self, cursor: u64) {
        const READ_PROGRESS_MIN_DELTA: u64 = 256 * 1024;
        if cursor.abs_diff(self.last_progress_cursor) < READ_PROGRESS_MIN_DELTA {
            return;
        }
        self.last_progress_cursor = cursor;
        self.bus.enqueue(HlsEvent::ReadProgress {
            position: cursor,
            total: self.route.len(),
        });
    }

    /// Announce a [`HlsEvent::SegmentReadStart`] when the reader's
    /// `cursor` lands in a different `(variant, segment_index)` than
    /// the previously observed one.
    fn maybe_publish_segment_start(&mut self, cursor: u64) {
        let Some((seg_idx, seg_start, _size)) = self.route.find_at_offset(cursor) else {
            return;
        };
        let variant = self.route.variant_index();
        let seg_us = seg_idx as usize;
        let key = (variant, seg_us);
        if self.last_segment == Some(key) {
            return;
        }
        if let Some((prev_variant, prev_segment)) = self.last_segment {
            self.bus.enqueue(HlsEvent::SegmentReadComplete {
                variant: prev_variant,
                segment_index: prev_segment,
                bytes_read: cursor.saturating_sub(self.last_segment_start_cursor),
            });
        }
        self.last_segment = Some(key);
        self.last_cursor = seg_start;
        self.last_segment_start_cursor = seg_start;
        self.bus.enqueue(HlsEvent::SegmentReadStart {
            variant,
            segment_index: seg_us,
            byte_offset: seg_start,
        });
    }

    fn publish_seek(&self, from: u64, to: u64) {
        let (variant, segment_index, byte_in_segment) = match self.route.find_at_offset(to) {
            Some((seg, seg_start, _size)) => (
                Some(self.route.variant_index()),
                Some(seg as usize),
                Some(to.saturating_sub(seg_start)),
            ),
            None => (None, None, None),
        };
        self.bus.enqueue(HlsEvent::ReaderSeek {
            variant,
            segment_index,
            byte_in_segment,
            from_offset: from,
            to_offset: to,
        });
    }

    fn with_route(bus: Arc<DeferredBus<HlsEvent>>, route: HlsReaderRoute<S>) -> Self {
        let last_cursor = route.position();
        Self {
            bus,
            route,
            last_cursor,
            last_segment_start_cursor: last_cursor,
            last_progress_cursor: last_cursor,
            last_segment: None,
        }
    }
}

impl<S> ReaderEventSink for HlsReaderEventSink<S>
where
    S: HasPool<u8> + Send + Sync + 'static,
{
    fn flush(&mut self) {
        self.bus.flush();
    }

    fn on_chunk(&mut self, signal: ReaderChunkSignal) {
        let cursor = self.route.position();
        match signal {
            ReaderChunkSignal::Chunk => {
                self.maybe_publish_segment_start(cursor);
                self.maybe_publish_read_progress(cursor);
                self.last_cursor = cursor;
            }
            ReaderChunkSignal::Eof => self.maybe_publish_end_of_stream(),
            _ => {}
        }
    }

    fn on_seek(&mut self, signal: ReaderSeekSignal) {
        let ReaderSeekSignal::Landed {
            landed_byte,
            preroll,
        } = signal
        else {
            return;
        };
        if let PrerollHint::Required(byte) = preroll {
            let _ = byte;
        }
        let Some(to) = landed_byte else {
            return;
        };
        let from = self.last_cursor;
        self.last_cursor = to;
        self.last_segment = None;
        self.publish_seek(from, to);
    }
}
#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use kithara_abr::{Abr, AbrController, AbrMock, AbrSettings, AbrState};
    use kithara_assets::{AssetResource, AssetSource, AssetStore, StorageBackend};
    use kithara_events::EventBus;
    use kithara_platform::{
        CancelToken,
        sync::{Arc, ThreadGate},
    };
    use kithara_stream::{ActivityWriter, AudioCodec, ContainerFormat, PlayheadState};
    use kithara_test_utils::kithara;
    use unimock::{MockFn, Unimock, matching};

    use super::*;
    use crate::{
        config::HlsConfig,
        playlist::{PlaylistState, SegmentState, VariantState},
        segment::{MediaSegment, Segment, SegmentContent, SegmentSize, SegmentSlotState},
        signal::SizeSignal,
        stream::HlsCoordEnv,
        variant::{PlanCtx, VariantParts},
    };

    type TestHlsCoord = HlsCoord<crate::test_pools::TestPools>;
    type TestPlanCtx = PlanCtx<crate::test_pools::TestPools>;

    fn test_ctx(bus: &EventBus) -> TestPlanCtx {
        let cancel = CancelToken::never();
        let store = Arc::new(
            AssetStore::builder(crate::test_pools::pools())
                .backend(StorageBackend::Memory)
                .cancel(cancel)
                .build(),
        );
        PlanCtx {
            bus: bus.clone(),
            scope: store
                .scope::<crate::Hls<crate::test_pools::TestPools>>(&AssetSource::Remote {
                    url: "https://example.com/master.m3u8"
                        .parse()
                        .expect("master url"),
                    discriminator: Some("reader-test".to_owned()),
                })
                .expect("reader asset scope"),
            look_ahead_segments: None,
            signal: SizeSignal::new(Arc::new(ThreadGate::default()), Arc::new(OnceLock::new())),
            config: Arc::new(
                HlsConfig::for_url("https://example.com/master.m3u8".parse().expect("url"))
                    .store((*store).clone())
                    .pools(crate::test_pools::pools())
                    .download_batch_size(1)
                    .build(),
            ),
        }
    }

    fn playlist_state() -> Arc<PlaylistState> {
        Arc::new(PlaylistState::new(vec![VariantState {
            codec: Some(AudioCodec::AacLc),
            container: Some(ContainerFormat::Fmp4),
            init_url: None,
            segments: vec![
                SegmentState {
                    url: "https://example.com/seg0.m4s".parse().expect("url"),
                    duration: kithara_platform::time::Duration::from_secs(2),
                    byte_range_len: Some(100),
                },
                SegmentState {
                    url: "https://example.com/seg1.m4s".parse().expect("url"),
                    duration: kithara_platform::time::Duration::from_secs(2),
                    byte_range_len: Some(200),
                },
            ],
        }]))
    }

    fn coord(bus: &EventBus) -> Arc<TestHlsCoord> {
        let ctx = test_ctx(bus);
        let playlist = playlist_state();
        let segments = vec![
            Segment::Media(MediaSegment {
                url: "https://example.com/seg0.m4s".parse().expect("url"),
                resource_id: ctx
                    .scope
                    .key(&AssetResource::Url(
                        "https://example.com/seg0.m4s".parse().expect("url"),
                    ))
                    .expect("segment key"),
                state: SegmentSlotState::missing(),
                size: SegmentSize::seed(100),
                content: SegmentContent::Plain,
                decode_time: kithara_platform::time::Duration::ZERO,
                duration: kithara_platform::time::Duration::from_secs(2),
            }),
            Segment::Media(MediaSegment {
                url: "https://example.com/seg1.m4s".parse().expect("url"),
                resource_id: ctx
                    .scope
                    .key(&AssetResource::Url(
                        "https://example.com/seg1.m4s".parse().expect("url"),
                    ))
                    .expect("segment key"),
                state: SegmentSlotState::missing(),
                size: SegmentSize::seed(200),
                content: SegmentContent::Plain,
                decode_time: kithara_platform::time::Duration::from_secs(2),
                duration: kithara_platform::time::Duration::from_secs(2),
            }),
        ];
        let variant = VariantParts {
            segments,
            init: None,
            codec: playlist.variant_codec(0),
            container: playlist.variant_container(0),
        }
        .into_variant(0, &ctx);
        let state = Arc::new(AbrState::new(AbrMode::Auto(Some(VariantIndex::new(0)))));
        let publisher = state.publisher();
        let cancel = CancelToken::never();
        let peer: Arc<dyn Abr> = Arc::new(Unimock::new((
            AbrMock::cancel
                .each_call(matching!())
                .returns(cancel.clone()),
            AbrMock::state.each_call(matching!()).returns(Some(state)),
        )));
        let settings = AbrSettings::builder().cancel(cancel.clone()).build();
        let controller = AbrController::new(settings);
        let handle = controller.register(&peer);
        Arc::new(HlsCoord::new(
            HlsCoordEnv {
                cancel,
                scope: ctx.scope.clone(),
                config: Arc::clone(&ctx.config),
                look_ahead_segments: ctx.look_ahead_segments,
                emit: Arc::new(DeferredBus::new(bus.clone(), 8)),
                signal: ctx.signal,
            },
            Arc::new(PlayheadState::new()),
            ActivityWriter::new(),
            handle,
            publisher,
            Arc::from(vec![variant]),
        ))
    }

    #[kithara::test]
    fn segment_read_complete_uses_prior_segment_start_cursor() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let coord = coord(&bus);
        coord.set_position(60);
        let mut sink = HlsReaderEventSink::new(Arc::new(DeferredBus::new(bus, 8)), coord.clone());

        sink.on_chunk(ReaderChunkSignal::Chunk);
        coord.set_position(100);
        sink.on_chunk(ReaderChunkSignal::Chunk);
        sink.flush();

        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(HlsEvent::SegmentReadStart {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
            })
        ));
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(HlsEvent::SegmentReadComplete {
                variant: 0,
                segment_index: 0,
                bytes_read: 100,
            })
        ));
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(HlsEvent::SegmentReadStart {
                variant: 0,
                segment_index: 1,
                byte_offset: 100,
            })
        ));
    }
}
