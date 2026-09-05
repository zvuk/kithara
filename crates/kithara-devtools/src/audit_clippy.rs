use std::{
    collections::BTreeMap,
    io::Cursor,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::Message;
use clap::Args;

use crate::{Ctx, common::report::print_check_block, util::ensure_clean_tree};

#[derive(Debug, Args)]
pub struct AuditClippyArgs {
    /// Optional cargo scope arguments passed before the clippy options.
    #[arg(
        value_name = "SCOPE",
        num_args = 0..,
        allow_hyphen_values = true,
        trailing_var_arg = true
    )]
    pub paths: Vec<String>,
    /// Skip the dirty-tree guard before applying fixes.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Apply every machine-applicable suggestion emitted by the configured
    /// advisory lints, then rerun the grouped report over the same scope.
    #[arg(long)]
    pub fix: bool,
    /// Print cargo's JSON message stream instead of the grouped report.
    #[arg(long)]
    pub raw: bool,
}

pub(crate) fn run(args: &AuditClippyArgs, ctx: &Ctx) -> Result<()> {
    if args.fix {
        ensure_clean_tree(args.allow_dirty, "audit-clippy")?;
    }
    let fix = args.fix.then(|| clippy_command(args, ctx, true));
    let report = clippy_command(args, ctx, false);
    run_clippy_passes(fix, report, args.raw)
}

fn run_clippy_passes(mut fix: Option<Command>, mut report: Command, raw: bool) -> Result<()> {
    if let Some(cmd) = fix.as_mut() {
        let status = cmd.status().context("run cargo clippy audit fix")?;
        if !status.success() {
            bail!(
                "cargo clippy audit fix failed (exit code {:?})",
                status.code()
            );
        }
    }

    report.stdout(Stdio::piped());
    report.stderr(Stdio::inherit());

    let output = report.output().context("run cargo clippy audit sweep")?;
    if raw {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    } else {
        let groups = parse_diagnostics(&output.stdout)?;
        print_grouped(&groups);
    }
    if !output.status.success() {
        bail!(
            "cargo clippy audit sweep failed (exit code {:?})",
            output.status.code()
        );
    }
    Ok(())
}

fn clippy_command(args: &AuditClippyArgs, ctx: &Ctx, fix: bool) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg("clippy");
    if fix {
        cmd.arg("--fix");
    }
    if !has_explicit_package_selector(&args.paths) {
        cmd.arg("--workspace");
    }
    if !fix {
        cmd.arg("--message-format=json");
    }
    cmd.arg("--all-targets");
    if fix && args.allow_dirty {
        cmd.arg("--allow-dirty").arg("--allow-staged");
    }
    cmd.args(&args.paths).arg("--");
    for lint in &ctx.config.audit_clippy.lints {
        cmd.arg("--force-warn").arg(format!("clippy::{lint}"));
    }
    cmd.env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_INCREMENTAL")
        .env("CARGO_TARGET_DIR", ctx.root.join("target-audit-clippy"));
    cmd
}

fn has_explicit_package_selector(args: &[String]) -> bool {
    args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "-p" | "--package" | "--manifest-path" | "--workspace" | "--all"
        ) || arg
            .strip_prefix("-p")
            .is_some_and(|value| !value.is_empty())
            || arg.starts_with("--package=")
            || arg.starts_with("--manifest-path=")
    })
}

fn parse_diagnostics(stdout: &[u8]) -> Result<BTreeMap<String, RuleGroup>> {
    let mut groups = BTreeMap::new();
    for message in Message::parse_stream(Cursor::new(stdout)) {
        match message.context("read cargo clippy JSON stream")? {
            Message::CompilerMessage(message) => {
                let diagnostic = message.message;
                let Some(code) = diagnostic.code.map(|code| code.code) else {
                    continue;
                };
                if !code.starts_with("clippy::") {
                    continue;
                }
                let group = groups.entry(code).or_insert_with(|| RuleGroup {
                    message: diagnostic.message.clone(),
                    hits: Vec::new(),
                });
                let span = diagnostic
                    .spans
                    .iter()
                    .find(|span| span.is_primary)
                    .or_else(|| diagnostic.spans.first());
                let location = span.map_or_else(
                    || "workspace".to_owned(),
                    |span| {
                        format!(
                            "{}:{}:{}",
                            span.file_name, span.line_start, span.column_start
                        )
                    },
                );
                group.hits.push(location);
            }
            Message::TextLine(line) if !line.trim().is_empty() => {
                bail!("non-JSON output from cargo clippy: {line}");
            }
            _ => {}
        }
    }
    Ok(groups)
}

struct RuleGroup {
    message: String,
    hits: Vec<String>,
}

