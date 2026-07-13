use kithara_test_utils::kithara;

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn receive_is_nonblocking(preloaded: bool, block_on_underrun: bool) -> bool {
    preloaded && !block_on_underrun
}

#[cfg(target_arch = "wasm32")]
pub(super) fn receive_is_nonblocking(_preloaded: bool, _block_on_underrun: bool) -> bool {
    true
}

#[cfg(not(target_arch = "wasm32"))]
#[kithara::allow_block]
pub(super) fn wait_for_fetch(timeout: kithara_platform::time::Duration) {
    kithara_platform::thread::park_timeout(timeout);
}

#[cfg(target_arch = "wasm32")]
#[kithara::allow_block]
pub(super) fn wait_for_fetch(timeout: kithara_platform::time::Duration) {
    if kithara_platform::thread::is_worker_thread() {
        kithara_platform::thread::park_timeout(timeout);
    } else {
        kithara_platform::thread::sleep(timeout);
    }
}
