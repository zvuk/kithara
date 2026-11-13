//! A sync `#[kithara::test]` body participates in flash the way an async body
//! does: its synchronous work holds the virtual clock, so a deadline another
//! participant parks on cannot pass while the body is still working. Gated on
//! `flash` (native only).
#![cfg(all(not(target_arch = "wasm32"), feature = "flash"))]

use std::time::Instant as RealInstant;

use kithara_platform::{
    thread,
    time::{Duration, Instant},
};
#[cfg(all(test, target_os = "android"))]
use kithara_test_dylib as _;
use kithara_test_utils::kithara;

#[kithara::test(native)]
fn a_sync_test_body_holds_the_virtual_clock_while_it_works() {
    let started = Instant::now();
    let sleeper = thread::spawn(|| thread::sleep(Duration::from_secs(1)));
    let working = RealInstant::now();
    while working.elapsed() < Duration::from_millis(50) {
        std::hint::spin_loop();
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the clock passed a parked deadline while the test body was still working"
    );
    sleeper.join().expect("sleeper thread must finish");
}
