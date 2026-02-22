use wasm_bindgen::prelude::wasm_bindgen;

// Set up panic hook and tracing. Called from JS main thread
// before any other player operations.
#[wasm_bindgen]
pub fn setup() {
    console_error_panic_hook::set_once();
    let _ = tracing_log::LogTracer::init();
    tracing_wasm::set_as_global_default();
}

/// Build revision string: "version git_hash build_timestamp"
#[wasm_bindgen]
pub fn build_info() -> String {
    format!(
        "v{} {} {}",
        env!("CARGO_PKG_VERSION"),
        env!("BUILD_GIT_HASH"),
        env!("BUILD_TIMESTAMP"),
    )
}
