use kithara_platform::{MaybeSend, MaybeSync};

use crate::error::PlayError;

/// Master N-band equalizer.
///
/// Provides read/write access to per-band gains by index. The number of bands
/// is fixed at player construction time via [`PlayerConfig::eq_layout`].
#[cfg_attr(
    any(test, feature = "test-utils"),
    unimock::unimock(api = EqualizerMock)
)]
pub trait Equalizer: MaybeSend + MaybeSync + 'static {
    /// Number of EQ bands.
    fn band_count(&self) -> usize;

    /// Get the current gain in dB for the given band index.
    fn gain(&self, band: usize) -> Option<f32>;

    /// Set the gain in dB for the given band index (clamped to -24..+6 dB).
    fn set_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError>;

    /// Reset all bands to 0 dB.
    fn reset(&self) -> Result<(), PlayError>;
}
