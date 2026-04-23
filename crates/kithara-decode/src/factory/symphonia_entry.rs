//! Symphonia-backed decoder construction (direct and probe paths).

use std::io::{Read, Seek};

use kithara_stream::{AudioCodec, ContainerFormat};

use super::DecoderConfig;
use crate::{
    InnerDecoder,
    backend::BoxedSource,
    error::{DecodeError, DecodeResult},
    symphonia::{
        Symphonia, SymphoniaAac, SymphoniaConfig, SymphoniaFlac, SymphoniaMp3, SymphoniaVorbis,
    },
    traits::{Alac, AudioDecoder, CodecType},
};

/// Marker used only as a generic placeholder when Symphonia auto-probes.
struct Pcm;
impl CodecType for Pcm {
    const CODEC: AudioCodec = AudioCodec::Pcm;
}

/// Marker used when the real codec is determined by Symphonia's native probe.
struct ProbeAny;
impl CodecType for ProbeAny {
    const CODEC: AudioCodec = AudioCodec::Pcm;
}

/// Create a Symphonia decoder from an already-boxed source with known codec and container.
pub(super) fn create_symphonia_from_boxed(
    source: BoxedSource,
    codec: AudioCodec,
    container: Option<ContainerFormat>,
    config: &DecoderConfig,
) -> DecodeResult<Box<dyn InnerDecoder>> {
    tracing::debug!(?container, hint = ?config.hint, "Using Symphonia decoder");

    let symphonia_config = SymphoniaConfig {
        verify: false,
        gapless: config.gapless,
        byte_len_handle: config.byte_len_handle.clone(),
        container,
        hint: config.hint.clone(),
        stream_ctx: config.stream_ctx.clone(),
        epoch: config.epoch,
        pcm_pool: config.pcm_pool.clone(),
        ..Default::default()
    };

    create_symphonia_decoder(source, codec, symphonia_config)
}

/// Create a Symphonia decoder for `codec`.
fn create_symphonia_decoder(
    source: BoxedSource,
    codec: AudioCodec,
    config: SymphoniaConfig,
) -> DecodeResult<Box<dyn InnerDecoder>> {
    match codec {
        AudioCodec::Mp3 => {
            let decoder = SymphoniaMp3::create(source, config)?;
            Ok(Box::new(decoder))
        }
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
            let decoder = SymphoniaAac::create(source, config)?;
            Ok(Box::new(decoder))
        }
        AudioCodec::Flac => {
            let decoder = SymphoniaFlac::create(source, config)?;
            Ok(Box::new(decoder))
        }
        AudioCodec::Vorbis => {
            let decoder = SymphoniaVorbis::create(source, config)?;
            Ok(Box::new(decoder))
        }
        AudioCodec::Alac => {
            let decoder = Symphonia::<Alac>::create(source, config)?;
            Ok(Box::new(decoder))
        }
        AudioCodec::Pcm => {
            let decoder = Symphonia::<Pcm>::create(source, config)?;
            Ok(Box::new(decoder))
        }
        AudioCodec::Opus | AudioCodec::Adpcm => Err(DecodeError::UnsupportedCodec(codec)),
    }
}

/// Let Symphonia probe the data itself (no codec hints applied).
pub(super) fn create_with_symphonia_probe<R>(
    source: R,
    config: DecoderConfig,
) -> DecodeResult<Box<dyn InnerDecoder>>
where
    R: Read + Seek + Send + Sync + 'static,
{
    tracing::debug!("Attempting Symphonia native probe (no codec hints)");

    let symphonia_config = SymphoniaConfig {
        verify: false,
        gapless: config.gapless,
        byte_len_handle: config.byte_len_handle,
        container: None,
        hint: config.hint,
        probe_no_seek: true,
        stream_ctx: config.stream_ctx.clone(),
        epoch: config.epoch,
        pcm_pool: config.pcm_pool,
    };

    let source: Box<dyn crate::traits::DecoderInput> = Box::new(source);
    let decoder = Symphonia::<ProbeAny>::create(source, symphonia_config)?;
    Ok(Box::new(decoder))
}
