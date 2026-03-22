use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod android;
mod apple;
mod arch;
mod perf_compare;
mod publish;
mod quality;
mod util;
mod wasm;

use android::AndroidCommand;
use apple::AppleCommand;
use publish::PublishArgs;
use quality::QualityCommand;
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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::PerfCompare {
            current,
            baseline,
            threshold,
        } => perf_compare::run(&current, &baseline, threshold),
        Command::Quality { command } => quality::run(command),
        Command::Android { command } => android::run(command),
        Command::Apple { command } => apple::run(command),
        Command::Wasm { command } => wasm::run(command),
        Command::Publish(ref args) => publish::run(args),
    }
}
