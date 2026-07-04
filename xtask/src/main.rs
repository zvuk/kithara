use std::path::PathBuf;

use clap::{Parser, Subcommand};
use kithara_xtask_core::{CoreCommand, Ctx};

mod agent_hook;
mod android;
mod apple;
mod apple_docgen;
mod arch;
mod health;
mod idioms;
mod lint;
mod perf_compare;
mod publish;
mod quality;
mod release;
mod scope;
mod style;
mod test;
mod viz;
mod wasm;

use agent_hook::AgentHookArgs;
use android::AndroidCommand;
use apple::AppleCommand;
use health::HealthArgs;
use lint::LintArgs;
use publish::PublishArgs;
use quality::QualityCommand;
use release::ReleaseArgs;
use scope::ScopeArgs;
use test::TestArgs;
use viz::VizArgs;
use wasm::WasmCommand;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum BuildProfile {
    Debug,
    Release,
}

impl std::fmt::Display for BuildProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Debug => write!(f, "debug"),
            Self::Release => write!(f, "release"),
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Workspace automation tasks for kithara")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compare perf results.
    PerfCompare {
        /// Path to the current results file.
        current: PathBuf,
        /// Path to the baseline results file.
        baseline: PathBuf,
        /// Regression threshold percentage.
        #[arg(long, default_value_t = 10)]
        threshold: u32,
    },
    /// Workspace linters: arch, style, idioms (run all, or one via subcommand).
    Lint(LintArgs),
    /// Code quality checks.
    Quality {
        #[command(subcommand)]
        command: QualityCommand,
    },
    /// Android build tasks.
    Android {
        #[command(subcommand)]
        command: AndroidCommand,
    },
    /// Apple build tasks.
    Apple {
        #[command(subcommand)]
        command: AppleCommand,
    },
    /// WASM build and post-build tasks.
    Wasm {
        #[command(subcommand)]
        command: WasmCommand,
    },
    /// Publish all public crates to crates.io in dependency order.
    Publish(PublishArgs),
    /// Apple release flow: prepare (stamp manifests) and publish
    /// (GitHub release + `GitLab` mirror).
    Release(ReleaseArgs),
    /// Translate scope tokens to tool-specific flags (used by `just audit`).
    Scope(ScopeArgs),
    /// Agent editor/shell hooks for tool-specific adapters.
    AgentHook(AgentHookArgs),
    /// Run workspace tests through `cargo nextest`.
    Test(TestArgs),
    #[command(flatten)]
    Core(CoreCommand),
    /// Comprehensive workspace health check with markdown report.
    Health(HealthArgs),
    /// Architecture visualization tools (hierarchy, arc-map).
    Viz(VizArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let ctx = Ctx::load()?;

    match cli.command {
        Command::PerfCompare {
            current,
            baseline,
            threshold,
        } => perf_compare::run(&current, &baseline, threshold),
        Command::Lint(ref args) => lint::run(args),
        Command::Quality { command } => quality::run(command),
        Command::Android { command } => android::run(command),
        Command::Apple { command } => apple::run(command),
        Command::Wasm { command } => wasm::run(command),
        Command::Publish(ref args) => publish::run(args),
        Command::Release(ref args) => release::run(args),
        Command::Scope(ref args) => scope::run(args),
        Command::AgentHook(ref args) => agent_hook::run(args),
        Command::Test(ref args) => test::run(args),
        Command::Core(cmd) => kithara_xtask_core::run(&cmd, &ctx),
        Command::Health(ref args) => health::run(args),
        Command::Viz(ref args) => viz::run(args),
    }
}
