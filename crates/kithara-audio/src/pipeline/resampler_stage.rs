use std::sync::{Arc, atomic::AtomicU32};

use kithara_bufpool::PcmPool;
use kithara_decode::PcmSpec;

use crate::{
    pipeline::config::ResamplerStage,
    resampler::{ResamplerParams, ResamplerProcessor},
    traits::AudioEffect,
};

pub(crate) fn append(
    chain: &mut Vec<Box<dyn AudioEffect>>,
    stage: ResamplerStage,
    initial_spec: PcmSpec,
    host_sample_rate: &Arc<AtomicU32>,
    pool: &PcmPool,
) {
    let ResamplerStage::Present { quality, options } = stage else {
        return;
    };

    let params = ResamplerParams::builder()
        .host_sample_rate(Arc::clone(host_sample_rate))
        .source_sample_rate(initial_spec.sample_rate.get())
        .channels(usize::from(initial_spec.channels))
        .quality(quality)
        .options(options)
        .pool(pool.clone())
        .build();

    chain.push(Box::new(ResamplerProcessor::new(params)));
}
