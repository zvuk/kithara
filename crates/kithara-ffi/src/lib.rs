//! Swift FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API using
//! cfg-switchable backends (`UniFFI` or `BoltFFI`).

mod event_bridge;
mod item;
mod observer;
mod player;
#[expect(
    dead_code,
    reason = "types consumed by player/item modules in upcoming subtasks"
)]
mod types;
