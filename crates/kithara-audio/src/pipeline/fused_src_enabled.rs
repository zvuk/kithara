use std::sync::{Arc, atomic::AtomicU32};

use kithara_decode::{DecoderBackend, ResamplerQuality};

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
) -> ResamplerStage {
    if fused_src_active(backend) {
        ResamplerStage::Absent
    } else {
        ResamplerStage::Present(quality)
    }
}

fn fused_src_active(backend: DecoderBackend) -> bool {
    matches!(backend, DecoderBackend::Apple)
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, sync::atomic::Ordering};

    use super::*;

    fn current_target_output_rate(rate: Option<&Arc<AtomicU32>>) -> Option<u32> {
        rate.and_then(|rate| NonZeroU32::new(rate.load(Ordering::Acquire)).map(NonZeroU32::get))
    }

    #[test]
    fn apple_backend_uses_fused_output_rate_and_omits_stage() {
        let host_rate = Arc::new(AtomicU32::new(48_000));

        let target = decoder_target_output_rate(DecoderBackend::Apple, &host_rate);

        assert_eq!(current_target_output_rate(target.as_ref()), Some(48_000));
        assert_eq!(
            resampler_stage(DecoderBackend::Apple, ResamplerQuality::default()),
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
            resampler_stage(DecoderBackend::Symphonia, ResamplerQuality::High),
            ResamplerStage::Present(ResamplerQuality::High)
        );
    }
}
