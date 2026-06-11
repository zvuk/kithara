#[cfg(test)]
extern crate self as kithara_test_utils;

/// Re-export of `kithara-platform` so the `#[kithara::test]` macro can reach the
/// flash time API (`ambient_scope`, the lexical-rewrite `flash_virtual_*`
/// targets) through a path present in EVERY crate that uses the macro — they all
/// depend on `kithara-test-utils` (it vends the macro), but not all depend on
/// `kithara-platform` directly. The macro emits
/// `::kithara_test_utils::kithara_platform::time::…` for its body-injected
/// flash wrapping.
pub use kithara_platform;

mod flash_dump;
pub mod hang;
pub mod mock;
pub mod probe;
pub mod rtsan;
pub mod test;

pub use flash_dump::flash_dump_to_stderr;

pub mod kithara {
    pub use kithara_test_macros::{
        Probe, fixture, flash, hang_watchdog, mock, probe, rtsan_allow_blocking,
        rtsan_forbid_blocking, test,
    };
}
