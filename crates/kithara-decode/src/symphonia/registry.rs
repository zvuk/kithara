//! Custom Symphonia [`CodecRegistry`] used by [`crate::symphonia::SymphoniaCodec`].
//!
//! Built once on first access. Starts from Symphonia's default codec set.
//! When the `fdk-aac` feature is enabled, the AAC entry is overridden with
//! [`symphonia_adapter_fdk_aac::AacDecoder`] to support HE-AAC v1/v2 — the
//! default `symphonia-codec-aac` is LC-only and rejects SBR/PS with
//! `"aac too complex"`.

use std::sync::OnceLock;

use symphonia::core::codecs::registry::CodecRegistry;

static CODEC_REGISTRY: OnceLock<CodecRegistry> = OnceLock::new();

pub(crate) fn get_codecs() -> &'static CodecRegistry {
    CODEC_REGISTRY.get_or_init(|| {
        let mut registry = CodecRegistry::new();
        symphonia::default::register_enabled_codecs(&mut registry);
        #[cfg(feature = "fdk-aac")]
        registry.register_audio_decoder::<symphonia_adapter_fdk_aac::AacDecoder>();
        registry
    })
}
