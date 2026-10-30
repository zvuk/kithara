use std::{
    collections::BTreeMap,
    path::Path,
    process::{Command, Output, Stdio},
};

use anyhow::{Context as _, Result, bail, ensure};
use clap::Args;
use serde::Deserialize;

use crate::{
    Ctx,
    ast_rules::{Rules, excluded_globs, filter_file_paths, source_paths, test_module_globs},
    common::{exclude::cfg_test_byte_ranges, report::print_check_block, walker::matches_any},
    util::ensure_clean_tree,
    verdict::NotClean,
};

#[derive(Debug, Args)]
pub struct AstGrepArgs {
    /// Optional paths to scan. Empty = whole workspace.
    pub paths: Vec<String>,
    /// Skip the dirty-tree gate that protects `--fix` from mixing with
    /// uncommitted user edits. Mirrors `cargo fmt`/`cargo fix` UX.
    #[arg(long = "allow-dirty")]
    pub allow_dirty: bool,
    /// Apply rule fixes by passing `--update-all` to ast-grep. Only rules
    /// that declare a `fix:` block in `.config/ast-grep/*.yml` actually
    /// rewrite anything; rules without one stay reporting-only.
    /// Refuses to run on a dirty working tree unless `--allow-dirty`.
    #[arg(long)]
    pub fix: bool,
    /// Print one diagnostic per line instead of grouping by rule.
    #[arg(long)]
    pub raw: bool,
    /// Promote every warning to an error (passes `--warning` to ast-grep).
    /// Use for exhaustive sweeps when warning-level rules should also fail.
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Deserialize)]
struct Match {
    range: Range,
    file: String,
    message: String,
    #[serde(rename = "ruleId")]
    rule_id: String,
    severity: String,
    metadata: Option<RuleMetadata>,
}

#[derive(Debug, Deserialize)]
struct RuleMetadata {
    category: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Range {
    #[serde(rename = "byteOffset")]
    byte_offset: ByteOffset,
    start: Position,
}

#[derive(Debug, Deserialize)]
struct ByteOffset {
    start: usize,
}

#[derive(Debug, Deserialize)]
struct Position {
    column: u32,
    line: u32,
}

pub(crate) fn run(args: &AstGrepArgs, ctx: &Ctx) -> Result<()> {
    if args.fix {
        ensure!(
            std::fs::canonicalize(std::env::current_dir()?)? == std::fs::canonicalize(&ctx.root)?,
            "ast-grep --fix must run from its workspace root so the dirty-tree gate protects the target",
        );
        ensure_clean_tree(&ctx.root, args.allow_dirty, "ast-grep")?;
        run_native(args, ctx)?;
    }
    run_grouped(args, ctx)
}

/// Exclude test code from ast-grep at the source, consistent with the
/// `arch`/`style`/`idioms` namespaces. Each `[lint_exclude].paths` glob is
/// passed as a negated `--globs` so ast-grep skips those files for scanning
/// AND `--fix`, uniformly across all rules (per-rule `ignores:` blocks vary).
/// ast-grep's `--globs` "always overrides any other ignore logic". A file a
/// `#[cfg(test)] mod name;` declaration brings in is excluded the same way:
/// ast-grep does no cfg evaluation and reads one file at a time, so the
/// attribute standing in the parent is invisible to every rule. Inline
/// test-only items are filtered from the structured diagnostics below.
fn add_exclude_globs(cmd: &mut Command, ctx: &Ctx) {
    let project = &ctx.config;
    for pat in &project.lint_exclude.runtime_paths() {
        cmd.arg("--globs").arg(format!("!{pat}"));
    }
    for pat in test_module_globs(ctx) {
        cmd.arg("--globs").arg(format!("!{pat}"));
    }
}

fn add_source_globs(cmd: &mut Command, ctx: &Ctx) {
    for pattern in &ctx.config.workspace_scan.exclude {
        cmd.arg("--globs").arg(format!("!{pattern}"));
    }
}

fn run_native(args: &AstGrepArgs, ctx: &Ctx) -> Result<()> {
    let rules = Rules::load(ctx)?;
    let paths = filter_file_paths(ctx, &source_paths(ctx, &args.paths)?, false)?;
    if args.fix {
        let root = std::fs::canonicalize(&ctx.root)?;
        for path in &paths {
            ensure!(
                std::fs::canonicalize(ctx.root.join(path))?.starts_with(&root),
                "ast-grep --fix path is outside its workspace: {}",
                path.display(),
            );
        }
    }
    if paths.is_empty() {
        return Ok(());
    }
    let mut cmd = Command::new(ctx.config.tools.program("ast-grep"));
    cmd.current_dir(&ctx.root)
        .arg("scan")
        .arg("--config")
        .arg(rules.config())
        .arg("--report-style")
        .arg("short");
    add_exclude_globs(&mut cmd, ctx);
    add_source_globs(&mut cmd, ctx);
    if args.strict {
        cmd.arg("--warning");
    }
    if args.fix {
        cmd.arg("--update-all");
    }
    cmd.arg("--");
    for p in &paths {
        cmd.arg(p);
    }
    let status = cmd.status()?;
    if !status.success() && status.code() != Some(1) {
        bail!("ast-grep autofix failed (exit code {:?})", status.code());
    }
    Ok(())
}

/// Group scoped native rules under their declared report category.
fn parse_into(stdout: &str, by_rule: &mut BTreeMap<String, RuleGroup>) -> Result<()> {
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let m: Match = serde_json::from_str(line).context("parse ast-grep diagnostic")?;
        let rule_id = m
            .metadata
            .and_then(|metadata| metadata.category)
            .unwrap_or_else(|| m.rule_id.clone());
        let entry = by_rule.entry(rule_id.clone()).or_insert_with(|| RuleGroup {
            severity: m.severity.clone(),
            message: m.message.clone(),
            hits: Vec::new(),
        });
        ensure!(
            entry.severity == m.severity,
            "inconsistent severity for ast-grep category {rule_id}",
        );
        entry.hits.push(Hit {
            file: m.file,
            rule_id: m.rule_id,
            column: m.range.start.column + 1,
            line: m.range.start.line + 1,
            byte_offset: m.range.byte_offset.start,
        });
    }
    Ok(())
}

fn parse_output(output: &Output, by_rule: &mut BTreeMap<String, RuleGroup>) -> Result<()> {
    // Exit 1 reports diagnostics; other nonzero exits are execution failures.
    if !output.status.success() && output.status.code() != Some(1) {
        bail!(
            "ast-grep failed (exit code {:?}): {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim(),
        );
    }
    let stdout = std::str::from_utf8(&output.stdout).context("decode ast-grep diagnostics")?;
    if !output.status.success() && stdout.trim().is_empty() {
        bail!("ast-grep failed without diagnostics");
    }
    parse_into(stdout, by_rule)
}

