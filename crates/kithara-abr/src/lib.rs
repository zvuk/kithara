//! Adaptive Bitrate (ABR) streaming algorithm.
//!
//! Protocol-agnostic: provides a shared [`AbrController`] owned by the
//! downloader, a [`trait@Abr`] trait implemented by peers that want variant
//! switching, and an [`AbrHandle`] returned on registration.
//!
//! All shared vocabulary (`AbrMode`, `AbrReason`, `VariantInfo`,
//! `BandwidthSource`, `AbrSettings`, `AbrDecision`, `AbrProgressSnapshot`,
//! `AbrPeerId`, `VariantDuration`) lives in `kithara-events`; we
//! re-export the names for convenience.

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
    VariantDuration, VariantInfo,
};
