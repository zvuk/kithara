use std::{
    io::{Read, Seek},
    num::NonZeroU64,
};

use kithara_bufpool::HasPool;
use kithara_resampler::ResamplerBackend;
use kithara_stream::{
    ByteMap, ContainerFormat, MediaInfo, ReaderInput, ReaderProfile, ReaderWarmup,
};
use serde::Deserialize;

use super::{
    probe::{
        ProbeHint, codec_from_mp4_fourcc, resolve_codec_container, sniff_caf_codec,
        sniff_container_from_source, sniff_ogg_codec, sniff_wav_codec,
    },
    segment::segment_aware_container,
};
use crate::{
    DecodeResult, Decoder, DecoderConfig, consts, mp4::sniff_mp4_codec, traits::BoxedSource,
};

#[cfg(not(any(
    feature = "symphonia",
    apple_backend,
    android_backend,
    all(target_arch = "wasm32", feature = "webcodecs"),
)))]
compile_error!(
    "kithara-decode: enable a decoder backend for this target — `symphonia` \
     anywhere, `apple` on macOS/iOS, `android` on Android, `webcodecs` on \
     wasm32. With none of them every variant below is configured out, and a \
     build that decodes nothing is not one this crate can serve."
);

/// Which decoder implementation constructs the decode path.
///
/// A configuration document names the variant in `snake_case`. The variants
/// are gated by the features and targets that can actually provide them, so a
/// build that has no Apple backend refuses `apple` by name rather than
/// accepting a value it could not honour.
///
/// Defaults to the enabled native platform backend, or Symphonia elsewhere.
/// Android never dispatches to another
/// backend. Apple and `WebCodecs` can use Symphonia for unsupported formats when
/// that feature is also enabled.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, Deserialize, derive_more::Display, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecoderBackend {
    /// Apple `AudioToolbox` (macOS/iOS, requires the `apple` feature).
    #[cfg(apple_backend)]
    #[default]
    #[display("apple")]
    Apple,
    /// Android `MediaCodec` (Android, requires the `android` feature).
    #[cfg(android_backend)]
    #[default]
    #[display("android")]
    Android,
    /// Browser `AudioDecoder` (wasm32, requires the `webcodecs` feature).
    #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
    #[default]
    #[display("webcodecs")]
    WebCodecs,
    /// Symphonia software decoder (cross-platform, requires the
    /// `symphonia` feature).
    #[cfg(feature = "symphonia")]
    #[cfg_attr(
        not(any(
            apple_backend,
            android_backend,
            all(target_arch = "wasm32", feature = "webcodecs")
        )),
        default
    )]
    #[display("symphonia")]
    Symphonia,
}
/// Creates decoders under the backend selected by [`DecoderConfig::backend`].
///
/// Backend variants are available only on their configured targets. Android
/// selection stays within the native extractor/codec pipeline; unsupported
/// formats return [`DecodeError::UnsupportedCodec`].
pub struct DecoderFactory;

impl DecoderFactory {
    /// Create a decoder with the single selected backend.
    pub(crate) fn create<R, B, S>(
        source: R,
        hint: &ProbeHint,
        config: DecoderConfig<B, S>,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
        B: ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        let source: BoxedSource = Box::new(source);
        Self::dispatch_backend(source, hint, config)
    }

    /// Create decoder from `MediaInfo` (kithara-audio entry point).
    ///
    /// Extracts codec from `MediaInfo` and creates the appropriate decoder.
    ///
    /// # Errors
    ///
    /// Returns error if codec cannot be determined or decoder creation fails.
    /// No fallback — a failure is terminal.
    pub fn create_from_media_info<R, B, S>(
        source: R,
        media_info: &MediaInfo,
        config: DecoderConfig<B, S>,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
        B: ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        tracing::debug!(?media_info, "create_from_media_info called");

        let hint = ProbeHint {
            codec: media_info.codec,
            container: media_info.container,
            extension: None,
            mime: None,
        };

        Self::create(source, &hint, config)
    }

    /// Create a decoder from encoded input and an optional file-extension hint.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::ProbeFailed` when neither the input signature nor
    /// the supplied hint identifies a codec, and `DecodeError::*` for backend failures.
    ///
    /// MP4 and M4A are container-only formats, so the `stsd` sample-entry tag is sniffed to choose
    /// the actual codec backend.
    pub fn create_with_probe<R, B, S>(
        source: R,
        hint: Option<&str>,
        config: DecoderConfig<B, S>,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
        B: ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        let probe_hint = ProbeHint {
            container: hint
                .and_then(ContainerFormat::parse_extension)
                .filter(|container| *container != ContainerFormat::Mp4),
            extension: hint.map(String::from),
            ..Default::default()
        };

        Self::create(source, &probe_hint, config)
    }

