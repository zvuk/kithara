use kithara_platform::time::Duration;
use serde::Serialize;

/// Context payload that a [`HangDetector`](super::HangDetector) serializes when
/// a hang fires.
pub trait HangDump {
    fn dump_json(&self) -> String;
    fn label(&self) -> Option<&str> {
        None
    }
}

impl<T: Serialize> HangDump for T {
    fn dump_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// Default empty context for a detector that carries no payload.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoContext;

impl Serialize for NoContext {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_unit_struct("NoContext")
    }
}

/// Watchdog timeout, from `KITHARA_HANG_TIMEOUT_SECS` (native) or a built-in
/// fallback when unset.
#[must_use]
pub fn default_timeout() -> Duration {
    // xtask-lint-ignore: retry_fallback
    const FALLBACK_TIMEOUT: Duration = Duration::from_secs(10);
    super::platform::env_timeout().unwrap_or(FALLBACK_TIMEOUT)
}
