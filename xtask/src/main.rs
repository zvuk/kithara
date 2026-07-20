use clap::{Parser, Subcommand};
use kithara_devtools::{CoreCommand, Ctx};

mod agent_hook;
mod android;
mod apple;
mod apple_docgen;
mod config;
mod publish;
mod release;
mod wasm;

use agent_hook::AgentHookArgs;
use android::AndroidCommand;
use apple::AppleCommand;
use publish::PublishArgs;
use release::ReleaseArgs;
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
    /// Agent editor/shell hooks for tool-specific adapters.
    AgentHook(AgentHookArgs),
    #[command(flatten)]
    Core(CoreCommand),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Command::AgentHook(args) = &cli.command {
        return agent_hook::run(args);
    }
    let ctx = Ctx::load()?;

    match cli.command {
        Command::Android { command } => android::run(command, &ctx),
        Command::Apple { command } => apple::run(command, &ctx),
        Command::Wasm { command } => wasm::run(command, &ctx),
        Command::Publish(ref args) => publish::run(args, &ctx),
        Command::Release(ref args) => release::run(args, &ctx),
        Command::AgentHook(ref args) => agent_hook::run(args),
        Command::Core(cmd) => kithara_devtools::run(&cmd, &ctx),
    }
}
