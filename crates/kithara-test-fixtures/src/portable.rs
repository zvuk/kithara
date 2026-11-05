//! Fixed signal inputs prepared by the host build, available on every target.

use crate::consts;

/// One second of an unquantized 440 Hz mono sine computed in f32 at 48 kHz.
#[must_use]
pub fn eq_sine_440() -> &'static [f32] {
    &consts::EQ_SINE
}

/// The 4096 original hero-wave levels, with a 0.3 floor and 0.6 sine envelope.
#[must_use]
pub fn hero_wave_levels() -> &'static [f32] {
    &consts::HERO_LEVELS
}

/// The first 8192 mono samples of a full-scale quantized 440 Hz sine at 48 kHz.
#[must_use]
pub fn sine_440() -> &'static [f32] {
    &consts::SINE
}

/// Eight 4096-sample mono harmonics of 440 Hz at 48 kHz, normalized by 32768.
#[must_use]
pub fn harmonics() -> [&'static [f32]; 8] {
    [
        &consts::SINE[..4_096],
        &consts::SECOND,
        &consts::THIRD,
        &consts::FOURTH,
        &consts::FIFTH,
        &consts::SIXTH,
        &consts::SEVENTH,
        &consts::EIGHTH,
    ]
}

/// The first 4096 mono samples of the ascending i16 saw, normalized by 32768.
#[must_use]
pub fn sawtooth() -> &'static [f32] {
    &consts::SAW
}

/// A 4096-sample 48 kHz mixture of a half-level sine and ascending saw.
#[must_use]
pub fn spectrum_mix() -> &'static [f32] {
    &consts::MIX
}

/// A 4096-sample mono mixture of 1 kHz and 30 kHz at 96 kHz.
#[must_use]
pub fn glide_two_tones() -> &'static [f32] {
    &consts::TWO_TONES
}

/// Full-depth duck input at the canonical limiter ceiling.
#[must_use]
pub fn host_metronome_held() -> &'static [f32] {
    &consts::METRONOME_HELD
}

/// A 10 kHz moving mix under the canonical full-depth duck envelope.
#[must_use]
pub fn host_metronome_tone() -> &'static [f32] {
    &consts::METRONOME_TONE
}
