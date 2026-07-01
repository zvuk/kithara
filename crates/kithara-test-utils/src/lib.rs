#[cfg(test)]
extern crate self as kithara_test_utils;

/// Re-export of `kithara-platform` so the `#[kithara::test]` macro can reach the
/// flash control surface (`ambient_scope`, the lexical-rewrite `virtual_*`
/// targets) through a path present in EVERY crate that uses the macro — they all
/// depend on `kithara-test-utils` (it vends the macro), but not all depend on
/// `kithara-platform` directly. The macro emits
/// `::kithara_test_utils::kithara_platform::flash::…` for its body-injected
/// flash wrapping.
pub use kithara_platform;

pub mod hang;
pub mod mock;
pub mod probe;
pub mod rtsan;
#[cfg(any(test, feature = "probe"))]
pub mod test;

pub mod kithara {
    pub use kithara_test_macros::{
        Probe, fixture, flash, hang_watchdog, mock, probe, rtsan_allow_blocking,
        rtsan_forbid_blocking, test,
    };
}
