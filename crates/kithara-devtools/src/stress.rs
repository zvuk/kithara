//! Portable repeated-test runs and independent evidence verification.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus},
};

use anyhow::{Context, Error, Result, ensure};
use clap::{Args, Subcommand};

/// Bounds the distinct sanitizer findings one lane section lists.
const MAX_FINDING_ROWS: usize = 100;

/// Hands the run's repeat count to a lane that performs its own repeats.
const REPEATS_ENV: &str = "KITHARA_STRESS_REPEATS";

use crate::{
    Ctx,
    common::project::{
        ProjectConfig, StressArtifactConfig, StressConfig, StressEvidenceConfig, StressModeConfig,
    },
    lease,
    stress_report::{self, StressReportArgs},
    stress_run::{self, StressRunSpec},
    test::{ConfiguredLane, configured_lane},
    verdict::{ChildFailure, NotClean},
};

mod environment;
mod manifest;
mod output;
pub(crate) mod pressure;
mod system;

use environment::RunEnvironment;
use manifest::{
    BuildSnapshot, ExecuteResult, ExpectedProvenance, Manifest, ManifestConfig, ManifestSpec,
    PolicySnapshot, Selection,
};
use pressure::Sampler;

#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum StressCommand {
    /// Run every configured lane and preserve the evidence.
    Run(RunArgs),
    /// Independently verify and render a downloaded run artifact.
    Report(ReportArgs),
}

#[derive(Debug, Args)]
#[non_exhaustive]
pub struct RunArgs {
    /// Number of times to run every selected test.
    #[arg(long)]
    count: Option<usize>,
    /// Trusted controller revision to compare with the checkout.
    #[arg(long)]
    expected_controller_sha: Option<String>,
    /// Trusted subject revision to compare with the checkout.
    #[arg(long)]
    expected_subject_sha: Option<String>,
    /// Nextest filterset selecting tests to repeat.
    #[arg(long)]
    filter: Option<String>,
    /// Fresh raw evidence directory owned by this run.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Subject workspace whose tests are selected and executed.
    #[arg(long, default_value = ".")]
    subject_root: PathBuf,
    /// Configured stress mode; repeat the flag for several, empty for the
    /// project's own list. Each becomes one lane of the same run.
    #[arg(long = "mode")]
    modes: Vec<String>,
}

#[derive(Debug, Args)]
#[non_exhaustive]
pub struct ReportArgs {
    #[arg(long)]
    execute_result: ExecuteResult,
    #[arg(long)]
    count: Option<usize>,
    #[arg(long)]
    filter: Option<String>,
    /// Markdown report destination.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Downloaded raw evidence directory.
    #[arg(long)]
    raw: PathBuf,
    #[arg(long)]
    expected_controller_sha: String,
    #[arg(long)]
    expected_subject_sha: String,
    /// Lane to verify; repeat to verify several, empty for the project's list.
    #[arg(long = "mode")]
    modes: Vec<String>,
}

/// Runs the selected stress command.
///
/// # Errors
///
/// Returns an error when execution, finalization, or verification fails.
pub(crate) fn run(command: &StressCommand, ctx: &Ctx) -> Result<()> {
    match command {
        StressCommand::Run(args) => execute_run(args, ctx),
        StressCommand::Report(args) => run_report(args, ctx),
    }
}

pub(crate) fn run_output(command: &mut Command, path: &Path) -> Result<ExitStatus> {
    output::run(command, path)
}

pub(crate) fn run_stderr_output(command: &mut Command, path: &Path) -> Result<ExitStatus> {
    output::run_stderr(command, path)
}

#[derive(Debug)]
struct Paths {
    envelopes: Option<PathBuf>,
    lines: Option<PathBuf>,
    /// One `JUnit` copy per attempt of a command lane, named by attempt.
    attempt_junit: PathBuf,
    attempts: PathBuf,
    inventory: PathBuf,
    junit: PathBuf,
    log: PathBuf,
    manifest: PathBuf,
    pressure: PathBuf,
    raw: PathBuf,
    report: PathBuf,
}

struct ReportExpectation<'a> {
    config: &'a StressConfig,
    mode: &'a StressModeConfig,
    filter: &'a str,
    mode_name: &'a str,
    runner: ConfiguredLane,
    count: usize,
}

impl<'a> ReportExpectation<'a> {
    /// Derives what the lane should have been from the same place the lane
    /// itself did.
    ///
    /// The runner is not passed in. A lane records the runner `lane_runner`
    /// gave it, so anything else the report expects is a second opinion about
    /// the same question, and the two disagree exactly where the lanes differ
    /// most — a command lane runs its own command and would read as evidence
    /// of unknown origin against the test runner's identity.
    fn new(
        project: &ProjectConfig,
        config: &'a StressConfig,
        mode_name: &'a str,
        mode: &'a StressModeConfig,
        filter: &'a str,
        count: usize,
    ) -> Result<Self> {
        Ok(Self {
            config,
            mode_name,
            mode,
            filter,
            count,
            runner: lane_runner(project, config, mode)?,
        })
    }
}

impl Paths {
    fn new(raw: PathBuf, artifacts: &StressArtifactConfig) -> Self {
        Self {
            attempts: raw.join(&artifacts.attempts),
            attempt_junit: raw.join("attempt-junit"),
            envelopes: artifacts.envelope_dir.as_deref().map(|path| raw.join(path)),
            inventory: raw.join(&artifacts.inventory),
            junit: raw.join(&artifacts.junit),
            log: raw.join(&artifacts.log),
            manifest: raw.join(&artifacts.manifest),
            lines: artifacts.line_log.as_deref().map(|path| raw.join(path)),
            pressure: raw.join(&artifacts.pressure),
            report: raw.join(&artifacts.report),
            raw,
        }
    }
}

/// Runs every lane, in order, into one evidence directory.
///
/// A lane that fails does not stop the ones after it. A run exists to
/// find out which lane a flake belongs to, and a run that stopped at the first
/// red lane would answer that question only when the answer was already known.
/// The first failure is what the caller sees, once every lane has finished.
fn execute_run(args: &RunArgs, ctx: &Ctx) -> Result<()> {
    let config = &ctx.config.stress;
    ensure!(config.is_configured(), "stress run is not configured");
    let lanes = resolve_lanes(&args.modes, config)?;
    let root = absolute_from(
        &ctx.root,
        args.output
            .as_deref()
            .unwrap_or_else(|| Path::new(&config.raw_output)),
    );
    let subject_root = absolute_existing_directory(&ctx.root, &args.subject_root, "subject")?;
    let subject_junit = subject_junit(&subject_root, config);
    ensure!(
        !subject_junit
            .try_exists()
            .with_context(|| format!("inspect stress JUnit path {}", subject_junit.display()))?,
        "stress JUnit already exists: {}; remove it before starting a new run",
        subject_junit.display()
    );
    prepare_run_root(&root)?;
    let mut failure = None;
    for lane in &lanes {
        let outcome = run_lane(args, ctx, lane, &root.join(lane));
        if let Err(error) = outcome
            && failure.is_none()
        {
            failure = Some(error);
        }
    }
    failure.map_or(Ok(()), Err)
}

/// Where a lane building `root` leaves its artifacts.
///
/// The run names this directory instead of inheriting `CARGO_TARGET_DIR`, and it
/// belongs to the checkout it builds: a lane that builds into a directory shared
/// with the rest of the machine can have its binaries taken away while it is
/// still running them, and a stress run that lasts hours is the one most likely
/// to be standing there when it happens.
fn build_root(root: &Path, config: &StressConfig) -> PathBuf {
    root.join(&config.build_dir)
}

/// Where the test runner leaves the report a lane is measured by.
///
/// Anchored at the checkout, not the build directory: nextest's store is
/// rooted at the workspace root and does not follow `CARGO_TARGET_DIR`, so a
/// report anchor under the build directory points where no report is ever
/// written.
fn subject_junit(subject_root: &Path, config: &StressConfig) -> PathBuf {
    subject_root.join(&config.artifacts.subject_junit)
}

/// The lanes this invocation is made of: what was asked for, or what the
/// project says a run is.
fn resolve_lanes(requested: &[String], config: &StressConfig) -> Result<Vec<String>> {
    let lanes = if requested.is_empty() {
        config.default_modes.clone()
    } else {
        requested.to_vec()
    };
    ensure!(!lanes.is_empty(), "a run must name at least one mode");
    let mut seen = BTreeSet::new();
    for lane in &lanes {
        config.mode(lane)?;
        validate_lane_directory(lane)?;
        ensure!(seen.insert(lane), "stress mode `{lane}` is named twice");
    }
    Ok(lanes)
}

