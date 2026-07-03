use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::perf::{
    matrix::{self, MatrixParams},
    profile::{self, ProfileParams},
    report, slow,
};

#[derive(Debug, Args)]
pub(crate) struct PerfArgs {
    #[command(subcommand)]
    command: PerfCommand,
}

#[derive(Debug, Subcommand)]
enum PerfCommand {
    /// Run the full suite across feature lanes, saving `JUnit` + metadata.
    Matrix(MatrixCli),
    /// Profile every slow-list test in isolation under samply.
    Profile(ProfileCli),
    /// Render merged slow/profile report markdown.
    Report(ReportCli),
    /// Aggregate matrix `JUnit` data into `slow.json` / `slow.md`.
    Slow(SlowCli),
}

#[derive(Debug, Args)]
struct MatrixCli {
    /// Root directory for measurement data (outside the repo).
    #[arg(long)]
    data_dir: PathBuf,
    /// Identifier for this measurement run; default: run-<unix-secs>.
    #[arg(long)]
    run_id: Option<String>,
    /// Root for per-lane `CARGO_TARGET_DIRs`.
    #[arg(long)]
    target_root: PathBuf,
    /// Full-suite repetitions per lane.
    #[arg(long, default_value_t = 3)]
    repeats: u32,
    /// Lane subset (repeatable); default: all four lanes.
    #[arg(long = "lane")]
    lanes: Vec<String>,
    /// Extra args passed through to `cargo nextest run` (e.g. `-p kithara-hls`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    extra: Vec<String>,
}

#[derive(Debug, Args)]
struct SlowCli {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    run_id: String,
    /// Slow threshold in milliseconds.
    #[arg(long, default_value_t = 1000.0)]
    threshold_ms: f64,
}

#[derive(Debug, Args)]
struct ProfileCli {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    target_root: PathBuf,
    /// Lane whose build/list is used for binaries (default: primary).
    #[arg(long, default_value = "flash-on-wreq")]
    lane: String,
    /// Kill a profiled test after this many seconds.
    #[arg(long, default_value_t = 180)]
    timeout_secs: u64,
    /// Profile only the first N slow tests (pilot use).
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Debug, Args)]
struct ReportCli {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    run_id: String,
    /// Lane whose isolated profiles are read.
    #[arg(long, default_value = "flash-on-wreq")]
    lane: String,
}

pub(crate) fn run(args: &PerfArgs) -> Result<()> {
    match &args.command {
        PerfCommand::Matrix(cli) => {
            let run_id = match &cli.run_id {
                Some(id) => id.clone(),
                None => default_run_id()?,
            };
            let params = MatrixParams {
                data_dir: cli.data_dir.clone(),
                run_id,
                target_root: cli.target_root.clone(),
                repeats: cli.repeats,
                lanes: cli.lanes.clone(),
                extra: cli.extra.clone(),
            };
            matrix::run(&params)
        }
        PerfCommand::Profile(cli) => {
            let params = ProfileParams {
                data_dir: cli.data_dir.clone(),
                run_id: cli.run_id.clone(),
                target_root: cli.target_root.clone(),
                lane: cli.lane.clone(),
                timeout_secs: cli.timeout_secs,
                limit: cli.limit,
            };
            profile::run(&params)
        }
        PerfCommand::Report(cli) => report::run(&cli.data_dir, &cli.run_id, &cli.lane),
        PerfCommand::Slow(cli) => slow::run(&cli.data_dir, &cli.run_id, cli.threshold_ms),
    }
}

fn default_run_id() -> Result<String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("clock before epoch")?
        .as_secs();
    Ok(format!("run-{secs}"))
}
