use std::{
    env,
    ffi::OsStr,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;

struct Consts;

impl Consts {
    const DESTRUCTIVE_GIT_OVERRIDE_ENV: &str = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT";
    const DESTRUCTIVE_GIT_OVERRIDE_MARKER: &str = "kithara-allow-destructive-git";
}

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

#[derive(Debug, Deserialize)]
struct HookInput {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    tool_input: HookToolInput,
}

#[derive(Debug, Default, Deserialize)]
struct HookToolInput {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    timeout: Option<Value>,
    #[serde(default)]
    file_path: Option<String>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FormatTarget {
    Rust,
    Manifest,
    Toml,
    Json,
}

impl FormatTarget {
    fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Manifest => "manifest",
            Self::Toml => "toml",
            Self::Json => "json",
        }
    }
}

pub(crate) fn run(args: &AgentHookArgs) -> Result<()> {
    let input = read_hook_input()?;
    match args.command {
        AgentHookCommand::PreBash => run_pre_bash(&input),
        AgentHookCommand::PostEdit => run_post_edit(&input),
    }
}

fn read_hook_input() -> Result<HookInput> {
    let mut body = String::new();
    io::stdin()
        .read_to_string(&mut body)
        .context("read hook JSON from stdin")?;
    if body.trim().is_empty() {
        return Ok(HookInput {
            tool_name: String::new(),
            cwd: String::new(),
            tool_input: HookToolInput::default(),
        });
    }
    serde_json::from_str(&body).context("parse hook JSON from stdin")
}

fn run_pre_bash(input: &HookInput) -> Result<()> {
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

fn run_post_edit(input: &HookInput) -> Result<()> {
    if !matches!(input.tool_name.as_str(), "Write" | "Edit" | "MultiEdit") {
        return Ok(());
    }
    let Some(path) = input.tool_input.file_path.as_deref() else {
        return Ok(());
    };
    let Some(target) = format_target_for_path(Path::new(path)) else {
        return Ok(());
    };
    let cwd = hook_working_dir(input);
    let status = Command::new("cargo")
        .current_dir(cwd)
        .args(["xtask", "format", "--only", target.name(), "--allow-dirty"])
        .status()
        .with_context(|| format!("run post-edit formatter for {}", target.name()))?;
    if !status.success() {
        bail!(
            "post-edit formatter for {} failed (exit code {:?})",
            target.name(),
            status.code()
        );
    }
    Ok(())
}

fn hook_working_dir(input: &HookInput) -> PathBuf {
    env::var_os("CLAUDE_PROJECT_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            if input.cwd.is_empty() {
                None
            } else {
                Some(PathBuf::from(&input.cwd))
            }
        })
        .unwrap_or_else(|| PathBuf::from("."))
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

fn format_target_for_path(path: &Path) -> Option<FormatTarget> {
    if path.file_name() == Some(OsStr::new("Cargo.toml")) {
        return Some(FormatTarget::Manifest);
    }
    match path.extension().and_then(OsStr::to_str) {
        Some("rs") => Some(FormatTarget::Rust),
        Some("toml") => Some(FormatTarget::Toml),
        Some("json" | "jsonc") => Some(FormatTarget::Json),
        _ => None,
    }
}

