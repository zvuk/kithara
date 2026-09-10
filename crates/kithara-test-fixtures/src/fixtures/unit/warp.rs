use kithara_test_macros as kithara;

use crate::{assets, fixtures::samples};

/// Prepared build-time PCM input for warp sine.
#[kithara::fixture]
#[must_use]
pub fn warp_sine() -> Vec<f32> {
    samples(&assets::unit_pcm_warp_sine())
}

/// Prepared build-time PCM input for warp pair.
#[kithara::fixture]
#[must_use]
pub fn warp_pair() -> Vec<f32> {
    samples(&assets::unit_pcm_warp_pair())
}

/// Prepared build-time PCM input for warp constant.
#[kithara::fixture]
#[must_use]
pub fn warp_constant() -> Vec<f32> {
    samples(&assets::unit_pcm_warp_constant())
}

/// Prepared build-time PCM input for nominal warp clicks.
#[kithara::fixture]
#[must_use]
pub fn warp_nominal_clicks() -> Vec<f32> {
    samples(&assets::unit_pcm_warp_nominal_clicks())
}

/// Prepared build-time PCM input for warp clicks.
#[kithara::fixture]
#[must_use]
pub fn warp_clicks() -> Vec<f32> {
    samples(&assets::unit_pcm_warp_clicks())
}
