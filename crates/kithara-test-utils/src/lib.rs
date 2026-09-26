#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

extern crate self as kithara_test_utils;

/// Re-export of `kithara-platform` so the `#[kithara::test]` macro can reach the
/// flash control surface (`ambient_scope`, the lexical-rewrite `virtual_*`
/// targets) through a path present in EVERY crate that uses the macro — they all
/// depend on `kithara-test-utils` (it vends the macro), but not all depend on
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
pub mod cancel;
#[cfg(not(target_arch = "wasm32"))]
pub mod flight;
pub mod hang;
#[cfg(all(feature = "http-server", not(target_arch = "wasm32")))]
pub mod http_server;
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
pub mod test;
#[cfg(not(target_arch = "wasm32"))]
pub mod wait;

pub use cancel::{cancel_token, cancel_token_cancelled};
#[cfg(all(feature = "http-server", not(target_arch = "wasm32")))]
pub use http_server::TestHttpServer;
#[cfg(not(target_arch = "wasm32"))]
pub use pace::virtual_pace;
pub use rng::Xorshift64;
#[cfg(feature = "temp-dir")]
pub use temp_dir::{TestTempDir, temp_dir, temp_path};
#[cfg(not(target_arch = "wasm32"))]
pub use wait::wait_until;

pub mod kithara {
    pub use kithara_test_macros::{
        IntoProbeArg, Probe, allow_block, asset, fixture, hang_watchdog, measure, measure_block,
        mock, no_block, probe, probe_event, rtsan_allow_blocking, rtsan_forbid_blocking, test,
        test_utils_flash as flash,
    };
}
