#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "performance test crate — unwraps are acceptable in test code"
)]

mod abr;
mod decoder;
mod pool;
mod resampler;
mod storage;
