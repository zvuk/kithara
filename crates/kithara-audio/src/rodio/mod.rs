//! rodio integration: `rodio::Source` adapters for audio playback.
//!
//! This module is only available when the `rodio` feature is enabled.

mod source_impl;
mod sync;

pub use sync::AudioSyncReader;
