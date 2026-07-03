use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use crate::perf::matrix::{self, MatrixParams};

#[derive(Debug, Args)]
pub(crate) struct PerfArgs {
    #[command(subcommand)]
    command: PerfCommand,
}

#[derive(Debug, Subcommand)]
enum PerfCommand {
    /// Run the full suite across feature lanes, saving `JUnit` + metadata.
    Matrix(MatrixCli),
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
    }
}

fn default_run_id() -> Result<String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("clock before epoch")?
        .as_secs();
    Ok(format!("run-{secs}"))
}
