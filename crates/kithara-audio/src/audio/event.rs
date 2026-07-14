use kithara_decode::{DecodeError, DecoderResamplerConfig, PcmMeta, PcmSpec};
use kithara_events::{
    AudioCodecKind, AudioEvent, DecodeErrorClass, DecodeErrorKind,
    DecoderBackend as EventDecoderBackend, DecoderChangeCause, DecoderEvent, DeferredBus, Event,
    EventBus, FrameDomain, GaplessSpan, PlaybackResamplerKind, ResamplerKind, SeekLifecycleStage,
    SegmentLocation,
};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_resampler::ResamplerBackend;
use kithara_stream::{MediaInfo, PlayheadWrite, SeekObserve};

use super::{ReadOutcome, ThreadWake, WakeSignal};

struct Consts;

impl Consts {
    const AUDIO_EVENT_CAPACITY: usize = 64;
    const PROGRESS_EMIT_MIN_DELTA_MS: u64 = 100;
}

pub(super) struct AudioEvents {
    emit: Arc<DeferredBus<Event>>,
    last_progress_emit: Option<(u64, u64)>,
    underrun_active: bool,
}

impl AudioEvents {
    pub(super) fn new(emit: Arc<DeferredBus<Event>>) -> Self {
        Self {
            emit,
            last_progress_emit: None,
            underrun_active: false,
        }
    }

    pub(super) fn bus(&self) -> &EventBus {
        self.emit.bus()
    }

    pub(super) fn deferred(bus: &EventBus) -> Arc<DeferredBus<Event>> {
        Arc::new(DeferredBus::new(bus.clone(), Consts::AUDIO_EVENT_CAPACITY))
    }

    pub(super) fn publish(&self, event: AudioEvent) {
        self.emit.bus().publish(event);
    }

    fn emit(&self, event: AudioEvent) {
        self.emit.enqueue(event.into());
    }

    pub(super) fn progress(&mut self, playhead: &dyn PlayheadWrite, epoch: u64) {
        let position_ms = clamp_millis(playhead.position());
        if let Some((last_epoch, last_ms)) = self.last_progress_emit
            && last_epoch == epoch
            && position_ms.abs_diff(last_ms) < Consts::PROGRESS_EMIT_MIN_DELTA_MS
        {
            return;
        }
        self.last_progress_emit = Some((epoch, position_ms));

        let total_ms = playhead.duration().map(clamp_millis);
        let decoded_ms = clamp_millis(playhead.decoded_frontier());
        let buffered_ms = Some(total_ms.map_or(decoded_ms, |total| decoded_ms.min(total)));
        self.emit(AudioEvent::PlaybackProgress {
            position_ms,
            total_ms,
            buffered_ms,
            seek_epoch: epoch,
        });
    }

    pub(super) fn post_seek_output(
        &self,
        seek: &dyn SeekObserve,
        epoch: u64,
        meta: Option<PcmMeta>,
        position: Duration,
    ) {
        let Some(seek_epoch) = seek.pending_epoch() else {
            return;
        };
        if seek_epoch != epoch {
            return;
        }

        let variant = meta.as_ref().and_then(|value| value.variant_index);
        let segment_index = meta.as_ref().and_then(|value| value.segment_index);
        self.emit(AudioEvent::SeekLifecycle {
            seek_epoch,
            stage: SeekLifecycleStage::OutputCommitted,
            location: SegmentLocation::new(variant, segment_index, None, None),
        });
        self.emit(AudioEvent::SeekComplete {
            seek_epoch,
            position,
        });
        let _ = seek.clear_pending_epoch(seek_epoch);
    }

    pub(super) fn commit_read(
        &mut self,
        session: &super::core::Session,
        epoch: u64,
        read: super::cursor::CursorRead,
    ) -> ReadOutcome {
        let super::cursor::CursorRead {
            outcome,
            last_output_meta,
        } = read;
        if matches!(outcome, ReadOutcome::Frames { .. }) {
            let position = session.playhead.position();
            self.post_seek_output(session.seek_obs.as_ref(), epoch, last_output_meta, position);
            self.progress(session.playhead.as_ref(), epoch);
        }
        outcome
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
                self.emit(AudioEvent::UnderrunEnded {
                    position_ms: clamp_millis(position),
                    seek_epoch: epoch,
                });
            }
        } else if was_playing && !self.underrun_active {
            self.underrun_active = true;
            self.emit(AudioEvent::UnderrunStarted {
                position_ms: clamp_millis(position),
                seek_epoch: epoch,
            });
        }
    }

    pub(super) fn reset_underrun(&mut self) {
        self.underrun_active = false;
    }

    #[cfg(test)]
    pub(super) fn test() -> Self {
        let bus = EventBus::new(16);
        let emit = Self::deferred(&bus);
        Self::new(emit)
    }
}

