/// MPEG-1 Layer III bitrates in kbps, indexed by the header's bitrate bits.
pub(crate) const MPEG1_BITRATES_KBPS: [u32; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];

/// MPEG-1 sample rates, indexed by the header's sampling-rate bits.
pub(crate) const MPEG1_SAMPLE_RATES: [u32; 4] = [44_100, 48_000, 32_000, 0];
