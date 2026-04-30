//! Factory implementation — split out of mod.rs because lib.rs / mod.rs
//! files must stay declaration-only (see `style.no-items-in-lib-or-mod-rs`).
//!
//! See the `factory` module-level docs in `mod.rs` for usage.

use std::{
    io::{Read, Seek},
    sync::{Arc, atomic::AtomicU64},
};

use derivative::Derivative;
use kithara_bufpool::{BytePool, PcmPool};
use kithara_stream::{
    AudioCodec, ContainerFormat, MediaInfo, SegmentLayout, SharedHooks, StreamContext,
};

use super::probe::{
    CodecSelector, ProbeHint, container_from_extension, probe_codec, resolve_codec_container,
};
use crate::{
    Decoder,
    backend::BoxedSource,
    error::{DecodeError, DecodeResult},
    hooks::HookedDecoder,
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
    Apple,
    /// Android `MediaCodec` (Android, requires the `android` feature).
    #[cfg(all(feature = "android", target_os = "android"))]
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
#[derive(Clone, Derivative)]
#[derivative(Default)]
pub struct DecoderConfig {
    /// Which decoder backend to use. See [`DecoderBackend`]. Default
    /// is [`DecoderBackend::Auto`].
    pub backend: DecoderBackend,
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Optional byte buffer pool override.
    ///
    /// Used by backends that need a reusable raw byte buffer (e.g. the
    /// Apple fMP4 reader loads the container into a pooled slice and
    /// slices packets out without per-packet allocations).  When `None`,
    /// the global `kithara_bufpool::byte_pool()` is used.
    pub byte_pool: Option<BytePool>,
    /// File extension hint for Symphonia probe (e.g., "mp3", "aac").
    ///
    /// Used when the container format is not specified, as a hint for auto-detection.
    pub hint: Option<String>,
    /// Reader-side hooks injected into the resulting decoder via
    /// [`HookedDecoder`]. The factory wraps the inner decoder in
    /// `HookedDecoder` whenever this is `Some`. `None` keeps the
    /// inner decoder unwrapped (mock sources, tests).
    pub hooks: Option<SharedHooks>,
    /// Optional PCM buffer pool override.
    ///
    /// When `None`, the global `kithara_bufpool::pcm_pool()` is used.
    pub pcm_pool: Option<PcmPool>,
    /// Stream context for segment/variant metadata.
    pub stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Optional segment-layout handle over the underlying source. When
    /// present, fMP4 AAC / FLAC streams dispatch through the
    /// segment-by-segment demuxer (`Fmp4SegmentDemuxer`) instead of the
    /// whole-stream container parser, side-stepping prefix walks during
    /// forward seek.
    pub segment_layout: Option<Arc<dyn SegmentLayout>>,
    /// Enable gapless playback.
    #[derivative(Default(value = "true"))]
    pub gapless: bool,
    /// Epoch counter for decoder recreation tracking.
    pub epoch: u64,
}

pub(super) fn wrap_with_hooks(
    inner: Box<dyn Decoder>,
    hooks: Option<SharedHooks>,
) -> Box<dyn Decoder> {
    match hooks {
        Some(hooks) => Box::new(HookedDecoder::new(inner, hooks)),
        None => inner,
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
        selector: &CodecSelector,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let source: BoxedSource = Box::new(source);
        Self::dispatch_backend(source, selector, config)
    }

    /// Recreate a decoder on HLS variant switch / format change.
    ///
    /// Thin wrapper over `create_from_media_info`: the caller (audio
    /// pipeline) is responsible for handing in a `base_offset` that
    /// aligns with the container's init region, so the metadata-driven
    /// path is the only correct answer. Failures are propagated
    /// verbatim — no native-probe fallback, since probing mid-segment
    /// bytes at a mismatched offset would silently pick an unrelated
    /// codec (e.g. match MP3 frame sync in raw AAC-in-fMP4 bytes) and
    /// drift `session.media_info` out of sync with the decoder it
    /// actually got.
    ///
    /// # Errors
    ///
    /// Returns the decoder-creation error verbatim when the metadata-
    /// driven path cannot build a decoder (unsupported codec, mismatch
    /// between the declared container and the bytes at `base_offset`,
    /// etc.).
    pub fn create_for_recreate<R, F>(
        make_source: F,
        media_info: &MediaInfo,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn Decoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
        F: Fn() -> R,
    {
        Self::create_from_media_info(make_source(), media_info, config).inspect_err(|error| {
            tracing::warn!(
                ?error,
                ?media_info,
                "create_from_media_info failed during recreate"
            );
        })
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

        Self::create(source, &CodecSelector::Probe(hint), config)
    }

    /// Create decoder from a file-extension hint.
    ///
    /// # Errors
    ///
    /// Returns `DecodeError::ProbeFailed` when the hint is missing or too
    /// weak to pick a codec, and `DecodeError::*` for backend failures.
    /// No fallback — callers must supply a usable hint.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "by-value lets callers pass DecoderConfig::default() without explicit borrow"
    )]
    pub fn create_with_probe<R>(
        source: R,
        hint: Option<&str>,
        config: DecoderConfig,
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
        Self::create(source, &CodecSelector::Probe(probe_hint), &config)
    }

    pub(super) fn dispatch_backend(
        source: BoxedSource,
        selector: &CodecSelector,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn Decoder>> {
        let (codec, container) = resolve_codec_container(selector)?;

        tracing::debug!(
            ?codec,
            ?container,
            backend = ?config.backend,
            "DecoderFactory::create called"
        );

        let inner = match config.backend {
            #[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
            DecoderBackend::Apple => create_apple(source, codec, container, config),
            #[cfg(all(feature = "android", target_os = "android"))]
            DecoderBackend::Android => create_android(source, codec, container, config),
            #[cfg(feature = "symphonia")]
            DecoderBackend::Symphonia => create_symphonia(source, codec, container, config),
        }?;

        Ok(wrap_with_hooks(inner, config.hooks.clone()))
    }
}

#[cfg(all(feature = "apple", any(target_os = "macos", target_os = "ios")))]
fn create_apple(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    #[cfg(feature = "symphonia")]
    if should_use_segment_aware(codec, container, config)
        && let Some(layout) = config.segment_layout.clone()
    {
        if crate::codec::AppleCodec::supports(codec) {
            return create_fmp4_segment_apple(source, codec, layout, config);
        }
        return create_fmp4_segment_symphonia(source, codec, layout, config);
    }
    // Apple HW only owns the segment-aware fMP4 path. Anything else
    // (file MP3, raw FLAC, WAV, etc.) falls through to the Symphonia
    // path — the same software stack that owns non-Apple platforms.
    #[cfg(feature = "symphonia")]
    return create_symphonia(source, codec, container, config);
    #[cfg(not(feature = "symphonia"))]
    {
        let _ = (source, codec, container, config);
        Err(DecodeError::UnsupportedCodec(codec))
    }
}

#[cfg(all(
    feature = "apple",
    feature = "symphonia",
    any(target_os = "macos", target_os = "ios")
))]
fn create_fmp4_segment_apple(
    source: BoxedSource,
    codec: AudioCodec,
    layout: Arc<dyn SegmentLayout>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::{
        FrameCodec, UniversalDecoder,
        codec::AppleCodec,
        demuxer::{Demuxer, Fmp4SegmentDemuxer},
    };

    tracing::debug!(
        ?codec,
        "fmp4_segment: dispatching to segment-aware Apple HW codec path"
    );
    let demuxer = Fmp4SegmentDemuxer::open(source, layout)?;
    let codec = AppleCodec::open(demuxer.track_info())?;
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| kithara_bufpool::pcm_pool().clone());
    let decoder = UniversalDecoder::new(
        demuxer,
        codec,
        pool,
        config.epoch,
        config.byte_len_handle.clone(),
        config.stream_ctx.clone(),
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
    #[cfg(feature = "symphonia")]
    if should_use_segment_aware(codec, container, config)
        && let Some(layout) = config.segment_layout.clone()
    {
        if crate::codec::AndroidCodec::supports(codec) {
            return create_fmp4_segment_android(source, codec, layout, config);
        }
        return create_fmp4_segment_symphonia(source, codec, layout, config);
    }
    // Android HW only owns the segment-aware fMP4 path. Everything else
    // falls through to the Symphonia path. (`feature = "android"`
    // implies `feature = "symphonia"` in the workspace lock; we don't
    // build an Android target without Symphonia.)
    create_symphonia(source, codec, container, config)
}

