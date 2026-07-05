use bon::Builder;
use kithara_bufpool::PcmPool;

#[derive(Clone, Debug, Builder)]
#[non_exhaustive]
pub struct StretchOptions {
    pub sample_rate: u32,
    pub channels: usize,
    #[builder(default = 8192)]
    pub max_input_frames: usize,
    pub pool: PcmPool,
}
