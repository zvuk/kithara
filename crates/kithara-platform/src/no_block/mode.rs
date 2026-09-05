#[cfg(test)]
use std::cell::{Cell, RefCell};
use std::{
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    Census,
    Panic,
}

struct Consts;

impl Consts {
    const ENV_BUDGET_MS: &str = "KITHARA_NO_BLOCK_BUDGET_MS";
    const ENV_LOG: &str = "KITHARA_NO_BLOCK_LOG";
    const ENV_MODE: &str = "KITHARA_NO_BLOCK";
    /// Blanket budget panics on CPU spin only; wait class logs by construction, and `KITHARA_NO_BLOCK_BUDGET_MS` overrides.
    const FALLBACK_BLANKET: Duration = Duration::from_millis(3_000);
}

#[cfg(test)]
thread_local! {
    static FORCED: Cell<Option<Mode>> = const { Cell::new(None) };
    static FORCED_BUDGET: Cell<Option<Duration>> = const { Cell::new(None) };
    /// The outer `Option` is whether a test overrides the configured sink, the
    /// inner one the sink it names. A test of the sink-less path needs both:
    /// the lane that runs it sets `KITHARA_NO_BLOCK_LOG`.
    static FORCED_LOG: RefCell<Option<Option<PathBuf>>> = const { RefCell::new(None) };
}

/// Set while a [`PanicMode`] guard lives; see [`force_panic_mode`].
static FORCED_PANIC: AtomicBool = AtomicBool::new(false);

/// Restores the previous panic-mode request when dropped.
#[derive(Debug)]
#[must_use = "the forced mode lasts only while this guard is alive"]
pub struct PanicMode(bool);

impl Drop for PanicMode {
    fn drop(&mut self) {
        FORCED_PANIC.store(self.0, Ordering::Release);
    }
}

/// Judge blocking waits in `panic` mode while the guard lives, whatever
/// `KITHARA_NO_BLOCK` the lane configured.
///
/// A test asserting that a blocking wait panics takes this instead of writing
/// `KITHARA_NO_BLOCK`: mutating the process environment while any other thread
/// reads it is undefined behaviour. Process-global rather than thread-local
/// because a watched poll runs on whichever thread drives it, not on the
/// thread that declared the mode.
pub fn force_panic_mode() -> PanicMode {
    PanicMode(FORCED_PANIC.swap(true, Ordering::AcqRel))
}

pub(super) fn mode() -> Mode {
    #[cfg(test)]
    {
        if let Some(mode) = FORCED.with(Cell::get) {
            return mode;
        }
    }

    if FORCED_PANIC.load(Ordering::Acquire) {
        return Mode::Panic;
    }

    static CACHED: OnceLock<Mode> = OnceLock::new();
    *CACHED.get_or_init(|| match std::env::var(Consts::ENV_MODE).as_deref() {
        Ok("off") => Mode::Off,
        Ok("census") => Mode::Census,
        _ => Mode::Panic,
    })
}

pub(super) fn is_off() -> bool {
    mode() == Mode::Off
}

pub(super) fn blanket_budget() -> Duration {
    #[cfg(test)]
    {
        if let Some(budget) = FORCED_BUDGET.with(Cell::get) {
            return budget;
        }
    }

    static CACHED: OnceLock<Duration> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var(Consts::ENV_BUDGET_MS)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map_or(Consts::FALLBACK_BLANKET, Duration::from_millis)
    })
}

pub(super) fn log_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Some(forced) = FORCED_LOG.with(|forced| forced.borrow().clone()) {
            return forced;
        }
    }

    static CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHED
        .get_or_init(|| std::env::var_os(Consts::ENV_LOG).map(PathBuf::from))
        .clone()
}

#[cfg(test)]
pub(crate) fn force_mode(m: Mode) {
    FORCED.with(|forced| forced.set(Some(m)));
}

#[cfg(test)]
pub(crate) fn force_blanket_budget(budget: Duration) {
    FORCED_BUDGET.with(|forced| forced.set(Some(budget)));
}

#[cfg(test)]
pub(crate) fn force_log_path(path: PathBuf) {
    FORCED_LOG.with(|forced| forced.replace(Some(Some(path))));
}

#[cfg(test)]
pub(crate) fn force_no_log_path() {
    FORCED_LOG.with(|forced| forced.replace(Some(None)));
}
