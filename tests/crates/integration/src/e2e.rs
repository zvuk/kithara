use std::env;

const DEFAULT_VOLUME: f32 = 0.1;

/// Master volume for audible e2e playback (cpal output on real hardware).
/// Quiet by default; `KITHARA_E2E_VOLUME=<0.0..=1.0>` raises it for a
/// deliberate listening session.
#[must_use]
pub fn volume() -> f32 {
    env::var("KITHARA_E2E_VOLUME")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .map_or(DEFAULT_VOLUME, |v| v.clamp(0.0, 1.0))
}
