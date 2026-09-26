use std::sync::atomic::{AtomicBool, Ordering};

use kithara_decode::{
    DecodeError, DecoderBackend as DecodeBackend, DecoderResamplerConfig, ErrorClass,
};
use kithara_events::{DeferredBus, EventBus};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_resampler::ResamplerBackend;
use kithara_signal::{AudioChunkInfo, AudioSpec};
use kithara_stream::{MediaInfo, PlayheadWrite, SeekObserve};
use kithara_test_utils::kithara;
use num_traits::cast::ToPrimitive;

use super::{AudioLaneEvent, ReadOutcome, ThreadWake, WakeSignal};
use crate::{
    AudioEvent, ConsumerWakeMode, DecodeErrorClass, DecodeErrorKind,
    DecoderBackend as EventDecoderBackend, DecoderChangeCause, DecoderEvent, FrameDomain,
    GaplessSpan, PlaybackResamplerKind, ResamplerKind, SeekLifecycleStage, SegmentLocation, consts,
};

/// Reader-side event sink.
///
/// A `RealtimeDeferred` consumer reads on the audio callback, so its events
/// go through the lock-free [`DeferredBus`] ring and reach the bus when the
/// scheduler shell flushes. An `ImmediateOffRt` consumer runs off the
/// real-time thread and may take the `broadcast::send` lock, so its events
/// publish inline: everything a read births is on the bus when that read
/// returns, and the deferred ring keeps the shell as its only flusher.
pub(super) struct AudioEvents {
    emit: Arc<DeferredBus<AudioLaneEvent>>,
    wake_mode: ConsumerWakeMode,
    last_progress_emit: Option<(u64, u64)>,
    underrun_active: bool,
    wake_pending: bool,
}

impl AudioEvents {
    pub(super) const fn new(
        emit: Arc<DeferredBus<AudioLaneEvent>>,
        wake_mode: ConsumerWakeMode,
    ) -> Self {
        Self {
            emit,
            wake_mode,
            wake_pending: false,
            last_progress_emit: None,
            underrun_active: false,
        }
    }

    pub(super) fn commit_read(
        &mut self,
        session: &super::core::Session,
        epoch: u64,
        read: super::cursor::CursorRead,
    ) -> ReadOutcome {
        let super::cursor::CursorRead {
            outcome,
            first_output_meta,
        } = read;
        if matches!(outcome, ReadOutcome::Frames { .. }) {
            debug_assert!(first_output_meta.is_some());
            let position = session.playhead.position();
            self.fill_result(true, false, false, position, epoch);
            self.post_seek_output(session.seek_obs.as_ref(), epoch, first_output_meta);
            self.progress(session.playhead.as_ref(), epoch);
        }
        outcome
    }

    pub(super) fn deferred(bus: &EventBus) -> Arc<DeferredBus<AudioLaneEvent>> {
        Arc::new(DeferredBus::new(bus.clone(), consts::AUDIO_EVENT_CAPACITY))
    }

    pub(super) fn fill_result(
        &mut self,
        filled: bool,
        was_playing: bool,
        terminal: bool,
        position: Duration,
        epoch: u64,
    ) {
        if terminal {
            self.underrun_active = false;
            return;
        }
        if filled {
            if self.underrun_active {
                self.underrun_active = false;
                self.publish(AudioEvent::UnderrunEnded {
                    position_ms: clamp_millis(position),
                    seek_epoch: epoch,
                });
            }
        } else if was_playing && !self.underrun_active {
            self.underrun_active = true;
            self.publish(AudioEvent::UnderrunStarted {
                position_ms: clamp_millis(position),
                seek_epoch: epoch,
            });
        }
    }

    #[kithara::probe(epoch, pending = seek.pending_epoch().unwrap_or(0))]
    pub(super) fn post_seek_output(
        &mut self,
        seek: &dyn SeekObserve,
        epoch: u64,
        meta: Option<AudioChunkInfo>,
    ) {
        let Some(seek_epoch) = seek.pending_epoch() else {
            return;
        };
        if seek_epoch != epoch {
            return;
        }
        let Some(meta) = meta else {
            return;
        };

        let variant = meta.variant_index;
        let segment_index = meta.segment_index;
        self.publish(AudioEvent::SeekLifecycle {
            seek_epoch,
            stage: SeekLifecycleStage::OutputCommitted,
            location: SegmentLocation::new(variant, segment_index, None, None),
        });
        self.publish(AudioEvent::SeekComplete {
            seek_epoch,
            position: meta.timestamp,
        });
        let _ = seek.clear_pending_epoch(seek_epoch);
    }

