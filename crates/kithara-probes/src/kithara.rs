//! Namespace re-export so consumers can write `#[kithara::probe]`.
//!
//! Mirrors the `kithara_test_utils::kithara::test` pattern.

pub use kithara_probe_macros::{Probe, probe};