#[cfg(all(feature = "android", feature = "symphonia", target_os = "android"))]
fn create_fmp4_segment_android(
    source: BoxedSource,
    codec: AudioCodec,
    layout: Arc<dyn SegmentLayout>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::{
        FrameCodec, UniversalDecoder,
        codec::AndroidCodec,
        demuxer::{Demuxer, Fmp4SegmentDemuxer},
    };

    tracing::debug!(
        ?codec,
        "fmp4_segment: dispatching to segment-aware Android HW codec path"
    );
    let demuxer = Fmp4SegmentDemuxer::open(source, layout)?;
    let codec = AndroidCodec::open(demuxer.track_info())?;
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| kithara_bufpool::pcm_pool().clone());
    let decoder = UniversalDecoder::new(
        demuxer,
        codec,
        pool,
        config.epoch,
        config.byte_len_handle.clone(),
        config.stream_ctx.clone(),
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
    if crate::SymphoniaCodec::supports(codec) {
        return create_file_symphonia_universal(source, codec, container, config);
    }
    // PCM / ADPCM / Opus still need bit-depth + endianness or workspace-
    // level workarounds the generic `AudioCodec` enum does not encode —
    // those tracks fall through to the legacy whole-stream decoder.
    create_legacy_symphonia(source, codec, container, config)
}

