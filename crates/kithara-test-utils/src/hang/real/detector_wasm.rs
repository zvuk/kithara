use std::{marker::PhantomData, path::PathBuf};

use kithara_platform::time::{Duration, coarse_now_ms};

use super::{
    platform::write_dump,
    shared::{HangDump, NoContext},
};

/// Watchdog that detects loops stuck without progress (wasm build).
///
/// Mirrors the native [`HangDetector`](super::HangDetector) API but cannot
/// panic — on wasm a panic is a fatal `RuntimeError: unreachable` that kills
/// the Worker — so on the deadline it only reports via
/// [`write_dump`](super::platform::write_dump) (the browser console) and
/// re-arms. The clock is [`coarse_now_ms`](kithara_platform::time::coarse_now_ms)
/// (`Date.now()`), which is safe in every scope including the audio worklet,
/// sampled sparsely (see [`Self::CLOCK_SAMPLE_TICKS`]) to keep the render thread free
/// of per-tick JS-boundary crossings.
pub struct HangDetector<C: HangDump = NoContext> {
    label: &'static str,
    timeout: Duration,
    /// Clock at the first sampled tick since construction or the last
    /// [`reset`](Self::reset). `None` until the first sample, so the common
    /// fast path never reads the clock.
    started_ms: Option<f64>,
    /// `tick()` calls since the last clock sample; gates clock reads to once
    /// per [`Self::CLOCK_SAMPLE_TICKS`].
    ticks: u32,
    ctx: Option<C>,
    dump_dir: Option<PathBuf>,
    _marker: PhantomData<C>,
    fired: bool,
}

impl<C: HangDump> HangDetector<C> {
    /// Milliseconds in a second.
    const MS_PER_SECOND: f64 = 1000.0;

    /// Read the clock only once per this many `tick()` calls. On wasm every
    /// clock read is a JS-boundary crossing — too costly to do per tick on the
    /// audio render thread, where this detector runs (the `Audio::read` consume
    /// path executes inside the `AudioWorkletGlobalScope`). A short-lived
    /// detector that ticks fewer times than this — the common read path — never
    /// reads the clock at all; only a loop that spins past the threshold samples
    /// it, and a genuinely stuck spin racks up ticks fast enough to be caught
    /// well within any second-scale timeout.
    const CLOCK_SAMPLE_TICKS: u32 = 64;

    #[must_use]
    pub fn new(label: &'static str, timeout: Duration) -> Self {
        Self {
            label,
            timeout,
            started_ms: None,
            ticks: 0,
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
        self.started_ms = None;
        self.ticks = 0;
        self.fired = false;
    }

    /// Progress check without updating the stored context. Keeps whatever was
    /// moved in by the last [`tick_with`](Self::tick_with). Reads the clock
    /// only once per [`CLOCK_SAMPLE_TICKS`] calls. On the deadline it reports
    /// the hang and re-arms (wasm cannot panic).
    pub fn tick(&mut self) {
        self.ticks += 1;
        if self.ticks < Self::CLOCK_SAMPLE_TICKS {
            return;
        }
        self.ticks = 0;

        let now = coarse_now_ms();
        let Some(start) = self.started_ms else {
            self.started_ms = Some(now);
            return;
        };
        if now - start < self.timeout.as_secs_f64() * Self::MS_PER_SECOND {
            return;
        }
        self.fire_dump();
        // Re-anchor so a still-stuck loop reports again only after another
        // timeout rather than on every sampled tick.
        self.started_ms = None;
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
