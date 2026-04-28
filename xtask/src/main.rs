use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod android;
mod apple;
mod arch;
mod ast_grep;
mod common;
mod idioms;
mod lint;
mod perf_compare;
mod publish;
mod quality;
mod scope;
mod style;
mod util;
mod wasm;

use android::AndroidCommand;
use apple::AppleCommand;
use ast_grep::AstGrepArgs;
use lint::LintArgs;
use publish::PublishArgs;
use quality::QualityCommand;
use scope::ScopeArgs;
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
    /// Translate scope tokens to tool-specific flags (used by `just audit`).
    Scope(ScopeArgs),
    /// Thin wrapper around `ast-grep scan` that bakes in the policy filter list.
    AstGrep(AstGrepArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

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
        Command::Scope(ref args) => scope::run(args),
        Command::AstGrep(ref args) => ast_grep::run(args),
    }
}
