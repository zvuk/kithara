//! Global musical key estimation from decoded PCM.

#![forbid(unsafe_code)]

#[cfg(feature = "dsp")]
mod consts;
#[cfg(feature = "dsp")]
mod correlation;
mod document;
#[cfg(feature = "dsp")]
mod error;
#[cfg(feature = "dsp")]
mod estimator;
#[cfg(feature = "dsp")]
mod grid;
mod key;
#[cfg(feature = "dsp")]
mod profile;

pub use document::{KEY_SCHEMA_VERSION, KeyDocumentError, RawKey};
#[cfg(feature = "dsp")]
pub use error::KeyError;
#[cfg(feature = "dsp")]
pub use estimator::KeyEstimator;
#[cfg(feature = "dsp")]
pub use grid::KeyResume;
pub use key::{KeyEstimate, Mode, MusicalKey, PitchClass};
