//! Custom Symphonia [`CodecRegistry`] used by [`crate::symphonia::SymphoniaCodec`].
//!
//! Built once on first access. Starts from Symphonia's default codec set.
//! When the `fdk-aac` feature is enabled, the AAC entry is overridden with
//! [`crate::symphonia::aac_fdk::AacDecoder`] to support HE-AAC v1/v2 — the
//! default `symphonia-codec-aac` is LC-only and rejects SBR/PS with
//! `"aac too complex"`. Our in-tree adapter also strips fdk-aac's
//! algorithmic delay (`stream_info.outputDelay`), which the upstream
//! `symphonia-adapter-fdk-aac` 0.2.0 emits as ~36 ms of leading silence
//! on every AAC track.

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
