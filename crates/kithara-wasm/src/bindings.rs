use wasm_bindgen::prelude::wasm_bindgen;

// Set up panic hook and tracing. Called from JS main thread
// before any other player operations.
#[wasm_bindgen]
pub fn setup() {
    console_error_panic_hook::set_once();
    let _ = tracing_log::LogTracer::init();
    // Disable `report_logs_in_timings` — the subscriber is shared via
    // shared memory with the AudioWorklet, where `performance.mark()`
    // is not available and would crash the audio thread.
    let config = tracing_wasm::WASMLayerConfigBuilder::new()
        .set_report_logs_in_timings(false)
        .build();
    tracing_wasm::set_as_global_default_with_config(config);
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
