//! Adaptive Bitrate (ABR) streaming algorithm.
//!
//! Protocol-agnostic: provides a shared [`AbrController`] owned by the
//! downloader, a [`trait@Abr`] trait implemented by peers that want variant
//! switching, and an [`AbrHandle`] returned on registration.
//!
//! Shared event vocabulary and controller state are defined here.

#![forbid(unsafe_code)]

mod abr;
mod controller;
mod estimator;
mod event;
mod handle;
mod state;
mod types;

pub use abr::Abr;
#[cfg(any(test, feature = "mock"))]
pub use abr::AbrMock;
pub use controller::AbrController;
#[cfg(test)]
pub use estimator::EstimatorMock;
pub use estimator::{Estimator, ThroughputEstimator};
pub use event::{
    AbrEvent, AbrMode, AbrProgressSnapshot, AbrReason, BandwidthSource, BoundsError,
    VariantDuration, VariantIndex, VariantInfo,
};
pub use handle::AbrHandle;
use humantime_serde as _;
pub use state::{AbrError, AbrPublisher, AbrState, AbrView};
pub use types::{
    AbrDecision, AbrPeerId, AbrSettings, AbrSettingsPatch, AbrTicket, PendingAbrClaim,
    PendingAbrDecision,
};
mod consts;
