use std::{fs::OpenOptions, sync::Mutex};

use tracing::metadata::LevelFilter;
use tracing_subscriber::EnvFilter;

/// Default log file name for the kithara binary. Matches the legacy
/// convention used in production / iOS demo logs; override at runtime
/// via `KITHARA_LOG_FILE=<path>`.
pub const DEFAULT_LOG_FILE: &str = "app.log";

/// Initialize tracing subscriber for the application.
///
/// Filter precedence: `RUST_LOG` env if set, otherwise the `directives`
/// passed in. Output goes to `KITHARA_LOG_FILE` (or [`DEFAULT_LOG_FILE`]
/// by default — `app.log` next to the binary's working directory).
///
/// # Errors
/// Returns an error if a tracing directive cannot be parsed or the log
/// file cannot be opened.
pub fn init_tracing(directives: &[&str]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let filter = if let Ok(env_filter) = EnvFilter::try_from_default_env() {
        env_filter
    } else {
        let mut filter = EnvFilter::default();
        for directive in directives {
            filter = filter.add_directive((*directive).parse()?);
        }
        let has_global_level = directives.iter().any(|directive| !directive.contains('='));
        if !has_global_level {
            filter = filter.add_directive(LevelFilter::WARN.into());
        }
        filter
    };

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_line_number(false)
        .with_file(false);

    let path = std::env::var_os("KITHARA_LOG_FILE").unwrap_or_else(|| DEFAULT_LOG_FILE.into());
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
        builder
            .with_writer(Mutex::new(file))
            .with_ansi(false)
            .init();
    } else {
        builder.init();
    }

    Ok(())
}
