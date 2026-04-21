#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
use std::env;
#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(not(feature = "disable-hang-detector"))]
use web_time::Instant;

/// Callback invoked the first time a hang is detected. Runs before the
/// native panic (or before the WASM error log), so it can dump state
/// into a file / logs while the hang is still observable.
#[cfg(not(feature = "disable-hang-detector"))]
pub(crate) type OnHangCallback = Box<dyn Fn() + Send + Sync>;

/// Watchdog that detects loops stuck without progress.
///
/// On native targets `tick()` panics after the deadline.
/// On `wasm32` it logs an error and resets — panics are fatal there
/// (`panic = "immediate-abort"` turns every panic into
/// `RuntimeError: unreachable` which kills the Web Worker).
#[cfg(not(feature = "disable-hang-detector"))]
pub struct HangDetector {
    deadline: Instant,
    label: &'static str,
    timeout: Duration,
    /// Optional pre-panic / pre-log hook. Called exactly once per hang
    /// window so a consumer can dump state (Timeline flags, scheduler,
    /// pending fetches) while the process is still alive.
    on_hang: Option<OnHangCallback>,
    /// Set after `on_hang` fires for the current window; cleared by
    /// `reset()`. Prevents duplicate dumps when `tick()` is called
    /// multiple times past the deadline (WASM path resets the deadline
    /// internally, native path panics so sees it once — but this keeps
    /// the contract uniform).
    fired: bool,
}

#[cfg(not(feature = "disable-hang-detector"))]
impl HangDetector {
    #[must_use]
    pub fn new(label: &'static str, timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            label,
            timeout,
            on_hang: None,
            fired: false,
        }
    }

    /// Attach a state-dump callback invoked once per hang window before
    /// the panic / error log. Useful for writing a `/tmp/*.json` snapshot
    /// of the frozen subsystem so post-mortem analysis isn't limited to
    /// a stacktrace.
    #[must_use]
    pub fn with_on_hang<F>(mut self, callback: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.on_hang = Some(Box::new(callback));
        self
    }

    /// Check progress.
    ///
    /// # Panics
    ///
    /// On native targets, panics when no progress is observed before the
    /// configured timeout. On `wasm32` logs an error to the console instead.
    pub fn tick(&mut self) {
        if Instant::now() < self.deadline {
            return;
        }

        if !self.fired {
            if let Some(cb) = &self.on_hang {
                cb();
            }
            self.fired = true;
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            panic!(
                "[HangDetector] `{}` no progress for {:?} — likely deadlock or hang",
                self.label, self.timeout,
            );
        }

        #[cfg(target_arch = "wasm32")]
        {
            tracing::error!(
                label = self.label,
                timeout_secs = self.timeout.as_secs(),
                "[HangDetector] no progress — possible deadlock or hang"
            );
            // Reset to avoid busy-logging every tick.
            self.deadline = Instant::now() + self.timeout;
        }
    }

    pub fn reset(&mut self) {
        self.deadline = Instant::now() + self.timeout;
        self.fired = false;
    }
}

/// Zero-cost no-op stub when hang detection is disabled.
#[cfg(feature = "disable-hang-detector")]
pub struct HangDetector;

#[cfg(feature = "disable-hang-detector")]
impl HangDetector {
    #[inline(always)]
    #[must_use]
    pub fn new(_label: &'static str, _timeout: Duration) -> Self {
        Self
    }

    #[inline(always)]
    #[must_use]
    pub fn with_on_hang<F>(self, _callback: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        self
    }

    #[inline(always)]
    pub fn tick(&mut self) {}

    #[inline(always)]
    pub fn reset(&mut self) {}
}

#[must_use]
pub fn default_timeout() -> Duration {
    default_timeout_with_env(fallback_timeout())
}

#[must_use]
fn default_timeout_with_env(default_timeout: Duration) -> Duration {
    env_timeout().unwrap_or(default_timeout)
}

