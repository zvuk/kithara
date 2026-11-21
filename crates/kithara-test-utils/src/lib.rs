#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

extern crate self as kithara_test_utils;

#[cfg(all(test, target_arch = "wasm32"))]
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

/// Re-export of `kithara-platform` so the `#[kithara::test]` macro can reach the
/// flash control surface through a path present in EVERY crate that uses the
/// macro. They all depend on `kithara-test-utils`, but not all depend on
/// `kithara-platform` directly. The macro emits
/// `::kithara_test_utils::kithara_platform::flash::…` for its body-injected
/// flash wrapping.
pub use kithara_platform;
/// Native serialization runtime used by generated test wrappers.
#[cfg(not(target_arch = "wasm32"))]
pub use serial_test;
/// Re-exported for the platform-independent USDT tracing backend emitted by
/// `#[kithara::probe]`.
pub use tracing;

pub mod bufpool;
#[cfg(feature = "test")]
pub mod cancel;
pub mod clock;
#[cfg(not(target_arch = "wasm32"))]
pub mod flight;
pub mod hang;
#[cfg(all(feature = "http-server", not(target_arch = "wasm32")))]
pub mod http_server;
#[cfg(not(target_arch = "wasm32"))]
pub mod load;
pub mod memory;
#[cfg(feature = "mock")]
pub mod mock;
pub mod no_block;
#[cfg(not(target_arch = "wasm32"))]
pub mod off_thread;
#[cfg(not(target_arch = "wasm32"))]
pub mod pace;
pub mod probe;
pub mod rng;
pub mod rtsan;
#[cfg(feature = "temp-dir")]
pub mod temp_dir;
#[cfg(feature = "test")]
pub mod test;
pub mod wait;

#[cfg(feature = "test")]
pub use cancel::{cancel_token, cancel_token_cancelled};
#[cfg(all(feature = "http-server", not(target_arch = "wasm32")))]
pub use http_server::{TestHttpServer, free_loopback_port};
#[cfg(not(target_arch = "wasm32"))]
pub use pace::pace;
pub use rng::Xorshift64;
#[cfg(feature = "temp-dir")]
pub use temp_dir::{TestTempDir, temp_dir, temp_path};
#[cfg(feature = "test")]
pub use test::TestBudget;
pub use wait::{wait_until, wait_until_blocking};

/// Test and instrumentation attributes.
/// A native synchronous test with an active `should_panic` must return unit,
/// including when the harness serializes it.
///
/// ```compile_fail,E0308,test_harness
/// use kithara_test_utils::kithara;
/// #[kithara::test(serial)]
/// #[should_panic]
/// fn returned_error() -> Result<(), &'static str> { Err("body failed") }
/// ```
///
/// ```compile_fail,E0308,test_harness
/// use kithara_test_utils::kithara;
/// type TestResult = Result<(), &'static str>;
/// #[kithara::test(native, serial)]
/// #[cfg_attr(all(), cfg_attr(all(), should_panic))]
/// fn returned_error() -> TestResult { Err("body failed") }
/// ```
pub mod kithara {
    pub use kithara_test_macros::{
        IntoProbeArg, Probe, allow_block, hang_watchdog, measure, measure_block, mock, no_block,
        probe, probe_event, rtsan_allow_blocking, rtsan_forbid_blocking, test_utils_flash as flash,
    };
    #[cfg(feature = "test")]
    pub use kithara_test_macros::{asset, fixture, test};
}
mod consts;
