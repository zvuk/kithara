//! Adaptive Bitrate (ABR) streaming algorithm.
//!
//! This crate provides a protocol-agnostic ABR controller for adaptive streaming.
//! It works with any streaming protocol (HLS, DASH, etc.).
//!
//! ## Features
//!
//! - **Protocol-agnostic**: Works with any streaming protocol
//! - **Auto and Manual modes**: Supports both automatic adaptation and fixed quality
//! - **Throughput-based decisions**: Uses network throughput estimation
//! - **Buffer-aware**: Considers buffer level for switching decisions
//! - **Configurable**: Extensive configuration options for tuning behavior
//!
//! ## Example
//!
//! ```rust
//! use kithara_abr::{AbrController, AbrOptions, AbrMode, Variant};
//! use std::time::Instant;
//!
//! // Define your variants
//! let variants = vec![
//!     Variant { variant_index: 0, bandwidth_bps: 500_000 },
//!     Variant { variant_index: 1, bandwidth_bps: 1_000_000 },
//!     Variant { variant_index: 2, bandwidth_bps: 2_000_000 },
//! ];
//!
//! // Create ABR controller in Auto mode with variants
//! let opts = AbrOptions {
//!     mode: AbrMode::Auto(Some(0)),
//!     variants,
//!     ..Default::default()
//! };
//! let controller = AbrController::new(opts);
//!
//! // Get ABR decision
//! let decision = controller.decide(Instant::now());
//! ```

#![forbid(unsafe_code)]
// unimock macro generates `_` patterns for unit args; suppress the clippy lint in test builds
#![cfg_attr(test, allow(clippy::ignored_unit_patterns, clippy::allow_attributes))]

mod controller;
mod estimator;
mod types;

pub use controller::{AbrController, AbrDecision, AbrReason, DefaultAbrController};
pub use estimator::{Estimator, ThroughputEstimator};
pub use types::{
    AbrMode, AbrOptions, ThroughputSample, ThroughputSampleSource, Variant, VariantInfo,
    VariantSource,
};
