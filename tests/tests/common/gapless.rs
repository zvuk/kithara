pub(crate) const GAPLESS_SAMPLE_RATE: u32 = 48_000;
pub(crate) const GAPLESS_CHANNELS: u16 = 2;
pub(crate) const AAC_GAPLESS_SEGMENTS: usize = 3;
pub(crate) const AAC_GAPLESS_SEGMENT_SECS: f64 = 0.5;
pub(crate) const AAC_GAPLESS_SEGMENT_FRAMES: usize = 24_000;
pub(crate) const AAC_FRAME_SAMPLES: usize = 1_024;
pub(crate) const AAC_GAPLESS_ENCODER_DELAY: u32 = 2_112;
pub(crate) const AAC_GAPLESS_TRAILING_DELAY: u32 = 960;

/// fdk-aac algorithmic delay (`CStreamInfo::outputDelay`) for AAC-LC
/// stereo @ 48 kHz. The decoder strips these leading frames internally
/// to time-align decoded PCM with the encoded stream — see
/// `crates/kithara-decode/src/symphonia/aac_fdk.rs`.
///
/// Decision log: prior to landing the in-tree fdk-aac adapter the
/// upstream `symphonia-adapter-fdk-aac` emitted `outputDelay` worth of
/// silent leading frames (audible ~36 ms gap in prod). Visible-frame
/// formulas were calibrated to the broken behaviour; they now subtract
/// `AAC_FDK_OUTPUT_DELAY` so they account for the strip on top of
/// container-level gapless (`elst`, iTunSMPB).
pub(crate) const AAC_FDK_OUTPUT_DELAY: usize = 1_744;

pub(crate) fn generated_aac_elst_visible_frames() -> usize {
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
        .saturating_sub(AAC_FDK_OUTPUT_DELAY)
}
