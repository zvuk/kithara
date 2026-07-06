use std::sync::{Arc, atomic::AtomicU32};

use kithara_bufpool::PcmPool;
use kithara_decode::{PcmSpec, ResamplerQuality};

use crate::{
    pipeline::config::ResamplerStage,
    resampler::{ResamplerParams, ResamplerProcessor},
    traits::AudioEffect,
};

#[cfg(feature = "apple-fused-src")]
pub(crate) fn append(
    chain: &mut Vec<Box<dyn AudioEffect>>,
    stage: ResamplerStage,
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    pool: Option<PcmPool>,
) {
    let ResamplerStage::Present(quality) = stage else {
        return;
    };
    append_present(chain, initial_spec, host_sample_rate, quality, pool);
}

#[cfg(not(feature = "apple-fused-src"))]
pub(crate) fn append(
    chain: &mut Vec<Box<dyn AudioEffect>>,
    stage: ResamplerStage,
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    pool: Option<PcmPool>,
) {
    let ResamplerStage::Present(quality) = stage;
    append_present(chain, initial_spec, host_sample_rate, quality, pool);
}

fn append_present(
    chain: &mut Vec<Box<dyn AudioEffect>>,
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    quality: ResamplerQuality,
    pool: Option<PcmPool>,
) {
    let params = ResamplerParams::builder()
        .host_sample_rate(Arc::clone(host_sample_rate))
        .source_sample_rate(initial_spec.sample_rate.get())
        .channels(usize::from(initial_spec.channels))
        .quality(quality)
        .maybe_pool(pool)
        .build();

    chain.push(Box::new(ResamplerProcessor::new(params)));
}
