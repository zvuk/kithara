//! Builds a bounded Markdown summary from a nextest stress `JUnit` report.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fmt::{Display, Write as _},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Deserialize;

use crate::{
    common::project::{StressEvidenceConfig, StressRenderBudgets},
    junit::{CaseTiming, parse_junit_report},
    verdict::NotClean,
};

mod evidence;
mod sanitizer;

pub(crate) use sanitizer::{ATTEMPT_MARKER, Findings, findings as sanitizer_findings};

const PERCENT_SCALE: usize = 100;
const PERCENT_HUNDREDTHS: usize = PERCENT_SCALE * PERCENT_SCALE;
const MAX_INVENTORY_CASES: usize = 100_000;
/// A repeat only qualifies for quarantine when it ran at least this many
/// cases: a narrow filter (one test, fifty repeats) must keep its honest
/// failures, and there mass failure is indistinguishable from the flake
/// itself.
const QUARANTINE_MIN_CASES: usize = 20;
/// Quarantine a repeat when `failed * SHARE >= cases` — a quarter of the
/// suite failing in one repeat. Real flake clusters stay far below this
/// (run #5's worst honest repeat lost under 1% of its cases), while an
/// environment event (evicted volume, vanished binaries) fails the suite
/// wholesale.
const QUARANTINE_FAIL_SHARE: usize = 4;
pub(crate) const MAX_INVENTORY_BYTES: u64 = 64 * 1_024 * 1_024;
pub(crate) const MAX_JUNIT_BYTES: u64 = 512 * 1_024 * 1_024;
/// Bounds a lane log, which a run appends to once per attempt.
pub(crate) const MAX_LANE_LOG_BYTES: u64 = 512 * 1_024 * 1_024;

#[derive(Debug, Args, fieldwork::Fieldwork)]
#[fieldwork(opt_in, with)]
pub(crate) struct StressReportArgs {
    /// Optional directory containing structured attempt envelopes.
    #[arg(long)]
    #[field(with = with_optional_envelopes, vis = "pub(crate)")]
    envelope_dir: Option<PathBuf>,
    /// Optional line evidence whose records carry nextest attempt identifiers.
    #[arg(long)]
    #[field(with = with_optional_lines, vis = "pub(crate)")]
    line_log: Option<PathBuf>,
    /// Optional one-second Linux host and cgroup pressure samples.
    #[arg(long)]
    #[field(with = with_pressure, option_set_some, vis = "pub(crate)")]
    pressure_log: Option<PathBuf>,
    /// Machine-readable output from `cargo nextest list` for the same selection.
    #[arg(long)]
    inventory: PathBuf,
    /// `JUnit` emitted by the nextest stress profile.
    #[arg(long)]
    junit: PathBuf,
    /// Markdown summary destination.
    #[arg(long)]
    output: PathBuf,
    #[arg(skip)]
    #[field(with, vis = "pub(crate)")]
    evidence: StressEvidenceConfig,
    #[arg(skip)]
    #[field(with, vis = "pub(crate)")]
    render: StressRenderBudgets,
    /// Explain an absent `JUnit` as fallout from the primary nextest step.
    #[arg(long)]
    #[field(with, vis = "pub(crate)")]
    allow_missing: bool,
    /// Number of stress iterations requested from nextest.
    #[arg(long)]
    expected_count: usize,
}

impl StressReportArgs {
    #[must_use]
    pub(crate) fn new(
        junit: PathBuf,
        inventory: PathBuf,
        output: PathBuf,
        expected_count: usize,
    ) -> Self {
        Self {
            junit,
            inventory,
            output,
            expected_count,
            line_log: None,
            envelope_dir: None,
            pressure_log: None,
            allow_missing: false,
            evidence: StressEvidenceConfig::default(),
            render: StressRenderBudgets::default(),
        }
    }
}

#[derive(Debug, Default)]
struct TestStats {
    failed_iterations: BTreeSet<usize>,
    observed_iterations: BTreeSet<usize>,
    max_secs: f64,
}

#[derive(Debug)]
struct RenderedReport {
    /// What each test did in this lane, kept so lanes can be put side by side.
    rates: BTreeMap<TestId, LaneRate>,
    /// Repeats [`quarantine_poisoned_iterations`] threw out, kept so the
    /// evidence census can be held to the same set as the rate tables.
    quarantined: BTreeSet<usize>,
    markdown: String,
    complete: bool,
}

#[derive(Debug)]
struct EvidenceProblems {
    rows: Vec<String>,
    invalid: bool,
    total: usize,
    row_budget: usize,
}

type TestId = (String, String);

#[derive(Debug, Deserialize)]
struct Inventory {
    #[serde(rename = "rust-suites")]
    rust_suites: BTreeMap<String, InventorySuite>,
}

#[derive(Debug, Deserialize)]
struct InventorySuite {
    testcases: BTreeMap<String, InventoryCase>,
    #[serde(rename = "binary-id")]
    binary_id: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct InventoryCase {
    #[serde(rename = "filter-match")]
    filter_match: InventoryMatch,
    ignored: bool,
}

#[derive(Debug, Deserialize)]
struct InventoryMatch {
    status: String,
}

impl EvidenceProblems {
    fn new(row_budget: usize) -> Self {
        Self {
            rows: Vec::new(),
            invalid: false,
            total: 0,
            row_budget,
        }
    }

    fn add(&mut self, problem: String, invalid: bool) {
        self.total = self.total.saturating_add(1);
        self.invalid |= invalid;
        if self.rows.len() < self.row_budget {
            self.rows.push(problem);
        }
    }
}

/// Summarizes one nextest stress run.
///
/// # Errors
///
/// Returns an error when the evidence is absent, incomplete, invalid, or
/// contains a failed attempt, the expected count is zero, or the output cannot
/// be written.
pub(crate) fn run(args: &StressReportArgs) -> Result<()> {
    let lane = lane_report(args)?;
    write_report(&args.output, &lane.markdown)?;
    lane.verdict
}

/// One lane's evidence: what it reads out of the artifact, and the verdict that
/// reading carries.
///
/// Rendering is separated from writing so that a run of several lanes can
/// put them all in one document. A file per lane would leave the question the
/// run exists to answer — which lane does this flake belong to — spread
/// across two documents for the reader to join by hand.
pub(crate) struct LaneReport {
    pub(crate) rates: BTreeMap<TestId, LaneRate>,
    /// How many of this lane's attempts the command rejected, for a lane whose
    /// verdict is an exit code rather than a set of test results.
    pub(crate) attempts: Option<LaneRate>,
    pub(crate) verdict: Result<()>,
    pub(crate) markdown: String,
    /// Whether every requested iteration is accounted for. A readable lane can
    /// still fall short of its own request — quarantined repeats, truncated
    /// output, a run that stopped early — and a rate measured over the
    /// survivors answers a different question than the run asked.
    pub(crate) complete: bool,
    /// Whether the lane produced valid per-attempt evidence at all. A lane
    /// whose artifact was missing or invalid has nothing to stand in a
    /// comparison — counting it as trustworthy is how a run summary
    /// contradicts its own per-lane verdicts.
    pub(crate) readable: bool,
}

/// How often one test failed in one lane.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct LaneRate {
    pub(crate) attempts: usize,
    pub(crate) failed: usize,
}

