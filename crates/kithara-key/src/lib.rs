//! Global musical key estimation from decoded PCM.

#![forbid(unsafe_code)]

mod consts;
mod correlation;
mod error;
mod estimator;
mod grid;
mod key;
mod profile;

pub use error::KeyError;
pub use estimator::KeyEstimator;
pub use grid::KeyResume;
pub use key::{KeyEstimate, Mode, MusicalKey, PitchClass};
