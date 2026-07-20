use anyhow::Result;

use crate::{input, post_edit, pre_bash};

/// Hook command implemented by the repo-owned agent policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum HookCommand {
    /// Guards Claude PreToolUse(Bash) commands.
    PreBash,
    /// Formats a file reported by Claude PostToolUse(Write|Edit|MultiEdit).
    PostEdit,
}

/// Runs one agent hook command against the JSON payload on standard input.
///
/// # Errors
///
/// Returns an error when the input is invalid or the selected hook action fails.
pub fn run(command: HookCommand) -> Result<()> {
    let input = input::read()?;
    match command {
        HookCommand::PreBash => pre_bash::run(&input),
        HookCommand::PostEdit => post_edit::run(&input),
    }
}
