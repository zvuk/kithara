//! The one state-wait primitive, in an async and a blocking form.
//!
//! Its virtual poll-tick sleep is the only timer sleep in the test surface.
//! Under the flash virtual clock that tick advances time between
//! predicate checks, so a wait resolves the instant the program reaches the
//! asserted state — never on a wall-clock guess. The `deadline` argument bounds
//! only a non-progress watchdog that returns `Err` (callers `.expect()` it); it
//! is never an early-success path that lets a later assertion pass on timeout.

use kithara_platform::{
    thread,
    time::{Duration, Instant, sleep},
};

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
        if let Some(done) = settled(start, deadline, label, &mut cond) {
            return done;
        }
        sleep(POLL_TICK).await;
    }
}

/// [`wait_until`] for a synchronous driver: the same predicate and deadline,
/// with the poll tick taken as a blocking virtual sleep.
///
/// # Errors
///
/// Returns the formatted watchdog message when `deadline` elapses first.
#[kithara::flash(true)]
pub fn wait_until_blocking<F>(deadline: Duration, label: &str, mut cond: F) -> Result<(), String>
where
    F: FnMut() -> bool,
{
    let start = Instant::now();
    loop {
        if let Some(done) = settled(start, deadline, label, &mut cond) {
            return done;
        }
        thread::sleep(POLL_TICK);
    }
}

fn settled(
    start: Instant,
    deadline: Duration,
    label: &str,
    cond: &mut impl FnMut() -> bool,
) -> Option<Result<(), String>> {
    if cond() {
        return Some(Ok(()));
    }
    (start.elapsed() >= deadline).then(|| {
        Err(format!(
            "wait_until({label}) exceeded {deadline:?} without reaching the target state"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "flash")]
    #[kithara::test(native)]
    fn a_macro_sync_driver_keeps_deadlines_behind_its_work() {
        use std::{
            future::Future,
            task::{Context, Waker},
        };

        let started = Instant::now();
        let wall_started = kithara_platform::time::WallInstant::now();
        let mut timer = Box::pin(sleep(Duration::from_secs(17)));
        let mut context = Context::from_waker(Waker::noop());
        assert!(timer.as_mut().poll(&mut context).is_pending());
        assert!(
            timer.as_mut().poll(&mut context).is_pending(),
            "runnable synchronous work must keep a registered timer pending"
        );
        assert_eq!(Instant::now(), started);
        drop(timer);

        let child = thread::spawn(|| {
            thread::sleep(Duration::from_secs(17));
            7_u8
        });
        assert_eq!(child.join().expect("child completes after driver parks"), 7);
        assert_eq!(started.elapsed(), Duration::from_secs(17));
        assert!(wall_started.elapsed() < Duration::from_secs(1));
    }

    #[kithara::test(native, tokio)]
    async fn both_wait_forms_check_the_predicate_before_a_zero_deadline() {
        assert_eq!(wait_until(Duration::ZERO, "ready", || true).await, Ok(()));
        assert_eq!(
            wait_until_blocking(Duration::ZERO, "ready", || true),
            Ok(())
        );
        let expected =
            Err("wait_until(stalled) exceeded 0ns without reaching the target state".to_owned());
        assert_eq!(
            wait_until(Duration::ZERO, "stalled", || false).await,
            expected
        );
        assert_eq!(
            wait_until_blocking(Duration::ZERO, "stalled", || false),
            expected
        );
    }

    #[kithara::test(native)]
    fn a_blocking_wait_releases_the_clock_for_its_child() {
        kithara_platform::flash::participate_sync(|| {
            let ready = kithara_platform::sync::Arc::new(kithara_platform::sync::Mutex::new(false));
            let child_ready = ready.clone();
            let child = thread::spawn(move || {
                thread::sleep(Duration::from_millis(50));
                *child_ready.lock() = true;
            });
            wait_until_blocking(Duration::from_secs(1), "child state", || *ready.lock())
                .expect("the child publishes its state");
            child.join().expect("the child exits normally");
        });
    }
}
