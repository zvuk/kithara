use std::{
    fs::OpenOptions,
    io::{self, Write},
    sync::Mutex,
};

use tracing::metadata::LevelFilter;
use tracing_subscriber::EnvFilter;

/// A `Write` adapter that converts lone `\n` into `\r\n`.
///
/// Required for tracing output in raw-mode terminals where `\n` alone
/// moves the cursor down without returning to the start of the line.
struct CrlfWriter<W> {
    inner: W,
}

impl<W> CrlfWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner }
    }
}

impl<W: Write> Write for CrlfWriter<W> {
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        const CRLF_OVERHEAD: usize = 8;

        if !buf.contains(&b'\n') {
            self.inner.write_all(buf)?;
            return Ok(buf.len());
        }
        let mut out = Vec::with_capacity(buf.len() + CRLF_OVERHEAD);
        for byte in buf {
            if *byte == b'\n' {
                out.push(b'\r');
            }
            out.push(*byte);
        }

        self.inner.write_all(&out)?;
        Ok(buf.len())
    }
}

fn make_log_writer() -> CrlfWriter<io::Stderr> {
    CrlfWriter::new(io::stderr())
}

/// Default log file name for the kithara binary. Matches the legacy
/// convention used in production / iOS demo logs; override at runtime
/// via `KITHARA_LOG_FILE=<path>`.
pub const DEFAULT_LOG_FILE: &str = "app.log";

/// Initialize tracing subscriber for the application.
///
/// Filter precedence: `RUST_LOG` env if set, otherwise the `directives`
/// passed in. Output goes to `KITHARA_LOG_FILE` (or [`DEFAULT_LOG_FILE`]
/// by default — `app.log` next to the binary's working directory).
/// If `use_crlf_writer` is true and it falls back to stderr, it wraps
/// with [`CrlfWriter`].
///
/// # Errors
/// Returns an error if a tracing directive cannot be parsed or the log
/// file cannot be opened.
pub fn init_tracing(
    directives: &[&str],
    use_crlf_writer: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    } else if use_crlf_writer {
        builder.with_writer(make_log_writer).init();
    } else {
        builder.init();
    }

    Ok(())
}
