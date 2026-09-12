#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

//! Integration tests for kithara-net

use kithara_test_dylib as _;

mod http_client;
mod retry;
mod timeout;