pub(crate) fn lane_report(args: &StressReportArgs) -> Result<LaneReport> {
    validate_expected_count(args.expected_count)?;
    let unreadable = |markdown: String| {
        Ok(LaneReport {
            markdown,
            rates: BTreeMap::new(),
            attempts: None,
            verdict: Err(NotClean::reported("stress evidence")),
            readable: false,
            complete: false,
        })
    };
    let inventory = match read_inventory(&args.inventory) {
        Ok(inventory) => inventory,
        Err(error) => {
            let detail = format!("{error:#}");
            return unreadable(render_invalid_artifact(
                "INVALID INVENTORY",
                args.expected_count,
                &args.inventory,
                &detail,
                &args.render,
            ));
        }
    };
    let xml = match read_bounded_utf8(&args.junit, MAX_JUNIT_BYTES, "stress JUnit") {
        Ok(xml) => xml,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return unreadable(render_missing(
                args.expected_count,
                &args.junit,
                args.allow_missing,
                &args.render,
            ));
        }
        Err(error) => {
            let detail = format!("{error:#}");
            return unreadable(render_invalid_artifact(
                "INVALID JUNIT",
                args.expected_count,
                &args.junit,
                &detail,
                &args.render,
            ));
        }
    };
    let mut junit = match parse_junit_report(&xml) {
        Ok(junit) => junit,
        Err(error) => {
            let detail = format!("{error:#}");
            return unreadable(render_invalid_artifact(
                "INVALID JUNIT",
                args.expected_count,
                &args.junit,
                &detail,
                &args.render,
            ));
        }
    };
    let has_failures = junit.cases.iter().any(|case| case.failed);
    if let Err(error) = validate_correlation_metadata(&junit) {
        return unreadable(render_invalid_artifact(
            "INVALID JUNIT",
            args.expected_count,
            &args.junit,
            &error,
            &args.render,
        ));
    }
    let mut report = render(
        &junit.cases,
        &inventory,
        args.expected_count,
        junit.run_id.as_deref(),
        junit.timestamp.as_deref(),
        &args.render,
    );
    retain_census_cases(&mut junit.cases, &report.quarantined);
    let correlated_complete = evidence::append_correlated_evidence(
        &mut report.markdown,
        &junit.cases,
        junit.run_id.as_deref(),
        args,
    );

    let truncated = junit
        .cases
        .iter()
        .filter(|case| case.output_truncated)
        .map(|case| {
            format!(
                "`{} {}`",
                markdown_cell(&case.suite, &args.render),
                markdown_cell(&case.name, &args.render)
            )
        })
        .collect::<Vec<_>>();
    if !truncated.is_empty() {
        let _ = writeln!(
            report.markdown,
            "\nEvidence problem: retained failure output of `{}` testcase(s) hit the per-case byte budget and lost its tail: {}",
            truncated.len(),
            truncated
                .iter()
                .take(args.render.signature_examples)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if !correlated_complete || !truncated.is_empty() {
        report.complete = false;
        mark_incomplete(&mut report.markdown);
    }
    let verdict = if report.complete && !has_failures {
        Ok(())
    } else {
        Err(NotClean::reported("stress evidence"))
    };
    Ok(LaneReport {
        verdict,
        markdown: report.markdown,
        rates: report.rates,
        attempts: None,
        readable: true,
        complete: report.complete,
    })
}

/// Check the inventory-by-iteration contract before the run records its
/// primary exit status. The full reporter later writes the actionable detail;
/// this narrow verdict prevents a missing or partial `JUnit` from being
/// mistaken for a successful nextest stress run.
///
/// # Errors
///
/// Returns a concise stress verdict when either artifact is unavailable or
/// invalid, any selected attempt is absent, or any recorded attempt failed.
pub(crate) fn validate_primary_evidence(
    inventory_path: &Path,
    junit_path: &Path,
    expected_count: usize,
    budgets: &StressRenderBudgets,
) -> Result<()> {
    validate_expected_count(expected_count)?;
    let inventory = read_inventory(inventory_path).map_err(primary_evidence_finding)?;
    let xml = read_bounded_utf8(junit_path, MAX_JUNIT_BYTES, "stress JUnit")
        .map_err(primary_evidence_finding)?;
    let junit = parse_junit_report(&xml).map_err(primary_evidence_finding)?;
    validate_correlation_metadata(&junit).map_err(primary_evidence_finding)?;
    let report = render(
        &junit.cases,
        &inventory,
        expected_count,
        junit.run_id.as_deref(),
        junit.timestamp.as_deref(),
        budgets,
    );
    let failed = junit.cases.iter().filter(|case| case.failed).count();
    if report.complete && failed == 0 {
        Ok(())
    } else {
        println!(
            "stress run evidence: complete={complete}, failed cases={failed}",
            complete = report.complete,
        );
        Err(NotClean::reported("stress run evidence"))
    }
}

/// Names the finding a verdict would otherwise swallow.
///
/// `NotClean` stands for a check whose findings are printed above it; a reader
/// error converted without printing leaves a verdict with nothing above it,
/// and the one fact that names the unreadable path is the fact that is lost.
fn primary_evidence_finding(error: impl Display) -> anyhow::Error {
    println!("stress run evidence: {error:#}");
    NotClean::reported("stress run evidence")
}

fn validate_correlation_metadata(junit: &crate::junit::JunitReport) -> Result<(), String> {
    if junit.run_id.as_deref().is_none_or(str::is_empty) {
        return Err("nextest JUnit root uuid is missing".to_owned());
    }
    let Some(timestamp) = &junit.timestamp else {
        return Err("nextest JUnit root timestamp is missing".to_owned());
    };
    if evidence::parse_timestamp_ms(timestamp).is_none() {
        return Err("nextest JUnit root timestamp is malformed".to_owned());
    }
    for case in &junit.cases {
        let Some(timestamp) = case.timestamp.as_deref() else {
            return Err(format!(
                "testcase timestamp is missing on {} {}",
                case.suite, case.name
            ));
        };
        if evidence::parse_timestamp_ms(timestamp).is_none() {
            return Err(format!(
                "testcase timestamp is malformed on {} {}",
                case.suite, case.name
            ));
        }
    }
    Ok(())
}

fn render_missing(
    expected_count: usize,
    junit: &Path,
    allow_missing: bool,
    budgets: &StressRenderBudgets,
) -> String {
    let explanation = if allow_missing {
        "No JUnit was staged for this lane: either nextest died before writing one, or the run looked for it at a path nextest does not write. The primary step log names which."
    } else {
        "The required per-iteration evidence was not produced. Inspect the command and input path."
    };
    format!(
        "# Stress evidence\n\n- Result: **NO JUNIT**\n- Requested iterations: `{expected_count}`\n- JUnit path: `{}`\n\n{explanation}\n",
        markdown_cell(&junit.display().to_string(), budgets),
    )
}

fn render_invalid_artifact(
    result: &str,
    expected_count: usize,
    artifact: &Path,
    detail: &str,
    budgets: &StressRenderBudgets,
) -> String {
    format!(
        "# Stress evidence\n\n- Result: **{result}**\n- Requested iterations: `{expected_count}`\n- Evidence path: `{}`\n\n## Evidence problems\n\n- {}\n",
        markdown_cell(&artifact.display().to_string(), budgets),
        markdown_cell(detail, budgets),
    )
}

fn read_inventory(path: &Path) -> Result<BTreeSet<TestId>> {
    let json = read_bounded_utf8(path, MAX_INVENTORY_BYTES, "stress inventory")?;
    parse_inventory(&json)
}

pub(crate) fn read_bounded_utf8(path: &Path, max_bytes: u64, artifact: &str) -> Result<String> {
    let file =
        File::open(path).with_context(|| format!("open {artifact} at {}", path.display()))?;
    let length = file
        .metadata()
        .with_context(|| format!("read {artifact} metadata at {}", path.display()))?
        .len();
    if length > max_bytes {
        bail!(
            "{artifact} exceeds the deterministic limit of {max_bytes} bytes (observed {length}); the raw artifact was left untouched"
        );
    }
    let capacity = usize::try_from(length).context("artifact length does not fit in memory")?;
    let max_capacity =
        usize::try_from(max_bytes).context("artifact byte limit does not fit in memory")?;
    let read_capacity = capacity
        .checked_add(1)
        .context("artifact read capacity overflow")?;
    let mut bytes = Vec::with_capacity(read_capacity);
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {artifact} at {}", path.display()))?;
    if bytes.len() > max_capacity {
        bail!(
            "{artifact} exceeds the deterministic limit of {max_bytes} bytes; the raw artifact was left untouched"
        );
    }
    String::from_utf8(bytes).with_context(|| format!("{artifact} is not UTF-8"))
}

pub(crate) fn validate_inventory(json: &str) -> Result<()> {
    parse_inventory(json).map(|_| ())
}

fn parse_inventory(json: &str) -> Result<BTreeSet<TestId>> {
    let inventory: Inventory = serde_json::from_str(json).context("parse stress inventory JSON")?;
    if inventory.rust_suites.is_empty() {
        bail!("stress inventory contains no Rust test suites");
    }
    let mut tests = BTreeSet::new();
    let mut inventory_cases = 0usize;
    for (suite, inventory) in inventory.rust_suites {
        if suite.trim().is_empty() || inventory.binary_id.trim().is_empty() {
            bail!("stress inventory contains an empty suite identity");
        }
        if suite != inventory.binary_id {
            bail!("stress inventory suite key does not match its binary-id");
        }
        match inventory.status.as_str() {
            "listed" => {}
            // A target the project's `default-filter` excludes is inventoried
            // with this status and none of its cases. It owes the run
            // nothing, and reading the exclusion as a malformed inventory is
            // how a run ends before its first test. The suite is dropped
            // whole rather than filtered case by case, so a future nextest
            // that does list them still cannot contribute any.
            "skipped-default-filter" => continue,
            status => bail!("stress inventory suite `{suite}` has unsupported status `{status}`"),
        }
        for (name, case) in inventory.testcases {
            inventory_cases = inventory_cases.saturating_add(1);
            validate_inventory_case_count(inventory_cases)?;
            if name.trim().is_empty() {
                bail!("stress inventory contains an empty test name");
            }
            match case.filter_match.status.as_str() {
                "matches" if !case.ignored => {
                    tests.insert((suite.clone(), name));
                }
                "matches" | "mismatch" => {}
                status => bail!("stress inventory contains unknown filter status `{status}`"),
            }
        }
    }
    if tests.is_empty() {
        bail!("stress inventory contains no runnable selected tests");
    }
    Ok(tests)
}

fn validate_inventory_case_count(count: usize) -> Result<()> {
    if count > MAX_INVENTORY_CASES {
        bail!(
            "stress inventory exceeds the deterministic limit of {MAX_INVENTORY_CASES} testcases"
        );
    }
    Ok(())
}

fn validate_expected_count(expected_count: usize) -> Result<()> {
    if expected_count == 0 {
        bail!("expected-count must be greater than zero");
    }
    Ok(())
}

/// The lanes of one run, side by side, ordered by how much they disagree.
///
/// Disagreement is what the table is for. A test that fails at the same rate on
/// both clocks is a flake that owes nothing to either of them; a test that
/// fails on one and not the other is the run's whole point, and it must
/// not be buried under a hundred rows of the first kind.
///
/// A test present in one lane and absent from the other is reported as absent
/// rather than as zero: the lanes do not select the same tests, because targets
/// behind a feature exist in one and not the other, and calling that a rate of
/// zero would invent a passing result for a test that never ran.
/// Lanes measured by attempts are reported separately. Their verdict is one
/// exit code per attempt, so they have no per-test rate to place in the table
/// above and would otherwise be a column of tests that were never selected.
/// Lanes kept out of the comparison are named with the reason that kept them
/// out. A run that silently drops a lane reads as though it covered every
/// lane it requested, which is the one thing the summary must never imply.
pub(crate) fn render_lane_comparison(
    lanes: &[(String, BTreeMap<TestId, LaneRate>)],
    commanded: &[(String, LaneRate)],
    excluded: &[(String, String)],
    requested: usize,
    budgets: &StressRenderBudgets,
) -> String {
    let mut out = String::from("# Stress run\n");
    let _ = writeln!(out, "\n- Lanes requested: `{requested}`");
    let _ = writeln!(
        out,
        "- Lanes with trustworthy evidence: `{}`",
        lanes.len() + commanded.len()
    );
    render_excluded_lanes(&mut out, excluded, budgets);
    render_per_test_comparison(&mut out, lanes, budgets);
    render_attempt_comparison(&mut out, commanded, budgets);
    out
}

fn render_excluded_lanes(
    out: &mut String,
    excluded: &[(String, String)],
    budgets: &StressRenderBudgets,
) {
    if excluded.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "- Lanes excluded from comparison: `{}`",
        excluded.len()
    );
    out.push_str("\n## Lanes excluded from comparison\n\n| lane | reason |\n|---|---|\n");
    for (name, reason) in excluded {
        let _ = writeln!(
            out,
            "| `{}` | {} |",
            markdown_cell(name, budgets),
            markdown_cell(reason, budgets)
        );
    }
}

