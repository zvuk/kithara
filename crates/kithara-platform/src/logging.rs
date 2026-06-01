/// Emit an error-level diagnostic line through the platform-appropriate sink.
///
/// On native this routes through `tracing`. On wasm it writes to the browser
/// `console` directly rather than through the global `tracing` subscriber: on
/// a non-main wasm instance (e.g. the audio worklet) that subscriber is a
/// `dyn` object whose vtable lives in the main instance's function table, so
/// dispatching to it cross-instance would trap. `console.error` is a per-realm
/// import that is valid in every scope, including `AudioWorkletGlobalScope`.
#[cfg(target_arch = "wasm32")]
pub fn log_error(msg: &str) {
    web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(msg));
}

#[cfg(not(target_arch = "wasm32"))]
pub fn log_error(msg: &str) {
    tracing::error!(target: "kithara_platform", "{msg}");
}
