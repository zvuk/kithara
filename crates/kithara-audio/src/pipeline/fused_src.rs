use std::sync::{Arc, atomic::AtomicU32};

use kithara_decode::{DecoderBackend, ResamplerOptions, ResamplerQuality};

use crate::pipeline::config::ResamplerStage;

pub(crate) fn decoder_target_output_rate(
    backend: DecoderBackend,
    host_sample_rate: &Arc<AtomicU32>,
) -> Option<Arc<AtomicU32>> {
    fused_src_active(backend).then(|| Arc::clone(host_sample_rate))
}

pub(crate) fn resampler_stage(
    backend: DecoderBackend,
    quality: ResamplerQuality,
    options: ResamplerOptions,
) -> ResamplerStage {
    if fused_src_active(backend) {
        ResamplerStage::Absent
    } else {
        ResamplerStage::Present { quality, options }
    }
}

#[cfg(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
))]
fn fused_src_active(backend: DecoderBackend) -> bool {
    matches!(backend, DecoderBackend::Apple)
}

#[cfg(not(all(
    feature = "apple-fused-src",
    any(target_os = "macos", target_os = "ios")
)))]
fn fused_src_active(_backend: DecoderBackend) -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[cfg(all(
        feature = "apple-fused-src",
        any(target_os = "macos", target_os = "ios")
    ))]
    use std::{num::NonZeroU32, sync::atomic::Ordering};

    use super::*;

    #[cfg(all(
        feature = "apple-fused-src",
        any(target_os = "macos", target_os = "ios")
    ))]
    fn current_target_output_rate(rate: Option<&Arc<AtomicU32>>) -> Option<u32> {
        rate.and_then(|rate| NonZeroU32::new(rate.load(Ordering::Acquire)).map(NonZeroU32::get))
    }

    #[cfg(all(
        feature = "apple-fused-src",
        any(target_os = "macos", target_os = "ios")
    ))]
    #[test]
    fn apple_backend_uses_fused_output_rate_and_omits_stage() {
        let host_rate = Arc::new(AtomicU32::new(48_000));

        let target = decoder_target_output_rate(DecoderBackend::Apple, &host_rate);

        assert_eq!(current_target_output_rate(target.as_ref()), Some(48_000));
        assert_eq!(
            resampler_stage(
                DecoderBackend::Apple,
                ResamplerQuality::default(),
                ResamplerOptions::default()
            ),
            ResamplerStage::Absent
        );
    }

    #[cfg(feature = "symphonia")]
    #[test]
    fn non_apple_backend_keeps_resampler_stage() {
        let host_rate = Arc::new(AtomicU32::new(48_000));

        let target = decoder_target_output_rate(DecoderBackend::Symphonia, &host_rate);

        assert!(target.is_none());
        assert_eq!(
            resampler_stage(
                DecoderBackend::Symphonia,
                ResamplerQuality::High,
                ResamplerOptions::default()
            ),
            ResamplerStage::Present {
                quality: ResamplerQuality::High,
                options: ResamplerOptions::default()
            }
        );
    }

    #[cfg(not(all(
        feature = "apple-fused-src",
        any(target_os = "macos", target_os = "ios")
    )))]
    #[test]
    fn non_fused_build_keeps_decoder_source_rate_and_resampler_stage() {
        let host_rate = Arc::new(AtomicU32::new(48_000));

        assert!(decoder_target_output_rate(DecoderBackend::default(), &host_rate).is_none());
        assert_eq!(
            resampler_stage(
                DecoderBackend::default(),
                ResamplerQuality::High,
                ResamplerOptions::default()
            ),
            ResamplerStage::Present {
                quality: ResamplerQuality::High,
                options: ResamplerOptions::default()
            }
        );
    }
}
