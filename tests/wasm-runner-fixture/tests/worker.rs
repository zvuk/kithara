#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

/// Passes only when the runner serves the page cross-origin isolated, as the
/// suites need for `SharedArrayBuffer`.
#[wasm_bindgen_test]
fn passes() {
    let isolated = js_sys::Reflect::get(&js_sys::global(), &"crossOriginIsolated".into())
        .ok()
        .and_then(|value| value.as_bool());
    assert_eq!(
        isolated,
        Some(true),
        "the page is not cross-origin isolated"
    );
}

#[wasm_bindgen_test]
fn fails() {
    panic!("the runner protocol fixture fails on purpose");
}

#[wasm_bindgen_test]
#[ignore = "the runner reports it as ignored unless asked to include it"]
fn ignored() {}
