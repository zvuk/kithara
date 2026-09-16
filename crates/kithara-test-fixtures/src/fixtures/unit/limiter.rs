use kithara_test_macros as kithara;

use crate::{assets, fixtures::samples};

/// Prepared build-time PCM input for limiter peak.
#[kithara::fixture]
#[must_use]
pub fn limiter_peak() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_peak())
}

/// Prepared build-time PCM input for limiter negative.
#[kithara::fixture]
#[must_use]
pub fn limiter_negative() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_negative())
}

/// Prepared build-time PCM input for limiter right.
#[kithara::fixture]
#[must_use]
pub fn limiter_right() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_right())
}

/// Prepared build-time PCM input for limiter left.
#[kithara::fixture]
#[must_use]
pub fn limiter_left() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_left())
}

/// Prepared build-time PCM input for limiter quiet.
#[kithara::fixture]
#[must_use]
pub fn limiter_quiet() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_quiet())
}

/// Prepared build-time PCM input for limiter two.
#[kithara::fixture]
#[must_use]
pub fn limiter_two() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_two())
}

/// Prepared build-time PCM input for limiter attack.
#[kithara::fixture]
#[must_use]
pub fn limiter_attack() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_attack())
}

/// Prepared build-time PCM input for limiter half.
#[kithara::fixture]
#[must_use]
pub fn limiter_half() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_half())
}

/// Prepared build-time PCM input for limiter spike.
#[kithara::fixture]
#[must_use]
pub fn limiter_spike() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_spike())
}

/// Prepared build-time PCM input for a limiter signal that stays below the ceiling
/// between its samples as well as at them: a smooth sinusoid at half scale.
#[kithara::fixture]
#[must_use]
pub fn limiter_smooth() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_smooth())
}

/// Prepared build-time PCM input for a limiter overload whose true peak is stable:
/// a smooth sinusoid at twice full scale, free of the discontinuity that makes a
/// step's reconstructed peak depend on the reconstruction kernel.
#[kithara::fixture]
#[must_use]
pub fn limiter_sine() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_sine())
}

/// Prepared build-time PCM input for limiter inter-sample peaks: a quarter-rate
/// sinusoid sampled at its 45-degree phase, so every sample sits at 1/sqrt(2)
/// while the reconstructed waveform reaches full scale between them.
#[kithara::fixture]
#[must_use]
pub fn limiter_intersample() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_intersample())
}

/// Prepared build-time PCM input for limiter silence.
#[kithara::fixture]
#[must_use]
pub fn limiter_silence() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_silence())
}

/// Prepared build-time PCM input for limiter recovery.
#[kithara::fixture]
#[must_use]
pub fn limiter_recovery() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_recovery())
}

/// Prepared build-time PCM input for limiter negative infinity.
#[kithara::fixture]
#[must_use]
pub fn limiter_negative_infinity() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_negative_infinity())
}

/// Prepared build-time PCM input for limiter infinity.
#[kithara::fixture]
#[must_use]
pub fn limiter_infinity() -> Vec<f32> {
    samples(&assets::unit_pcm_limiter_infinity())
}
