mod cargo;
mod git;
mod shell;
#[cfg(test)]
mod tests;

use std::io::{self, Write};

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

pub(super) fn run(
    event_name: &str,
    command: &str,
    tool_timeout: bool,
    destructive_git_override_env: &str,
) -> Result<()> {
    if let Some(reason) = deny_reason_for_bash(command, tool_timeout, destructive_git_override_env)
    {
        print_deny(event_name, &reason)?;
    }
    Ok(())
}

fn print_deny(event_name: &str, reason: &str) -> Result<()> {
    let out = HookOutput {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: event_name,
            permission_decision: "deny",
            permission_decision_reason: reason,
        },
    };
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    serde_json::to_writer(&mut writer, &out).context("serialize hook deny output")?;
    writeln!(writer).context("write hook deny output")?;
    Ok(())
}

fn deny_reason_for_bash(
    command: &str,
    tool_timeout: bool,
    destructive_git_override_env: &str,
) -> Option<String> {
    let tokens = shell_tokens(command);
    if tokens.is_empty() {
        return None;
    }
    deny_reason_for_tokens(&tokens, tool_timeout, false, destructive_git_override_env)
}

fn deny_reason_for_tokens(
    tokens: &[String],
    tool_timeout: bool,
    inherited_git_override: bool,
    destructive_git_override_env: &str,
) -> Option<String> {
    for segment in command_segments(tokens) {
        let destructive_git_allowed = inherited_git_override
            || segment
                .iter()
                .any(|token| is_override_assignment(token, destructive_git_override_env));
        let stripped = match normalize_command(segment) {
            NormalizedCommand::Args(stripped) => stripped,
            NormalizedCommand::Shell(command) => {
                let nested = shell_tokens(&command);
                if let Some(reason) = deny_reason_for_tokens(
                    &nested,
                    tool_timeout,
                    destructive_git_allowed,
                    destructive_git_override_env,
                ) {
                    return Some(reason);
                }
                continue;
            }
        };
        let stripped = stripped.as_slice();
        if tool_timeout && is_full_test_harness(stripped) {
            return Some("error: do not set a tool timeout around the full test harness; run `just test` without an outer timeout".to_owned());
        }
        if let Some(timeout_inner) = strip_timeout_wrapper(stripped) {
            match normalize_command(timeout_inner) {
                NormalizedCommand::Args(timeout_inner) => {
                    if is_full_test_harness(&timeout_inner) {
                        return Some("error: do not wrap the full test harness in `timeout`; run `just test` directly".to_owned());
                    }
                    if let Some(reason) = deny_reason_for_segment(
                        &timeout_inner,
                        destructive_git_allowed,
                        destructive_git_override_env,
                    ) {
                        return Some(reason);
                    }
                }
                NormalizedCommand::Shell(command) => {
                    let nested = shell_tokens(&command);
                    if let Some(reason) = deny_reason_for_tokens(
                        &nested,
                        true,
                        destructive_git_allowed,
                        destructive_git_override_env,
                    ) {
                        return Some(reason);
                    }
                }
            }
        }
        if let Some(reason) = deny_reason_for_segment(
            stripped,
            destructive_git_allowed,
            destructive_git_override_env,
        ) {
            return Some(reason);
        }
    }
    None
}

fn deny_reason_for_segment(
    tokens: &[String],
    destructive_git_allowed: bool,
    destructive_git_override_env: &str,
) -> Option<String> {
    let argv0 = argv0(tokens)?;
    if argv0 == "rustfmt" {
        return Some(
            "error: do not run `rustfmt` directly; use `just xtask format --only rust`".to_owned(),
        );
    }
    if matches!(argv0, "taplo" | "mdfmt") && !is_version_or_help(tokens) {
        return Some(format!(
            "error: do not run `{argv0}` directly as a formatting gate; use `just fmt`"
        ));
    }
    if let Some((subcommand, rest)) = cargo_subcommand(tokens) {
        match subcommand {
            "test" if is_broad_cargo_test(rest) => {
                return Some(
                    "error: broad raw `cargo test` is not an acceptance gate; use `just test`"
                        .to_owned(),
                );
            }
            "nextest" if is_broad_nextest(rest) => {
                return Some("error: broad raw `cargo nextest run` is not an acceptance gate; use `just test`".to_owned());
            }
            "sort" if rest.iter().any(|arg| arg == "--check") => {
                return Some("error: do not run `cargo sort --check`; use `just manifest dependency-order` or `just fmt-check`".to_owned());
            }
            _ => {}
        }
    }
    if argv0 == "git" && !destructive_git_allowed && is_destructive_git(tokens) {
        return Some(format!(
            "error: destructive git command blocked; add `{}=1` only after explicit user approval",
            destructive_git_override_env
        ));
    }
    None
}

fn is_override_assignment(token: &str, name: &str) -> bool {
    token.strip_prefix(name) == Some("=1")
}

fn is_version_or_help(tokens: &[String]) -> bool {
    tokens
        .iter()
        .skip(1)
        .all(|arg| matches!(arg.as_str(), "--version" | "-V" | "--help" | "-h"))
}
