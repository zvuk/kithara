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
    checks,
    checks::{Check, Context, registry},
    config::IdiomsConfig,
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
        verdict_cache::VerdictCache,
        violation::{Report, Violation},
    },
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
    /// Print each check's wall time to stderr, slowest first.
    #[arg(long)]
    pub timings: bool,
    #[arg(long = "update-baseline")]
    pub update_baseline: bool,
}

pub(crate) fn run(args: &IdiomsArgs, workspace: &Ctx) -> Result<()> {
    validate(args)?;

    let metadata = workspace.metadata()?;
    let workspace_root = workspace.root.clone();
    let config_dir = workspace_root.join(&args.config_dir);
    let config = IdiomsConfig::load(&config_dir)?;
    let scope = Scope::new(args.crates.clone(), args.paths.clone()).with_metadata(metadata);

    let fix_scan = Scan::new(&workspace_root);
    let ctx = Context {
        workspace_root: &workspace_root,
        metadata,
        config: &config,
        scope: &scope,
        scan: &fix_scan,
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

    let project = &workspace.config;

    let scan = Scan::new(&workspace_root);
    let ctx = Context { scan: &scan, ..ctx };

    let selected: Vec<&dyn Check> = registry
        .iter()
        .map(Box::as_ref)
        .filter(|check| filter.as_ref().is_none_or(|ids| ids.contains(check.id())))
        .collect();
    let ran: Vec<&'static str> = selected.iter().map(|check| check.id()).collect();

    let cache = VerdictCache::open(
        metadata.target_directory.as_std_path(),
        "idioms",
        &std::env::current_exe().context("locate the running linter")?,
        &workspace_root,
        &format!("{config:?}"),
    )?;
    let outcomes = run_checks(&selected, &ctx, &scope, project, &cache)?;

    if args.timings {
        let rows: Vec<_> = ran
            .iter()
            .copied()
            .zip(outcomes.iter().map(|(elapsed, _)| *elapsed))
            .collect();
        report::print_timings("idioms", &rows);
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
            "wrote idioms baseline ({} entry across {} check(s)) to {}",
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
            "idioms ratchet failed: {} regression(s), {} new violation(s)",
            diff.regressions.len(),
            diff.new_violations.len(),
        );
    }
    Ok(())
}

fn apply_common_exclusions(
    report: &mut Report,
    policy: checks::CheckPolicy,
    path_patterns: &[String],
    module_patterns: &[String],
    test_module_paths: &[String],
    workspace_root: &Path,
) {
    if policy.keeps_source_findings() {
        return;
    }
    apply_lint_excludes(
        report,
        path_patterns,
        module_patterns,
        test_module_paths,
        workspace_root,
    );
}

/// Runs each selected check, returning its wall time and its violations in
/// registry order, each check's ordered by key.
///
/// A parsed `syn::File` holds `proc_macro2` spans and is neither `Send` nor
/// `Sync`, so a tree can be neither shared between checks nor moved across a
/// thread; what spreads is the checks themselves. The shared scan means they
/// no longer each re-walk the tree or re-read the same bytes.
///
/// Memory scales with the pool, not the registry: a worker holds one file's
/// tree at a time. Measured with twelve workers the run peaks at 2.0 GB; a
/// host with many more cores than memory sets `RAYON_NUM_THREADS`.
fn run_checks(
    selected: &[&dyn Check],
    ctx: &Context<'_>,
    scope: &Scope,
    project: &ProjectConfig,
    cache: &VerdictCache,
) -> Result<Vec<(Duration, Vec<Violation>)>> {
    let test_module_paths = cfg_test_module_globs(ctx.workspace_root);
    selected
        .par_iter()
        .map(|check| {
            let effective_scope = check.policy().scope(scope);
            let check_ctx = Context {
                scope: &effective_scope,
                ..*ctx
            };
            let started = Instant::now();
            let mut check_report = Report::default();
            check_report.extend(run_one_check(*check, &check_ctx, cache)?);
            apply_common_exclusions(
                &mut check_report,
                check.policy(),
                &project.lint_exclude.runtime_paths(),
                &project.lint_exclude.modules,
                &test_module_paths,
                ctx.workspace_root,
            );
            check_report.violations.sort_by(|a, b| a.key.cmp(&b.key));
            Ok((started.elapsed(), check_report.violations))
        })
        .collect()
}

