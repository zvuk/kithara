use std::{
    io::{Read, Seek},
    sync::{Arc, atomic::AtomicU64},
};

use derivative::Derivative;
use derive_setters::Setters;
use kithara_bufpool::{BytePool, PcmPool};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, SegmentLayout, SharedHooks};

use super::probe::{ProbeHint, container_from_extension, probe_codec, resolve_codec_container};
use crate::{
    Decoder,
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

/// Explicit backend selection for [`DecoderFactory`].
///
/// Replaces the legacy boolean `prefer_hardware` flag with a typed
/// enum so callers spell out which backend they want. Failures of the
/// selected backend are terminal — there is no fallback chain.
///
/// Variants are gated on cargo features: a hardware variant exists in
/// the type only when its platform feature is enabled (and only on a
/// matching `target_os`). Picking `DecoderBackend::Apple` on Linux is
/// therefore a compile error, not a runtime `BackendUnavailable`.
///
/// Default = [`DecoderBackend::Symphonia`]: the software path is
/// cross-platform and capability-complete (gapless seek, full
/// `StreamContext` propagation). Hardware backends (`Apple`/`Android`)
/// are opt-in — there is no runtime fallback.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DecoderBackend {
    /// Apple `AudioToolbox` (macOS/iOS, requires the `apple` feature).
    #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
    #[cfg_attr(
        all(
            not(feature = "symphonia"),
            feature = "apple",
            any(target_os = "macos", target_os = "ios")
        ),
        default
    )]
    Apple,
    /// Android `MediaCodec` (Android, requires the `android` feature).
    #[cfg(all(feature = "android", target_os = "android"))]
    #[cfg_attr(
        all(
            not(feature = "symphonia"),
            feature = "android",
            target_os = "android",
            not(all(feature = "apple", any(target_os = "macos", target_os = "ios")))
        ),
        default
    )]
    Android,
    /// Symphonia software decoder (cross-platform, requires the
    /// `symphonia` feature).
    #[cfg(feature = "symphonia")]
    #[default]
    Symphonia,
}

impl std::fmt::Display for DecoderBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
            Self::Apple => f.write_str("apple"),
            #[cfg(all(feature = "android", target_os = "android"))]
            Self::Android => f.write_str("android"),
            #[cfg(feature = "symphonia")]
            Self::Symphonia => f.write_str("symphonia"),
        }
    }
}

/// Configuration for `DecoderFactory`.
///
/// `pcm_pool` / `byte_pool` are intentionally `Option<_>` — the
/// production discipline is to propagate one pool down the entire host
/// chain (player → `AudioConfig` → `DecoderConfig`). The `None` arm is
/// a last-resort fallback for unit tests that don't care about pool
/// budgets and for legacy call sites that haven't been threaded yet;
/// it routes to the process-global `PcmPool::default()` /
/// `BytePool::default()`. Don't construct fresh `PcmPool::new` / `BytePool::new`
/// inside library components — that fragments the heap into many small
/// per-component pools and defeats recycling.
#[derive(Clone, Derivative, Setters)]
#[derivative(Default)]
#[setters(prefix = "with_", strip_option)]
#[non_exhaustive]
pub struct DecoderConfig {
    /// Which decoder backend to use. See [`DecoderBackend`].
    pub backend: DecoderBackend,
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Raw byte buffer pool, propagated from the host. `None` falls
    /// back to `BytePool::default()`.
    pub byte_pool: Option<BytePool>,
    /// File extension hint for Symphonia probe (e.g., "mp3", "aac").
    ///
    /// Used when the container format is not specified, as a hint for auto-detection.
    pub hint: Option<String>,
    /// Reader-side observer hooks. Forwarded into [`ComposedDecoder`]
    /// directly; emitting them is opt-in and zero-overhead when `None`.
    /// `Some(_)` wires the chunk/seek signals into the supplied
    /// [`SharedHooks`] sink (mock sources, tests, telemetry).
    ///
    /// [`ComposedDecoder`]: crate::composed::ComposedDecoder
    #[setters(skip)]
    pub hooks: Option<SharedHooks>,
    /// PCM buffer pool, propagated from the host. `None` falls back to
    /// `PcmPool::default()`.
    pub pcm_pool: Option<PcmPool>,
    /// Optional segment-layout handle over the underlying source. When
    /// present, fMP4 AAC / FLAC streams dispatch through the
    /// segment-by-segment demuxer (`Fmp4SegmentDemuxer`) instead of the
    /// whole-stream container parser, side-stepping prefix walks during
    /// forward seek.
    #[setters(skip)]
    pub segment_layout: Option<Arc<dyn SegmentLayout>>,
    /// Enable gapless trim wiring through the per-backend codec.
    ///
    /// `true` (default) flips the matching `*Config::gapless` for the
    /// selected backend (Apple / Symphonia / Android) so the codec
    /// emits priming and padding-trimmed PCM for codecs whose encoder
    /// reports those numbers (FLAC, Opus, Vorbis via Symphonia; AAC
    /// via Apple `kAudioConverterPrimeInfo`). Container-level priming
    /// (MP4 `iTunSMPB` for AAC) is captured separately via the
    /// `*Codec::probe_track_info` helpers — wired in a follow-up
    /// alongside the [`crate::DecoderTrackInfo`] propagation through
    /// the `Decoder` trait.
    #[derivative(Default(value = "true"))]
    pub gapless: bool,
    /// Epoch counter for decoder recreation tracking.
    pub epoch: u64,
}

