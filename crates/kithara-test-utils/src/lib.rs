#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "test utility crate — unwraps are acceptable"
)]
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "test utility crate — numeric casts are acceptable for WAV generation"
)]
#![expect(
    clippy::missing_panics_doc,
    reason = "test utility crate — panic documentation not needed"
)]

//! Shared test utilities for the kithara workspace.

pub mod fixtures;
pub mod http_server;
pub mod memory_source;
pub mod rng;
pub mod wav;

pub use fixtures::*;
pub use http_server::TestHttpServer;
pub use rng::*;
pub use wav::{create_saw_wav, create_test_wav};
