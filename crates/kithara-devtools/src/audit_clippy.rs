use std::{
    collections::BTreeMap,
    io::Cursor,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use cargo_metadata::Message;
use clap::Args;

use crate::{Ctx, common::report::print_check_block};

#[derive(Debug, Args)]
pub struct AuditClippyArgs {
    /// Optional cargo scope arguments passed before the clippy options.
    pub paths: Vec<String>,
    /// Print cargo's JSON message stream instead of the grouped report.
    #[arg(long)]
    pub raw: bool,
}

pub(crate) fn run(args: &AuditClippyArgs, ctx: &Ctx) -> Result<()> {
    let target_dir = ctx.root.join("target-audit-clippy");
    let mut cmd = Command::new("cargo");
    cmd.arg("clippy")
        .arg("--workspace")
        .arg("--message-format=json")
        .args(&args.paths)
        .arg("--");
    for lint in &ctx.config.audit_clippy.lints {
        cmd.arg("--force-warn").arg(format!("clippy::{lint}"));
    }
    cmd.env("CARGO_TARGET_DIR", target_dir);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let output = cmd.output().context("run cargo clippy audit sweep")?;
    if args.raw {
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
