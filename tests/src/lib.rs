#![deny(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::impl_trait_in_params,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::option_if_let_else,
    clippy::unwrap_used
)]

pub mod abr_fixtures;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple_warmup;
pub mod asset_fixture;
pub mod audio_fixture;
pub mod audio_mock;
pub mod hls_fixture;
#[cfg(not(target_arch = "wasm32"))]
pub mod hls_test_helpers;
pub mod memory_source;
#[cfg(not(target_arch = "wasm32"))]
pub mod net_fixture;
#[cfg(not(target_arch = "wasm32"))]
pub mod offline;
pub mod signal_source;

pub use abr_fixtures::{abr_fast, abr_initial_mode, abr_switch_trigger};
