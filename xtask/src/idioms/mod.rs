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
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use cargo_metadata::MetadataCommand;
use clap::Args;

mod checks;
mod config;

use checks::{Context, registry};
use config::IdiomsConfig;

use crate::common::{
    baseline::{Baseline, RatchetDiff},
    report,
    scope::Scope,
    violation::Report,
    walker::{compile_globs, matches_any},
};

#[derive(Debug, Default, Args)]
pub(crate) struct IdiomsArgs {
    #[arg(long = "check")]
    pub check: Vec<String>,
    #[arg(long)]
    pub report: Option<PathBuf>,
    #[arg(long)]
    pub json: bool,
    #[arg(long = "update-baseline")]
    pub update_baseline: bool,
    #[arg(long, default_value = ".config/idioms")]
    pub config_dir: PathBuf,
    /// Restrict scan to specific crate(s) by name. Repeatable.
    #[arg(long = "crate", value_name = "NAME")]
    pub crates: Vec<String>,
    /// Restrict scan to workspace-relative path(s). Repeatable.
    #[arg(long = "path", value_name = "PATH")]
    pub paths: Vec<PathBuf>,
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

    apply_exclude_paths(&mut report, &config.thresholds.exclude_paths);

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

/// Drop violations whose path portion matches any `exclude_paths` glob.
/// Applied centrally before baseline-write and ratchet-diff so the exclusion
/// is consistent across both. A no-op when `patterns` is empty.
fn apply_exclude_paths(report: &mut Report, patterns: &[String]) {
    if patterns.is_empty() {
        return;
    }
    let globs = compile_globs(patterns);
    report
        .violations
        .retain(|v| !matches_any(&globs, Path::new(key_path(&v.key))));
}

/// Extract the workspace-relative path portion from a violation key. Keys look
/// like `crates/<crate>/src/.../file.rs:line:col` or `file.rs::Name`; the path
/// always ends at the `.rs` extension, so we slice up to and including it.
fn key_path(key: &str) -> &str {
    key.find(".rs").map_or(key, |i| &key[..i + 3])
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::violation::Violation;

    fn exclude_globs() -> Vec<String> {
        [
            "**/tests/**",
            "**/tests.rs",
            "**/*_test.rs",
            "**/test_*.rs",
            "crates/kithara-test-utils/**",
            "crates/kithara-test-macros/**",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect()
    }

    #[test]
    fn excludes_test_code_keeps_production() {
        let mut report = Report::default();
        report.extend([
            Violation::warn("loop_allocation", "crates/x/tests.rs:10:4", "test"),
            Violation::warn(
                "fat_loop_body",
                "crates/x/tests/helper.rs:5:loop_body",
                "test",
            ),
            Violation::warn(
                "box_concrete_type",
                "crates/kithara-test-utils/src/a.rs:1:1",
                "test",
            ),
            Violation::warn(
                "arc_mutex_collection",
                "crates/kithara-test-macros/src/b.rs:2:2",
                "test",
            ),
            Violation::warn("loop_allocation", "crates/x/src/foo.rs:7:8", "test"),
        ]);

        apply_exclude_paths(&mut report, &exclude_globs());

        let keys: Vec<&str> = report.violations.iter().map(|v| v.key.as_str()).collect();
        assert_eq!(keys, ["crates/x/src/foo.rs:7:8"]);
    }

    #[test]
    fn empty_patterns_is_noop() {
        let mut report = Report::default();
        report.extend([Violation::warn(
            "loop_allocation",
            "crates/x/tests.rs:10:4",
            "test",
        )]);
        apply_exclude_paths(&mut report, &[]);
        assert_eq!(report.violations.len(), 1);
    }

    #[test]
    fn key_path_strips_line_col_and_name_suffix() {
        assert_eq!(key_path("crates/x/src/foo.rs:7:8"), "crates/x/src/foo.rs");
        assert_eq!(
            key_path("crates/x/src/foo.rs:173:48::outcome"),
            "crates/x/src/foo.rs"
        );
        assert_eq!(key_path("kithara-foo"), "kithara-foo");
    }
}
