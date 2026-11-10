#![cfg(target_arch = "wasm32")]

use js_sys::futures::JsFuture;
use kithara::platform::{
    thread,
    time::{Duration, timeout},
};
use kithara_ffi::{player::AudioPlayer, web::bindings};
use kithara_test_utils::wait_until;
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(module = "/../../tests/fixtures/bootstrap_admission.js")]
extern "C" {
    #[wasm_bindgen(js_name = holdBootstrapImports)]
    fn hold_bootstrap_imports() -> js_sys::Promise;
    #[wasm_bindgen(js_name = releaseBootstrapImports)]
    fn release_bootstrap_imports() -> js_sys::Promise;
    #[wasm_bindgen(js_name = workletImports)]
    fn worklet_imports() -> u32;
    #[wasm_bindgen(js_name = bootstrapRequests)]
    fn bootstrap_requests() -> u32;
    #[wasm_bindgen(js_name = bootstrapLocks)]
    fn bootstrap_locks() -> js_sys::Promise;
    #[wasm_bindgen(js_name = audioContexts)]
    fn audio_contexts() -> String;
}

#[wasm_bindgen_test]
async fn worklet_import_waits_for_worker_bootstrap_admission() {
    assert!(thread::is_main_thread());
    thread::set_wasm_shim_name("wasm-bindgen-test");
    JsFuture::from(hold_bootstrap_imports())
        .await
        .expect("hold bootstrap admission before host construction");
    let setup = bindings::setup();
    let player = AudioPlayer::new_js();
    let command = player.set_volume_js(0.25);
    let observed = wait_until(Duration::from_secs(5), "worklet admission", || {
        worklet_imports() > 0 || bootstrap_requests() > 0
    })
    .await;
    let imported = worklet_imports();
    let requested = bootstrap_requests();
    JsFuture::from(release_bootstrap_imports())
        .await
        .expect("release bootstrap admission");

    setup.expect("browser bindings initialize");
    command.expect("volume command admitted");
    observed.expect("worklet attempts module admission");
    assert_eq!(imported, 0, "worklet import must wait for the held lease");
    assert_eq!(requested, 1, "one host requests one worklet module load");

    let document = web_sys::window()
        .expect("browser window")
        .document()
        .expect("browser document");
    let ready = wait_until(Duration::from_secs(5), "owner after bootstrap", || {
        player.tick_js();
        let click = web_sys::Event::new("click").expect("click event");
        document.dispatch_event(&click).expect("dispatch click");
        player.volume() == 0.25 && player.rt_process_calls_js() > 0
    })
    .await;
    let volume = player.volume();
    let mixer_blocks = player.rt_process_calls_js();
    let contexts = audio_contexts();
    // A stuck snapshot query must not hold up reporting a readiness failure.
    let locks = timeout(
        Duration::from_millis(100),
        JsFuture::from(bootstrap_locks()),
    )
    .await;
    wasm_bindgen_test::console_log!(
        "BOOTSTRAP_READY volume={} mixer_blocks={} audio_contexts={} bootstrap_locks={locks:?}",
        volume,
        mixer_blocks,
        contexts,
    );
    ready.expect("the real mixer and owner run after admission is released");
}