    pub(super) fn progress(&mut self, playhead: &dyn PlayheadWrite, epoch: u64) {
        let position_ms = clamp_millis(playhead.position());
        if let Some((last_epoch, last_ms)) = self.last_progress_emit
            && last_epoch == epoch
            && position_ms.abs_diff(last_ms) < consts::PROGRESS_EMIT_MIN_DELTA_MS
        {
            return;
        }
        self.last_progress_emit = Some((epoch, position_ms));

        let total_ms = playhead.duration().map(clamp_millis);
        let decoded_ms = clamp_millis(playhead.decoded_frontier());
        let buffered_ms = Some(total_ms.map_or(decoded_ms, |total| decoded_ms.min(total)));
        self.publish(AudioEvent::PlaybackProgress {
            position_ms,
            total_ms,
            buffered_ms,
            seek_epoch: epoch,
        });
    }

    pub(super) fn publish(&mut self, event: AudioEvent) {
        match self.wake_mode {
            ConsumerWakeMode::RealtimeDeferred => {
                self.emit.enqueue(event);
                self.wake_pending = true;
            }
            ConsumerWakeMode::ImmediateOffRt => self.emit.bus().publish(event),
        }
    }

    pub(super) const fn reset_underrun(&mut self) {
        self.underrun_active = false;
    }

    pub(super) const fn set_wake_mode(&mut self, wake_mode: ConsumerWakeMode) {
        self.wake_mode = wake_mode;
    }

    pub(super) const fn take_wake_pending(&mut self) -> bool {
        let pending = self.wake_pending;
        self.wake_pending = false;
        pending
    }

    #[cfg(test)]
    pub(super) fn test() -> Self {
        Self::new(
            Self::deferred(&EventBus::new(16)),
            ConsumerWakeMode::RealtimeDeferred,
        )
    }

    delegate::delegate! {
        to self.emit {
            pub(super) fn bus(&self) -> &EventBus;
        }
    }
}

pub(super) struct ReaderOutputWake {
    emit: Arc<DeferredBus<AudioLaneEvent>>,
    thread: Arc<ThreadWake>,
    pending: AtomicBool,
}

impl ReaderOutputWake {
    pub(super) fn new(thread: Arc<ThreadWake>, emit: Arc<DeferredBus<AudioLaneEvent>>) -> Self {
        Self {
            emit,
            thread,
            pending: AtomicBool::new(false),
        }
    }
}

impl WakeSignal for ReaderOutputWake {
    /// A pre-push emptiness snapshot can race a consumer that is draining the ring.
    fn flush_deferred(&self) {
        self.emit.flush();
        if self.pending.swap(false, Ordering::AcqRel) {
            WakeSignal::wake(self.thread.as_ref());
            self.emit.bus().publish(AudioEvent::OutputAvailable);
        }
    }

    fn wake(&self) {
        self.pending.store(true, Ordering::Release);
    }
}

