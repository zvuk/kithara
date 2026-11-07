use std::{ffi::OsString, path::PathBuf};

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use kithara_devtools::Ctx;

use super::{
    bridge::BridgeArgs,
    cache::CacheArgs,
    host::HostArgs,
    image::ImageArgs,
    lane::{direct::LaneArgs, select::LanesArgs},
    run::RunArgs,
    verdict::VerdictArgs,
};

#[derive(Debug, Args)]
pub(crate) struct CiArgs {
    #[command(subcommand)]
    command: CiCommand,
}

#[derive(Debug, Subcommand)]
enum CiCommand {
    /// Run both Flash modes, stopping on the first failure.
    Gate {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },
    /// Run lint, tests and mutations with independent stage logs.
    Full { output: PathBuf },
    /// Provision and verify the shared compiler cache.
    Cache(CacheArgs),
    /// Reconcile the public GitHub and private `GitLab` repositories.
    Bridge(BridgeArgs),
    /// Provision and maintain a CI machine: its images, runners and agents.
    Host(HostArgs),
    /// Build the pinned container images a CI machine runs jobs in.
    Image(ImageArgs),
    /// Run one declared lane in a prepared environment.
    Lane(LaneArgs),
    /// Render the jobs a role runs for one pipeline kind.
    Lanes(LanesArgs),
    /// Execute one repository CI lane.
    Run(RunArgs),
    /// Print whether a branch changes any scoped test lane's owned paths.
    Touched(TouchedArgs),
    /// Record what the default branch fails, and hold a run that fails more.
    Verdict(VerdictArgs),
    /// Capture native Windows counters and retained test failure evidence.
    WindowsDiagnostics {
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Args)]
struct TouchedArgs {
    /// Test lanes whose ownership to inspect.
    #[arg(long, required = true)]
    lane: Vec<String>,
}

pub(crate) const fn is_standalone(args: &CiArgs) -> bool {
    matches!(
        args.command,
        CiCommand::Cache(_)
            | CiCommand::Bridge(_)
            | CiCommand::Host(_)
            | CiCommand::Image(_)
            | CiCommand::Verdict(_)
    )
}

pub(crate) fn run_standalone(args: &CiArgs) -> Result<()> {
    match &args.command {
        CiCommand::Cache(args) => super::cache::run(args),
        CiCommand::Bridge(args) => super::bridge::run(args),
        CiCommand::Host(args) => super::host::run(args),
        CiCommand::Image(args) => super::image::run(args),
        CiCommand::Verdict(args) => super::verdict::run(args),
        CiCommand::Run(_)
        | CiCommand::Gate { .. }
        | CiCommand::Full { .. }
        | CiCommand::Lane(_)
        | CiCommand::Lanes(_)
        | CiCommand::Touched(_)
        | CiCommand::WindowsDiagnostics { .. } => {
            bail!("repository CI lanes require a workspace")
        }
    }
}

pub(crate) fn run(args: &CiArgs, ctx: &Ctx) -> Result<()> {
    match &args.command {
        CiCommand::Cache(_)
        | CiCommand::Bridge(_)
        | CiCommand::Host(_)
        | CiCommand::Image(_)
        | CiCommand::Verdict(_) => run_standalone(args),
        CiCommand::Run(args) => super::run::run(args, ctx),
        CiCommand::Gate { arguments } => super::cycle::gate(ctx, arguments),
        CiCommand::Full { output } => super::cycle::full(ctx, output),
        CiCommand::Lane(args) => super::lane::direct::run(args, ctx),
        CiCommand::Lanes(args) => super::lane::select::run(args, ctx),
        CiCommand::WindowsDiagnostics { output } => super::windows_diagnostics::run(output, ctx),
        CiCommand::Touched(args) => {
            let touched = kithara_devtools::test::touches_lanes(
                &ctx.config.test,
                &ctx.root,
                &kithara_devtools::test::touched_base(&|name| std::env::var_os(name)),
                &args.lane,
            )?;
            println!("{touched}");
            Ok(())
        }
    }
}
