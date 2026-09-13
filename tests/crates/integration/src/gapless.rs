pub const GAPLESS_SAMPLE_RATE: u32 = 48_000;
pub const GAPLESS_CHANNELS: u16 = 2;
pub const AAC_GAPLESS_SEGMENTS: usize = 3;
pub const AAC_GAPLESS_SEGMENT_SECS: f64 = 0.5;
pub const AAC_GAPLESS_SEGMENT_FRAMES: usize = 24_000;
pub const AAC_FRAME_SAMPLES: usize = 1_024;
pub const AAC_GAPLESS_ENCODER_DELAY: u32 = 2_112;
pub const AAC_GAPLESS_TRAILING_DELAY: u32 = 960;

// MediaCodec compensates its algorithmic delay when draining; the host FDK
// adapter removes 1744 leading frames without restoring them at EOF.
const AAC_DECODER_HEAD_STRIP: usize = if cfg!(target_os = "android") {
    0
} else {
    1_744
};

pub fn generated_aac_elst_visible_frames() -> usize {
    let packets_per_segment = AAC_GAPLESS_SEGMENT_FRAMES.div_ceil(AAC_FRAME_SAMPLES);
    let native_encoder_delay = AAC_FRAME_SAMPLES;
    let encoder_delay = usize::try_from(AAC_GAPLESS_ENCODER_DELAY).expect("AAC delay fits usize");
    let trailing_delay =
        usize::try_from(AAC_GAPLESS_TRAILING_DELAY).expect("AAC padding fits usize");
    packets_per_segment
        .saturating_mul(AAC_FRAME_SAMPLES)
        .saturating_mul(AAC_GAPLESS_SEGMENTS)
        .saturating_sub(native_encoder_delay)
        .saturating_sub(encoder_delay)
        .saturating_sub(trailing_delay)
        .saturating_sub(AAC_DECODER_HEAD_STRIP)
}
