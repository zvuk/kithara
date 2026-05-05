//! Shared constants for test fixtures and specifications.

pub(crate) struct Consts;

impl Consts {
    pub(crate) const MIN_SAMPLE_RATE: u32 = 8_000;
    pub(crate) const MAX_SAMPLE_RATE: u32 = 192_000;
    pub(crate) const MAX_CHANNELS: u16 = 8;

    pub(crate) const MAX_SIGNAL_SECONDS: f64 = 300.0;
    pub(crate) const MAX_HLS_DURATION_SECS: f64 = 600.0;

    pub(crate) const MAX_SIGNAL_PCM_BYTES: usize = 32 * 1024 * 1024;
    pub(crate) const MAX_HLS_SEGMENT_SIZE: usize = 8 * 1024 * 1024;

    pub(crate) const WAV_HEADER_SIZE: usize = 44;

    pub(crate) const MAX_SIGNAL_SPEC_BYTES: usize = 4096;
    pub(crate) const MAX_HLS_SPEC_BYTES: usize = 32 * 1024;

    pub(crate) const MAX_HLS_VARIANTS: usize = 16;
    pub(crate) const MAX_HLS_SEGMENTS_PER_VARIANT: usize = 4096;
}