impl DecoderConfig {
    /// Set [`Self::hooks`] from an `Option`. Same `Option` rationale as
    /// [`Self::with_segment_layout`].
    #[must_use]
    pub fn with_hooks(mut self, hooks: Option<SharedHooks>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Set [`Self::segment_layout`] from an `Option`. Distinct from
    /// the derived `with_*` setters because `derive_setters` with
    /// `strip_option` would force callers to unwrap the `Option`
    /// themselves. Stream sources surface segment layout as
    /// `Option<...>` already, so this signature avoids the dance.
    #[must_use]
    pub fn with_segment_layout(mut self, layout: Option<Arc<dyn SegmentLayout>>) -> Self {
        self.segment_layout = layout;
        self
    }
}

/// Factory for creating decoders with a single, strict backend selection.
///
/// Backend matrix (driven by [`DecoderConfig::backend`]):
/// - [`DecoderBackend::Apple`] / [`DecoderBackend::Android`] — hardware
///   backend, only present in the type when the matching feature and
///   `target_os` are active. No runtime fallback.
/// - [`DecoderBackend::Symphonia`] — software backend, present when
///   the `symphonia` feature is enabled. No runtime fallback.
pub struct DecoderFactory;

impl DecoderFactory {
    /// Create a decoder with the single selected backend.
    pub(crate) fn create<R>(
        source: R,
        hint: &ProbeHint,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
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
    pub fn create_from_media_info<R>(
        source: R,
        media_info: &MediaInfo,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
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

    /// Create decoder from a file-extension hint.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::ProbeFailed` when the hint is missing or too
    /// weak to pick a codec, and `DecodeError::*` for backend failures.
    /// No fallback — callers must supply a usable hint.
    pub fn create_with_probe<R>(
        source: R,
        hint: Option<&str>,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let probe_hint = ProbeHint {
            container: hint.and_then(container_from_extension),
            extension: hint.map(String::from),
            ..Default::default()
        };

        probe_codec(&probe_hint)?;
        Self::create(source, &probe_hint, config)
    }

    pub(super) fn dispatch_backend(
        source: BoxedSource,
        hint: &ProbeHint,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn Decoder>> {
        let (codec, container) = resolve_codec_container(hint)?;

        tracing::debug!(
            ?codec,
            ?container,
            backend = ?config.backend,
            "DecoderFactory::create called"
        );

        match config.backend {
            #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
            DecoderBackend::Apple => create_apple(source, codec, container, config),
            #[cfg(all(feature = "android", target_os = "android"))]
            DecoderBackend::Android => create_android(source, codec, container, config),
            #[cfg(feature = "symphonia")]
            DecoderBackend::Symphonia => create_symphonia(source, codec, container, config),
        }
    }
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn create_apple(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::apple::AppleCodec;

    if should_use_segment_aware(codec, container, config)
        && let Some(layout) = config.segment_layout.clone()
    {
        if AppleCodec::supports(codec) {
            tracing::debug!(
                ?codec,
                "fmp4_segment: dispatching to segment-aware Apple HW codec path"
            );
            let gapless = config.gapless;
            return build_fmp4_segment_decoder(source, layout, config, |track| {
                AppleCodec::open_with_config(track, gapless)
            });
        }
        #[cfg(feature = "symphonia")]
        return create_fmp4_segment_symphonia(source, codec, layout, config);
        #[cfg(not(feature = "symphonia"))]
        {
            let _ = layout;
            return Err(DecodeError::UnsupportedCodec(codec));
        }
    }

    if apple_standalone_supports(codec, container) {
        tracing::debug!(
            ?codec,
            ?container,
            "apple-standalone: routing via AudioFileServices"
        );
        return build_apple_standalone_decoder(source, codec, container, config);
    }

    #[cfg(feature = "symphonia")]
    return create_symphonia(source, codec, container, config);
    #[cfg(not(feature = "symphonia"))]
    {
        let _ = (source, container, config);
        Err(DecodeError::UnsupportedCodec(codec))
    }
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn apple_standalone_supports(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
    matches!(
        (codec, container),
        (AudioCodec::Pcm, Some(ContainerFormat::Wav))
            | (AudioCodec::Mp3, Some(ContainerFormat::MpegAudio))
            | (AudioCodec::Alac, Some(ContainerFormat::Mp4))
            | (AudioCodec::Alac, Some(ContainerFormat::Caf))
    )
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn build_apple_standalone_decoder(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::{
        apple::{AppleAudioFileDemuxer, AppleCodec},
        composed::ComposedDecoder,
        demuxer::Demuxer,
    };
    let demuxer = match (codec, container) {
        (AudioCodec::Pcm, Some(ContainerFormat::Wav)) => AppleAudioFileDemuxer::open_wav(source)?,
        (AudioCodec::Mp3, Some(ContainerFormat::MpegAudio)) => {
            AppleAudioFileDemuxer::open_mp3(source)?
        }
        (AudioCodec::Alac, Some(ContainerFormat::Mp4)) => {
            AppleAudioFileDemuxer::open_alac_m4a(source)?
        }
        (AudioCodec::Alac, Some(ContainerFormat::Caf)) => {
            AppleAudioFileDemuxer::open_alac_caf(source)?
        }
        _ => return Err(DecodeError::UnsupportedCodec(codec)),
    };
    let codec_impl = AppleCodec::open_with_config(demuxer.track_info(), config.gapless)?;
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| PcmPool::default().clone());
    let decoder = ComposedDecoder::new(
        demuxer,
        codec_impl,
        pool,
        config.epoch,
        config.byte_len_handle.clone(),
        config.hooks.clone(),
    );
    Ok(Box::new(decoder))
}

#[cfg(all(feature = "android", target_os = "android"))]
fn create_android(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::android::AndroidCodec;

    if should_use_segment_aware(codec, container, config)
        && let Some(layout) = config.segment_layout.clone()
    {
        if AndroidCodec::supports(codec) {
            tracing::debug!(
                ?codec,
                "fmp4_segment: dispatching to segment-aware Android HW codec path"
            );
            let gapless = config.gapless;
            return build_fmp4_segment_decoder(source, layout, config, |track| {
                AndroidCodec::open_with_config(track, gapless)
            });
        }
        #[cfg(feature = "symphonia")]
        return create_fmp4_segment_symphonia(source, codec, layout, config);
        #[cfg(not(feature = "symphonia"))]
        {
            let _ = layout;
            return Err(DecodeError::UnsupportedCodec(codec));
        }
    }

    if android_standalone_supports(codec, container) {
        tracing::debug!(
            ?codec,
            ?container,
            "android-standalone: routing via AMediaExtractor"
        );
        return build_android_standalone_decoder(source, codec, container, config);
    }

    #[cfg(feature = "symphonia")]
    return create_symphonia(source, codec, container, config);
    #[cfg(not(feature = "symphonia"))]
    {
        let _ = (source, container, config);
        Err(DecodeError::UnsupportedCodec(codec))
    }
}

#[cfg(all(feature = "android", target_os = "android"))]
fn android_standalone_supports(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
    matches!(
        (codec, container),
        (AudioCodec::Pcm, Some(ContainerFormat::Wav))
            | (AudioCodec::Mp3, Some(ContainerFormat::MpegAudio))
            | (AudioCodec::Alac, Some(ContainerFormat::Mp4))
    )
}

#[cfg(all(feature = "android", target_os = "android"))]
fn build_android_standalone_decoder(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::{
        android::{AndroidCodec, AndroidMediaExtractorDemuxer},
        composed::ComposedDecoder,
        demuxer::Demuxer,
    };
    let demuxer = match (codec, container) {
        (AudioCodec::Pcm, Some(ContainerFormat::Wav)) => {
            AndroidMediaExtractorDemuxer::open_wav(source)?
        }
        (AudioCodec::Mp3, Some(ContainerFormat::MpegAudio)) => {
            AndroidMediaExtractorDemuxer::open_mp3(source)?
        }
        (AudioCodec::Alac, Some(ContainerFormat::Mp4)) => {
            AndroidMediaExtractorDemuxer::open_alac_m4a(source)?
        }
        _ => return Err(DecodeError::UnsupportedCodec(codec)),
    };
    let codec_impl = AndroidCodec::open_with_config(demuxer.track_info(), config.gapless)?;
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| PcmPool::default().clone());
    let decoder = ComposedDecoder::new(
        demuxer,
        codec_impl,
        pool,
        config.epoch,
        config.byte_len_handle.clone(),
        config.hooks.clone(),
    );
    Ok(Box::new(decoder))
}

#[cfg(feature = "symphonia")]
fn create_symphonia(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    if should_use_segment_aware(codec, container, config)
        && let Some(layout) = config.segment_layout.clone()
    {
        return create_fmp4_segment_symphonia(source, codec, layout, config);
    }
    create_file_symphonia_universal(source, codec, container, config)
}

#[cfg(feature = "symphonia")]
fn create_file_symphonia_universal(
    mut source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use std::io::SeekFrom;

    use crate::{
        GaplessInfo,
        composed::ComposedDecoder,
        demuxer::Demuxer,
        gapless::{LAME_DECODER_DELAY, probe_mp4_gapless_dyn, read_lame_trim},
        symphonia::{SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer},
        traits::DecoderInput,
    };

    /// LAME header probe window: read up to ~16 `KiB` to cover `ID3v2`
    /// tags and a couple of MP3 frames before the Xing/Info+LAME slot.
    const LAME_PROBE_WINDOW_BYTES: usize = 16 * 1024;

    fn probe_codec_gapless(
        codec: AudioCodec,
        source: &mut dyn DecoderInput,
    ) -> Option<GaplessInfo> {
        match codec {
            AudioCodec::AacLc => probe_mp4_gapless_dyn(source).ok().flatten(),
            AudioCodec::Mp3 => {
                let mut buffer = Vec::with_capacity(LAME_PROBE_WINDOW_BYTES);
                source
                    .take(LAME_PROBE_WINDOW_BYTES as u64)
                    .read_to_end(&mut buffer)
                    .ok()?;
                let trim = read_lame_trim(&buffer)?;
                Some(GaplessInfo {
                    leading_frames: u64::from(trim.enc_delay)
                        .saturating_add(u64::from(LAME_DECODER_DELAY)),
                    trailing_frames: u64::from(trim.enc_padding)
                        .saturating_sub(u64::from(LAME_DECODER_DELAY)),
                })
            }
            _ => None,
        }
    }

    tracing::debug!(
        ?codec,
        ?container,
        "file-symphonia: dispatching to ComposedDecoder<SymphoniaDemuxer, SymphoniaCodec>"
    );

    let probed_gapless = if config.gapless {
        let _ = source.seek(SeekFrom::Start(0));
        let info = probe_codec_gapless(codec, &mut *source);
        let _ = source.seek(SeekFrom::Start(0));
        info
    } else {
        None
    };

    let (mut demuxer, _byte_len) = SymphoniaDemuxer::open_file(
        source,
        config.hint.clone(),
        container,
        config.byte_len_handle.clone(),
        config.segment_layout.clone(),
    )?;
    if probed_gapless.is_some() {
        demuxer.set_gapless(probed_gapless);
    }
    let symphonia_config = SymphoniaConfig {
        gapless: config.gapless,
        ..Default::default()
    };
    let codec_impl = if SymphoniaCodec::supports(codec) {
        SymphoniaCodec::open_with_config(demuxer.track_info(), &symphonia_config)?
    } else {
        SymphoniaCodec::open_native(demuxer.native_params())?
    };
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| PcmPool::default().clone());
    let decoder = ComposedDecoder::new(
        demuxer,
        codec_impl,
        pool,
        config.epoch,
        config.byte_len_handle.clone(),
        config.hooks.clone(),
    );
    Ok(Box::new(decoder))
}

/// Gate for the segment-aware fMP4 path. Routes AAC / FLAC fMP4 with a
/// surfaced `SegmentedSource` (HLS) through `Fmp4SegmentDecoder`. File
/// sources without segment metadata fall through to the legacy
/// `IsoMp4Reader` path.
fn should_use_segment_aware(
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> bool {
    matches!(codec, AudioCodec::AacLc | AudioCodec::Flac)
        && matches!(container, Some(ContainerFormat::Fmp4))
        && config.segment_layout.is_some()
}

#[cfg(feature = "symphonia")]
fn create_fmp4_segment_symphonia(
    source: BoxedSource,
    codec: AudioCodec,
    layout: Arc<dyn SegmentLayout>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::symphonia::{SymphoniaCodec, SymphoniaConfig};

    tracing::debug!(
        ?codec,
        "fmp4_segment: dispatching to segment-aware Symphonia path"
    );
    match codec {
        AudioCodec::AacLc | AudioCodec::Flac => {
            let symphonia_config = SymphoniaConfig {
                gapless: config.gapless,
                ..Default::default()
            };
            build_fmp4_segment_decoder(source, layout, config, |track| {
                SymphoniaCodec::open_with_config(track, &symphonia_config)
            })
        }
        other => Err(DecodeError::UnsupportedCodec(other)),
    }
}

/// Generic builder for the segment-aware fMP4 path. Owns the
/// [`Fmp4SegmentDemuxer`] open + pool-resolution + [`ComposedDecoder`]
/// boilerplate so apple/android/symphonia call-sites collapse into a
/// single closure that opens the codec from `TrackInfo`.
fn build_fmp4_segment_decoder<C, F>(
    source: BoxedSource,
    layout: Arc<dyn SegmentLayout>,
    config: &DecoderConfig,
    open_codec: F,
) -> DecodeResult<Box<dyn Decoder>>
where
    C: crate::codec::FrameCodec + 'static,
    F: FnOnce(&crate::demuxer::TrackInfo) -> DecodeResult<C>,
{
    use crate::{composed::ComposedDecoder, demuxer::Demuxer, fmp4::Fmp4SegmentDemuxer};

    let demuxer = Fmp4SegmentDemuxer::open(source, layout)?;
    let codec = open_codec(demuxer.track_info())?;
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| PcmPool::default().clone());
    let decoder = ComposedDecoder::new(
        demuxer,
        codec,
        pool,
        config.epoch,
        config.byte_len_handle.clone(),
        config.hooks.clone(),
    );
    Ok(Box::new(decoder))
}
