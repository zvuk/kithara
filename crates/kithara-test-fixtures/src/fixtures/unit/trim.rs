use kithara_test_macros as kithara;

use crate::{assets, fixtures::samples};

/// Prepared build-time PCM input for trim ramp.
#[kithara::fixture]
#[must_use]
pub fn trim_ramp() -> Vec<f32> {
    samples(&assets::unit_pcm_trim_ramp())
}

/// Prepared build-time PCM input for trim silence.
#[kithara::fixture]
#[must_use]
pub fn trim_silence() -> Vec<f32> {
    samples(&assets::unit_pcm_trim_silence())
}

/// Prepared build-time PCM input for trim codec priming drops leading frames and fades in.
#[kithara::fixture]
#[must_use]
pub fn trim_codec_priming_drops_leading_frames_and_fades_in() -> Vec<f32> {
    samples(&assets::unit_pcm_trim_codec_priming_drops_leading_frames_and_fades_in())
}

/// Prepared build-time PCM input for trim codec priming metadata takes precedence when combined.
#[kithara::fixture]
#[must_use]
pub fn trim_codec_priming_metadata_takes_precedence_when_combined() -> Vec<f32> {
    samples(&assets::unit_pcm_trim_codec_priming_metadata_takes_precedence_when_combined())
}
