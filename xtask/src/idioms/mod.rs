//! Idiomatic-construction fitness functions for the workspace.
//!
//! Run via `cargo xtask idioms`. Same shape as `arch` and `style`: declarative
//! rules from `.config/idioms/*.toml`, ratchet baseline at
//! `.config/idioms/baseline.toml`. The namespace flags constructions that
//! compile and pass clippy but suggest a better Rust pattern (performance,
//! readability, expressivity).

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fs,
    ops::RangeInclusive,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use cargo_metadata::MetadataCommand;
use clap::Args;
use syn::{ImplItem, Item, Meta, spanned::Spanned};

mod checks;
mod config;

use checks::{Context, registry};
use config::IdiomsConfig;

use crate::common::{
    baseline::{Baseline, RatchetDiff},
    parse::parse_file,
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
    apply_cfg_test_exclusion(&mut report, &workspace_root);

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

/// Extract the 1-based source line a violation key points at. The line is the
/// first `:`-separated field after the `.rs` path portion, e.g. `561` in
/// `crates/x/src/foo.rs:561:for_body`. Returns `None` for keys that carry no
/// line (e.g. `file.rs::Name`).
fn key_line(key: &str) -> Option<usize> {
    let path = key_path(key);
    let rest = key.get(path.len()..)?.strip_prefix(':')?;
    let field = rest.split(':').next()?;
    field.parse::<usize>().ok()
}

/// Drop violations that land on a line inside a `#[cfg(test)]`-predicated item
/// (`mod tests { ... }`, a test `fn`, a test `impl`, ...). Complements the
/// path-glob `exclude_paths` pass: inline test modules in production `src/*.rs`
/// files are test code but are not matched by path globs. Only `test`-keyed
/// cfgs count — `#[cfg(feature = ...)]` and other cfgs are left untouched.
/// Files that fail to parse are skipped (their violations are kept).
fn apply_cfg_test_exclusion(report: &mut Report, workspace_root: &Path) {
    let mut files: BTreeSet<String> = BTreeSet::new();
    for v in &report.violations {
        files.insert(key_path(&v.key).to_string());
    }

    let ranges: BTreeMap<String, Vec<RangeInclusive<usize>>> = files
        .into_iter()
        .filter_map(|rel| {
            let file = parse_file(&workspace_root.join(&rel)).ok()?;
            let mut out = Vec::new();
            collect_cfg_test_ranges(&file.items, &mut out);
            (!out.is_empty()).then_some((rel, out))
        })
        .collect();

    report.violations.retain(|v| {
        let Some(line) = key_line(&v.key) else {
            return true;
        };
        ranges
            .get(key_path(&v.key))
            .is_none_or(|rs| !rs.iter().any(|r| r.contains(&line)))
    });
}

/// Walk items recursively, recording the line range of every item whose
/// attributes carry a `test`-predicated cfg. Recurses into module bodies so
/// nested `#[cfg(test)] mod`/`fn` are caught regardless of depth.
fn collect_cfg_test_ranges(items: &[Item], out: &mut Vec<RangeInclusive<usize>>) {
    for item in items {
        if attrs_have_cfg_test(item_attrs(item)) {
            out.push(item.span().start().line..=item.span().end().line);
        }
        if let Item::Mod(m) = item
            && let Some((_, inner)) = &m.content
        {
            collect_cfg_test_ranges(inner, out);
        }
        if let Item::Impl(im) = item {
            for it in &im.items {
                if let ImplItem::Fn(f) = it
                    && attrs_have_cfg_test(&f.attrs)
                {
                    out.push(f.span().start().line..=f.span().end().line);
                }
            }
        }
    }
}

fn item_attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

/// True when any `#[cfg(...)]` attribute is keyed on `test` — either directly
/// (`#[cfg(test)]`) or as a bare predicate inside `any(...)` / `all(...)`
/// (`#[cfg(any(test, feature = "x"))]`). `#[cfg(not(test))]`, `feature = "test"`,
/// and non-cfg attributes are intentionally not matched.
fn attrs_have_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| match &a.meta {
        Meta::List(list) if list.path.is_ident("cfg") => meta_tokens_have_test(&list.tokens),
        _ => false,
    })
}

fn meta_tokens_have_test(tokens: &proc_macro2::TokenStream) -> bool {
    syn::parse2::<Meta>(tokens.clone()).is_ok_and(|m| cfg_predicate_has_test(&m))
}

