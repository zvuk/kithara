use std::{
    io::{Read, Seek},
    sync::{Arc, atomic::AtomicU64},
};

use bon::Builder;
use kithara_bufpool::{BytePool, PcmPool};
use kithara_stream::{
    AudioCodec, BoxedEventSink, ByteMap, ContainerFormat, MediaInfo, needs_exact_byte_sizes,
};

use super::probe::{
    ProbeHint, codec_from_mp4_fourcc, container_from_extension, probe_codec,
    resolve_codec_container, sniff_container_from_source,
};
#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
use crate::GaplessInfo;
use crate::{
    Decoder, InputRequirement,
    error::{DecodeError, DecodeResult},
    mp4::sniff_mp4_codec,
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
///
/// Exactly one backend feature is expected per build: device builds
/// (`apple` / `android`) compile with `--no-default-features` so
/// `symphonia` is absent, and the hardware variant is the sole default.
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
#[derive(Builder)]
#[builder(state_mod(vis = "pub"))]
#[non_exhaustive]
pub struct DecoderConfig {
    /// Which decoder backend to use. See [`DecoderBackend`].
    #[builder(default)]
    pub backend: DecoderBackend,
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Optional byte-map handle over the underlying source.
    pub byte_map: Option<Arc<dyn ByteMap>>,
    /// Raw byte buffer pool, propagated from the host. `None` falls
    /// back to `BytePool::default()`.
    pub byte_pool: Option<BytePool>,
    /// File extension hint for Symphonia probe (e.g., "mp3", "aac").
    pub hint: Option<String>,
    /// Reader-side observer hooks. Single-owner; moved into
    /// [`ComposedDecoder`] by the chosen backend path.
    pub hooks: Option<BoxedEventSink>,
    /// PCM buffer pool, propagated from the host. `None` falls back to
    /// `PcmPool::default()`.
    pub pcm_pool: Option<PcmPool>,
    /// Optional hardware-decoder output sample rate. `None` means the
    /// backend emits at the source rate.
    pub target_output_rate: Option<u32>,
    /// Enable gapless trim wiring through the per-backend codec.
    #[builder(default = true)]
    pub gapless: bool,
    /// Epoch counter for decoder recreation tracking.
    #[builder(default)]
    pub epoch: u64,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self::builder().build()
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
        config: DecoderConfig,
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
        config: DecoderConfig,
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
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let mut source = source;
        let mut probe_hint = ProbeHint {
            container: hint.and_then(container_from_extension),
            extension: hint.map(String::from),
            ..Default::default()
        };

        // WHY: MP4/M4A is container-only (AAC/ALAC/FLAC all live there); sniff the `stsd` sample-entry tag to pick the right codec backend.
        if matches!(
            probe_hint.container,
            Some(ContainerFormat::Mp4 | ContainerFormat::Fmp4)
        ) && let Some(codec) = sniff_mp4_codec(&mut source).and_then(codec_from_mp4_fourcc)
        {
            probe_hint.codec = Some(codec);
        }

        probe_codec(&probe_hint)?;
        Self::create(source, &probe_hint, config)
    }

    pub(super) fn dispatch_backend(
        mut source: BoxedSource,
        hint: &ProbeHint,
        config: DecoderConfig,
    ) -> DecodeResult<Box<dyn Decoder>> {
        let (codec, mut container) = resolve_codec_container(hint)?;
        if container.is_none() {
            container = sniff_container_from_source(&mut source);
        }

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

    /// Input contract of the demuxer this factory would build for `media_info`,
    /// for the kithara-audio readiness gate. Mirrors [`should_use_segment_aware`]:
    /// only the segment-aware fMP4 path is `InitOnly`. See the crate `CONTEXT.md`
    /// "Decoder input contract".
    #[must_use]
    pub fn input_requirement(
        media_info: &MediaInfo,
        byte_map: Option<&dyn ByteMap>,
    ) -> InputRequirement {
        match byte_map {
            Some(_)
                if media_info
                    .codec
                    .is_some_and(|codec| segment_aware_container(codec, media_info.container)) =>
            {
                <crate::fmp4::Fmp4SegmentDemuxer as crate::demuxer::Demuxer>::required_input()
            }
            _ => InputRequirement::Incremental,
        }
    }
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn create_apple(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::apple::AppleCodec;

    if should_use_segment_aware(codec, container, &config)
        && let Some(layout) = config.byte_map.clone()
    {
        if AppleCodec::supports(codec) {
            tracing::debug!(
                ?codec,
                "fmp4_segment: dispatching to segment-aware Apple HW codec path"
            );
            let gapless = config.gapless;
            let target_output_rate = config.target_output_rate;
            return build_fmp4_segment_decoder(source, layout, config, |track| {
                let output_track = track_with_output_domain_gapless(track, target_output_rate)?;
                AppleCodec::open_with_config(&output_track, gapless, target_output_rate)
            });
        }
        #[cfg(feature = "symphonia")]
        return create_fmp4_segment_symphonia(source, codec, layout, config);
        #[cfg(not(feature = "symphonia"))]
        {
            let _ = layout;
            return Err(DecodeError::UnsupportedCodec { codec });
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
        Err(DecodeError::UnsupportedCodec { codec })
    }
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn apple_standalone_supports(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
    crate::apple::AppleAudioFileDemuxer::supports(codec, container)
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn build_apple_standalone_decoder(
    mut source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::{
        apple::{AppleAudioFileDemuxer, AppleCodec, SourceOpenMode},
        composed::{ComposedDecoder, DecoderRuntime},
        demuxer::Demuxer,
        gapless::{scoped_probe, scoped_startup_probe},
    };
    let startup_probe = scoped_startup_probe(&mut *source, codec)?;
    let probed_gapless = if config.gapless {
        if matches!(codec, AudioCodec::Mp3) {
            startup_probe.gapless
        } else {
            scoped_probe(&mut *source, codec)?
        }
    } else {
        None
    };
    let open_mode = if config.byte_len_handle.is_some() {
        SourceOpenMode::Streaming
    } else {
        SourceOpenMode::Complete
    };
    let mut demuxer = AppleAudioFileDemuxer::open_for_with_mode(
        source,
        codec,
        container,
        open_mode,
        startup_probe.duration,
    )?;
    let output_gapless = scale_gapless_for_output_domain(
        probed_gapless,
        demuxer.track_info().sample_rate,
        config.target_output_rate,
    )?;
    if output_gapless.is_some() {
        demuxer.set_gapless(output_gapless);
    }
    let codec_impl = AppleCodec::open_with_config(
        demuxer.track_info(),
        config.gapless,
        config.target_output_rate,
    )?;
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| PcmPool::default().clone());
    let decoder = ComposedDecoder::new(
        demuxer,
        codec_impl,
        DecoderRuntime {
            pool,
            epoch: config.epoch,
            byte_len_handle: config.byte_len_handle.clone(),
            hooks: config.hooks,
        },
    );
    Ok(Box::new(decoder))
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn track_with_output_domain_gapless(
    track: &crate::demuxer::TrackInfo,
    target_output_rate: Option<u32>,
) -> DecodeResult<crate::demuxer::TrackInfo> {
    let mut output_track = track.clone();
    output_track.gapless = scale_gapless_for_output_domain(
        output_track.gapless,
        output_track.sample_rate,
        target_output_rate,
    )?;
    Ok(output_track)
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn scale_gapless_for_output_domain(
    gapless: Option<GaplessInfo>,
    source_rate: u32,
    target_output_rate: Option<u32>,
) -> DecodeResult<Option<GaplessInfo>> {
    let Some(info) = gapless else {
        return Ok(None);
    };
    let Some(output_rate) = target_output_rate.filter(|rate| *rate != source_rate) else {
        return Ok(Some(info));
    };

    if source_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "apple.gapless.source",
        });
    }
    if output_rate == 0 {
        return Err(DecodeError::InvalidSampleRate {
            resource: "apple.gapless.output",
        });
    }

    // Container probes are born in source-rate frames; the trimmer only sees
    // decoder-output frames, so Apple fused SRC scales once at this boundary.
    Ok(Some(GaplessInfo {
        leading_frames: round_scaled_frames(info.leading_frames, source_rate, output_rate)?,
        trailing_frames: round_scaled_frames(info.trailing_frames, source_rate, output_rate)?,
    }))
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn round_scaled_frames(count: u64, source_rate: u32, output_rate: u32) -> DecodeResult<u64> {
    let numerator = u128::from(count)
        .saturating_mul(u128::from(output_rate))
        .saturating_add(u128::from(source_rate / 2));
    let scaled = numerator / u128::from(source_rate);
    u64::try_from(scaled).map_err(|_| DecodeError::InvalidData {
        detail: "apple gapless output-domain frame count overflow",
    })
}

#[cfg(all(feature = "android", target_os = "android"))]
fn create_android(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::android::AndroidCodec;

    if should_use_segment_aware(codec, container, &config)
        && let Some(layout) = config.byte_map.clone()
    {
        if AndroidCodec::supports(codec) {
            tracing::debug!(
                ?codec,
                "fmp4_segment: dispatching to segment-aware Android HW codec path"
            );
            return build_fmp4_segment_decoder(source, layout, config, |track| {
                AndroidCodec::open_with_config(track)
            });
        }
        #[cfg(feature = "symphonia")]
        return create_fmp4_segment_symphonia(source, codec, layout, config);
        #[cfg(not(feature = "symphonia"))]
        {
            let _ = layout;
            return Err(DecodeError::UnsupportedCodec { codec });
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
        Err(DecodeError::UnsupportedCodec { codec })
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
    config: DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::{
        android::{AndroidCodec, AndroidMediaExtractorDemuxer},
        composed::{ComposedDecoder, DecoderRuntime},
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
        _ => return Err(DecodeError::UnsupportedCodec { codec }),
    };
    let codec_impl = AndroidCodec::open_with_config(demuxer.track_info())?;
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| PcmPool::default().clone());
    let decoder = ComposedDecoder::new(
        demuxer,
        codec_impl,
        DecoderRuntime {
            pool,
            epoch: config.epoch,
            byte_len_handle: config.byte_len_handle.clone(),
            hooks: config.hooks,
        },
    );
    Ok(Box::new(decoder))
}

#[cfg(feature = "symphonia")]
fn create_symphonia(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    if should_use_segment_aware(codec, container, &config)
        && let Some(layout) = config.byte_map.clone()
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
    config: DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::{
        composed::{ComposedDecoder, DecoderRuntime},
        demuxer::Demuxer,
        gapless::scoped_probe,
        symphonia::{FileOpen, SymphoniaCodec, SymphoniaConfig, SymphoniaDemuxer},
    };

    tracing::debug!(
        ?codec,
        ?container,
        "file-symphonia: dispatching to ComposedDecoder<SymphoniaDemuxer, SymphoniaCodec>"
    );

    let probed_gapless = if config.gapless {
        scoped_probe(&mut *source, codec)?
    } else {
        None
    };

    let (mut demuxer, _byte_len) = SymphoniaDemuxer::open_file(
        source,
        FileOpen {
            container,
            hint: config.hint.clone(),
            byte_len_handle: config.byte_len_handle.clone(),
            byte_map: config.byte_map.clone(),
        },
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
        DecoderRuntime {
            pool,
            epoch: config.epoch,
            byte_len_handle: config.byte_len_handle.clone(),
            hooks: config.hooks,
        },
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
    segment_aware_container(codec, container) && config.byte_map.is_some()
}

/// Codec+container half of the segment-aware fMP4 decision, factored out of
/// [`should_use_segment_aware`] so [`DecoderFactory::input_requirement`] can
/// ask the same question without a [`DecoderConfig`]. AAC / FLAC in fMP4 is
/// the only segment-aware path; everything else reads incrementally.
fn segment_aware_container(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
    !needs_exact_byte_sizes(Some(codec), container)
}

#[cfg(feature = "symphonia")]
fn create_fmp4_segment_symphonia(
    source: BoxedSource,
    codec: AudioCodec,
    layout: Arc<dyn ByteMap>,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::symphonia::{SymphoniaCodec, SymphoniaConfig};

    tracing::debug!(
        ?codec,
        "fmp4_segment: dispatching to segment-aware Symphonia path"
    );
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 | AudioCodec::Flac => {
            let symphonia_config = SymphoniaConfig {
                gapless: config.gapless,
                ..Default::default()
            };
            build_fmp4_segment_decoder(source, layout, config, |track| {
                SymphoniaCodec::open_with_config(track, &symphonia_config)
            })
        }
        other => Err(DecodeError::UnsupportedCodec { codec: other }),
    }
}

/// Generic builder for the segment-aware fMP4 path. Owns the
/// [`Fmp4SegmentDemuxer`] open + pool-resolution + [`ComposedDecoder`]
/// boilerplate so apple/android/symphonia call-sites collapse into a
/// single closure that opens the codec from `TrackInfo`.
fn build_fmp4_segment_decoder<C, F>(
    source: BoxedSource,
    layout: Arc<dyn ByteMap>,
    config: DecoderConfig,
    open_codec: F,
) -> DecodeResult<Box<dyn Decoder>>
where
    C: crate::codec::FrameCodec + 'static,
    F: FnOnce(&crate::demuxer::TrackInfo) -> DecodeResult<C>,
{
    use crate::{
        composed::{ComposedDecoder, DecoderRuntime},
        demuxer::Demuxer,
        fmp4::Fmp4SegmentDemuxer,
    };

    let demuxer =
        Fmp4SegmentDemuxer::open(source, layout, config.byte_pool.clone().unwrap_or_default())?;
    let codec = open_codec(demuxer.track_info())?;
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| PcmPool::default().clone());
    let decoder = ComposedDecoder::new(
        demuxer,
        codec,
        DecoderRuntime {
            pool,
            epoch: config.epoch,
            byte_len_handle: config.byte_len_handle.clone(),
            hooks: config.hooks,
        },
    );
    Ok(Box::new(decoder))
}

/// RED (device repro): on the size-reduced apple-only build (no symphonia
/// fallback), `MediaInfo { codec: AacLc, container: None }` over real fMP4
/// bytes must resolve the shared container hint before backend dispatch.
#[cfg(all(test, feature = "apple", any(target_os = "macos", target_os = "ios")))]
mod apple_factory_tests {
    use std::{io::Cursor, num::NonZeroU32, sync::Arc};

    use kithara_bufpool::PcmBuf;
    use kithara_platform::time::Duration;
    use kithara_stream::{AudioCodec, ByteMap, MediaInfo};
    use kithara_test_utils::kithara;

    use super::{
        DecoderBackend, DecoderConfig, DecoderFactory, round_scaled_frames,
        track_with_output_domain_gapless,
    };
    use crate::{
        DecodeError, DecoderChunkOutcome, DecoderTrackInfo, GaplessInfo, GaplessTrimmer, PcmSpec,
        codec::FrameCodec,
        composed::{ComposedDecoder, DecoderRuntime},
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
        duration_for_frames,
        error::DecodeResult,
        fmp4::test_layout::{TestLayoutCodec, build_test_layout},
        traits::Decoder,
    };

    struct PacketDemuxer {
        track: TrackInfo,
        held: Vec<u8>,
        next_index: u64,
        packet_count: u64,
        packet_frames: u32,
        source_rate: u32,
    }

    impl Demuxer for PacketDemuxer {
        fn duration(&self) -> Option<Duration> {
            Some(duration_for_frames(
                self.source_rate,
                self.packet_count
                    .saturating_mul(u64::from(self.packet_frames)),
            ))
        }

        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            if self.next_index >= self.packet_count {
                return Ok(DemuxOutcome::Eof);
            }
            let packet_idx = self.next_index;
            self.next_index = self.next_index.saturating_add(1);
            self.held = vec![1u8; 4];
            Ok(DemuxOutcome::Frame(Frame {
                data: &self.held,
                packet_desc: &[],
                pts: duration_for_frames(
                    self.source_rate,
                    packet_idx.saturating_mul(u64::from(self.packet_frames)),
                ),
                duration: duration_for_frames(self.source_rate, u64::from(self.packet_frames)),
            }))
        }

        fn seek(
            &mut self,
            _target: Duration,
            _priming: crate::codec::CodecPriming,
        ) -> DecodeResult<DemuxSeekOutcome> {
            self.next_index = 0;
            Ok(DemuxSeekOutcome::Landed {
                landed_at: Duration::ZERO,
                landed_byte: Some(0),
                preroll: crate::demuxer::PrerollHint::NotNeeded,
            })
        }

        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
    }

    struct OutputDomainCodec {
        spec: PcmSpec,
        frames_per_call: u32,
        track_info: DecoderTrackInfo,
    }

    impl FrameCodec for OutputDomainCodec {
        fn decode_frame(
            &mut self,
            bytes: &[u8],
            _pts: Duration,
            _packet_desc: &[u8],
            out: &mut PcmBuf,
        ) -> DecodeResult<u32> {
            if bytes.is_empty() {
                out.clear();
                return Ok(0);
            }
            write_silent_frame(self.spec, self.frames_per_call, out)
        }

        fn flush(&mut self) -> DecodeResult<()> {
            Ok(())
        }

        fn spec(&self) -> PcmSpec {
            self.spec
        }

        fn track_info(&self) -> DecoderTrackInfo {
            self.track_info.clone()
        }
    }

    fn write_silent_frame(spec: PcmSpec, frames: u32, out: &mut PcmBuf) -> DecodeResult<u32> {
        let samples = usize::try_from(frames)?
            .checked_mul(usize::from(spec.channels))
            .ok_or(DecodeError::InvalidData {
                detail: "factory test sample count overflow",
            })?;
        out.ensure_len(samples)?;
        for sample in out.iter_mut() {
            *sample = 0.0;
        }
        out.truncate(samples);
        Ok(frames)
    }

    fn aac_track(sample_rate: u32, gapless: Option<GaplessInfo>) -> TrackInfo {
        TrackInfo {
            codec: AudioCodec::AacLc,
            sample_rate,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
            gapless,
        }
    }

    fn output_frames(chunks: impl IntoIterator<Item = crate::PcmChunk>) -> u64 {
        chunks
            .into_iter()
            .map(|chunk| u64::from(chunk.meta.frames))
            .sum()
    }

    #[kithara::test]
    fn apple_aac_lc_metadata_probe_is_not_rejected_as_unsupported_codec() {
        let (blob, segmented) = build_test_layout(TestLayoutCodec::Aac, 1);
        let source = Cursor::new(blob);
        let byte_map: Arc<dyn ByteMap> = Arc::new(segmented);
        let media_info = MediaInfo::new(Some(AudioCodec::AacLc), None);
        let config = DecoderConfig {
            backend: DecoderBackend::Apple,
            byte_map: Some(byte_map),
            ..DecoderConfig::default()
        };

        let result = DecoderFactory::create_from_media_info(source, &media_info, config);

        assert!(
            !matches!(
                result,
                Err(DecodeError::UnsupportedCodec {
                    codec: AudioCodec::AacLc
                })
            ),
            "apple-only AAC-LC fMP4 metadata probe was rejected as UnsupportedCodec"
        );
    }

    #[kithara::test]
    fn apple_gapless_scaling_is_identity_without_fused_src() {
        let source_gapless = Some(GaplessInfo {
            leading_frames: 1024,
            trailing_frames: 441,
        });
        let track = aac_track(44_100, source_gapless);

        let no_target = track_with_output_domain_gapless(&track, None)
            .expect("BUG: output-domain track without target");
        let equal_target = track_with_output_domain_gapless(&track, Some(44_100))
            .expect("BUG: output-domain track with equal target");

        assert_eq!(no_target.gapless, source_gapless);
        assert_eq!(equal_target.gapless, source_gapless);
    }

    #[kithara::test]
    fn apple_gapless_scaling_rounds_source_counts_to_output_rate() {
        let source_gapless = Some(GaplessInfo {
            leading_frames: 1024,
            trailing_frames: 441,
        });
        let track = aac_track(44_100, source_gapless);

        let output_track = track_with_output_domain_gapless(&track, Some(48_000))
            .expect("BUG: output-domain track with fused SRC");

        assert_eq!(
            output_track.gapless,
            Some(GaplessInfo {
                leading_frames: 1115,
                trailing_frames: 480,
            })
        );
    }

    #[kithara::test]
    fn apple_scaled_gapless_trims_composed_resampled_output_domain() {
        const SOURCE_RATE: u32 = 44_100;
        const OUTPUT_RATE: u32 = 48_000;
        const PACKET_COUNT: u64 = 10;
        const PACKET_FRAMES: u32 = 1024;

        let source_gapless = GaplessInfo {
            leading_frames: 1024,
            trailing_frames: 441,
        };
        let source_track = aac_track(SOURCE_RATE, Some(source_gapless));
        let output_track = track_with_output_domain_gapless(&source_track, Some(OUTPUT_RATE))
            .expect("BUG: output-domain track with fused SRC");
        let output_gapless = output_track.gapless.expect("BUG: scaled gapless");
        let frames_per_call = u32::try_from(
            round_scaled_frames(u64::from(PACKET_FRAMES), SOURCE_RATE, OUTPUT_RATE)
                .expect("BUG: scaled packet frame count"),
        )
        .expect("BUG: scaled packet frames fit u32");
        let decoded_frames = u64::from(frames_per_call).saturating_mul(PACKET_COUNT);
        let demuxer = PacketDemuxer {
            track: source_track,
            held: Vec::new(),
            next_index: 0,
            packet_count: PACKET_COUNT,
            packet_frames: PACKET_FRAMES,
            source_rate: SOURCE_RATE,
        };
        let codec = OutputDomainCodec {
            spec: PcmSpec::new(2, NonZeroU32::new(OUTPUT_RATE).expect("test rate")),
            frames_per_call,
            track_info: DecoderTrackInfo {
                gapless: Some(output_gapless),
                ..DecoderTrackInfo::default()
            },
        };
        let mut decoder = ComposedDecoder::new(demuxer, codec, DecoderRuntime::for_test());
        let mut trimmer =
            GaplessTrimmer::from(decoder.track_info().gapless.expect("BUG: decoder gapless"));
        let mut trimmed_frames = 0_u64;
        loop {
            match decoder.next_chunk().expect("BUG: next chunk") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    trimmed_frames =
                        trimmed_frames.saturating_add(output_frames(trimmer.push(chunk)));
                }
                DecoderChunkOutcome::Pending(reason) => panic!("unexpected Pending: {reason:?}"),
                DecoderChunkOutcome::Eof => {
                    trimmed_frames = trimmed_frames.saturating_add(output_frames(trimmer.flush()));
                    break;
                }
            }
        }
        let expected_frames = decoded_frames
            .saturating_sub(output_gapless.leading_frames)
            .saturating_sub(output_gapless.trailing_frames);

        assert_eq!(output_gapless.leading_frames, 1115);
        assert_eq!(output_gapless.trailing_frames, 480);
        assert_eq!(decoded_frames, 11_150);
        assert_eq!(trimmed_frames, expected_frames);
    }
}
