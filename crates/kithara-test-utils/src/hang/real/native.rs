use std::{
    env,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::detector::{HangDump, sanitize_label};

struct Consts;
impl Consts {
    const ENV_DUMP_DIR: &str = "KITHARA_HANG_DUMP_DIR";
    const ENV_TIMEOUT_SECS: &str = "KITHARA_HANG_TIMEOUT_SECS";
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[must_use]
pub(crate) fn resolve_dump_dir(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    if let Some(env) = env::var_os(Consts::ENV_DUMP_DIR) {
        return PathBuf::from(env);
    }
    env::temp_dir()
}

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

#[must_use]
pub(crate) fn env_timeout() -> Option<Duration> {
    static CACHED: OnceLock<Option<Duration>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let value = env::var(Consts::ENV_TIMEOUT_SECS).ok()?;
        parse_timeout_secs(&value)
    })
}

#[must_use]
pub(crate) fn parse_timeout_secs(value: &str) -> Option<Duration> {
    let secs = value.parse::<u64>().ok()?;
    (secs > 0).then_some(Duration::from_secs(secs))
}
