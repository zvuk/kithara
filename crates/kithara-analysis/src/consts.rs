use std::{
    mem::size_of,
    num::{NonZeroU32, NonZeroUsize},
};

use kithara_resampler::ResamplerQuality;

pub(crate) const DEFAULT_BEAT_BLOCK_FRAMES: usize = 1024;
pub(crate) const DEFAULT_BEAT_DETECTOR_MIN_WINDOW_SECONDS: u32 = 10;
pub(crate) const DEFAULT_BEAT_DETECTOR_OVERLAP_SECONDS: u32 = 2;
pub(crate) const DEFAULT_BEAT_DETECTOR_WINDOW_SECONDS: u32 = 30;
pub(crate) const DEFAULT_BEAT_RESAMPLER_QUALITY: ResamplerQuality = ResamplerQuality::High;
pub(crate) const DEFAULT_BEAT_TARGET_RATE: u32 = 22_050;
pub(crate) const FINGERPRINT_MAX: usize = 1024;
pub(crate) const INDEX_ENTRY_LEN: usize = 1;
pub(crate) const HEADER_FIELDS_LEN: usize = 88;
pub(crate) const HEADER_LEN: usize = HEADER_FIELDS_LEN + 2 * (size_of::<u32>() + FINGERPRINT_MAX);

#[cfg(all(
    test,
    feature = "analysis-beat",
    feature = "analysis-waveform",
    not(target_arch = "wasm32")
))]
pub(crate) const RESUME_TESTS_CHUNK_FRAMES: u64 = 128;

#[cfg(all(
    test,
    feature = "analysis-beat",
    feature = "analysis-waveform",
    not(target_arch = "wasm32")
))]
pub(crate) const RESUME_TESTS_EXTENT: u64 = 4 * RESUME_TESTS_CHUNK_FRAMES;

#[cfg(test)]
pub(crate) const ARCHIVE_EXTENT: u64 = 64;

#[cfg(test)]
pub(crate) const ARCHIVE_CHUNK_FRAMES: u64 = 16;

pub(crate) const FRAME_BYTES: usize = size_of::<u64>() + size_of::<u32>() + size_of::<f32>();
pub(crate) const LEN_PREFIX_BYTES: usize = size_of::<u64>();
pub(crate) const LIST_COUNT: usize = 3;
pub(crate) const SEGMENT_BYTES: usize = size_of::<u64>() * 2 + size_of::<f64>();
pub(crate) const VERSION: u32 = 2;

#[cfg(test)]
pub(crate) const V2_FIXTURE: &[u8] = &[
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x5e, 0x40, // 120 BPM
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, // frame 0
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f, // observed, confidence 1
    0x22, 0x56, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // frame 22_050
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // frame 0
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f, // observed, confidence 1
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, // start 0
    0x44, 0xac, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // end 44_100
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f, // ratio 1
];

pub(crate) const SECONDS_PER_MINUTE: f64 = 60.0;

#[cfg(test)]
pub(crate) const BPM: f64 = 120.0;

/// Half a second at each rate, so the same music lands on the same
/// seconds from two different frame counts.
#[cfg(test)]
pub(crate) const PERIOD_44_1: u64 = 22_050;

#[cfg(test)]
pub(crate) const PERIOD_48: u64 = 24_000;

#[cfg(test)]
pub(crate) const RATE_44_1: u32 = 44_100;

#[cfg(test)]
pub(crate) const RATE_48: u32 = 48_000;

#[cfg(feature = "analysis-beat")]
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const SRC: u32 = 44_100;

#[cfg(feature = "analysis-beat")]
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const TARGET: usize = 22_050;

#[cfg(feature = "analysis-beat")]
#[cfg(test)]
pub(crate) const CORE_SR: u32 = 44_100;

#[cfg(feature = "analysis-beat")]
#[cfg(test)]
pub(crate) const TOL_100MS: u64 = 4_410;

#[cfg(feature = "analysis-beat")]
#[cfg(test)]
pub(crate) const TOL_20MS: u64 = 882;

#[cfg(feature = "analysis-beat")]
#[cfg(test)]
pub(crate) const EXTEND_RATE: u32 = 44_100;