/// A lane names the directory its evidence lands in, so it has to be a plain
/// directory name rather than anything that could climb out of the run.
fn validate_lane_directory(lane: &str) -> Result<()> {
    let mut components = Path::new(lane).components();
    let single =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    ensure!(
        single && !lane.is_empty(),
        "stress mode `{lane}` is not usable as a directory name"
    );
    Ok(())
}

/// What a lane actually invokes, in the shape the manifest records.
///
/// A command lane is described by its own words rather than by the project's
/// test runner, so that its manifest names what really ran and the reporter
/// can verify it the same way it verifies any other lane.
fn lane_runner(
    project: &ProjectConfig,
    config: &StressConfig,
    mode: &StressModeConfig,
) -> Result<ConfiguredLane> {
    let Some((program, arguments)) = mode.command.split_first() else {
        return configured_lane(project, &config.lane, &config.backend, &mode.features);
    };
    Ok(ConfiguredLane {
        lane: config.lane.clone(),
        backend: config.backend.clone(),
        program: program.clone(),
        prefix_args: arguments.to_vec(),
        suffix_args: Vec::new(),
        feature_arg: "--features".to_owned(),
        features: Vec::new(),
        // A mode that names its own command replaces the runner, so it carries
        // no lane environment; `set_env` is the channel a mode has for that.
        env: BTreeMap::new(),
    })
}

/// Records one exit code per attempt, in order.
/// Separates one attempt's output from the next in the lane's shared log.
///
/// The attempts append to one file, so without a boundary a finding cannot be
/// told apart from the same finding on a later attempt — and a violation that
/// fires once in fifty would be indistinguishable from one that fires always.
fn mark_attempt(log: &Path, attempt: usize) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("open stress log {}", log.display()))?;
    writeln!(file, "{}{attempt}", stress_report::ATTEMPT_MARKER)
        .with_context(|| format!("write stress log {}", log.display()))
}

fn write_attempts(path: &Path, codes: &[i32]) -> Result<()> {
    let json = serde_json::to_string(codes).context("serialize command lane attempts")?;
    fs::write(path, json).with_context(|| format!("write command lane attempts {}", path.display()))
}

/// Runs a lane's own command as many times as the lane needs, and records what
/// each attempt did.
///
/// Repetition is the point: a violation that fires on one attempt in two is a
/// defect with a rate, and a single green run has never been evidence that it is
/// gone. Which side performs the repeats is the lane's own business. A command
/// that runs its tests under nextest is handed the count and launched once, so
/// the workspace builds once and every repeat lands in one report that names the
/// test; a command that cannot is launched once per repeat, and then an exit
/// code per attempt is all there is to collect.
fn run_command_lane(
    ctx: &Ctx,
    mode: &StressModeConfig,
    paths: &Paths,
    count: usize,
    environment: &RunEnvironment,
) -> Result<Vec<i32>> {
    let (program, arguments) = mode
        .command
        .split_first()
        .context("a command lane needs a program to run")?;
    let report = mode
        .attempt_junit
        .as_deref()
        .map(|path| ctx.root.join(path));
    let attempts = command_lane_attempts(mode, count);
    let mut codes = Vec::with_capacity(attempts);
    for attempt in 0..attempts {
        mark_attempt(&paths.log, attempt)?;
        // The runner writes to one path. Removing it first means a copy can
        // only ever be this attempt's: an attempt that died before writing
        // leaves no file rather than the previous attempt's verdict under a
        // new number.
        if let Some(report) = report.as_deref()
            && let Err(error) = fs::remove_file(report)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error)
                .with_context(|| format!("clear command lane report {}", report.display()));
        }
        let mut command = Command::new(program);
        command.args(arguments).current_dir(&ctx.root);
        environment.apply(&mut command);
        if mode.owns_repeats {
            command.env(REPEATS_ENV, count.to_string());
        }
        let status = run_output(&mut command, &paths.log)?;
        let code = status.code().unwrap_or_else(|| i32::from(u8::MAX));
        codes.push(code);
        if let Some(report) = report.as_deref() {
            keep_attempt_report(report, &paths.attempt_junit, attempt)?;
        }
    }
    Ok(codes)
}

/// How many times the run launches a command lane to buy `count` repeats.
///
/// One launch when the command repeats internally, `count` launches when it does
/// not. The distinction is what the lane's numbers mean afterwards: launches are
/// what an exit code can speak about, repeats are what a rate is measured over,
/// and reporting one as the other is how a lane that ran fifty times reads as
/// though it ran once.
fn command_lane_attempts(mode: &StressModeConfig, count: usize) -> usize {
    if mode.owns_repeats { 1 } else { count }
}

/// Keeps this attempt's report, if the command left one.
///
/// An aborted attempt may leave nothing at all — that absence is itself
/// evidence, and it is recorded by the copy that is missing rather than by an
/// error here.
fn keep_attempt_report(report: &Path, directory: &Path, attempt: usize) -> Result<()> {
    if !report.is_file() {
        return Ok(());
    }
    fs::create_dir_all(directory)
        .with_context(|| format!("create attempt report directory {}", directory.display()))?;
    let kept = directory.join(format!("attempt-{attempt}.xml"));
    fs::copy(report, &kept)
        .with_context(|| format!("keep attempt report {}", kept.display()))
        .map(|_| ())
}

fn run_lane(args: &RunArgs, ctx: &Ctx, mode_name: &str, raw: &Path) -> Result<()> {
    let config = &ctx.config.stress;
    let mode = config.mode(mode_name)?;
    let filter = args
        .filter
        .clone()
        .unwrap_or_else(|| config.default_filter.clone());
    let count = args.count.unwrap_or(config.default_count);
    validate_count(count, config.max_count)?;
    let subject_root = absolute_existing_directory(&ctx.root, &args.subject_root, "subject")?;
    let paths = Paths::new(raw.to_path_buf(), &config.artifacts);
    let config_file = absolute_existing_file(
        &ctx.root,
        Path::new(&config.nextest_config),
        "nextest config",
    )?;
    let controller_sha = revision(
        &ctx.root,
        args.expected_controller_sha.as_deref(),
        "controller",
    )?;
    let subject_sha = revision(
        &subject_root,
        args.expected_subject_sha.as_deref(),
        "subject",
    )?;
    let subject_junit = subject_junit(&subject_root, config);
    let runner = lane_runner(&ctx.config, config, mode)?;
    let commanded = !mode.command.is_empty();
    // A command lane runs its own recipe in the controller checkout; every
    // other lane runs the configured runner against the subject. Each builds
    // beside the tree it compiles, so one lane's artifacts never answer for
    // another lane's source.
    let build = build_root(if commanded { &ctx.root } else { &subject_root }, config);
    // Held for the lane, so the host's build-cache budget leaves these
    // artifacts alone while the lane is still executing them.
    let _build_lease = lease::hold(&build);
    let spec = StressRunSpec {
        count,
        inventory: paths.inventory.clone(),
        junit: subject_junit.clone(),
        config_file: config_file.clone(),
        filter: filter.clone(),
        test_threads: config.test_threads.clone(),
        profile: config.nextest_profile.clone(),
        max_count: config.max_count,
        max_test_threads: config.max_test_threads,
        runner: runner.clone(),
    };
    if !commanded {
        stress_run::validate(&spec)?;
        clear_previous_lane_junit(&subject_junit)?;
    }
    ensure_raw_outside_subject_evidence(&paths.raw, &subject_junit)?;
    let system = system::capture()?;
    let environment = RunEnvironment::new(&paths.raw, &build, config, mode)?;
    let mut manifest = Manifest::start(
        ManifestSpec {
            controller_sha,
            subject_sha,
            runner,
            mode: mode_name.to_owned(),
            build: BuildSnapshot::new(&build)?,
            config: ManifestConfig::new(
                config.nextest_profile.clone(),
                config.nextest_config.clone(),
                config.workflow_job_timeout_minutes,
            ),
            selection: Selection {
                count,
                filter: filter.clone(),
                test_threads: config.test_threads.clone(),
            },
            policy: policy_snapshot(config, mode),
        },
        system.clone(),
    )?;
    prepare_raw_directory(&paths)?;
    let manifest_start = manifest.write_atomic(&paths.manifest);
    let sampler = Sampler::start(
        &paths.pressure,
        system.cgroup_v2.path.as_deref(),
        system.cgroup_v2.scope.as_str(),
    );

    let (primary, sampler_result) = match (manifest_start, sampler) {
        (Ok(()), Ok(sampler)) => {
            let primary = if commanded {
                run_command_lane(ctx, mode, &paths, count, &environment).and_then(|codes| {
                    write_attempts(&paths.attempts, &codes)?;
                    let failed = codes.iter().filter(|code| **code != 0).count();
                    if failed == 0 {
                        Ok(())
                    } else {
                        Err(ChildFailure::inherited(
                            format!("{failed} of {} attempts", codes.len()),
                            codes.iter().copied().find(|code| *code != 0),
                        ))
                    }
                })
            } else {
                stress_run::run(&spec, &subject_root, &paths.log, &|command| {
                    environment.apply(command);
                })
            };
            let primary_code = result_code(&primary);
            let sampler_result = sampler.finish(Some(primary_code));
            (primary, sampler_result)
        }
        (Err(error), Ok(sampler)) => {
            let primary = Err(error);
            let sampler_result = sampler.finish(None);
            (primary, sampler_result)
        }
        (Ok(()), Err(error)) => (Ok(()), Err(error)),
        (Err(manifest_error), Err(sampler_error)) => (Err(manifest_error), Err(sampler_error)),
    };
    let sampler_healthy = sampler_result.is_ok();
    // A command lane produces no per-test evidence, so there is nothing to
    // stage and nothing for the per-test reporter to read. Its verdict is the
    // attempts it recorded.
    let (stage_result, report_result) = if commanded {
        (Ok(()), Ok(()))
    } else {
        (
            stage_junit(&subject_junit, &paths.junit),
            render_raw_report(&paths, count, config),
        )
    };
    let final_error = choose_failure(
        primary,
        sampler_result.map(|_| ()),
        stage_result,
        report_result,
    );
    let final_code = final_error.as_ref().map_or(0, error_code);
    manifest.finalize(final_code, sampler_healthy)?;
    manifest.write_atomic(&paths.manifest)?;
    final_error.map_or(Ok(()), Err)
}

