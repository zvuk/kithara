//! Public helpers for WASM Worker architecture.
//!
//! Called by `kithara-wasm` to set up and drive the session host on the
//! main thread while the player engine runs in a Web Worker.

use crate::impls::session_engine;

/// Ensure the main-thread session client exists in `Local` mode.
///
/// Must be called on the main thread **before** [`init_worker_session`]
/// so that the main thread gets a `Local` session and Workers get `Remote`.
pub fn ensure_main_session() {
    session_engine::session_client();
}

/// Initialise the Worker ↔ main-thread session channel.
///
/// Must be called **once** on the main thread **before** any Worker
/// calls [`PlayerImpl::new`](crate::PlayerImpl::new).
pub fn init_worker_session() {
    session_engine::init_worker_channel();
}

/// Poll pending session commands from Workers and update the audio graph.
///
/// Call this on the main thread from `requestAnimationFrame`.
pub fn tick_and_poll() {
    session_engine::tick_and_poll_remote();
}

/// Current playback position in seconds (read from shared atomics).
pub fn bridge_position_secs() -> f64 {
    session_engine::bridge_position_secs()
}

/// Current media duration in seconds (read from shared atomics).
pub fn bridge_duration_secs() -> f64 {
    session_engine::bridge_duration_secs()
}

/// Whether playback is active (read from shared atomics).
pub fn bridge_is_playing() -> bool {
    session_engine::bridge_is_playing()
}

/// Audio-thread process count (read from shared atomics).
pub fn bridge_process_count() -> u64 {
    session_engine::bridge_process_count()
}