pub(super) struct ReaderOutputWake {
    thread: Arc<ThreadWake>,
    emit: Arc<DeferredBus<Event>>,
}

impl ReaderOutputWake {
    pub(super) fn new(thread: Arc<ThreadWake>, emit: Arc<DeferredBus<Event>>) -> Self {
        Self { thread, emit }
    }
}

impl WakeSignal for ReaderOutputWake {
    fn wake(&self) {
        WakeSignal::wake(self.thread.as_ref());
    }

    fn on_data_available(&self) {
        self.emit.enqueue(AudioEvent::OutputAvailable.into());
    }

    fn flush_deferred(&self) {
        self.emit.flush();
    }
}

fn clamp_millis(duration: Duration) -> u64 {
    num_traits::cast::ToPrimitive::to_u64(&duration.as_millis()).unwrap_or(u64::MAX)
}

pub(crate) fn map_audio_codec_kind(codec: kithara_stream::AudioCodec) -> AudioCodecKind {
    match codec {
        kithara_stream::AudioCodec::AacLc => AudioCodecKind::AacLc,
        kithara_stream::AudioCodec::AacHe => AudioCodecKind::AacHe,
        kithara_stream::AudioCodec::AacHeV2 => AudioCodecKind::AacHeV2,
        kithara_stream::AudioCodec::Mp3 => AudioCodecKind::Mp3,
        kithara_stream::AudioCodec::Flac => AudioCodecKind::Flac,
        kithara_stream::AudioCodec::Vorbis => AudioCodecKind::Vorbis,
        kithara_stream::AudioCodec::Opus => AudioCodecKind::Opus,
        kithara_stream::AudioCodec::Alac => AudioCodecKind::Alac,
        kithara_stream::AudioCodec::Pcm => AudioCodecKind::Pcm,
        kithara_stream::AudioCodec::Adpcm => AudioCodecKind::Adpcm,
    }
}

pub(crate) fn map_container_kind(
    container: kithara_stream::ContainerFormat,
) -> kithara_events::ContainerKind {
    match container {
        kithara_stream::ContainerFormat::Mp4 => kithara_events::ContainerKind::Mp4,
        kithara_stream::ContainerFormat::Fmp4 => kithara_events::ContainerKind::Fmp4,
        kithara_stream::ContainerFormat::MpegTs => kithara_events::ContainerKind::MpegTs,
        kithara_stream::ContainerFormat::MpegAudio => kithara_events::ContainerKind::MpegAudio,
        kithara_stream::ContainerFormat::Adts => kithara_events::ContainerKind::Adts,
        kithara_stream::ContainerFormat::Flac => kithara_events::ContainerKind::Flac,
        kithara_stream::ContainerFormat::Wav => kithara_events::ContainerKind::Wav,
        kithara_stream::ContainerFormat::Ogg => kithara_events::ContainerKind::Ogg,
        kithara_stream::ContainerFormat::Caf => kithara_events::ContainerKind::Caf,
        kithara_stream::ContainerFormat::Mkv => kithara_events::ContainerKind::Mkv,
    }
}

