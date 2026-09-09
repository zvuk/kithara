//! B4 lexical body-only flash containment: `#[kithara::test(flash(true))]`
//! rewrites the test BODY's direct time-primitive calls onto the unconditional
//! `virtual_*` engine variants, so the body's own time reads collapse onto
//! the VIRTUAL clock WITHOUT setting `FLASH_ACTIVE`. A prod-like fn the body
//! calls is NOT rewritten and its stateless time reads see `FLASH_ACTIVE` false,
//! so they stay on the REAL clock. This is the lexical/body-only containment.
//! Gated on `flash` (native only).
#![cfg(all(not(target_arch = "wasm32"), feature = "flash"))]

use kithara::{
    self,
    platform::time::{self, Duration, Instant},
};

/// Prod-like fn WITHOUT a flash annotation. Its `Instant::now` is NOT rewritten
/// and reads `flash_enabled()` (false), so it observes the REAL monotonic clock
/// — unaffected by the body advancing the VIRTUAL clock. Returns the instant it
/// sampled so the caller can compare clocks.
async fn unannotated_prod_now() -> Instant {
    // Sentinel to keep this a genuine async callee with its own body.
    time::sleep(Duration::from_millis(1)).await;
    Instant::now()
}

#[kithara::test(tokio, flash(true), timeout(Duration::from_secs(5)))]
async fn lexical_collapses_body_not_callee() {
    // BODY: `Instant::now` and `time::sleep` are lexically rewritten to the
    // unconditional `virtual_*` engine variants. The body sleep registers
    // a virtual deadline; the engine collapses it and jumps the VIRTUAL clock by
    // a full hour. Both `body_before`/`body_after` read the VIRTUAL clock.
    let body_before = Instant::now();
    time::sleep(Duration::from_secs(3600)).await;
    let body_after = Instant::now();
    let body_jump = body_after.duration_since(body_before);
    assert!(
        body_jump >= Duration::from_secs(3000),
        "body time-reads track the virtual clock that jumped ~1h (jump = {body_jump:?})"
    );

    // CALLEE: NOT rewritten. Its `Instant::now` reads `flash_enabled()` (false)
    // → the REAL monotonic clock, which has barely moved (the hour was virtual,
    // collapsed to ~0 real). So the callee's instant is ENORMOUSLY behind the
    // body's virtual instant — proving the rewrite did not leak into the callee.
    let callee_now = unannotated_prod_now().await;
    let body_ahead_of_callee = body_after.duration_since(callee_now);
    assert!(
        body_ahead_of_callee >= Duration::from_secs(3000),
        "callee Instant::now stayed on the REAL clock (body virtual instant is \
         ~1h ahead: delta = {body_ahead_of_callee:?})"
    );
}
