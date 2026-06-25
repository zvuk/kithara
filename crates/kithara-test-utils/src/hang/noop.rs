use std::{marker::PhantomData, path::PathBuf};

use kithara_platform::time::Duration;

pub trait HangDump {
    fn dump_json(&self) -> String;
    fn label(&self) -> Option<&str> {
        None
    }
}

impl<T> HangDump for T {
    fn dump_json(&self) -> String {
        String::new()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoContext;

pub struct HangDetector<C: HangDump = NoContext>(PhantomData<C>);

impl<C: HangDump> HangDetector<C> {
    #[inline(always)]
    #[must_use]
    pub fn new(_label: &'static str, _timeout: Duration) -> Self {
        Self(PhantomData)
    }

    /// No watchdog compiled in: bound an event wait by the fallback timeout
    /// rather than below it, so a no-`hang` build still re-checks instead of
    /// blocking forever on a lost wakeup. (`&mut self` mirrors the real
    /// detector, whose lazy deadline stamp needs it.)
    #[inline(always)]
    #[must_use]
    pub fn remaining(&mut self) -> Duration {
        default_timeout()
    }

    #[inline(always)]
    pub fn reset(&mut self) {}

    #[inline(always)]
    pub fn reset_from(&mut self, _file: &'static str, _line: u32) {}

    #[inline(always)]
    pub fn tick(&mut self) {}

    #[inline(always)]
    pub fn tick_from(&mut self, _file: &'static str, _line: u32) {}

    #[inline(always)]
    pub fn tick_with(&mut self, _ctx: C) {}

    #[inline(always)]
    pub fn tick_with_from(&mut self, _ctx: C, _file: &'static str, _line: u32) {}

    #[inline(always)]
    #[must_use]
    pub fn with_dump_dir(self, _dir: PathBuf) -> Self {
        self
    }
}

#[must_use]
pub fn default_timeout() -> Duration {
    // xtask-lint-ignore: retry_fallback
    const FALLBACK_SECS: u64 = 10;
    Duration::from_secs(FALLBACK_SECS)
}
