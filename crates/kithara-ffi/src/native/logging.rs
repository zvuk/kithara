use std::{
    fs::OpenOptions,
    io::stderr,
    sync::{Mutex, Once},
};

use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*};

#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn init_logging(level: u8) {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let filter = level_filter(level);
        let _ = tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .with_writer(stderr)
                    .with_target(true)
                    .with_ansi(false)
                    .with_filter(filter),
            )
            .try_init();
    });
}

/// Initialize logging to both stderr and an append-mode file at `path`.
///
/// For on-device diagnostics: the host passes a writable path inside its
/// sandbox (e.g. `Caches/kithara.log`) and pulls the file afterwards. Mutually
/// exclusive with [`init_logging`] — only the first global subscriber install
/// wins, so the host calls exactly one. If the file cannot be opened the
/// stderr layer is still installed (the file layer becomes a no-op).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn init_logging_to_file(path: String, level: u8) {
    static INIT: Once = Once::new();

    INIT.call_once(|| {
        let file_layer = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
            .map(|file| {
                fmt::layer()
                    .with_writer(Mutex::new(file))
                    .with_target(true)
                    .with_ansi(false)
                    .with_filter(level_filter(level))
            });
        let _ = tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .with_writer(stderr)
                    .with_target(true)
                    .with_ansi(false)
                    .with_filter(level_filter(level)),
            )
            .with(file_layer)
            .try_init();
    });
}

fn level_filter(ordinal: u8) -> LevelFilter {
    const LOG_LEVEL_TRACE: u8 = 0;
    const LOG_LEVEL_DEBUG: u8 = 1;
    const LOG_LEVEL_INFO: u8 = 2;
    const LOG_LEVEL_WARN: u8 = 3;
    const LOG_LEVEL_ERROR: u8 = 4;

    match ordinal {
        LOG_LEVEL_TRACE => LevelFilter::TRACE,
        LOG_LEVEL_DEBUG => LevelFilter::DEBUG,
        LOG_LEVEL_INFO => LevelFilter::INFO,
        LOG_LEVEL_WARN => LevelFilter::WARN,
        LOG_LEVEL_ERROR => LevelFilter::ERROR,
        _ => LevelFilter::OFF,
    }
}
