//! Swift FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API using
//! cfg-switchable backends (`UniFFI` or `BoltFFI`).

#[cfg(feature = "backend-uniffi")]
uniffi::setup_scaffolding!();

#[expect(dead_code, reason = "wired up when AudioPlayer starts the bridge")]
mod event_bridge;
pub mod item;
pub mod observer;
pub mod player;
pub mod types;