fn render_attempt_comparison(
    out: &mut String,
    commanded: &[(String, LaneRate)],
    budgets: &StressRenderBudgets,
) {
    if commanded.is_empty() {
        return;
    }
    out.push_str("\n## Failure rate by attempt\n\n| lane | rate |\n|---|---:|\n");
    for (name, rate) in commanded {
        let _ = writeln!(
            out,
            "| `{}` | {} ({}/{}) |",
            markdown_cell(name, budgets),
            rate_percent(rate.failed, rate.attempts),
            rate.failed,
            rate.attempts
        );
    }
}

fn render_per_test_comparison(
    out: &mut String,
    lanes: &[(String, BTreeMap<TestId, LaneRate>)],
    budgets: &StressRenderBudgets,
) {
    if lanes.len() < 2 {
        out.push_str(
            "\nA comparison needs two verified lanes. The lane sections below stand on their own.\n",
        );
        return;
    }

    let mut rows = BTreeMap::<TestId, Vec<Option<LaneRate>>>::new();
    for (index, (_, rates)) in lanes.iter().enumerate() {
        for (id, rate) in rates {
            rows.entry(id.clone())
                .or_insert_with(|| vec![None; lanes.len()])[index] = Some(*rate);
        }
    }
    let mut ranked = rows
        .into_iter()
        .filter(|(_, cells)| cells.iter().flatten().any(|rate| rate.failed > 0))
        .map(|(id, cells)| {
            let spread = disagreement(&cells);
            (spread, id, cells)
        })
        .collect::<Vec<_>>();
    if ranked.is_empty() {
        out.push_str("\nNo test failed in any lane.\n");
        return;
    }
    ranked.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
    });

    out.push_str("\n## Failure rate by test\n\n| test |");
    for (name, _) in lanes {
        let _ = write!(out, " {} |", markdown_cell(name, budgets));
    }
    out.push_str(" holds in |\n|---|");
    for _ in lanes {
        out.push_str("---:|");
    }
    out.push_str("---|\n");
    for (_, (suite, name), cells) in ranked.iter().take(budgets.failure_rows) {
        let _ = write!(
            out,
            "| `{}` |",
            markdown_cell(&format!("{suite} {name}"), budgets)
        );
        for cell in cells {
            match cell {
                Some(rate) => {
                    let _ = write!(
                        out,
                        " {} ({}/{}) |",
                        rate_percent(rate.failed, rate.attempts),
                        rate.failed,
                        rate.attempts
                    );
                }
                None => out.push_str(" not selected |"),
            }
        }
        let _ = writeln!(
            out,
            " {} |",
            markdown_cell(&lane_span(lanes, cells), budgets)
        );
    }
    if ranked.len() > budgets.failure_rows {
        let rows = budgets.failure_rows;
        let _ = writeln!(
            out,
            "\nShowing the first {rows} of {} tests that failed somewhere.",
            ranked.len()
        );
    }
}

/// Which lanes a test's redness survives in, as one phrase.
///
/// The rate columns already carry this, but reading it off them means holding
/// three numbers at once and knowing which lane means what. Run
/// 32075786002 cost an afternoon to that: `packaged_abr_switch` is 2/50 on
/// `reproduction-flash-on` and 0/50 on both of the other lanes, so the virtual
/// clock is the whole defect and no product path is implicated — but the table
/// said that only to a reader who compared the columns by hand.
fn lane_span(lanes: &[(String, BTreeMap<TestId, LaneRate>)], cells: &[Option<LaneRate>]) -> String {
    let selected = cells.iter().flatten().count();
    let red = cells
        .iter()
        .enumerate()
        .filter(|(_, cell)| cell.is_some_and(|rate| rate.failed > 0))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    match (selected, red.as_slice()) {
        (0 | 1, _) => "only one lane ran it".to_string(),
        (selected, red) if red.len() == selected => "every lane".to_string(),
        (_, [index]) => lanes.get(*index).map_or_else(
            || "one lane".to_string(),
            |(name, _)| format!("only {name}"),
        ),
        (selected, red) => format!("{} of {selected} lanes", red.len()),
    }
}

/// How far apart the lanes are on one test, as a fraction.
///
/// A test selected by only some of the lanes is maximally interesting: the
/// lanes cannot even be compared on it, and the reader should see it first.
fn disagreement(cells: &[Option<LaneRate>]) -> f64 {
    if cells.iter().any(Option::is_none) {
        return f64::INFINITY;
    }
    let rates = cells
        .iter()
        .flatten()
        .map(|rate| {
            if rate.attempts == 0 {
                0.0
            } else {
                f64::from(u32::try_from(rate.failed).unwrap_or(u32::MAX))
                    / f64::from(u32::try_from(rate.attempts).unwrap_or(u32::MAX))
            }
        })
        .collect::<Vec<_>>();
    let high = rates.iter().copied().fold(f64::MIN, f64::max);
    let low = rates.iter().copied().fold(f64::MAX, f64::min);
    high - low
}

/// What a command lane's own test runner recorded, gathered from the reports the
/// lane kept.
///
/// Counted per execution rather than per attempt, because those are not the same
/// number: a lane launched fifty times records one execution of each test per
/// launch, and a lane launched once with a repeat count records fifty in a single
/// report. Counting attempts would call the second one "one in one".
#[derive(Debug, Default)]
pub(crate) struct AttemptRecords {
    /// Failed and total executions of each test the reports name.
    pub(crate) rates: BTreeMap<TestId, LaneRate>,
    /// Attempts each failing test failed in.
    failed_in: BTreeMap<TestId, BTreeSet<usize>>,
    /// Rejected attempts that wrote no report at all.
    silent: BTreeSet<usize>,
    /// Attempts whose report could not be read.
    unreadable: BTreeSet<usize>,
    /// How many attempts the lane recorded.
    attempts: usize,
}

impl AttemptRecords {
    pub(crate) fn is_empty(&self) -> bool {
        self.rates.is_empty() && self.silent.is_empty() && self.unreadable.is_empty()
    }

    /// The most executions any one test recorded.
    ///
    /// This is what a lane that repeats internally can be held to: its report
    /// must show the repeats that were asked for, or the run stopped short of the
    /// run it claims to be.
    pub(crate) fn repeats(&self) -> usize {
        self.rates
            .values()
            .map(|rate| rate.attempts)
            .max()
            .unwrap_or(0)
    }
}

/// Reads every report a command lane kept and counts what its runner recorded.
///
/// A rejected attempt that left no report at all is recorded too, and is the more
/// serious of the two: the process died before the runner could write anything —
/// the signature of a crash outside any test.
pub(crate) fn attempt_records(directory: &Path, codes: &[i32]) -> AttemptRecords {
    let mut records = AttemptRecords {
        attempts: codes.len(),
        ..AttemptRecords::default()
    };
    for (attempt, code) in codes.iter().enumerate() {
        let kept = directory.join(format!("attempt-{attempt}.xml"));
        let Ok(xml) = fs::read_to_string(&kept) else {
            if *code != 0 {
                records.silent.insert(attempt);
            }
            continue;
        };
        match parse_junit_report(&xml) {
            Ok(report) => {
                for case in &report.cases {
                    let id = (case.suite.clone(), case.name.clone());
                    let rate = records.rates.entry(id.clone()).or_default();
                    rate.attempts += 1;
                    if case.failed {
                        rate.failed += 1;
                        records.failed_in.entry(id).or_default().insert(attempt);
                    }
                }
            }
            Err(_) => {
                records.unreadable.insert(attempt);
            }
        }
    }
    records
}

/// Names what a command lane's own test runner recorded.
///
/// An exit code says a run was rejected; it does not say in which test. When the
/// command runs its tests under a runner that writes a report, this is where that
/// report becomes the sentence a reader needs: this test, this often.
pub(crate) fn append_attempt_reports(
    out: &mut String,
    records: &AttemptRecords,
    budgets: &StressRenderBudgets,
) {
    if records.is_empty() {
        return;
    }
    let failures = records
        .rates
        .iter()
        .filter(|(_, rate)| rate.failed > 0)
        .collect::<Vec<_>>();
    out.push_str("\n## What the lane's own runner recorded\n");
    if failures.is_empty() {
        // A heading with nothing under it reads as evidence that failed to
        // arrive. What actually happened — the runner reported, over this many
        // tests and this many repeats, and none of them failed — is the reason
        // the lane is green, so it belongs here as a sentence.
        let _ = writeln!(
            out,
            "\nNo test failed in any repeat the runner reported: `{}` test(s) over `{}` repeat(s).",
            records.rates.len(),
            records.repeats()
        );
    } else {
        // The attempt a failure belongs to is only worth a column when there is
        // more than one attempt to tell apart. A lane that repeats inside a
        // single launch would print the same "0" on every row.
        let per_attempt = records.attempts > 1;
        out.push_str(if per_attempt {
            "\n| test | rate | attempts |\n|---|---:|---|\n"
        } else {
            "\n| test | rate |\n|---|---:|\n"
        });
        let mut rows = failures;
        rows.sort_by_key(|(id, rate)| (Reverse(rate.failed), (*id).clone()));
        for (id, rate) in rows.into_iter().take(budgets.failure_rows) {
            let named = markdown_cell(&format!("{} {}", id.0, id.1), budgets);
            let measured = format!(
                "{} ({}/{})",
                rate_percent(rate.failed, rate.attempts),
                rate.failed,
                rate.attempts
            );
            if per_attempt {
                let attempts = records.failed_in.get(id).cloned().unwrap_or_default();
                let _ = writeln!(
                    out,
                    "| `{named}` | {measured} | {} |",
                    markdown_cell(&join_attempts(&attempts, budgets), budgets)
                );
            } else {
                let _ = writeln!(out, "| `{named}` | {measured} |");
            }
        }
    }
    if !records.silent.is_empty() {
        let _ = writeln!(
            out,
            "\n- Rejected attempts that wrote no report (died before any verdict): `{}`",
            markdown_cell(&join_attempts(&records.silent, budgets), budgets)
        );
    }
    if !records.unreadable.is_empty() {
        let _ = writeln!(
            out,
            "\n- Attempts whose report could not be read: `{}`",
            markdown_cell(&join_attempts(&records.unreadable, budgets), budgets)
        );
    }
}

