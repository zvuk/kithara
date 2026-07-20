use anyhow::Result;
use clap::{Args, Subcommand};
use kithara_agent_hook::HookCommand;

#[derive(Debug, Args)]
pub(crate) struct AgentHookArgs {
    #[command(subcommand)]
    pub(crate) command: AgentHookCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentHookCommand {
    /// Claude PreToolUse(Bash) guard for command misuse.
    PreBash,
    /// Claude PostToolUse(Write|Edit|MultiEdit) formatting hook.
    PostEdit,
}

pub(crate) fn run(args: &AgentHookArgs) -> Result<()> {
    let command = match args.command {
        AgentHookCommand::PreBash => HookCommand::PreBash,
        AgentHookCommand::PostEdit => HookCommand::PostEdit,
    };
    kithara_agent_hook::run(command)
}
