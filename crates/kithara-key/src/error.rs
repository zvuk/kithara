use kithara_bufpool::PoolError;
use kithara_dsp::spectrum::SpectrumError;

/// Why a key estimator could not be built or fed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeyError {
    /// No backend runs the frame's FFT.
    #[error("key estimation FFT failed: {0}")]
    Spectrum(#[from] SpectrumError),
    /// A frame buffer does not fit the region budget.
    #[error("key estimation buffer allocation failed: {0}")]
    Pool(#[from] PoolError),
}