fn join_attempts(attempts: &BTreeSet<usize>, budgets: &StressRenderBudgets) -> String {
    attempts
        .iter()
        .take(budgets.iterations_per_test)
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn write_report(path: &Path, markdown: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create stress report directory {}", parent.display()))?;
    }
    fs::write(path, markdown).with_context(|| format!("write stress report {}", path.display()))
}

fn mark_incomplete(markdown: &mut String) {
    for result in ["PASSED", "FAILED"] {
        let marker = format!("- Result: **{result}**");
        if let Some(index) = markdown.find(&marker) {
            markdown.replace_range(index..index + marker.len(), "- Result: **INCOMPLETE**");
            return;
        }
    }
}

/// Validate each testcase against the selection and fold the survivors into
/// per-test stats; every rejection becomes an evidence problem.
fn collect_stats(
    cases: &[CaseTiming],
    inventory: &BTreeSet<TestId>,
    expected_count: usize,
    problems: &mut EvidenceProblems,
    budgets: &StressRenderBudgets,
) -> (BTreeMap<TestId, TestStats>, BTreeSet<usize>) {
    let mut tests = BTreeMap::<TestId, TestStats>::new();
    let mut observed_iterations = BTreeSet::new();
    let mut unique_cases = BTreeSet::new();
    if cases.is_empty() {
        problems.add("the JUnit report contains no testcases".to_owned(), false);
    }
    for case in cases {
        if case.suite.trim().is_empty() || case.name.trim().is_empty() {
            problems.add("a testcase has an empty suite or name".to_owned(), true);
            continue;
        }
        let id = (case.suite.clone(), case.name.clone());
        if !inventory.contains(&id) {
            problems.add(
                format!(
                    "JUnit contains unselected test `{}`",
                    test_id(case, budgets)
                ),
                true,
            );
            continue;
        }
        let Some(iteration) = case.iteration else {
            problems.add(
                format!("test `{}` has no stress iteration", test_id(case, budgets)),
                true,
            );
            continue;
        };
        if iteration >= expected_count {
            problems.add(
                format!(
                    "test `{}` has out-of-range iteration {iteration}",
                    test_id(case, budgets)
                ),
                true,
            );
            continue;
        }
        if !unique_cases.insert((case.suite.clone(), case.name.clone(), iteration)) {
            problems.add(
                format!(
                    "test `{}` has duplicate iteration {iteration}",
                    test_id(case, budgets)
                ),
                true,
            );
            continue;
        }
        let stats = tests
            .entry((case.suite.clone(), case.name.clone()))
            .or_default();
        stats.max_secs = stats.max_secs.max(case.secs);
        observed_iterations.insert(iteration);
        stats.observed_iterations.insert(iteration);
        if case.failed {
            stats.failed_iterations.insert(iteration);
        }
    }
    (tests, observed_iterations)
}

fn render(
    cases: &[CaseTiming],
    inventory: &BTreeSet<TestId>,
    expected_count: usize,
    run_id: Option<&str>,
    timestamp: Option<&str>,
    budgets: &StressRenderBudgets,
) -> RenderedReport {
    let mut problems = EvidenceProblems::new(budgets.problem_rows);
    let (mut tests, mut observed_iterations) =
        collect_stats(cases, inventory, expected_count, &mut problems, budgets);

    let quarantined = quarantine_poisoned_iterations(&mut tests, &mut observed_iterations);
    for (iteration, (failed, total)) in &quarantined {
        problems.add(
            format!(
                "iteration {iteration} failed en masse ({failed} of {total} cases) and was quarantined as environment poisoning"
            ),
            false,
        );
    }

    add_coverage_problem(
        &mut problems,
        "the report",
        &observed_iterations,
        expected_count,
        budgets,
    );
    for (suite, name) in inventory {
        if !tests.contains_key(&(suite.clone(), name.clone())) {
            let id = markdown_cell(&format!("{suite} {name}"), budgets);
            problems.add(
                format!("selected test `{id}` is absent from the JUnit report"),
                false,
            );
        }
    }
    for ((suite, name), stats) in &tests {
        let id = markdown_cell(&format!("{suite} {name}"), budgets);
        let subject = format!("test `{id}`");
        add_coverage_problem(
            &mut problems,
            &subject,
            &stats.observed_iterations,
            expected_count,
            budgets,
        );
    }

    let rates = tests
        .iter()
        .map(|(id, stats)| {
            (
                id.clone(),
                LaneRate {
                    failed: stats.failed_iterations.len(),
                    attempts: stats.observed_iterations.len(),
                },
            )
        })
        .collect();
    let observed_count = observed_iterations.len();
    let complete = problems.total == 0;
    let failed_attempts = tests
        .values()
        .map(|stats| stats.failed_iterations.len())
        .sum::<usize>();
    let attempts = tests
        .values()
        .map(|stats| stats.observed_iterations.len())
        .sum::<usize>();
    let result = if problems.invalid {
        "INVALID JUNIT"
    } else if !complete {
        "INCOMPLETE"
    } else if failed_attempts > 0 {
        "FAILED"
    } else {
        "PASSED"
    };

    let mut out = String::from("# Stress evidence\n\n");
    let _ = writeln!(out, "- Result: **{result}**");
    if let Some(run_id) = run_id {
        let _ = writeln!(
            out,
            "- Nextest run ID: `{}`",
            markdown_cell(run_id, budgets)
        );
    }
    if let Some(timestamp) = timestamp {
        let _ = writeln!(
            out,
            "- Run started: `{}`",
            markdown_cell(timestamp, budgets)
        );
    }
    let _ = writeln!(out, "- Requested iterations: `{expected_count}`");
    let _ = writeln!(out, "- Observed iterations: `{observed_count}`");
    let _ = writeln!(out, "- Tests observed: `{}`", tests.len());
    let _ = writeln!(out, "- Tests selected: `{}`", inventory.len());
    let _ = writeln!(out, "- Testcases read: `{}`", cases.len());
    let _ = writeln!(out, "- Unique test iterations: `{attempts}`");
    let _ = writeln!(out, "- Failed attempts: `{failed_attempts}`");
    let _ = writeln!(out, "- Quarantined iterations: `{}`", quarantined.len());

    if problems.total > 0 {
        out.push_str("\n## Evidence problems\n");
        for problem in &problems.rows {
            let _ = writeln!(out, "\n- {problem}");
        }
        if problems.total > problems.rows.len() {
            let _ = writeln!(
                out,
                "\n- ... and {} more problems; inspect the JUnit artifact",
                problems.total - problems.rows.len()
            );
        }
    }

    render_quarantine(&mut out, &quarantined);
    render_failures(&mut out, tests, budgets);
    RenderedReport {
        complete,
        rates,
        markdown: out,
        quarantined: quarantined.into_keys().collect(),
    }
}

/// Hold the evidence census to the repeats the rate tables kept.
///
/// Run 32068408884 lost repeats 23-49 of its `flash-off` lane to a host
/// that stopped executing the binaries. The rate tables quarantined them, but
/// the census still read every case: its only symptom cluster was 98226
/// poisoned failures, and all 412 of its "divergent" lines were the difference
/// between a repeat that ran and one that did not.
fn retain_census_cases(cases: &mut Vec<CaseTiming>, quarantined: &BTreeSet<usize>) {
    if quarantined.is_empty() {
        return;
    }
    cases.retain(|case| {
        case.iteration
            .is_none_or(|iteration| !quarantined.contains(&iteration))
    });
}

/// Pull environment-poisoned repeats out of the aggregate before any rate is
/// computed.
///
/// A flake is a property of a test: it fails in some repeats while the suite
/// around it passes. A repeat in which a quarter or more of the suite fails
/// at once is a property of the environment — run #5 lost repeats 44-50
/// to an external volume eviction mid-run, and the report then attributed
/// ~1200 phantom per-test rates to tests whose binaries had simply vanished.
/// Such repeats leave every rate; the report names them and their coverage
/// gap instead.
fn quarantine_poisoned_iterations(
    tests: &mut BTreeMap<TestId, TestStats>,
    observed: &mut BTreeSet<usize>,
) -> BTreeMap<usize, (usize, usize)> {
    let mut per_iteration = BTreeMap::<usize, (usize, usize)>::new();
    for stats in tests.values() {
        for &iteration in &stats.observed_iterations {
            per_iteration.entry(iteration).or_default().1 += 1;
        }
        for &iteration in &stats.failed_iterations {
            per_iteration.entry(iteration).or_default().0 += 1;
        }
    }
    let quarantined = per_iteration
        .into_iter()
        .filter(|&(_, (failed, total))| {
            total >= QUARANTINE_MIN_CASES && failed.saturating_mul(QUARANTINE_FAIL_SHARE) >= total
        })
        .collect::<BTreeMap<_, _>>();
    if quarantined.is_empty() {
        return quarantined;
    }
    for stats in tests.values_mut() {
        stats
            .observed_iterations
            .retain(|iteration| !quarantined.contains_key(iteration));
        stats
            .failed_iterations
            .retain(|iteration| !quarantined.contains_key(iteration));
    }
    observed.retain(|iteration| !quarantined.contains_key(iteration));
    quarantined
}

