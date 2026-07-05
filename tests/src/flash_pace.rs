//! Sanctioned virtual-time pacing for code that runs OUTSIDE a test body.
//!
//! Some shared helpers and harness tasks (real-time playback-gap simulators, the
//! test-server network-delay releaser) must advance the *virtual* clock from code
//! that runs outside a `#[kithara::test]` body — where the macro's lexical
//! sleep-rewrite does not reach and stateless time calls (`time::sleep` /
//! `thread::sleep`) default to REAL because they gate on the per-callstack
//! `flash_enabled()` flag, which only the body sets.
//!
//! The ONLY sanctioned way to make such a region follow the test's flash mode is
//! the `#[kithara::flash]` guard macro: it is a no-op off the `flash` feature and
//! off ambient (so `flash(false)` tests keep real timing), and virtual inside an
//! ambient flash test. Raw `flash::enter_dynamic` / `flash::dynamic` are that
//! macro's private expansion and must never be called by hand.

#![cfg(not(target_arch = "wasm32"))]

use kithara::platform::time::Duration;

/// A `thread::sleep` that runs on the flash virtual clock inside an ambient flash
/// test, and on the real clock otherwise.
///
/// Use only for *deliberate* time advance — real-time playback pacing or
/// simulated network latency, where the duration itself is the thing under test.
/// This is NOT a substitute for waiting on program state; for "wait until X
/// happens" use [`crate::waits`].
/// `no_block`: deliberate virtual-time pace; the sleep duration is the behavior under test.
#[kithara::allow_block]
#[kithara::flash(true)]
pub fn virtual_pace(duration: Duration) {
    kithara::platform::thread::sleep(duration);
}