pub(crate) fn map_decoder_backend(backend: kithara_decode::DecoderBackend) -> EventDecoderBackend {
    match backend {
        #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
        kithara_decode::DecoderBackend::Apple => EventDecoderBackend::Apple,
        #[cfg(all(feature = "android", target_os = "android"))]
        kithara_decode::DecoderBackend::Android => EventDecoderBackend::Android,
        #[cfg(feature = "symphonia")]
        kithara_decode::DecoderBackend::Symphonia => EventDecoderBackend::Symphonia,
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

pub(crate) fn map_decode_error_kind(error: &DecodeError) -> DecodeErrorKind {
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

pub(crate) fn map_decode_error_class(class: kithara_decode::ErrorClass) -> DecodeErrorClass {
    match class {
        kithara_decode::ErrorClass::Interrupted => DecodeErrorClass::Interrupted,
        kithara_decode::ErrorClass::VariantChange => DecodeErrorClass::VariantChange,
        _ => DecodeErrorClass::Other,
    }
}

pub(crate) fn decode_error_detail(error: &DecodeError) -> &'static str {
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
    pub(crate) backend: kithara_decode::DecoderBackend,
    pub(crate) media_info: Option<&'a MediaInfo>,
    pub(crate) spec: PcmSpec,
    pub(crate) track_info: &'a kithara_decode::DecoderTrackInfo,
    pub(crate) epoch: u64,
    pub(crate) cause: DecoderChangeCause,
    pub(crate) base_offset: u64,
    pub(crate) duration: Option<Duration>,
}

pub(crate) fn decoder_changed_event(data: DecoderChangedEventData<'_>) -> DecoderEvent {
    DecoderEvent::DecoderChanged {
        backend: map_decoder_backend(data.backend),
        codec: data
            .media_info
            .and_then(|info| info.codec)
            .map(map_audio_codec_kind),
        container: data
            .media_info
            .and_then(|info| info.container)
            .map(map_container_kind),
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
    spec: PcmSpec,
    track_info: &kithara_decode::DecoderTrackInfo,
    domain: FrameDomain,
) -> Option<DecoderEvent> {
    let gapless = track_info.gapless?;
    Some(DecoderEvent::GaplessResolved {
        leading_frames: gapless.leading_frames,
        trailing_frames: gapless.trailing_frames,
        domain,
        codec: media_info
            .and_then(|info| info.codec)
            .map(map_audio_codec_kind),
        sample_rate: spec.sample_rate.get(),
    })
}

pub(crate) fn decoder_resampler_event<B>(
    resampler: Option<&DecoderResamplerConfig<B>>,
    spec: PcmSpec,
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
    use kithara_bufpool::PcmPool;
    use kithara_decode::{PcmChunk, PcmMeta};
    use kithara_events::{AudioEvent, Event, EventBus};
    use kithara_platform::sync::Arc;
    use kithara_test_utils::kithara;

    use super::*;
    use crate::audio::{Fetch, ring::create_channels};

    fn empty_chunk() -> PcmChunk {
        PcmChunk::new(PcmMeta::default(), PcmPool::default().attach(Vec::new()))
    }

    #[kithara::test]
    fn output_available_event_fires_on_empty_to_nonempty_ring_transition() {
        let bus = EventBus::new(8);
        let mut events = bus.subscribe();
        let reader_wake = Arc::new(ThreadWake::default());
        let emit = AudioEvents::deferred(&bus);
        let (mut tx, mut rx) = create_channels(2, emit, &reader_wake);

        tx.try_push(Fetch::data(empty_chunk(), 0))
            .expect("first push reaches ring");
        assert!(events.try_recv().is_err());
        tx.flush_wake_signals();
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(Event::Audio(AudioEvent::OutputAvailable))
        ));

        tx.try_push(Fetch::data(empty_chunk(), 0))
            .expect("second push reaches ring");
        tx.flush_wake_signals();
        assert!(events.try_recv().is_err());

        assert!(rx.try_pop().is_some());
        assert!(rx.try_pop().is_some());

        tx.try_push(Fetch::data(empty_chunk(), 0))
            .expect("third push reaches empty ring");
        tx.flush_wake_signals();
        assert!(matches!(
            events.try_recv().map(|envelope| envelope.event),
            Ok(Event::Audio(AudioEvent::OutputAvailable))
        ));
    }

    #[kithara::test]
    fn underrun_edges_emit_once_per_starvation_window() {
        let bus = EventBus::new(8);
        let mut receiver = bus.subscribe();
        let emit = AudioEvents::deferred(&bus);
        let mut events = AudioEvents::new(Arc::clone(&emit));
        let position = Duration::from_millis(321);

        events.fill_result(false, true, false, position, 0);
        events.fill_result(false, true, false, position, 0);
        emit.flush();

        assert!(matches!(
            receiver.try_recv().map(|envelope| envelope.event),
            Ok(Event::Audio(AudioEvent::UnderrunStarted {
                position_ms: 321,
                seek_epoch: 0,
            }))
        ));
        assert!(receiver.try_recv().is_err());

        events.fill_result(true, true, false, position, 0);
        emit.flush();
        assert!(matches!(
            receiver.try_recv().map(|envelope| envelope.event),
            Ok(Event::Audio(AudioEvent::UnderrunEnded {
                position_ms: 321,
                seek_epoch: 0,
            }))
        ));
    }
}
