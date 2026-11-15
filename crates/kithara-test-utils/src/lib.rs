#![cfg_attr(all(rtsan, not(rtsan_standalone)), feature(sanitize))]

extern crate self as kithara_test_utils;

/// Re-export of `kithara-platform` so the `#[kithara::test]` macro can reach the
/// flash control surface through a path present in EVERY crate that uses the
/// macro. They all depend on `kithara-test-utils`, but not all depend on
/// `kithara-platform` directly. The macro emits
/// `::kithara_test_utils::kithara_platform::flash::…` for its body-injected
/// flash wrapping.
pub use kithara_platform;
/// Native serialization runtime used by generated test wrappers.
#[cfg(all(feature = "test", not(target_arch = "wasm32")))]
pub use serial_test;
/// Re-exported for the platform-independent USDT tracing backend emitted by
/// `#[kithara::probe]`.
pub use tracing;

#[cfg(feature = "test")]
pub mod bufpool;
#[cfg(feature = "test")]
pub mod cancel;
pub mod clock;
#[cfg(not(target_arch = "wasm32"))]
pub mod flight;
pub mod hang;
#[cfg(all(feature = "http-server", not(target_arch = "wasm32")))]
pub mod http_server;
#[cfg(all(feature = "test", not(target_arch = "wasm32")))]
pub mod load;
#[cfg(feature = "test")]
pub mod memory;
#[cfg(feature = "mock")]
pub mod mock;
pub mod no_block;
#[cfg(all(feature = "test", not(target_arch = "wasm32")))]
pub mod off_thread;
#[cfg(all(feature = "test", not(target_arch = "wasm32")))]
pub mod pace;
pub mod probe;
#[cfg(feature = "test")]
pub mod rng;
pub mod rtsan;
#[cfg(feature = "temp-dir")]
pub mod temp_dir;
#[cfg(feature = "test")]
pub mod test;
#[cfg(feature = "test")]
pub mod wait;

#[cfg(feature = "test")]
pub use cancel::{cancel_token, cancel_token_cancelled};
#[cfg(all(feature = "http-server", not(target_arch = "wasm32")))]
pub use http_server::TestHttpServer;
#[cfg(all(feature = "test", not(target_arch = "wasm32")))]
pub use pace::pace;
#[cfg(feature = "test")]
pub use rng::Xorshift64;
#[cfg(feature = "temp-dir")]
pub use temp_dir::{TestTempDir, temp_dir, temp_path};
#[cfg(feature = "test")]
pub use wait::{wait_until, wait_until_blocking};

pub mod kithara {
    pub use kithara_test_macros::{
        IntoProbeArg, Probe, allow_block, hang_watchdog, measure, measure_block, mock, no_block,
        probe, probe_event, rtsan_allow_blocking, rtsan_forbid_blocking, test_utils_flash as flash,
    };
    #[cfg(feature = "test")]
    pub use kithara_test_macros::{asset, fixture, test};
}
#[cfg(feature = "test")]
mod consts;
