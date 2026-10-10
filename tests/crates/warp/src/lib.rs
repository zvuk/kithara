//! Warp integration test support.

#[cfg(all(feature = "mock", feature = "playback", not(target_arch = "wasm32")))]
pub mod mock;
