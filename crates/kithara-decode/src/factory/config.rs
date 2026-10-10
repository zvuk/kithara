use std::{num::NonZeroU32, sync::atomic::AtomicU64};

use kithara_bufpool::PoolRegion;
use kithara_config::Config;
use kithara_platform::sync::Arc;
use kithara_resampler::{NoResamplerBackend, ResamplerBackend, ResamplerOptions, ResamplerQuality};
use kithara_stream::{BoxedEventSink, ByteMap};

use super::inner::DecoderBackend;

/// Decoder-side resampler selected by the caller.
///
/// This describes conversion that is part of decoder construction, not the
/// playback graph's effects chain. Backend choice is encoded by `B`.
#[derive(Clone, Config)]
#[config(fields(value))]
#[non_exhaustive]
#[derive(derive_more::Debug)]
#[debug(bound(B: ResamplerBackend))]
pub struct DecoderResamplerConfig<B = NoResamplerBackend> {
    #[config(skip = "resampler backend strategy")]
    #[debug("{:?}", self.backend.name())]
    pub backend: B,
    pub target_sample_rate: NonZeroU32,
    #[config(nested, builder(default))]
    pub options: ResamplerOptions,
    #[config(builder(default))]
    pub quality: ResamplerQuality,
}

/// Configuration for `DecoderFactory`.
#[derive(Config)]
#[config(construction, builder(state_mod(vis = "pub")))]
#[non_exhaustive]
pub struct DecoderConfig<B, S> {
    /// Which decoder backend to use. See [`DecoderBackend`].
    #[config(value, builder(default))]
    pub(crate) backend: DecoderBackend,
    /// Handle for dynamic byte length updates (HLS).
    #[config(skip = "transferred to the decoder")]
    pub(crate) byte_len_handle: Option<Arc<AtomicU64>>,
    /// Optional byte-map handle over the underlying source.
    #[config(skip = "transferred to the decoder")]
    pub(crate) byte_map: Option<Arc<dyn ByteMap>>,
    /// File extension hint for Symphonia probe (e.g., "mp3", "aac").
    #[cfg(feature = "symphonia")]
    #[config(skip = "consumed by the decoder probe", builder(into))]
    pub(crate) hint: Option<String>,
    /// Reader-side observer hooks. Single-owner; moved into
    /// [`crate::composed::ComposedDecoder`] by the chosen backend path.
    #[config(skip = "transferred to the composed decoder")]
    pub(crate) hooks: Option<BoxedEventSink>,
    /// Optional decoder-side resampler plan. `None` means the decoder emits
    /// at the source rate.
    #[config(skip = "transferred to the resampled decoder")]
    pub(crate) resampler: Option<DecoderResamplerConfig<B>>,
    /// Shared typed buffer-pool facade propagated from the host.
    #[config(skip = "transferred to decoder owners")]
    pub(crate) pools: PoolRegion<S>,
    /// Enable gapless trim wiring through the per-backend codec.
    #[config(value, builder(default = true))]
    pub(crate) gapless: bool,
}

#[cfg(test)]
mod tests {
    #[cfg(apple_backend)]
    use std::sync::atomic::AtomicU64;

    use kithara_test_utils::kithara;

    use super::*;
    use crate::test_pools::{TestPools, pools};

    type TestDecoderConfig = DecoderConfig<NoResamplerBackend, TestPools>;

    #[kithara::test]
    fn decoder_config_selects_the_expected_backend() {
        let config: TestDecoderConfig = TestDecoderConfig::builder().pools(pools()).build();
        #[cfg(android_backend)]
        let expected = DecoderBackend::Android;
        #[cfg(apple_backend)]
        let expected = DecoderBackend::Apple;
        #[cfg(all(target_arch = "wasm32", feature = "webcodecs"))]
        let expected = DecoderBackend::WebCodecs;
        #[cfg(not(any(
            apple_backend,
            android_backend,
            all(target_arch = "wasm32", feature = "webcodecs")
        )))]
        let expected = DecoderBackend::Symphonia;
        assert_eq!(config.backend, expected);
        assert!(config.byte_len_handle.is_none());
    }

    #[cfg(apple_backend)]
    #[kithara::test]
    fn decoder_config_custom_apple_backend_preserves_fields() {
        let handle = Arc::new(AtomicU64::new(1000));
        let builder = TestDecoderConfig::builder()
            .pools(pools())
            .backend(DecoderBackend::Apple)
            .byte_len_handle(Arc::clone(&handle));
        #[cfg(feature = "symphonia")]
        let builder = builder.hint("mp3");
        let config: TestDecoderConfig = builder.build();
        assert_eq!(config.backend, DecoderBackend::Apple);
        assert!(config.byte_len_handle.is_some());
        #[cfg(feature = "symphonia")]
        assert_eq!(config.hint, Some("mp3".to_string()));
    }
}
