//! Context dump primitives used by `HangDetector` when a hang fires.
//!
//! `HangDump` is the user-facing contract: any `T: serde::Serialize`
//! implements it automatically via the blanket impl below. `NoContext`
//! is the zero-payload default used when no dump is configured.

use serde::Serialize;

/// Trait implemented by every value the detector can dump when a hang
/// fires. The default `label` returns `None`, letting the detector
/// fall back to its own constructor label.
pub trait HangDump {
    fn label(&self) -> Option<&str> {
        None
    }
    fn to_json(&self) -> String;
}

impl<T: Serialize> HangDump for T {
    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// Marker used when no context is provided. Serializes as JSON `null`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoContext;

impl Serialize for NoContext {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_unit_struct("NoContext")
    }
}

/// Replace every character outside `[A-Za-z0-9_-]` with `.` so the
/// label is safe to embed in a file path. Used for the filename segment
/// of hang-dump files.
#[must_use]
pub(crate) fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '.',
        })
        .collect()
}

/// Native file-writing dump path. WASM falls through to `wasm::write_dump`.
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::{
        fs::File,
        io::Write,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{HangDump, sanitize_label};

    const ENV_DUMP_DIR: &str = "KITHARA_HANG_DUMP_DIR";

    fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    /// Pick the directory for hang-dump files. Precedence: explicit
    /// parameter > `KITHARA_HANG_DUMP_DIR` env var > `std::env::temp_dir()`.
    #[must_use]
    pub(crate) fn resolve_dump_dir(explicit: Option<&Path>) -> PathBuf {
        if let Some(p) = explicit {
            return p.to_path_buf();
        }
        if let Some(env) = std::env::var_os(ENV_DUMP_DIR) {
            return PathBuf::from(env);
        }
        std::env::temp_dir()
    }

    /// Serialize `ctx`, write
    /// `{dir}/kithara-hang-{sanitized_label}-{ts}-{pid}.json`, emit a
    /// `tracing::error!` with the payload and resulting file path (or
    /// the write error). Called once per hang window by the detector.
    pub(crate) fn write_dump<C: HangDump>(label: &str, ctx: &C, dir: Option<&Path>) {
        let payload = ctx.to_json();
        let ts = now_ms();
        let pid = std::process::id();
        let dir = resolve_dump_dir(dir);
        let file = dir.join(format!(
            "kithara-hang-{label}-{ts}-{pid}.json",
            label = sanitize_label(label),
        ));
        match File::create(&file).and_then(|mut f| f.write_all(payload.as_bytes())) {
            Ok(()) => {
                tracing::error!(
                    target: "kithara_hang_detector",
                    label,
                    ts_ms = %ts,
                    pid,
                    dump_path = %file.display(),
                    payload = %payload,
                    "hang detected — context dump written"
                );
            }
            Err(err) => {
                tracing::error!(
                    target: "kithara_hang_detector",
                    label,
                    ts_ms = %ts,
                    pid,
                    dump_path = %file.display(),
                    payload = %payload,
                    error = %err,
                    "hang detected — failed to write context dump"
                );
            }
        }
    }
}

/// WASM dump path — no filesystem, just a `tracing::error!`.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::path::Path;

    use super::HangDump;

    pub(crate) fn write_dump<C: HangDump>(label: &str, ctx: &C, _dir: Option<&Path>) {
        tracing::error!(
            target: "kithara_hang_detector",
            label,
            payload = %ctx.to_json(),
            "hang detected — context dump (wasm; file write skipped)"
        );
    }
}

#[cfg(all(not(target_arch = "wasm32"), test))]
pub(crate) use native::resolve_dump_dir;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::write_dump;
#[cfg(target_arch = "wasm32")]
pub(crate) use wasm::write_dump;
