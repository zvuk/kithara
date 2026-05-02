//! Code-style fitness functions for the workspace.
//!
//! Run via `cargo xtask style`. Reads declarative rules from `.config/style/*.toml`,
//! evaluates them against the workspace, and ratchets results against
//! `.config/style/baseline.toml`. Same shape as `arch`, but with a separate
//! baseline and config tree to keep topological and stylistic concerns split.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::PathBuf,
};

use anyhow::{Context as _, Result, bail};
use cargo_metadata::MetadataCommand;
use clap::Args;

mod checks;
mod config;

use checks::{Check, Context, registry};
use config::StyleConfig;

use crate::common::{
    baseline::{Baseline, RatchetDiff},
    report,
    scope::Scope,
    violation::Report,
};

#[derive(Debug, Default, Args)]
pub(crate) struct StyleArgs {
    /// Run only one check by id. Repeatable.
    #[arg(long = "check")]
    pub check: Vec<String>,
    /// Write a markdown report to the given path.
    #[arg(long)]
    pub report: Option<PathBuf>,
    /// Emit JSON to stdout (suppresses human output).
    #[arg(long)]
    pub json: bool,
    /// Re-write baseline.toml from current observations (does not fail on regressions).
    #[arg(long = "update-baseline")]
    pub update_baseline: bool,
    /// Apply each check's autofix in place. Refuses to run on a dirty
    /// working tree unless `--allow-dirty`. After fixes, re-runs the
    /// checks so the printed report reflects what remains.
    #[arg(long)]
    pub fix: bool,
    /// Skip the dirty-tree gate that protects `--fix` from mixing with
    /// uncommitted user edits.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Override config directory (default `.config/style`).
    #[arg(long, default_value = ".config/style")]
    pub config_dir: PathBuf,
    /// Restrict scan to specific crate(s) by name. Repeatable.
    #[arg(long = "crate", value_name = "NAME")]
    pub crates: Vec<String>,
    /// Restrict scan to workspace-relative path(s). Repeatable.
    #[arg(long = "path", value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

pub(crate) fn run(args: &StyleArgs) -> Result<()> {
    validate(args)?;

    let metadata = MetadataCommand::new().exec()?;
    let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();
    let config = StyleConfig::load(&args.config_dir)?;
    let scope = Scope::new(args.crates.clone(), args.paths.clone());

    let ctx = Context {
        workspace_root: &workspace_root,
        metadata: &metadata,
        config: &config,
        scope: &scope,
    };

    let registry = registry();
    let known_ids: HashSet<&str> = registry.iter().map(|c| c.id()).collect();

    let filter: Option<HashSet<&str>> = if args.check.is_empty() {
        None
    } else {
        for requested in &args.check {
            if !known_ids.contains(requested.as_str()) {
                bail!("unknown style check id: '{requested}'");
            }
        }
        Some(args.check.iter().map(String::as_str).collect())
    };

    if args.fix {
        run_fix(&registry, &filter, &ctx, args.allow_dirty)?;
    }

    let mut report = Report::default();
    let mut ran: Vec<&'static str> = Vec::new();
    for check in &registry {
        if let Some(filter) = &filter
            && !filter.contains(check.id())
        {
            continue;
        }
        ran.push(check.id());
        let violations = check.run(&ctx)?;
        report.extend(violations);
    }

    if args.update_baseline {
        let new_baseline = Baseline::from_report(&report);
        let path = new_baseline.save(&args.config_dir)?;
        let total: usize = new_baseline.checks.values().map(BTreeMap::len).sum();
        println!(
            "wrote style baseline ({} entry across {} check(s)) to {}",
            total,
            new_baseline.checks.len(),
            path.display(),
        );
        return Ok(());
    }

    let baseline = Baseline::load(&args.config_dir)?;
    let baseline = if scope.is_empty() {
        baseline
    } else {
        baseline.filter_keys(|k| scope.key_in_scope(k))
    };
    let diff = baseline.diff(&report.violations);

    if let Some(path) = &args.report {
        let md = report::render_markdown(&report, &ran, &diff);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create report dir: {}", parent.display()))?;
        }
        fs::write(path, md).with_context(|| format!("write report: {}", path.display()))?;
        eprintln!("wrote markdown report to {}", path.display());
    } else if args.json {
        print!("{}", report::render_json(&report, &ran, &diff));
    } else {
        print_report(&report, &ran, &diff);
    }

    if diff.has_failures() {
        bail!(
            "style ratchet failed: {} regression(s), {} new violation(s)",
            diff.regressions.len(),
            diff.new_violations.len(),
        );
    }
    Ok(())
}

fn print_report(report: &Report, ran: &[&'static str], diff: &RatchetDiff<'_>) {
    if ran.is_empty() {
        println!("OK: no style checks registered yet.");
        return;
    }
    if report.violations.is_empty() && diff.improvements.is_empty() {
        println!(
            "OK: {} style check(s) passed: {}.",
            ran.len(),
            ran.join(", ")
        );
        return;
    }

    let mut sorted = report.violations.clone();
    sorted.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .reverse()
            .then_with(|| a.check.cmp(b.check))
            .then_with(|| a.key.cmp(&b.key))
    });
    for v in &sorted {
        println!(
            "[{sev}] {check}: {key} — {msg}",
            sev = v.severity,
            check = v.check,
            key = v.key,
            msg = v.message,
        );
    }

    if !diff.improvements.is_empty() {
        println!("ratchet improvements:");
        for imp in &diff.improvements {
            println!(
                "  {check}/{key}: {from} -> {to}",
                check = imp.check,
                key = imp.key,
                from = imp.from,
                to = imp.to,
            );
        }
    }

    println!(
        "summary: {deny} deny, {warn} warn, {regr} regression(s), {new} new across {n} check(s).",
        deny = report.deny_count(),
        warn = report.warn_count(),
        regr = diff.regressions.len(),
        new = diff.new_violations.len(),
        n = ran.len(),
    );
}

fn run_fix(
    registry: &[Box<dyn Check>],
    filter: &Option<HashSet<&str>>,
    ctx: &Context<'_>,
    allow_dirty: bool,
) -> Result<()> {
    crate::util::ensure_clean_tree(allow_dirty, "xtask lint style")?;
    let mut total_writes = 0_usize;
    let mut all_skipped: Vec<String> = Vec::new();
    for check in registry {
        if let Some(filter) = filter
            && !filter.contains(check.id())
        {
            continue;
        }
        let outcome = check.fix(ctx)?;
        if outcome.writes > 0 {
            println!("style/{}: patched {} file(s)", check.id(), outcome.writes);
            total_writes += outcome.writes;
        }
        for r in outcome.skipped {
            all_skipped.push(format!("style/{}: {}", check.id(), r));
        }
    }
    println!("style fix: wrote {total_writes} file(s)");
    for s in &all_skipped {
        println!("  skipped — {s}");
    }
    Ok(())
}

fn validate(args: &StyleArgs) -> Result<()> {
    if args.update_baseline && (args.report.is_some() || args.json) {
        bail!("--update-baseline cannot be combined with --report or --json");
    }
    if args.json && args.report.is_some() {
        bail!("--json and --report are mutually exclusive");
    }
    Ok(())
}
