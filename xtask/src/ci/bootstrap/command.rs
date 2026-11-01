use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

use super::{CompilerCache, cache, lease};

/// Execute one compilation phase, optionally holding its build target's lease.
///
/// # Errors
/// Returns an error for invalid arguments, an unavailable cache, or a failed lease or child launch.
pub fn run<I>(mut args: I) -> io::Result<ExitStatus>
where
    I: Iterator<Item = OsString>,
{
    let mode = args
        .next()
        .ok_or_else(|| io::Error::other("missing bootstrap mode"))?;
    let mode = if mode == "--bootstrap-runtime" {
        let runtime = args
            .next()
            .ok_or_else(|| io::Error::other("missing bootstrap runtime"))?;
        cache::finish_bootstrap(Path::new(&runtime))?;
        args.next()
            .ok_or_else(|| io::Error::other("missing bootstrap mode"))?
    } else {
        mode
    };
    let directory = match mode.to_str() {
        Some("lease") => {
            Some(PathBuf::from(args.next().ok_or_else(|| {
                io::Error::other("missing build directory")
            })?))
        }
        Some("cache") => None,
        _ => return Err(io::Error::other("invalid bootstrap mode")),
    };
    let phase = args
        .next()
        .ok_or_else(|| io::Error::other("missing compilation phase"))?;
    let program = args
        .next()
        .ok_or_else(|| io::Error::other("missing compilation command"))?;
    let mut command = Command::new(program);
    command.args(args);
    let cache = if directory.is_none() {
        CompilerCache::enter()?
    } else {
        None
    };
    if let Some(cache) = &cache {
        cache.apply(&mut command);
    }
    let result = match directory {
        Some(directory) => lease::run(&directory, &mut command),
        None => command.status(),
    };
    if let Some(cache) = cache {
        cache.record(&phase);
        let stopped = cache.finish();
        if result.as_ref().is_ok_and(ExitStatus::success) {
            stopped?;
        }
    }
    result
}
