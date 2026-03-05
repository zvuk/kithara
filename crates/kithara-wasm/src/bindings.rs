use wasm_bindgen::prelude::wasm_bindgen;

// Set up panic hook and tracing. Called automatically during WASM module
// initialisation (`initSync`). On Worker threads, skip all setup: the main
// thread already installed the global subscriber and panic hook, and
// allocating here would race with the main thread's dlmalloc spin lock,
// corrupting the shared WASM heap.
#[allow(unreachable_pub)]
#[wasm_bindgen(start)]
pub fn setup() {
    // Always install panic hook — even on Workers — so panics are visible.
    console_error_panic_hook::set_once();

    // Skip tracing setup on Workers: the main thread already installed the
    // global subscriber, and allocating the tracing layer here would race
    // with the main thread's dlmalloc spin lock.
    if web_sys::window().is_none() {
        return;
    }

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
