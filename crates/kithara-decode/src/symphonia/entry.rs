//! Symphonia-backed decoder construction entry point.
//!
//! Called from `DecoderFactory` via the single `#[cfg(feature = "symphonia")]`
//! integration site — all config assembly lives here, inside the
//! `symphonia` module.

use kithara_stream::{AudioCodec, ContainerFormat};

use super::{Symphonia, SymphoniaConfig};
use crate::{
    DecoderConfig, InnerDecoder,
    backend::BoxedSource,
    error::{DecodeError, DecodeResult},
};

/// Build a `SymphoniaConfig` from `DecoderConfig` with an optional container hint.
fn symphonia_config_from(
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> SymphoniaConfig {
    SymphoniaConfig {
        verify: false,
        gapless: config.gapless,
        byte_len_handle: config.byte_len_handle.clone(),
        container,
        hint: config.hint.clone(),
        stream_ctx: config.stream_ctx.clone(),
        epoch: config.epoch,
        pcm_pool: config.pcm_pool.clone(),
    }
}

/// Create a Symphonia decoder from an already-boxed source with known codec/container.
pub(crate) fn create_from_boxed(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn InnerDecoder>> {
    tracing::debug!(?codec, ?container, hint = ?config.hint, "Using Symphonia decoder");

    match codec {
        AudioCodec::Opus | AudioCodec::Adpcm => return Err(DecodeError::UnsupportedCodec(codec)),
        _ => {}
    }

    Ok(Box::new(Symphonia::new(
        source,
        &symphonia_config_from(container, config),
    )?))
}

/// Create a Symphonia decoder by letting it probe the bitstream directly.
///
/// Used as recreation-time robustness: when the metadata-driven path fails
/// (typically at ABR variant hops where the cached `MediaInfo` no longer
/// matches the bytes at `base_offset`), Symphonia's native probe reads the
/// bitstream and picks the format itself.
pub(crate) fn create_probed_from_boxed(
    source: BoxedSource,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn InnerDecoder>> {
    tracing::debug!(hint = ?config.hint, "Using Symphonia native probe");

    Ok(Box::new(Symphonia::new(
        source,
        &symphonia_config_from(None, config),
    )?))
}