fn print_grouped(groups: &BTreeMap<String, RuleGroup>) {
    println!("Advisory clippy audit sweep (non-gating; findings do not fail the command)");

    let mut rules: Vec<_> = groups.iter().collect();
    rules.sort_by(|a, b| {
        b.1.hits
            .len()
            .cmp(&a.1.hits.len())
            .then_with(|| a.0.cmp(b.0))
    });
    let total: usize = rules.iter().map(|(_, group)| group.hits.len()).sum();

    for (index, (rule, group)) in rules.iter().enumerate() {
        if index > 0 {
            println!();
        }
        print_check_block(
            rule,
            "warning",
            &format!("x{} advisory", group.hits.len()),
            Some(group.message.trim()),
            group.hits.iter().map(|location| (location, None)),
        );
    }

    println!();
    println!(
        "audit-clippy: {total} advisory finding(s) across {} lint(s)",
        rules.len()
    );
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, fs, path::PathBuf, process::Command};

    use tempfile::tempdir;

    use super::*;
    use crate::common::project::ProjectConfig;

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn command_env(command: &Command, key: &str) -> Option<Option<String>> {
        command.get_envs().find_map(|(name, value)| {
            (name == OsStr::new(key))
                .then(|| value.map(|value| value.to_string_lossy().into_owned()))
        })
    }

    fn context() -> Ctx {
        let mut config = ProjectConfig::default();
        config.audit_clippy.lints = vec!["redundant_clone".to_owned()];
        Ctx::new(PathBuf::from("/workspace"), config)
    }

    #[test]
    fn fix_and_report_commands_share_explicit_scope() {
        let args = AuditClippyArgs {
            paths: vec!["-p".to_owned(), "kithara-ui".to_owned()],
            fix: true,
            allow_dirty: true,
            raw: false,
        };
        let ctx = context();

        let fix = clippy_command(&args, &ctx, true);
        let report = clippy_command(&args, &ctx, false);

        assert_eq!(
            command_args(&fix),
            [
                "clippy",
                "--fix",
                "--all-targets",
                "--allow-dirty",
                "--allow-staged",
                "-p",
                "kithara-ui",
                "--",
                "--force-warn",
                "clippy::redundant_clone",
            ]
        );
        assert_eq!(
            command_args(&report),
            [
                "clippy",
                "--message-format=json",
                "--all-targets",
                "-p",
                "kithara-ui",
                "--",
                "--force-warn",
                "clippy::redundant_clone",
            ]
        );
    }

    #[test]
    fn report_without_scope_keeps_workspace_selection() {
        let args = AuditClippyArgs {
            paths: Vec::new(),
            fix: false,
            allow_dirty: true,
            raw: false,
        };

        let report = clippy_command(&args, &context(), false);

        assert_eq!(
            command_args(&report),
            [
                "clippy",
                "--workspace",
                "--message-format=json",
                "--all-targets",
                "--",
                "--force-warn",
                "clippy::redundant_clone",
            ]
        );
    }

    #[test]
    fn clippy_passes_disable_sccache_and_keep_incremental_builds() {
        let args = AuditClippyArgs {
            paths: Vec::new(),
            fix: true,
            allow_dirty: false,
            raw: false,
        };
        let ctx = context();

        for command in [
            clippy_command(&args, &ctx, true),
            clippy_command(&args, &ctx, false),
        ] {
            assert_eq!(command_env(&command, "RUSTC_WRAPPER"), Some(None));
            assert_eq!(command_env(&command, "CARGO_INCREMENTAL"), Some(None));
        }
    }

    #[test]
    fn implicit_workspace_is_removed_only_by_explicit_package_selectors() {
        let cases: &[(&[&str], bool)] = &[
            (&["--all-features"], true),
            (&["--exclude", "kithara-ui"], true),
            (&["--all-targets"], true),
            (&["-p", "kithara-ui"], false),
            (&["-pkithara-ui"], false),
            (&["--package", "kithara-ui"], false),
            (&["--package=kithara-ui"], false),
            (&["--manifest-path", "crates/kithara-ui/Cargo.toml"], false),
            (&["--manifest-path=crates/kithara-ui/Cargo.toml"], false),
            (&["--workspace"], false),
            (&["--all"], false),
        ];

        for (paths, uses_implicit_workspace) in cases {
            let args = AuditClippyArgs {
                paths: paths.iter().map(ToString::to_string).collect(),
                fix: false,
                allow_dirty: false,
                raw: false,
            };

            let command = command_args(&clippy_command(&args, &context(), false));

            assert_eq!(
                command.get(1).map(String::as_str) == Some("--workspace"),
                *uses_implicit_workspace,
                "scope arguments: {paths:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn successful_fix_is_followed_by_report() {
        let temp = tempdir().expect("tempdir");
        let log = temp.path().join("passes.log");
        let mut fix = Command::new("sh");
        fix.args(["-c", "printf 'fix\\n' >> \"$1\"", "sh"])
            .arg(&log);
        let mut report = Command::new("sh");
        report
            .args(["-c", "printf 'report\\n' >> \"$1\"", "sh"])
            .arg(&log);

        run_clippy_passes(Some(fix), report, true).expect("fix and report");

        assert_eq!(fs::read_to_string(log).expect("pass log"), "fix\nreport\n");
    }

    #[cfg(unix)]
    #[test]
    fn failed_fix_stops_before_report() {
        let temp = tempdir().expect("tempdir");
        let log = temp.path().join("passes.log");
        let mut fix = Command::new("sh");
        fix.args(["-c", "printf 'fix\\n' >> \"$1\"; exit 7", "sh"])
            .arg(&log);
        let mut report = Command::new("sh");
        report
            .args(["-c", "printf 'report\\n' >> \"$1\"", "sh"])
            .arg(&log);

        let error = run_clippy_passes(Some(fix), report, false).expect_err("fix failure");

        assert!(format!("{error:#}").contains("cargo clippy audit fix failed (exit code Some(7))"));
        assert_eq!(fs::read_to_string(log).expect("pass log"), "fix\n");
    }
}
