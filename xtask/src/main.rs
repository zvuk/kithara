use clap::{Parser, Subcommand};
use kithara_devtools::{CoreCommand, Ctx};

mod agent_hook;
mod android;
mod apple;
mod apple_docgen;
mod config;
mod publish;
mod release;
mod self_cache;
mod wasm;

use android::AndroidCommand;
use apple::AppleCommand;
use publish::PublishArgs;
use release::ReleaseArgs;
use self_cache::SelfCacheArgs;
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
    AgentHook,
    #[command(hide = true)]
    SelfCache(SelfCacheArgs),
    #[command(flatten)]
    Core(CoreCommand),
}

fn main() -> anyhow::Result<()> {
    let _lease: Option<self_cache::GenerationLease> = self_cache::lease_current()?;
    let cli = Cli::parse();
    match &cli.command {
        Command::AgentHook => return agent_hook::run(),
        Command::SelfCache(args) => return self_cache::run(args),
        _ => {}
    }
    let ctx = Ctx::load()?;

    match cli.command {
        Command::Android { command } => android::run(command, &ctx),
        Command::Apple { command } => apple::run(command, &ctx),
        Command::Wasm { command } => wasm::run(command, &ctx),
        Command::Publish(ref args) => publish::run(args, &ctx),
        Command::Release(ref args) => release::run(args, &ctx),
        Command::AgentHook => agent_hook::run(),
        Command::SelfCache(ref args) => self_cache::run(args),
        Command::Core(cmd) => kithara_devtools::run(&cmd, &ctx),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cli;

    #[test]
    fn agent_hook_uses_payload_without_cli_discriminator() {
        assert!(Cli::try_parse_from(["xtask", "agent-hook"]).is_ok());
        assert!(Cli::try_parse_from(["xtask", "agent-hook", "pre-bash"]).is_err());
    }

    #[test]
    fn self_cache_command_is_available_to_just_transport() {
        assert!(Cli::try_parse_from(["xtask", "self-cache", "probe"]).is_ok());
    }
}
