#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]
#![forbid(unsafe_code)]
#![expect(
    clippy::unwrap_used,
    reason = "integration test crate — unwraps are acceptable in test code"
)]
//! `PlayerConfig<S>`'s generic depth, inside this crate's large aggregate of
//! async test modules, pushes the type-layout query past the default limit for
//! `run_crossfade_flac_case` and its kin.
#![recursion_limit = "256"]

mod thread_budget;
