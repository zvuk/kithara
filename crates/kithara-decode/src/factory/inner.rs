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
use kithara_stream::{MediaInfo, SharedHooks, StreamContext};

use super::{
    hardware::{HardwareAttempt, try_hardware_decoder},
    probe::{
        CodecSelector, ProbeHint, container_from_extension, probe_codec, resolve_codec_container,
    },
};
use crate::{
    InnerDecoder,
    backend::{BoxedSource, HardwareBackend, current::Current},
    error::{DecodeError, DecodeResult},
    hooks::HookedDecoder,
};

/// Configuration for `DecoderFactory`.
#[derive(Clone, Derivative)]
#[derivative(Default)]
pub struct DecoderConfig {
    /// Optional byte buffer pool override.
    ///
    /// Used by backends that need a reusable raw byte buffer (e.g. the
    /// Apple fMP4 reader loads the container into a pooled slice and
    /// slices packets out without per-packet allocations).  When `None`,
    /// the global `kithara_bufpool::byte_pool()` is used.
    pub byte_pool: Option<BytePool>,
    /// Handle for dynamic byte length updates (HLS).
    pub byte_len_handle: Option<Arc<AtomicU64>>,
    /// Reader-side hooks injected into the resulting decoder via
    /// [`HookedDecoder`]. The factory wraps the inner decoder in
    /// `HookedDecoder` whenever this is `Some`. `None` keeps the
    /// inner decoder unwrapped (mock sources, tests).
    pub hooks: Option<SharedHooks>,
    /// File extension hint for Symphonia probe (e.g., "mp3", "aac").
    ///
    /// Used when the container format is not specified, as a hint for auto-detection.
    pub hint: Option<String>,
    /// Optional PCM buffer pool override.
    ///
    /// When `None`, the global `kithara_bufpool::pcm_pool()` is used.
    pub pcm_pool: Option<PcmPool>,
    /// Stream context for segment/variant metadata.
    pub stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Epoch counter for decoder recreation tracking.
    pub epoch: u64,
    /// Enable gapless playback.
    #[derivative(Default(value = "true"))]
    pub gapless: bool,
    /// Route decoding through the hardware backend (Apple `AudioToolbox`
    /// or Android `MediaCodec`). When `false`, the factory uses Symphonia
    /// (requires the `symphonia` feature). There is no fallback between
    /// the two paths.
    pub prefer_hardware: bool,
}

pub(super) fn wrap_with_hooks(
    inner: Box<dyn InnerDecoder>,
    hooks: Option<SharedHooks>,
) -> Box<dyn InnerDecoder> {
    match hooks {
        Some(hooks) => Box::new(HookedDecoder::new(inner, hooks)),
        None => inner,
    }
}

/// Factory for creating decoders with a single, strict backend selection.
///
/// Backend matrix:
/// - `prefer_hardware == true` → hardware backend (Apple/Android). Fails if
///   the codec/container is unsupported or decoder construction errors.
/// - `prefer_hardware == false` → Symphonia (requires `feature = "symphonia"`).
///   Without the feature the call fails with `UnsupportedCodec`.
pub struct DecoderFactory;

impl DecoderFactory {
    /// Create a decoder with the single selected backend.
    pub(crate) fn create<R>(
        source: R,
        selector: &CodecSelector,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>>
    where
        R: Read + Seek + Send + Sync + 'static,
    {
        let source: BoxedSource = Box::new(source);
        Self::create_with_backend::<Current>(source, selector, config)
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
    ) -> DecodeResult<Box<dyn InnerDecoder>>
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
    ) -> DecodeResult<Box<dyn InnerDecoder>>
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
    ) -> DecodeResult<Box<dyn InnerDecoder>>
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

    pub(super) fn create_with_backend<B: HardwareBackend>(
        source: BoxedSource,
        selector: &CodecSelector,
        config: &DecoderConfig,
    ) -> DecodeResult<Box<dyn InnerDecoder>> {
        let (codec, container) = resolve_codec_container(selector)?;

        tracing::debug!(
            ?codec,
            ?container,
            prefer_hardware = config.prefer_hardware,
            "DecoderFactory::create called"
        );

        let inner = if config.prefer_hardware {
            match try_hardware_decoder::<B>(source, codec, container, config) {
                HardwareAttempt::Decoded(decoder) => Ok(decoder),
                HardwareAttempt::Skipped => {
                    tracing::warn!(
                        ?codec,
                        ?container,
                        "hardware backend does not support codec/container; \
                         no fallback when prefer_hardware=true"
                    );
                    Err(DecodeError::UnsupportedCodec(codec))
                }
                HardwareAttempt::Failed(err) => Err(err),
            }
        } else {
            #[cfg(feature = "symphonia")]
            {
                crate::symphonia::create_from_boxed(source, codec, container, config)
            }
            #[cfg(not(feature = "symphonia"))]
            {
                let _ = (source, container);
                tracing::warn!(
                    ?codec,
                    "symphonia feature disabled; software decoder unavailable"
                );
                Err(DecodeError::UnsupportedCodec(codec))
            }
        }?;

        Ok(wrap_with_hooks(inner, config.hooks.clone()))
    }
}
