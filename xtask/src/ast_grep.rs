//! `cargo xtask ast-grep` — wrapper around the `ast-grep` CLI.
//!
//! Discovers every rule under `.config/ast-grep/` and lets each rule's
//! own `severity:` field decide whether a hit blocks (`error`) or just
//! reports (`warning`/`info`/`hint`). No per-id filter list lives in
//! Rust — adding a new rule file is enough.
//!
//! Default mode parses ast-grep's JSON output and groups hits by `ruleId`,
//! so each rule's description is printed once even when the rule fires
//! many times. Pass `--raw` to fall through to ast-grep's native short
//! formatter (one repeated paragraph per hit).
//!
//! Pass `--strict` to promote every warning to an error (used by
//! exhaustive sweeps, not by the default audit).
//!
//! `--fix` (implies `--update-all`) cannot be combined with `--json`
//! per ast-grep — in that mode we transparently fall back to the native
//! output regardless of `--raw`.

use std::{
    collections::BTreeMap,
    process::{Command, Stdio},
};

use anyhow::{Result, bail};
use clap::Args;
use serde::Deserialize;

use crate::util::ensure_clean_tree;

#[derive(Debug, Args)]
pub(crate) struct AstGrepArgs {
    /// Promote every warning to an error (passes `--warning` to ast-grep).
    /// Use for exhaustive sweeps when warning-level rules should also fail.
    #[arg(long)]
    pub strict: bool,
    /// Apply rule fixes by passing `--update-all` to ast-grep. Only rules
    /// that declare a `fix:` block in `.config/ast-grep/*.yml` actually
    /// rewrite anything; rules without one stay reporting-only.
    /// Refuses to run on a dirty working tree unless `--allow-dirty`.
    #[arg(long)]
    pub fix: bool,
    /// Skip the dirty-tree gate that protects `--fix` from mixing with
    /// uncommitted user edits. Mirrors `cargo fmt`/`cargo fix` UX.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Bypass the grouped renderer and stream ast-grep's native short
    /// output verbatim. Useful when you need the upstream formatting.
    #[arg(long)]
    pub raw: bool,
    /// Optional paths to scan. Empty = whole workspace.
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Match {
    #[serde(rename = "ruleId")]
    rule_id: String,
    severity: String,
    file: String,
    range: Range,
    message: String,
}

#[derive(Debug, Deserialize)]
struct Range {
    start: Position,
}

#[derive(Debug, Deserialize)]
struct Position {
    line: u32,
    column: u32,
}

pub(crate) fn run(args: &AstGrepArgs) -> Result<()> {
    if args.fix {
        ensure_clean_tree(args.allow_dirty, "ast-grep")?;
        return run_native(args);
    }
    if args.raw {
        return run_native(args);
    }
    run_grouped(args)
}

fn run_native(args: &AstGrepArgs) -> Result<()> {
    let mut cmd = Command::new("ast-grep");
    cmd.arg("scan")
        .arg("--config")
        .arg("sgconfig.yml")
        .arg("--report-style")
        .arg("short");
    if args.strict {
        cmd.arg("--warning");
    }
    if args.fix {
        cmd.arg("--update-all");
    }
    for p in &args.paths {
        cmd.arg(p);
    }
    let status = cmd.status()?;
    if !status.success() {
        bail!("ast-grep failed (exit code {:?})", status.code());
    }
    Ok(())
}

fn run_grouped(args: &AstGrepArgs) -> Result<()> {
    let mut cmd = Command::new("ast-grep");
    cmd.arg("scan")
        .arg("--config")
        .arg("sgconfig.yml")
        .arg("--json=stream");
    if args.strict {
        cmd.arg("--warning");
    }
    for p in &args.paths {
        cmd.arg(p);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let output = cmd.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut by_rule: BTreeMap<String, RuleGroup> = BTreeMap::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let m: Match = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let entry = by_rule
            .entry(m.rule_id.clone())
            .or_insert_with(|| RuleGroup {
                severity: m.severity.clone(),
                message: m.message.clone(),
                hits: Vec::new(),
            });
        entry.hits.push(Hit {
            file: m.file,
            line: m.range.start.line,
            column: m.range.start.column,
        });
    }

    print_grouped(&by_rule);

    if !output.status.success() {
        bail!("ast-grep failed (exit code {:?})", output.status.code());
    }
    Ok(())
}

struct RuleGroup {
    severity: String,
    message: String,
    hits: Vec<Hit>,
}

struct Hit {
    file: String,
    line: u32,
    column: u32,
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "error" => 0,
        "warning" => 1,
        "info" => 2,
        "hint" => 3,
        _ => 4,
    }
}

fn print_grouped(groups: &BTreeMap<String, RuleGroup>) {
    if groups.is_empty() {
        return;
    }
    let mut rules: Vec<(&String, &RuleGroup)> = groups.iter().collect();
    rules.sort_by(|a, b| {
        severity_rank(&a.1.severity)
            .cmp(&severity_rank(&b.1.severity))
            .then_with(|| b.1.hits.len().cmp(&a.1.hits.len()))
            .then_with(|| a.0.cmp(b.0))
    });

    let total: usize = rules.iter().map(|(_, g)| g.hits.len()).sum();

    for (i, (rule_id, group)) in rules.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let summary = format!("×{} {}", group.hits.len(), group.severity);
        crate::common::report::print_check_block(
            rule_id,
            &group.severity,
            &summary,
            Some(group.message.trim()),
            group.hits.iter().map(|h| {
                let location = format!("{}:{}:{}", h.file, h.line, h.column);
                (location, None)
            }),
        );
    }

    println!();
    println!(
        "ast-grep: {total} hit{plural} in {rules_n} rule{rules_plural}",
        plural = if total == 1 { "" } else { "s" },
        rules_n = rules.len(),
        rules_plural = if rules.len() == 1 { "" } else { "s" },
    );
}
