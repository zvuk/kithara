use std::sync::{Arc, atomic::AtomicU32};

use kithara_decode::{DecoderBackend, ResamplerQuality};

use crate::pipeline::config::ResamplerStage;

pub(crate) fn decoder_target_output_rate(
    _backend: DecoderBackend,
    _host_sample_rate: &Arc<AtomicU32>,
) -> Option<Arc<AtomicU32>> {
    None
}

pub(crate) fn resampler_stage(
    _backend: DecoderBackend,
    quality: ResamplerQuality,
) -> ResamplerStage {
    ResamplerStage::Present(quality)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_fused_build_keeps_decoder_source_rate_and_resampler_stage() {
        let host_rate = Arc::new(AtomicU32::new(48_000));

        assert!(decoder_target_output_rate(DecoderBackend::default(), &host_rate).is_none());
        assert_eq!(
            resampler_stage(DecoderBackend::default(), ResamplerQuality::High),
            ResamplerStage::Present(ResamplerQuality::High)
        );
    }
}
