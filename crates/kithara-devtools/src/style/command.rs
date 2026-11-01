use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
#[cfg(test)]
use cargo_metadata::MetadataCommand;
use clap::Args;
use rayon::prelude::*;

use super::{
    checks::{Check, Context, registry},
    config::StyleConfig,
};
use crate::{
    Ctx,
    common::{
        baseline::{Baseline, RatchetDiff},
        exclude::{apply_lint_excludes, cfg_test_module_globs},
        project::ProjectConfig,
        report,
        scan::Scan,
        scope::Scope,
        violation::{Report, Violation},
    },
};

#[derive(Debug, Default, Args)]
pub struct StyleArgs {
    /// Write a markdown report to the given path.
    #[arg(long)]
    pub report: Option<PathBuf>,
    /// Override config directory (default `.config/style`).
    #[arg(long, default_value = ".config/style")]
    pub config_dir: PathBuf,
    /// Run only one check by id. Repeatable.
    #[arg(long = "check")]
    pub check: Vec<String>,
    /// Restrict scan to specific crate(s) by name. Repeatable.
    #[arg(long = "crate", value_name = "NAME")]
    pub crates: Vec<String>,
    /// Restrict scan to workspace-relative path(s). Repeatable.
    #[arg(long = "path", value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Skip the dirty-tree gate that protects `--fix` from mixing with
    /// uncommitted user edits.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Apply each check's autofix in place. Refuses to run on a dirty
    /// working tree unless `--allow-dirty`. After fixes, re-runs the
    /// checks so the printed report reflects what remains.
    #[arg(long)]
    pub fix: bool,
    /// Emit JSON to stdout (suppresses human output).
    #[arg(long)]
    pub json: bool,
    /// Re-write baseline.toml from current observations (does not fail on regressions).
    /// Print each check's wall time to stderr, slowest first.
    #[arg(long)]
    pub timings: bool,
    #[arg(long = "update-baseline")]
    pub update_baseline: bool,
}

pub(crate) fn run(args: &StyleArgs, workspace: &Ctx) -> Result<()> {
    validate(args)?;

    let metadata = workspace.metadata()?;
    let workspace_root = workspace.root.clone();
    let config_dir = workspace_root.join(&args.config_dir);
    let config = StyleConfig::load(&config_dir)?;
    let scope = Scope::new(args.crates.clone(), args.paths.clone()).with_metadata(metadata);

    let fix_scan = Scan::new(&workspace_root);
    let ctx = Context {
        workspace_root: &workspace_root,
        metadata,
        config: &config,
        scope: &scope,
        scan: &fix_scan,
    };
    let project = &workspace.config;

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

    let scan = Scan::new(&workspace_root);
    let ctx = Context { scan: &scan, ..ctx };

    let selected: Vec<&dyn Check> = registry
        .iter()
        .map(Box::as_ref)
        .filter(|check| filter.as_ref().is_none_or(|ids| ids.contains(check.id())))
        .collect();
    let ran: Vec<&'static str> = selected.iter().map(|check| check.id()).collect();

    let outcomes = run_checks(&selected, &ctx, project, &workspace_root)?;

    if args.timings {
        let rows: Vec<_> = ran
            .iter()
            .copied()
            .zip(outcomes.iter().map(|(elapsed, _)| *elapsed))
            .collect();
        report::print_timings("style", &rows);
    }

    let mut report = Report::default();
    for (_, violations) in outcomes {
        report.extend(violations);
    }

    if args.update_baseline {
        let new_baseline = Baseline::from_report(&report);
        let path = new_baseline.save(&config_dir)?;
        let total: usize = new_baseline.checks.values().map(BTreeMap::len).sum();
        println!(
            "wrote style baseline ({} entry across {} check(s)) to {}",
            total,
            new_baseline.checks.len(),
            path.display(),
        );
        return Ok(());
    }

    let baseline = Baseline::load(&config_dir)?;
    let baseline = if scope.is_empty() {
        baseline
    } else {
        baseline.filter_keys(|k| scope.key_in_scope(k))
    };
    let diff = baseline.diff(&report.violations);

    if let Some(path) = &args.report {
        let path = workspace_root.join(path);
        let md = report::render_markdown(&report, &ran, &diff);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create report dir: {}", parent.display()))?;
        }
        fs::write(&path, md).with_context(|| format!("write report: {}", path.display()))?;
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

/// Runs each selected check, returning its wall time and its violations in
/// registry order.
///
/// Eleven of the twelve checks walked the tree themselves, so the namespace
/// scanned the workspace eleven times on one core. The shared scan does that
/// once and the checks spread over the machine; `collect` preserves registry
/// order, so the merged report is the one the sequential loop produced.
fn run_checks(
    selected: &[&dyn Check],
    ctx: &Context<'_>,
    project: &ProjectConfig,
    workspace_root: &Path,
) -> Result<Vec<(Duration, Vec<Violation>)>> {
    let test_module_paths = cfg_test_module_globs(workspace_root);
    selected
        .par_iter()
        .map(|check| {
            let started = Instant::now();
            let mut violations = check.run(ctx)?;
            if check.uses_global_lint_excludes() {
                let mut check_report = Report::default();
                check_report.extend(violations);
                apply_lint_excludes(
                    &mut check_report,
                    &project.lint_exclude.paths,
                    &project.lint_exclude.modules,
                    &test_module_paths,
                    workspace_root,
                );
                violations = check_report.violations;
            }
            Ok((started.elapsed(), violations))
        })
        .collect()
}

fn run_fix(
    registry: &[Box<dyn Check>],
    filter: &Option<HashSet<&str>>,
    ctx: &Context<'_>,
    allow_dirty: bool,
) -> Result<()> {
    crate::util::ensure_clean_tree(ctx.workspace_root, allow_dirty, "xtask lint style")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fix_edits_the_text_the_previous_fix_left() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("crates/demo/src");
        fs::create_dir_all(&src).expect("mkdir");
        let manifest = dir.path().join("crates/demo/Cargo.toml");
        fs::write(
            &manifest,
            "[package]\nname = \"demo\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .expect("write manifest");
        let path = src.join("lib.rs");
        fs::write(
            &path,
            "pub struct S {\n    b: u8,\n    a: u8,\n}\n\nimpl S {\n    fn b(&self) {}\n\n    fn a(&self) {}\n}\n",
        )
        .expect("write");
        let metadata = MetadataCommand::new()
            .manifest_path(&manifest)
            .no_deps()
            .exec()
            .expect("metadata");
        let config = StyleConfig::default();
        let scan = Scan::new(dir.path());
        let scope = Scope::default();
        let ctx = Context {
            workspace_root: dir.path(),
            metadata: &metadata,
            scan: &scan,
            scope: &scope,
            config: &config,
        };
        let filter = Some(HashSet::from(["struct_field_order", "trait_item_order"]));

        run_fix(&registry(), &filter, &ctx, true).expect("fix");

        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "pub struct S {\n    a: u8,\n    b: u8,\n}\n\nimpl S {\n    fn a(&self) {}\n\n    fn b(&self) {}\n}\n",
        );
    }
}
