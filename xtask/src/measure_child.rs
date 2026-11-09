use std::{
    ffi::OsString,
    os::unix::process::ExitStatusExt as _,
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use kithara_devtools::verdict::ChildFailure;
use nix::sys::{
    resource::{UsageWho, getrusage},
    time::TimeValLike as _,
};

#[derive(Debug, Args)]
pub(crate) struct MeasureChildArgs {
    /// Executable and its original arguments.
    #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<OsString>,
}

pub(crate) fn run(args: &MeasureChildArgs) -> Result<()> {
    let (program, arguments) = args
        .command
        .split_first()
        .context("missing child command")?;
    let before = getrusage(UsageWho::RUSAGE_CHILDREN).context("read initial child CPU")?;
    let started = Instant::now();
    let status = Command::new(program)
        .args(arguments)
        .status()
        .context("execute measured child")?;
    let elapsed = started.elapsed();
    let after = getrusage(UsageWho::RUSAGE_CHILDREN).context("read final child CPU")?;
    let user = Duration::from_micros(u64::try_from(
        (after.user_time() - before.user_time()).num_microseconds(),
    )?);
    let system = Duration::from_micros(u64::try_from(
        (after.system_time() - before.system_time()).num_microseconds(),
    )?);
    let metrics = serde_json::json!({
        "wall_seconds": elapsed.as_secs_f64(),
        "user_seconds": user.as_secs_f64(),
        "system_seconds": system.as_secs_f64(),
    });
    eprintln!("KITHARA_NATIVE_TIMING {metrics}");
    if let Some(signal) = status.signal() {
        signal_hook::low_level::emulate_default_handler(signal)
            .context("preserve measured child signal")?;
        bail!("measured child signal did not terminate the observer");
    }
    if !status.success() {
        return Err(ChildFailure::inherited(
            "measured child".to_owned(),
            status.code(),
        ));
    }
    Ok(())
}