fn policy_snapshot(config: &StressConfig, mode: &StressModeConfig) -> PolicySnapshot {
    PolicySnapshot {
        features: mode.features.clone(),
        remove_env: config.environment.remove.clone(),
        set_env: mode.set_env.clone(),
        raw_path_env: mode.raw_path_env.clone(),
        evidence: config.evidence.clone(),
    }
}

fn render_raw_report(paths: &Paths, count: usize, config: &StressConfig) -> Result<()> {
    let args = StressReportArgs::new(
        paths.junit.clone(),
        paths.inventory.clone(),
        paths.report.clone(),
        count,
    )
    .with_evidence(config.evidence.clone())
    .with_pressure(paths.pressure.clone())
    .with_optional_envelopes(
        paths
            .envelopes
            .as_ref()
            .filter(|path| path.is_dir())
            .cloned(),
    )
    .with_optional_lines(paths.lines.clone());
    stress_report::run(&args)
}

/// Verifies every lane of a downloaded run and renders them as one report.
///
/// The lanes are read independently — each carries its own manifest, inventory
/// and `JUnit`, and each is checked against what the project says it should have
/// been. They are rendered together because the question a multi-lane run
/// answers is a comparison, and a comparison split across two documents is one
/// the reader has to make by hand.
fn run_report(args: &ReportArgs, ctx: &Ctx) -> Result<()> {
    let config = &ctx.config.stress;
    ensure!(config.is_configured(), "stress run is not configured");
    let lanes = resolve_lanes(&args.modes, config)?;
    let filter = args
        .filter
        .clone()
        .unwrap_or_else(|| config.default_filter.clone());
    let count = args.count.unwrap_or(config.default_count);
    validate_count(count, config.max_count)?;
    let raw_root = absolute_from_current(&args.raw)?;
    let output = absolute_from(
        &ctx.root,
        args.output
            .as_deref()
            .unwrap_or_else(|| Path::new(&config.report_output)),
    );
    ensure_report_outside_raw(&raw_root, &output)?;

    let mut sections = String::new();
    let mut measured = Vec::new();
    let mut commanded = Vec::new();
    let mut excluded = Vec::new();
    let mut exit_codes = Vec::new();
    let mut failure = None;
    let mut unclean = Vec::new();
    for lane_name in &lanes {
        let mode = config.mode(lane_name)?;
        let paths = Paths::new(raw_root.join(lane_name), &config.artifacts);
        let report_args = StressReportArgs::new(
            paths.junit.clone(),
            paths.inventory.clone(),
            output.clone(),
            count,
        )
        .with_allow_missing(true)
        .with_evidence(config.evidence.clone())
        .with_pressure(paths.pressure.clone())
        .with_optional_envelopes(
            paths
                .envelopes
                .as_ref()
                .filter(|path| path.is_dir())
                .cloned(),
        )
        .with_optional_lines(paths.lines.clone());
        let lane = if mode.command.is_empty() {
            stress_report::lane_report(&report_args)?
        } else {
            command_lane_report(&paths, mode, count, &config.evidence)
        };
        let expectation =
            ReportExpectation::new(&ctx.config, config, lane_name, mode, &filter, count)?;
        let checked = verify_manifest(args, &expectation, &paths.manifest);
        let trusted = checked.verdict.is_ok();
        let excluded_because = exclusion_reason(trusted, &lane);
        if let Some(reason) = unclean_reason(excluded_because.as_deref(), &lane.verdict) {
            unclean.push((lane_name.clone(), reason));
        }
        exit_codes.push(checked.exit_code);
        let body = with_provenance(lane.markdown, &checked.verdict, &checked.details)?;
        writeln!(sections, "\n# Lane `{}`\n", markdown_cell(lane_name))?;
        sections.push_str(&body);
        // Only a lane that verified against its expected identity, read valid
        // evidence, AND accounted for every requested iteration may stand in a
        // comparison. Numbers from one that did not are of unknown origin, and
        // putting them beside trustworthy ones is how a run reports a
        // difference between lanes that is really a difference between runs.
        // A lane short of its own request measures a smaller run than the
        // one that was asked for, so its rate belongs to a different question.
        match excluded_because {
            Some(reason) => excluded.push((lane_name.clone(), reason)),
            None => match lane.attempts {
                Some(rate) => commanded.push((lane_name.clone(), rate)),
                None => measured.push((lane_name.clone(), lane.rates)),
            },
        }
        let lane_failure = choose_failure(lane.verdict, checked.verdict, Ok(()), Ok(()));
        if let Some(error) = lane_failure
            && failure.is_none()
        {
            failure = Some(error);
        }
    }

    let run = verify_run_result(args.execute_result, &exit_codes);
    let mut document =
        stress_report::render_lane_comparison(&measured, &commanded, &excluded, lanes.len());
    if let Err(error) = &run {
        let _ = writeln!(
            document,
            "\n- Run provenance: `{}`",
            markdown_cell(&format!("{error:#}"))
        );
    }
    document.push_str(&sections);
    stress_report::write_report(&output, &document)?;
    if let Some(summary) = unclean_summary(&unclean, &output) {
        println!("{summary}");
    }
    choose_failure(failure.map_or(Ok(()), Err), run, Ok(()), Ok(())).map_or(Ok(()), Err)
}

/// Why this lane is not clean, or `None` when it is.
///
/// A lane can be red for two unrelated reasons: the evidence cannot be trusted,
/// or the evidence is fine and the tests failed. The first already has a
/// sentence; the second needs one, because "the check did not pass" with nothing
/// named sends the reader looking for output that was never printed.
fn unclean_reason(excluded: Option<&str>, verdict: &Result<()>) -> Option<String> {
    if let Some(reason) = excluded {
        return Some(reason.to_owned());
    }
    verdict
        .as_ref()
        .err()
        .map(|_| "recorded a failed test or attempt".to_owned())
}

/// The verdict's findings, for the log the operator actually reads.
///
/// The report itself is a document a workflow puts in a step summary. The
/// process that wrote it exits with "the check ran and did not pass. Its
/// findings are above" — and above, in the job log, there was nothing. These
/// lines are that "above": which lane, and why.
fn unclean_summary(unclean: &[(String, String)], report: &Path) -> Option<String> {
    if unclean.is_empty() {
        return None;
    }
    let mut summary = String::from("stress evidence — lanes that did not come back clean:\n");
    for (lane, reason) in unclean {
        let _ = writeln!(summary, "  - {lane}: {reason}");
    }
    let _ = write!(summary, "Per-lane detail: {}", report.display());
    Some(summary)
}

