//! Adaptive Bitrate (ABR) streaming algorithm.
//!
//! Protocol-agnostic: provides a shared [`AbrController`] owned by the
//! downloader, a [`trait@Abr`] trait implemented by peers that want variant
//! switching, and an [`AbrHandle`] returned on registration.
//!
//! Shared event vocabulary (`AbrMode`, `AbrReason`, `VariantInfo`,
//! `BandwidthSource`, `AbrProgressSnapshot`, `VariantDuration`) comes from
//! `kithara-events`; controller/state-owned types (`AbrSettings`,
//! `AbrDecision`, `AbrPeerId`) are defined here and re-exported for convenience.

#![forbid(unsafe_code)]

mod abr;
mod controller;
mod estimator;
mod handle;
mod state;
mod types;

pub use abr::Abr;
#[cfg(test)]
pub use abr::AbrMock;
pub use controller::AbrController;
#[cfg(test)]
pub use estimator::EstimatorMock;
pub use estimator::{Estimator, ThroughputEstimator};
pub use handle::AbrHandle;
pub use state::{AbrError, AbrState, AbrView};
pub use types::{
    AbrDecision, AbrMode, AbrPeerId, AbrProgressSnapshot, AbrReason, AbrSettings, BandwidthSource,
    BoundsError, VariantDuration, VariantIndex, VariantInfo,
};
