//! Android `MediaCodec` implementation of [`Backend`].

use kithara_stream::{AudioCodec, ContainerFormat};

use super::{
    AndroidConfig, can_seek_container as android_can_seek_container, decoder::AndroidDecoder,
    default_container_for_codec as android_default_container_for_codec, error::AndroidBackendError,
    supports_codec as android_supports_codec,
};
use crate::{
    DecodeError, DecoderConfig,
    backend::{Backend, BoxedSource},
};

impl Backend for AndroidDecoder {
    fn supports(codec: AudioCodec, container: Option<ContainerFormat>) -> bool {
        if !android_supports_codec(codec) {
            return false;
        }
        match container.or_else(|| android_default_container_for_codec(codec)) {
            Some(c) => android_can_seek_container(c),
            None => false,
        }
    }

    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Self, DecodeError> {
        match codec {
            AudioCodec::AacLc
            | AudioCodec::AacHe
            | AudioCodec::AacHeV2
            | AudioCodec::Mp3
            | AudioCodec::Flac
            | AudioCodec::Pcm => {}
            unsupported => {
                return Err(AndroidBackendError::UnsupportedCodec { codec: unsupported }
                    .into_decode_error());
            }
        }

        let resolved = container.or_else(|| android_default_container_for_codec(codec));
        let android_config = AndroidConfig {
            byte_len_handle: config.byte_len_handle.clone(),
            container: resolved,
            pcm_pool: config.pcm_pool.clone(),
            stream_ctx: config.stream_ctx.clone(),
            epoch: config.epoch,
        };

        AndroidDecoder::bootstrap(source, android_config, codec)
    }
}
