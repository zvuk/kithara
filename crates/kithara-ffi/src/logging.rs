//! Tracing subscriber init exposed across FFI.
//!
//! Writes formatted events to stderr — Xcode console captures stderr on
//! Apple targets, desktop hosts see it directly. Android consumers may
//! still layer the platform-native `tracing-android` adapter on top via
//! `nativeInit` for `logcat` integration.

use std::sync::Once;

use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*};

static INIT: Once = Once::new();

const LOG_LEVEL_TRACE: u8 = 0;
const LOG_LEVEL_DEBUG: u8 = 1;
const LOG_LEVEL_INFO: u8 = 2;
const LOG_LEVEL_WARN: u8 = 3;
const LOG_LEVEL_ERROR: u8 = 4;

/// Initialize Rust tracing for FFI consumers.
///
/// Writes formatted events to stderr. Idempotent — subsequent calls are
/// no-ops.
///
/// `level` ordinal: 0=trace, 1=debug, 2=info, 3=warn, 4=error, anything
/// else = off.
#[cfg_attr(feature = "backend-uniffi", uniffi::export)]
pub fn init_logging(level: u8) {
    INIT.call_once(|| {
        let filter = level_filter(level);
        let _ = tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_target(true)
                    .with_ansi(false)
                    .with_filter(filter),
            )
            .try_init();
    });
}

fn level_filter(ordinal: u8) -> LevelFilter {
    match ordinal {
        LOG_LEVEL_TRACE => LevelFilter::TRACE,
        LOG_LEVEL_DEBUG => LevelFilter::DEBUG,
        LOG_LEVEL_INFO => LevelFilter::INFO,
        LOG_LEVEL_WARN => LevelFilter::WARN,
        LOG_LEVEL_ERROR => LevelFilter::ERROR,
        _ => LevelFilter::OFF,
    }
}