fn render_quarantine(out: &mut String, quarantined: &BTreeMap<usize, (usize, usize)>) {
    if quarantined.is_empty() {
        return;
    }
    out.push_str(
        "\n## Quarantined repeats\n\nA repeat in which a quarter or more of the suite fails at once is an environment event (an evicted volume, vanished binaries), not a property of the tests. These repeats are excluded from every rate in this report; the raw JUnit artifact remains exhaustive.\n\n| iteration (zero-based) | failed / cases |\n|---:|---:|\n",
    );
    for (iteration, (failed, total)) in quarantined {
        let _ = writeln!(out, "| {iteration} | {failed} / {total} |");
    }
}

fn render_failures(
    out: &mut String,
    tests: BTreeMap<TestId, TestStats>,
    budgets: &StressRenderBudgets,
) {
    let mut failures = tests
        .into_iter()
        .filter(|(_, stats)| !stats.failed_iterations.is_empty())
        .collect::<Vec<_>>();
    failures.sort_by(|left, right| {
        Reverse(left.1.failed_iterations.len())
            .cmp(&Reverse(right.1.failed_iterations.len()))
            .then_with(|| left.0.cmp(&right.0))
    });
    if failures.is_empty() {
        out.push_str("\nNo failed attempts were recorded.\n");
        return;
    }

    let _ = writeln!(
        out,
        "\n## Failed tests\n\n| test | failed / attempts | rate | failed iterations (zero-based) | max |\n|---|---:|---:|---|---:|"
    );
    for ((suite, name), stats) in failures.iter().take(budgets.failure_rows) {
        let failures = stats.failed_iterations.len();
        let attempts = stats.observed_iterations.len();
        let rate = rate_percent(failures, attempts);
        let iterations = render_iterations(&stats.failed_iterations, budgets);
        let id = markdown_cell(&format!("{suite} {name}"), budgets);
        let _ = writeln!(
            out,
            "| `{id}` | {} / {} | {rate} | {iterations} | {:.0} ms |",
            failures,
            attempts,
            stats.max_secs * 1000.0,
        );
    }
    if failures.len() > budgets.failure_rows {
        let rows = budgets.failure_rows;
        let _ = writeln!(
            out,
            "\nShowing the first {rows} of {} failed tests. The JUnit artifact is exhaustive.",
            failures.len()
        );
    }
}

fn add_coverage_problem(
    problems: &mut EvidenceProblems,
    subject: &str,
    observed: &BTreeSet<usize>,
    expected_count: usize,
    budgets: &StressRenderBudgets,
) {
    let in_range = observed.range(..expected_count).count();
    let missing_count = expected_count.saturating_sub(in_range);
    if missing_count == 0 {
        return;
    }
    let missing = (0..expected_count)
        .filter(|iteration| !observed.contains(iteration))
        .take(budgets.iterations_per_test)
        .collect::<BTreeSet<_>>();
    let mut detail = render_iterations(&missing, budgets);
    if missing_count > budgets.iterations_per_test {
        detail = format!("{detail}, ... ({missing_count} total)");
    }
    problems.add(
        format!("{subject} is missing requested iterations: {detail}"),
        false,
    );
}

fn test_id(case: &CaseTiming, budgets: &StressRenderBudgets) -> String {
    markdown_cell(&format!("{} {}", case.suite, case.name), budgets)
}

pub(crate) fn rate_percent(failures: usize, attempts: usize) -> String {
    if attempts == 0 {
        return "0.00%".to_owned();
    }
    let hundredths = failures.saturating_mul(PERCENT_HUNDREDTHS) / attempts;
    format!(
        "{}.{:02}%",
        hundredths / PERCENT_SCALE,
        hundredths % PERCENT_SCALE
    )
}

fn render_iterations(iterations: &BTreeSet<usize>, budgets: &StressRenderBudgets) -> String {
    if iterations.is_empty() {
        return "unknown".to_owned();
    }
    let mut rendered = iterations
        .iter()
        .take(budgets.iterations_per_test)
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    if iterations.len() > budgets.iterations_per_test {
        let _ = write!(rendered, ", ... ({} total)", iterations.len());
    }
    rendered
}

