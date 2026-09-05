//! Correlates per-attempt stress failures with runtime evidence.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    sync::LazyLock,
    time::Duration,
};

use regex::{Captures, Regex};

use self::attempt::{AttemptKey, AttemptOutcome, attempt_outcomes};
use super::{MAX_FAILURE_ROWS, StressReportArgs, markdown_cell, test_id};
use crate::{common::project::StressEvidenceConfig, junit::CaseTiming};

mod attempt;
mod divergence;
mod envelope;
mod line;
mod line_reader;
mod overlap;
mod pressure;

/// Payload lines kept after a panic header: the assertion's message and, for
/// `assert_eq!`, its `left` and `right`.
const PANIC_DETAIL_LINES: usize = 4;
const MAX_SIGNATURE_ROWS: usize = 100;
pub(super) const MAX_SIGNATURE_EXAMPLES: usize = 5;

#[derive(Debug, Default)]
struct SignatureCluster {
    details: BTreeSet<String>,
    failed_attempts: BTreeSet<String>,
    passed_attempts: BTreeSet<String>,
    tests: BTreeSet<String>,
    unattributed_attempts: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct AttemptDossier {
    co_runners: BTreeSet<String>,
    envelopes: BTreeSet<String>,
    lines: BTreeSet<String>,
    wait_graph: BTreeSet<String>,
    backtrace: String,
    display: String,
    pressure: String,
    symptom: String,
    test: String,
    /// Newest run-length groups of the attempt envelope's DEBUG-event tail,
    /// oldest first — the state transitions immediately preceding the failure.
    event_tail: Vec<String>,
    /// Newest run-length groups of the attempt envelope's probe tail, oldest
    /// first. Ordered evidence, not a set: the last firing before the dump is
    /// the verdict, and its `(xN)` count is the starvation streak.
    flight_tail: Vec<String>,
}

impl SignatureCluster {
    fn attempts(&self) -> usize {
        self.failed_attempts
            .len()
            .saturating_add(self.passed_attempts.len())
            .saturating_add(self.unattributed_attempts.len())
    }
}

pub(super) fn append_correlated_evidence(
    out: &mut String,
    cases: &[CaseTiming],
    run_id: Option<&str>,
    args: &StressReportArgs,
) -> bool {
    let failed = cases
        .iter()
        .filter(|case| case.failing())
        .collect::<Vec<_>>();
    let expected_envelopes = failed
        .iter()
        .filter(|case| requires_envelope(&case.output, &args.evidence))
        .filter_map(|case| attempt_key(case))
        .collect::<BTreeSet<_>>();
    let outcomes = attempt_outcomes(cases);
    let dossier_keys = failed
        .iter()
        .filter_map(|case| attempt_key(case))
        .take(MAX_FAILURE_ROWS)
        .collect::<BTreeSet<_>>();
    let mut overlaps = overlap::for_targets(cases, &dossier_keys);
    let mut dossiers = failed
        .iter()
        .filter_map(|case| {
            let key = attempt_key(case)?;
            if !dossier_keys.contains(&key) {
                return None;
            }
            let dossier = AttemptDossier {
                display: attempt_id(case, run_id),
                test: test_id(case),
                symptom: failure_signature(case, &args.evidence),
                backtrace: backtrace_signature(&case.output, &args.evidence).unwrap_or_default(),
                wait_graph: wait_signatures(&case.output, &args.evidence)
                    .into_iter()
                    .collect(),
                co_runners: overlaps.remove(&key).unwrap_or_default(),
                ..AttemptDossier::default()
            };
            Some((key, dossier))
        })
        .collect::<BTreeMap<_, _>>();
    let mut symptoms = BTreeMap::new();
    let mut backtraces = BTreeMap::new();
    let mut waits = BTreeMap::new();
    let mut complete = true;
    for case in &failed {
        let attempt = attempt_id(case, run_id);
        let test = test_id(case);
        add_signature(
            &mut symptoms,
            failure_signature(case, &args.evidence),
            &attempt,
            &test,
            AttemptOutcome::Failed,
            None,
        );
        if let Some(signature) = backtrace_signature(&case.output, &args.evidence) {
            add_signature(
                &mut backtraces,
                signature,
                &attempt,
                &test,
                AttemptOutcome::Failed,
                None,
            );
        }
        for signature in wait_signatures(&case.output, &args.evidence) {
            add_signature(
                &mut waits,
                signature,
                &attempt,
                &test,
                AttemptOutcome::Failed,
                None,
            );
        }
    }

    render_clusters(
        out,
        "Failure symptom clusters",
        &symptoms,
        "The terminal panic or timeout. This locates the observed endpoint, not necessarily its cause.",
    );
    render_clusters(
        out,
        "Backtrace overlays",
        &backtraces,
        "The first project frames shared by failing attempts. Wrapper and address noise is removed.",
    );
    complete &= divergence::append(out, cases);
    if let Some(path) = &args.line_log {
        complete &= line::append(
            out,
            path,
            args.evidence.line_marker.as_deref(),
            &outcomes,
            run_id,
            &mut dossiers,
        );
    }
    let mut flight = envelope::FlightClusters::default();
    if let Some(path) = &args.envelope_dir {
        complete &= envelope::append(
            out,
            path,
            envelope::Input::new(&outcomes, &expected_envelopes, run_id, &args.evidence),
            &mut waits,
            &mut flight,
            &mut dossiers,
        );
    } else if !expected_envelopes.is_empty() {
        let _ = writeln!(
            out,
            "\nEvidence problem: `{}` timeout-class failed attempts require exact same-run attempt envelopes, but no envelope directory was provided.",
            expected_envelopes.len(),
        );
        complete = false;
    }
    render_clusters(
        out,
        "Wait-graph signatures",
        &waits,
        "Repeated holders, waiters, or quiescence pins are causal candidates. Task IDs and timing counters are removed. An optional backtrace belongs to the snapshot caller, not necessarily to a holder or waiter.",
    );
    render_clusters(
        out,
        "Flight probe signatures",
        &flight.probes,
        "Probe firings recorded in the in-memory flight ring at dump time. Only failing attempts write dumps, so the failed column counts how many of them carried the line; numeric field values are normalized. A `waiting branch` line names the step that kept ticking the hang watchdog.",
    );
    render_clusters(
        out,
        "Flight event signatures",
        &flight.events,
        "DEBUG events from the flight ring at dump time — the state transitions immediately preceding the failure, independent of the stdout filter.",
    );
    if let Some(path) = &args.pressure_log {
        let (points, pressure_complete) = pressure::append(out, path);
        complete &= pressure_complete;
        pressure::correlate(&mut dossiers, cases, &points);
    }
    render_attempt_dossiers(out, &dossiers, failed.len());
    complete
}

fn attempt_key(case: &CaseTiming) -> Option<AttemptKey> {
    Some(AttemptKey {
        suite: case.suite.clone(),
        name: case.name.clone(),
        iteration: case.iteration?,
    })
}

fn add_signature(
    clusters: &mut BTreeMap<String, SignatureCluster>,
    signature: String,
    attempt: &str,
    test: &str,
    outcome: AttemptOutcome,
    detail: Option<&str>,
) {
    let cluster = clusters.entry(signature).or_default();
    match outcome {
        AttemptOutcome::Failed => &mut cluster.failed_attempts,
        AttemptOutcome::Passed => &mut cluster.passed_attempts,
        AttemptOutcome::Unattributed => &mut cluster.unattributed_attempts,
    }
    .insert(attempt.to_owned());
    if !test.is_empty() {
        cluster.tests.insert(test.to_owned());
    }
    if let Some(detail) = detail
        && cluster.details.len() < MAX_SIGNATURE_EXAMPLES
    {
        cluster.details.insert(markdown_cell(detail));
    }
}

fn render_clusters(
    out: &mut String,
    heading: &str,
    clusters: &BTreeMap<String, SignatureCluster>,
    explanation: &str,
) {
    if clusters.is_empty() {
        return;
    }
    let mut rows = clusters.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        Reverse(left.1.failed_attempts.len())
            .cmp(&Reverse(right.1.failed_attempts.len()))
            .then_with(|| Reverse(left.1.attempts()).cmp(&Reverse(right.1.attempts())))
            .then_with(|| left.0.cmp(right.0))
    });
    let _ = write!(
        out,
        "\n## {heading}\n\n{explanation}\n\n| signature | failed | passed | unattributed | tests | examples |\n|---|---:|---:|---:|---:|---|\n"
    );
    for (signature, cluster) in rows.iter().take(MAX_SIGNATURE_ROWS) {
        let examples = cluster
            .failed_attempts
            .iter()
            .chain(cluster.passed_attempts.iter())
            .chain(cluster.unattributed_attempts.iter())
            .take(MAX_SIGNATURE_EXAMPLES)
            .cloned()
            .collect::<Vec<_>>()
            .join("<br>");
        let detail = cluster
            .details
            .iter()
            .next()
            .map_or(String::new(), |detail| format!("<br>{detail}"));
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {}{} |",
            markdown_cell(signature),
            cluster.failed_attempts.len(),
            cluster.passed_attempts.len(),
            cluster.unattributed_attempts.len(),
            cluster.tests.len(),
            markdown_cell(&examples),
            detail,
        );
    }
    if rows.len() > MAX_SIGNATURE_ROWS {
        let _ = writeln!(
            out,
            "\nShowing the first {MAX_SIGNATURE_ROWS} of {} signatures. Raw artifacts are exhaustive.",
            rows.len()
        );
    }
}

