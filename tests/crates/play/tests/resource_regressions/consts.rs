use super::helpers::{Duration, shared};

pub(super) const READ_TIMEOUT: Duration = shared::READ_TIMEOUT;
pub(super) const HLS_SEGMENT_COUNT: usize = 3;
pub(super) const HLS_SEGMENT_SIZE: usize = shared::SEGMENT_SIZE;
pub(super) const HLS_SAMPLE_RATE: f64 = shared::SAMPLE_RATE as f64;
pub(super) const HLS_CHANNELS: f64 = shared::CHANNELS as f64;
/// Expected duration of the generated `signal_mp3_track_sine440_187s` clip.
pub(super) const EXPECTED_DURATION_SECS: f64 = shared::TEST_MP3_DURATION_SECS;
