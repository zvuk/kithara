use std::{path::Path, time::Duration};

use super::detector::HangDump;

pub(crate) fn write_dump<C: HangDump>(label: &str, ctx: &C, _dir: Option<&Path>) {
    tracing::error!(
        target: "kithara_hang_detector",
        label,
        payload = %ctx.to_json(),
        "hang detected — context dump (wasm; file write skipped)"
    );
}

#[must_use]
pub(crate) fn env_timeout() -> Option<Duration> {
    None
}

#[must_use]
pub(crate) fn parse_timeout_secs(_value: &str) -> Option<Duration> {
    None
}

#[cfg(test)]
#[must_use]
pub(crate) fn resolve_dump_dir(_explicit: Option<&Path>) -> std::path::PathBuf {
    std::path::PathBuf::new()
}
