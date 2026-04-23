//! Apple `AudioToolbox` implementation of [`HardwareBackend`].

use kithara_stream::{AudioCodec, ContainerFormat};

use super::{AppleConfig, try_create_apple_decoder};
use crate::{
    DecoderConfig, InnerDecoder,
    backend::{BoxedSource, HardwareBackend, RecoverableHardwareError},
};

pub(crate) struct AppleBackend;

impl HardwareBackend for AppleBackend {
    fn supports_codec(codec: AudioCodec) -> bool {
        matches!(
            codec,
            AudioCodec::AacLc
                | AudioCodec::AacHe
                | AudioCodec::AacHeV2
                | AudioCodec::Mp3
                | AudioCodec::Flac
                | AudioCodec::Alac
        )
    }

    fn can_seek_container(container: ContainerFormat) -> bool {
        // Frame-based formats have self-delimiting sync patterns.
        // Structured containers (fMP4, MPEG-TS, CAF) require
        // atom-boundary awareness that AudioFileStream lacks.
        matches!(
            container,
            ContainerFormat::MpegAudio | ContainerFormat::Adts | ContainerFormat::Flac
        )
    }

    fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
        match codec {
            AudioCodec::AacLc | AudioCodec::AacHe | AudioCodec::AacHeV2 => {
                Some(ContainerFormat::Adts)
            }
            AudioCodec::Mp3 => Some(ContainerFormat::MpegAudio),
            AudioCodec::Flac => Some(ContainerFormat::Flac),
            AudioCodec::Alac => Some(ContainerFormat::Fmp4),
            _ => None,
        }
    }

    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        let apple_config = AppleConfig {
            byte_len_handle: config.byte_len_handle.clone(),
            container,
            pcm_pool: config.pcm_pool.clone(),
        };

        try_create_apple_decoder(codec, source, &apple_config)
    }
}
