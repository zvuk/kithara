//! The runtime handle is asked for in a process that never published it.
//!
//! The device harness links every test binary against `init_context`, so on
//! Android the global is published before the test runs.
#![cfg(not(target_os = "android"))]

use kithara_test_utils::{hang::suppress_expected_panic_dumps, kithara};

#[kithara::test(native, flash(false), timeout(Short))]
fn attach_reports_the_missing_initialization() {
    // The reader turns the platform global's panic into a variant, so the
    // panic this test drives is its contract, not evidence of a hang.
    suppress_expected_panic_dumps();

    let error = kithara_android::attach_current_thread()
        .expect_err("nothing published the runtime handle in this process");

    assert!(
        matches!(error, kithara_android::AndroidBackendError::NotInitialized),
        "expected the missing initialization, got {error}"
    );
}