/// Half a second at [`EXTEND_RATE`].
#[cfg(feature = "analysis-beat")]
#[cfg(test)]
pub(crate) const EXTEND_BEAT: u64 = 22_050;

pub(crate) const ANALYSIS_PROGRESS_BYTES_VERSION: u32 = 0x4b41_5001;
pub(crate) const TRACK_ANALYSIS_BYTES_VERSION: u32 = 0x4b41_0007;

#[cfg(test)]
pub(crate) const BEAT_TAG: &str = "beat:test:v1";

#[cfg(test)]
pub(crate) const TRACK_TOKEN: &str = "assets/track.analysis";

#[cfg(test)]
pub(crate) const V5_FIXTURE: &[u8] = &[
    0x05, 0x00, 0x41, 0x4b, 0x09, 0x00, 0x00, 0x00, 0x67, 0x6f, 0x6c, 0x64, 0x65, 0x6e, 0x2d, 0x76,
    0x35, 0x80, 0xbb, 0x00, 0x00, 0x01, 0xd2, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc8, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x77, 0x61,
    0x76, 0x65, 0x3a, 0x76, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00,
    0x00, 0x62, 0x65, 0x61, 0x74, 0x3a, 0x76, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00,
];

#[cfg(test)]
pub(crate) const V7_FIXTURE: &[u8] = &[
    0x07, 0x00, 0x41, 0x4b, 0x09, 0x00, 0x00, 0x00, 0x67, 0x6f, 0x6c, 0x64, 0x65, 0x6e, 0x2d, 0x76,
    0x37, 0x44, 0xac, 0x00, 0x00, 0x01, 0xd2, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x09, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc8,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07,
    0x00, 0x00, 0x00, 0x77, 0x61, 0x76, 0x65, 0x3a, 0x76, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x07, 0x00, 0x00, 0x00, 0x62, 0x65, 0x61, 0x74, 0x3a, 0x76, 0x31, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

pub(crate) const RESUME_VERSION: u32 = 0x4b41_5202;

#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const MARKER_TOLERANCE: u64 = 64;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const FIXTURES_SR: u32 = 44_100;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const CH: u16 = 2;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) const HOLD_BUCKETS: usize = 64;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) const HOLD_CHUNK_FRAMES: u64 = 200;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) const HOLD_CHUNK_SECONDS: u32 = 16;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) const HOG_FRAMES: usize = 8192;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) const PATIENCE: u32 = 16;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) const HOLD_RATE: u32 = 1000;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) const TICK_LIMIT: u64 = 1 << 20;

#[cfg(feature = "analysis-waveform")]
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const NODE_BUCKETS: usize = 64;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(feature = "beat-backend")]
pub(crate) const PROBE_CHUNK_FRAMES: u64 = 8820;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const CHUNK: u64 = 8820;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const SCHEDULE_EXTENT: u64 = 4 * 44_100;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const TICKS: usize = 8192;

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const SCHEDULE_TOKEN: &str = "scheduled-track";

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) const SCHEDULE_WINDOW_SECONDS: u32 = 1;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) const ARTIFACTS_WINDOW_SECONDS: u32 = 2;

#[cfg(all(test, not(target_arch = "wasm32")))]
#[cfg(all(feature = "analysis-beat", feature = "analysis-waveform"))]
pub(crate) const ARTIFACTS_EXTENT: u64 = 12 * 44_100;

pub(crate) const CAPACITY: NonZeroUsize = match NonZeroUsize::new(64) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const CONFIG_CHUNK_SECONDS: NonZeroU32 = match NonZeroU32::new(16) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const FAIRNESS_YIELD_INTERVAL: NonZeroU32 = match NonZeroU32::new(16) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const PRODUCER_DRAIN_LIMIT: NonZeroUsize = match NonZeroUsize::new(8) {
    Some(value) => value,
    None => unreachable!(),
};

pub(crate) const PUBLISH_SECONDS: NonZeroU32 = match NonZeroU32::new(5) {
    Some(value) => value,
    None => unreachable!(),
};

#[cfg(test)]
pub(crate) const WORKER_SCHEDULE_EXTENT: u64 = 1000;

#[cfg(test)]
pub(crate) const WINDOW: u64 = 200;
