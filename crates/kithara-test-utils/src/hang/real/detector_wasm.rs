use std::{marker::PhantomData, panic::Location, path::PathBuf};

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
///
/// Like native it carries a [`diagnostic`](Self::diagnostic) (stuck location,
/// last-progress location, spin depth) in the report, so the console line says
/// *where* the worklet stalled, not just that it did.
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
    /// Source location of the most recent tick — where the loop stalled.
    last_tick: Option<(&'static str, u32)>,
    /// Source location of the most recent reset — the last observed progress.
    last_progress: Option<(&'static str, u32)>,
    /// Ticks observed since the last reset (spin depth before the deadline).
    spins_since_progress: u32,
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
            last_tick: None,
            last_progress: None,
            spins_since_progress: 0,
        }
    }

    /// Human-readable account of the stall: where it stuck, where it last made
    /// progress, and the spin depth. Carried in the dump/console report.
    fn diagnostic(&self) -> String {
        let fmt = |loc: Option<(&'static str, u32)>| {
            loc.map_or_else(
                || "<unknown>".to_string(),
                |(file, line)| format!("{file}:{line}"),
            )
        };
        format!(
            "stuck at {stuck} | last progress at {progress} | {spins} tick(s) since progress | timeout {timeout:?}",
            stuck = fmt(self.last_tick),
            progress = fmt(self.last_progress),
            spins = self.spins_since_progress,
            timeout = self.timeout,
        )
    }

    fn fire_dump(&mut self) {
        if self.fired {
            return;
        }
        self.fired = true;
        let diag = self.diagnostic();
        match self.ctx.as_ref() {
            Some(ctx) => write_dump(self.label, ctx, self.dump_dir.as_deref(), &diag),
            None => write_dump(self.label, &NoContext, self.dump_dir.as_deref(), &diag),
        }
    }

    #[track_caller]
    pub fn reset(&mut self) {
        let loc = Location::caller();
        self.reset_from(loc.file(), loc.line());
    }

    /// `reset` with an explicit source location, forwarded by `hang_reset!`.
    pub fn reset_from(&mut self, file: &'static str, line: u32) {
        self.started_ms = None;
        self.ticks = 0;
        self.fired = false;
        self.last_progress = Some((file, line));
        self.spins_since_progress = 0;
    }

    /// Liveness budget left before the deadline on the coarse wasm clock; zero
    /// once it has passed. Mirrors the native `remaining` so the `hang_park!`
    /// event-driven wait parks bounded by it — woken early by its event on
    /// progress, or released at the deadline so the next [`tick`](Self::tick)
    /// reports a genuine stall. A park is not the per-tick hot path, so the
    /// single clock read here is affordable; like [`tick`](Self::tick) the first
    /// observation lazily anchors `started_ms` (one clock by construction).
    #[must_use]
    pub fn remaining(&mut self) -> Duration {
        let now = coarse_now_ms();
        let start = *self.started_ms.get_or_insert(now);
        let timeout_ms = self.timeout.as_secs_f64() * Self::MS_PER_SECOND;
        let left_ms = (start + timeout_ms - now).max(0.0);
        Duration::from_secs_f64(left_ms / Self::MS_PER_SECOND)
    }

    /// Progress check without updating the stored context. Keeps whatever was
    /// moved in by the last [`tick_with`](Self::tick_with). Reads the clock
    /// only once per [`CLOCK_SAMPLE_TICKS`] calls. On the deadline it reports
    /// the hang and re-arms (wasm cannot panic).
    #[track_caller]
    pub fn tick(&mut self) {
        let loc = Location::caller();
        self.tick_from(loc.file(), loc.line());
    }

    /// `tick` with an explicit source location, forwarded by `hang_tick!`/
    /// `hang_park!`. Records the location (cheap, no clock read) so a fired
    /// watchdog reports the spinning line.
    pub fn tick_from(&mut self, file: &'static str, line: u32) {
        self.last_tick = Some((file, line));
        self.spins_since_progress = self.spins_since_progress.saturating_add(1);
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

    #[track_caller]
    pub fn tick_with(&mut self, ctx: C) {
        self.ctx = Some(ctx);
        let loc = Location::caller();
        self.tick_from(loc.file(), loc.line());
    }

    /// `tick_with` with an explicit source location, forwarded by `hang_tick!`.
    pub fn tick_with_from(&mut self, ctx: C, file: &'static str, line: u32) {
        self.ctx = Some(ctx);
        self.tick_from(file, line);
    }

    #[must_use]
    pub fn with_dump_dir(mut self, dir: PathBuf) -> Self {
        self.dump_dir = Some(dir);
        self
    }
}
