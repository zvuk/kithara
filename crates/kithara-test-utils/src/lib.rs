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
#[cfg(not(target_arch = "wasm32"))]
pub mod flight;
pub mod hang;
pub mod memory;
#[cfg(feature = "mock")]
pub mod mock;
pub mod no_block;
#[cfg(not(target_arch = "wasm32"))]
pub mod off_thread;
pub mod probe;
pub mod rtsan;
pub mod test;

pub mod kithara {
    pub use kithara_test_macros::{
        IntoProbeArg, Probe, allow_block, asset, fixture, hang_watchdog, measure, measure_block,
        mock, no_block, probe, probe_event, rtsan_allow_blocking, rtsan_forbid_blocking, test,
        test_utils_flash as flash,
    };
}
mod consts;
