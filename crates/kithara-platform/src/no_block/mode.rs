#[cfg(test)]
use std::cell::{Cell, RefCell};
use std::{path::PathBuf, sync::OnceLock, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    Census,
    Panic,
}

struct Consts;

impl Consts {
    const ENV_MODE: &str = "KITHARA_NO_BLOCK";
    const ENV_LOG: &str = "KITHARA_NO_BLOCK_LOG";
    const ENV_BUDGET_MS: &str = "KITHARA_NO_BLOCK_BUDGET_MS";
    /// Blanket budget panics on CPU spin only; wait class logs by construction, and `KITHARA_NO_BLOCK_BUDGET_MS` overrides.
    const FALLBACK_BLANKET: Duration = Duration::from_millis(3_000);
}

#[cfg(test)]
thread_local! {
    static FORCED: Cell<Option<Mode>> = const { Cell::new(None) };
    static FORCED_BUDGET: Cell<Option<Duration>> = const { Cell::new(None) };
    static FORCED_LOG: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub(super) fn mode() -> Mode {
    #[cfg(test)]
    {
        if let Some(mode) = FORCED.with(Cell::get) {
            return mode;
        }
    }

    static CACHED: OnceLock<Mode> = OnceLock::new();
    *CACHED.get_or_init(|| match std::env::var(Consts::ENV_MODE).as_deref() {
        Ok("off") => Mode::Off,
        Ok("census") => Mode::Census,
        _ => Mode::Panic,
    })
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
        if let Some(path) = FORCED_LOG.with(|forced| forced.borrow().clone()) {
            return Some(path);
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
    FORCED_LOG.with(|forced| forced.replace(Some(path)));
}
