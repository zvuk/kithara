use tracing_log::LogTracer;
use tracing_wasm::WASMLayerConfigBuilder;
use wasm_bindgen::prelude::wasm_bindgen;

#[cfg_attr(target_family = "wasm", allow(unreachable_pub))]
#[wasm_bindgen(start)]
pub fn setup() {
    console_error_panic_hook::set_once();

    if web_sys::window().is_none() {
        return;
    }

    let _ = LogTracer::init();
    let config = WASMLayerConfigBuilder::new()
        .set_report_logs_in_timings(false)
        .build();
    tracing_wasm::set_as_global_default_with_config(config);
}

/// Build revision string: `"version git_hash build_timestamp"`.
#[must_use]
#[wasm_bindgen]
pub fn build_info() -> String {
    format!(
        "v{} {} {}",
        env!("CARGO_PKG_VERSION"),
        env!("BUILD_GIT_HASH"),
        env!("BUILD_TIMESTAMP"),
    )
}
