mod cargo;
mod git;
mod shell;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use serde::Serialize;

use self::{
    cargo::{cargo_subcommand, is_broad_cargo_test, is_broad_nextest, is_full_test_harness},
    git::is_destructive_git,
    shell::{
        NormalizedCommand, argv0, command_segments, normalize_command, shell_tokens,
        strip_timeout_wrapper,
    },
};
use crate::input::HookInput;

struct Consts;

impl Consts {
    const DESTRUCTIVE_GIT_OVERRIDE_ASSIGNMENT: &str = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT=1";
    const DESTRUCTIVE_GIT_OVERRIDE_ENV: &str = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT";
}

#[derive(Serialize)]
struct HookOutput<'a> {
    #[serde(rename = "hookSpecificOutput")]
    hook_specific_output: HookSpecificOutput<'a>,
}

#[derive(Serialize)]
struct HookSpecificOutput<'a> {
    #[serde(rename = "hookEventName")]
    hook_event_name: &'a str,
    #[serde(rename = "permissionDecision")]
    permission_decision: &'a str,
    #[serde(rename = "permissionDecisionReason")]
    permission_decision_reason: &'a str,
}

pub(crate) fn run(input: &HookInput) -> Result<()> {
    if input.tool_name != "Bash" {
        return Ok(());
    }
    let Some(command) = input.tool_input.command.as_deref() else {
        return Ok(());
    };
    if let Some(reason) = deny_reason_for_bash(command, input.tool_input.timeout.is_some()) {
        print_deny("PreToolUse", &reason)?;
    }
    Ok(())
}

fn print_deny(event_name: &'static str, reason: &str) -> Result<()> {
    let out = HookOutput {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: event_name,
            permission_decision: "deny",
            permission_decision_reason: reason,
        },
    };
    println!(
        "{}",
        serde_json::to_string(&out).context("serialize hook deny output")?
    );
    Ok(())
}

fn deny_reason_for_bash(command: &str, tool_timeout: bool) -> Option<String> {
    let tokens = shell_tokens(command);
    if tokens.is_empty() {
        return None;
    }
    deny_reason_for_tokens(&tokens, tool_timeout, false)
}

fn deny_reason_for_tokens(
    tokens: &[String],
    tool_timeout: bool,
    inherited_git_override: bool,
) -> Option<String> {
    for segment in command_segments(tokens) {
        let destructive_git_allowed = inherited_git_override
            || segment
                .iter()
                .any(|token| token == Consts::DESTRUCTIVE_GIT_OVERRIDE_ASSIGNMENT);
        let stripped = match normalize_command(segment) {
            NormalizedCommand::Args(stripped) => stripped,
            NormalizedCommand::Shell(command) => {
                let nested = shell_tokens(&command);
                if let Some(reason) =
                    deny_reason_for_tokens(&nested, tool_timeout, destructive_git_allowed)
                {
                    return Some(reason);
                }
                continue;
            }
        };
        let stripped = stripped.as_slice();
        if tool_timeout && is_full_test_harness(stripped) {
            return Some("error: do not set a tool timeout around the full test harness; run `cargo xtask test` or `just test` without an outer timeout".to_owned());
        }
        if let Some(timeout_inner) = strip_timeout_wrapper(stripped) {
            match normalize_command(timeout_inner) {
                NormalizedCommand::Args(timeout_inner) => {
                    if is_full_test_harness(&timeout_inner) {
                        return Some("error: do not wrap the full test harness in `timeout`; run `cargo xtask test` or `just test` directly".to_owned());
                    }
                    if let Some(reason) =
                        deny_reason_for_segment(&timeout_inner, destructive_git_allowed)
                    {
                        return Some(reason);
                    }
                }
                NormalizedCommand::Shell(command) => {
                    let nested = shell_tokens(&command);
                    if let Some(reason) =
                        deny_reason_for_tokens(&nested, true, destructive_git_allowed)
                    {
                        return Some(reason);
                    }
                }
            }
        }
        if let Some(reason) = deny_reason_for_segment(stripped, destructive_git_allowed) {
            return Some(reason);
        }
    }
    None
}

fn deny_reason_for_segment(tokens: &[String], destructive_git_allowed: bool) -> Option<String> {
    let argv0 = argv0(tokens)?;
    if argv0 == "rustfmt" {
        return Some(
            "error: do not run `rustfmt` directly; use `cargo xtask format --only rust`".to_owned(),
        );
    }
    if matches!(argv0, "taplo" | "mdfmt") && !is_version_or_help(tokens) {
        return Some(format!(
            "error: do not run `{argv0}` directly as a formatting gate; use `cargo xtask format`"
        ));
    }
    if let Some((subcommand, rest)) = cargo_subcommand(tokens) {
        match subcommand {
            "test" if is_broad_cargo_test(rest) => {
                return Some("error: broad raw `cargo test` is not an acceptance gate; use `cargo xtask test` or `just test`".to_owned());
            }
            "nextest" if is_broad_nextest(rest) => {
                return Some("error: broad raw `cargo nextest run` is not an acceptance gate; use `cargo xtask test` or `just test`".to_owned());
            }
            "sort" if rest.iter().any(|arg| arg == "--check") => {
                return Some("error: do not run `cargo sort --check`; use `cargo xtask manifest dependency-order` or `cargo xtask format --check`".to_owned());
            }
            _ => {}
        }
    }
    if argv0 == "git" && !destructive_git_allowed && is_destructive_git(tokens) {
        return Some(format!(
            "error: destructive git command blocked; add `{}=1` only after explicit user approval",
            Consts::DESTRUCTIVE_GIT_OVERRIDE_ENV
        ));
    }
    None
}

fn is_version_or_help(tokens: &[String]) -> bool {
    tokens
        .iter()
        .skip(1)
        .all(|arg| matches!(arg.as_str(), "--version" | "-V" | "--help" | "-h"))
}