#[must_use]
fn fallback_timeout() -> Duration {
    const DEFAULT_TIMEOUT_SECS: u64 = 10;
    Duration::from_secs(DEFAULT_TIMEOUT_SECS)
}

#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
#[must_use]
fn env_timeout() -> Option<Duration> {
    const ENV_TIMEOUT_SECS: &str = "KITHARA_HANG_TIMEOUT_SECS";
    static CACHED: OnceLock<Option<Duration>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let value = env::var(ENV_TIMEOUT_SECS).ok()?;
        parse_timeout_secs(&value)
    })
}

#[cfg(any(feature = "disable-hang-detector", target_arch = "wasm32"))]
#[must_use]
fn env_timeout() -> Option<Duration> {
    None
}

#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
#[must_use]
fn parse_timeout_secs(value: &str) -> Option<Duration> {
    let secs = value.parse::<u64>().ok()?;
    (secs > 0).then_some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    mod kithara {
        pub(crate) use kithara_test_macros::test;
    }

    use std::thread::sleep;

    use super::*;

    #[kithara::test]
    fn parse_timeout_secs_rejects_invalid() {
        assert_eq!(parse_timeout_secs(""), None);
        assert_eq!(parse_timeout_secs("abc"), None);
        assert_eq!(parse_timeout_secs("0"), None);
    }

    #[kithara::test]
    fn parse_timeout_secs_accepts_positive_numbers() {
        assert_eq!(parse_timeout_secs("7"), Some(Duration::from_secs(7)));
    }

    #[kithara::test]
    fn fallback_timeout_is_ten_seconds() {
        assert_eq!(fallback_timeout(), Duration::from_secs(10));
    }

    #[kithara::test]
    fn tick_within_timeout_does_not_panic() {
        let mut detector = HangDetector::new("test", Duration::from_secs(5));
        for _ in 0..100 {
            detector.tick();
        }
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[kithara::test(native)]
    #[should_panic(expected = "HangDetector")]
    fn tick_after_timeout_panics() {
        let mut detector = HangDetector::new("test.wait", Duration::from_millis(1));
        sleep(Duration::from_millis(10));
        detector.tick();
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[kithara::test(wasm)]
    fn tick_after_timeout_does_not_panic_on_wasm() {
        let mut detector = HangDetector::new("test.wait", Duration::from_millis(1));
        sleep(Duration::from_millis(10));
        detector.tick();
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[kithara::test]
    fn reset_extends_deadline() {
        let mut detector = HangDetector::new("test", Duration::from_millis(50));
        sleep(Duration::from_millis(30));
        detector.reset();
        sleep(Duration::from_millis(30));
        detector.tick();
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[kithara::test(native)]
    #[should_panic(expected = "HangDetector")]
    fn on_hang_fires_before_panic() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);
        let mut detector =
            HangDetector::new("test.hook", Duration::from_millis(1)).with_on_hang(move || {
                fired_clone.store(true, Ordering::SeqCst);
            });
        sleep(Duration::from_millis(10));
        // Sanity: the hook runs before panic, so we can observe it from
        // a `std::panic::catch_unwind` harness in a separate test. Here
        // we only assert the panic; the next test exercises the side
        // effect under `catch_unwind`.
        assert!(!fired.load(Ordering::SeqCst));
        detector.tick();
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[kithara::test(native)]
    fn on_hang_side_effect_visible_under_catch_unwind() {
        use std::{
            panic::{AssertUnwindSafe, catch_unwind},
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };

        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut detector = HangDetector::new("test.hook.catch", Duration::from_millis(1))
                .with_on_hang(move || {
                    fired_clone.store(true, Ordering::SeqCst);
                });
            sleep(Duration::from_millis(10));
            detector.tick();
        }));
        assert!(result.is_err(), "tick past deadline must panic");
        assert!(
            fired.load(Ordering::SeqCst),
            "on_hang callback must fire before the panic"
        );
    }
}