/// Why this lane may not stand beside the others, or `None` when it may.
///
/// Named rather than counted: "one lane was dropped" sends the reader back to
/// the per-lane sections to work out which one and why, and that is the join
/// the run document exists to spare them.
fn exclusion_reason(trusted: bool, lane: &stress_report::LaneReport) -> Option<String> {
    if !trusted {
        return Some("failed provenance against its expected identity".to_owned());
    }
    if !lane.readable {
        return Some("evidence artifact missing or invalid".to_owned());
    }
    if let Some(reason) = &lane.incomplete {
        return Some(format!("incomplete evidence: {reason}"));
    }
    None
}

/// Reads what a command lane recorded and states it as rates.
///
/// A lane that repeats internally has a report per repeat, so it says how often
/// each test failed — the same number the run's own lanes report, which is
/// what lets a sanitizer lane stand in the cross-lane comparison. A lane launched
/// per repeat has only its exit codes, and how many attempts the command rejected
/// is all it can say. Either way the number is the one a one-shot gate cannot
/// produce: a sanitizer that aborts on one attempt in two is green half the time,
/// and half the time is what has kept its defect open.
///
/// A lane launched per repeat records no retried passes: exit codes cannot say
/// that an attempt failed and was retried into a pass.
fn command_lane_report(
    paths: &Paths,
    mode: &StressModeConfig,
    count: usize,
    evidence: &StressEvidenceConfig,
) -> stress_report::LaneReport {
    let expected = command_lane_attempts(mode, count);
    let attempts = &paths.attempts;
    let log = &paths.log;
    let mut markdown = String::from("# Stress evidence\n");
    let codes = match fs::read_to_string(attempts)
        .with_context(|| format!("read command lane attempts {}", attempts.display()))
        .and_then(|text| {
            serde_json::from_str::<Vec<i32>>(&text).context("parse command lane attempts")
        }) {
        Ok(codes) => codes,
        Err(error) => {
            let _ = writeln!(
                markdown,
                "\n- Result: **NO ATTEMPTS**\n\n`{}`\n",
                markdown_cell(&format!("{error:#}"))
            );
            return stress_report::LaneReport {
                markdown,
                rates: BTreeMap::new(),
                attempts: None,
                verdict: Err(NotClean::reported("stress evidence")),
                readable: false,
                incomplete: Some("the lane recorded no attempts".to_owned()),
            };
        }
    };
    let failed = codes.iter().filter(|code| **code != 0).count();
    let observed = codes.len();
    let records = stress_report::attempt_records(&paths.attempt_junit, &codes);
    // A lane that repeats inside one launch is only as complete as its own
    // report: the exit code says the command finished, not that it ran the
    // repeats it was given. Requiring the recorded repeats to match what was
    // asked is what keeps a run that stopped early out of the comparison.
    let short = mode.owns_repeats && records.repeats() != count;
    let retried = records.retried();
    let result = if observed != expected || short {
        "INCOMPLETE"
    } else if failed > 0 {
        "FAILED"
    } else if retried > 0 {
        "FLAKY"
    } else {
        "PASSED"
    };
    let _ = writeln!(markdown, "\n- Result: **{result}**");
    let _ = writeln!(markdown, "- Requested attempts: `{expected}`");
    let _ = writeln!(markdown, "- Observed attempts: `{observed}`");
    let _ = writeln!(markdown, "- Rejected attempts: `{failed}`");
    let _ = writeln!(markdown, "- Retried passes: `{retried}`");
    if mode.owns_repeats {
        let _ = writeln!(
            markdown,
            "- Repeats the command performed itself: requested `{count}`, recorded `{}`",
            records.repeats()
        );
    }
    if failed > 0 {
        let codes = codes
            .iter()
            .enumerate()
            .filter(|(_, code)| **code != 0)
            .map(|(attempt, code)| format!("{attempt}:{code}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            markdown,
            "- Rejected attempt:code — `{}`",
            markdown_cell(&codes)
        );
    }
    stress_report::append_attempt_reports(&mut markdown, &records);
    append_findings(
        &mut markdown,
        log,
        evidence,
        failed,
        attributable(mode, observed),
    );
    let verdict = if result == "PASSED" {
        Ok(())
    } else {
        Err(NotClean::reported("stress evidence"))
    };
    // A lane that repeats internally is measured per test, like the lanes the
    // run drives itself, and belongs in that comparison. A lane launched per
    // repeat has only its exit codes, and a per-test table it cannot fill would
    // read as though every test passed.
    let (rates, attempts) = if mode.owns_repeats {
        (records.rates, None)
    } else {
        (
            BTreeMap::new(),
            Some(stress_report::LaneRate {
                failed,
                flaky: 0,
                attempts: observed,
            }),
        )
    };
    stress_report::LaneReport {
        markdown,
        rates,
        attempts,
        verdict,
        readable: true,
        incomplete: command_lane_shortfall(observed, expected, short),
    }
}

/// Why a command lane may not stand beside the others, or `None`.
///
/// A lane launched per repeat falls short when the run rejected launches; a
/// lane that repeats inside one launch falls short when its own report records
/// fewer repeats than it was given. Both leave a rate measured over fewer
/// attempts than requested, and which one it was is what the run document has
/// to print instead of guessing.
fn command_lane_shortfall(observed: usize, expected: usize, short: bool) -> Option<String> {
    if observed != expected {
        return Some(format!(
            "the lane recorded {observed} of {expected} requested attempts"
        ));
    }
    short.then(|| "the command recorded fewer repeats than it was given".to_owned())
}

/// The denominator a log finding may be reported against, or `None` when the log
/// cannot say.
///
/// Findings are attributed by the attempt marker written before each launch. A
/// lane that repeats inside one launch writes one marker, so every finding it
/// reports belongs to "attempt 0" — a rate computed from that would say 100% for
/// a violation that fired once in fifty repeats. The per-test table above carries
/// the rate for those lanes; this refuses to invent a second one.
fn attributable(mode: &StressModeConfig, observed: usize) -> Option<usize> {
    (!mode.owns_repeats).then_some(observed)
}

/// Reports what the sanitizer itself said, and says so when it said nothing.
///
/// A rejected attempt with no finding is not a smaller version of a finding: it
/// means the command failed for a reason this report cannot name, and printing
/// only a count there would read as though the cause had been located.
fn append_findings(
    markdown: &mut String,
    log: &Path,
    evidence: &StressEvidenceConfig,
    failed: usize,
    observed: Option<usize>,
) {
    let text = match stress_report::read_bounded_utf8(
        log,
        stress_report::MAX_LANE_LOG_BYTES,
        "stress lane log",
    ) {
        Ok(text) => text,
        Err(error) => {
            let _ = writeln!(
                markdown,
                "\nEvidence problem: this lane's log could not be read, so its findings are \
                 unknown — `{}`",
                markdown_cell(&format!("{error:#}"))
            );
            return;
        }
    };
    let findings = stress_report::sanitizer_findings(&text, evidence);
    if findings.is_empty() {
        if failed > 0 {
            let _ = writeln!(
                markdown,
                "\nNo sanitizer report was found in this lane's log. The rejected attempts failed \
                 for a reason this report cannot name; the command's own output is in the log."
            );
        }
        return;
    }
    let _ = writeln!(
        markdown,
        "\n## Sanitizer findings\n\nEach names the violated contract, the call that violated it, \
         and the first project frames that reached it."
    );
    if let Some(observed) = observed {
        append_rated_findings(markdown, &findings, observed);
    } else {
        append_unrated_findings(markdown, &findings);
    }
    if findings.len() > MAX_FINDING_ROWS {
        let _ = writeln!(
            markdown,
            "\nShowing the first {MAX_FINDING_ROWS} of {} distinct findings.",
            findings.len()
        );
    }
}

/// Lists each finding with the attempts it appeared on, out of the attempts the
/// lane ran.
fn append_rated_findings(
    markdown: &mut String,
    findings: &stress_report::Findings,
    observed: usize,
) {
    let _ = writeln!(markdown, "\n| finding | attempts | rate |\n|---|---|---:|");
    for (signature, attempts) in findings.iter().take(MAX_FINDING_ROWS) {
        let listed = attempts
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            markdown,
            "| `{}` | {} | {} |",
            markdown_cell(signature),
            markdown_cell(&listed),
            stress_report::rate_percent(attempts.len(), observed)
        );
    }
}

