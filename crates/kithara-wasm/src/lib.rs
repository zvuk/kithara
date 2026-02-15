// WASM HLS player library entry point.

#[cfg(target_arch = "wasm32")]
mod player;

#[cfg(target_arch = "wasm32")]
pub use player::WasmPlayer;

// Re-export wasm_memory for JS to access SharedArrayBuffer.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn wasm_memory() -> wasm_bindgen::JsValue {
    wasm_bindgen::memory()
}

// Re-export initThreadPool from wasm-bindgen-rayon.
// JS calls: `await initThreadPool(navigator.hardwareConcurrency)`
#[cfg(all(target_arch = "wasm32", feature = "threads"))]
pub use wasm_bindgen_rayon::init_thread_pool;
