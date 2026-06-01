use std::{path::Path, time::Duration};

use super::shared::HangDump;

/// Report a detected hang. wasm cannot panic (`panic = "immediate-abort"`
/// turns it into a fatal `RuntimeError: unreachable` that kills the Worker),
/// so the detector reports through [`kithara_platform::log_error`], which
/// writes to the browser `console` and is safe in every scope (including the
/// audio worklet, where the global `tracing` subscriber's cross-instance
/// vtable would trap).
pub(crate) fn write_dump<C: HangDump>(label: &str, ctx: &C, _dir: Option<&Path>) {
    kithara_platform::log_error(&format!(
        "[kithara_hang_detector] hang detected: {label} — {}",
        ctx.dump_json()
    ));
}

#[must_use]
pub(crate) fn env_timeout() -> Option<Duration> {
    None
}