fn attempt_id(case: &CaseTiming, run_id: Option<&str>) -> String {
    let iteration = case
        .iteration
        .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
    run_id.map_or_else(
        || format!("{} {} @stress-{iteration}", case.suite, case.name),
        |run_id| format!("{run_id}:{}@stress-{iteration}${}", case.suite, case.name),
    )
}

fn failure_signature(case: &CaseTiming, evidence: &StressEvidenceConfig) -> String {
    let lines = clean_lines(&case.output);
    for (index, line) in lines.iter().enumerate() {
        if evidence
            .envelope_marker
            .as_deref()
            .is_some_and(|marker| line.contains(marker))
        {
            let summary = evidence
                .envelope_suffix_markers
                .iter()
                .filter_map(|marker| line.find(marker))
                .min()
                .map_or(line.as_str(), |index| &line[..index]);
            return normalize_signature(summary);
        }
        if is_timeout_line(line) {
            return normalize_signature(line);
        }
        if is_panic_header(line) {
            // The header arrives twice — nextest puts it in the failure
            // `message` attribute and the body repeats it — so the first line
            // after it is the duplicate, not the payload. Taking that one line
            // as the detail spent the whole signature on saying the same thing
            // twice and dropped what distinguishes one failure from another:
            // the assertion's own message and its values.
            let detail: Vec<&str> = lines
                .iter()
                .skip(index + 1)
                .map(String::as_str)
                .filter(|candidate| !is_panic_header(candidate))
                .take_while(|candidate| {
                    !candidate.starts_with("stack backtrace")
                        && !candidate.starts_with("note: run with")
                })
                .take(PANIC_DETAIL_LINES)
                .collect();
            let head = line.trim_end_matches(':');
            let detail = if detail.is_empty() {
                String::new()
            } else {
                format!(": {}", detail.join(" "))
            };
            return normalize_signature(&format!("{head}{detail}"));
        }
    }
    lines.first().map_or_else(
        || "failure output unavailable".to_owned(),
        |line| normalize_signature(line),
    )
}

