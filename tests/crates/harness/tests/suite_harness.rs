#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]

mod flash_lexical;
mod timeout_guard;
