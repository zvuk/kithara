use anyhow::Result;

use crate::ci::process::{Process, require_os};

fn preflight(process: &Process) -> Result<()> {
    require_os("macos", "deep Apple")?;
    process.require_tools(&["cargo", "just"])
}

pub(crate) fn rtsan(process: &Process) -> Result<()> {
    preflight(process)?;
    for command in [
        ["test", "rtsan"].as_slice(),
        ["test", "rtsan-async"].as_slice(),
        ["test", "rtsan-file"].as_slice(),
        ["test", "rtsan-hls"].as_slice(),
    ] {
        process.run("just", command, "real-time sanitizer suite")?;
    }
    Ok(())
}

pub(crate) fn perf(process: &Process) -> Result<()> {
    preflight(process)?;
    process.run("just", &["perf", "memory"], "memory profile")?;
    process.run("just", &["perf"], "performance profile")
}

pub(crate) fn bench(process: &Process) -> Result<()> {
    preflight(process)?;
    process.run("just", &["perf", "bench", "ci"], "benchmark suite")
}
