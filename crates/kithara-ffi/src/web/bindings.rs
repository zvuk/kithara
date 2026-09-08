use kithara::platform::thread::set_wasm_shim_name;
use tracing_log::LogTracer;
use tracing_wasm::WASMLayerConfigBuilder;
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

// WHY: wasm-ld synthesizes `__heap_base` and `__heap_end` only when the module does not define them.
#[used]
#[unsafe(export_name = "__heap_end")]
static HEAP_END: u8 = 0;

#[cfg_attr(target_family = "wasm", allow(unreachable_pub))]
#[wasm_bindgen(start)]
pub fn setup() -> Result<(), JsValue> {
    kithara::platform::logging::install_panic_hook();

    // WHY: Worker threads import `<shim>.js` for `initSync`; register our wasm-bindgen output name so the engine worker loads the right
    // shim (auto-detection mis-picks a co-loaded `.js` like coi-serviceworker).
    set_wasm_shim_name(env!("CARGO_PKG_NAME"));

    if web_sys::window().is_none() {
        return Ok(());
    }

    crate::web::bridge::initialize()?;

    let _ = LogTracer::init();
    let config = WASMLayerConfigBuilder::new()
        .set_report_logs_in_timings(false)
        .build();
    tracing_wasm::set_as_global_default_with_config(config);
    Ok(())
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