fn is_panic_header(line: &str) -> bool {
    line.contains("panicked at")
}

fn requires_envelope(output: &str, evidence: &StressEvidenceConfig) -> bool {
    let lines = clean_lines(output);
    lines.first().is_some_and(|line| is_junit_timeout(line))
        || lines.iter().any(|line| {
            evidence
                .envelope_marker
                .as_deref()
                .is_some_and(|marker| line.contains(marker))
                || line.to_ascii_lowercase().contains("hard timeout")
        })
}

fn is_timeout_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    is_junit_timeout(&lower) || lower.contains("hard timeout") || lower.contains("timed out after")
}

fn is_junit_timeout(line: &str) -> bool {
    line == "test timeout" || line.starts_with("test timeout:")
}

pub(super) fn backtrace_signature(output: &str, evidence: &StressEvidenceConfig) -> Option<String> {
    static SOURCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_./-]+\.rs:\d+(?::\d+)?")
            .expect("source-location regex")
    });
    let clean = strip_ansi(output);
    let frames = SOURCE
        .find_iter(&clean)
        .map(|matched| matched.as_str().to_owned())
        .filter(|frame| {
            !evidence
                .source_excludes
                .iter()
                .any(|excluded| frame.contains(excluded))
        })
        .fold(Vec::<String>::new(), |mut frames, frame| {
            if frames.len() < MAX_SIGNATURE_EXAMPLES && !frames.contains(&frame) {
                frames.push(frame);
            }
            frames
        });
    (!frames.is_empty()).then(|| frames.join(" -> "))
}