/// Sanitize text for one Markdown table cell, bounding it around the middle.
///
/// An over-long diagnostic keeps its head (level, target, message) *and* its
/// tail: tracing lines put the load-bearing fields last (run #5's
/// readiness line lost `queued=` to a tail cut, the one field separating "a
/// stall" from "still fetching"), and assertion messages put their values
/// last.
fn markdown_cell(text: &str, budgets: &StressRenderBudgets) -> String {
    let sanitized = text
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
        .replace('`', "'");
    let total = sanitized.chars().count();
    if total <= budgets.cell_chars {
        return sanitized;
    }
    let head_chars = budgets.cell_chars * 2 / 3;
    let tail_chars = budgets.cell_chars - head_chars;
    let head = sanitized.chars().take(head_chars).collect::<String>();
    let tail = sanitized
        .chars()
        .skip(total - tail_chars)
        .collect::<String>();
    format!("{head}...{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &str, iteration: usize, failed: bool, secs: f64) -> CaseTiming {
        CaseTiming {
            failed,
            secs,
            name: name.to_owned(),
            suite: "demo::tests".to_owned(),
            iteration: Some(iteration),
            timestamp: None,
            output: String::new(),
            output_truncated: false,
        }
    }

    fn inventory(names: &[&str]) -> BTreeSet<TestId> {
        names
            .iter()
            .map(|name| ("demo::tests".to_owned(), (*name).to_owned()))
            .collect()
    }

    #[test]
    fn reports_failure_rate_and_exact_iterations() {
        let cases = vec![
            case("seek", 0, false, 0.1),
            case("seek", 1, true, 0.25),
            case("seek", 2, false, 0.2),
            case("other", 0, false, 0.01),
            case("other", 1, false, 0.01),
            case("other", 2, false, 0.01),
        ];

        let report = render(
            &cases,
            &inventory(&["seek", "other"]),
            3,
            None,
            None,
            &StressRenderBudgets::default(),
        );
        let markdown = &report.markdown;

        assert!(report.complete, "{markdown}");
        assert!(markdown.contains("Result: **FAILED**"), "{markdown}");
        assert!(markdown.contains("1 / 3"), "{markdown}");
        assert!(markdown.contains("33.33%"), "{markdown}");
        assert!(markdown.contains("| 1 | 250 ms |"), "{markdown}");
        assert!(!markdown.contains("demo::tests other"), "{markdown}");
    }

    #[test]
    fn a_bounded_cell_keeps_the_line_tail() {
        let line = format!("DEBUG target: {} queued=7", "x".repeat(400));

        let cell = markdown_cell(&line, &StressRenderBudgets::default());

        assert!(cell.ends_with("queued=7"), "{cell}");
    }

    /// The cell bound is config, not a constant: a project that narrows it
    /// gets a narrower cell, still head-and-tail rather than head-only.
    #[test]
    fn a_narrowed_cell_budget_bounds_the_cell_at_the_configured_width() {
        let budgets = StressRenderBudgets {
            cell_chars: 30,
            ..StressRenderBudgets::default()
        };
        let line = "h".repeat(20) + &"t".repeat(20);

        let cell = markdown_cell(&line, &budgets);

        assert_eq!(cell.chars().count(), 33);
        assert!(cell.starts_with(&"h".repeat(20)));
        assert!(cell.ends_with(&"t".repeat(10)));
    }

    /// The failure table is bounded by config: a project that lowers the
    /// budget sees fewer rows and an accurate count of what it is not seeing.
    #[test]
    fn a_lowered_failure_budget_shortens_the_failure_table() {
        let cases = vec![
            case("first", 0, true, 1.0),
            case("second", 0, true, 1.0),
            case("third", 0, true, 1.0),
        ];
        let budgets = StressRenderBudgets {
            failure_rows: 1,
            ..StressRenderBudgets::default()
        };

        let report = render(
            &cases,
            &inventory(&["first", "second", "third"]),
            1,
            None,
            None,
            &budgets,
        );

        assert!(
            report
                .markdown
                .contains("Showing the first 1 of 3 failed tests"),
            "{}",
            report.markdown
        );
    }

    /// 25 tests, 4 repeats; repeat 0 fails wholesale, the rest are clean.
    fn mass_failure_run() -> (Vec<CaseTiming>, BTreeSet<TestId>) {
        let names = (0..25).map(|i| format!("case_{i:02}")).collect::<Vec<_>>();
        let mut cases = Vec::new();
        for name in &names {
            for iteration in 0..4 {
                cases.push(case(name, iteration, iteration == 0, 0.1));
            }
        }
        let selected = names
            .iter()
            .map(|name| ("demo::tests".to_owned(), name.clone()))
            .collect();
        (cases, selected)
    }

    #[test]
    fn a_mass_failing_repeat_leaves_the_per_test_rates() {
        let (cases, selected) = mass_failure_run();

        let report = render(
            &cases,
            &selected,
            4,
            None,
            None,
            &StressRenderBudgets::default(),
        );

        let rate = report.rates[&("demo::tests".to_owned(), "case_00".to_owned())];
        assert_eq!(
            rate,
            LaneRate {
                failed: 0,
                attempts: 3
            },
            "{}",
            report.markdown
        );
    }

    #[test]
    fn a_quarantined_repeat_is_named_in_the_report() {
        let (cases, selected) = mass_failure_run();

        let report = render(
            &cases,
            &selected,
            4,
            None,
            None,
            &StressRenderBudgets::default(),
        );

        assert!(
            report.markdown.contains("## Quarantined repeats"),
            "{}",
            report.markdown
        );
    }

    /// The census reads a repeat that never executed as a repeat that failed,
    /// so every line a live repeat logged becomes "divergent" against it.
    #[test]
    fn a_quarantined_repeat_is_kept_out_of_the_census() {
        let (mut cases, selected) = mass_failure_run();
        let report = render(
            &cases,
            &selected,
            4,
            None,
            None,
            &StressRenderBudgets::default(),
        );

        retain_census_cases(&mut cases, &report.quarantined);

        assert!(
            cases.iter().all(|case| case.iteration != Some(0)),
            "{:?}",
            report.quarantined
        );
    }

    /// The census is what names a cause, so it must keep every repeat the rate
    /// tables still count.
    #[test]
    fn a_kept_repeat_stays_in_the_census() {
        let (mut cases, selected) = mass_failure_run();
        let report = render(
            &cases,
            &selected,
            4,
            None,
            None,
            &StressRenderBudgets::default(),
        );

        retain_census_cases(&mut cases, &report.quarantined);

        assert_eq!(
            cases
                .iter()
                .filter(|case| case.iteration == Some(1))
                .count(),
            25,
            "{:?}",
            report.quarantined
        );
    }

    #[test]
    fn a_narrow_filter_keeps_its_honest_failures() {
        let cases = vec![case("seek", 0, true, 0.1), case("seek", 1, false, 0.1)];

        let report = render(
            &cases,
            &inventory(&["seek"]),
            2,
            None,
            None,
            &StressRenderBudgets::default(),
        );

        let rate = report.rates[&("demo::tests".to_owned(), "seek".to_owned())];
        assert_eq!(
            rate,
            LaneRate {
                failed: 1,
                attempts: 2
            },
            "{}",
            report.markdown
        );
    }

    #[test]
    fn distinguishes_a_green_partial_report_from_a_complete_run() {
        let cases = vec![case("seek", 0, false, 0.1)];

        let inventory = inventory(&["seek"]);
        let partial = render(
            &cases,
            &inventory,
            2,
            None,
            None,
            &StressRenderBudgets::default(),
        );
        let complete = render(
            &cases,
            &inventory,
            1,
            None,
            None,
            &StressRenderBudgets::default(),
        );

        assert!(!partial.complete, "{}", partial.markdown);
        assert!(
            partial.markdown.contains("Result: **INCOMPLETE**"),
            "{}",
            partial.markdown
        );
        assert!(complete.complete, "{}", complete.markdown);
        assert!(
            complete.markdown.contains("Result: **PASSED**"),
            "{}",
            complete.markdown
        );
    }

    #[test]
    fn a_failure_before_all_iterations_is_still_incomplete() {
        let cases = vec![case("seek", 0, true, 0.1)];

        let report = render(
            &cases,
            &inventory(&["seek"]),
            2,
            None,
            None,
            &StressRenderBudgets::default(),
        );

        assert!(!report.complete, "{}", report.markdown);
        assert!(
            report.markdown.contains("Result: **INCOMPLETE**"),
            "{}",
            report.markdown
        );
        assert!(
            report.markdown.contains("Failed attempts: `1`"),
            "{}",
            report.markdown
        );
    }

    #[test]
    fn duplicate_iterations_do_not_hide_a_gap() {
        let cases = vec![
            case("seek", 0, false, 0.1),
            case("seek", 0, false, 0.1),
            case("seek", 2, false, 0.1),
        ];

        let report = render(
            &cases,
            &inventory(&["seek"]),
            3,
            None,
            None,
            &StressRenderBudgets::default(),
        );

        assert!(!report.complete, "{}", report.markdown);
        assert!(
            report.markdown.contains("Result: **INVALID JUNIT**"),
            "{}",
            report.markdown
        );
        assert!(
            report.markdown.contains("duplicate iteration 0"),
            "{}",
            report.markdown
        );
        assert!(
            report.markdown.contains("Observed iterations: `2`"),
            "{}",
            report.markdown
        );
    }

    #[test]
    fn missing_junit_is_explicitly_incomplete() {
        let report = render_missing(
            50,
            Path::new("target/nextest/stress/junit.xml"),
            true,
            &StressRenderBudgets::default(),
        );

        assert!(report.contains("Result: **NO JUNIT**"), "{report}");
        assert!(report.contains("Requested iterations: `50`"), "{report}");
        assert!(report.contains("primary step log"), "{report}");
    }

    #[test]
    fn invalid_entries_do_not_inflate_failure_rates() {
        let mut unindexed = case("seek", 0, false, 0.4);
        unindexed.iteration = None;
        let cases = vec![
            case("seek", 0, true, 0.1),
            case("seek", 0, false, 0.2),
            case("seek", 2, true, 0.3),
            unindexed,
        ];

        let report = render(
            &cases,
            &inventory(&["seek"]),
            1,
            None,
            None,
            &StressRenderBudgets::default(),
        );
        let markdown = &report.markdown;

        assert!(!report.complete, "{markdown}");
        assert!(markdown.contains("duplicate iteration 0"), "{markdown}");
        assert!(markdown.contains("out-of-range iteration 2"), "{markdown}");
        assert!(markdown.contains("has no stress iteration"), "{markdown}");
        assert!(markdown.contains("Observed iterations: `1`"), "{markdown}");
        assert!(markdown.contains("1 / 1"), "{markdown}");
        assert!(markdown.contains("100.00%"), "{markdown}");
        assert!(markdown.contains("100 ms"), "{markdown}");
        assert!(!markdown.contains("2 / 2"), "{markdown}");
        assert!(!markdown.contains("200 ms"), "{markdown}");
        assert!(!markdown.contains("300 ms"), "{markdown}");
        assert!(!markdown.contains("400 ms"), "{markdown}");
    }

    #[test]
    fn invalid_evidence_details_stay_bounded() {
        let budgets = StressRenderBudgets::default();
        let too_wide = "x".repeat(budgets.cell_chars + 1);
        let cases = (0..budgets.problem_rows + 5)
            .map(|index| {
                let mut timing = case(&format!("case-{index}"), 0, false, 0.1);
                timing.suite.clone_from(&too_wide);
                timing.iteration = None;
                timing
            })
            .collect::<Vec<_>>();

        let names = (0..budgets.problem_rows + 5)
            .map(|index| format!("case-{index}"))
            .collect::<Vec<_>>();
        let selected = names
            .iter()
            .map(|name| (too_wide.clone(), name.clone()))
            .collect();
        let report = render(&cases, &selected, 1, None, None, &budgets);
        let markdown = &report.markdown;

        assert_eq!(
            markdown.matches("has no stress iteration").count(),
            budgets.problem_rows,
            "{markdown}"
        );
        assert!(markdown.contains("more problems"), "{markdown}");
        assert!(!markdown.contains(too_wide.as_str()), "{markdown}");
    }

    /// One case's runaway retained output names itself and marks the lane
    /// incomplete — it must not invalidate the artifact and erase the other
    /// cases' evidence, which is what cost run #8 its flash-off lane.
    #[test]
    fn an_oversized_case_is_named_without_invalidating_the_lane() {
        let temp = tempfile::tempdir().expect("tempdir");
        let inventory = temp.path().join("inventory.json");
        fs::write(
            &inventory,
            r#"{
  "rust-suites": {
    "demo::tests": {
      "binary-id": "demo::tests",
      "status": "listed",
      "testcases": {
        "seek": {"ignored": false, "filter-match": {"status": "matches"}}
      }
    }
  }
}"#,
        )
        .expect("write inventory");
        let junit = temp.path().join("junit.xml");
        let oversized = "y".repeat(crate::junit::MAX_CASE_OUTPUT_BYTES);
        fs::write(
            &junit,
            format!(
                r#"<testsuites uuid="run" timestamp="2026-08-16T12:00:00Z">
  <testsuite name="demo::tests@stress-0">
    <testcase name="seek" classname="demo::tests" time="0.1" timestamp="2026-08-16T12:00:00Z">
      <failure type="test failure">boom</failure>
      <system-out>{oversized}</system-out>
    </testcase>
  </testsuite>
</testsuites>"#
            ),
        )
        .expect("write junit");
        let output = temp.path().join("report.md");

        let lane =
            lane_report(&StressReportArgs::new(junit, inventory, output, 1)).expect("lane report");

        assert!(lane.readable, "one noisy case must not erase the lane");
        assert!(
            !lane.markdown.contains("INVALID JUNIT"),
            "{}",
            lane.markdown
        );
        assert!(
            lane.markdown
                .contains("Evidence problem: retained failure output of `1` testcase(s)"),
            "{}",
            lane.markdown
        );
        assert!(
            lane.markdown.contains("`demo::tests seek`"),
            "the noisy case is named: {}",
            lane.markdown
        );
        assert!(
            lane.markdown.contains("Result: **INCOMPLETE**"),
            "{}",
            lane.markdown
        );
        assert!(!lane.rates.is_empty(), "the case still counts in rates");
        assert!(lane.verdict.is_err(), "a failed attempt keeps the verdict");
    }

    #[test]
    fn incomplete_inputs_write_reports_and_return_a_verdict() {
        const PARTIAL: &str = r#"<testsuites uuid="run" timestamp="2026-08-13T12:00:00Z">
  <testsuite name="demo::tests@stress-0">
    <testcase name="seek" classname="demo::tests" time="0.1" timestamp="2026-08-13T12:00:00Z"/>
  </testsuite>
</testsuites>"#;
        const MISMATCHED: &str = r#"<testsuites uuid="run" timestamp="2026-08-13T12:00:00Z">
  <testsuite name="other::tests@stress-0">
    <testcase name="seek" classname="demo::tests" time="0.1" timestamp="2026-08-13T12:00:00Z"/>
  </testsuite>
</testsuites>"#;
        let temp = tempfile::tempdir().expect("tempdir");
        let inventory = temp.path().join("inventory.json");
        fs::write(
            &inventory,
            r#"{
  "rust-suites": {
    "demo::tests": {
      "binary-id": "demo::tests",
      "status": "listed",
      "testcases": {
        "seek": {"ignored": false, "filter-match": {"status": "matches"}}
      }
    }
  }
}"#,
        )
        .expect("write inventory");
        let scenarios = [
            ("missing", None, "Result: **NO JUNIT**"),
            (
                "empty",
                Some("<testsuites uuid=\"run\"/>"),
                "Result: **INVALID JUNIT**",
            ),
            ("partial", Some(PARTIAL), "Result: **INCOMPLETE**"),
            ("mismatched", Some(MISMATCHED), "Result: **INVALID JUNIT**"),
        ];

        for (name, xml, marker) in scenarios {
            let junit = temp.path().join(format!("{name}.xml"));
            let output = temp.path().join(format!("{name}.md"));
            if let Some(xml) = xml {
                fs::write(&junit, xml).expect("write fixture");
            }
            let args = StressReportArgs {
                junit,
                inventory: inventory.clone(),
                line_log: None,
                envelope_dir: None,
                pressure_log: None,
                output: output.clone(),
                expected_count: 2,
                allow_missing: true,
                evidence: StressEvidenceConfig::default(),
                render: StressRenderBudgets::default(),
            };

            let error = run(&args).expect_err("incomplete evidence must fail closed");
            let markdown = fs::read_to_string(output).expect("read report");

            assert!(error.downcast_ref::<NotClean>().is_some(), "{error:?}");
            assert!(markdown.contains(marker), "{markdown}");
        }
    }

    #[test]
    fn complete_failed_input_writes_report_and_returns_a_verdict() {
        const FAILED: &str = r#"<testsuites uuid="run" timestamp="2026-08-13T12:00:00Z">
  <testsuite name="demo::tests@stress-0">
    <testcase name="seek" classname="demo::tests" time="0.1" timestamp="2026-08-13T12:00:00Z">
      <failure type="test failure">boom</failure>
    </testcase>
  </testsuite>
  <testsuite name="demo::tests@stress-1">
    <testcase name="seek" classname="demo::tests" time="0.1" timestamp="2026-08-13T12:00:01Z"/>
  </testsuite>
</testsuites>"#;
        let temp = tempfile::tempdir().expect("tempdir");
        let inventory = temp.path().join("inventory.json");
        let junit = temp.path().join("junit.xml");
        let output = temp.path().join("report.md");
        fs::write(
            &inventory,
            r#"{
  "rust-suites": {
    "demo::tests": {
      "binary-id": "demo::tests",
      "status": "listed",
      "testcases": {
        "seek": {"ignored": false, "filter-match": {"status": "matches"}}
      }
    }
  }
}"#,
        )
        .expect("write inventory");
        fs::write(&junit, FAILED).expect("write junit");
        let args = StressReportArgs {
            junit,
            inventory,
            line_log: None,
            envelope_dir: None,
            pressure_log: None,
            output: output.clone(),
            expected_count: 2,
            allow_missing: false,
            evidence: StressEvidenceConfig::default(),
            render: StressRenderBudgets::default(),
        };

        let error = run(&args).expect_err("failed attempt must fail closed");
        let markdown = fs::read_to_string(output).expect("read report");

        assert!(error.downcast_ref::<NotClean>().is_some(), "{error:?}");
        assert!(markdown.contains("Result: **FAILED**"), "{markdown}");
        assert!(markdown.contains("Observed iterations: `2`"), "{markdown}");
        assert!(markdown.contains("Failed attempts: `1`"), "{markdown}");
    }

    #[test]
    fn selected_test_missing_from_junit_fails_closed() {
        let cases = vec![case("seek", 0, false, 0.1)];

        let report = render(
            &cases,
            &inventory(&["seek", "missing"]),
            1,
            None,
            None,
            &StressRenderBudgets::default(),
        );

        assert!(!report.complete, "{}", report.markdown);
        assert!(
            report
                .markdown
                .contains("selected test `demo::tests missing` is absent"),
            "{}",
            report.markdown
        );
    }

    #[test]
    fn inventory_parser_keeps_only_runnable_matches() {
        let json = r#"{
  "rust-suites": {
    "demo::tests": {
      "binary-id": "demo::tests",
      "status": "listed",
      "testcases": {
        "selected": {"ignored": false, "filter-match": {"status": "matches"}},
        "ignored": {"ignored": true, "filter-match": {"status": "matches"}},
        "filtered": {"ignored": false, "filter-match": {"status": "mismatch"}}
      }
    }
  }
}"#;

        assert_eq!(
            parse_inventory(json).expect("parse inventory"),
            inventory(&["selected"])
        );
    }

    /// A target the project's `default-filter` excludes is listed by nextest
    /// with this status and no testcases. Reading it as a malformed inventory
    /// stopped the run before it ran a single test.
    #[test]
    fn a_suite_skipped_by_the_default_filter_contributes_no_tests() {
        let json = r#"{
  "rust-suites": {
    "demo::tests": {
      "binary-id": "demo::tests",
      "status": "listed",
      "testcases": {
        "selected": {"ignored": false, "filter-match": {"status": "matches"}}
      }
    },
    "excluded::tests": {
      "binary-id": "excluded::tests",
      "status": "skipped-default-filter",
      "testcases": {
        "unreachable": {"ignored": false, "filter-match": {"status": "matches"}}
      }
    }
  }
}"#;

        assert_eq!(
            parse_inventory(json).expect("parse inventory"),
            inventory(&["selected"])
        );
    }

    #[test]
    fn a_suite_status_the_parser_does_not_know_is_rejected() {
        let json = r#"{
  "rust-suites": {
    "demo::tests": {
      "binary-id": "demo::tests",
      "status": "invented",
      "testcases": {}
    }
  }
}"#;

        assert!(
            parse_inventory(json)
                .expect_err("unknown suite status must fail")
                .to_string()
                .contains("unsupported status `invented`")
        );
    }

    #[test]
    fn an_inventory_of_nothing_but_skipped_suites_is_rejected() {
        let json = r#"{
  "rust-suites": {
    "excluded::tests": {
      "binary-id": "excluded::tests",
      "status": "skipped-default-filter",
      "testcases": {}
    }
  }
}"#;

        assert!(
            parse_inventory(json)
                .expect_err("an empty selection must fail")
                .to_string()
                .contains("no runnable selected tests")
        );
    }

    fn lane(name: &str, rows: &[(&str, usize, usize)]) -> (String, BTreeMap<TestId, LaneRate>) {
        (
            name.to_owned(),
            rows.iter()
                .map(|(test, failed, attempts)| {
                    (
                        ("demo::tests".to_owned(), (*test).to_owned()),
                        LaneRate {
                            failed: *failed,
                            attempts: *attempts,
                        },
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn a_test_the_lanes_disagree_about_is_ranked_above_one_they_agree_on() {
        let table = render_lane_comparison(
            &[
                lane("on", &[("agreed", 5, 10), ("disputed", 0, 10)]),
                lane("off", &[("agreed", 5, 10), ("disputed", 9, 10)]),
            ],
            &[],
            &[],
            2,
            &StressRenderBudgets::default(),
        );

        let disputed = table.find("disputed").expect("disputed row");
        let agreed = table.find("agreed").expect("agreed row");
        assert!(disputed < agreed, "{table}");
    }

    #[test]
    fn a_test_absent_from_one_lane_is_reported_absent_rather_than_passing() {
        let table = render_lane_comparison(
            &[
                lane("on", &[("flash_only", 3, 10)]),
                lane("off", &[("shared", 1, 10)]),
            ],
            &[],
            &[],
            2,
            &StressRenderBudgets::default(),
        );

        assert!(table.contains("not selected"), "{table}");
    }

    #[test]
    fn a_run_with_one_trustworthy_lane_refuses_to_compare() {
        let table = render_lane_comparison(
            &[lane("on", &[("solo", 1, 10)])],
            &[],
            &[],
            2,
            &StressRenderBudgets::default(),
        );

        assert!(table.contains("needs two verified lanes"), "{table}");
    }

    /// A test that only one lane can redden is a defect of that lane's
    /// configuration, not of the code every lane shares.
    #[test]
    fn a_test_red_in_one_lane_names_that_lane() {
        let table = render_lane_comparison(
            &[
                lane("flash-on", &[("clockbound", 2, 50)]),
                lane("flash-off", &[("clockbound", 0, 50)]),
            ],
            &[],
            &[],
            2,
            &StressRenderBudgets::default(),
        );

        assert!(table.contains("only flash-on"), "{table}");
    }

    /// Redness every lane reproduces cannot be blamed on any one lane's
    /// configuration, and the table must say so rather than name a lane.
    #[test]
    fn a_test_red_in_every_lane_names_no_lane() {
        let table = render_lane_comparison(
            &[
                lane("flash-on", &[("everywhere", 2, 50)]),
                lane("flash-off", &[("everywhere", 3, 50)]),
            ],
            &[],
            &[],
            2,
            &StressRenderBudgets::default(),
        );

        assert!(table.contains("every lane"), "{table}");
    }

    fn attempted(name: &str, failed: usize, attempts: usize) -> (String, LaneRate) {
        (name.to_owned(), LaneRate { failed, attempts })
    }

    /// A lane whose verdict is one exit code per attempt has no per-test rate to
    /// put in the table above. Dropping it from the run for that reason is
    /// how a sanitizer lane runs and reports nothing.
    #[test]
    fn a_lane_measured_by_attempts_is_reported_beside_the_per_test_lanes() {
        let table = render_lane_comparison(
            &[
                lane("on", &[("solo", 1, 10)]),
                lane("off", &[("solo", 0, 10)]),
            ],
            &[attempted("rtsan", 1, 4)],
            &[],
            3,
            &StressRenderBudgets::default(),
        );

        assert!(table.contains("rtsan"), "{table}");
        assert!(table.contains("25.00% (1/4)"), "{table}");
    }

    #[test]
    fn a_lane_measured_by_attempts_counts_as_trustworthy_evidence() {
        let table = render_lane_comparison(
            &[],
            &[attempted("rtsan", 0, 2)],
            &[],
            1,
            &StressRenderBudgets::default(),
        );

        assert!(
            table.contains("Lanes with trustworthy evidence: `1`"),
            "{table}"
        );
    }

    /// The run of 2026-08-16 quarantined thirty-three of one lane's fifty
    /// repeats as environment poisoning, reported that lane `INCOMPLETE`, and
    /// still counted it among the six lanes with trustworthy evidence.
    #[test]
    fn an_excluded_lane_is_kept_out_of_the_trustworthy_count() {
        let table = render_lane_comparison(
            &[
                lane("on", &[("solo", 1, 10)]),
                lane("off", &[("solo", 0, 10)]),
            ],
            &[],
            &[(
                "flash-off".to_owned(),
                "incomplete evidence: fewer iterations than requested".to_owned(),
            )],
            3,
            &StressRenderBudgets::default(),
        );

        assert!(
            table.contains("Lanes with trustworthy evidence: `2`"),
            "{table}"
        );
    }

    #[test]
    fn an_excluded_lane_is_named_with_its_reason() {
        let table = render_lane_comparison(
            &[
                lane("on", &[("solo", 1, 10)]),
                lane("off", &[("solo", 0, 10)]),
            ],
            &[],
            &[(
                "flash-off".to_owned(),
                "incomplete evidence: fewer iterations than requested".to_owned(),
            )],
            3,
            &StressRenderBudgets::default(),
        );

        assert!(table.contains("flash-off"), "{table}");
    }

    fn kept_reports(reports: &[(usize, &str)]) -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("tempdir");
        for (attempt, xml) in reports {
            fs::write(temp.path().join(format!("attempt-{attempt}.xml")), xml)
                .expect("write kept report");
        }
        temp
    }

    fn failed_case(name: &str) -> String {
        format!(
            r#"<testsuites uuid="run" timestamp="2026-08-17T12:00:00Z">
  <testsuite name="demo::tests@rtsan-0">
    <testcase name="{name}" classname="demo::tests" time="0.1" timestamp="2026-08-17T12:00:00Z">
      <failure type="test failure">aborted</failure>
    </testcase>
  </testsuite>
</testsuites>"#
        )
    }

    fn rendered(temp: &tempfile::TempDir, codes: &[i32]) -> String {
        let mut out = String::new();
        append_attempt_reports(
            &mut out,
            &attempt_records(temp.path(), codes),
            &StressRenderBudgets::default(),
        );
        out
    }

    /// One report holding several repeats of one test, which is what a lane run
    /// under `--stress-count` writes.
    fn repeated_case(name: &str, outcomes: &[bool]) -> String {
        let suites = outcomes
            .iter()
            .enumerate()
            .map(|(repeat, failed)| {
                let failure = if *failed {
                    "\n      <failure type=\"test failure\">aborted</failure>\n    "
                } else {
                    ""
                };
                format!(
                    r#"  <testsuite name="demo::tests@stress-{repeat}">
    <testcase name="{name}" classname="demo::tests" time="0.1">{failure}</testcase>
  </testsuite>
"#
                )
            })
            .collect::<String>();
        format!(
            r#"<testsuites uuid="run" timestamp="2026-08-17T12:00:00Z">
{suites}</testsuites>"#
        )
    }

    #[test]
    fn a_command_lanes_own_runner_names_the_test_behind_an_exit_code() {
        let temp = kept_reports(&[(1, &failed_case("mix_tap"))]);

        let out = rendered(&temp, &[0, 101, 0]);

        assert!(out.contains("demo::tests mix_tap"), "{out}");
    }

    #[test]
    fn a_named_test_carries_the_attempts_it_was_rejected_on() {
        let temp = kept_reports(&[(1, &failed_case("mix_tap")), (2, &failed_case("mix_tap"))]);

        let out = rendered(&temp, &[0, 101, 101]);

        assert!(out.contains("| 1, 2 |"), "{out}");
    }

    #[test]
    fn a_rejected_attempt_that_wrote_no_report_is_reported_as_silent() {
        let temp = kept_reports(&[]);

        let out = rendered(&temp, &[0, 139]);

        assert!(out.contains("died before any verdict"), "{out}");
    }

    #[test]
    fn an_accepted_attempt_that_wrote_no_report_is_not_called_silent() {
        let temp = kept_reports(&[]);

        let out = rendered(&temp, &[0, 0]);

        assert!(out.is_empty(), "{out}");
    }

    #[test]
    fn a_report_that_cannot_be_parsed_is_named_rather_than_counted_as_passing() {
        let temp = kept_reports(&[(0, "<testsuites")]);

        let out = rendered(&temp, &[101]);

        assert!(out.contains("could not be read"), "{out}");
    }

    /// A lane launched once with a repeat count runs every repeat inside one
    /// report. Counting launches there would divide by one and call a violation
    /// that fired once in three "every attempt".
    #[test]
    fn every_repeat_inside_one_report_is_counted() {
        let temp = kept_reports(&[(0, &repeated_case("mix_tap", &[false, true, false]))]);

        let records = attempt_records(temp.path(), &[101]);

        assert_eq!(
            records.rates[&("demo::tests".to_owned(), "mix_tap".to_owned())],
            LaneRate {
                failed: 1,
                attempts: 3
            }
        );
    }

    /// The repeats a lane recorded is what its requested count is checked
    /// against: a run that stopped after two of fifty is not the run it
    /// claims to be, and only the report can say so.
    #[test]
    fn the_recorded_repeats_are_the_most_any_test_ran() {
        let temp = kept_reports(&[(0, &repeated_case("mix_tap", &[false, true, false]))]);

        let records = attempt_records(temp.path(), &[101]);

        assert_eq!(records.repeats(), 3);
    }

    /// Repeats spread over separate launches add up the same way: the number
    /// that matters is executions, whichever side performed the repetition.
    #[test]
    fn executions_from_separate_launches_are_added_together() {
        let temp = kept_reports(&[
            (0, &repeated_case("mix_tap", &[false, false])),
            (1, &repeated_case("mix_tap", &[true, false])),
        ]);

        let records = attempt_records(temp.path(), &[0, 101]);

        assert_eq!(
            records.rates[&("demo::tests".to_owned(), "mix_tap".to_owned())],
            LaneRate {
                failed: 1,
                attempts: 4
            }
        );
    }

    /// A lane launched once writes one attempt marker, so an attempt column
    /// would print the same `0` on every row and read as though each failure had
    /// been located in time.
    #[test]
    fn a_single_launch_is_reported_without_an_attempt_column() {
        let temp = kept_reports(&[(0, &repeated_case("mix_tap", &[false, true, false]))]);

        let out = rendered(&temp, &[101]);

        assert!(out.contains("| test | rate |"), "{out}");
    }

    #[test]
    fn positive_run_counts_and_inventory_case_limits_are_explicit() {
        for count in [1, 50, usize::MAX] {
            validate_expected_count(count).expect("valid expected count");
        }
        assert!(validate_expected_count(0).is_err());
        validate_inventory_case_count(MAX_INVENTORY_CASES).expect("case limit is inclusive");
        assert!(validate_inventory_case_count(MAX_INVENTORY_CASES + 1).is_err());
    }

    #[test]
    fn bounded_reader_rejects_oversized_artifact_without_modifying_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("artifact");
        fs::write(&path, "12345").expect("write fixture");

        let error = read_bounded_utf8(&path, 4, "fixture").expect_err("oversize must fail");

        assert!(
            error.to_string().contains("deterministic limit"),
            "{error:?}"
        );
        assert_eq!(fs::read_to_string(path).expect("read fixture"), "12345");
    }

    #[test]
    fn correlation_metadata_rejects_missing_or_malformed_attempt_times() {
        let mut report = crate::junit::JunitReport {
            run_id: Some("run".to_owned()),
            timestamp: Some("2026-08-13T12:00:00Z".to_owned()),
            cases: vec![case("seek", 0, false, 0.1)],
        };

        assert!(
            validate_correlation_metadata(&report)
                .expect_err("missing timestamp must fail")
                .contains("timestamp is missing")
        );
        report.cases[0].timestamp = Some("not-a-time".to_owned());
        assert!(
            validate_correlation_metadata(&report)
                .expect_err("malformed timestamp must fail")
                .contains("timestamp is malformed")
        );
        report.cases[0].timestamp = Some("2026-08-13T12:00:00Z".to_owned());
        report.timestamp = None;
        assert!(
            validate_correlation_metadata(&report)
                .expect_err("missing root timestamp must fail")
                .contains("root timestamp is missing")
        );
        report.timestamp = Some("not-a-time".to_owned());
        assert!(
            validate_correlation_metadata(&report)
                .expect_err("malformed root timestamp must fail")
                .contains("root timestamp is malformed")
        );
        report.timestamp = Some("2026-08-13T12:00:00Z".to_owned());
        report.run_id = None;
        assert!(
            validate_correlation_metadata(&report)
                .expect_err("missing run ID must fail")
                .contains("uuid is missing")
        );
    }

    #[test]
    fn missing_supplementary_evidence_marks_a_report_incomplete() {
        let mut markdown = "# Stress evidence\n\n- Result: **FAILED**\n".to_owned();

        mark_incomplete(&mut markdown);

        assert!(markdown.contains("Result: **INCOMPLETE**"), "{markdown}");
        assert!(!markdown.contains("Result: **FAILED**"), "{markdown}");
    }
}
