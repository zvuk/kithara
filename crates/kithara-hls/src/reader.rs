#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use kithara_events::{DeferredBus, HlsEvent};
use kithara_platform::sync::Arc;
use kithara_stream::{PrerollHint, ReaderChunkSignal, ReaderEventSink, ReaderSeekSignal};

use crate::stream::HlsCoord;

/// Decoder→HLS reader hook bridge: turns the decoder's per-chunk and
/// per-seek signals into [`HlsEvent`]s on the track's [`EventBus`].
///
/// Mirrors `kithara-file`'s `FileReaderEventSink` but resolves the
/// landed byte to its `(variant, segment_index, byte_in_segment)` triple
/// via the variant-aware [`HlsCoord::find_at_offset`] — this is what
/// makes the integration tests' `HlsEvent::ReaderSeek { segment_index,
/// .. }` assertion observable.
pub(crate) struct HlsReaderEventSink {
    coord: Arc<HlsCoord>,
    seek_epoch_handle: Arc<AtomicU64>,
    bus: Arc<DeferredBus<HlsEvent>>,
    /// `(variant_index, segment_index)` of the last segment the
    /// reader was observed in. A change between `on_chunk` calls
    /// drives [`HlsEvent::SegmentReadStart`]; the same pair held
    /// across a seek is intentionally treated as a no-op for the
    /// boundary event (the seek itself was already announced).
    last_segment: Option<(usize, usize)>,
    initial_seek_published: bool,
    /// Cursor at hooks-creation time. After a `Seek` failure the
    /// decoder may discard the seek outcome and resume at the
    /// pre-seek cursor; we publish that fallback as a `ReaderSeek`
    /// the first time `on_chunk` fires so the bus subscriber sees a
    /// non-default `seek_epoch` even if `on_seek` never landed.
    initial_cursor: u64,
    last_cursor: u64,
    last_segment_start_cursor: u64,
    last_progress_cursor: u64,
}

impl HlsReaderEventSink {
    pub(crate) fn new(
        bus: Arc<DeferredBus<HlsEvent>>,
        coord: Arc<HlsCoord>,
        seek_epoch_handle: Arc<AtomicU64>,
    ) -> Self {
        let last_cursor = coord.position();
        Self {
            bus,
            coord,
            last_cursor,
            last_segment_start_cursor: last_cursor,
            seek_epoch_handle,
            initial_cursor: last_cursor,
            last_progress_cursor: last_cursor,
            initial_seek_published: false,
            last_segment: None,
        }
    }

    /// Announce a [`HlsEvent::SegmentReadStart`] when the reader's
    /// `cursor` lands in a different `(variant, segment_index)` than
    /// the previously observed one.
    fn maybe_publish_segment_start(&mut self, cursor: u64) {
        let Some((seg_idx, seg_start, _size)) = self.coord.find_at_offset(cursor) else {
            return;
        };
        let variant = self.coord.variant_index();
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

    fn maybe_publish_read_progress(&mut self, cursor: u64) {
        const READ_PROGRESS_MIN_DELTA: u64 = 256 * 1024;
        if cursor.abs_diff(self.last_progress_cursor) < READ_PROGRESS_MIN_DELTA {
            return;
        }
        self.last_progress_cursor = cursor;
        self.bus.enqueue(HlsEvent::ReadProgress {
            position: cursor,
            total: self.coord.len(),
        });
    }

    fn publish_initial_seek(&mut self, cursor: u64) {
        if self.initial_seek_published {
            return;
        }
        self.initial_seek_published = true;
        let seek_epoch = self.seek_epoch_handle.load(Ordering::Acquire);
        if seek_epoch == 0 {
            return;
        }
        self.publish_seek(self.initial_cursor, cursor);
    }

    fn publish_seek(&self, from: u64, to: u64) {
        let (variant, segment_index, byte_in_segment) = match self.coord.find_at_offset(to) {
            Some((seg, seg_start, _size)) => (
                Some(self.coord.variant_index()),
                Some(seg as usize),
                Some(to.saturating_sub(seg_start)),
            ),
            None => (None, None, None),
        };
        let seek_epoch = self.seek_epoch_handle.load(Ordering::Acquire);
        self.bus.enqueue(HlsEvent::ReaderSeek {
            seek_epoch,
            variant,
            segment_index,
            byte_in_segment,
            from_offset: from,
            to_offset: to,
        });
    }
}

impl ReaderEventSink for HlsReaderEventSink {
    fn flush(&mut self) {
        self.bus.flush();
    }

    fn on_chunk(&mut self, signal: ReaderChunkSignal) {
        let cursor = self.coord.position();
        match signal {
            ReaderChunkSignal::Chunk => {
                self.publish_initial_seek(cursor);
                self.maybe_publish_segment_start(cursor);
                self.maybe_publish_read_progress(cursor);
                self.last_cursor = cursor;
            }
            ReaderChunkSignal::Eof => {
                if self.last_segment.take().is_some() {
                    self.bus.enqueue(HlsEvent::EndOfStream);
                }
            }
            _ => {}
        }
    }