fn deny_reason_for_bash(command: &str, tool_timeout: bool) -> Option<String> {
    let tokens = shell_tokens(command);
    if tokens.is_empty() {
        return None;
    }
    let destructive_git_allowed = command.contains(Consts::DESTRUCTIVE_GIT_OVERRIDE_MARKER)
        || command.contains(&format!("{}=1", Consts::DESTRUCTIVE_GIT_OVERRIDE_ENV));
    for segment in command_segments(&tokens) {
        let stripped = strip_env_assignments(segment);
        if stripped.is_empty() {
            continue;
        }
        if tool_timeout && is_full_test_harness(stripped) {
            return Some("error: do not set a tool timeout around the full test harness; run `cargo xtask test` or `just test` without an outer timeout".to_owned());
        }
        if let Some(timeout_inner) = strip_timeout_wrapper(stripped) {
            if is_full_test_harness(timeout_inner) {
                return Some("error: do not wrap the full test harness in `timeout`; run `cargo xtask test` or `just test` directly".to_owned());
            }
            if let Some(reason) = deny_reason_for_segment(timeout_inner, destructive_git_allowed) {
                return Some(reason);
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
    if matches!(argv0.as_str(), "taplo" | "mdfmt") && !is_version_or_help(tokens) {
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

fn shell_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                ' ' | '\t' | '\n' => push_token(&mut tokens, &mut current),
                ';' => {
                    push_token(&mut tokens, &mut current);
                    tokens.push(";".to_owned());
                }
                '&' | '|' => {
                    push_token(&mut tokens, &mut current);
                    if chars.peek() == Some(&ch) {
                        chars.next();
                        tokens.push(format!("{ch}{ch}"));
                    } else {
                        tokens.push(ch.to_string());
                    }
                }
                _ => current.push(ch),
            },
        }
    }
    push_token(&mut tokens, &mut current);
    tokens
}

fn push_token(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

fn command_segments(tokens: &[String]) -> Vec<&[String]> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (idx, token) in tokens.iter().enumerate() {
        if matches!(token.as_str(), ";" | "&&" | "||" | "|") {
            if start < idx {
                segments.push(&tokens[start..idx]);
            }
            start = idx + 1;
        }
    }
    if start < tokens.len() {
        segments.push(&tokens[start..]);
    }
    segments
}

fn strip_env_assignments(tokens: &[String]) -> &[String] {
    let mut start = 0;
    while let Some(token) = tokens.get(start) {
        if is_env_assignment(token) {
            start += 1;
        } else {
            break;
        }
    }
    &tokens[start..]
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.contains('/')
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        && !name.chars().next().is_some_and(|ch| ch.is_ascii_digit())
}

fn strip_timeout_wrapper(tokens: &[String]) -> Option<&[String]> {
    if !matches!(argv0(tokens)?.as_str(), "timeout" | "gtimeout") {
        return None;
    }
    let mut idx = 1;
    while let Some(token) = tokens.get(idx) {
        if token == "--" {
            idx += 1;
            break;
        }
        if matches!(token.as_str(), "-k" | "--kill-after") {
            idx += 2;
            continue;
        }
        if token.starts_with("--kill-after=") {
            idx += 1;
            continue;
        }
        if token.starts_with('-') {
            idx += 1;
            continue;
        }
        idx += 1;
        break;
    }
    (idx < tokens.len()).then_some(&tokens[idx..])
}

fn argv0(tokens: &[String]) -> Option<String> {
    let first = tokens.first()?;
    let name = Path::new(first).file_name()?.to_str()?;
    Some(name.trim_end_matches(".exe").to_owned())
}

fn cargo_subcommand(tokens: &[String]) -> Option<(&str, &[String])> {
    if argv0(tokens)? != "cargo" {
        return None;
    }
    let mut idx = 1;
    if tokens.get(idx).is_some_and(|arg| arg.starts_with('+')) {
        idx += 1;
    }
    let subcommand = tokens.get(idx)?.as_str();
    Some((subcommand, &tokens[idx + 1..]))
}

fn is_full_test_harness(tokens: &[String]) -> bool {
    match argv0(tokens).as_deref() {
        Some("just") => tokens.get(1).is_some_and(|arg| arg.starts_with("test")),
        Some("cargo") => {
            let Some((subcommand, rest)) = cargo_subcommand(tokens) else {
                return false;
            };
            subcommand == "xtask" && rest.first().is_some_and(|arg| arg == "test")
        }
        _ => false,
    }
}

fn is_broad_cargo_test(args: &[String]) -> bool {
    if args.is_empty() {
        return true;
    }
    if has_any(
        args,
        &[
            "-p",
            "--package",
            "--test",
            "--bin",
            "--lib",
            "--manifest-path",
        ],
    ) {
        return false;
    }
    if has_test_filter(args) {
        return false;
    }
    has_any(args, &["--workspace", "--all", "--all-targets"]) || !has_scoping_flag(args)
}

fn is_broad_nextest(args: &[String]) -> bool {
    let Some((run_idx, _)) = args.iter().enumerate().find(|(_, arg)| *arg == "run") else {
        return false;
    };
    let run_args = &args[run_idx + 1..];
    !has_any(
        run_args,
        &[
            "-p",
            "--package",
            "-E",
            "--filter-expr",
            "--test",
            "--bin",
            "--manifest-path",
        ],
    )
}

fn has_scoping_flag(args: &[String]) -> bool {
    has_any(
        args,
        &[
            "-p",
            "--package",
            "--test",
            "--bin",
            "--lib",
            "--manifest-path",
        ],
    )
}

fn has_any(args: &[String], needles: &[&str]) -> bool {
    args.iter().any(|arg| {
        needles.iter().any(|needle| {
            arg == needle
                || arg
                    .strip_prefix(*needle)
                    .is_some_and(|rest| rest.starts_with('='))
        })
    })
}

fn has_test_filter(args: &[String]) -> bool {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--" {
            return true;
        }
        if flag_takes_value(arg) {
            skip_next = !arg.contains('=');
            continue;
        }
        if !arg.starts_with('-') {
            return true;
        }
    }
    false
}

