use std::{
    marker::PhantomData,
    path::PathBuf,
    time::{Duration, Instant},
};

use super::{
    platform::write_dump,
    shared::{HangDump, NoContext},
};

/// Watchdog that detects loops stuck without progress.
///
/// Generic over the context payload `C`. Defaults to [`NoContext`] so the
/// call-site `HangDetector::new("label", timeout)` stays source-compatible.
/// On the deadline the stored context (if any) is reported via
/// [`write_dump`](super::platform::write_dump) and the thread panics with a
/// stacktrace-friendly message so a hung test fails loudly.
///
/// Native uses a monotonic [`Instant`] and checks it on every [`tick`](Self::tick).
/// (The wasm build has a separate detector with its own clock and reporting.)
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

    /// Progress check without updating the stored context. Keeps whatever was
    /// moved in by the last [`tick_with`](Self::tick_with).
    ///
    /// # Panics
    ///
    /// Panics when no progress is observed before the configured timeout.
    pub fn tick(&mut self) {
        if Instant::now() < self.deadline {
            return;
        }
        self.fire_dump();
        panic!(
            "[HangDetector] `{}` no progress for {:?} — likely deadlock or hang",
            self.label, self.timeout,
        );
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
}
