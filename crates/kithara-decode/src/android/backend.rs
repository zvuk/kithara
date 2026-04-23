//! Android `MediaCodec` implementation of [`HardwareBackend`].

use kithara_stream::{AudioCodec, ContainerFormat};

use super::{
    AndroidConfig, can_seek_container as android_can_seek_container,
    default_container_for_codec as android_default_container_for_codec,
    supports_codec as android_supports_codec, try_create_android_decoder,
};
use crate::{
    DecoderConfig, InnerDecoder,
    backend::{BoxedSource, HardwareBackend, RecoverableHardwareError},
};

pub(crate) struct AndroidBackend;

impl HardwareBackend for AndroidBackend {
    fn supports_codec(codec: AudioCodec) -> bool {
        android_supports_codec(codec)
    }

    fn can_seek_container(container: ContainerFormat) -> bool {
        android_can_seek_container(container)
    }

    fn default_container_for_codec(codec: AudioCodec) -> Option<ContainerFormat> {
        android_default_container_for_codec(codec)
    }

    fn try_create(
        source: BoxedSource,
        config: &DecoderConfig,
        codec: AudioCodec,
        container: Option<ContainerFormat>,
    ) -> Result<Box<dyn InnerDecoder>, RecoverableHardwareError> {
        let android_config = AndroidConfig {
            byte_len_handle: config.byte_len_handle.clone(),
            container,
            pcm_pool: config.pcm_pool.clone(),
            stream_ctx: config.stream_ctx.clone(),
            epoch: config.epoch,
        };

        try_create_android_decoder(codec, source, android_config)
    }
}
