use kithara_platform::maybe_send::{MaybeSend, MaybeSync};

use crate::error::PlayError;

mod kithara {
    pub(crate) use kithara_test_macros::mock;
}

/// Master N-band equalizer.
///
/// Provides read/write access to the current per-band gains by index. A concrete
/// player may replace its layout through `PlayerImpl::set_eq_layout`.
#[kithara::mock(api = EqualizerMock)]
pub trait Equalizer: MaybeSend + MaybeSync + 'static {
    /// Number of EQ bands.
    fn band_count(&self) -> usize;

    /// Get the current gain in dB for the given band index.
    fn gain(&self, band: usize) -> Option<f32>;

    /// Reset all bands to 0 dB.
    fn reset(&self) -> Result<(), PlayError>;

    /// Set the gain in dB for the given band index (clamped to -24..+6 dB).
    fn set_gain(&self, band: usize, gain_db: f32) -> Result<(), PlayError>;
}
