use std::sync::OnceLock;

use symphonia::core::codecs::registry::CodecRegistry;

static CODEC_REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();

pub(crate) fn get_codecs() -> &'static CodecRegistry {
    CODEC_REGISTRY.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        #[cfg(feature = "fdk-aac")]
        registry.register_audio_decoder::<crate::symphonia::aac_fdk::AacDecoder>();
        registry
    })
}
