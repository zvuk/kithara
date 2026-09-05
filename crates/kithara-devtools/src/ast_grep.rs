use std::{
    collections::BTreeMap,
    process::{Command, Stdio},
};

use anyhow::{Result, bail};
use clap::Args;
use serde::Deserialize;

use crate::{
    Ctx,
    common::{exclude::cfg_test_module_globs, report::print_check_block},
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
    /// Bypass the grouped renderer and stream ast-grep's native short
    /// output verbatim. Useful when you need the upstream formatting.
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
}

#[derive(Debug, Deserialize)]
struct Range {
    start: Position,
}

#[derive(Debug, Deserialize)]
struct Position {
    column: u32,
    line: u32,
}

pub(crate) fn run(args: &AstGrepArgs, ctx: &Ctx) -> Result<()> {
    if args.fix {
        ensure_clean_tree(args.allow_dirty, "ast-grep")?;
        return run_native(args, ctx);
    }
    if args.raw {
        return run_native(args, ctx);
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
/// attribute standing in the parent is invisible to every rule. (Inline
/// `#[cfg(test)]` is still not handled here; rules that care use a
/// `not: inside cfg(test)` clause.)
fn add_exclude_globs(cmd: &mut Command, ctx: &Ctx) {
    let project = &ctx.config;
    for pat in &project.lint_exclude.runtime_paths() {
        cmd.arg("--globs").arg(format!("!{pat}"));
    }
    for pat in cfg_test_module_globs(&ctx.root) {
        cmd.arg("--globs").arg(format!("!{pat}"));
    }
}

fn run_native(args: &AstGrepArgs, ctx: &Ctx) -> Result<()> {
    let mut cmd = Command::new(ctx.config.tools.program("ast-grep"));
    cmd.arg("scan")
        .arg("--config")
        .arg("sgconfig.yml")
        .arg("--report-style")
        .arg("short");
    add_exclude_globs(&mut cmd, ctx);
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

/// Parse ast-grep `--json=stream` output into the per-rule grouping.
fn parse_into(stdout: &str, by_rule: &mut BTreeMap<String, RuleGroup>) {
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
            // ast-grep counts from zero and every reader of this report counts
            // from one. Printing its own numbers sent each hit one line above
            // the code it is about, which is a different statement.
            line: m.range.start.line + 1,
            column: m.range.start.column + 1,
        });
    }
}

fn run_grouped(args: &AstGrepArgs, ctx: &Ctx) -> Result<()> {
    let project = &ctx.config;

    let mut cmd = Command::new(ctx.config.tools.program("ast-grep"));
    cmd.arg("scan")
        .arg("--config")
        .arg("sgconfig.yml")
        .arg("--json=stream");
    add_exclude_globs(&mut cmd, ctx);
    if args.strict {
        cmd.arg("--warning");
    }
    for p in &args.paths {
        cmd.arg(p);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::inherit());

    let output = cmd.output()?;
    let mut by_rule: BTreeMap<String, RuleGroup> = BTreeMap::new();
    parse_into(&String::from_utf8_lossy(&output.stdout), &mut by_rule);
    let mut ok = output.status.success();

    // Second pass: hard-correctness rules that must see tests too. The main
    // scan applied the `[lint_exclude].paths` globs (production-only); re-run
    // each `scan_all` rule standalone with NO exclude globs so its own
    // `files:`/`ignores:` are the only scope, then replace its prod-only group.
    for rule_id in &project.lint_exclude.scan_all_rules {
        let rule_file = format!(".config/ast-grep/{rule_id}.yml");
        let mut rule_cmd = Command::new(ctx.config.tools.program("ast-grep"));
        rule_cmd
            .arg("scan")
            .arg("--rule")
            .arg(&rule_file)
            .arg("--json=stream");
        if args.strict {
            rule_cmd.arg("--warning");
        }
        for p in &args.paths {
            rule_cmd.arg(p);
        }
        rule_cmd.stdout(Stdio::piped());
        rule_cmd.stderr(Stdio::inherit());
        let rule_out = rule_cmd.output()?;
        by_rule.remove(rule_id);
        parse_into(&String::from_utf8_lossy(&rule_out.stdout), &mut by_rule);
        ok = ok && rule_out.status.success();
    }

    print_grouped(&by_rule);

    if !ok {
        let findings = print_failing_rules(&by_rule, args.strict);
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
    column: u32,
    line: u32,
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

    use super::parse_into;

    fn rule_hits_at(rule_file: &str, relative_path: &str, source: &str) -> usize {
        let temp = tempdir().expect("tempdir");
        let source_path = temp.path().join(relative_path);
        fs::create_dir_all(source_path.parent().expect("fixture parent"))
            .expect("create fixture directory");
        fs::write(&source_path, source).expect("write fixture");

        let rule = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.config/ast-grep")
            .join(rule_file);
        let output = Command::new("ast-grep")
            .current_dir(temp.path())
            .args(["scan", "--rule"])
            .arg(rule)
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
    fn a_reported_hit_names_the_line_an_editor_calls_it() {
        let stdout = r#"{"file":"crates/kithara-ui/src/capture/set.rs","message":"m","ruleId":"perf.prefer-primitive-pool","severity":"error","range":{"start":{"line":29,"column":4}}}"#;
        let mut by_rule = BTreeMap::new();
        parse_into(stdout, &mut by_rule);
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

#[cfg(all(feature = "probe", test))]
impl HasPool<f32> for AllCfgFixture {
    fn __slot(&self) -> &PoolSlot<f32> { todo!() }
}

#[cfg(all(feature = "probe", test))]
pool_schema!(pub AllCfgPools { samples: f32 });

#[cfg(test)]
fn fixture() {
    let pools = TestPools::builder(OverallBudget(1024));
    let region = PoolRegion::__build(OverallBudget(1024), |_| Ok(()));
    let values: Vec<f32> = vec![0.0; 8];
}

#[cfg(all(feature = "probe", test))]
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
