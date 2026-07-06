use std::sync::{Arc, atomic::AtomicU32};

use kithara_bufpool::PcmPool;
use kithara_decode::PcmSpec;

use crate::{pipeline::config::ResamplerStage, traits::AudioEffect};

#[cfg(feature = "apple-fused-src")]
pub(crate) fn append(
    _chain: &mut Vec<Box<dyn AudioEffect>>,
    stage: ResamplerStage,
    _initial_spec: PcmSpec,
    _host_sample_rate: &Arc<AtomicU32>,
    _pool: Option<PcmPool>,
) {
    debug_assert!(
        matches!(stage, ResamplerStage::Absent),
        "resampler stage requested in a build without resample-rubato"
    );
}

#[cfg(not(feature = "apple-fused-src"))]
pub(crate) fn append(
    _chain: &mut Vec<Box<dyn AudioEffect>>,
    _stage: ResamplerStage,
    _initial_spec: PcmSpec,
    _host_sample_rate: &Arc<AtomicU32>,
    _pool: Option<PcmPool>,
) {
}
