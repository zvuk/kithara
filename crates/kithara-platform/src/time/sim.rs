pub use std::time::Duration;
use std::{
    ops::{Add, Sub},
    sync::atomic::{AtomicU64, Ordering},
};

/// Quiescence-driven virtual-clock engine. Submodule of `sim`, which is already
/// gated on `feature = "sim-time"` + native, so it needs no extra feature gate.
/// The engine drives `SIM_NANOS` forward at quiescent points. In Inc 0 its sole
/// consumer is the harness, so the module is `#[cfg(test)]`-scoped; the first
/// increment that routes a real wait onto it drops this gate.
#[cfg(test)]
pub(crate) mod sched;

/// Process-global virtual timeline, in nanoseconds. Only moves forward, via
/// [`advance`] or the `sched` quiescence engine; starts at
/// [`Instant::BASE_NANOS`].
static SIM_NANOS: AtomicU64 = AtomicU64::new(Instant::BASE_NANOS);

/// Current virtual instant in nanoseconds since the timeline origin. Used by the
/// `sched` engine (test-scoped in Inc 0).
#[cfg(test)]
#[inline]
pub(crate) fn now_nanos() -> u64 {
    SIM_NANOS.load(Ordering::Acquire)
}

fn duration_to_nanos(d: Duration) -> u64 {
    // Fold via `u64` seconds + `u32` subsec — no `u128` intermediate, no cast.
    const NANOS_PER_SEC: u64 = 1_000_000_000;
    d.as_secs()
        .saturating_mul(NANOS_PER_SEC)
        .saturating_add(u64::from(d.subsec_nanos()))
}

/// Advance the virtual clock by `delta`. Additive: each caller moves the single
/// shared timeline forward, so sim runs target tests with exactly one pacing
/// thread (the offline render loop, whose `park_timeout` calls this). The
/// harness needs no change.
#[inline]
pub fn advance(delta: Duration) {
    SIM_NANOS.fetch_add(duration_to_nanos(delta), Ordering::Release);
}

/// Reset the timeline to its base and clear the quiescence engine. For unit
/// tests that share one process; production tests get per-test process
/// isolation from nextest. Order matters: store the base first, then drop the
/// engine state, so afterwards the clock reads `Instant::BASE_NANOS` and the
/// engine is empty. The engine clear is test-scoped in Inc 0 (see `sched`).
#[inline]
pub fn reset() {
    SIM_NANOS.store(Instant::BASE_NANOS, Ordering::Release);
    #[cfg(test)]
    sched::reset();
}

/// Drop-in for `web_time::Instant` backed by the virtual clock. Exposes exactly
/// the API surface the workspace uses on instants (`now`, `elapsed`,
/// `duration_since`, `saturating_duration_since`, `+`/`-`, ordering); all
/// arithmetic saturates so misuse never panics or wraps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(u64);

impl Instant {
    /// Timeline origin: one day in, so realistic backward offsets from `now()`
    /// (e.g. crossfade start instants) stay positive; arithmetic saturates anyway.
    const BASE_NANOS: u64 = 86_400_000_000_000;

    #[inline]
    #[must_use]
    pub fn now() -> Self {
        Self(SIM_NANOS.load(Ordering::Acquire))
    }

    #[inline]
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        Self::now().saturating_duration_since(*self)
    }

    #[inline]
    #[must_use]
    pub fn duration_since(&self, earlier: Self) -> Duration {
        self.saturating_duration_since(earlier)
    }

    #[inline]
    #[must_use]
    pub fn saturating_duration_since(&self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

impl Add<Duration> for Instant {
    type Output = Self;
    #[inline]
    fn add(self, rhs: Duration) -> Self {
        Self(self.0.saturating_add(duration_to_nanos(rhs)))
    }
}

impl Sub<Duration> for Instant {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: Duration) -> Self {
        Self(self.0.saturating_sub(duration_to_nanos(rhs)))
    }
}

impl Sub<Self> for Instant {
    type Output = Duration;
    #[inline]
    fn sub(self, rhs: Self) -> Duration {
        self.saturating_duration_since(rhs)
    }
}

#[cfg(test)]
mod tests;
