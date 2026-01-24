//! Adaptive Bitrate (ABR) streaming algorithm.
//!
//! This crate provides a protocol-agnostic ABR controller for adaptive streaming.
//! It works with any streaming protocol (HLS, DASH, etc.) through the `VariantSource` trait.
//!
//! ## Features
//!
//! - **Protocol-agnostic**: Works with any streaming protocol via trait abstraction
//! - **Auto and Manual modes**: Supports both automatic adaptation and fixed quality
//! - **Throughput-based decisions**: Uses network throughput estimation
//! - **Buffer-aware**: Considers buffer level for switching decisions
//! - **Configurable**: Extensive configuration options for tuning behavior
//!
//! ## Example
//!
//! ```rust
//! use kithara_abr::{AbrController, AbrOptions, AbrMode, VariantSource};
//! use std::time::Instant;
//!
//! // Define your variants
//! struct MyVariants {
//!     bandwidths: Vec<u64>,
//! }
//!
//! impl VariantSource for MyVariants {
//!     fn variant_count(&self) -> usize {
//!         self.bandwidths.len()
//!     }
//!
//!     fn variant_bandwidth(&self, index: usize) -> Option<u64> {
//!         self.bandwidths.get(index).copied()
//!     }
//! }
//!
//! // Create ABR controller in Auto mode
//! let opts = AbrOptions {
//!     mode: AbrMode::Auto(Some(0)),
//!     ..Default::default()
//! };
//! let controller = AbrController::new(opts);
//!
//! // Get ABR decision
//! let variants = MyVariants {
//!     bandwidths: vec![500_000, 1_000_000, 2_000_000],
//! };
//! let decision = controller.decide(&variants, Instant::now());
//! ```

#![forbid(unsafe_code)]

mod controller;
mod estimator;
mod types;

pub use controller::{AbrController, AbrDecision, AbrReason, DefaultAbrController};
pub use estimator::{Estimator, ThroughputEstimator};
pub use types::{
    AbrMode, AbrOptions, ThroughputSample, ThroughputSampleSource, Variant, VariantInfo,
    VariantSource,
};