    pub(super) fn dispatch_backend<B, S>(
        mut source: BoxedSource,
        hint: &ProbeHint,
        config: DecoderConfig<B, S>,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        B: ResamplerBackend,
        S: HasPool<u8> + HasPool<f32> + Send + Sync + 'static,
    {
        let mut hint = hint.clone();
        if hint.container.is_none() {
            hint.container = sniff_container_from_source(&mut source);
        }
        if hint.codec.is_none()
            && matches!(
                hint.container,
                Some(ContainerFormat::Mp4 | ContainerFormat::Fmp4)
            )
        {
            hint.codec =
                sniff_mp4_codec(&mut *source, &config.pools)?.and_then(codec_from_mp4_fourcc);
        }
        if hint.codec.is_none() && hint.container == Some(ContainerFormat::Ogg) {
            hint.codec = Some(sniff_ogg_codec(&mut source)?);
        }
        if hint.codec.is_none() && hint.container == Some(ContainerFormat::Caf) {
            hint.codec = Some(sniff_caf_codec(&mut source)?);
        }
        if hint.container == Some(ContainerFormat::Wav) {
            hint.codec = Some(sniff_wav_codec(&mut source)?);
        }
        let (codec, container) = resolve_codec_container(&hint)?;

        tracing::debug!(
            ?codec,
            ?container,
            backend = ?config.backend,
            "DecoderFactory::create called"
        );

        match config.backend {
            #[cfg(apple_backend)]
            DecoderBackend::Apple => {
                super::backend::apple::create(source, codec, container, config)
            }
            #[cfg(android_backend)]
            DecoderBackend::Android => {
                super::backend::android::create(source, codec, container, config)
            }
            #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
            DecoderBackend::WebCodecs => {
                super::backend::webcodecs::create(source, codec, container, config)
            }
            #[cfg(feature = "symphonia")]
            DecoderBackend::Symphonia => super::software::create(source, codec, container, config),
        }
    }

    /// Reader contract of the demuxer this factory would build for
    /// `media_info`, for the kithara-audio readiness gate.
    #[must_use]
    pub fn reader_profile(media_info: &MediaInfo, byte_map: Option<&dyn ByteMap>) -> ReaderProfile {
        const READER_READ_AHEAD_BYTES: NonZeroU64 = match NonZeroU64::new(32 * 1_024) {
            Some(bytes) => bytes,
            None => unreachable!(),
        };

        let input = match byte_map {
            Some(_)
                if media_info
                    .codec
                    .is_some_and(|codec| segment_aware_container(codec, media_info.container)) =>
            {
                consts::REQUIRED_INPUT
            }
            _ if matches!(media_info.container, Some(ContainerFormat::Wav)) => {
                ReaderInput::InitOnly
            }
            _ => ReaderInput::Incremental,
        };
        ReaderProfile::new(input, ReaderWarmup::None, READER_READ_AHEAD_BYTES)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "resample-rubato")]
    use kithara_resampler::rubato::RubatoBackend;
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;

    use super::*;
    #[cfg(feature = "resample-rubato")]
    use crate::DecoderResamplerConfig;

    #[kithara::test(native, flash(false))]
    fn wav_reader_profile_requires_gated_input() {
        let media_info = MediaInfo::builder()
            .maybe_codec(Some(AudioCodec::Pcm))
            .maybe_container(Some(ContainerFormat::Wav))
            .build();

        let profile = DecoderFactory::reader_profile(&media_info, None);

        assert_eq!(profile.input(), ReaderInput::InitOnly);
    }

    #[kithara::test(native, flash(false))]
    fn self_framing_reader_profile_remains_incremental() {
        let media_info = MediaInfo::builder()
            .maybe_codec(Some(AudioCodec::Mp3))
            .maybe_container(Some(ContainerFormat::MpegAudio))
            .build();

        let profile = DecoderFactory::reader_profile(&media_info, None);

        assert_eq!(profile.input(), ReaderInput::Incremental);
    }

    #[cfg(feature = "resample-rubato")]
    #[kithara::test(native, flash(false))]
    fn decoder_resampler_config_keeps_typed_backend() {
        let target_sample_rate = std::num::NonZeroU32::new(48_000).expect("test rate");
        let config: DecoderResamplerConfig<RubatoBackend> = DecoderResamplerConfig::builder()
            .target_sample_rate(target_sample_rate)
            .backend(RubatoBackend::default())
            .build();

        assert_eq!(config.target_sample_rate, target_sample_rate);
        assert_eq!(config.backend.name(), RubatoBackend::default().name());
    }
}
