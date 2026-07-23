//! Architectural fitness functions for the workspace.
//!
//! Run via `cargo xtask arch`. Reads declarative rules from `.config/arch/*.toml`,
//! evaluates them against the workspace, and ratchets results against
//! `.config/arch/baseline.toml`.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use cargo_metadata::{Metadata, MetadataCommand};
use clap::Args;

mod checks;
mod config;

use checks::{Context, registry};
use config::ArchConfig;

use crate::common::{
    baseline::{Baseline, RatchetDiff},
    exclude::{apply_cfg_test_exclusion, apply_module_excludes, apply_path_excludes},
    project::ProjectConfig,
    report,
    scope::Scope,
    violation::Report,
};

pub(crate) fn redundant_accessor_keys(
    metadata: &Metadata,
    workspace_root: &Path,
    scope: &Scope,
) -> Result<BTreeSet<String>> {
    let config = ArchConfig::load(&workspace_root.join(".config/arch"))?;
    let ctx = Context {
        config: &config,
        metadata,
        workspace_root,
        scope,
    };
    let violations = checks::Check::run(&checks::redundant_accessors::RedundantAccessors, &ctx)?;
    Ok(violations
        .into_iter()
        .map(|violation| violation.key)
        .collect())
}

#[derive(Debug, Default, Args)]
pub struct ArchArgs {
    /// Write a markdown report to the given path.
    #[arg(long)]
    pub report: Option<PathBuf>,
    /// Override config directory (default `.config/arch`).
    #[arg(long, default_value = ".config/arch")]
    pub config_dir: PathBuf,
    /// Run only one check by id (e.g. `direction`, `canonical_types`). Repeatable.
    #[arg(long = "check")]
    pub check: Vec<String>,
    /// Restrict scan to specific crate(s) by name. Repeatable.
    #[arg(long = "crate", value_name = "NAME")]
    pub crates: Vec<String>,
    /// Restrict scan to workspace-relative path(s). Repeatable.
    #[arg(long = "path", value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// With `--fix`, write changes to disk (otherwise only report the plan).
    #[arg(long)]
    pub apply: bool,
    /// Run each selected check's autofix instead of reporting. Dry run unless
    /// `--apply` is also passed.
    #[arg(long)]
    pub fix: bool,
    /// Emit JSON to stdout (suppresses human output).
    #[arg(long)]
    pub json: bool,
    /// Re-write baseline.toml from current observations (does not fail on regressions).
    #[arg(long = "update-baseline")]
    pub update_baseline: bool,
}

pub(crate) fn run(args: &ArchArgs) -> Result<()> {
    validate(args)?;

    let metadata = MetadataCommand::new().exec()?;
    let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();
    let config = ArchConfig::load(&args.config_dir)?;
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
                bail!("unknown check id: '{requested}'");
            }
        }
        Some(args.check.iter().map(String::as_str).collect())
    };

    if args.fix {
        let mut outcome = crate::common::fix::FixOutcome::default();
        for check in &registry {
            if let Some(filter) = &filter
                && !filter.contains(check.id())
            {
                continue;
            }
            let o = check.fix(&ctx, args.apply)?;
            outcome.writes += o.writes;
            outcome.skipped.extend(o.skipped);
            outcome.changes.extend(o.changes);
        }
        outcome.changes.sort();
        for c in &outcome.changes {
            println!("  - {c}");
        }
        let verb = if args.apply { "wrote" } else { "would change" };
        println!("{verb} {} file(s)", outcome.writes);
        for s in &outcome.skipped {
            println!("  skipped: {s}");
        }
        if !args.apply {
            println!("(dry run — pass --apply to write)");
        }
        return Ok(());
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

    let project = ProjectConfig::load(&workspace_root)?;
    apply_path_excludes(&mut report, &project.lint_exclude.paths);
    apply_cfg_test_exclusion(&mut report, &workspace_root);
    apply_module_excludes(&mut report, &project.lint_exclude.modules, &workspace_root);

    if args.update_baseline {
        let new_baseline = Baseline::from_report(&report);
        let path = new_baseline.save(&args.config_dir)?;
        let total: usize = new_baseline.checks.values().map(BTreeMap::len).sum();
        println!(
            "wrote baseline ({} entry across {} check(s)) to {}",
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
        report::print_ratchet_failures(&diff);
        bail!(
            "architectural ratchet failed: {} regression(s), {} new violation(s)",
            diff.regressions.len(),
            diff.new_violations.len(),
        );
    }
    Ok(())
}

fn print_report(report: &Report, ran: &[&'static str], diff: &RatchetDiff<'_>) {
    if report.violations.is_empty() && diff.improvements.is_empty() {
        println!("OK: {} check(s) passed: {}.", ran.len(), ran.join(", "));
        return;
    }
    report::print_grouped(report, diff);
    println!(
        "summary: {deny} deny, {warn} warn, {regr} regression(s), {new} new across {n} check(s).",
        deny = report.deny_count(),
        warn = report.warn_count(),
        regr = diff.regressions.len(),
        new = diff.new_violations.len(),
        n = ran.len(),
    );
}

fn validate(args: &ArchArgs) -> Result<()> {
    if args.update_baseline && (args.report.is_some() || args.json) {
        bail!("--update-baseline cannot be combined with --report or --json");
    }
    if args.json && args.report.is_some() {
        bail!("--json and --report are mutually exclusive");
    }
    Ok(())
}