fn flag_takes_value(arg: &str) -> bool {
    matches!(
        arg,
        "-p" | "--package"
            | "--test"
            | "--bin"
            | "--bench"
            | "--example"
            | "--manifest-path"
            | "--profile"
            | "--features"
            | "-Z"
            | "-j"
            | "--jobs"
    ) || arg.starts_with("--package=")
        || arg.starts_with("--test=")
        || arg.starts_with("--bin=")
        || arg.starts_with("--bench=")
        || arg.starts_with("--example=")
        || arg.starts_with("--manifest-path=")
        || arg.starts_with("--profile=")
        || arg.starts_with("--features=")
}

fn is_version_or_help(tokens: &[String]) -> bool {
    tokens
        .iter()
        .skip(1)
        .all(|arg| matches!(arg.as_str(), "--version" | "-V" | "--help" | "-h"))
}

fn is_destructive_git(tokens: &[String]) -> bool {
    match tokens.get(1).map(String::as_str) {
        Some("clean") => true,
        Some("reset") => tokens.iter().skip(2).any(|arg| arg == "--hard"),
        Some("checkout") => tokens.iter().skip(2).any(|arg| arg == "-f" || arg == "--"),
        Some("restore") => tokens.iter().skip(2).any(|arg| arg == "." || arg == ":/"),
        Some("branch") => tokens.iter().skip(2).any(|arg| arg == "-D"),
        Some("push") => tokens
            .iter()
            .skip(2)
            .any(|arg| matches!(arg.as_str(), "--force" | "-f" | "--force-with-lease")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{FormatTarget, deny_reason_for_bash, format_target_for_path, shell_tokens};

    fn denied(command: &str) -> bool {
        deny_reason_for_bash(command, false).is_some()
    }

    #[test]
    fn denies_broad_raw_test_commands() {
        assert!(denied("cargo test"));
        assert!(denied("cargo test --workspace --profile test-release"));
        assert!(denied(
            "cargo nextest run --workspace --cargo-profile test-release"
        ));
    }

    #[test]
    fn allows_scoped_raw_test_probes() {
        assert!(!denied("cargo test -p xtask agent_hook"));
        assert!(!denied(
            "cargo nextest run -p kithara-platform -E 'test(foo)'"
        ));
        assert!(!denied("cargo nextest run --workspace -E 'test(foo)'"));
    }

    #[test]
    fn denies_formatter_bypass_commands() {
        assert!(denied("rustfmt crates/foo/src/lib.rs"));
        assert!(denied("cargo sort --check --workspace"));
        assert!(denied("taplo format --check Cargo.toml"));
        assert!(denied("mdfmt --check AGENTS.md"));
    }

    #[test]
    fn allows_formatter_version_probes() {
        assert!(!denied("taplo --version"));
        assert!(!denied("mdfmt --help"));
    }

    #[test]
    fn denies_timeout_around_full_harness() {
        assert!(denied("timeout 120s just test"));
        assert!(denied(
            "timeout --kill-after=5s 120s cargo xtask test --lane workspace"
        ));
        assert!(deny_reason_for_bash("just test", true).is_some());
    }

    #[test]
    fn allows_timeout_around_scoped_probe() {
        assert!(!denied(
            "timeout 30s cargo nextest run -p xtask -E 'test(agent_hook)'"
        ));
    }

    #[test]
    fn denies_destructive_git_without_override() {
        assert!(denied("git reset --hard HEAD"));
        assert!(denied("git clean -fdx"));
        assert!(denied("git checkout -- crates/foo/src/lib.rs"));
    }

    #[test]
    fn allows_destructive_git_with_override_marker() {
        assert!(!denied(
            "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT=1 git reset --hard HEAD"
        ));
        assert!(!denied("git clean -fdx # kithara-allow-destructive-git"));
    }

    #[test]
    fn scans_later_shell_segments() {
        assert!(denied(
            "cargo xtask format --check && cargo nextest run --workspace"
        ));
    }

    #[test]
    fn tokenizes_quotes_and_separators() {
        assert_eq!(
            shell_tokens("FOO=1 cargo nextest run -E 'test(foo)' && just test"),
            [
                "FOO=1",
                "cargo",
                "nextest",
                "run",
                "-E",
                "test(foo)",
                "&&",
                "just",
                "test"
            ]
        );
    }

    #[test]
    fn maps_post_edit_format_targets() {
        assert_eq!(
            format_target_for_path(Path::new("crates/foo/src/lib.rs")),
            Some(FormatTarget::Rust)
        );
        assert_eq!(
            format_target_for_path(Path::new("crates/foo/Cargo.toml")),
            Some(FormatTarget::Manifest)
        );
        assert_eq!(
            format_target_for_path(Path::new(".config/foo.toml")),
            Some(FormatTarget::Toml)
        );
        assert_eq!(
            format_target_for_path(Path::new("tests/foo.jsonc")),
            Some(FormatTarget::Json)
        );
        assert_eq!(format_target_for_path(Path::new("README.md")), None);
    }
}
