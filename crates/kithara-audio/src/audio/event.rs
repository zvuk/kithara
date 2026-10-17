use kithara_decode::{
    DecodeError, DecoderBackend as DecodeBackend, DecoderResamplerConfig, ErrorClass,
};
use kithara_platform::time::Duration;
use kithara_resampler::ResamplerBackend;
use kithara_signal::AudioSpec;
use kithara_stream::MediaInfo;

use crate::{
    AudioEvent, AudioReadError, DecodeErrorClass, DecodeErrorKind,
    DecoderBackend as EventDecoderBackend, DecoderChangeCause, DecoderEvent, FailureSource,
    FrameDomain, PlaybackResamplerKind, ResamplerKind, TrackFailureKind,
};

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

impl From<FailureSource> for TrackFailureKind {
    fn from(source: FailureSource) -> Self {
        match source {
            FailureSource::Producer { failure } | FailureSource::ProducerAfterSeek { failure } => {
                failure
            }
            FailureSource::ChannelClosed => Self::ChannelClosed,
        }
    }
}

impl From<&AudioReadError> for TrackFailureKind {
    fn from(error: &AudioReadError) -> Self {
        match error {
            AudioReadError::Decode(error) => Self::Decode {
                kind: map_decode_error_kind(error),
            },
            AudioReadError::Stream { source, .. } => match source {
                FailureSource::Producer { failure }
                | FailureSource::ProducerAfterSeek { failure } => *failure,
                FailureSource::ChannelClosed => Self::ChannelClosed,
            },
        }
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

#[derive(Clone, Copy)]
pub(crate) struct DecoderChangedEventData<'a> {
    pub(crate) track_info: &'a kithara_decode::DecoderTrackInfo,
    pub(crate) spec: AudioSpec,
    pub(crate) backend: DecodeBackend,
    pub(crate) cause: DecoderChangeCause,
    pub(crate) duration: Option<Duration>,
    pub(crate) media_info: Option<&'a MediaInfo>,
    pub(crate) base_offset: u64,
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
        cause: data.cause,
        variant: data.media_info.and_then(|info| info.variant_index),
        base_offset: data.base_offset,
        duration: data.duration,
        gapless: data.track_info.gapless,
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
