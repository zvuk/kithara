use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod arch;
mod perf_compare;
mod quality;
mod xcframework;

use quality::QualityCommand;

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Workspace automation tasks for kithara")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate workspace architecture.
    Arch,
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
    /// Build an XCFramework.
    Xcframework {
        /// Build profile.
        #[arg(long, default_value = "release")]
        profile: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Arch => arch::run(),
        Command::PerfCompare {
            current,
            baseline,
            threshold,
        } => perf_compare::run(current, baseline, threshold),
        Command::Quality { command } => quality::run(command),
        Command::Xcframework { profile } => xcframework::run(profile),
    }
}