fn clamp_millis(duration: Duration) -> u64 {
    ToPrimitive::to_u64(&duration.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) const fn map_decoder_backend(backend: DecodeBackend) -> EventDecoderBackend {
    match backend {
        #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
        kithara_decode::DecoderBackend::Apple => EventDecoderBackend::Apple,
        #[cfg(all(feature = "android", target_os = "android"))]
        kithara_decode::DecoderBackend::Android => EventDecoderBackend::Android,
        #[cfg(feature = "symphonia")]
        DecodeBackend::Symphonia => EventDecoderBackend::Symphonia,
        _ => EventDecoderBackend::Symphonia,
    }
}

pub(crate) fn map_resampler_kind(name: &'static str) -> ResamplerKind {
    match name {
        "rubato" => ResamplerKind::Rubato,
        "apple" => ResamplerKind::Apple,
        "glide" => ResamplerKind::Glide,
        _ => ResamplerKind::None,
    }
}

pub(crate) fn map_playback_resampler_kind(name: &'static str) -> PlaybackResamplerKind {
    match name {
        "rubato" => PlaybackResamplerKind::Rubato,
        "glide" => PlaybackResamplerKind::Glide,
        _ => PlaybackResamplerKind::None,
    }
}

/// Reduce a decoder error to the `Copy` code that can travel on the audio thread.
///
/// `DecodeError` owns strings and a boxed source, so it cannot cross a render
/// callback that must not allocate or drop. This crate owns `DecodeErrorKind`
/// and `kithara-decode` owns `DecodeError`, so neither can carry an inherent
/// conversion: the mapping is published here, next to the kind it produces.
#[must_use]
pub const fn map_decode_error_kind(error: &DecodeError) -> DecodeErrorKind {
    match error {
        DecodeError::Io { .. } => DecodeErrorKind::Io,
        DecodeError::UnsupportedCodec { .. } => DecodeErrorKind::UnsupportedCodec,
        DecodeError::UnsupportedContainer { .. } => DecodeErrorKind::UnsupportedContainer,
        DecodeError::InvalidData { .. } => DecodeErrorKind::InvalidData,
        DecodeError::SeekFailed { .. } => DecodeErrorKind::SeekFailed,
        DecodeError::SeekOutOfRange { .. } => DecodeErrorKind::SeekOutOfRange,
        DecodeError::Parse { .. } => DecodeErrorKind::Parse,
        DecodeError::ProbeFailed => DecodeErrorKind::ProbeFailed,
        DecodeError::BackendUnavailable { .. } => DecodeErrorKind::BackendUnavailable,
        DecodeError::InvalidSampleRate { .. } => DecodeErrorKind::InvalidSampleRate,
        DecodeError::BackendStatus { .. } => DecodeErrorKind::BackendStatus,
        DecodeError::Interrupted => DecodeErrorKind::Interrupted,
        _ => DecodeErrorKind::Backend,
    }
}

pub(crate) const fn map_decode_error_class(class: ErrorClass) -> DecodeErrorClass {
    match class {
        ErrorClass::Interrupted => DecodeErrorClass::Interrupted,
        ErrorClass::VariantChange => DecodeErrorClass::VariantChange,
        _ => DecodeErrorClass::Other,
    }
}

pub(crate) const fn decode_error_detail(error: &DecodeError) -> &'static str {
    match error {
        DecodeError::Io { .. } => "io",
        DecodeError::UnsupportedCodec { .. } => "unsupported codec",
        DecodeError::UnsupportedContainer { .. } => "unsupported container",
        DecodeError::InvalidData { detail }
        | DecodeError::SeekFailed { detail }
        | DecodeError::SeekOutOfRange { detail } => detail,
        DecodeError::Parse { what, .. } => what,
        DecodeError::ProbeFailed => "probe failed",
        DecodeError::BackendUnavailable { backend } => backend,
        DecodeError::InvalidSampleRate { resource } => resource,
        DecodeError::BackendStatus { op, .. } => op,
        DecodeError::Interrupted => "interrupted",
        _ => "other",
    }
}

fn gapless_span(track_info: &kithara_decode::DecoderTrackInfo) -> Option<GaplessSpan> {
    track_info
        .gapless
        .map(|gapless| GaplessSpan::new(gapless.leading_frames, gapless.trailing_frames))
}

#[derive(Clone, Copy)]
pub(crate) struct DecoderChangedEventData<'a> {
    pub(crate) track_info: &'a kithara_decode::DecoderTrackInfo,
    pub(crate) spec: AudioSpec,
    pub(crate) backend: DecodeBackend,
    pub(crate) cause: DecoderChangeCause,
    pub(crate) duration: Option<Duration>,
    pub(crate) media_info: Option<&'a MediaInfo>,
    pub(crate) base_offset: u64,
    pub(crate) epoch: u64,
}

