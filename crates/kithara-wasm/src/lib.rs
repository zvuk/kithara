// WASM HLS player library entry point.

#[cfg(feature = "internal")]
pub mod internal;

#[cfg(target_arch = "wasm32")]
mod bindings;

#[cfg(target_arch = "wasm32")]
mod player;

#[cfg(target_arch = "wasm32")]
pub use bindings::{build_info, setup};
#[cfg(target_arch = "wasm32")]
pub use player::WasmPlayer;
// Re-export initThreadPool from wasm-bindgen-rayon.
// JS calls: `await initThreadPool(navigator.hardwareConcurrency)`
#[cfg(all(target_arch = "wasm32", feature = "threads"))]
pub use wasm_bindgen_rayon::init_thread_pool;
