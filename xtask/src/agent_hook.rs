use anyhow::Result;
use clap::{Args, Subcommand};

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
    /// Claude PreToolUse(Bash) guard for command misuse.
    PreBash,
    /// Claude PostToolUse(Write|Edit|MultiEdit) formatting hook.
    PostEdit,
}

pub(crate) fn run(args: &AgentHookArgs) -> Result<()> {
    let input = input::read()?;
    match args.command {
        AgentHookCommand::PreBash => pre_bash::run(&input),
        AgentHookCommand::PostEdit => post_edit::run(&input),
    }
}
