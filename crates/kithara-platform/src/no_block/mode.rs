#[cfg(test)]
use std::cell::Cell;
use std::{sync::OnceLock, time::Duration};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Off,
    Census,
    Panic,
}

struct Consts;

impl Consts {
    const ENV_MODE: &str = "KITHARA_NO_BLOCK";
    const ENV_BUDGET_MS: &str = "KITHARA_NO_BLOCK_BUDGET_MS";
    const FALLBACK_BLANKET: Duration = Duration::from_millis(250);
}

#[cfg(test)]
thread_local! {
    static FORCED: Cell<Option<Mode>> = const { Cell::new(None) };
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
        Ok("panic") => Mode::Panic,
        _ => Mode::Census,
    })
}

pub(super) fn blanket_budget() -> Duration {
    static CACHED: OnceLock<Duration> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var(Consts::ENV_BUDGET_MS)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map_or(Consts::FALLBACK_BLANKET, Duration::from_millis)
    })
}

#[cfg(test)]
pub(crate) fn force_mode(m: Mode) {
    FORCED.with(|forced| forced.set(Some(m)));
}
