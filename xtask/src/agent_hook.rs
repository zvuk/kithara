use anyhow::Result;
use clap::{Args, Subcommand};

mod cache;
mod fingerprint;
mod input;
mod post_edit;
mod pre_bash;

#[derive(Debug, Args)]
pub(crate) struct AgentHookArgs {
    #[command(subcommand)]
    command: AgentHookCommand,
}

#[derive(Debug, Subcommand)]
enum AgentHookCommand {
    /// Install the current xtask binary for this Git worktree.
    Install,
    /// Claude PreToolUse(Bash) guard for command misuse.
    PreBash,
    /// Claude PostToolUse(Write|Edit|MultiEdit) formatting hook.
    PostEdit,
}

pub(crate) fn run(args: &AgentHookArgs) -> Result<()> {
    match args.command {
        AgentHookCommand::Install => cache::install(),
        AgentHookCommand::PreBash => {
            cache::warn_if_stale();
            pre_bash::run(&input::read()?)
        }
        AgentHookCommand::PostEdit => {
            cache::warn_if_stale();
            post_edit::run(&input::read()?)
        }
    }
}
