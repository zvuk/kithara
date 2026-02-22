use std::{
    error::Error,
    io::{self, Write},
};

use tracing::metadata::LevelFilter;
use tracing_subscriber::EnvFilter;

type ExampleError = Box<dyn Error + Send + Sync>;
type ExampleResult<T = ()> = Result<T, ExampleError>;

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
        if !buf.contains(&b'\n') {
            self.inner.write_all(buf)?;
            return Ok(buf.len());
        }

        let mut out = Vec::with_capacity(buf.len() + 8);
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

pub(crate) fn init_tracing(directives: &[&str], use_crlf_writer: bool) -> ExampleResult {
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