/// Lists each finding on its own, for a lane whose log cannot place it in time.
fn append_unrated_findings(markdown: &mut String, findings: &stress_report::Findings) {
    let _ = writeln!(
        markdown,
        "\nThe command performed its own repeats, so the log cannot say which repeat a finding \
         came from — how often each test failed is in the table above.\n\n| finding |\n|---|"
    );
    for signature in findings.keys().take(MAX_FINDING_ROWS) {
        let _ = writeln!(markdown, "| `{}` |", markdown_cell(signature));
    }
}

/// Checks the job's own result against the run as a whole.
///
/// A red job means some lane failed, and which one is a fact about the
/// run rather than about any single manifest. Checking it per lane would
/// call every lane that passed a liar; checking it here catches the case that
/// actually matters — a job reported as failed whose lanes all say they
/// succeeded, which means the failure came from somewhere the evidence does
/// not cover.
fn verify_run_result(execute_result: ExecuteResult, exit_codes: &[Option<i32>]) -> Result<()> {
    if matches!(execute_result, ExecuteResult::Success) {
        return Ok(());
    }
    ensure!(
        exit_codes
            .iter()
            .any(|code| code.is_none_or(|code| code != 0)),
        "execute reported {} while every lane finished cleanly",
        execute_result.as_str()
    );
    Ok(())
}

/// What one lane's manifest says about itself, and whether it is believable.
struct LaneProvenance {
    /// The lane's own exit code, kept so the run can check the job's
    /// result against all of its lanes rather than against each one alone.
    exit_code: Option<i32>,
    verdict: Result<()>,
    details: Vec<String>,
}

fn verify_manifest(
    args: &ReportArgs,
    expected: &ReportExpectation<'_>,
    path: &Path,
) -> LaneProvenance {
    let manifest = match Manifest::read(path) {
        Ok(manifest) => manifest,
        Err(error) => {
            let detail = format!("{error:#}");
            return LaneProvenance {
                verdict: Err(error),
                details: vec![detail],
                exit_code: None,
            };
        }
    };
    let exit_code = manifest.timing.exit_code;
    let expected = ExpectedProvenance {
        controller_sha: args.expected_controller_sha.clone(),
        subject_sha: args.expected_subject_sha.clone(),
        filter: expected.filter.to_owned(),
        count: expected.count,
        test_threads: expected.config.test_threads.clone(),
        mode: expected.mode_name.to_owned(),
        config: ManifestConfig::new(
            expected.config.nextest_profile.clone(),
            expected.config.nextest_config.clone(),
            expected.config.workflow_job_timeout_minutes,
        ),
        runner: expected.runner.clone(),
        policy: policy_snapshot(expected.config, expected.mode),
        execute_result: args.execute_result,
        sampler_healthy: true,
    };
    let mismatches = manifest.validate_provenance(&expected);
    if mismatches.is_empty() {
        return LaneProvenance {
            exit_code,
            verdict: Ok(()),
            details: Vec::new(),
        };
    }
    let details = mismatches.iter().map(ToString::to_string).collect();
    LaneProvenance {
        details,
        exit_code,
        verdict: Err(NotClean::raised("stress provenance", mismatches.len())),
    }
}

fn with_provenance(
    mut markdown: String,
    result: &Result<()>,
    details: &[String],
) -> Result<String> {
    if result.is_err() {
        invalidate_result(&mut markdown);
    }
    writeln!(markdown, "\n## Provenance")?;
    match result {
        Ok(()) => writeln!(
            markdown,
            "\nValidated against trusted workflow inputs: **yes**"
        )?,
        Err(error) => {
            writeln!(
                markdown,
                "\nValidated against trusted workflow inputs: **no**\n\n`{}`",
                markdown_cell(&format!("{error:#}"))
            )?;
            for detail in details.iter().take(100) {
                writeln!(markdown, "- `{}`", markdown_cell(detail))?;
            }
        }
    }
    Ok(markdown)
}

fn invalidate_result(markdown: &mut String) {
    for result in ["PASSED", "FAILED", "INCOMPLETE"] {
        let marker = format!("- Result: **{result}**");
        if let Some(index) = markdown.find(&marker) {
            markdown.replace_range(
                index..index + marker.len(),
                "- Result: **INVALID PROVENANCE**",
            );
            return;
        }
    }
    markdown.push_str("\n- Result: **INVALID PROVENANCE**\n");
}

/// The directory the whole run writes into, one level above its lanes.
///
/// Freshness is demanded here rather than per lane: the lanes are created
/// inside it as they run, so asking each of them for a directory that does not
/// exist yet would fail on the second one.
fn prepare_run_root(root: &Path) -> Result<()> {
    ensure!(
        !root
            .try_exists()
            .with_context(|| format!("inspect stress output {}", root.display()))?,
        "stress output already exists: {}; choose a fresh directory",
        root.display()
    );
    fs::create_dir_all(root).with_context(|| format!("create stress output {}", root.display()))?;
    Ok(())
}

fn prepare_raw_directory(paths: &Paths) -> Result<()> {
    ensure!(
        !paths
            .raw
            .try_exists()
            .with_context(|| format!("inspect stress output {}", paths.raw.display()))?,
        "stress output already exists: {}; choose a fresh directory",
        paths.raw.display()
    );
    fs::create_dir_all(&paths.raw)
        .with_context(|| format!("create stress output {}", paths.raw.display()))?;
    if let Some(envelopes) = &paths.envelopes {
        fs::create_dir_all(envelopes)
            .with_context(|| format!("create evidence directory {}", envelopes.display()))?;
    }
    fs::File::create(&paths.log)
        .with_context(|| format!("create stress command log {}", paths.log.display()))?;
    if let Some(lines) = &paths.lines {
        fs::File::create(lines)
            .with_context(|| format!("create line evidence sink {}", lines.display()))?;
    }
    Ok(())
}

/// Removes what the previous lane left at the subject's one `JUnit` path.
///
/// Staging is a copy with no proof of freshness, so the proof has to come from
/// the path being empty when the lane starts. Without this, a lane whose
/// nextest died before writing evidence would have its predecessor's `JUnit`
/// staged under its own name, and the report would attribute one lane's
/// failures to the other — the exact confusion a multi-lane run exists to
/// resolve.
fn clear_previous_lane_junit(junit: &Path) -> Result<()> {
    match fs::remove_file(junit) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("clear stress JUnit {}", junit.display())),
    }
}

fn stage_junit(source: &Path, destination: &Path) -> Result<()> {
    match fs::copy(source, destination) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("stress JUnit staging: nothing at {}", source.display());
            Err(NotClean::reported("stress JUnit staging"))
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "stage stress JUnit {} as {}",
                source.display(),
                destination.display()
            )
        }),
    }
}

fn revision(root: &Path, expected: Option<&str>, label: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .with_context(|| format!("read {label} revision from {}", root.display()))?;
    if !output.status.success() {
        return Err(ChildFailure::captured(
            format!("read {label} revision"),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let actual = String::from_utf8(output.stdout)
        .context("git revision is not UTF-8")?
        .trim()
        .to_ascii_lowercase();
    ensure!(
        valid_sha(&actual),
        "{label} revision is not a full SHA: {actual:?}"
    );
    if let Some(expected) = expected {
        ensure!(
            actual.eq_ignore_ascii_case(expected),
            "{label} revision is {actual}, expected {expected}"
        );
    }
    Ok(actual)
}

fn choose_failure(
    first: Result<()>,
    second: Result<()>,
    third: Result<()>,
    fourth: Result<()>,
) -> Option<Error> {
    let mut verdict = None;
    let mut child = None;
    for result in [first, second, third, fourth] {
        let Err(error) = result else { continue };
        if error.downcast_ref::<ChildFailure>().is_some() {
            child.get_or_insert(error);
        } else if error.downcast_ref::<NotClean>().is_some() {
            verdict.get_or_insert(error);
        } else {
            return Some(error);
        }
    }
    child.or(verdict)
}

fn result_code(result: &Result<()>) -> i32 {
    result.as_ref().err().map_or(0, error_code)
}

fn error_code(error: &Error) -> i32 {
    if error.downcast_ref::<NotClean>().is_some() {
        1
    } else if let Some(failure) = error.downcast_ref::<ChildFailure>() {
        failure.exit_code()
    } else {
        1
    }
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn absolute_from_current(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("read current directory")?
            .join(path))
    }
}

fn ensure_report_outside_raw(raw: &Path, output: &Path) -> Result<()> {
    let raw = resolve_path_identity(raw)?;
    let output_identity = resolve_path_identity(output)?;
    ensure!(
        !output_identity.starts_with(&raw),
        "stress report output must be outside the raw evidence directory: {}",
        output.display()
    );
    let parent = output
        .parent()
        .context("stress report output has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create stress report parent {}", parent.display()))?;
    Ok(())
}

fn ensure_raw_outside_subject_evidence(raw: &Path, subject_junit: &Path) -> Result<()> {
    let raw = resolve_path_identity(raw)?;
    let evidence = subject_junit
        .parent()
        .context("subject JUnit path has no parent directory")?;
    let evidence = resolve_path_identity(evidence)?;
    ensure!(
        !raw.starts_with(&evidence) && !evidence.starts_with(&raw),
        "stress output must not overlap subject nextest evidence: {}",
        raw.display()
    );
    Ok(())
}

fn resolve_path_identity(path: &Path) -> Result<PathBuf> {
    let normalized = normalize_absolute(path)?;
    let mut existing = normalized.as_path();
    while !existing
        .try_exists()
        .with_context(|| format!("inspect path identity {}", existing.display()))?
    {
        existing = existing
            .parent()
            .with_context(|| format!("path has no existing ancestor: {}", path.display()))?;
    }
    let suffix = normalized
        .strip_prefix(existing)
        .context("derive unresolved path suffix")?;
    let resolved = fs::canonicalize(existing)
        .with_context(|| format!("resolve path identity {}", existing.display()))?;
    Ok(resolved.join(suffix))
}

fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "path is not absolute: {}",
        path.display()
    );
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(
                    normalized.pop(),
                    "path escapes its filesystem root: {}",
                    path.display()
                );
            }
        }
    }
    Ok(normalized)
}

