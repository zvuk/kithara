//! Re-exports for [`kithara::mock`]-style integration consumers.
//!
//! Macro-generated mocks (`PcmReaderMock`, `AudioEffectMock`) are produced
//! by `#[kithara::mock]` next to their trait definitions in `traits.rs`.
//! This module surfaces them as `kithara_audio::mock::*` so the
//! workspace-level `kithara::mock::*` aggregate stays uniform across
//! domain crates.

pub use crate::traits::{AudioEffectMock, PcmReaderMock};
