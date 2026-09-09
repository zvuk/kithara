#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate - unwraps are acceptable in test code"
)]

pub use kithara_integration_tests::bufpool_ext;

#[path = "silvercomet_seek_hang.rs"]
mod silvercomet_seek_hang;
