#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

#[wasm_bindgen_test]
fn passes() {}

#[wasm_bindgen_test]
fn fails() {
    panic!("the runner protocol fixture fails on purpose");
}

#[wasm_bindgen_test]
#[ignore = "the runner reports it as ignored unless asked to include it"]
fn ignored() {}