fn cfg_predicate_has_test(meta: &Meta) -> bool {
    match meta {
        Meta::Path(p) => p.is_ident("test"),
        Meta::List(list) if list.path.is_ident("any") || list.path.is_ident("all") => list
            .parse_args_with(syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated)
            .is_ok_and(|nested| nested.iter().any(cfg_predicate_has_test)),
        _ => false,
    }
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

    #[test]
    fn key_line_reads_first_field_after_path() {
        assert_eq!(key_line("crates/x/src/foo.rs:561:for_body"), Some(561));
        assert_eq!(key_line("crates/x/src/foo.rs:7:8"), Some(7));
        assert_eq!(key_line("crates/x/src/foo.rs:173:48::outcome"), Some(173));
        assert_eq!(key_line("crates/x/src/foo.rs::Name"), None);
        assert_eq!(key_line("kithara-foo"), None);
    }

    fn ranges(src: &str) -> Vec<(usize, usize)> {
        let file: syn::File = syn::parse_str(src).expect("valid Rust source");
        let mut out = Vec::new();
        collect_cfg_test_ranges(&file.items, &mut out);
        out.iter().map(|r| (*r.start(), *r.end())).collect()
    }

    fn line_in_any(line: usize, rs: &[(usize, usize)]) -> bool {
        rs.iter().any(|(s, e)| (*s..=*e).contains(&line))
    }

    #[test]
    fn cfg_test_mod_range_excludes_inner_lines() {
        let src = "fn prod() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       fn a() {}\n\
                       fn b() {}\n\
                   }\n";
        let rs = ranges(src);
        assert_eq!(rs.len(), 1);
        // production `fn prod` on line 1 is not covered.
        assert!(!line_in_any(1, &rs));
        // lines 2..=6 (the cfg(test) attr + mod body) are covered.
        assert!(line_in_any(4, &rs));
        assert!(line_in_any(5, &rs));
    }

    #[test]
    fn cfg_any_test_mod_is_excluded() {
        let src = "#[cfg(any(test, feature = \"x\"))]\n\
                   mod tests {\n\
                       fn a() {}\n\
                   }\n";
        let rs = ranges(src);
        assert_eq!(rs.len(), 1);
        assert!(line_in_any(3, &rs));
    }

    #[test]
    fn cfg_all_test_mod_is_excluded() {
        let src = "#[cfg(all(test, feature = \"x\"))]\n\
                   mod tests {\n\
                       fn a() {}\n\
                   }\n";
        let rs = ranges(src);
        assert_eq!(rs.len(), 1);
        assert!(line_in_any(3, &rs));
    }

    #[test]
    fn cfg_test_fn_and_impl_are_excluded() {
        let src = "#[cfg(test)]\n\
                   fn helper() {}\n\
                   struct S;\n\
                   #[cfg(test)]\n\
                   impl S {\n\
                       fn t(&self) {}\n\
                   }\n";
        let rs = ranges(src);
        assert_eq!(rs.len(), 2);
        assert!(line_in_any(2, &rs));
        assert!(line_in_any(6, &rs));
    }

    #[test]
    fn non_test_cfg_blocks_are_not_excluded() {
        let src = "#[cfg(feature = \"probe\")]\n\
                   mod probes {\n\
                       fn a() {}\n\
                   }\n\
                   #[cfg(not(test))]\n\
                   fn prod_only() {}\n";
        assert!(ranges(src).is_empty());
    }

    #[test]
    fn apply_cfg_test_exclusion_filters_inline_test_module() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rel = "crates/x/src/foo.rs";
        let src = "fn prod() {\n\
                   \x20\x20\x20\x20for _ in 0..1 {}\n\
                   }\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       fn unit() {\n\
                       \x20\x20\x20\x20for _ in 0..1 {}\n\
                       }\n\
                   }\n";
        let path = tmp.path().join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, src).expect("write fixture");

        let mut report = Report::default();
        report.extend([
            // production loop on line 2 — must stay.
            Violation::warn("fat_loop_body", format!("{rel}:2:for_body"), "prod"),
            // test loop on line 7 (inside cfg(test) mod) — must drop.
            Violation::warn("fat_loop_body", format!("{rel}:7:for_body"), "test"),
        ]);

        apply_cfg_test_exclusion(&mut report, tmp.path());

        let keys: Vec<&str> = report.violations.iter().map(|v| v.key.as_str()).collect();
        assert_eq!(keys, [format!("{rel}:2:for_body")]);
    }

    #[test]
    fn apply_cfg_test_exclusion_keeps_unparsable_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rel = "crates/x/src/broken.rs";
        let path = tmp.path().join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, "this is not valid rust ::: {{{").expect("write fixture");

        let mut report = Report::default();
        report.extend([Violation::warn(
            "fat_loop_body",
            format!("{rel}:3:for_body"),
            "kept",
        )]);

        apply_cfg_test_exclusion(&mut report, tmp.path());
        assert_eq!(report.violations.len(), 1);
    }
}
