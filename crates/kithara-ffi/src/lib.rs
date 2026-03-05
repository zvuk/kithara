//! Swift FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API using
//! cfg-switchable backends (`UniFFI` or `BoltFFI`).

mod event_bridge;
mod item;
mod observer;
mod player;
mod types;
