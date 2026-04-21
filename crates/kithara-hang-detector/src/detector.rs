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
    deadline: Instant,
    label: &'static str,
    timeout: Duration,
    /// Last context snapshot moved in via [`Self::tick_with`]. Read
    /// only when the deadline fires; otherwise untouched.
    ctx: Option<C>,
    /// Explicit dump directory set via [`Self::with_dump_dir`]. Wins
    /// over `KITHARA_HANG_DUMP_DIR` and `std::env::temp_dir()`.
    dump_dir: Option<PathBuf>,
    /// Set after the dump for the current hang window has been
    /// written. Cleared by [`Self::reset`].
    fired: bool,
    _marker: PhantomData<C>,
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

    /// Override the dump directory with highest precedence (beats both
    /// `KITHARA_HANG_DUMP_DIR` and `std::env::temp_dir()`).
    #[must_use]
    pub fn with_dump_dir(mut self, dir: PathBuf) -> Self {
        self.dump_dir = Some(dir);
        self
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

        #[cfg(not(target_arch = "wasm32"))]
        {
            panic!(
                "[HangDetector] `{}` no progress for {:?} — likely deadlock or hang",
                self.label, self.timeout,
            );
        }

        #[cfg(target_arch = "wasm32")]
        {
            // Reset to avoid busy-logging every tick.
            self.deadline = Instant::now() + self.timeout;
        }
    }

    /// Progress check that also replaces the stored context snapshot.
    /// Hot-path cost is an `Instant::now()` comparison plus an inline
    /// move of `C` into the local `Option<C>` — no allocations.
    /// Serialization happens only when the deadline fires.
    pub fn tick_with(&mut self, ctx: C) {
        self.ctx = Some(ctx);
        self.tick();
    }

    pub fn reset(&mut self) {
        self.deadline = Instant::now() + self.timeout;
        self.fired = false;
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
    #[must_use]
    pub fn with_dump_dir(self, _dir: std::path::PathBuf) -> Self {
        self
    }

    #[inline(always)]
    pub fn tick(&mut self) {}

    #[inline(always)]
    pub fn tick_with(&mut self, _ctx: C) {}

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
        let mut detector: HangDetector = HangDetector::new("test", Duration::from_secs(5));
        for _ in 0..100 {
            detector.tick();
        }
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[kithara::test(native)]
    #[should_panic(expected = "HangDetector")]
    fn tick_after_timeout_panics() {
        let mut detector: HangDetector = HangDetector::new("test.wait", Duration::from_millis(1));
        sleep(Duration::from_millis(10));
        detector.tick();
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[kithara::test(wasm)]
    fn tick_after_timeout_does_not_panic_on_wasm() {
        let mut detector: HangDetector = HangDetector::new("test.wait", Duration::from_millis(1));
        sleep(Duration::from_millis(10));
        detector.tick();
    }

    #[cfg(not(feature = "disable-hang-detector"))]
    #[kithara::test]
    fn reset_extends_deadline() {
        let mut detector: HangDetector = HangDetector::new("test", Duration::from_millis(50));
        sleep(Duration::from_millis(30));
        detector.reset();
        sleep(Duration::from_millis(30));
        detector.tick();
    }

    #[cfg(all(not(feature = "disable-hang-detector"), not(target_arch = "wasm32")))]
    #[kithara::test(native)]
    fn tick_with_stores_context_for_dump() {
        use std::{
            panic::{AssertUnwindSafe, catch_unwind},
            path::PathBuf,
        };

        #[derive(serde::Serialize)]
        struct Ctx {
            phase: u32,
        }

        let dir: PathBuf = env::temp_dir().join("kithara-hang-tick-with-test");
        let _ = std::fs::create_dir_all(&dir);
        let dir_for_closure = dir.clone();

        let result = catch_unwind(AssertUnwindSafe(move || {
            let mut detector: HangDetector<Ctx> =
                HangDetector::new("tests.tick_with", Duration::from_millis(1))
                    .with_dump_dir(dir_for_closure);
            detector.tick_with(Ctx { phase: 5 });
            sleep(Duration::from_millis(10));
            detector.tick_with(Ctx { phase: 7 });
        }));
        assert!(result.is_err(), "detector must panic past deadline");

        let newest = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("kithara-hang-tests.tick_with-")
            })
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).unwrap())
            .expect("no dump file produced");
        let body = std::fs::read_to_string(newest.path()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["phase"], 7, "last tick_with wins");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