fn exclude_test_hits(groups: &mut BTreeMap<String, RuleGroup>, ctx: &Ctx) -> Result<()> {
    let excludes = excluded_globs(ctx, false)?;
    let mut ranges_by_file: BTreeMap<String, Option<Vec<std::ops::Range<usize>>>> = BTreeMap::new();
    for group in groups.values_mut() {
        group.hits.retain(|hit| {
            let path = Path::new(&hit.file);
            if matches_any(&excludes, path.strip_prefix(&ctx.root).unwrap_or(path)) {
                return false;
            }
            let ranges = ranges_by_file.entry(hit.file.clone()).or_insert_with(|| {
                let source = std::fs::read_to_string(ctx.root.join(&hit.file)).ok()?;
                cfg_test_byte_ranges(&source)
            });
            ranges
                .as_ref()
                .is_none_or(|ranges| !ranges.iter().any(|range| range.contains(&hit.byte_offset)))
        });
    }
    groups.retain(|_, group| !group.hits.is_empty());
    Ok(())
}

/// Runs a second pass for hard-correctness rules that must also see tests: the main scan applies
/// `[lint_exclude].paths` (production-only), so each `scan_all` rule is re-run standalone with no
/// exclude globs, replacing its prod-only group.
fn run_grouped(args: &AstGrepArgs, ctx: &Ctx) -> Result<()> {
    let project = &ctx.config;
    let rules = Rules::load(ctx)?;
    let paths = filter_file_paths(ctx, &source_paths(ctx, &args.paths)?, true)?;
    let production_paths = filter_file_paths(ctx, &paths, false)?;

    let mut cmd = Command::new(ctx.config.tools.program("ast-grep"));
    cmd.current_dir(&ctx.root)
        .arg("scan")
        .arg("--config")
        .arg(rules.config())
        .arg("--include-metadata")
        .arg("--json=stream");
    add_exclude_globs(&mut cmd, ctx);
    add_source_globs(&mut cmd, ctx);
    if args.strict {
        cmd.arg("--warning");
    }
    cmd.arg("--");
    for p in &production_paths {
        cmd.arg(p);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut by_rule: BTreeMap<String, RuleGroup> = BTreeMap::new();
    if !production_paths.is_empty() {
        let output = cmd.output()?;
        parse_output(&output, &mut by_rule)?;
        exclude_test_hits(&mut by_rule, ctx)?;
    }

    for rule_id in &project.lint_exclude.scan_all_rules {
        if paths.is_empty() {
            break;
        }
        let selected = rules.rule(rule_id)?;
        let filter = selected
            .iter()
            .map(|id| regex::escape(id))
            .collect::<Vec<_>>()
            .join("|");
        let mut rule_cmd = Command::new(ctx.config.tools.program("ast-grep"));
        rule_cmd
            .current_dir(&ctx.root)
            .arg("scan")
            .arg("--config")
            .arg(rules.config())
            .arg("--filter")
            .arg(format!("^(?:{filter})$"))
            .arg("--include-metadata")
            .arg("--json=stream");
        if args.strict {
            rule_cmd.arg("--warning");
        }
        add_source_globs(&mut rule_cmd, ctx);
        rule_cmd.arg("--");
        for p in &paths {
            rule_cmd.arg(p);
        }
        rule_cmd.stdout(Stdio::piped());
        rule_cmd.stderr(Stdio::piped());
        let rule_out = rule_cmd.output()?;
        for group in by_rule.values_mut() {
            group.hits.retain(|hit| !selected.contains(&hit.rule_id));
        }
        by_rule.retain(|_, group| !group.hits.is_empty());
        parse_output(&rule_out, &mut by_rule)?;
    }

    if args.raw {
        for (rule, group) in &by_rule {
            for hit in &group.hits {
                println!(
                    "{}:{}:{}: {}[{rule}]: {}",
                    hit.file,
                    hit.line,
                    hit.column,
                    group.severity,
                    group.message.trim(),
                );
            }
        }
    } else {
        print_grouped(&by_rule);
    }

    let findings = print_failing_rules(&by_rule, args.strict);
    if findings > 0 {
        return Err(NotClean::raised("ast-grep", findings));
    }
    Ok(())
}

struct RuleGroup {
    message: String,
    severity: String,
    hits: Vec<Hit>,
}

struct Hit {
    file: String,
    rule_id: String,
    column: u32,
    line: u32,
    byte_offset: usize,
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

/// Print ONLY the rules that FAIL the run — error-severity always, plus
/// warning-severity under `--strict` (the severities ast-grep exits non-zero
/// on) — to stderr with each hit's `file:line:col`, so the actionable subset
/// stands out from the full grouped dump above instead of being buried in it.
/// Mirrors the arch ratchet's focused-failure block.
fn print_failing_rules(groups: &BTreeMap<String, RuleGroup>, strict: bool) -> usize {
    let threshold = if strict { 1 } else { 0 };
    let mut failing: Vec<(&String, &RuleGroup)> = groups
        .iter()
        .filter(|(_, g)| severity_rank(&g.severity) <= threshold)
        .collect();
    if failing.is_empty() {
        return 0;
    }
    failing.sort_by(|a, b| {
        severity_rank(&a.1.severity)
            .cmp(&severity_rank(&b.1.severity))
            .then_with(|| a.0.cmp(b.0))
    });
    let total: usize = failing.iter().map(|(_, g)| g.hits.len()).sum();
    eprintln!(
        "\n🛑 ast-grep: {total} failing hit(s) across {} rule(s) — fix these:",
        failing.len(),
    );
    for (rule_id, group) in failing {
        eprintln!("  [{}] {rule_id}", group.severity);
        for h in &group.hits {
            eprintln!("      ▸ {}:{}:{}", h.file, h.line, h.column);
        }
    }
    total
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
        print_check_block(
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

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::PathBuf, process::Command};

    use tempfile::tempdir;

    use super::{AstGrepArgs, parse_into, run_grouped};
    use crate::{Ctx, common::project::ProjectConfig};

    fn grouped_fixture(source: &str, severity: &str) -> (tempfile::TempDir, Ctx, AstGrepArgs) {
        let root = tempdir().expect("tempdir");
        fs::create_dir_all(root.path().join(".config/ast-grep")).expect("rules directory");
        fs::write(
            root.path().join("sgconfig.yml"),
            "ruleDirs: [.config/ast-grep]\n",
        )
        .expect("scan config");
        fs::write(
            root.path().join(".config/ast-grep/context.yml"),
            format!("id: context\nlanguage: Rust\nmessage: domain value\nseverity: {severity}\nrule:\n  kind: integer_literal\n  regex: '^42$'\n"),
        )
        .expect("rule");
        fs::create_dir_all(root.path().join("crates/example/src")).expect("source directory");
        fs::write(root.path().join("crates/example/src/lib.rs"), source).expect("source");
        let ctx = Ctx::new(root.path().to_owned(), ProjectConfig::default());
        let args = AstGrepArgs {
            paths: vec!["crates".to_owned()],
            allow_dirty: false,
            fix: false,
            raw: false,
            strict: false,
        };
        (root, ctx, args)
    }

    #[test]
    fn grouped_scan_filters_test_only_hits_before_deciding_failure() {
        for attr in [
            "cfg(test)",
            "cfg(all(\n test, feature = \"fixtures\"\n))",
            "test",
        ] {
            let source = format!("#[{attr}]\nfn fixture() -> u32 {{ 42 }}");
            let (_root, ctx, mut args) = grouped_fixture(&source, "error");
            assert!(run_grouped(&args, &ctx).is_ok(), "{attr}");
            args.raw = true;
            assert!(run_grouped(&args, &ctx).is_ok(), "raw: {attr}");
        }
    }

    #[test]
    fn standalone_context_scans_its_root_without_cargo_metadata() {
        let (root, ctx, mut args) = grouped_fixture("pub struct Value;", "error");
        fs::write(
            root.path().join("standalone.rs"),
            "fn value() -> u32 { 42 }",
        )
        .expect("standalone source");
        args.paths.clear();
        let error = run_grouped(&args, &ctx).expect_err("root finding remains");
        assert_eq!(
            error
                .downcast_ref::<crate::verdict::NotClean>()
                .expect("lint verdict")
                .findings,
            Some(1),
        );
        fs::write(root.path().join("standalone.rs"), "pub struct Value;")
            .expect("corrected source");
        run_grouped(&args, &ctx).expect("corrected standalone source");
    }

    #[test]
    fn grouped_scan_keeps_production_hits_on_shared_lines_and_inside_functions() {
        for source in [
            "#[cfg(test)] fn fixture() -> u32 { 42 } fn production() -> u32 { 42 }",
            "fn production() -> u32 { #[cfg(test)] fn fixture() -> u32 { 42 } 42 }",
        ] {
            let (_root, ctx, args) = grouped_fixture(source, "error");
            let error = run_grouped(&args, &ctx).expect_err("production finding remains");
            let findings = error
                .downcast_ref::<crate::verdict::NotClean>()
                .expect("lint verdict");
            assert_eq!(findings.findings, Some(1));
        }
    }

    #[test]
    fn fixer_rejects_a_target_outside_the_checked_worktree() {
        let (root, ctx, mut args) = grouped_fixture("fn production() -> u32 { 42 }", "error");
        args.fix = true;
        let error = super::run(&args, &ctx).expect_err("target and checked tree must coincide");
        assert!(error.to_string().contains("workspace root"));
        assert_eq!(
            fs::read_to_string(root.path().join("crates/example/src/lib.rs"))
                .expect("unchanged source"),
            "fn production() -> u32 { 42 }",
        );
    }

    #[test]
    fn grouped_scan_retains_production_errors_and_strict_warnings() {
        for attr in [
            "cfg(any(test, feature = \"mock\"))",
            "cfg(not(test))",
            "cfg_attr(feature = \"mock\", cfg(test))",
        ] {
            let source = format!("#[{attr}]\nfn production() -> u32 {{ 42 }}");
            let (_root, ctx, args) = grouped_fixture(&source, "error");
            assert!(run_grouped(&args, &ctx).is_err(), "{attr}");
        }
        let (_root, ctx, mut args) = grouped_fixture("fn production() -> u32 { 42 }", "warning");
        assert!(run_grouped(&args, &ctx).is_ok());
        args.strict = true;
        assert!(run_grouped(&args, &ctx).is_err());
    }

    #[test]
    fn grouped_scan_keeps_full_source_correctness_rules_in_tests() {
        let (root, mut ctx, args) = grouped_fixture("#[cfg(test)] mod fixture;", "error");
        fs::write(
            root.path().join("crates/example/src/fixture.rs"),
            "fn fixture() -> u32 { 42 }",
        )
        .expect("test-only external source");
        assert!(run_grouped(&args, &ctx).is_ok());
        ctx.config
            .lint_exclude
            .scan_all_rules
            .push("context".to_owned());
        assert!(run_grouped(&args, &ctx).is_err());
    }

    #[test]
    fn grouped_scan_rejects_execution_and_diagnostic_errors() {
        let (root, ctx, args) = grouped_fixture("", "error");
        fs::remove_file(root.path().join("sgconfig.yml")).expect("remove config");
        assert!(run_grouped(&args, &ctx).is_err());
        assert!(parse_into("invalid json", &mut BTreeMap::new()).is_err());
    }

    #[test]
    fn native_fixer_applies_edits_before_context_aware_reporting() {
        let (root, ctx, mut args) = grouped_fixture("fn production() -> u32 { 42 }", "error");
        let rule = root.path().join(".config/ast-grep/context.yml");
        let source = fs::read_to_string(&rule).expect("read rule");
        fs::write(&rule, format!("{source}fix: '43'\n")).expect("fix rule");
        args.fix = true;
        super::run_native(&args, &ctx).expect("native rewrite");
        assert_eq!(
            fs::read_to_string(root.path().join("crates/example/src/lib.rs"))
                .expect("fixed source"),
            "fn production() -> u32 { 43 }",
        );
        assert!(run_grouped(&args, &ctx).is_ok());
    }

    #[test]
    fn native_fixer_rejects_files_outside_its_workspace() {
        let (root, ctx, mut args) = grouped_fixture("", "error");
        let outside = tempdir().expect("outside workspace");
        let file = outside.path().join("outside.rs");
        fs::write(&file, "fn production() -> u32 { 42 }").expect("outside source");
        let rule = root.path().join(".config/ast-grep/context.yml");
        let text = fs::read_to_string(&rule).expect("rule");
        fs::write(&rule, format!("{text}fix: '43'\n")).expect("fix rule");
        args.fix = true;
        args.paths = vec![file.to_string_lossy().into_owned()];
        let error = super::run_native(&args, &ctx).expect_err("foreign file cannot be fixed");
        assert!(error.to_string().contains("outside"));
        assert_eq!(
            fs::read_to_string(file).expect("unchanged"),
            "fn production() -> u32 { 42 }"
        );
    }

    fn rule_hits_at(rule_file: &str, relative_path: &str, source: &str) -> usize {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join(relative_path);
        fs::create_dir_all(source_path.parent().expect("fixture parent"))
            .expect("create fixture directory");
        fs::write(&source_path, source).expect("write fixture");

        let rules = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.config/ast-grep");
        fs::write(
            temp.path().join("sgconfig.yml"),
            format!(
                "ruleDirs: [{}]\n",
                serde_json::to_string(&rules).expect("rule directory"),
            ),
        )
        .expect("scan config");
        let rule_id = rule_file.trim_end_matches(".yml").replace('.', "\\.");
        let output = Command::new("ast-grep")
            .current_dir(temp.path())
            .args(["scan", "--config", "sgconfig.yml", "--filter"])
            .arg(format!("^{rule_id}(\\.|$)"))
            .args(["--json=stream", relative_path])
            .output()
            .expect("run ast-grep");
        let stdout = String::from_utf8(output.stdout).expect("ast-grep stdout");
        assert!(
            output.status.success() || !stdout.is_empty(),
            "ast-grep produced no findings: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    }

    fn rule_hits(rule_file: &str, source: &str) -> usize {
        rule_hits_at(
            rule_file,
            "crates/kithara-audio/src/analysis/fixture.rs",
            source,
        )
    }

    fn primitive_pool_hits(source: &str) -> usize {
        rule_hits("perf.prefer-primitive-pool.yml", source)
    }

    fn manual_pool_registration_hits(source: &str) -> usize {
        rule_hits("perf.no-manual-pool-registration.yml", source)
    }

    fn local_test_pool_hits(source: &str) -> usize {
        rule_hits("perf.no-local-test-pools.yml", source)
    }

    fn magic_number_hits(source: &str) -> usize {
        rule_hits("style.no-magic-numbers.yml", source)
    }

    #[test]
    fn magic_number_rule_keeps_feature_enabled_and_non_test_code_visible() {
        for cfg in [
            "any(test, feature = \"mock\")",
            "not(test)",
            "feature = \"test\"",
        ] {
            let source = format!("#[cfg({cfg})]\nmod enabled {{ fn value() -> u32 {{ 42 }} }}");
            assert_eq!(magic_number_hits(&source), 1, "{cfg}");
        }
        assert_eq!(
            magic_number_hits(
                "#[cfg_attr(feature = \"mock\", cfg(test))]\nmod enabled { fn value() -> u32 { 42 } }"
            ),
            1,
        );
    }

    #[test]
    fn target_gate_rule_checks_business_code_next_to_abi_wiring() {
        let source = "#[cfg(target_os = \"android\")]\nfn gate() {}\n";
        assert_eq!(
            rule_hits_at(
                "arch.no-target-os-outside-platform.yml",
                "crates/kithara-platform/src/common/gate.rs",
                source,
            ),
            0,
        );
        for path in [
            "crates/kithara-android/src/lib.rs",
            "crates/kithara-android/src/runtime.rs",
            "crates/kithara-android/src/http/client.rs",
            "crates/kithara-ffi/src/lib.rs",
            "crates/kithara-ffi/src/native/mod.rs",
            "crates/kithara-audio/src/gate.rs",
        ] {
            assert_eq!(
                rule_hits_at("arch.no-target-os-outside-platform.yml", path, source),
                1,
                "{path}"
            );
        }
    }

    #[test]
    fn target_gate_rule_does_not_extend_abi_allowances_to_other_items() {
        for (path, source) in [
            (
                "crates/kithara-android/src/lib.rs",
                "#[cfg(target_os = \"linux\")] mod buffer;",
            ),
            (
                "crates/kithara-android/src/lib.rs",
                "#[cfg(target_os = \"android\")] mod other;",
            ),
            (
                "crates/kithara-android/src/lib.rs",
                "#[cfg(target_os = \"android\")] mod buffer { fn gate() {} }",
            ),
            (
                "crates/kithara-android/src/lib.rs",
                "mod nested { #[cfg(target_os = \"android\")] mod buffer; }",
            ),
            (
                "crates/kithara-android/src/media/sys.rs",
                "#[cfg(target_os = \"android\")] mod buffer;",
            ),
            (
                "crates/kithara-ffi/src/lib.rs",
                "#[cfg(target_arch = \"wasm32\")] mod native;",
            ),
            (
                "crates/kithara-ffi/src/lib.rs",
                "#[cfg(not(target_arch = \"wasm32\"))] use other::Inner;",
            ),
            (
                "crates/kithara-ffi/src/lib.rs",
                "#[cfg(all(feature = \"uniffi\", not(target_arch = \"wasm32\")))] uniffi::other!();",
            ),
            (
                "crates/kithara-ffi/src/native/mod.rs",
                "#[cfg(target_os = \"android\")] fn android() {}",
            ),
        ] {
            assert_eq!(
                rule_hits_at("arch.no-target-os-outside-platform.yml", path, source),
                1,
                "{path}: {source}",
            );
        }
        let with_attributes = "#[cfg(target_os = \"android\")]\n// ABI module\n#[doc = \"Buffer\"]\nmod buffer;\n#[cfg(target_os = \"android\")]\nfn gate() {}";
        assert_eq!(
            rule_hits_at(
                "arch.no-target-os-outside-platform.yml",
                "crates/kithara-android/src/lib.rs",
                with_attributes,
            ),
            1,
        );
    }

    #[test]
    fn target_gate_rule_reports_redirected_abi_modules() {
        for (path, gate, declaration) in [
            (
                "crates/kithara-android/src/lib.rs",
                "target_os = \"android\"",
                "mod buffer;",
            ),
            (
                "crates/kithara-ffi/src/lib.rs",
                "not(target_arch = \"wasm32\")",
                "mod native;",
            ),
            (
                "crates/kithara-ffi/src/lib.rs",
                "target_arch = \"wasm32\"",
                "pub mod web;",
            ),
            (
                "crates/kithara-ffi/src/native/mod.rs",
                "target_os = \"android\"",
                "pub(crate) mod android;",
            ),
        ] {
            for redirect in [
                "#[path = \"business.rs\"]",
                "#[cfg_attr(feature = \"mock\", path = \"business.rs\")]",
            ] {
                for source in [
                    format!("{redirect}\n#[cfg({gate})]\n{declaration}"),
                    format!("#[cfg({gate})]\n{redirect}\n{declaration}"),
                ] {
                    assert_eq!(
                        rule_hits_at("arch.no-target-os-outside-platform.yml", path, &source),
                        1,
                        "{path}: {source}",
                    );
                }
            }
        }
    }

    #[test]
    fn target_gate_rule_sees_nested_cfg_without_matching_feature_strings() {
        for cfg in [
            "all(feature = \"uniffi\", not(target_arch = \"wasm32\"))",
            "any(feature = \"mock\", all(target_os = \"android\", not(target_arch = \"wasm32\")))",
        ] {
            assert_eq!(
                rule_hits(
                    "arch.no-target-os-outside-platform.yml",
                    &format!("#[cfg({cfg})] fn gate() {{}}")
                ),
                1,
                "{cfg}",
            );
        }
        assert_eq!(
            rule_hits(
                "arch.no-target-os-outside-platform.yml",
                "#[cfg(feature = \"target_os\")] fn gate() {}"
            ),
            0,
        );
    }

    #[test]
    fn grouped_scan_rejects_conflicting_category_severities() {
        let (root, ctx, args) = grouped_fixture("fn value() -> u32 { 42 }", "warning");
        let rule = root.path().join(".config/ast-grep/context.yml");
        let source = fs::read_to_string(&rule).expect("rule");
        fs::write(
            rule,
            format!("{source}\n---\nid: scoped-context\nlanguage: Rust\nmetadata:\n  category: context\nmessage: domain value\nseverity: error\nrule:\n  kind: integer_literal\n  regex: '^42$'\n"),
        )
        .expect("scoped rule");
        let error = run_grouped(&args, &ctx).expect_err("category mismatch must fail");
        assert_eq!(
            error.to_string(),
            "inconsistent severity for ast-grep category context",
        );
    }

    #[test]
    fn grouped_scan_preserves_native_messages_with_different_captures() {
        let (root, ctx, mut args) = grouped_fixture("fn first() {} fn second() {}", "warning");
        fs::write(
            root.path().join(".config/ast-grep/context.yml"),
            "id: context\nlanguage: Rust\nmessage: 'Function $NAME'\nseverity: warning\nrule:\n  pattern: 'fn $NAME() {}'\n",
        )
        .expect("capture rule");
        for raw in [false, true] {
            args.raw = raw;
            args.strict = false;
            assert!(run_grouped(&args, &ctx).is_ok(), "raw: {raw}");
            args.strict = true;
            let error = run_grouped(&args, &ctx).expect_err("both warnings remain findings");
            assert_eq!(
                error
                    .downcast_ref::<crate::verdict::NotClean>()
                    .expect("lint verdict")
                    .findings,
                Some(2),
                "raw: {raw}",
            );
        }
    }

    fn module_root_hits(relative_path: &str, source: &str) -> usize {
        rule_hits_at("style.no-items-in-lib-or-mod-rs.yml", relative_path, source)
    }

    #[test]
    fn module_root_rule_reports_code_in_every_module_root() {
        let source = "mod child;\npub use child::Item;\n\nfn helper() {}\n";
        for path in [
            "crates/kithara-audio/src/lib.rs",
            "crates/kithara-audio/src/engine/mod.rs",
            "crates/kithara-audio/src/engine/tests/mod.rs",
            "crates/kithara-audio/tests/common/mod.rs",
            "tests/crates/core/src/lib.rs",
            "xtask/src/android/mod.rs",
        ] {
            assert_eq!(module_root_hits(path, source), 1, "{path}");
        }
    }

    #[test]
    fn module_root_rule_allows_declarations_and_reexports() {
        let root = "//! Engine.\n\nmod child;\n#[cfg(test)]\nmod tests;\n\npub use child::Item;\npub(crate) use child::{helper, other};\n";
        assert_eq!(
            module_root_hits("crates/kithara-audio/src/engine/mod.rs", root),
            0
        );

        let code = "fn helper() {}\n";
        assert_eq!(
            module_root_hits("crates/kithara-audio/src/engine/child.rs", code),
            0
        );
        assert_eq!(
            module_root_hits(
                "crates/kithara-devtools/tests/fixtures/workspace/crates/demo/src/lib.rs",
                code
            ),
            0
        );
    }

    #[test]
    fn local_use_rule_only_reports_production_functions() {
        let source = r#"
fn production() {
    use std::fmt::Write;
}

#[cfg(test)]
mod tests {
    fn fixture() {
        use std::fmt::Write;
    }
}

#[kithara::test]
fn macro_test() {
    use std::fmt::Write;
}

#[cfg(test)]
fn cfg_test() {
    use std::fmt::Write;
}

#[cfg(target_arch = "wasm32")]
fn target_function() {
    use std::fmt::Write;
}
"#;

        assert_eq!(
            rule_hits("style.no-local-use-in-prod-functions.yml", source),
            1
        );
    }

    #[test]
    fn inline_qualified_path_rule_only_reports_production_functions() {
        let source = r#"
fn production() {
    let _ = std::io::ErrorKind::NotFound;
}

#[cfg(test)]
mod tests {
    fn fixture() {
        let _ = std::io::ErrorKind::NotFound;
    }
}

#[kithara::test]
fn macro_test() {
    let _ = std::io::ErrorKind::NotFound;
}

#[cfg(test)]
fn cfg_test() {
    let _ = std::io::ErrorKind::NotFound;
}
"#;

        assert_eq!(rule_hits("style.no-inline-qualified-paths.yml", source), 1);
    }

    #[test]
    fn default_rule_ignores_types_that_already_implement_default() {
        let source = r#"
struct Existing;

impl Existing {
    fn new() -> Self {
        Self::default()
    }
}

impl Default for Existing {
    fn default() -> Self {
        Self
    }
}

struct Missing;

impl Missing {
    fn new() -> Self {
        Self
    }
}

#[derive(Default)]
struct Derived;

impl Derived {
    fn new() -> Self {
        Self::default()
    }
}
"#;

        assert_eq!(rule_hits("style.prefer-default-derive.yml", source), 1);
    }

    #[test]
    fn conversion_rule_only_reports_owned_direct_conversions() {
        let source = r#"
enum Input {
    One,
}

enum Output {
    One,
}

impl Input {
    fn convert(self) -> Output {
        match self {
            Self::One => Output::One,
        }
    }

    fn inspect(&self) -> Output {
        match self {
            Self::One => Output::One,
        }
    }

    fn resolve(self) -> Option<Output> {
        match self {
            Self::One => Some(Output::One),
        }
    }
}
"#;

        assert_eq!(rule_hits("idioms.match-self-conversion.yml", source), 1);
    }

    #[test]
    fn magic_number_rule_accepts_self_explanatory_math() {
        let source = r#"
fn math(value: usize, width: f32, values: &[f32]) {
    let _ = value / 2;
    let _ = value * 2;
    let _ = 2 * value;
    let _ = value % 2;
    let _ = width / 2.0;
    let _ = width * 2.0;
    let _ = 2.0 * width;
    let _ = width.rem_euclid(2.0);
    let _ = width.powi(2);
    let _ = values.windows(2);
    let _ = value / 3;
    let _ = value * 3;
    let _ = 3 * value;
    let _ = value / 4;
    let _ = value * 4;
    let _ = 4 * value;
    let _ = width / 3.0;
    let _ = width * 3.0;
    let _ = 3.0 * width;
    let _ = width / 4.0;
    let _ = width * 4.0;
    let _ = 4.0 * width;
    let _ = width + 0.5;
    let _ = 0.5 + width;
    let _ = width - 0.5;
    let _ = width * 0.5;
    let _ = 0.5 * width;
    let _: [u8; 4] = u32::to_be_bytes(0);
    let _ = u32::from_be_bytes([0, 1, 2, 3]);
    let _ = u32::from_le_bytes([0, 1, 2, 3]);
}
"#;

        assert_eq!(magic_number_hits(source), 0);
    }

    #[test]
    fn magic_number_rule_keeps_domain_values_visible() {
        let source = r#"
fn domain(value: usize, bytes: &[u8]) {
    let _ = value + 2;
    let _ = bytes[2];
    let _ = Duration::from_secs(2);
    let _ = match value { 2 => true, _ => false };
    let _ = Status { code: 2.0 };
}
"#;

        assert_eq!(magic_number_hits(source), 5);
    }

    #[test]
    fn prefer_expect_rule_preserves_formatted_panics() {
        let source = r#"
fn values(plain: Option<u8>, captured: Option<u8>, key: &str) {
    let _ = plain.unwrap_or_else(|| panic!("missing value"));
    let _ = captured.unwrap_or_else(|| panic!("missing {key}"));
}
"#;

        assert_eq!(rule_hits("style.prefer-expect.yml", source), 1);
    }

    #[test]
    fn impl_only_consts_rule_rejects_a_type_used_as_a_constant_namespace() {
        let source = r#"
struct Consts;

impl Consts {
    const FIRST: u32 = 1;
    const SECOND: u32 = 2;
    const THIRD: u32 = 3;
}

pub(crate) enum Limits {}

impl Limits {
    pub(crate) const LOW: u32 = 1;
    pub(crate) const MID: u32 = 2;
    pub(crate) const HIGH: u32 = 3;
}
"#;

        assert_eq!(rule_hits("style.no-impl-only-consts.yml", source), 2);
    }

    #[test]
    fn impl_only_consts_rule_keeps_constant_modules_and_real_types() {
        let source = r#"
mod consts {
    pub(super) const FIRST: u32 = 1;
    pub(super) const SECOND: u32 = 2;
    pub(super) const THIRD: u32 = 3;
}

struct Band {
    gain: f32,
}

impl Band {
    const LOW: u32 = 1;
    const MID: u32 = 2;
    const HIGH: u32 = 3;
}

enum Mode {
    Fast,
    Slow,
}

impl Mode {
    const FIRST: u32 = 1;
    const SECOND: u32 = 2;
    const THIRD: u32 = 3;
}

struct Widget;

impl Widget {
    const FIRST: u32 = 1;
    const SECOND: u32 = 2;
    const THIRD: u32 = 3;
}

impl Clone for Widget {
    fn clone(&self) -> Self {
        Self
    }
}
"#;

        assert_eq!(rule_hits("style.no-impl-only-consts.yml", source), 0);
    }

    #[test]
    fn module_consts_rule_rejects_private_constants_outside_a_consts_module() {
        let source = r#"
const LOOSE: u32 = 1;
pub(crate) const SHARED: u32 = 2;

mod nested {
    pub(super) const INNER: u32 = 3;
}
"#;

        assert_eq!(
            rule_hits("style.module-consts-in-crate-consts.yml", source),
            3
        );
    }

    #[test]
    fn module_consts_rule_keeps_consts_modules_public_api_and_scoped_constants() {
        let source = r#"
pub const EXPORTED: u32 = 1;
const _: () = ();

mod consts {
    pub(super) const GATED: u32 = 2;
}

struct Band;

impl Band {
    const LOW: u32 = 3;
}

fn local() -> u32 {
    const STEP: u32 = 4;
    STEP
}
"#;

        assert_eq!(
            rule_hits("style.module-consts-in-crate-consts.yml", source),
            0
        );
    }

    #[test]
    fn module_consts_rule_skips_the_crate_consts_file() {
        let source = "pub(crate) const SHARED: u32 = 1;\n";

        assert_eq!(
            rule_hits_at(
                "style.module-consts-in-crate-consts.yml",
                "crates/kithara-audio/src/consts.rs",
                source,
            ),
            0
        );
    }

    #[test]
    fn a_reported_hit_names_the_line_an_editor_calls_it() {
        let stdout = r#"{"file":"crates/kithara-ui/src/capture/set.rs","message":"m","ruleId":"perf.prefer-primitive-pool","severity":"error","range":{"byteOffset":{"start":300},"start":{"line":29,"column":4}}}"#;
        let mut by_rule = BTreeMap::new();
        parse_into(stdout, &mut by_rule).expect("valid diagnostic");
        let hit = &by_rule["perf.prefer-primitive-pool"].hits[0];
        assert_eq!(
            hit.line, 30,
            "ast-grep counts lines from zero, a reader from one"
        );
        assert_eq!(
            hit.column, 5,
            "ast-grep counts columns from zero, a reader from one"
        );
    }

    #[test]
    fn component_pool_rule_rejects_non_owner_region_construction() {
        let source = r#"
kithara_bufpool::pool_schema! {
    pub LocalPools {
        bytes: u8,
        samples: f32,
        commands: VecKey<DrawCmd, 1>,
        text: StringKey<1>,
    }
}

impl HasPool<u8> for HandWrittenPools {
    fn __slot(&self) -> &PoolSlot<u8> { todo!() }
}

impl<T: Copy> kithara_bufpool::HasPool<f32> for GenericPools<T> {
    fn __slot(&self) -> &PoolSlot<f32> { todo!() }
}

fn component(overall_budget: OverallBudget) {
    let direct = PoolRegion::__build(OverallBudget(64), |_| Ok(()));
    let typed = PoolRegion::<LocalPools>::__build(OverallBudget(64), |_| Ok(()));
    let qualified = kithara_bufpool::PoolRegion::__build(OverallBudget(64), |_| Ok(()));
    let local = LocalPools::builder(OverallBudget(64));
    let arbitrary_schema = BufferSchema::builder(kithara_bufpool::OverallBudget(64));
    let imported = crate::pools::AppPools::builder(overall_budget);
    let ffi = kithara_ffi::pools::FfiPools::builder(overall_budget);
}

#[cfg(test)]
mod tests {
    fn fixture() {
        let pools = TestPools::builder(OverallBudget(64));
        let region = PoolRegion::__build(OverallBudget(64), |_| Ok(()));
    }
}
"#;

        assert_eq!(
            rule_hits("perf.no-component-pool-construction.yml", source),
            6
        );
        assert_eq!(manual_pool_registration_hits(source), 2);
    }

    #[test]
    fn local_test_pool_rule_requires_the_shared_schema() {
        let source = r#"
pool_schema! {
    pub(crate) TestPools {
        bytes: u8,
        samples: f32,
    }
}

kithara_bufpool::pool_schema!(pub TestPools { bytes: u8 });
pool_schema![TestPools { samples: f32 }];

pool_schema! {
    pub InlineTestPools { bytes: u8 }
}

mod test_pools;

fn local_region() {
    let pools = TestPools::region(overall, bytes, samples);
}
"#;

        assert_eq!(local_test_pool_hits(source), 5);
        assert_eq!(
            rule_hits_at(
                "perf.no-local-test-pools.yml",
                "crates/kithara-bufpool/src/testing.rs",
                source,
            ),
            0
        );
    }

    #[test]
    fn component_pool_rule_covers_aliases_hidden_constructor_and_macro_delimiters() {
        let source = r#"
use kithara_bufpool::{pool_schema as schema, PoolRegion as Region};

pool_schema!(pub RoundPools { bytes: u8 });
pool_schema![pub SquarePools { samples: f32 }];
schema!(pub AliasedPools { bytes: u8 });

fn component() {
    let direct = Region::__build(OverallBudget(64), |_| Ok(()));
    let hidden = crate::RoundPools::__build_region(
        OverallBudget(64),
        PoolConfig::builder().max_buffers(8).build(),
    );
    let aliased = AliasedPools::builder(OverallBudget(64));
}
"#;

        assert_eq!(
            rule_hits("perf.no-component-pool-construction.yml", source),
            5
        );
    }

    #[test]
    fn component_pool_rule_allows_only_exact_owner_modules() {
        let source = r#"
pool_schema! {
    pub AppPools { bytes: u8 }
}

impl HasPool<u8> for HandWrittenPools {
    fn __slot(&self) -> &PoolSlot<u8> { todo!() }
}

fn build(overall_budget: OverallBudget) {
    let direct = PoolRegion::__build(overall_budget, |_| Ok(()));
    let generated = AppPools::builder(OverallBudget(1024));
}
"#;

        for owner in [
            "crates/kithara-app/src/pools.rs",
            "crates/kithara-ffi/src/pools.rs",
        ] {
            assert_eq!(
                rule_hits_at("perf.no-component-pool-construction.yml", owner, source),
                0,
                "composition owner {owner}"
            );
            assert_eq!(
                rule_hits_at("perf.no-manual-pool-registration.yml", owner, source),
                1,
                "manual registration at {owner}"
            );
        }
        for non_owner in [
            "crates/kithara-app/src/app.rs",
            "crates/kithara-ffi/src/player.rs",
        ] {
            assert_eq!(
                rule_hits_at("perf.no-component-pool-construction.yml", non_owner, source),
                3,
                "non-owner module {non_owner}"
            );
            assert_eq!(
                rule_hits_at("perf.no-manual-pool-registration.yml", non_owner, source),
                1,
                "manual registration at {non_owner}"
            );
        }
    }

    #[test]
    fn pool_rules_cover_product_crates_and_ignore_pool_infrastructure() {
        let primitive = "fn scratch() { let values: Vec<f32> = vec![0.0; 8]; }";
        let local = "fn component() { let pools = AppPools::builder(OverallBudget(1024)); }";
        let manual = r#"
impl Registered<u8> for HandWrittenPools {
    fn __slot(&self) -> &PoolSlot<u8> { todo!() }
}
"#;

        for path in [
            "crates/kithara-beat/src/fixture.rs",
            "crates/kithara-encode/src/fixture.rs",
        ] {
            assert_eq!(
                rule_hits_at("perf.prefer-primitive-pool.yml", path, primitive),
                1,
                "primitive allocation at {path}"
            );
            assert_eq!(
                rule_hits_at("perf.no-component-pool-construction.yml", path, local),
                1,
                "local pool at {path}"
            );
            assert_eq!(
                rule_hits_at("perf.no-manual-pool-registration.yml", path, manual),
                1,
                "manual registration at {path}"
            );
        }

        let infrastructure = "crates/kithara-bufpool/src/fixture.rs";
        assert_eq!(
            rule_hits_at("perf.prefer-primitive-pool.yml", infrastructure, primitive),
            0
        );
        assert_eq!(
            rule_hits_at(
                "perf.no-component-pool-construction.yml",
                infrastructure,
                local
            ),
            0
        );
        assert_eq!(
            rule_hits_at(
                "perf.no-manual-pool-registration.yml",
                infrastructure,
                manual
            ),
            0
        );
        assert_eq!(
            rule_hits_at(
                "perf.no-component-pool-construction.yml",
                "crates/kithara-play/src/session/testing.rs",
                local
            ),
            0
        );
        assert_eq!(
            rule_hits_at(
                "perf.prefer-primitive-pool.yml",
                "crates/kithara-host/src/session/testing.rs",
                primitive
            ),
            0
        );
        assert_eq!(
            rule_hits_at(
                "perf.no-manual-pool-registration.yml",
                "crates/kithara-play/src/session/testing.rs",
                manual
            ),
            0
        );

        for (rule, source) in [
            ("perf.prefer-primitive-pool.yml", primitive),
            ("perf.no-component-pool-construction.yml", local),
            ("perf.no-manual-pool-registration.yml", manual),
        ] {
            assert_eq!(
                rule_hits_at(rule, "crates/kithara-audio/src/testing.rs", source),
                1,
                "only the two explicit test facades may be ignored by {rule}"
            );
        }
    }

    #[test]
    fn pool_rules_allow_test_items_and_reject_wasm_construction() {
        let source = r#"
#[cfg(test)]
impl Fixture {
    fn pool() {
        let pools = TestPools::builder(OverallBudget(1024));
        let region = PoolRegion::__build(OverallBudget(1024), |_| Ok(()));
        let values: Vec<f32> = vec![0.0; 8];
    }
}

#[cfg(test)]
impl HasPool<u8> for Fixture {
    fn __slot(&self) -> &PoolSlot<u8> { todo!() }
}

#[cfg(test)]
pool_schema! { pub InlineTestPools { bytes: u8 } }

#[cfg(all(feature = "usdt", test))]
impl HasPool<f32> for AllCfgFixture {
    fn __slot(&self) -> &PoolSlot<f32> { todo!() }
}

#[cfg(all(feature = "usdt", test))]
pool_schema!(pub AllCfgPools { samples: f32 });

#[cfg(test)]
fn fixture() {
    let pools = TestPools::builder(OverallBudget(1024));
    let region = PoolRegion::__build(OverallBudget(1024), |_| Ok(()));
    let values: Vec<f32> = vec![0.0; 8];
}

#[cfg(all(feature = "usdt", test))]
fn all_cfg_fixture() {
    let region = PoolRegion::__build(OverallBudget(1024), |_| Ok(()));
}

#[kithara::test]
async fn attributed_fixture() {
    let region = PoolRegion::__build(OverallBudget(1024), |_| Ok(()));
}

#[wasm_bindgen(start)]
fn setup() {
    let pools = FfiPools::builder(OverallBudget(1024));
    let region = PoolRegion::__build(OverallBudget(1024), |_| Ok(()));
    let values: Vec<f32> = vec![0.0; 8];
}
"#;

        assert_eq!(
            rule_hits("perf.no-component-pool-construction.yml", source),
            2
        );
        assert_eq!(manual_pool_registration_hits(source), 0);
        assert_eq!(primitive_pool_hits(source), 1);
    }

    #[test]
    fn primitive_pool_rule_covers_vec_allocation_forms() {
        let source = r#"
fn scratch(values: &[f64], count: usize) {
    let bytes = Vec::<u8>::with_capacity(count);
    let pcm: Vec<f32> = values.iter().map(|&value| value as f32).collect();
    let inferred_bytes = vec![0_u8; count];
    let collected: Vec<f64> = values.iter().copied().collect();
    let copied: Vec<i16> = [1_i16, 2].to_vec();
    let repeated: Vec<bool> = [true].repeat(count);
    let macro_buffer: Vec<char> = vec!['x'; count];
    let empty = Vec::<u64>::new();
    let reserved = Vec::<usize>::with_capacity(count);
    let converted = Vec::<u32>::from([1_u32, 2]);
}

fn inferred(values: &[f64]) -> Vec<f64> {
    values.iter().copied().collect::<Vec<_>>()
}
"#;

        assert_eq!(primitive_pool_hits(source), 11);
    }

    #[test]
    fn primitive_pool_rule_rejects_inferred_vec_returns() {
        let source = r#"
fn vector(values: &[f32]) -> Vec<f32> {
    values.iter().copied().collect()
}
"#;

        assert_eq!(primitive_pool_hits(source), 1);
    }

    #[test]
    fn primitive_pool_rule_rejects_empty_vectors_that_can_grow() {
        let source = r#"
fn scratch(bytes: &[u8], count: usize) {
    let mut copied: Vec<u8> = Vec::new();
    copied.extend_from_slice(bytes);

    let mut pcm = Vec::<f32>::new();
    pcm.push(0.0);

    let mut inferred = Vec::new();
    inferred.push(0_u16);

    let mut ids: Vec<u64> = Vec::default();
    ids.reserve(count);

    let mut flags: Vec<bool> = Default::default();
    flags.resize(count, false);
}

fn samples(sample: f32) {
    let mut copied = Vec::new();
    copied.push(sample);
}
"#;

        assert_eq!(primitive_pool_hits(source), 6);
    }

    #[test]
    fn primitive_pool_rule_rejects_all_inferred_growth_forms() {
        let source = r#"
struct Item;

fn owner(items: &[Item], count: usize) {
    let mut pushed = Vec::new();
    pushed.push(Item);

    let mut resized = Vec::default();
    resized.resize(count, Item);

    let mut extended = Default::default();
    extended.extend(items.iter());

    let mut copied = Vec::with_capacity(count);
    copied.extend_from_slice(items);

    let mut reserved = Vec::new();
    reserved.reserve(count);

    let mut reserved_exact = Vec::new();
    reserved_exact.reserve_exact(count);
}
"#;

        assert_eq!(primitive_pool_hits(source), 6);
    }

    #[test]
    fn primitive_pool_rule_requires_explicit_non_primitive_collection_types() {
        let source = r#"
struct Item;

fn owner(count: usize) {
    let mut values: Vec<Item> = Vec::with_capacity(count);
    values.push(Item);
    let explicit = std::iter::repeat_with(|| Item)
        .take(count)
        .collect::<Vec<Item>>();
    let inferred = std::iter::repeat_with(|| Item).take(count).collect::<Vec<_>>();
}
"#;

        assert_eq!(primitive_pool_hits(source), 1);
    }

    #[test]
    fn primitive_pool_rule_allows_documented_durable_output() {
        let source = r#"
struct EncodedAccessUnit {
    bytes: Vec<u8>,
}

fn access_unit(output: &[u8]) -> EncodedAccessUnit {
    EncodedAccessUnit {
        bytes: output.to_vec(),
    }
}
"#;

        assert_eq!(primitive_pool_hits(source), 0);
    }

    #[test]
    fn primitive_pool_rule_rejects_owned_result_and_field_construction() {
        let source = r#"
struct Owner {
    values: Vec<f64>,
}

fn field(count: usize) -> Owner {
    Owner {
        values: vec![0.0; count],
    }
}

fn local(count: usize) -> Vec<u8> {
    let mut values = vec![0_u8; count];
    values.fill(1);
    return values;
}

fn tuple(count: usize) -> Result<(usize, Vec<u8>), ()> {
    let mut values = vec![0_u8; count];
    values.fill(1);
    return Ok((count, values));
}

fn replace(owner: &mut Owner, count: usize) {
    owner.values = vec![0.0; count];
}
"#;

        assert_eq!(primitive_pool_hits(source), 4);
    }

    #[test]
    fn primitive_pool_rule_preserves_legacy_byte_and_pcm_coverage() {
        let source = r#"
fn legacy(count: usize) {
    let _ = vec![0u8; count];
    let _ = vec![0_u8; count];
    let _ = vec![0; count];
    let _ = Vec::<u8>::with_capacity(count);
    let _ = Vec::<u8>::new();
    let _ = vec![0.0f32; count];
    let _ = vec![0_f32; count];
    let _ = vec![0.0_f32; count];
    let _ = vec![0.0; count];
    let _ = Vec::<f32>::with_capacity(count);
    let _ = Vec::<f32>::new();
}
"#;

        assert_eq!(primitive_pool_hits(source), 11);
    }

    #[test]
    fn primitive_pool_rule_ignores_inline_tests() {
        let source = r#"
fn production(values: &[f64]) {
    let scratch: Vec<f64> = values.to_vec();
}

#[cfg(test)]
mod tests {
    fn fixture(values: &[f64]) {
        let owned: Vec<f64> = values.to_vec();
    }
}
"#;

        assert_eq!(primitive_pool_hits(source), 1);
    }
}
