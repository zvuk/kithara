use std::{marker::PhantomData, path::PathBuf, time::Duration};

pub trait HangDump {
    fn label(&self) -> Option<&str> {
        None
    }
    fn to_json(&self) -> String;
}

impl<T> HangDump for T {
    fn to_json(&self) -> String {
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

    #[inline(always)]
    pub fn reset(&mut self) {}

    #[inline(always)]
    pub fn tick(&mut self) {}

    #[inline(always)]
    pub fn tick_with(&mut self, _ctx: C) {}

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
