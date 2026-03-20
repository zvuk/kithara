#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};

#[kithara::test(
    browser,
    timeout(std::time::Duration::from_secs(5)),
    env(KITHARA_HANG_TIMEOUT_SECS = "1")
)]
async fn executes_inside_browser_runner() {
    #[cfg(target_arch = "wasm32")]
    {
        let global = js_sys::global();
        let has_document = js_sys::Reflect::has(&global, &JsValue::from_str("document"))
            .expect("Reflect::has(document) must succeed");
        let is_worker = global
            .dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
            .is_ok();

        assert!(
            has_document || is_worker,
            "expected browser window or dedicated worker global"
        );
    }
}