    fn on_seek(&mut self, signal: ReaderSeekSignal) {
        self.initial_seek_published = true;
        let ReaderSeekSignal::Landed {
            landed_byte,
            preroll,
        } = signal
        else {
            return;
        };
        if let PrerollHint::Required(byte) = preroll {
            // TODO: route preroll byte to the coordinator once preroll plumbing lands.
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
    use std::sync::{OnceLock, atomic::AtomicU64};

    use kithara_abr::{Abr, AbrController, AbrSettings, AbrState};
    use kithara_assets::{AssetStoreBuilder, StorageBackend};
    use kithara_events::{Event, EventBus};
    use kithara_platform::{
        CancelToken,
        sync::{Arc, ThreadGate},
    };
    use kithara_stream::{AudioCodec, ContainerFormat, PlayheadState, SeekState};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        config::SizeProbeMethod,
        playlist::{PlaylistState, SegmentState, VariantState},
        segment::{MediaSegment, Segment, SegmentContent, SegmentSize, SegmentSlotState},
        signal::SizeSignal,
        stream::HlsCoordEnv,
        variant::{PlanCtx, VariantParts},
    };

    struct TestAbrPeer {
        state: Arc<AbrState>,
    }

    impl Abr for TestAbrPeer {
        fn state(&self) -> Option<Arc<AbrState>> {
            Some(Arc::clone(&self.state))
        }
    }

    fn test_ctx(bus: &EventBus) -> PlanCtx {
        let cancel = CancelToken::never();
        let store = Arc::new(
            AssetStoreBuilder::default()
                .backend(StorageBackend::Memory)
                .cancel(cancel.clone())
                .build(),
        );
        PlanCtx {
            bus: bus.clone(),
            prefetch_budget: 1,
            master_cancel: cancel,
            scope: store.scope("reader-test"),
            seek_epoch: 0,
            look_ahead_bytes: None,
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
            segments: vec![
                SegmentState {
                    url: "https://example.com/seg0.m4s".parse().expect("url"),
                    duration: kithara_platform::time::Duration::from_secs(2),
                    byte_range_len: Some(100),
                    index: crate::ids::SegmentIndex::try_new(0, 2).expect("idx"),
                },
                SegmentState {
                    url: "https://example.com/seg1.m4s".parse().expect("url"),
                    duration: kithara_platform::time::Duration::from_secs(2),
                    byte_range_len: Some(200),
                    index: crate::ids::SegmentIndex::try_new(1, 2).expect("idx"),
                },
            ],
        }]))
    }

    fn coord(bus: &EventBus) -> Arc<HlsCoord> {
        let ctx = test_ctx(bus);
        let playlist = playlist_state();
        let segments = vec![
            Segment::Media(MediaSegment {
                url: "https://example.com/seg0.m4s".parse().expect("url"),
                resource_id: ctx
                    .scope
                    .key_for(&"https://example.com/seg0.m4s".parse().expect("url")),
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
                    .key_for(&"https://example.com/seg1.m4s".parse().expect("url")),
                state: SegmentSlotState::missing(),
                size: SegmentSize::seed(200),
                content: SegmentContent::Plain,
                decode_time: kithara_platform::time::Duration::from_secs(2),
                duration: kithara_platform::time::Duration::from_secs(2),
            }),
        ];
        let variant = VariantParts {
            init: None,
            segments,
            seek_obs: Arc::new(SeekState::new()) as Arc<dyn kithara_stream::SeekObserve>,
            codec: playlist.variant_codec(0),
            container: playlist.variant_container(0),
        }
        .into_variant(0, &ctx);
        let state = Arc::new(AbrState::new(kithara_events::AbrMode::Auto(Some(
            kithara_events::VariantIndex::new(0),
        ))));
        let peer: Arc<dyn Abr> = Arc::new(TestAbrPeer { state });
        let controller = Arc::new(AbrController::new(
            AbrSettings::default(),
            CancelToken::never(),
        ));
        Arc::new(HlsCoord::new(
            HlsCoordEnv {
                scope: ctx.scope.clone(),
                cancel: ctx.master_cancel.clone(),
                headers: None,
                emit: Arc::new(DeferredBus::new(bus.clone(), 8)),
                signal: ctx.signal.clone(),
            },
            Arc::new(PlayheadState::new()),
            Arc::new(SeekState::new()),
            controller.register(&peer),
            Arc::new(vec![variant]),
            playlist,
        ))
    }

    #[kithara::test]
    fn segment_read_complete_uses_prior_segment_start_cursor() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let coord = coord(&bus);
        coord.set_position(60);
        let mut sink = HlsReaderEventSink::new(
            Arc::new(DeferredBus::new(bus, 8)),
            coord.clone(),
            Arc::new(AtomicU64::new(0)),
        );

        sink.on_chunk(ReaderChunkSignal::Chunk);
        coord.set_position(100);
        sink.on_chunk(ReaderChunkSignal::Chunk);
        sink.flush();

        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(Event::Hls(HlsEvent::SegmentReadStart {
                variant: 0,
                segment_index: 0,
                byte_offset: 0,
            }))
        ));
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(Event::Hls(HlsEvent::SegmentReadComplete {
                variant: 0,
                segment_index: 0,
                bytes_read: 100,
            }))
        ));
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(Event::Hls(HlsEvent::SegmentReadStart {
                variant: 0,
                segment_index: 1,
                byte_offset: 100,
            }))
        ));
    }
}
