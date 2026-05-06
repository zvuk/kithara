#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
use std::env;
#[cfg(not(feature = "disable-hang-detector"))]
use std::path::PathBuf;
#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
use std::sync::OnceLock;
use std::{marker::PhantomData, time::Duration};

#[cfg(not(feature = "disable-hang-detector"))]
use web_time::Instant;

#[cfg(not(feature = "disable-hang-detector"))]
use crate::dump::write_dump;
use crate::dump::{HangDump, NoContext};

/// Watchdog that detects loops stuck without progress.
///
/// Generic over the context payload `C`. Defaults to `NoContext` so the
/// old call-site `HangDetector::new("label", timeout)` stays
/// source-compatible. When a hang fires the stored context (if any) is
/// serialized and written to disk (native) or logged (wasm) via
/// [`crate::dump::write_dump`], then — on native only — the process
/// panics with a stacktrace-friendly message.
///
/// On native targets `tick()` panics after the deadline. On `wasm32`
/// it logs an error and resets — panics there are fatal
/// (`panic = "immediate-abort"` turns every panic into
/// `RuntimeError: unreachable` which kills the Web Worker).
#[cfg(not(feature = "disable-hang-detector"))]
pub struct HangDetector<C: HangDump = NoContext> {
    label: &'static str,
    timeout: Duration,
    deadline: Instant,
    /// Last context snapshot moved in via [`Self::tick_with`]. Read
    /// only when the deadline fires; otherwise untouched.
    ctx: Option<C>,
    /// Explicit dump directory set via [`Self::with_dump_dir`]. Wins
    /// over `KITHARA_HANG_DUMP_DIR` and `std::env::temp_dir()`.
    dump_dir: Option<PathBuf>,
    _marker: PhantomData<C>,
    /// Set after the dump for the current hang window has been
    /// written. Cleared by [`Self::reset`].
    fired: bool,
}

#[cfg(not(feature = "disable-hang-detector"))]
impl<C: HangDump> HangDetector<C> {
    #[must_use]
    pub fn new(label: &'static str, timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + timeout,
            label,
            timeout,
            ctx: None,
            dump_dir: None,
            fired: false,
            _marker: PhantomData,
        }
    }

    fn fire_dump(&mut self) {
        if self.fired {
            return;
        }
        self.fired = true;
        match self.ctx.as_ref() {
            Some(ctx) => write_dump(self.label, ctx, self.dump_dir.as_deref()),
            None => write_dump(self.label, &NoContext, self.dump_dir.as_deref()),
        }
    }

    pub fn reset(&mut self) {
        self.deadline = Instant::now() + self.timeout;
        self.fired = false;
    }

    /// Progress check without updating the stored context. Keeps whatever
    /// was moved in by the last [`Self::tick_with`].
    ///
    /// # Panics
    ///
    /// On native targets, panics when no progress is observed before
    /// the configured timeout. On `wasm32` logs an error to the console
    /// instead.
    pub fn tick(&mut self) {
        if Instant::now() < self.deadline {
            return;
        }
        self.fire_dump();
        handle_deadline_exceeded(self);
    }

    /// Progress check that also replaces the stored context snapshot.
    /// Hot-path cost is an `Instant::now()` comparison plus an inline
    /// move of `C` into the local `Option<C>` — no allocations.
    /// Serialization happens only when the deadline fires.
    pub fn tick_with(&mut self, ctx: C) {
        self.ctx = Some(ctx);
        self.tick();
    }

    /// Override the dump directory with highest precedence (beats both
    /// `KITHARA_HANG_DUMP_DIR` and `std::env::temp_dir()`).
    #[must_use]
    pub fn with_dump_dir(mut self, dir: PathBuf) -> Self {
        self.dump_dir = Some(dir);
        self
    }
}

/// On native targets a missed deadline panics; on wasm it logs and
/// resets to avoid busy-logging every tick. Same `&mut self` signature
/// in both halves so `tick()` can call it without any cfg of its own.
#[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
fn handle_deadline_exceeded<C: HangDump>(d: &mut HangDetector<C>) {
    panic!(
        "[HangDetector] `{}` no progress for {:?} — likely deadlock or hang",
        d.label, d.timeout,
    );
}

#[cfg(all(not(feature = "disable-hang-detector"), target_arch = "wasm32"))]
fn handle_deadline_exceeded<C: HangDump>(d: &mut HangDetector<C>) {
    d.deadline = Instant::now() + d.timeout;
}

/// Zero-cost no-op stub when hang detection is disabled.
#[cfg(feature = "disable-hang-detector")]
pub struct HangDetector<C: HangDump = NoContext>(PhantomData<C>);

#[cfg(feature = "disable-hang-detector")]
impl<C: HangDump> HangDetector<C> {
    #[inline(always)]
    #[must_use]
    pub fn new(_label: &'static str, _timeout: Duration) -> Self {
        Self(PhantomData)
    }

    #[inline(always)]
    pub fn reset(&mut self) {}

    #[inline(always)]
    pub fn tick(&mut self) {}

    #[inline(always)]
    pub fn tick_with(&mut self, _ctx: C) {}

    #[inline(always)]
    #[must_use]
    pub fn with_dump_dir(self, _dir: std::path::PathBuf) -> Self {
        let _ = _dir;
        self
    }
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
pub(crate) fn fallback_timeout() -> Duration {
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
pub(crate) fn parse_timeout_secs(value: &str) -> Option<Duration> {
    let secs = value.parse::<u64>().ok()?;
    (secs > 0).then_some(Duration::from_secs(secs))
}