#[cfg(feature = "symphonia")]
fn create_legacy_symphonia(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::symphonia::{config::SymphoniaConfig, decoder::SymphoniaDecoder};

    if matches!(codec, AudioCodec::Opus | AudioCodec::Adpcm) {
        return Err(DecodeError::UnsupportedCodec(codec));
    }

    let symphonia_config = SymphoniaConfig {
        container,
        verify: false,
        gapless: config.gapless,
        byte_len_handle: config.byte_len_handle.clone(),
        hint: config.hint.clone(),
        stream_ctx: config.stream_ctx.clone(),
        epoch: config.epoch,
        pcm_pool: config.pcm_pool.clone(),
    };
    tracing::debug!(?codec, ?container, "create_legacy_symphonia (PCM path)");
    Ok(Box::new(SymphoniaDecoder::new(source, &symphonia_config)?))
}

#[cfg(feature = "symphonia")]
fn create_file_symphonia_universal(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn Decoder>> {
    use crate::{FrameCodec, SymphoniaCodec, SymphoniaDemuxer, UniversalDecoder, demuxer::Demuxer};

    tracing::debug!(
        ?codec,
        ?container,
        "file-symphonia: dispatching to UniversalDecoder<SymphoniaDemuxer, SymphoniaCodec>"
    );
    let (demuxer, _byte_len) = SymphoniaDemuxer::open_file(
        source,
        config.hint.clone(),
        container,
        config.byte_len_handle.clone(),
    )?;
    let codec = SymphoniaCodec::open(demuxer.track_info())?;
    let pool = config
        .pcm_pool
        .clone()
        .unwrap_or_else(|| kithara_bufpool::pcm_pool().clone());
    let decoder = UniversalDecoder::new(
        demuxer,
        codec,
        pool,
        config.epoch,
        config.byte_len_handle.clone(),
        config.stream_ctx.clone(),
    );
    Ok(Box::new(decoder))
}

/// Gate for the segment-aware fMP4 path. Routes AAC / FLAC fMP4 with a
/// surfaced `SegmentedSource` (HLS) through `Fmp4SegmentDecoder`. File
/// sources without segment metadata fall through to the legacy
/// `IsoMp4Reader` path.
#[cfg(feature = "symphonia")]
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
    use crate::{
        FrameCodec, SymphoniaCodec, UniversalDecoder,
        demuxer::{Demuxer, Fmp4SegmentDemuxer},
    };

    tracing::debug!(
        ?codec,
        "fmp4_segment: dispatching to segment-aware Symphonia path"
    );
    match codec {
        AudioCodec::AacLc | AudioCodec::Flac => {
            let demuxer = Fmp4SegmentDemuxer::open(source, layout)?;
            let codec = SymphoniaCodec::open(demuxer.track_info())?;
            let pool = config
                .pcm_pool
                .clone()
                .unwrap_or_else(|| kithara_bufpool::pcm_pool().clone());
            let decoder = UniversalDecoder::new(
                demuxer,
                codec,
                pool,
                config.epoch,
                config.byte_len_handle.clone(),
                config.stream_ctx.clone(),
            );
            Ok(Box::new(decoder))
        }
        other => Err(DecodeError::UnsupportedCodec(other)),
    }
}
