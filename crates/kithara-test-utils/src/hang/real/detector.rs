use std::{marker::PhantomData, path::PathBuf, time::Duration};

use serde::Serialize;
use web_time::Instant;

use super::platform::write_dump;

pub trait HangDump {
    fn label(&self) -> Option<&str> {
        None
    }
    fn to_json(&self) -> String;
}

impl<T: Serialize> HangDump for T {
    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoContext;

impl Serialize for NoContext {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_unit_struct("NoContext")
    }
}

#[must_use]
pub(crate) fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '.',
        })
        .collect()
}

/// Watchdog that detects loops stuck without progress.
///
/// Generic over the context payload `C`. Defaults to `NoContext` so the
/// call-site `HangDetector::new("label", timeout)` stays source-compatible.
/// When a hang fires the stored context (if any) is serialized and written
/// to disk (native) or logged (wasm), then — on native only — the process
/// panics with a stacktrace-friendly message.
///
/// On native targets `tick()` panics after the deadline. On `wasm32`
/// it logs an error and resets — panics there are fatal
/// (`panic = "immediate-abort"` turns every panic into
/// `RuntimeError: unreachable` which kills the Web Worker).
pub struct HangDetector<C: HangDump = NoContext> {
    label: &'static str,
    timeout: Duration,
    deadline: Instant,
    ctx: Option<C>,
    dump_dir: Option<PathBuf>,
    _marker: PhantomData<C>,
    fired: bool,
}

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
        self.handle_deadline_exceeded();
    }

    pub fn tick_with(&mut self, ctx: C) {
        self.ctx = Some(ctx);
        self.tick();
    }

    #[must_use]
    pub fn with_dump_dir(mut self, dir: PathBuf) -> Self {
        self.dump_dir = Some(dir);
        self
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn handle_deadline_exceeded(&mut self) {
        panic!(
            "[HangDetector] `{}` no progress for {:?} — likely deadlock or hang",
            self.label, self.timeout,
        );
    }

    #[cfg(target_arch = "wasm32")]
    fn handle_deadline_exceeded(&mut self) {
        self.deadline = Instant::now() + self.timeout;
    }
}

#[must_use]
pub fn default_timeout() -> Duration {
    default_timeout_with_env(fallback_timeout())
}

#[must_use]
fn default_timeout_with_env(default_timeout: Duration) -> Duration {
    super::platform::env_timeout().unwrap_or(default_timeout)
}

#[must_use]
pub(crate) fn fallback_timeout() -> Duration {
    const DEFAULT_TIMEOUT_SECS: u64 = 10;
    Duration::from_secs(DEFAULT_TIMEOUT_SECS)
}