fn wait_signatures(output: &str, evidence: &StressEvidenceConfig) -> Vec<String> {
    let mut signatures = BTreeSet::new();
    let mut context = "wait graph".to_owned();
    let mut primitive = None::<String>;
    let mut holder = None::<String>;
    for line in clean_lines(output) {
        let trimmed = line.trim();
        if let Some(value) = evidence
            .dump_marker
            .as_deref()
            .and_then(|marker| trimmed.split(marker).nth(1))
        {
            context = normalize_signature(value);
            continue;
        }
        if trimmed.starts_with('#')
            && evidence
                .primitive_marker
                .as_deref()
                .is_some_and(|marker| trimmed.contains(marker))
        {
            primitive = Some(normalize_wait(trimmed));
            holder = None;
            continue;
        }
        if evidence
            .holder_marker
            .as_deref()
            .is_some_and(|marker| trimmed.contains(marker))
        {
            holder = Some(normalize_wait(trimmed));
            continue;
        }
        if evidence
            .wait_marker
            .as_deref()
            .is_some_and(|marker| trimmed.contains(marker))
        {
            let edge = [
                Some(context.as_str()),
                primitive.as_deref(),
                holder.as_deref(),
                Some(trimmed),
            ]
            .into_iter()
            .flatten()
            .map(normalize_wait)
            .collect::<Vec<_>>()
            .join(" | ");
            signatures.insert(edge);
            continue;
        }
        if evidence
            .direct_markers
            .iter()
            .any(|needle| trimmed.contains(needle))
        {
            signatures.insert(format!("{} | {}", context, normalize_wait(trimmed)));
        }
    }
    signatures.into_iter().collect()
}

fn render_attempt_dossiers(
    out: &mut String,
    dossiers: &BTreeMap<AttemptKey, AttemptDossier>,
    failed_attempts: usize,
) {
    if dossiers.is_empty() {
        return;
    }
    out.push_str(
        "\n## Failed-attempt evidence overlay\n\nEach bounded example row joins the terminal symptom with same-attempt runtime evidence; raw artifacts remain exhaustive. Empty cells mean that source emitted no attributable record. The flight and event tails are ordered, oldest first, with `(xN)` marking consecutive repeats of one line; each group shows its last firing's field values, so the newest group carries the exact state the attempt died in. Co-runners and pressure are correlation candidates, not causes.\n\n| attempt | symptom | project frames | wait graph | line evidence | envelope | flight tail | event tail | pressure | co-running tests |\n|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for dossier in dossiers.values().take(MAX_FAILURE_ROWS) {
        let _ = writeln!(
            out,
            "| `{}`<br>{} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            markdown_cell(&dossier.display),
            markdown_cell(&dossier.test),
            markdown_cell(&dossier.symptom),
            markdown_cell(&dossier.backtrace),
            render_set(&dossier.wait_graph),
            render_set(&dossier.lines),
            render_set(&dossier.envelopes),
            render_ordered(&dossier.flight_tail),
            render_ordered(&dossier.event_tail),
            markdown_cell(&dossier.pressure),
            render_set(&dossier.co_runners),
        );
    }
    if failed_attempts > dossiers.len() {
        let _ = writeln!(
            out,
            "\nShowing {} bounded examples of {failed_attempts} failed attempts. Raw artifacts are exhaustive.",
            dossiers.len(),
        );
    }
}

/// Ordered evidence cell: the sequence is the meaning, so no sorting and no
/// truncation beyond what the producer already bounded.
fn render_ordered(values: &[String]) -> String {
    values
        .iter()
        .map(|value| markdown_cell(value))
        .collect::<Vec<_>>()
        .join("<br>")
}

fn render_set(values: &BTreeSet<String>) -> String {
    let rendered = values
        .iter()
        .take(MAX_SIGNATURE_EXAMPLES)
        .map(|value| markdown_cell(value))
        .collect::<Vec<_>>()
        .join("<br>");
    if values.len() > MAX_SIGNATURE_EXAMPLES {
        format!("{rendered}<br>... ({} total)", values.len())
    } else {
        rendered
    }
}

