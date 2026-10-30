#[cfg(feature = "spectrum")]
mod autocorrelation;
#[cfg(feature = "spectrum")]
mod bins;
mod error;
#[cfg(feature = "spectrum")]
mod fft;
mod len;
#[cfg(all(test, feature = "spectrum"))]
pub(crate) mod oracle;
#[cfg(all(test, feature = "spectrum"))]
mod tests;

#[cfg(feature = "spectrum")]
pub use autocorrelation::Autocorrelation;
#[cfg(feature = "spectrum")]
pub use bins::{magnitude, phase, prediction_difference};
pub use error::SpectrumError;
#[cfg(feature = "spectrum")]
pub use fft::{Fft, Spectrum};
pub use len::FftLen;

#[cfg(all(test, feature = "spectrum"))]
mod prediction_tests;
