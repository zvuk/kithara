use super::*;

pub(super) fn cpal_config(sample_rate: u32, output_block_frames: Option<NonZeroU32>) -> CpalConfig {
    let mut config = CpalConfig::default();
    config.output.desired_sample_rate = NonZeroU32::new(sample_rate).map(NonZeroU32::get);
    if let Some(frames) = output_block_frames {
        config.output.desired_block_frames = Some(frames.get());
    }
    config
}
