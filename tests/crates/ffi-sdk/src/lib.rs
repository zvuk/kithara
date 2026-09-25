//! Test-only transport and lifecycle model for generated SDK acceptance.

mod api;
pub use api::*;

uniffi::setup_scaffolding!();
