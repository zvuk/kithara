//! Apple `AudioToolbox` implementation of [`Backend`].

use kithara_stream::{AudioCodec, ContainerFormat};

use super::{AppleConfig, decoder::AppleDecoder};
use crate::{
    DecodeError, DecoderConfig,
    backend::{Backend, BoxedSource},
};

fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
    match codec {
        AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => Some(ContainerFormat::Adts),
        AudioCodec::Mp3 => Some(ContainerFormat::MpegAudio),
        AudioCodec::Flac => Some(ContainerFormat::Flac),
        AudioCodec::Alac => Some(ContainerFormat::Fmp4),
        AudioCodec::Pcm => Some(ContainerFormat::Wav),
        _ => None,
    }
}

fn can_seek_container(container: ContainerFormat) -> bool {
    // Apple handles every container our pipelines deliver. Fragmented
    // MP4 is driven by the incremental `Fmp4Reader` — init-only open
    // plus per-fragment bodies — which fits streaming HLS sources
    // without materialising the whole track up front. Frame-based
    // formats (ADTS, MP3, FLAC) and the remaining atom-aware ones
    // (non-fragmented MP4, CAF, WAV) stay on the `AudioFile` path.
    matches!(
        container,
        ContainerFormat::MpegAudio
            | ContainerFormat::Adts
            | ContainerFormat::Flac
            | ContainerFormat::Fmp4
            | ContainerFormat::Mp4
            | ContainerFormat::Caf
            | ContainerFormat::Wav
    )
}

fn supports_codec(codec: AudioCodec) -> bool {
    matches!(
        codec,
        AudioCodec::AacLc
            | AudioCodec::AacHe
            | AudioCodec::AacHeV2
            | AudioCodec::Mp3
            | AudioCodec::Flac
            | AudioCodec::Alac
            | AudioCodec::Pcm
    )
}

impl Backend for AppleDecoder {
    fn supports(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
        if !supports_codec(codec) {
            return false;
        }
        container
            .or_else(|| default_container_for_codec(codec))
            .is_some_and(can_seek_container)
    }

    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Self, DecodeError> {
        if !supports_codec(codec) {
            return Err(DecodeError::UnsupportedCodec(codec));
        }
        let resolved = container.or_else(|| default_container_for_codec(codec));
        let apple_config = AppleConfig {
            byte_len_handle: config.byte_len_handle.clone(),
            container: resolved,
            pcm_pool: config.pcm_pool.clone(),
            byte_pool: config.byte_pool.clone(),
            stream_ctx: config.stream_ctx.clone(),
            epoch: config.epoch,
        };

        Self::new(source, &apple_config)
    }
}
