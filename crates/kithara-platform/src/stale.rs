//! Stale-loop detector for condvar wait loops.
//!
//! When enabled via `cfg(feature = "stale-detector")`, [`StaleDetector`] counts
//! iterations in a condvar wait loop and panics if the threshold is exceeded,
//! indicating a likely deadlock or hang.
//!
//! When the feature is disabled, all methods are no-ops and the struct is
//! zero-sized — no runtime cost.

/// Detects condvar wait loops that spin without progress.
///
/// Call [`tick()`](Self::tick) once per condvar wake-up. After `threshold`
/// ticks the detector panics with a diagnostic message naming the call site.
///
/// # Example
///
/// ```ignore
/// let mut stale = StaleDetector::new("storage.wait_range", 200);
/// loop {
///     // ... check condition ...
///     stale.tick();
///     condvar.wait_timeout(...);
/// }
/// ```
#[cfg(feature = "stale-detector")]
pub struct StaleDetector {
    label: &'static str,
    count: usize,
    threshold: usize,
}

#[cfg(feature = "stale-detector")]
impl StaleDetector {
    /// Create a new detector.
    ///
    /// - `label`: human-readable name of the wait site (for panic messages).
    /// - `threshold`: number of ticks before panic.
    #[must_use]
    pub fn new(label: &'static str, threshold: usize) -> Self {
        Self {
            label,
            count: 0,
            threshold,
        }
    }

    /// Record one condvar wake-up.
    ///
    /// # Panics
    ///
    /// Panics if the tick count reaches `threshold`, indicating a likely
    /// deadlock or stale wait loop.
    pub fn tick(&mut self) {
        self.count += 1;
        if self.count >= self.threshold {
            panic!(
                "[StaleDetector] `{}` exceeded {} ticks — likely deadlock or stale wait loop",
                self.label, self.threshold,
            );
        }
    }

    /// Reset tick counter (e.g. after progress is made).
    pub fn reset(&mut self) {
        self.count = 0;
    }
}

/// No-op stub when `stale-detector` feature is disabled.
#[cfg(not(feature = "stale-detector"))]
pub struct StaleDetector;

#[cfg(not(feature = "stale-detector"))]
impl StaleDetector {
    #[inline(always)]
    #[must_use]
    pub fn new(_label: &'static str, _threshold: usize) -> Self {
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

    #[test]
    fn tick_below_threshold_does_not_panic() {
        let mut sd = StaleDetector::new("test", 5);
        for _ in 0..4 {
            sd.tick();
        }
    }

    #[cfg(feature = "stale-detector")]
    #[test]
    #[should_panic(expected = "StaleDetector")]
    fn tick_at_threshold_panics() {
        let mut sd = StaleDetector::new("test.wait", 3);
        for _ in 0..3 {
            sd.tick();
        }
    }

    #[cfg(feature = "stale-detector")]
    #[test]
    fn reset_clears_counter() {
        let mut sd = StaleDetector::new("test", 3);
        sd.tick();
        sd.tick();
        sd.reset();
        // After reset, we can tick again without panic
        sd.tick();
        sd.tick();
    }
}