/// Runs `check` over its scope: file by file through the verdict cache when
/// it declares that its findings about a file depend on that file alone,
/// and over the whole scope at once when it correlates files.
fn run_one_check(
    check: &dyn Check,
    ctx: &Context<'_>,
    cache: &VerdictCache,
) -> Result<Vec<Violation>> {
    if !check.caches_by_file() {
        return check.run(ctx);
    }
    let files = ctx.scan.rs_files(ctx.scope)?;
    cache.verdicts(check.id(), ctx.scan, &files, |scan| {
        check.run(&Context { scan, ..*ctx })
    })
}

fn run_fix(
    registry: &[Box<dyn Check>],
    filter: &Option<HashSet<&str>>,
    ctx: &Context<'_>,
    allow_dirty: bool,
) -> Result<()> {
    crate::util::ensure_clean_tree(ctx.workspace_root, allow_dirty, "xtask lint idioms")?;
    let mut writes = 0;
    let mut changes = Vec::new();
    let mut skipped = Vec::new();
    for check in registry {
        if let Some(filter) = filter
            && !filter.contains(check.id())
        {
            continue;
        }
        let effective_scope = check.policy().scope(ctx.scope);
        let check_ctx = Context {
            scope: &effective_scope,
            ..*ctx
        };
        let outcome = check.fix(&check_ctx)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{scope::Scope, violation::Violation};

    #[test]
    fn derivable_policy_adds_tests_and_xtask_only_to_empty_scope() {
        let root = Path::new("/workspace");
        assert_eq!(Scope::default().roots(root), vec![root.join("crates")]);
        assert_eq!(
            Scope::default().with_workspace_sources().roots(root),
            vec![root.join("crates"), root.join("tests"), root.join("xtask")]
        );
        let explicit = Scope::new(vec!["kithara-queue".into()], vec![]);
        assert_eq!(
            explicit.clone().with_workspace_sources().roots(root),
            explicit.roots(root)
        );
    }

    #[test]
    fn ordinary_checks_drop_cfg_test_findings_but_derivable_checks_keep_them() {
        let dir = tempfile::tempdir().unwrap();
        let source = "#[cfg(test)] mod tests { fn only_test() {} }";
        fs::write(dir.path().join("fixture.rs"), source).unwrap();
        let mut ordinary = Report::default();
        ordinary.extend([Violation::deny("ordinary", "fixture.rs:1:0", "test")]);
        apply_common_exclusions(
            &mut ordinary,
            checks::CheckPolicy::Default,
            &[],
            &[],
            &[],
            dir.path(),
        );
        assert!(ordinary.violations.is_empty());

        let mut derivable = Report::default();
        derivable.extend([Violation::deny(
            "derivable_display",
            "fixture.rs:1:0",
            "test",
        )]);
        apply_common_exclusions(
            &mut derivable,
            checks::CheckPolicy::WorkspaceSources,
            &[],
            &[],
            &[],
            dir.path(),
        );
        assert_eq!(derivable.violations.len(), 1);
    }

    /// Reports each file of its scope by its path, in key order, as a check
    /// that sorts its findings does.
    struct EveryFile;

    impl Check for EveryFile {
        fn id(&self) -> &'static str {
            "every_file"
        }

        fn run(&self, ctx: &Context<'_>) -> Result<Vec<Violation>> {
            let mut found: Vec<Violation> = ctx
                .scan
                .rs_files(ctx.scope)?
                .iter()
                .map(|path| {
                    let key = crate::common::walker::relative_to(ctx.workspace_root, path)
                        .to_string_lossy()
                        .into_owned();
                    Violation::warn(self.id(), key, "seen")
                })
                .collect();
            found.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(found)
        }
    }

    /// A module file sorts before its directory's files by key and after
    /// them by path, so a check judged file by file meets its files in
    /// another order than the one it reports in over the whole scope.
    #[test]
    fn a_check_judged_file_by_file_reports_as_it_does_over_the_whole_scope() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("crates").join("one").join("src");
        fs::create_dir_all(src.join("a")).unwrap();
        fs::write(src.join("a.rs"), "").unwrap();
        fs::write(src.join("a").join("b.rs"), "").unwrap();
        let metadata = MetadataCommand::new().exec().unwrap();
        let config = IdiomsConfig::default();
        let scope = Scope::default();
        let scan = Scan::new(dir.path());
        let ctx = Context {
            config: &config,
            metadata: &metadata,
            workspace_root: dir.path(),
            scan: &scan,
            scope: &scope,
        };
        let cache = VerdictCache::open(
            &dir.path().join("target"),
            "idioms",
            &std::env::current_exe().unwrap(),
            dir.path(),
            "",
        )
        .unwrap();
        let whole: Vec<String> = EveryFile
            .run(&ctx)
            .unwrap()
            .into_iter()
            .map(|violation| violation.key)
            .collect();

        for run in ["cold", "warm"] {
            let keys: Vec<String> = run_checks(
                &[&EveryFile],
                &ctx,
                &scope,
                &ProjectConfig::default(),
                &cache,
            )
            .unwrap()
            .into_iter()
            .flat_map(|(_, violations)| violations)
            .map(|violation| violation.key)
            .collect();
            assert_eq!(keys, whole, "{run}");
        }
    }

    /// Findings as sorted key and message pairs.
    type Findings = Vec<(String, String)>;

    /// What the driver reports for `check` over the tree at `root`, and what
    /// the check finds over its whole scope there.
    fn driver_and_whole(
        check: &dyn Check,
        root: &Path,
        cache: &VerdictCache,
    ) -> (Findings, Findings) {
        let metadata = MetadataCommand::new()
            .manifest_path(root.join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap();
        let mut config = IdiomsConfig::default();
        config.thresholds.derivable_event.enabled = true;
        config.thresholds.derivable_event.owner_path = Some("crates/kithara-events".into());
        let scope = Scope::default().with_metadata(&metadata);
        let scan = Scan::new(root);
        let ctx = Context {
            config: &config,
            metadata: &metadata,
            workspace_root: root,
            scan: &scan,
            scope: &scope,
        };
        let findings = |violations: Vec<Violation>| {
            let mut pairs: Findings = violations
                .into_iter()
                .map(|violation| (violation.key, violation.message))
                .collect();
            pairs.sort();
            pairs
        };
        let driver = findings(
            run_checks(&[check], &ctx, &scope, &ProjectConfig::default(), cache)
                .unwrap()
                .into_iter()
                .flat_map(|(_, violations)| violations)
                .collect(),
        );
        let effective = check.policy().scope(&scope);
        let whole = findings(
            check
                .run(&Context {
                    scope: &effective,
                    ..ctx
                })
                .unwrap(),
        );
        (driver, whole)
    }

    /// The driver judges `files`, then `neighbour` is written beside them,
    /// and the driver must report what the check then finds over its whole
    /// scope. The neighbour changes those findings, or agreeing would prove
    /// nothing; a verdict kept per file for an unchanged file would miss it.
    fn the_driver_follows_a_neighbour(
        check: &dyn Check,
        files: &[(&str, &str)],
        neighbour: (&str, &str),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let write = |(rel, source): (&str, &str)| {
            let path = dir.path().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        };
        write((
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/one\"]\nresolver = \"2\"\n",
        ));
        write((
            "crates/one/Cargo.toml",
            "[package]\nname = \"one\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        ));
        write(("crates/one/src/lib.rs", ""));
        files.iter().copied().for_each(write);
        let cache = VerdictCache::open(
            &dir.path().join("target"),
            "idioms",
            &std::env::current_exe().unwrap(),
            dir.path(),
            "",
        )
        .unwrap();
        let (_, before) = driver_and_whole(check, dir.path(), &cache);

        write(neighbour);
        let (driver, after) = driver_and_whole(check, dir.path(), &cache);

        assert_ne!(before, after, "the neighbour changes nothing");
        assert_eq!(driver, after, "{}", check.id());
    }

    #[test]
    fn a_rust_only_event_needs_no_ffi_forwarding_and_manual_impls_stay_denied() {
        use checks::derivable_event::DerivableEvent;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("crates/one/src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/one\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        fs::write(
            src.parent().unwrap().join("Cargo.toml"),
            "[package]\nname = \"one\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            src.join("lib.rs"),
            "#[derive(Clone, Debug, Event)] pub struct Internal;\n",
        )
        .unwrap();
        let cache = VerdictCache::open(
            &dir.path().join("target"),
            "idioms",
            &std::env::current_exe().unwrap(),
            dir.path(),
            "",
        )
        .unwrap();

        for run in ["cold", "warm"] {
            let (driver, whole) = driver_and_whole(&DerivableEvent, dir.path(), &cache);
            assert!(whole.is_empty(), "{run}: {whole:?}");
            assert_eq!(driver, whole, "{run}");
        }

        let ffi = dir.path().join("crates/kithara-ffi/src");
        fs::create_dir_all(&ffi).unwrap();
        fs::write(
            ffi.join("lib.rs"),
            "#[derive(EventSet)] pub enum All { Internal(Internal) }\n",
        )
        .unwrap();
        let (driver, whole) = driver_and_whole(&DerivableEvent, dir.path(), &cache);
        assert!(whole.is_empty(), "{whole:?}");
        assert_eq!(driver, whole);

        fs::write(
            src.join("lib.rs"),
            "#[derive(Clone, Debug)] pub struct Internal;\nimpl Event for Internal {}\n",
        )
        .unwrap();
        let (driver, whole) = driver_and_whole(&DerivableEvent, dir.path(), &cache);
        assert_eq!(
            whole,
            vec![(
                "crates/one/src/lib.rs::Internal".into(),
                "Internal implements Event by hand outside the configured Event owner".into(),
            )]
        );
        assert_eq!(driver, whole);
    }

    #[test]
    fn an_event_verdict_is_cached_for_the_source_snapshot_it_judged() {
        use checks::derivable_event::DerivableEvent;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("crates/one/src/lib.rs");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        let manual = "#[derive(Clone, Debug)] pub struct Internal;\nimpl Event for Internal {}\n";
        fs::write(&src, manual).unwrap();
        let metadata = MetadataCommand::new().exec().unwrap();
        let mut config = IdiomsConfig::default();
        config.thresholds.derivable_event.enabled = true;
        config.thresholds.derivable_event.owner_path = Some("crates/kithara-events".into());
        let scope = Scope::default();
        let scan = Scan::new(dir.path());
        assert_eq!(scan.source(&src).unwrap().as_str(), manual);
        let ctx = Context {
            config: &config,
            metadata: &metadata,
            workspace_root: dir.path(),
            scan: &scan,
            scope: &scope,
        };
        let cache = VerdictCache::open(
            &dir.path().join("target"),
            "idioms",
            &std::env::current_exe().unwrap(),
            dir.path(),
            "",
        )
        .unwrap();

        fs::write(
            &src,
            "#[derive(Clone, Debug, Event)] pub struct Internal;\n",
        )
        .unwrap();
        let snapshot = run_one_check(&DerivableEvent, &ctx, &cache).unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].key, "crates/one/src/lib.rs::Internal");

        fs::write(&src, manual).unwrap();
        let scan = Scan::new(dir.path());
        let restored =
            run_one_check(&DerivableEvent, &Context { scan: &scan, ..ctx }, &cache).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].key, snapshot[0].key);
        assert_eq!(restored[0].message, snapshot[0].message);
    }

    /// A getter is redundant when its declaration's public field already
    /// exposes it, even when that declaration lives in another module.
    #[test]
    fn a_getter_is_judged_with_the_fields_that_already_expose_it() {
        use checks::derivable_getter::DerivableGetter;

        the_driver_follows_a_neighbour(
            &DerivableGetter,
            &[
                ("crates/one/src/lib.rs", "mod a;\nmod b;\n"),
                (
                    "crates/one/src/a.rs",
                    "use crate::b::User;\nimpl User { pub fn name(&self) -> &str { &self.name } }\n",
                ),
                (
                    "crates/one/src/b.rs",
                    "pub struct User {\n    pub(super) name: String,\n}\n",
                ),
            ],
            (
                "crates/one/src/b.rs",
                "pub struct User {\n    pub name: String,\n}\n",
            ),
        );
    }

    /// A wrapper's economy counts the calls and the namesakes across its
    /// crate, so a second function of its name elsewhere makes it unknown.
    #[test]
    fn a_wrapper_is_judged_with_the_rest_of_its_crate() {
        use checks::thin_wrapper_economy::ThinWrapperEconomy;

        let calls = (0..7)
            .map(|index| {
                format!(
                    "    fn call_{index}() -> Result<(), DecodeError> {{\n        Err(channel_disconnected(\"output\"))\n    }}\n"
                )
            })
            .collect::<String>();
        let wrapper = "fn channel_disconnected(channel: &'static str) -> DecodeError {\n    DecodeError::backend(WebCodecsError::ChannelDisconnected { channel })\n}\n";
        the_driver_follows_a_neighbour(
            &ThinWrapperEconomy,
            &[
                // rustfmt measures what the wrapper saves under the
                // workspace's own configuration.
                ("rustfmt.toml", ""),
                (
                    "crates/one/src/a.rs",
                    &format!("{wrapper}\nstruct Codec;\n\nimpl Codec {{\n{calls}}}\n"),
                ),
            ],
            ("crates/one/src/b.rs", wrapper),
        );
    }

    #[test]
    fn event_policy_stays_production_only() {
        use checks::{Check, derivable_event::DerivableEvent};
        let event = DerivableEvent;
        assert_eq!(event.policy(), checks::CheckPolicy::Default);
    }
}