pub(crate) fn decoder_changed_event(data: DecoderChangedEventData<'_>) -> DecoderEvent {
    DecoderEvent::DecoderChanged {
        backend: map_decoder_backend(data.backend),
        codec: data.media_info.and_then(|info| info.codec),
        container: data.media_info.and_then(|info| info.container),
        sample_rate: data.spec.sample_rate.get(),
        channels: data.spec.channels,
        bit_depth: None,
        bitrate: None,
        epoch: data.epoch,
        cause: data.cause,
        variant: data.media_info.and_then(|info| info.variant_index),
        base_offset: data.base_offset,
        duration: data.duration,
        gapless: gapless_span(data.track_info),
    }
}

pub(crate) fn decoder_gapless_event(
    media_info: Option<&MediaInfo>,
    spec: AudioSpec,
    track_info: &kithara_decode::DecoderTrackInfo,
    domain: FrameDomain,
) -> Option<DecoderEvent> {
    let gapless = track_info.gapless?;
    Some(DecoderEvent::GaplessResolved {
        domain,
        leading_frames: gapless.leading_frames,
        trailing_frames: gapless.trailing_frames,
        codec: media_info.and_then(|info| info.codec),
        sample_rate: spec.sample_rate.get(),
    })
}

pub(crate) fn decoder_resampler_event<B>(
    resampler: Option<&DecoderResamplerConfig<B>>,
    spec: AudioSpec,
    input_rate: Option<u32>,
) -> Option<DecoderEvent>
where
    B: ResamplerBackend,
{
    let resampler = resampler?;
    let input_rate = input_rate.unwrap_or_else(|| spec.sample_rate.get());
    Some(DecoderEvent::ResamplerConfigured {
        backend: map_resampler_kind(resampler.backend.name()),
        input_rate,
        output_rate: resampler.target_sample_rate.get(),
        channels: spec.channels,
        bypassed: input_rate == resampler.target_sample_rate.get(),
    })
}

pub(crate) fn playback_resampler_event<B>(
    backend: &B,
    host_sample_rate: u32,
    source_sample_rate: Option<u32>,
) -> Option<AudioEvent>
where
    B: ResamplerBackend,
{
    let source_sample_rate = source_sample_rate?;
    Some(AudioEvent::PlaybackResamplerConfigured {
        backend: map_playback_resampler_kind(backend.name()),
        host_sample_rate,
        source_sample_rate,
        active: host_sample_rate != source_sample_rate && backend.name() != "none",
    })
}

