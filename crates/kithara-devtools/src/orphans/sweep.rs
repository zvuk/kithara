use std::{
    collections::{BTreeMap, HashSet},
    fs,
    num::NonZero,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

use anyhow::{Result, anyhow, bail};
use cargo_metadata::{Metadata, MetadataCommand};
use clap::Args;

use super::declared::{declared_files, normalize};
use crate::{Ctx, util::check_tool};

struct Consts;
impl Consts {
    const INSTALL_HINT: &'static str = "cargo install cargo-modules";
    const MARKER: &'static str = "orphaned module `";
    /// A cgroup v2 job container reports its own cap here; a machine without
    /// one has no such file and is bounded by its cores alone.
    const MEMORY_MAX: &'static str = "/sys/fs/cgroup/memory.max";
    /// What one `cargo modules` run needs. It loads the whole workspace into a
    /// rust-analyzer database and peaked at three gibibytes on this one, so
    /// the budget is that measurement with room for the workspace to grow.
    const WORKER_MEMORY: u64 = 3584 * 1024 * 1024;
}

#[derive(Debug, Args)]
pub struct OrphansArgs {
    /// Limit to specific packages. Repeatable. Empty = all non-excluded
    /// workspace packages.
    #[arg(long = "package", short = 'p', value_name = "NAME")]
    pub packages: Vec<String>,
    /// In audit mode, skip the run when no packages are given (avoids
    /// the ~90s workspace sweep on every pre-commit `just ci audit`).
    /// `just arch orphans` and `just ci health` leave this off.
    #[arg(long = "audit-mode")]
    pub audit_mode: bool,
    /// Treat orphans as a hard failure (exit non-zero). Without it the
    /// run is advisory: orphans are printed but exit is success.
    #[arg(long)]
    pub deny: bool,
}

/// One target of one package. `cargo modules` analyzes a single target per
/// run, so a package with a library and binaries needs one run per target.
struct Job {
    src: PathBuf,
    label: String,
    package: String,
    selector: Vec<String>,
}

/// A module the tool reported, at the file it named.
#[derive(Clone)]
struct Finding {
    path: PathBuf,
    module: String,
}

/// One target's sweep once the source has been consulted.
struct Report {
    failure: Option<String>,
    label: String,
    package: String,
    declared: Vec<PathBuf>,
    orphans: Vec<Finding>,
}

impl Report {
    fn failed(job: &Job, failure: String) -> Self {
        Self {
            package: job.package.clone(),
            label: job.label.clone(),
            orphans: Vec::new(),
            declared: Vec::new(),
            failure: Some(failure),
        }
    }
}

/// A package's targets taken together.
#[derive(Default)]
struct Merged {
    declared: HashSet<PathBuf>,
    failures: Vec<String>,
    orphans: Vec<Finding>,
}

pub(crate) fn run(args: &OrphansArgs, ctx: &Ctx) -> Result<()> {
    let program: Arc<str> = Arc::from(ctx.config.tools.program("cargo-modules"));
    check_tool(
        &program,
        &["modules", "--version"],
        ctx.config
            .tools
            .install_hint("cargo-modules", Consts::INSTALL_HINT),
    )?;

    if args.packages.is_empty() && args.audit_mode {
        println!(
            "orphans: workspace-wide run skipped in audit mode \
             (run `just arch orphans` or `just ci health` for full sweep)"
        );
        return Ok(());
    }

    let metadata = MetadataCommand::new().no_deps().exec()?;
    let root = Arc::new(metadata.workspace_root.clone().into_std_path_buf());
    let (jobs, skipped) = scope(
        &metadata,
        &args.packages,
        &ctx.config.orphans.exclude_packages,
    );

    if !skipped.is_empty() {
        println!(
            "orphans: not checked, excluded by configuration: {}",
            skipped.join(", ")
        );
    }
    if jobs.is_empty() {
        println!("orphans: no targets in scope (skipped)");
        return Ok(());
    }

    let cpus = thread::available_parallelism().map_or(1, NonZero::get);
    let memory = memory_limit();
    let parallelism = workers(cpus, memory, ctx.config.orphans.max_parallelism);
    let cap = memory.map_or_else(
        || "no memory cap".to_owned(),
        |bytes| format!("{} MiB memory cap", bytes / (1024 * 1024)),
    );
    println!(
        "orphans: checking {} target(s) with {parallelism}-way parallelism \
         ({cpus} core(s), {cap})",
        jobs.len()
    );

    let queue = Arc::new(Mutex::new(jobs));
    let reports = Arc::new(Mutex::new(Vec::<Report>::new()));
    let mut handles = Vec::with_capacity(parallelism);

    for _ in 0..parallelism {
        let queue = Arc::clone(&queue);
        let reports = Arc::clone(&reports);
        let root = Arc::clone(&root);
        let program = Arc::clone(&program);
        handles.push(thread::spawn(move || {
            loop {
                let job = {
                    let mut queue = queue.lock().expect("BUG: orphans queue mutex poisoned");
                    queue.pop()
                };
                let Some(job) = job else { break };
                let report = examine(&job, &root, &program);
                reports
                    .lock()
                    .expect("BUG: orphans reports mutex poisoned")
                    .push(report);
            }
        }));
    }

    for handle in handles {
        let _ = handle.join();
    }

    let reports = Arc::try_unwrap(reports)
        .map_err(|_| anyhow!("BUG: orphans reports outlived the sweep"))?
        .into_inner()
        .expect("BUG: orphans reports mutex poisoned");

    verdict(&merge(reports), &root, args.deny)
}

/// How many runs the job has room for. A fixed count answers to the machine
/// the sweep was written on, and a CI container holds fewer rust-analyzer
/// databases than that: the kernel kills the job before it reports anything.
fn workers(cpus: usize, memory: Option<u64>, max_parallelism: usize) -> usize {
    let by_memory = memory.map_or(max_parallelism, |bytes| {
        usize::try_from(bytes / Consts::WORKER_MEMORY).unwrap_or(max_parallelism)
    });
    cpus.min(by_memory).clamp(1, max_parallelism)
}

/// What to report when a run fails. A tool that explained itself is quoted as
/// it stands; one that ended without a word is named by how it ended.
fn failure(program: &str, status: &str, text: &str) -> String {
    if text.trim().is_empty() {
        return format!("{program} ended with {status} and produced no output");
    }
    text.to_owned()
}

/// The cap this job runs under, when it runs under one. `memory.max` reads
/// `max` on an unbounded cgroup, which is not a size and leaves the count to
/// the cores.
fn memory_limit() -> Option<u64> {
    fs::read_to_string(Consts::MEMORY_MAX)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Folds a package's targets into one verdict. A file both the library and a
/// binary reach is one module, not two findings.
fn merge(reports: Vec<Report>) -> BTreeMap<String, Merged> {
    let mut merged: BTreeMap<String, Merged> = BTreeMap::new();
    for report in reports {
        let entry = merged.entry(report.package).or_default();
        if let Some(failure) = report.failure {
            entry
                .failures
                .push(format!("{}: {}", report.label, failure.trim_end()));
        }
        entry.declared.extend(report.declared);
        for finding in report.orphans {
            if !entry.orphans.iter().any(|seen| seen.path == finding.path) {
                entry.orphans.push(finding);
            }
        }
    }
    // A module some other target of the same package loads is not an orphan of
    // the package, whichever target reported it.
    for entry in merged.values_mut() {
        entry
            .orphans
            .retain(|finding| !entry.declared.contains(&finding.path));
    }
    merged
}

fn verdict(reports: &BTreeMap<String, Merged>, root: &Path, deny: bool) -> Result<()> {
    let mut broken = Vec::new();
    let mut orphaned = Vec::new();
    for (package, report) in reports {
        for failure in &report.failures {
            println!("orphans: {package} could not be checked\n{failure}");
        }
        if !report.failures.is_empty() {
            broken.push(package.clone());
        }
        // Stated rather than dropped: the filter is the reason this sweep can
        // be green, so what it removed has to stay visible.
        if !report.declared.is_empty() {
            println!(
                "orphans: {package}: {} module(s) the source declares behind a `cfg` \
                 or a `#[path]`, loaded by some other build",
                report.declared.len()
            );
        }
        for finding in &report.orphans {
            println!(
                "orphans: {package}: `{}` at {} is named by no `mod` declaration",
                finding.module,
                relative(&finding.path, root)
            );
        }
        if !report.orphans.is_empty() {
            orphaned.push(package.clone());
        }
    }

    if !broken.is_empty() {
        bail!(
            "orphans: {} package(s) could not be checked: {}",
            broken.len(),
            broken.join(", ")
        );
    }
    if orphaned.is_empty() {
        println!("orphans: no orphans");
        return Ok(());
    }
    if deny {
        bail!(
            "orphans: {} package(s) reported orphans: {}",
            orphaned.len(),
            orphaned.join(", ")
        );
    }
    println!(
        "orphans: {} package(s) reported orphans: {} (advisory; pass --deny to fail)",
        orphaned.len(),
        orphaned.join(", ")
    );
    Ok(())
}

/// Asks the tool, then asks the source. A module the tool cannot resolve in
/// this configuration is only an orphan when nothing declares it.
fn examine(job: &Job, root: &Path, program: &str) -> Report {
    let output = Command::new(program)
        .args(["modules", "orphans", "--cfg-test"])
        .args(&job.selector)
        .args(["--package", &job.package])
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) => return Report::failed(job, format!("{program} did not start: {err}")),
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let found = findings(&text, root);
    if found.is_empty() && !output.status.success() {
        return Report::failed(job, failure(program, &output.status.to_string(), &text));
    }
    let declared = match declared_files(&job.src) {
        Ok(declared) => declared,
        Err(err) => return Report::failed(job, format!("{err:#}")),
    };
    let (orphans, filtered) = split(found, &declared);
    Report {
        orphans,
        package: job.package.clone(),
        label: job.label.clone(),
        declared: filtered,
        failure: None,
    }
}

/// Separates what the source cannot account for from what it can.
fn split(found: Vec<Finding>, declared: &HashSet<PathBuf>) -> (Vec<Finding>, Vec<PathBuf>) {
    let mut orphans = Vec::new();
    let mut filtered = Vec::new();
    for finding in found {
        if declared.contains(&finding.path) {
            filtered.push(finding.path);
        } else {
            orphans.push(finding);
        }
    }
    (orphans, filtered)
}

fn findings(text: &str, root: &Path) -> Vec<Finding> {
    text.lines()
        .filter_map(|line| finding(line, root))
        .collect()
}

fn finding(line: &str, root: &Path) -> Option<Finding> {
    let (_, rest) = line.split_once(Consts::MARKER)?;
    let (module, rest) = rest.split_once('`')?;
    let (_, path) = rest.split_once(" at ")?;
    // Colour codes survive a piped run on some terminals; the escape is never
    // part of a module name or a path.
    let path = Path::new(path.split('\u{1b}').next().unwrap_or(path).trim());
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    Some(Finding {
        module: module.split('\u{1b}').next().unwrap_or(module).to_owned(),
        path: normalize(&path),
    })
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// `cargo modules` offers no selector beyond `--lib` and `--bin`, so those are
/// the targets a sweep can reach. A package without a library is swept through
/// its binaries rather than dropped.
fn scope(metadata: &Metadata, wanted: &[String], excluded: &[String]) -> (Vec<Job>, Vec<String>) {
    let mut jobs = Vec::new();
    let mut skipped = Vec::new();
    for package in metadata.workspace_packages() {
        let name = package.name.to_string();
        if !wanted.is_empty() && !wanted.contains(&name) {
            continue;
        }
        if excluded.contains(&name) {
            skipped.push(name);
            continue;
        }
        let Some(dir) = package.manifest_path.parent() else {
            continue;
        };
        let src = dir.as_std_path().join("src");
        for target in &package.targets {
            let selector = if target.is_bin() {
                Some((
                    format!("bin {}", target.name),
                    vec!["--bin".to_owned(), target.name.clone()],
                ))
            } else if is_library(target) {
                Some(("lib".to_owned(), vec!["--lib".to_owned()]))
            } else {
                None
            };
            let Some((label, selector)) = selector else {
                continue;
            };
            jobs.push(Job {
                label,
                selector,
                package: name.clone(),
                src: src.clone(),
            });
        }
    }
    jobs.sort_by(|left, right| (&left.package, &left.label).cmp(&(&right.package, &right.label)));
    skipped.sort();
    (jobs, skipped)
}

fn is_library(target: &cargo_metadata::Target) -> bool {
    !target.is_bin()
        && !target.is_example()
        && !target.is_test()
        && !target.is_bench()
        && !target.is_custom_build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::project::ProjectConfig;

    fn parse(line: &str) -> Option<Finding> {
        finding(line, Path::new("/workspace"))
    }

    fn found(line: &str) -> Vec<Finding> {
        findings(&format!("{line}\n"), Path::new("/workspace"))
    }

    #[test]
    fn a_reported_module_is_read_from_the_tool_line() {
        let found = parse("warning: orphaned module `apple` at crates/x/src/apple/mod.rs")
            .expect("finding");

        assert_eq!(found.module, "apple");
    }

    #[test]
    fn a_reported_path_is_anchored_at_the_workspace_root() {
        let found = parse("warning: orphaned module `apple` at crates/x/src/apple/mod.rs")
            .expect("finding");

        assert_eq!(
            found.path,
            Path::new("/workspace/crates/x/src/apple/mod.rs")
        );
    }

    #[test]
    fn an_unrelated_line_is_not_a_finding() {
        assert!(parse("   |  mod tests;").is_none());
    }

    /// The whole point of the sweep is the file nothing loads, so a path no
    /// declaration names has to survive the filter.
    #[test]
    fn a_finding_the_source_does_not_declare_survives() {
        let (orphans, _) = split(
            found("warning: orphaned module `stray` at crates/x/src/stray.rs"),
            &HashSet::new(),
        );

        assert_eq!(orphans.len(), 1);
    }

    #[test]
    fn a_finding_the_source_declares_is_filtered_out() {
        let declared = HashSet::from([PathBuf::from("/workspace/crates/x/src/android/mod.rs")]);

        let (orphans, _) = split(
            found("warning: orphaned module `android` at crates/x/src/android/mod.rs"),
            &declared,
        );

        assert!(orphans.is_empty());
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A worker the kernel kills leaves nothing behind, and a package reported
    /// as unchecked without a reason reads as a broken tool rather than as a
    /// job that ran out of room.
    #[test]
    fn a_run_that_said_nothing_reports_how_it_ended() {
        assert!(
            failure("cargo-modules", "signal: 9 (SIGKILL)", "").contains("signal: 9 (SIGKILL)")
        );
    }

    /// The program is configurable, so a message naming `cargo-modules` while
    /// something else ran would send the reader to a binary that never started.
    /// Seeded off the role name, which a message that ignored its argument
    /// could still print.
    #[test]
    fn a_run_that_said_nothing_names_the_program_that_ran() {
        assert!(
            failure("/opt/pinned/bin/cargo-modules", "exit status: 1", "")
                .contains("/opt/pinned/bin/cargo-modules")
        );
    }

    #[test]
    fn a_run_that_explained_itself_keeps_its_own_words() {
        assert_eq!(
            failure("cargo-modules", "exit status: 1", "error: no such package"),
            "error: no such package"
        );
    }

    /// The CI container: three cores and eight gibibytes. Four runs of a tool
    /// that holds three apiece is what the kernel killed the sweep for.
    #[test]
    fn a_capped_container_runs_what_its_memory_holds() {
        assert_eq!(
            workers(
                3,
                Some(8 * GIB),
                ProjectConfig::default().orphans.max_parallelism
            ),
            2
        );
    }

    #[test]
    fn a_machine_without_a_cap_runs_the_maximum() {
        let max_parallelism = ProjectConfig::default().orphans.max_parallelism;
        assert_eq!(workers(16, None, max_parallelism), max_parallelism);
    }

    /// A cap too small for even one run still has to sweep, one at a time.
    #[test]
    fn a_cap_below_one_run_still_runs_one() {
        assert_eq!(
            workers(
                3,
                Some(GIB),
                ProjectConfig::default().orphans.max_parallelism
            ),
            1
        );
    }

    #[test]
    fn fewer_cores_than_memory_allows_run_one_per_core() {
        assert_eq!(
            workers(
                2,
                Some(64 * GIB),
                ProjectConfig::default().orphans.max_parallelism
            ),
            2
        );
    }

    fn report(label: &str, orphans: Vec<Finding>, declared: Vec<PathBuf>) -> Report {
        Report {
            orphans,
            declared,
            package: "x".to_owned(),
            label: label.to_owned(),
            failure: None,
        }
    }

    /// A library and a binary of one package see the same tree, so the same
    /// file reported twice is one finding.
    #[test]
    fn two_targets_reporting_one_file_yield_one_finding() {
        let finding = found("warning: orphaned module `stray` at crates/x/src/stray.rs");

        let merged = merge(vec![
            report("lib", finding.clone(), Vec::new()),
            report("bin x", finding, Vec::new()),
        ]);

        assert_eq!(merged["x"].orphans.len(), 1);
    }

    /// The binary loads what the library cannot see, so the package has no
    /// orphan even though one target reported one.
    #[test]
    fn a_file_another_target_declares_is_not_an_orphan_of_the_package() {
        let stray = PathBuf::from("/workspace/crates/x/src/stray.rs");

        let merged = merge(vec![
            report(
                "lib",
                found("warning: orphaned module `stray` at crates/x/src/stray.rs"),
                Vec::new(),
            ),
            report("bin x", Vec::new(), vec![stray]),
        ]);

        assert!(merged["x"].orphans.is_empty());
    }
}
