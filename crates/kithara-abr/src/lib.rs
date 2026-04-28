//! Adaptive Bitrate (ABR) streaming algorithm.
//!
//! Protocol-agnostic: provides a shared [`AbrController`] owned by the
//! downloader, a [`trait@Abr`] trait implemented by peers that want variant
//! switching, and an [`AbrHandle`] returned on registration.
//!
//! All shared vocabulary (`AbrMode`, `AbrReason`, `AbrVariant`,
//! `VariantInfo`, `BandwidthSource`, `AbrSettings`, `AbrDecision`,
//! `AbrProgressSnapshot`, `AbrPeerId`, `VariantDuration`) lives in
//! `kithara-events`; we re-export the names for convenience.

#![forbid(unsafe_code)]
#![cfg_attr(
    any(test, feature = "internal"),
    allow(clippy::ignored_unit_patterns, clippy::allow_attributes)
)]

mod abr;
mod controller;
mod estimator;
mod handle;
mod state;
mod types;

#[cfg(feature = "internal")]
pub mod internal;

pub use abr::Abr;
#[cfg(any(test, feature = "internal"))]
pub use abr::AbrMock;
pub use controller::AbrController;
#[cfg(any(test, feature = "internal"))]
pub use estimator::EstimatorMock;
pub use estimator::{Estimator, ThroughputEstimator};
pub use handle::AbrHandle;
#[cfg(any(test, feature = "internal"))]
pub use state::test_variants_3;
pub use state::{AbrError, AbrState, AbrView};
pub use types::{
    AbrDecision, AbrMode, AbrPeerId, AbrProgressSnapshot, AbrReason, AbrSettings, AbrVariant,
    BandwidthSource, VariantDuration, VariantInfo,
};
