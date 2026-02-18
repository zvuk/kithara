// WASM HLS player library entry point.

pub mod ring_buffer;

#[cfg(target_arch = "wasm32")]
mod player;

#[cfg(target_arch = "wasm32")]
pub use player::WasmPlayer;

// Set up panic hook and tracing. Called from JS main thread
// before any other player operations.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn setup() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();
}

/// Build revision string: "version git_hash build_timestamp"
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn build_info() -> String {
    format!(
        "v{} {} {}",
        env!("CARGO_PKG_VERSION"),
        env!("BUILD_GIT_HASH"),
        env!("BUILD_TIMESTAMP"),
    )
}

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
