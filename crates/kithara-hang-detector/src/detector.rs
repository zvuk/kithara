#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
use std::env;
#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
use std::sync::OnceLock;
use std::time::Duration;

#[cfg(not(feature = "disable-hang-detector"))]
use web_time::Instant;

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
    /// Suppresses repeated WASM error logs (one message per timeout window).
    #[cfg(target_arch = "wasm32")]
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
            #[cfg(target_arch = "wasm32")]
            fired: false,
        }
    }

    /// Check progress.
    ///
    /// # Panics
    ///
    /// On native targets, panics when no progress is observed before the
    /// configured timeout. On `wasm32` logs an error to the console instead.
    pub fn tick(&mut self) {
        if Instant::now() >= self.deadline {
            #[cfg(not(target_arch = "wasm32"))]
            {
                panic!(
                    "[HangDetector] `{}` no progress for {:?} — likely deadlock or hang",
                    self.label, self.timeout,
                );
            }

            #[cfg(target_arch = "wasm32")]
            {
                if !self.fired {
                    tracing::error!(
                        label = self.label,
                        timeout_secs = self.timeout.as_secs(),
                        "[HangDetector] no progress — possible deadlock or hang"
                    );
                    self.fired = true;
                }
                // Reset to avoid busy-logging every tick.
                self.deadline = Instant::now() + self.timeout;
            }
        }
    }

    pub fn reset(&mut self) {
        self.deadline = Instant::now() + self.timeout;
        #[cfg(target_arch = "wasm32")]
        {
            self.fired = false;
        }
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
}
