use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::{
    Ctx,
    perf::{
        lanes::primary_lane_name,
        matrix::{self, MatrixParams},
        profile::{self, ProfileParams},
        report, slow,
        trace::{self, TraceParams},
    },
};

#[derive(Debug, Args)]
pub struct PerfArgs {
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
    /// Record one test under Instruments Time Profiler.
    Trace(TraceCli),
}

#[derive(Debug, Args)]
struct MatrixCli {
    /// Identifier for this measurement run; default: run-<unix-secs>.
    #[arg(long)]
    run_id: Option<String>,
    /// Root directory for measurement data (outside the repo).
    #[arg(long)]
    data_dir: PathBuf,
    /// Root for per-lane `CARGO_TARGET_DIRs`.
    #[arg(long)]
    target_root: PathBuf,
    /// Extra args passed through to `cargo nextest run`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    extra: Vec<String>,
    /// Lane subset (repeatable); default: all configured lanes.
    #[arg(long = "lane")]
    lanes: Vec<String>,
    /// Full-suite repetitions per lane.
    #[arg(long, default_value_t = 3)]
    repeats: u32,
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
    /// Lane whose build/list is used for binaries (default: perf primary lane).
    #[arg(long)]
    lane: Option<String>,
    /// Profile only the first N slow tests (pilot use).
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    target_root: PathBuf,
    #[arg(long)]
    run_id: String,
    /// Kill a profiled test after this many seconds.
    #[arg(long, default_value_t = 180)]
    timeout_secs: u64,
}

#[derive(Debug, Args)]
struct ReportCli {
    /// Lane whose isolated profiles are read (default: perf primary lane).
    #[arg(long)]
    lane: Option<String>,
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    run_id: String,
}

#[derive(Debug, Args)]
struct TraceCli {
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long)]
    target_root: PathBuf,
    #[arg(long)]
    lane: String,
    #[arg(long)]
    run_id: String,
    #[arg(long)]
    suite: String,
    #[arg(long)]
    test: String,
}

pub(crate) fn run(args: &PerfArgs, ctx: &Ctx) -> Result<()> {
    match &args.command {
        PerfCommand::Matrix(cli) => {
            let run_id = match &cli.run_id {
                Some(id) => id.clone(),
                None => default_run_id()?,
            };
            let params = MatrixParams {
                run_id,
                data_dir: cli.data_dir.clone(),
                target_root: cli.target_root.clone(),
                repeats: cli.repeats,
                lanes: cli.lanes.clone(),
                extra: cli.extra.clone(),
            };
            matrix::run(&params, &ctx.config)
        }
        PerfCommand::Profile(cli) => {
            let lane = match &cli.lane {
                Some(lane) => lane.clone(),
                None => primary_lane_name(&ctx.config.perf)?,
            };
            let params = ProfileParams {
                lane,
                data_dir: cli.data_dir.clone(),
                run_id: cli.run_id.clone(),
                target_root: cli.target_root.clone(),
                timeout_secs: cli.timeout_secs,
                limit: cli.limit,
            };
            profile::run(&params, &ctx.config)
        }
        PerfCommand::Report(cli) => {
            let lane = match &cli.lane {
                Some(lane) => lane.clone(),
                None => primary_lane_name(&ctx.config.perf)?,
            };
            report::run(&ctx.config, &cli.data_dir, &cli.run_id, &lane)
        }
        PerfCommand::Slow(cli) => {
            slow::run(&ctx.config, &cli.data_dir, &cli.run_id, cli.threshold_ms)
        }
        PerfCommand::Trace(cli) => {
            let params = TraceParams {
                data_dir: cli.data_dir.clone(),
                run_id: cli.run_id.clone(),
                target_root: cli.target_root.clone(),
                lane: cli.lane.clone(),
                suite: cli.suite.clone(),
                test: cli.test.clone(),
            };
            trace::run(&params, &ctx.config)
        }
    }
}

fn default_run_id() -> Result<String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("clock before epoch")?
        .as_secs();
    Ok(format!("run-{secs}"))
}
