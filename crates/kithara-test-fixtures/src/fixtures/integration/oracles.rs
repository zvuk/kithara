use kithara_test_macros as kithara;

use crate::{assets, fixtures::samples};

#[kithara::fixture]
#[must_use]
pub fn shifted_pitch() -> Vec<f32> {
    samples(&assets::shifted_pitch_default())
}

#[kithara::fixture]
#[must_use]
pub fn quality_control_a() -> Vec<f32> {
    samples(&assets::quality_control_a())
}

#[kithara::fixture]
#[must_use]
pub fn quality_control_b() -> Vec<f32> {
    samples(&assets::quality_control_b())
}

#[kithara::fixture]
#[must_use]
pub fn quality_control_joined() -> Vec<f32> {
    samples(&assets::quality_control_joined())
}

#[kithara::fixture]
#[must_use]
pub fn oracle_stem_a() -> Vec<f32> {
    samples(&assets::oracle_stem_a_default())
}

#[kithara::fixture]
#[must_use]
pub fn oracle_stem_b() -> Vec<f32> {
    samples(&assets::oracle_stem_b_default())
}

#[kithara::fixture]
#[must_use]
pub fn phase_sine_measured() -> Vec<f32> {
    samples(&assets::phase_sine_measured())
}

#[kithara::fixture]
#[must_use]
pub fn phase_sine_jitter() -> Vec<f32> {
    samples(&assets::phase_sine_jitter())
}

#[kithara::fixture]
#[must_use]
pub fn phase_sine_anchor() -> Vec<f32> {
    samples(&assets::phase_sine_anchor())
}

#[kithara::fixture]
#[must_use]
pub fn phase_sine_dropped() -> Vec<f32> {
    samples(&assets::phase_sine_dropped())
}

#[kithara::fixture]
#[must_use]
/// # Panics
/// Panics if a prepared sample does not contain eight bytes.
pub fn phase_noise() -> Vec<f64> {
    assets::phase_noise_default()
        .bytes()
        .chunks_exact(size_of::<f64>())
        .map(|bytes| f64::from_le_bytes(bytes.try_into().expect("prepared f64 sample")))
        .collect()
}

#[kithara::fixture]
#[must_use]
pub fn listening_reference() -> Vec<f32> {
    samples(&assets::listening_reference_default())
}

#[kithara::fixture]
#[must_use]
pub fn cochlea_control() -> Vec<f32> {
    samples(&assets::cochlea_signal_control())
}

#[kithara::fixture]
#[must_use]
pub fn cochlea_loudness() -> Vec<f32> {
    samples(&assets::cochlea_signal_loudness())
}
