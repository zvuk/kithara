#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
use std::env;
use std::time::Duration;

use web_time::Instant;

const DEFAULT_TIMEOUT_SECS: u64 = 10;
const ENV_TIMEOUT_SECS: &str = "KITHARA_HANG_TIMEOUT_SECS";

/// Watchdog that panics when progress is not observed before deadline.
#[cfg(not(feature = "disable-hang-detector"))]
pub struct HangDetector {
    deadline: Instant,
    label: &'static str,
    timeout: Duration,
}

#[cfg(not(feature = "disable-hang-detector"))]
impl HangDetector {
    #[must_use]
    pub fn new(label: &'static str, timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            label,
            timeout,
        }
    }

    /// # Panics
    ///
    /// Panics when no progress is observed before the configured timeout.
    pub fn tick(&mut self) {
        if Instant::now() >= self.deadline {
            panic!(
                "[HangDetector] `{}` no progress for {:?} — likely deadlock or hang",
                self.label, self.timeout,
            );
        }
    }

    pub fn reset(&mut self) {
        self.deadline = Instant::now() + self.timeout;
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
    Duration::from_secs(DEFAULT_TIMEOUT_SECS)
}

#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
#[must_use]
fn env_timeout() -> Option<Duration> {
    let value = env::var(ENV_TIMEOUT_SECS).ok()?;
    parse_timeout_secs(&value)
}

#[cfg(any(feature = "disable-hang-detector", target_arch = "wasm32"))]
#[must_use]
fn env_timeout() -> Option<Duration> {
    None
}

#[must_use]
fn parse_timeout_secs(value: &str) -> Option<Duration> {
    let secs = value.parse::<u64>().ok()?;
    (secs > 0).then_some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;

    use super::*;

    #[test]
    fn parse_timeout_secs_rejects_invalid() {
        assert_eq!(parse_timeout_secs(""), None);
        assert_eq!(parse_timeout_secs("abc"), None);
        assert_eq!(parse_timeout_secs("0"), None);
    }

    #[test]
    fn parse_timeout_secs_accepts_positive_numbers() {
        assert_eq!(parse_timeout_secs("7"), Some(Duration::from_secs(7)));
    }

    #[test]
    fn fallback_timeout_is_ten_seconds() {
        assert_eq!(fallback_timeout(), Duration::from_secs(10));
    }

    #[test]
    fn tick_within_timeout_does_not_panic() {
        let mut detector = HangDetector::new("test", Duration::from_secs(5));
        for _ in 0..100 {
            detector.tick();
        }
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[test]
    #[should_panic(expected = "HangDetector")]
    fn tick_after_timeout_panics() {
        let mut detector = HangDetector::new("test.wait", Duration::from_millis(1));
        sleep(Duration::from_millis(10));
        detector.tick();
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[test]
    fn reset_extends_deadline() {
        let mut detector = HangDetector::new("test", Duration::from_millis(50));
        sleep(Duration::from_millis(30));
        detector.reset();
        sleep(Duration::from_millis(30));
        detector.tick();
    }
}