fn clean_lines(text: &str) -> Vec<String> {
    strip_ansi(text)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(super) fn strip_ansi(text: &str) -> String {
    static ANSI: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("ANSI escape regex"));
    ANSI.replace_all(text, "").into_owned()
}

fn normalize_signature(text: &str) -> String {
    static VOLATILE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*(?:_ns|_ms)|pid|task|thread|id|dump|polls)=[^\s,;]+")
            .expect("volatile diagnostic regex")
    });
    /// A Rust panic header names the thread and its id before the location.
    /// The id changes on every attempt, which made each failure its own
    /// cluster — the one section whose purpose is to say "these are one cause"
    /// reported each of them separately. The thread NAME is the test's own,
    /// already carried by the examples column, and spending the row's width on
    /// it is what pushed the assertion's values off the end.
    static PANIC_THREAD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"thread '[^']*' \(\d+\) panicked at").expect("panic thread identity regex")
    });
    static HEX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"0x[0-9a-fA-F]+").expect("address regex"));
    /// A fixture server mints one of these per attempt and puts it in every URL
    /// it serves. Left standing, each attempt's playlist line is a signature no
    /// other attempt can carry, so the pass that asks which lines separate
    /// failures from passes answers "all of them" — eighteen such rows were the
    /// whole divergence table of `packaged_abr_switch_keeps_player_continuity`
    /// in run 33752112563, and the lines that did separate them were crowded
    /// out.
    static UUID: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
        )
        .expect("uuid regex")
    });
    /// A content or asset-root digest, volatile for the same reason. Unanchored
    /// because a cache path prefixes one with a namespace letter, and a word
    /// boundary that never falls between the two leaves the digest standing. A
    /// run of digits alone is left to the caller that normalizes numbers: a
    /// decimal counter is not a digest, and saying so would cost the reader the
    /// one axis these tables order by.
    static DIGEST: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[0-9a-f]{16,}").expect("digest regex"));
    /// A cache key spells an authority's `host:port` with the colon percent-
    /// escaped, which glues the port to a hex escape so no word boundary falls
    /// before its digits and the number pass steps over it. The port is as
    /// volatile as the one in the URL beside it, which that pass does
    /// normalize: leaving it standing split nine divergence rows of
    /// `packaged_abr_switch_keeps_player_continuity` into eighteen, one per
    /// attempt, in run 33752112563.
    static ESCAPED_PORT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"~3[aA]\d+").expect("escaped port regex"));
    let text = strip_ansi(text).replace(['\r', '\n'], " ");
    let text = VOLATILE.replace_all(&text, "$1=<volatile>");
    let text = PANIC_THREAD.replace_all(&text, "panicked at");
    let text = HEX.replace_all(&text, "0x<address>");
    let text = UUID.replace_all(&text, "<uuid>");
    let text = ESCAPED_PORT.replace_all(&text, "~3a<port>");
    let text = DIGEST.replace_all(&text, |captures: &Captures<'_>| {
        let matched = &captures[0];
        if matched.bytes().any(|byte| byte.is_ascii_alphabetic()) {
            "<digest>".to_owned()
        } else {
            matched.to_owned()
        }
    });
    markdown_cell(text.trim())
}

fn normalize_wait(text: &str) -> String {
    static IDS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?:#\d+|\b[A-Za-z_][A-Za-z0-9_]*id=[^\s]+|\b[A-Za-z_][A-Za-z0-9_:]*Key\([^)]*\))",
        )
        .expect("wait-graph identity regex")
    });
    let normalized = IDS.replace_all(text, "<id>");
    normalize_signature(&normalized)
}

fn duration_ms(secs: f64) -> u64 {
    let Ok(duration) = Duration::try_from_secs_f64(secs) else {
        return if secs.is_finite() && secs.is_sign_positive() {
            u64::MAX
        } else {
            1
        };
    };
    let millis = duration
        .as_millis()
        .saturating_add(u128::from(duration.subsec_nanos() % 1_000_000 != 0));
    u64::try_from(millis).unwrap_or(u64::MAX).max(1)
}

