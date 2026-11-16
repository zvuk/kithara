use js_sys::Promise;
use kithara_platform::time::Duration;
use kithara_test_utils::kithara;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

pub(in crate::session) const TIMEOUT: Duration = Duration::from_secs(10);

#[wasm_bindgen(module = "/src/session/tests/bootstrap.js")]
extern "C" {
    #[wasm_bindgen(js_name = holdBootstrap)]
    pub(crate) fn hold(timeout_millis: u32) -> Promise;
    #[wasm_bindgen(js_name = releaseBootstrap)]
    pub(crate) fn release(lease: &JsValue, timeout_millis: u32) -> Promise;
    #[wasm_bindgen(js_name = closedOutputContract)]
    fn closed_output_contract(timeout_millis: u32) -> Promise;
}

#[kithara::test(wasm, flash(false), timeout(Duration::from_secs(30)))]
async fn a_closed_queued_output_leaves_bootstrap_admission_available() {
    let millis = TIMEOUT
        .as_millis()
        .try_into()
        .expect("bootstrap timer fits u32");
    JsFuture::from(closed_output_contract(millis))
        .await
        .expect("closed output releases admission without importing; live output waits for ready");
}
