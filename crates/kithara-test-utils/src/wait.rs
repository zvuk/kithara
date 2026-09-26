//! The one state-wait primitive.
//!
//! Its single virtual poll-tick `sleep` is the only timer `sleep` in the test
//! surface. Under the flash virtual clock that tick advances time between
//! predicate checks, so a wait resolves the instant the program reaches the
//! asserted state — never on a wall-clock guess. The `deadline` argument bounds
//! only a non-progress watchdog that returns `Err` (callers `.expect()` it); it
//! is never an early-success path that lets a later assertion pass on timeout.

use kithara_platform::time::{Duration, Instant, sleep};

use crate::kithara;

/// Poll cadence for [`wait_until`]: a virtual tick that advances the flash
/// clock so the engine runs between predicate checks.
pub const POLL_TICK: Duration = Duration::from_millis(20);

/// Re-checks `cond` after every virtual tick until it is true, or returns `Err`
/// when `deadline` (virtual time) elapses without progress. Callers `.expect()`
/// the result, so a genuinely wedged pipeline panics with `label` instead of
/// silently letting a later assertion pass.
///
/// # Errors
///
/// Returns the formatted watchdog message when `deadline` elapses first.
#[kithara::flash(true)]
pub async fn wait_until<F>(deadline: Duration, label: &str, mut cond: F) -> Result<(), String>
where
    F: FnMut() -> bool,
{
    let start = Instant::now();
    loop {
        if cond() {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(format!(
                "wait_until({label}) exceeded {deadline:?} without reaching the target state"
            ));
        }
        sleep(POLL_TICK).await;
    }
}
