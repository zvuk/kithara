//! Stale-loop detector for condvar wait loops.
//!
//! When enabled via `cfg(feature = "stale-detector")`, [`StaleDetector`] tracks
//! wall-clock time since the last progress reset and panics if the timeout is
//! exceeded, indicating a likely deadlock or hang.
//!
//! When the feature is disabled, all methods are no-ops and the struct is
//! zero-sized — no runtime cost.

/// Detects condvar wait loops that spin without progress.
///
/// Call [`tick()`](Self::tick) once per condvar wake-up. If wall-clock time
/// since the last [`reset()`](Self::reset) exceeds `timeout`, the detector
/// panics with a diagnostic message naming the call site.
///
/// Unlike a tick-counter approach, this is immune to spurious wake-ups:
/// `condvar.wait_timeout(50ms)` may return instantly, but real time still
/// advances at wall-clock rate.
///
/// # Example
///
/// ```ignore
/// use std::time::Duration;
/// let mut stale = StaleDetector::new("storage.wait_range", Duration::from_secs(5));
/// loop {
///     // ... check condition ...
///     stale.tick();
///     condvar.wait_timeout(...);
/// }
/// ```
#[cfg(feature = "stale-detector")]
pub struct StaleDetector {
    label: &'static str,
    timeout: crate::time::Duration,
    deadline: crate::time::Instant,
}

#[cfg(feature = "stale-detector")]
impl StaleDetector {
    /// Create a new detector.
    ///
    /// - `label`: human-readable name of the wait site (for panic messages).
    /// - `timeout`: wall-clock duration without progress before panic.
    #[must_use]
    pub fn new(label: &'static str, timeout: crate::time::Duration) -> Self {
        Self {
            label,
            timeout,
            deadline: crate::time::Instant::now() + timeout,
        }
    }

    /// Check whether the timeout has been exceeded.
    ///
    /// # Panics
    ///
    /// Panics if wall-clock time since the last `reset()` (or construction)
    /// exceeds the configured timeout.
    pub fn tick(&mut self) {
        if crate::time::Instant::now() >= self.deadline {
            panic!(
                "[StaleDetector] `{}` stale for {:?} — likely deadlock or stale wait loop",
                self.label, self.timeout,
            );
        }
    }

    /// Reset the deadline (call when progress is observed).
    pub fn reset(&mut self) {
        self.deadline = crate::time::Instant::now() + self.timeout;
    }
}

/// No-op stub when `stale-detector` feature is disabled.
#[cfg(not(feature = "stale-detector"))]
pub struct StaleDetector;

#[cfg(not(feature = "stale-detector"))]
impl StaleDetector {
    #[inline(always)]
    #[must_use]
    pub fn new(_label: &'static str, _timeout: crate::time::Duration) -> Self {
        Self
    }

    #[inline(always)]
    pub fn tick(&mut self) {}

    #[inline(always)]
    pub fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Duration;

    #[test]
    fn tick_within_timeout_does_not_panic() {
        let mut sd = StaleDetector::new("test", Duration::from_secs(5));
        for _ in 0..100 {
            sd.tick();
        }
    }

    #[cfg(feature = "stale-detector")]
    #[test]
    #[should_panic(expected = "StaleDetector")]
    fn tick_after_timeout_panics() {
        let mut sd = StaleDetector::new("test.wait", Duration::from_millis(1));
        std::thread::sleep(std::time::Duration::from_millis(10));
        sd.tick();
    }

    #[cfg(feature = "stale-detector")]
    #[test]
    fn reset_extends_deadline() {
        let mut sd = StaleDetector::new("test", Duration::from_millis(50));
        std::thread::sleep(std::time::Duration::from_millis(30));
        sd.reset();
        std::thread::sleep(std::time::Duration::from_millis(30));
        // 60ms total, but only 30ms since reset — within 50ms timeout
        sd.tick();
    }
}
