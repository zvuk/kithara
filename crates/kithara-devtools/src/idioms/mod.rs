//! Idiomatic-construction fitness functions for the workspace.
//!
//! Run via `cargo xtask idioms`. Same shape as `arch` and `style`: declarative
//! rules from `.config/idioms/*.toml`, ratchet baseline at
//! `.config/idioms/baseline.toml`. The namespace flags constructions that
//! compile and pass clippy but suggest a better Rust pattern (performance,
//! readability, expressivity).

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
use config::IdiomsConfig;

use crate::common::{
    baseline::{Baseline, RatchetDiff},
    exclude::{apply_cfg_test_exclusion, apply_module_excludes, apply_path_excludes},
    project::ProjectConfig,
    report,
    scope::Scope,
    violation::Report,
};

#[derive(Debug, Default, Args)]
pub struct IdiomsArgs {
    #[arg(long)]
    pub report: Option<PathBuf>,
    #[arg(long, default_value = ".config/idioms")]
    pub config_dir: PathBuf,
    #[arg(long = "check")]
    pub check: Vec<String>,
    /// Restrict scan to specific crate(s) by name. Repeatable.
    #[arg(long = "crate", value_name = "NAME")]
    pub crates: Vec<String>,
    /// Restrict scan to workspace-relative path(s). Repeatable.
    #[arg(long = "path", value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Skip the dirty-tree gate that protects `--fix` from uncommitted edits.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Apply safe idiom collapses in place, then re-run detection.
    #[arg(long)]
    pub fix: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long = "update-baseline")]
    pub update_baseline: bool,
}

pub(crate) fn run(args: &IdiomsArgs) -> Result<()> {
    validate(args)?;

    let metadata = MetadataCommand::new().exec()?;
    let workspace_root = metadata.workspace_root.as_std_path().to_path_buf();
    let config = IdiomsConfig::load(&args.config_dir)?;
    let scope = Scope::new(args.crates.clone(), args.paths.clone());

    let ctx = Context {
        workspace_root: &workspace_root,
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
                bail!("unknown idioms check id: '{requested}'");
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

    let project = ProjectConfig::load(&workspace_root)?;
    apply_path_excludes(&mut report, &project.lint_exclude.paths);
    apply_cfg_test_exclusion(&mut report, &workspace_root);
    apply_module_excludes(&mut report, &project.lint_exclude.modules, &workspace_root);

    if args.update_baseline {
        let new_baseline = Baseline::from_report(&report);
        let path = new_baseline.save(&args.config_dir)?;
        let total: usize = new_baseline.checks.values().map(BTreeMap::len).sum();
        println!(
            "wrote idioms baseline ({} entry across {} check(s)) to {}",
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
            "idioms ratchet failed: {} regression(s), {} new violation(s)",
            diff.regressions.len(),
            diff.new_violations.len(),
        );
    }
    Ok(())
}

fn run_fix(
    registry: &[Box<dyn Check>],
    filter: &Option<HashSet<&str>>,
    ctx: &Context<'_>,
    allow_dirty: bool,
) -> Result<()> {
    crate::util::ensure_clean_tree(allow_dirty, "xtask lint idioms")?;
    let mut writes = 0;
    let mut changes = Vec::new();
    let mut skipped = Vec::new();
    for check in registry {
        if let Some(filter) = filter
            && !filter.contains(check.id())
        {
            continue;
        }
        let outcome = check.fix(ctx)?;
        writes += outcome.writes;
        changes.extend(
            outcome
                .changes
                .into_iter()
                .map(|value| format!("idioms/{}: {value}", check.id())),
        );
        skipped.extend(
            outcome
                .skipped
                .into_iter()
                .map(|value| format!("idioms/{}: {value}", check.id())),
        );
    }
    println!("idioms fix: wrote {writes} file(s)");
    for change in changes {
        println!("  changed — {change}");
    }
    for reason in skipped {
        println!("  skipped — {reason}");
    }
    Ok(())
}

fn print_report(report: &Report, ran: &[&'static str], diff: &RatchetDiff<'_>) {
    if ran.is_empty() {
        println!("OK: no idioms checks registered yet.");
        return;
    }
    if report.violations.is_empty() && diff.improvements.is_empty() {
        println!(
            "OK: {} idioms check(s) passed: {}.",
            ran.len(),
            ran.join(", ")
        );
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

fn validate(args: &IdiomsArgs) -> Result<()> {
    if args.update_baseline && (args.report.is_some() || args.json) {
        bail!("--update-baseline cannot be combined with --report or --json");
    }
    if args.json && args.report.is_some() {
        bail!("--json and --report are mutually exclusive");
    }
    Ok(())
}
