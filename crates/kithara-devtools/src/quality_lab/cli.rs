use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};

use super::{
    config::QualityLabConfig,
    manifest::{Profile, Tool},
    orchestrator,
};
use crate::Ctx;

#[derive(Debug, Subcommand)]
pub enum LabCommand {
    /// List configured Quality Lab profiles and tools.
    List,
    /// Run a profile or one analyzer.
    Run(RunArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(value_enum)]
    pub(super) target: RunTarget,
    /// LCOV input for the cargo-crap coverage profile.
    #[arg(long)]
    pub(super) lcov: Option<PathBuf>,
    /// Absolute cargo-crap JSON report from a successful production run.
    #[arg(long, requires = "lcov")]
    pub(super) baseline: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum RunTarget {
    Coverage,
    Scheduled,
    Manual,
    CargoCrap,
    Cha,
    Rustqual,
    CargoDupes,
    Pmat,
}

impl RunTarget {
    pub(super) const fn profile(self) -> Option<Profile> {
        match self {
            Self::Coverage => Some(Profile::Coverage),
            Self::Scheduled => Some(Profile::Scheduled),
            Self::Manual => Some(Profile::Manual),
            Self::CargoCrap | Self::Cha | Self::Rustqual | Self::CargoDupes | Self::Pmat => None,
        }
    }

    pub(super) const fn tool(self) -> Option<Tool> {
        match self {
            Self::CargoCrap => Some(Tool::CargoCrap),
            Self::Cha => Some(Tool::Cha),
            Self::Rustqual => Some(Tool::Rustqual),
            Self::CargoDupes => Some(Tool::CargoDupes),
            Self::Pmat => Some(Tool::Pmat),
            Self::Coverage | Self::Scheduled | Self::Manual => None,
        }
    }
}

/// Runs a configured heavyweight analysis profile or prints its policy.
///
/// # Errors
///
/// Returns an error when configuration is invalid, a required analyzer cannot
/// run, a native report is malformed, or a gating profile finds a regression.
pub fn run(command: &LabCommand, ctx: &Ctx) -> Result<()> {
    let config = QualityLabConfig::load(&ctx.root)?;
    match command {
        LabCommand::List => {
            print_config(&config);
            Ok(())
        }
        LabCommand::Run(args) => orchestrator::run(args, &config, ctx),
    }
}

fn print_config(config: &QualityLabConfig) {
    println!("Quality Lab profiles:");
    for profile in [Profile::Coverage, Profile::Scheduled, Profile::Manual] {
        let tools = config
            .profiles
            .get(profile)
            .tools
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {profile}: {tools} (budget: {}s)",
            config.profiles.get(profile).timeout_secs
        );
    }
    println!("Pinned tools:");
    for tool in Tool::ALL {
        let tool_config = config.tools.get(tool);
        println!(
            "  {tool} {} (timeout: {}s)",
            tool_config.version, tool_config.timeout_secs
        );
    }
}
