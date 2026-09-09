#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

//! Integration tests for kithara-net

mod http_client;
mod retry;
mod timeout;
