//! Swift FFI adapter for the kithara audio player.
//!
//! Wraps `kithara-play` types behind an FFI-friendly API using
//! cfg-switchable backends (`UniFFI` or `BoltFFI`).

// All modules are pub(crate) and will be re-exported via UniFFI proc-macros
// in Phase 2. Until then, nothing is externally visible.
#[expect(
    dead_code,
    reason = "public FFI surface added in Phase 2 (UniFFI integration)"
)]
mod event_bridge;
#[expect(
    dead_code,
    reason = "public FFI surface added in Phase 2 (UniFFI integration)"
)]
mod item;
#[expect(
    dead_code,
    reason = "public FFI surface added in Phase 2 (UniFFI integration)"
)]
mod observer;
#[expect(
    dead_code,
    reason = "public FFI surface added in Phase 2 (UniFFI integration)"
)]
mod player;
mod types;
