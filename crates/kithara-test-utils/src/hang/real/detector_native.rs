use std::{marker::PhantomData, panic::Location, path::PathBuf};

use kithara_platform::time::{Duration, Instant};

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
/// The panic and dump carry a [`diagnostic`](Self::diagnostic): the source
/// location of the spinning `hang_tick!` (where it stalled), the location of
/// the last `hang_reset!` (the last observed progress), and how many ticks
/// spun since that progress. The `hang_tick!`/`hang_reset!` macros forward
/// `file!()`/`line!()` through the `*_from` methods; direct method calls fall
/// back to `#[track_caller]`. The bare "no progress" message alone only says
/// *that* something hung — these locations say *where* and *since when*.
///
/// Native uses a monotonic [`Instant`] and checks it on every [`tick`](Self::tick).
/// (The wasm build has a separate detector with its own clock and reporting.)
pub struct HangDetector<C: HangDump = NoContext> {
    label: &'static str,
    timeout: Duration,
    ctx: Option<C>,
    /// Stamped lazily on the first observation (`tick`/`remaining`), NOT in
    /// `new`, so the deadline and every later comparison read the SAME clock.
    /// Under `flash` the constructor can run outside the watched fn's
    /// flash scope (attribute-expansion order puts the `#[kithara::hang_watchdog]`
    /// prelude before the `#[kithara::flash(true)]` scope entry), where
    /// `Instant::now` is real while the body's reads are virtual; an eager
    /// stamp mixes clocks and false-fires the moment virtual time outruns real.
    deadline: Option<Instant>,
    dump_dir: Option<PathBuf>,
    /// Source location of the most recent reset — the last observed progress.
    last_progress: Option<(&'static str, u32)>,
    /// Source location of the most recent tick — where the loop stalled.
    last_tick: Option<(&'static str, u32)>,
    _marker: PhantomData<C>,
    fired: bool,
    /// Ticks observed since the last reset (spin depth before the deadline).
    spins_since_progress: u32,
}

impl<C: HangDump> HangDetector<C> {
    #[must_use]
    pub fn new(label: &'static str, timeout: Duration) -> Self {
        Self {
            label,
            timeout,
            deadline: None,
            ctx: None,
            dump_dir: None,
            fired: false,
            _marker: PhantomData,
            last_tick: None,
            last_progress: None,
            spins_since_progress: 0,
        }
    }

    fn deadline(&mut self) -> Instant {
        *self
            .deadline
            .get_or_insert_with(|| Instant::now() + self.timeout)
    }

    /// Human-readable account of the stall: where it stuck, where it last made
    /// progress, and the spin depth. Carried in both the panic and the dump.
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

    /// Liveness budget left before the deadline, on the current (real or
    /// virtual) clock; zero once it has passed. An event-driven wait parks
    /// bounded by this: woken early by its event on progress, or released at
    /// the deadline so the following [`tick`](Self::tick) fires on a genuine
    /// stall — no busy-poll, and no dependency on counting the producer.
    #[must_use]
    pub fn remaining(&mut self) -> Duration {
        self.deadline().saturating_duration_since(Instant::now())
    }

    #[track_caller]
    pub fn reset(&mut self) {
        let loc = Location::caller();
        self.reset_from(loc.file(), loc.line());
    }

    /// `reset` with an explicit source location, forwarded by `hang_reset!`.
    pub fn reset_from(&mut self, file: &'static str, line: u32) {
        self.deadline = Some(Instant::now() + self.timeout);
        self.fired = false;
        self.last_progress = Some((file, line));
        self.spins_since_progress = 0;
    }

    /// `reset` that also refreshes the stored context, forwarded by
    /// `hang_reset!($ctx)`. Records the app state observed at the last progress
    /// point so a later stall dumps the freshest context even if the spinning
    /// `hang_tick!` carried none. The context is produced by a closure that this
    /// (real) detector invokes; the no-`hang` build's detector drops it
    /// uncalled, so the collection is provably skipped in release.
    #[track_caller]
    pub fn reset_with<F: FnOnce() -> C>(&mut self, ctx_fn: F) {
        self.ctx = Some(ctx_fn());
        let loc = Location::caller();
        self.reset_from(loc.file(), loc.line());
    }

    /// `reset_with` with an explicit source location, forwarded by
    /// `hang_reset!($ctx)`.
    pub fn reset_with_from<F: FnOnce() -> C>(&mut self, ctx_fn: F, file: &'static str, line: u32) {
        self.ctx = Some(ctx_fn());
        self.reset_from(file, line);
    }

    /// Progress check without updating the stored context. Keeps whatever was
    /// moved in by the last [`tick_with`](Self::tick_with).
    ///
    /// # Panics
    ///
    /// Panics when no progress is observed before the configured timeout.
    #[track_caller]
    pub fn tick(&mut self) {
        let loc = Location::caller();
        self.tick_from(loc.file(), loc.line());
    }

    /// `tick` with an explicit source location, forwarded by `hang_tick!`/
    /// `hang_park!`. Records the location so a fired watchdog reports the
    /// spinning line, not just that something stalled.
    ///
    /// # Panics
    ///
    /// Panics when no progress is observed before the configured timeout.
    pub fn tick_from(&mut self, file: &'static str, line: u32) {
        self.last_tick = Some((file, line));
        if Instant::now() < self.deadline() {
            self.spins_since_progress = self.spins_since_progress.saturating_add(1);
            return;
        }
        self.fire_dump();
        panic!(
            "[HangDetector] `{label}` no progress for {timeout:?} — {diag}",
            label = self.label,
            timeout = self.timeout,
            diag = self.diagnostic(),
        );
    }

    /// Progress check that refreshes the stored context from a closure. The
    /// closure runs only in this real detector; the no-`hang` build drops it
    /// uncalled, so the app-context collection is provably skipped in release.
    #[track_caller]
    pub fn tick_with<F: FnOnce() -> C>(&mut self, ctx_fn: F) {
        self.ctx = Some(ctx_fn());
        let loc = Location::caller();
        self.tick_from(loc.file(), loc.line());
    }

    /// `tick_with` with an explicit source location, forwarded by `hang_tick!`.
    ///
    /// # Panics
    ///
    /// Panics when no progress is observed before the configured timeout.
    pub fn tick_with_from<F: FnOnce() -> C>(&mut self, ctx_fn: F, file: &'static str, line: u32) {
        self.ctx = Some(ctx_fn());
        self.tick_from(file, line);
    }

    #[must_use]
    pub fn with_dump_dir(mut self, dir: PathBuf) -> Self {
        self.dump_dir = Some(dir);
        self
    }
}
