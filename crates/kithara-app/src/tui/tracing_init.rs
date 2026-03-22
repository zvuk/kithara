use std::io::{self, Write};

use tracing::metadata::LevelFilter;
use tracing_subscriber::EnvFilter;

use super::session::TuiResult;

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

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn make_log_writer() -> CrlfWriter<io::Stderr> {
    CrlfWriter::new(io::stderr())
}

/// Initialize tracing subscriber for TUI mode.
///
/// When `use_crlf_writer` is true, output is wrapped in [`CrlfWriter`]
/// to produce correct line endings in raw-mode terminals.
///
/// # Errors
/// Returns an error if a tracing directive cannot be parsed.
pub fn init_tracing(directives: &[&str], use_crlf_writer: bool) -> TuiResult {
    let mut filter = EnvFilter::default();
    for directive in directives {
        filter = filter.add_directive((*directive).parse()?);
    }
    let has_global_level = directives.iter().any(|directive| !directive.contains('='));
    if !has_global_level {
        filter = filter.add_directive(LevelFilter::WARN.into());
    }

    if use_crlf_writer {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(make_log_writer)
            .with_line_number(false)
            .with_file(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_line_number(false)
            .with_file(false)
            .init();
    }

    Ok(())
}