#[cfg(test)]
mod tests {
    use kithara_events::EventBus;
    use kithara_platform::sync::Arc;
    use kithara_signal::{AudioChunk, AudioChunkInfo};
    use kithara_stream::{SeekControl, SeekState};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        AudioEvent,
        audio::{Fetch, ring::create_channels},
        test_pools::{Pools, pools, sample_buffer},
    };

    fn empty_chunk(pools: &Pools) -> AudioChunk {
        AudioChunk::new(AudioChunkInfo::default(), sample_buffer(pools, &[]))
    }

    #[kithara::test]
    fn post_seek_output_reaches_the_bus_once_the_shell_flushes() {
        let bus = EventBus::new(8);
        let mut receiver = bus.subscribe();
        let emit = AudioEvents::deferred(&bus);
        let mut events = AudioEvents::new(Arc::clone(&emit), ConsumerWakeMode::RealtimeDeferred);
        let seek = SeekState::new();
        let target = Duration::from_millis(500);
        let epoch = seek.begin(target);
        seek.mark_pending(epoch);

        events.post_seek_output(
            &seek,
            epoch,
            Some(AudioChunkInfo {
                timestamp: target,
                end_timestamp: Duration::from_millis(521),
                ..Default::default()
            }),
        );
        assert!(events.take_wake_pending());
        assert!(!events.take_wake_pending());

        assert!(
            receiver.try_recv().is_err(),
            "a read runs on the audio callback, so its events wait for the shell"
        );
        emit.flush();

        assert!(matches!(
            receiver.try_recv().map(|envelope| envelope.event),
            Ok(AudioEvent::SeekLifecycle {
                seek_epoch,
                stage: SeekLifecycleStage::OutputCommitted,
                ..
            }) if seek_epoch == epoch
        ));
        assert!(matches!(
            receiver.try_recv().map(|envelope| envelope.event),
            Ok(AudioEvent::SeekComplete {
                seek_epoch,
                position,
            }) if seek_epoch == epoch && position == target
        ));
        assert_eq!(seek.pending_epoch(), None);
    }

    #[kithara::test]
    fn output_wake_is_deferred_until_the_scheduler_shell_flushes() {
        let bus = EventBus::new(8);
        let thread = Arc::new(ThreadWake::default());
        let since = thread.current();
        let wake = ReaderOutputWake::new(Arc::clone(&thread), AudioEvents::deferred(&bus));

        WakeSignal::wake(&wake);

        assert_eq!(thread.current(), since);
        assert!(wake.pending.load(Ordering::Acquire));
        wake.flush_deferred();
        let delivered = thread.current();
        assert_ne!(delivered, since);
        assert!(!wake.pending.load(Ordering::Acquire));
        wake.flush_deferred();
        assert_eq!(thread.current(), delivered);
    }

    #[kithara::test]
    fn output_available_event_is_coalesced_per_producer_pass() {
        let pools = pools();
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let reader_wake = Arc::new(ThreadWake::default());
        let emit = AudioEvents::deferred(&bus);
        let (mut tx, mut rx) = create_channels(2, emit, &reader_wake);

        tx.try_push(Fetch::data(empty_chunk(&pools), 0))
            .expect("first push reaches ring");
        assert!(events.try_recv().is_err());
        tx.flush_wake_signals();
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(AudioEvent::OutputAvailable)
        ));

        tx.try_push(Fetch::data(empty_chunk(&pools), 0))
            .expect("second push reaches ring");
        assert!(events.try_recv().is_err());
        tx.flush_wake_signals();
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(AudioEvent::OutputAvailable)
        ));
        tx.flush_wake_signals();
        assert!(events.try_recv().is_err());

        assert!(rx.try_pop().is_some());
        assert!(rx.try_pop().is_some());

        tx.try_push(Fetch::data(empty_chunk(&pools), 0))
            .expect("third push reaches empty ring");
        tx.try_push(Fetch::data(empty_chunk(&pools), 0))
            .expect("fourth push shares the producer pass");
        tx.flush_wake_signals();
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(AudioEvent::OutputAvailable)
        ));
        assert!(events.try_recv().is_err(), "one event per producer pass");
    }

    #[kithara::test]
    fn reader_output_wake_is_deferred_and_coalesced() {
        let bus = EventBus::new(8);
        let thread = Arc::new(ThreadWake::default());
        let wake = ReaderOutputWake::new(Arc::clone(&thread), AudioEvents::deferred(&bus));
        let gate_epoch = thread.current();

        wake.wake();
        wake.wake();

        assert_eq!(
            thread.current(),
            gate_epoch,
            "producer-core wakes must not signal the blocking reader directly"
        );

        wake.flush_deferred();
        assert_eq!(thread.current(), gate_epoch + 1);

        wake.flush_deferred();
        assert_eq!(
            thread.current(),
            gate_epoch + 1,
            "a second flush without another wake request must be a no-op"
        );
    }

    #[kithara::test]
    fn underrun_edges_emit_once_per_starvation_window() {
        let bus = EventBus::new(8);
        let mut receiver = bus.subscribe();
        let emit = AudioEvents::deferred(&bus);
        let mut events = AudioEvents::new(Arc::clone(&emit), ConsumerWakeMode::RealtimeDeferred);
        let position = Duration::from_millis(321);

        events.fill_result(false, true, false, position, 0);
        events.fill_result(false, true, false, position, 0);
        emit.flush();

        assert!(matches!(
            receiver.try_recv().map(|envelope| envelope.event),
            Ok(AudioEvent::UnderrunStarted {
                position_ms: 321,
                seek_epoch: 0,
            })
        ));
        assert!(receiver.try_recv().is_err());

        events.fill_result(true, true, false, position, 0);
        emit.flush();
        assert!(matches!(
            receiver.try_recv().map(|envelope| envelope.event),
            Ok(AudioEvent::UnderrunEnded {
                position_ms: 321,
                seek_epoch: 0,
            })
        ));
    }
}
