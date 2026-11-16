use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen(module = "/src/session/tests/bootstrap.js")]
extern "C" {
    #[wasm_bindgen(js_name = holdBootstrap)]
    pub(crate) fn hold() -> Promise;
    #[wasm_bindgen(js_name = releaseBootstrap)]
    pub(crate) fn release(lease: &JsValue) -> Promise;
    #[wasm_bindgen(js_name = closedOutputContract)]
    fn closed_output_contract() -> Promise;
}

#[wasm_bindgen_test::wasm_bindgen_test]
async fn a_closed_queued_output_leaves_bootstrap_admission_available() {
    JsFuture::from(closed_output_contract())
        .await
        .expect("closed output releases admission without importing; live output waits for ready");
}
