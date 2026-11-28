use std::sync::OnceLock;

use kithara_test_macros as kithara;

use crate::{assets, fixtures::samples};

/// Prepared stereo 440 Hz input for progressive analysis tests.
#[kithara::fixture]
pub fn analysis_pcm() -> &'static [f32] {
    static PCM: OnceLock<Vec<f32>> = OnceLock::new();
    PCM.get_or_init(|| samples(&assets::analysis_tone_stereo()))
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn waveform_tone() -> Vec<f32> {
    samples(&assets::waveform_tone_tone())
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn waveform_low() -> Vec<f32> {
    samples(&assets::waveform_tone_low())
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn waveform_mid() -> Vec<f32> {
    samples(&assets::waveform_tone_mid())
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn waveform_high() -> Vec<f32> {
    samples(&assets::waveform_tone_high())
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn waveform_half() -> Vec<f32> {
    samples(&assets::analysis_values_half())
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn waveform_square() -> Vec<f32> {
    samples(&assets::analysis_values_square())
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn analysis_silence() -> Vec<f32> {
    samples(&assets::analysis_values_silence())
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn waveform_tiny() -> Vec<f32> {
    samples(&assets::analysis_values_tiny())
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn waveform_opposed() -> Vec<f32> {
    samples(&assets::analysis_values_opposed())
}

/// Prepared input for analysis tests.
#[kithara::fixture]
#[must_use]
pub fn waveform_mix() -> Vec<f32> {
    samples(&assets::waveform_mix_full_spectrum())
}

/// Prepared input for waveform bucket tests.
#[kithara::fixture]
#[must_use]
pub fn bucket_ranges() -> Vec<f32> {
    samples(&assets::bucket_input_ranges())
}

/// Prepared input for waveform bucket tests.
#[kithara::fixture]
#[must_use]
pub fn bucket_components() -> Vec<f32> {
    samples(&assets::bucket_input_components())
}

/// Prepared input for waveform bucket tests.
#[kithara::fixture]
#[must_use]
pub fn bucket_short() -> Vec<f32> {
    samples(&assets::bucket_input_short())
}

/// Prepared input for waveform bucket tests.
#[kithara::fixture]
#[must_use]
pub fn bucket_sine() -> Vec<f32> {
    samples(&assets::bucket_input_sine())
}

/// Prepared two-second peak-envelope carriers, in low, burst, low-mid, high-mid, high order.
#[kithara::fixture]
#[must_use]
pub fn waveform_peak_tones() -> [Vec<f32>; 5] {
    [
        assets::waveform_peak_tone_low(),
        assets::waveform_peak_tone_burst(),
        assets::waveform_peak_tone_low_mid(),
        assets::waveform_peak_tone_high_mid(),
        assets::waveform_peak_tone_high(),
    ]
    .map(|asset| samples(&asset))
}

#[kithara::fixture]
#[must_use]
pub fn waveform_peak_clicks() -> Vec<f32> {
    samples(&assets::analysis_values_peak_clicks())
}
