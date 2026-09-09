#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;

#[path = "engine_cpal_tests.rs"]
mod engine_cpal_tests;
#[path = "engine_session_contract.rs"]
mod engine_session_contract;
