// WASM HLS player library entry point.

#[cfg(target_arch = "wasm32")]
mod player;

// Re-export for wasm-bindgen.
#[cfg(target_arch = "wasm32")]
pub use player::WasmPlayer;