pub(super) fn parse_timestamp_ms(timestamp: &str) -> Option<u64> {
    static RFC3339: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$",
        )
        .expect("RFC 3339 timestamp regex")
    });
    let captures = RFC3339.captures(timestamp)?;
    let read = |index| captures.get(index)?.as_str().parse::<i64>().ok();
    let year = read(1)?;
    let month = read(2)?;
    let day = read(3)?;
    let hour = read(4)?;
    let minute = read(5)?;
    let second = read(6)?;
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let millis = captures.get(7).map_or(0, |fraction| {
        let mut digits = fraction.as_str().chars();
        (0..3).fold(0_i64, |value, _| {
            value * 10
                + i64::from(
                    digits
                        .next()
                        .and_then(|digit| digit.to_digit(10))
                        .unwrap_or(0),
                )
        })
    });
    let zone = captures.get(8)?.as_str();
    let offset = if zone == "Z" {
        0
    } else {
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let hours = zone.get(1..3)?.parse::<i64>().ok()?;
        let minutes = zone.get(4..6)?.parse::<i64>().ok()?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        sign * (hours * 3_600 + minutes * 60)
    };
    let seconds = days_from_civil(year, month, day)
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?
        .checked_sub(offset)?;
    let timestamp = seconds.checked_mul(1_000)?.checked_add(millis)?;
    u64::try_from(timestamp).ok()
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> StressEvidenceConfig {
        StressEvidenceConfig {
            envelope_schema: Some("demo.hang.v1".to_owned()),
            envelope_marker: Some("[hang]".to_owned()),
            envelope_suffix_markers: vec![" payload=".to_owned(), " \u{2014} ".to_owned()],
            dump_marker: Some("[wait dump]".to_owned()),
            primitive_marker: Some("created_at=".to_owned()),
            holder_marker: Some("held by".to_owned()),
            wait_marker: Some("WAITING:".to_owned()),
            direct_markers: vec!["active holder".to_owned()],
            ..StressEvidenceConfig::default()
        }
    }

    fn case(name: &str, iteration: usize, failed: bool, secs: f64) -> CaseTiming {
        CaseTiming {
            failed,
            flaky: false,
            secs,
            name: name.to_owned(),
            suite: "demo::tests".to_owned(),
            iteration: Some(iteration),
            timestamp: None,
            output: String::new(),
            output_truncated: false,
        }
    }

    #[test]
    fn timestamp_parser_handles_fractional_seconds_and_offsets() {
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_timestamp_ms("1970-01-01T01:00:00.123456+01:00"),
            Some(123)
        );
        assert_eq!(parse_timestamp_ms("2000-02-30T00:00:00Z"), None);
        assert_eq!(parse_timestamp_ms("not-a-timestamp"), None);
    }

    #[test]
    fn duration_rounds_up_to_a_positive_millisecond() {
        assert_eq!(duration_ms(0.0), 1);
        assert_eq!(duration_ms(0.000_001), 1);
        assert_eq!(duration_ms(0.001), 1);
        assert_eq!(duration_ms(0.001_001), 2);
        assert_eq!(duration_ms(1.0), 1_000);
        assert_eq!(duration_ms(f64::NAN), 1);
        assert_eq!(duration_ms(f64::MAX), u64::MAX);
    }

    #[test]
    fn signature_normalization_keeps_field_names_and_removes_jitter() {
        let normalized = normalize_signature("pid=42 task=7 address=0xfeed");
        assert_eq!(
            normalized,
            "pid=<volatile> task=<volatile> address=0x<address>"
        );
    }

    /// A poll count is per-attempt jitter: nine hangs sharing one pinning task
    /// would cluster as nine separate causes if it survived into the signature.
    /// The gate state beside it is the discriminator and must survive.
    #[test]
    fn a_pinning_tasks_poll_count_does_not_split_one_cause_into_many() {
        let normalized = normalize_signature("active_async holder polls=41231 state=Runnable");
        assert_eq!(
            normalized,
            "active_async holder polls=<volatile> state=Runnable"
        );
    }

    /// The fixture server mints a fresh asset id per attempt, so a playlist line
    /// carrying one can only ever appear in the attempt that wrote it. The
    /// divergence pass then reports every such line as separating failures from
    /// passes, which is how eighteen rows of one run's ABR failure said nothing.
    #[test]
    fn a_per_attempt_asset_identity_does_not_separate_a_failure_from_a_pass() {
        let first = normalize_signature(
            "fetching url=http://127.0.0.1:8080/stream/3a2f20a5-40f9-45a3-8c0b-67e0f6f8d1e1/v0.m3u8 asset_root=c26bf5add3a9ad105e989b6e35595621",
        );
        let second = normalize_signature(
            "fetching url=http://127.0.0.1:8080/stream/32c0ac7e-a24a-446c-8e86-1a24e509164d/v0.m3u8 asset_root=e0f774bd91c2a3845f6b0d7e8a9c1b23",
        );

        assert_eq!(first, second);
        assert!(first.contains("<uuid>"), "{first}");
        assert!(first.contains("<digest>"), "{first}");
    }

    /// The cache path writes the same digest behind a namespace letter, so no
    /// word boundary falls before it.
    #[test]
    fn a_namespaced_cache_digest_is_normalized_too() {
        let first =
            normalize_signature("rel_path=track/host~oa47d51d70114ab95b6c833c29678b24d/stream");
        let second =
            normalize_signature("rel_path=track/host~o2125f04c437b75858496455a66cf20f3/stream");

        assert_eq!(first, second);
        assert!(first.contains("<digest>"), "{first}");
    }

    /// A decimal counter is not an identity, and the tables these signatures
    /// feed are ordered by exactly such counters.
    /// The cache key writes `127.0.0.1:34265` as `127.0.0.1~3a34265`, and the
    /// number pass needs a word boundary the hex escape denies it.
    #[test]
    fn an_escaped_authority_port_does_not_separate_a_failure_from_a_pass() {
        let first = normalize_signature("rel_path=track/127.0.0.1~3a34265~oabc/stream");
        let second = normalize_signature("rel_path=track/127.0.0.1~3a39797~oabc/stream");

        assert_eq!(first, second, "{first} vs {second}");
    }

    #[test]
    fn a_long_decimal_counter_is_not_read_as_a_digest() {
        let normalized = normalize_signature("committed=1207437641712345678 state=Runnable");

        assert_eq!(normalized, "committed=1207437641712345678 state=Runnable");
    }

    /// The first line the flash engine writes into a hang dump.
    const ENGINE_COUNTERS: &str = "virtual_now_ns=86410020000000 active=1 active_async=0 real_io=0 pace_anchor=none yielders=0";

    /// The engine's counter line is neither a primitive, a holder, nor a
    /// waiter, so `direct_markers` is the only route that carries it into a
    /// section — and it is the causal line of the whole dump.
    #[test]
    fn the_engine_counters_become_their_own_wait_signature() {
        let mut evidence = evidence();
        evidence.direct_markers.push("pace_anchor=".to_owned());

        let signatures = wait_signatures(
            &format!("[wait dump] audio_worker_loop\n{ENGINE_COUNTERS}\n"),
            &evidence,
        );

        assert!(
            signatures.iter().any(|s| s.contains("pace_anchor=none")),
            "{signatures:?}"
        );
    }

    /// `real_io` counts the real-I/O leases outstanding. Masked as jitter, the
    /// signature could no longer tell a test that outran its own socket from
    /// one whose work never started.
    #[test]
    fn a_pacing_signature_keeps_the_real_io_count() {
        let normalized = normalize_wait(ENGINE_COUNTERS);

        assert!(normalized.contains("real_io=0"), "{normalized}");
    }

    /// Whether the clock is anchored to real time is the other half of the
    /// same verdict: `real_io>0` with no anchor is a clock free-running past
    /// work in flight.
    #[test]
    fn a_pacing_signature_keeps_the_pace_anchor_state() {
        let normalized = normalize_wait(ENGINE_COUNTERS);

        assert!(normalized.contains("pace_anchor=none"), "{normalized}");
    }

    /// The virtual clock reads differently on every attempt; kept, it would
    /// give each hang a cluster of its own.
    #[test]
    fn a_pacing_signature_drops_the_virtual_clock() {
        let normalized = normalize_wait(ENGINE_COUNTERS);

        assert!(
            normalized.contains("virtual_now_ns=<volatile>"),
            "{normalized}"
        );
    }

    /// Shaped like the retained output of a failed `assert_eq!`: the kind, the
    /// panic header, then the assertion's message and its values.
    fn panic_output(thread_id: &str) -> String {
        format!(
            "test failure with exit code 101: thread 'demo::warms_pool' ({thread_id}) panicked at tests/demo.rs:166:5:\n\
             assertion `left == right` failed: a warmed pool must serve decode-sized buffers without allocating\n\
             \x20 left: 0\n\
             \x20right: 1\n\
             stack backtrace:\n\
             \x20  0: __rustc::rust_begin_unwind\n"
        )
    }

    /// The values are the whole diagnosis: one miss out of eight is a
    /// different defect from eight out of eight, and a signature that stops at
    /// "an assertion failed" cannot tell a reader which they have.
    #[test]
    fn a_panic_signature_carries_the_assertion_that_failed() {
        let mut failed = case("warms_pool", 0, true, 1.0);
        failed.output = panic_output("971370");

        let signature = failure_signature(&failed, &evidence());

        assert!(
            signature.contains("a warmed pool must serve"),
            "{signature}"
        );
        assert!(signature.contains("left: 0"), "{signature}");
        assert!(signature.contains("right: 1"), "{signature}");
    }

    /// The thread's name is the test's own, and the examples column already
    /// carries it. In a symptom cluster it is width spent on what the row
    /// does not ask.
    #[test]
    fn a_panic_signature_drops_the_thread_the_examples_already_name() {
        let mut failed = case("warms_pool", 0, true, 1.0);
        failed.output = panic_output("971370");

        let signature = failure_signature(&failed, &evidence());

        assert!(!signature.contains("demo::warms_pool"), "{signature}");
        assert!(signature.contains("tests/demo.rs:166:5"), "{signature}");
    }

    /// The point of a symptom cluster is that one cause is one row. The thread
    /// id changes on every attempt, so leaving it in the signature turned
    /// thirteen failures of one assertion into thirteen clusters of one — the
    /// section grouped nothing exactly where it was needed.
    #[test]
    fn two_attempts_of_one_assertion_share_a_signature() {
        let mut first = case("warms_pool", 0, true, 1.0);
        first.output = panic_output("971370");
        let mut second = case("warms_pool", 7, true, 1.0);
        second.output = panic_output("208442");

        assert_eq!(
            failure_signature(&first, &evidence()),
            failure_signature(&second, &evidence())
        );
    }

    /// The header arrives twice; spending the signature's width on saying it
    /// twice is what pushed the payload past the cell bound.
    #[test]
    fn a_panic_signature_does_not_repeat_its_own_header() {
        let mut failed = case("warms_pool", 0, true, 1.0);
        failed.output = panic_output("971370");

        let signature = failure_signature(&failed, &evidence());

        assert_eq!(signature.matches("panicked at").count(), 1, "{signature}");
    }

    #[test]
    fn hang_symptom_ignores_embedded_envelope_and_dump_filename() {
        let mut failed = case("seek", 0, true, 1.0);
        failed.output = "[hang] detected: pre-kill ts_ms=42 pid=7 dump=/tmp/hang-42.json [still running] \u{2014} {\"nextest\":{\"attempt_id\":\"run:a\"}}".to_owned();

        let signature = failure_signature(&failed, &evidence());

        assert_eq!(
            signature,
            "[hang] detected: pre-kill ts_ms=<volatile> pid=<volatile> dump=<volatile> [still running]"
        );
        assert!(!signature.contains("attempt_id"));
    }

    #[test]
    fn hang_symptom_ignores_ascii_embedded_envelope() {
        let mut failed = case("seek", 0, true, 1.0);
        failed.output =
            "[hang] detected: pre-kill ts_ms=42 payload={\"nextest\":{\"attempt_id\":\"run:a\"}}"
                .to_owned();

        assert_eq!(
            failure_signature(&failed, &evidence()),
            "[hang] detected: pre-kill ts_ms=<volatile>"
        );
    }

    #[test]
    fn nextest_timeout_class_requires_an_envelope_without_marker() {
        let evidence = evidence();
        assert!(requires_envelope(
            "test timeout: after 120s: process did not exit",
            &evidence,
        ));
        assert!(requires_envelope(
            "assertion failed\nHARD TIMEOUT after 30s",
            &evidence,
        ));
        assert!(!requires_envelope(
            "test failure: timeout setting wrong",
            &evidence,
        ));
        assert!(!requires_envelope(
            "test failure: assertion timed out after 120ms",
            &evidence,
        ));
    }
}
