use kithara::platform::thread::set_wasm_shim_name;
use tracing_log::LogTracer;
use tracing_wasm::WASMLayerConfigBuilder;
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

/// wasm-ld synthesizes `__heap_base` and `__heap_end` only when the module does not define them.
#[used]
#[unsafe(export_name = "__heap_end")]
static HEAP_END: u8 = 0;

/// Registers the wasm-bindgen output name so worker threads that import `<shim>.js` for `initSync`
/// load the right shim; auto-detection mis-picks a co-loaded `.js` such as coi-serviceworker.
#[cfg_attr(target_family = "wasm", allow(unreachable_pub))]
#[wasm_bindgen(start)]
pub fn setup() -> Result<(), JsValue> {
    kithara::platform::logging::install_panic_hook();

    set_wasm_shim_name(env!("CARGO_PKG_NAME"));

    if web_sys::window().is_none() {
        return Ok(());
    }

    crate::web::bridge::initialize()?;

    let _ = LogTracer::init();
    let mut config = WASMLayerConfigBuilder::new();
    config
        .set_report_logs_in_timings(false)
        .set_max_level(web_log_level());
    tracing_wasm::set_as_global_default_with_config(config.build());
    Ok(())
}

/// Console verbosity for the web bindings. Without a ceiling every DEBUG and
/// TRACE event from every dependency — the per-block scheduler pass, the
/// audio-graph dump — reaches the browser console on the page and on every
/// worker, which is what a CI lane log collects megabytes of. A verbose build
/// opts back in at compile time.
fn web_log_level() -> tracing::Level {
    match option_env!("KITHARA_WEB_LOG") {
        Some("trace") => tracing::Level::TRACE,
        Some("debug") => tracing::Level::DEBUG,
        _ => tracing::Level::INFO,
    }
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