fn absolute_existing_directory(root: &Path, path: &Path, label: &str) -> Result<PathBuf> {
    let path = absolute_from(root, path);
    ensure!(
        path.is_dir(),
        "{label} directory does not exist: {}",
        path.display()
    );
    Ok(path)
}

fn absolute_existing_file(root: &Path, path: &Path, label: &str) -> Result<PathBuf> {
    let path = absolute_from(root, path);
    ensure!(path.is_file(), "{label} does not exist: {}", path.display());
    Ok(path)
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_count(count: usize, max: usize) -> Result<()> {
    ensure!(count > 0, "stress count must be greater than zero");
    ensure!(count <= max, "stress count must not exceed {max}");
    Ok(())
}

fn markdown_cell(value: &str) -> String {
    value.replace(['\n', '\r', '`'], " ")
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::common::project::StressEnvironmentConfig;

    /// A lane the run launches once per repeat.
    fn per_repeat_mode() -> StressModeConfig {
        StressModeConfig {
            command: vec!["just".to_owned(), "test".to_owned(), "rtsan".to_owned()],
            ..StressModeConfig::default()
        }
    }

    /// A lane whose command performs the run's repeats itself.
    fn self_repeating_mode() -> StressModeConfig {
        StressModeConfig {
            owns_repeats: true,
            ..per_repeat_mode()
        }
    }

    fn command_lane_config() -> (StressConfig, StressModeConfig) {
        let config = StressConfig {
            lane: "workspace".to_owned(),
            backend: "wreq".to_owned(),
            ..StressConfig::default()
        };
        (config, per_repeat_mode())
    }

    /// The report has to expect the command the lane was told to run. Expecting
    /// the test runner instead condemns a lane that did exactly what the
    /// project asked, and a condemned lane leaves the comparison — which is how
    /// a run can repeat its sanitizer lanes and still report nothing about
    /// them.
    #[test]
    fn a_command_lane_is_expected_to_have_run_its_own_command() {
        let (config, mode) = command_lane_config();

        let expectation = ReportExpectation::new(
            &ProjectConfig::default(),
            &config,
            "rtsan",
            &mode,
            "all()",
            2,
        )
        .expect("a command lane needs no configured test runner");

        assert_eq!(expectation.runner.program, "just");
        assert_eq!(expectation.runner.prefix_args, ["test", "rtsan"]);
    }

    /// nextest's store is rooted at the workspace root and ignores
    /// `CARGO_TARGET_DIR`, so the report anchor stays on the checkout even
    /// while the build is sent to the run's own directory. Anchoring it under
    /// the build directory reads a path where no report is ever written.
    #[test]
    fn a_lane_is_measured_by_the_report_under_the_checkout_it_tests() {
        let config = StressConfig {
            build_dir: "target-stress".to_owned(),
            artifacts: StressArtifactConfig {
                subject_junit: "target/nextest/stress/junit.xml".to_owned(),
                ..StressArtifactConfig::default()
            },
            ..StressConfig::default()
        };

        assert_eq!(
            subject_junit(Path::new("/work/subject"), &config),
            Path::new("/work/subject/target/nextest/stress/junit.xml")
        );
    }

    const VIOLATION: &str = "\
==2534==ERROR: RealtimeSanitizer: unsafe-library-call
Intercepted call to real-time unsafe function `malloc` in real-time context!
    #0 0x5628d3a1b2c0 in malloc (/opt/bin/suite_stress+0x1042c0)
    #1 0x5628d3c11f30 in kithara_audio::renderer::mix crates/kithara-audio/src/renderer/mix.rs:214:23
";

    fn command_lane(temp: &tempfile::TempDir, attempts: &str, log: &str) -> Paths {
        let paths = Paths::new(
            temp.path().to_path_buf(),
            &StressArtifactConfig {
                attempts: "attempts.json".to_owned(),
                log: "lane.log".to_owned(),
                ..StressArtifactConfig::default()
            },
        );
        fs::write(&paths.attempts, attempts).expect("write attempts fixture");
        fs::write(&paths.log, log).expect("write log fixture");
        paths
    }

    /// A sanitizer that fires on one attempt in two is green half the time. The
    /// run's job is to state that as a rate rather than as a verdict.
    #[test]
    fn a_command_lane_reports_how_many_attempts_the_command_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = command_lane(&temp, "[0,134,0,0]", "");

        let report = command_lane_report(
            &paths,
            &per_repeat_mode(),
            4,
            &StressEvidenceConfig::default(),
        );

        assert!(
            report.markdown.contains("Rejected attempts: `1`"),
            "{}",
            report.markdown
        );
        assert!(report.verdict.is_err());
    }

    /// An exit code says a lane is red. It does not say which contract broke,
    /// where, or on which attempts — and without that the reader is back to
    /// opening the log and guessing.
    #[test]
    fn a_command_lane_names_the_violation_the_sanitizer_reported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log = format!("{}0\n{VIOLATION}", stress_report::ATTEMPT_MARKER);
        let paths = command_lane(&temp, "[134,0]", &log);

        let report = command_lane_report(
            &paths,
            &per_repeat_mode(),
            2,
            &StressEvidenceConfig::default(),
        );

        assert!(
            report.markdown.contains("unsafe-library-call"),
            "{}",
            report.markdown
        );
        assert!(report.markdown.contains("malloc"), "{}", report.markdown);
        assert!(
            report
                .markdown
                .contains("crates/kithara-audio/src/renderer/mix.rs:214:23"),
            "{}",
            report.markdown
        );
        assert!(report.markdown.contains("50.00%"), "{}", report.markdown);
    }

    /// A rejected attempt the report cannot explain must say so. Printing only
    /// a count there reads as though the cause had been located.
    #[test]
    fn a_rejection_without_a_sanitizer_report_is_declared_unexplained() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = command_lane(&temp, "[1,0]", "error: could not compile `kithara-audio`\n");

        let report = command_lane_report(
            &paths,
            &per_repeat_mode(),
            2,
            &StressEvidenceConfig::default(),
        );

        assert!(
            report.markdown.contains("cannot name"),
            "{}",
            report.markdown
        );
    }

    /// Where a launched lane records what repeat count it was handed.
    const REPEATS_RECORD_ENV: &str = "DEVTOOLS_STRESS_REPEATS_RECORD";
    const SUITE: &str = "kithara-integration-tests::rtsan";
    const CASE: &str = "audio::mix_tap";

    /// Runs a lane whose command is this test binary, and reports what
    /// `KITHARA_STRESS_REPEATS` held on each launch.
    fn recorded_repeats(mode: &StressModeConfig, count: usize) -> Vec<String> {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = temp.path().join("repeats.txt");
        let executable = std::env::current_exe().expect("current test executable");
        let mode = StressModeConfig {
            command: vec![
                executable.to_string_lossy().into_owned(),
                child_test_name("record_repeats"),
                "--exact".to_owned(),
                "--ignored".to_owned(),
                "--nocapture".to_owned(),
            ],
            set_env: BTreeMap::from([(
                REPEATS_RECORD_ENV.to_owned(),
                record.to_string_lossy().into_owned(),
            )]),
            ..mode.clone()
        };
        // The run clears the variable, as the project's own configuration
        // does: a lane that is not handed a count must not inherit one from
        // whatever launched the run.
        let config = StressConfig {
            environment: StressEnvironmentConfig {
                remove: vec![REPEATS_ENV.to_owned()],
            },
            ..StressConfig::default()
        };
        let environment =
            RunEnvironment::new(temp.path(), &temp.path().join("build"), &config, &mode)
                .expect("run environment");
        let paths = Paths::new(
            temp.path().to_path_buf(),
            &StressArtifactConfig {
                log: "lane.log".to_owned(),
                ..StressArtifactConfig::default()
            },
        );
        let ctx = Ctx::new(temp.path().to_path_buf(), ProjectConfig::default());

        run_command_lane(&ctx, &mode, &paths, count, &environment).expect("run command lane");

        fs::read_to_string(&record)
            .expect("read recorded repeats")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn child_test_name(name: &str) -> String {
        let module = module_path!();
        let module = module.split_once("::").map_or(module, |(_, module)| module);
        format!("{module}::{name}")
    }

    /// A lane that repeats inside one launch pays for a rebuild and a cold start
    /// per launch. Launched fifty times, it pays fifty of each for the fifty
    /// repeats its own runner was going to perform anyway.
    #[test]
    fn a_lane_that_owns_its_repeats_is_launched_once() {
        assert_eq!(recorded_repeats(&self_repeating_mode(), 3).len(), 1);
    }

    /// The count has to reach the runner that performs the repeats. Without it
    /// the lane runs its selection once and the report calls that fifty.
    #[test]
    fn a_lane_that_owns_its_repeats_is_handed_the_count() {
        assert_eq!(
            recorded_repeats(&self_repeating_mode(), 3)
                .first()
                .map(String::as_str),
            Some("3")
        );
    }

    #[test]
    fn a_lane_that_does_not_own_its_repeats_is_launched_once_per_repeat() {
        assert_eq!(recorded_repeats(&per_repeat_mode(), 3).len(), 3);
    }

    /// Handing the count to a lane launched per repeat would multiply the two:
    /// three launches of three repeats each, for the three that were asked for.
    #[test]
    fn a_lane_that_does_not_own_its_repeats_is_not_handed_a_count() {
        assert!(
            recorded_repeats(&per_repeat_mode(), 3)
                .iter()
                .all(|value| value == "unset"),
            "a lane launched per repeat was handed a count"
        );
    }

    #[test]
    #[ignore = "subprocess entrypoint"]
    fn record_repeats() {
        let record = std::env::var_os(REPEATS_RECORD_ENV).expect("record path");
        let value = std::env::var(REPEATS_ENV).unwrap_or_else(|_| "unset".to_owned());
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(record)
            .expect("open repeats record");
        writeln!(file, "{value}").expect("write repeats record");
    }

    /// The report a lane's own runner leaves behind, one suite per repeat, which
    /// is what nextest writes when it is given a repeat count.
    fn keep_repeats(paths: &Paths, attempt: usize, outcomes: &[bool]) {
        let suites = outcomes
            .iter()
            .enumerate()
            .map(|(repeat, failed)| {
                let failure = if *failed {
                    "<failure type=\"test failure\">aborted</failure>"
                } else {
                    ""
                };
                format!(
                    "  <testsuite name=\"{SUITE}@stress-{repeat}\">\n    <testcase \
                     name=\"{CASE}\" classname=\"{SUITE}\" time=\"0.1\">{failure}</testcase>\n  \
                     </testsuite>\n"
                )
            })
            .collect::<String>();
        fs::create_dir_all(&paths.attempt_junit).expect("create attempt report directory");
        fs::write(
            paths.attempt_junit.join(format!("attempt-{attempt}.xml")),
            format!("<testsuites uuid=\"run\">\n{suites}</testsuites>"),
        )
        .expect("write attempt report fixture");
    }

    fn self_repeating_lane(temp: &tempfile::TempDir, log: &str, outcomes: &[bool]) -> Paths {
        let paths = command_lane(temp, "[101]", log);
        keep_repeats(&paths, 0, outcomes);
        paths
    }

    fn measured_case() -> (String, String) {
        (SUITE.to_owned(), CASE.to_owned())
    }

    /// A lane that repeats internally is measured per test, like the lanes the
    /// run drives itself. Without those rates it contributes nothing to the
    /// cross-lane comparison — which is how a run can repeat three sanitizer
    /// lanes and say nothing about any test in them.
    #[test]
    fn a_self_repeating_lane_reports_a_rate_per_test() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = self_repeating_lane(&temp, "", &[false, true, false]);

        let report = command_lane_report(
            &paths,
            &self_repeating_mode(),
            3,
            &StressEvidenceConfig::default(),
        );

        assert_eq!(
            report.rates[&measured_case()],
            stress_report::LaneRate {
                failed: 1,
                flaky: 0,
                attempts: 3
            }
        );
    }

    /// One lane, one denominator. A lane already reporting per-test rates must
    /// not also enter the comparison as a lane with nothing but exit codes.
    #[test]
    fn a_self_repeating_lane_does_not_also_report_an_attempt_rate() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = self_repeating_lane(&temp, "", &[false, true, false]);

        let report = command_lane_report(
            &paths,
            &self_repeating_mode(),
            3,
            &StressEvidenceConfig::default(),
        );

        assert!(report.attempts.is_none());
    }

    /// A green lane's own runner did report, and how much it covered is the
    /// reason the lane is green. A heading with nothing under it reads instead as
    /// evidence that never arrived.
    #[test]
    fn a_self_repeating_lane_that_passed_says_what_its_runner_covered() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = command_lane(&temp, "[0]", "");
        keep_repeats(&paths, 0, &[false, false]);

        let report = command_lane_report(
            &paths,
            &self_repeating_mode(),
            2,
            &StressEvidenceConfig::default(),
        );

        assert!(
            report.markdown.contains(
                "No test failed in any repeat the runner reported: `1` test(s) over `2` repeat(s)."
            ),
            "{}",
            report.markdown
        );
    }

    /// The verdict ends with "its findings are above". In the report job's log
    /// there was nothing above, and the lane that made the run red had to be
    /// found by downloading the artifact.
    #[test]
    fn a_run_that_is_not_clean_names_the_lane_in_its_own_output() {
        let summary = unclean_summary(
            &[(
                "rtsan".to_owned(),
                "recorded a failed test or attempt".to_owned(),
            )],
            Path::new("/tmp/stress-report.md"),
        )
        .expect("a lane that is not clean has a summary");

        assert!(
            summary.contains("rtsan: recorded a failed test or attempt"),
            "{summary}"
        );
    }

    /// A summary that speaks on every run trains the reader to skip it.
    #[test]
    fn a_clean_run_prints_no_summary() {
        assert!(unclean_summary(&[], Path::new("/tmp/stress-report.md")).is_none());
    }

    /// Evidence that cannot be trusted and tests that failed are different
    /// problems; the reason a reader gets must be the one that applies.
    #[test]
    fn an_excluded_lane_keeps_the_reason_it_was_excluded_for() {
        assert_eq!(
            unclean_reason(Some("evidence artifact missing or invalid"), &Ok(())),
            Some("evidence artifact missing or invalid".to_owned())
        );
    }

    /// A lane that verified and passed is not a finding.
    #[test]
    fn a_trustworthy_lane_that_passed_is_clean() {
        assert!(unclean_reason(None, &Ok(())).is_none());
    }

    /// A lane launched per repeat has only its exit codes, and that rate is what
    /// keeps it in the comparison rather than out of it.
    #[test]
    fn a_lane_launched_per_repeat_reports_the_rate_its_exit_codes_support() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = command_lane(&temp, "[0,134,0,0]", "");

        let report = command_lane_report(
            &paths,
            &per_repeat_mode(),
            4,
            &StressEvidenceConfig::default(),
        );

        assert_eq!(
            report.attempts,
            Some(stress_report::LaneRate {
                failed: 1,
                flaky: 0,
                attempts: 4
            })
        );
    }

    /// An exit code says the command finished, not that it ran the repeats it was
    /// handed. A run that stopped after two of three is not evidence about three.
    #[test]
    fn a_self_repeating_lane_that_ran_fewer_repeats_than_asked_is_not_complete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = self_repeating_lane(&temp, "", &[false, false]);

        let report = command_lane_report(
            &paths,
            &self_repeating_mode(),
            3,
            &StressEvidenceConfig::default(),
        );

        assert!(report.incomplete.is_some(), "{}", report.markdown);
    }

    /// The run document is the only place a reader learns why a lane was struck
    /// out, and it used to assert one cause for every shortfall. Run 33752112563
    /// printed "fewer iterations than requested" against a lane whose own
    /// section reported all fifty it was asked for.
    #[test]
    fn an_excluded_lane_is_named_by_the_shortfall_it_reported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = self_repeating_lane(&temp, "", &[false, false]);

        let report = command_lane_report(
            &paths,
            &self_repeating_mode(),
            3,
            &StressEvidenceConfig::default(),
        );
        let reason = exclusion_reason(true, &report).expect("a short lane must be excluded");

        assert!(
            reason.contains("fewer repeats than it was given"),
            "{reason}"
        );
    }

    #[test]
    fn a_short_self_repeating_lane_says_so_in_its_headline() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = self_repeating_lane(&temp, "", &[false, false]);

        let report = command_lane_report(
            &paths,
            &self_repeating_mode(),
            3,
            &StressEvidenceConfig::default(),
        );

        assert!(
            report.markdown.contains("Result: **INCOMPLETE**"),
            "{}",
            report.markdown
        );
    }

    /// A lane launched once writes one attempt marker, so every finding in its
    /// log belongs to "attempt 0". A rate computed from that would say 100% for a
    /// violation that fired once in fifty repeats.
    #[test]
    fn a_self_repeating_lane_does_not_rate_its_findings_by_attempt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log = format!("{}0\n{VIOLATION}", stress_report::ATTEMPT_MARKER);
        let paths = self_repeating_lane(&temp, &log, &[false, true, false]);

        let report = command_lane_report(
            &paths,
            &self_repeating_mode(),
            3,
            &StressEvidenceConfig::default(),
        );

        assert!(
            !report.markdown.contains("| finding | attempts | rate |"),
            "{}",
            report.markdown
        );
    }

    #[test]
    fn a_self_repeating_lane_still_names_the_violation_it_reported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log = format!("{}0\n{VIOLATION}", stress_report::ATTEMPT_MARKER);
        let paths = self_repeating_lane(&temp, &log, &[false, true, false]);

        let report = command_lane_report(
            &paths,
            &self_repeating_mode(),
            3,
            &StressEvidenceConfig::default(),
        );

        assert!(
            report.markdown.contains("unsafe-library-call"),
            "{}",
            report.markdown
        );
    }

    #[test]
    fn a_command_lane_missing_its_attempts_does_not_read_as_a_clean_run() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = Paths::new(
            temp.path().to_path_buf(),
            &StressArtifactConfig {
                attempts: "absent.json".to_owned(),
                log: "lane.log".to_owned(),
                ..StressArtifactConfig::default()
            },
        );

        let report = command_lane_report(
            &paths,
            &per_repeat_mode(),
            2,
            &StressEvidenceConfig::default(),
        );

        assert!(
            report.markdown.contains("NO ATTEMPTS"),
            "{}",
            report.markdown
        );
        assert!(report.verdict.is_err());
    }

    #[test]
    fn a_failed_job_whose_lanes_all_passed_is_reported_as_unexplained() {
        let error = verify_run_result(ExecuteResult::Failure, &[Some(0), Some(0)])
            .expect_err("a red job with only clean lanes is not explained by its evidence");

        assert!(format!("{error:#}").contains("every lane finished cleanly"));
    }

    #[test]
    fn a_failed_job_is_explained_by_a_single_failing_lane() {
        verify_run_result(ExecuteResult::Failure, &[Some(0), Some(101)])
            .expect("one failing lane explains a failed job");
    }

    #[test]
    fn a_successful_job_says_nothing_about_lanes_beyond_their_own_manifests() {
        verify_run_result(ExecuteResult::Success, &[Some(0), Some(0)])
            .expect("a green job needs no run-level explanation");
    }

    #[test]
    fn clearing_the_previous_lane_junit_tolerates_a_path_that_is_already_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let junit = temp.path().join("junit.xml");

        clear_previous_lane_junit(&junit).expect("an absent file is nothing to clear");
        fs::write(&junit, "stale").expect("write fixture");
        clear_previous_lane_junit(&junit).expect("a stale file is cleared");

        assert!(!junit.exists());
    }

    #[test]
    fn a_lane_that_is_not_a_plain_directory_name_is_refused() {
        for lane in ["../escape", "nested/lane", "", "/absolute"] {
            assert!(
                validate_lane_directory(lane).is_err(),
                "accepted `{lane}` as a lane"
            );
        }
        validate_lane_directory("reproduction-flash-off").expect("a plain name is a lane");
    }

    #[test]
    fn genuine_coordinator_error_has_failure_precedence() {
        let selected = choose_failure(
            Err(NotClean::reported("evidence")),
            Err(ChildFailure::inherited("nextest".to_owned(), Some(42))),
            Err(anyhow::anyhow!("cannot persist manifest")),
            Ok(()),
        )
        .expect("failure selected");

        assert_eq!(selected.to_string(), "cannot persist manifest");
    }

    #[test]
    fn child_status_has_precedence_over_evidence_verdict() {
        let selected = choose_failure(
            Err(NotClean::reported("evidence")),
            Err(ChildFailure::inherited("nextest".to_owned(), Some(42))),
            Ok(()),
            Ok(()),
        )
        .expect("failure selected");

        assert!(selected.downcast_ref::<ChildFailure>().is_some());
        assert_eq!(error_code(&selected), 42);
    }

    #[test]
    fn revision_contract_requires_a_full_hex_sha() {
        assert!(valid_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(!valid_sha("0123456789abcdef"));
        assert!(!valid_sha("g123456789abcdef0123456789abcdef01234567"));
    }

    #[test]
    fn provenance_failure_invalidates_a_passing_headline() {
        let mut markdown = "# Stress\n\n- Result: **PASSED**\n".to_owned();

        invalidate_result(&mut markdown);

        assert!(markdown.contains("Result: **INVALID PROVENANCE**"));
        assert!(!markdown.contains("Result: **PASSED**"));
    }

    #[test]
    fn report_output_cannot_overwrite_raw_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let raw = temp.path().join("raw");
        fs::create_dir(&raw).expect("create raw directory");

        let error = ensure_report_outside_raw(&raw, &raw.join("junit.xml"))
            .expect_err("raw overlap must be rejected");

        assert!(error.to_string().contains("outside the raw evidence"));
    }

    #[cfg(unix)]
    #[test]
    fn report_output_symlink_is_rejected_before_creating_inside_raw() {
        let temp = tempfile::tempdir().expect("tempdir");
        let raw = temp.path().join("raw");
        let alias = temp.path().join("alias");
        fs::create_dir(&raw).expect("create raw directory");
        symlink(&raw, &alias).expect("create raw alias");
        let output = alias.join("missing/report.md");

        let error = ensure_report_outside_raw(&raw, &output)
            .expect_err("symlinked raw overlap must be rejected");

        assert!(error.to_string().contains("outside the raw evidence"));
        assert!(!raw.join("missing").exists());
    }

    #[test]
    fn raw_output_cannot_overlap_subject_nextest_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("subject");
        fs::create_dir(&root).expect("create subject");
        let junit = root.join("target/nextest/stress/junit.xml");

        for raw in [
            root.join("target/nextest"),
            root.join("target/nextest/stress"),
            root.join("target/nextest/stress/raw"),
        ] {
            let error = ensure_raw_outside_subject_evidence(&raw, &junit)
                .expect_err("subject evidence overlap must be rejected");
            assert!(error.to_string().contains("must not overlap"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn raw_output_symlink_cannot_alias_subject_nextest_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let subject = temp.path().join("subject");
        let alias = temp.path().join("alias");
        fs::create_dir(&subject).expect("create subject");
        symlink(&subject, &alias).expect("create subject alias");
        let junit = subject.join("target/nextest/stress/junit.xml");
        let raw = alias.join("target/nextest/stress/raw");

        let error = ensure_raw_outside_subject_evidence(&raw, &junit)
            .expect_err("symlinked subject evidence overlap must be rejected");

        assert!(error.to_string().contains("must not overlap"));
    }
}
