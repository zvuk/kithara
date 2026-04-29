//! Symphonia implementation of [`Backend`].
//!
//! Symphonia is the universal software backend: it accepts every
//! `AudioCodec` the workspace supports except `Opus` and `Adpcm`, and
//! probes the container from raw bytes — so the caller may pass
//! `container = None`.

use kithara_stream::{AudioCodec, ContainerFormat};

use super::{config::SymphoniaConfig, decoder::SymphoniaDecoder};
use crate::{
    DecoderConfig,
    backend::{Backend, BoxedSource},
    error::DecodeError,
};

/// Build a `SymphoniaConfig` from `DecoderConfig` with an optional container hint.
fn symphonia_config_from(
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> SymphoniaConfig {
    SymphoniaConfig {
        container,
        verify: false,
        gapless: config.gapless,
        byte_len_handle: config.byte_len_handle.clone(),
        hint: config.hint.clone(),
        stream_ctx: config.stream_ctx.clone(),
        epoch: config.epoch,
        pcm_pool: config.pcm_pool.clone(),
    }
}

impl Backend for SymphoniaDecoder {
    fn supports(codec: AudioCodec, _container: Option<ContainerFormat>) -> bool {
        !matches!(codec, AudioCodec::Opus | AudioCodec::Adpcm)
    }

    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Self, DecodeError> {
        tracing::debug!(?codec, ?container, hint = ?config.hint, "Using Symphonia decoder");

        if matches!(codec, AudioCodec::Opus | AudioCodec::Adpcm) {
            return Err(DecodeError::UnsupportedCodec(codec));
        }

        Self::new(source, &symphonia_config_from(container, config))
    }
}
