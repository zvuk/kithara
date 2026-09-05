use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use super::{
    AttemptDossier, SignatureCluster, add_signature,
    attempt::{
        AttemptKey, AttemptMetadata, AttemptOutcome, foreign_run, outcome_for, render_key,
        render_test,
    },
    normalize_signature, render_clusters, strip_ansi, wait_signatures,
};
use crate::common::project::{StressEvidenceConfig, StressRenderBudgets};

const MAX_ENVELOPE_BYTES: u64 = 4 * 1_024 * 1_024;
const MAX_ENVELOPE_DIRECTORY_ENTRIES: usize = 100_000;
/// Newest run-length groups of a failed attempt's probe tail kept in its
/// dossier row. The tail's tempo is the verdict — a branch marker repeated
/// hundreds of times is a starving pass loop — so groups carry `(xN)` counts.
const FLIGHT_TAIL_GROUPS: usize = 4;

/// Flight-recorder tail lines from attempt envelopes, clustered across
/// repeats. Only failed attempts write dumps, so the passed column stays
/// zero; the signal is how many failed attempts share a line.
#[derive(Debug, Default)]
pub(super) struct FlightClusters {
    pub(super) events: BTreeMap<String, SignatureCluster>,
    pub(super) probes: BTreeMap<String, SignatureCluster>,
}

#[derive(Debug, Deserialize)]
struct AttemptEnvelope {
    nextest: EnvelopeNextest,
    diagnostic: String,
    label: String,
    schema: String,
    context: Value,
    /// Flight-recorder tails; absent in envelopes written before the recorder
    /// existed, so they default to empty rather than invalidating the record.
    #[serde(default)]
    flight_events: Vec<String>,
    #[serde(default)]
    flight_probes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EnvelopeNextest {
    attempt_id: Option<String>,
    binary_id: Option<String>,
    run_id: Option<String>,
    stress_current: Option<String>,
    test_name: Option<String>,
}

#[derive(Clone, Copy)]
pub(super) struct Input<'a> {
    outcomes: &'a BTreeMap<AttemptKey, AttemptOutcome>,
    expected: &'a BTreeSet<AttemptKey>,
    evidence: &'a StressEvidenceConfig,
    budgets: &'a StressRenderBudgets,
    run_id: Option<&'a str>,
}

struct EnvelopeFiles {
    paths: Vec<PathBuf>,
    limit_exceeded: bool,
    invalid: usize,
}

impl<'a> Input<'a> {
    pub(super) const fn new(
        outcomes: &'a BTreeMap<AttemptKey, AttemptOutcome>,
        expected: &'a BTreeSet<AttemptKey>,
        run_id: Option<&'a str>,
        evidence: &'a StressEvidenceConfig,
        budgets: &'a StressRenderBudgets,
    ) -> Self {
        Self {
            outcomes,
            expected,
            evidence,
            budgets,
            run_id,
        }
    }
}

pub(super) fn append(
    out: &mut String,
    dir: &Path,
    input: Input<'_>,
    wait_clusters: &mut BTreeMap<String, SignatureCluster>,
    flight: &mut FlightClusters,
    dossiers: &mut BTreeMap<AttemptKey, AttemptDossier>,
) -> bool {
    let Some(files) = envelope_files(dir) else {
        let _ = writeln!(
            out,
            "\n## Attempt-envelope progress signatures\n\nThe requested envelope directory could not be read."
        );
        return false;
    };
    let mut invalid = files.invalid;
    let mut clusters = BTreeMap::new();
    let mut matched = BTreeSet::new();
    let mut foreign = 0usize;
    for path in files.paths {
        let Some(text) = read_envelope(&path) else {
            invalid = invalid.saturating_add(1);
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<AttemptEnvelope>(&text) else {
            invalid = invalid.saturating_add(1);
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            invalid = invalid.saturating_add(1);
            continue;
        };
        let payload = match input
            .evidence
            .envelope_text_field
            .as_deref()
            .and_then(|field| value.get(field))
        {
            None | Some(Value::Null) => None,
            Some(Value::String(payload)) => Some(payload.clone()),
            Some(_) => {
                invalid = invalid.saturating_add(1);
                continue;
            }
        };
        if input.evidence.envelope_schema.as_deref() != Some(envelope.schema.as_str()) {
            invalid = invalid.saturating_add(1);
            continue;
        }
        let Ok(attempt) = AttemptMetadata::from_fields(
            envelope.nextest.binary_id.as_deref(),
            envelope.nextest.test_name.as_deref(),
            envelope.nextest.stress_current.as_deref(),
            envelope.nextest.run_id.as_deref(),
            envelope.nextest.attempt_id.as_deref(),
        ) else {
            invalid = invalid.saturating_add(1);
            continue;
        };
        if foreign_run(&attempt, input.run_id) {
            foreign = foreign.saturating_add(1);
            continue;
        }

        matched.insert(attempt.key.clone());
        let test = render_test(&attempt.key);
        let outcome = outcome_for(&attempt, input.outcomes);
        let context = envelope.context.to_string();
        let signature = normalize_diagnostic(
            &format!("{}: {}", envelope.label, envelope.diagnostic),
            input.budgets,
        );
        if outcome == AttemptOutcome::Failed
            && let Some(dossier) = dossiers.get_mut(&attempt.key)
        {
            dossier.envelopes.insert(signature.clone());
            if dossier.flight_tail.is_empty() {
                dossier.flight_tail =
                    newest_groups(fold_flight_tail(&envelope.flight_probes, input.budgets));
            }
            if dossier.event_tail.is_empty() {
                dossier.event_tail =
                    newest_groups(fold_flight_tail(&envelope.flight_events, input.budgets));
            }
        }
        add_signature(
            &mut clusters,
            signature,
            &attempt.display,
            &test,
            outcome,
            Some(context.as_str()),
            input.budgets,
        );
        add_flight_signatures(flight, &envelope, &attempt, &test, outcome, input.budgets);
        if let Some(payload) = payload {
            add_wait_signatures(
                wait_clusters,
                dossiers,
                &payload,
                &attempt,
                &test,
                outcome,
                input,
            );
        }
    }
    render_clusters(
        out,
        "Attempt-envelope progress signatures",
        &clusters,
        "These pair the stalled source location with the last observed progress point and the exact nextest attempt.",
        input.budgets,
    );
    if foreign > 0 {
        let _ = writeln!(
            out,
            "\nIgnored `{foreign}` envelope records from a different nextest run."
        );
    }
    if invalid > 0 {
        let _ = writeln!(
            out,
            "\nEvidence problem: `{invalid}` envelope artifacts were unreadable, oversized, malformed, or carried invalid nextest metadata."
        );
    }
    if files.limit_exceeded {
        let _ = writeln!(
            out,
            "\nEvidence problem: the envelope directory exceeds the deterministic limit of `{MAX_ENVELOPE_DIRECTORY_ENTRIES}` entries. The raw directory was left untouched."
        );
    }
    let missing = input.expected.difference(&matched).collect::<Vec<_>>();
    if !missing.is_empty() {
        let examples = missing
            .iter()
            .take(input.budgets.signature_examples)
            .map(|key| super::markdown_cell(&render_key(key), input.budgets))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "\nEvidence problem: `{}` timeout-class failed attempts had no exact same-run attempt envelope: {examples}",
            missing.len(),
        );
    }
    invalid == 0 && !files.limit_exceeded && missing.is_empty()
}

/// Cluster the flight-recorder tails of one envelope. Both lanes share the
/// attempt attribution, so they differ only in which ring they came from.
fn add_flight_signatures(
    flight: &mut FlightClusters,
    envelope: &AttemptEnvelope,
    attempt: &AttemptMetadata,
    test: &str,
    outcome: AttemptOutcome,
    budgets: &StressRenderBudgets,
) {
    for (lane, lines) in [
        (&mut flight.events, &envelope.flight_events),
        (&mut flight.probes, &envelope.flight_probes),
    ] {
        for line in lines
            .iter()
            .map(|line| normalize_flight_line(line, budgets))
            .collect::<BTreeSet<_>>()
        {
            add_signature(lane, line, &attempt.display, test, outcome, None, budgets);
        }
    }
}

/// Cluster the wait-graph signatures carried by one envelope payload, and
/// record them on the attempt's dossier when the attempt failed.
fn add_wait_signatures(
    wait_clusters: &mut BTreeMap<String, SignatureCluster>,
    dossiers: &mut BTreeMap<AttemptKey, AttemptDossier>,
    payload: &str,
    attempt: &AttemptMetadata,
    test: &str,
    outcome: AttemptOutcome,
    input: Input<'_>,
) {
    for signature in wait_signatures(payload, input.evidence, input.budgets) {
        if outcome == AttemptOutcome::Failed
            && let Some(dossier) = dossiers.get_mut(&attempt.key)
        {
            dossier.wait_graph.insert(signature.clone());
        }
        add_signature(
            wait_clusters,
            signature,
            &attempt.display,
            test,
            outcome,
            None,
            input.budgets,
        );
    }
}

fn envelope_files(dir: &Path) -> Option<EnvelopeFiles> {
    let entries = fs::read_dir(dir).ok()?;
    let mut paths = Vec::new();
    let mut invalid = 0usize;
    let mut limit_exceeded = false;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ENVELOPE_DIRECTORY_ENTRIES {
            limit_exceeded = true;
            break;
        }
        let Ok(entry) = entry else {
            invalid = invalid.saturating_add(1);
            continue;
        };
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            paths.push(path);
        }
    }
    paths.sort();
    Some(EnvelopeFiles {
        paths,
        limit_exceeded,
        invalid,
    })
}

/// The tick counter measures how long the watchdog watched, not what stalled.
/// Left in the signature it split one stall into a cluster per attempt — the
/// section grouped nothing exactly where grouping was its purpose.
fn normalize_diagnostic(text: &str, budgets: &StressRenderBudgets) -> String {
    static TICKS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b\d+ tick\(s\)").expect("tick counter regex"));
    normalize_signature(&TICKS.replace_all(text, "<n> tick(s)"), budgets)
}

/// Flight lines carry per-call numeric fields (`seq=7`, `queue_len=12`) that
/// vary on every firing; left in place each firing becomes its own cluster.
/// String fields — the branch and probe names that identify the site — stay.
///
/// `caller_line` is exempt: it is a source location, not a per-call counter.
/// Masked, the section named the file a probe fired from and erased the line
/// inside it, so reading a signature meant opening the raw dump to find the
/// site by hand — the one thing the section exists to spare.
fn normalize_flight_line(line: &str, budgets: &StressRenderBudgets) -> String {
    static NUMERIC_FIELDS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\w+)=\d+\b").expect("numeric field regex"));
    let (line, _) = split_recorded_repeats(line);
    let masked = NUMERIC_FIELDS.replace_all(line, |caps: &regex::Captures<'_>| {
        let key = &caps[1];
        if key == "caller_line" {
            caps[0].to_owned()
        } else {
            format!("{key}=<n>")
        }
    });
    normalize_signature(&masked, budgets)
}

/// The in-test recorder folds a line that repeats inside its window, so a
/// line can arrive already counted. The count says how many firings the line
/// stands for — it belongs in the folded tail, never in the signature, where
/// it would split one cause into a cluster per repeat count.
fn split_recorded_repeats(line: &str) -> (&str, usize) {
    let Some(rest) = line.strip_suffix(')') else {
        return (line, 1);
    };
    let Some((head, count)) = rest.rsplit_once(" (x") else {
        return (line, 1);
    };
    count.parse().map_or((line, 1), |count| (head, count))
}

/// Run-length encode a flight tail: a starving pass loop fires the same probe
/// hundreds of times in a row, and `line (x312)` says so where three hundred
/// identical rows would say nothing. Grouping folds on the normalized form so
/// drifting values do not split one loop into many rows, but each group shows
/// its last firing's real field values — the tail is the verdict, and a
/// verdict of `queue_head=<n>` names no cause.
fn fold_flight_tail(lines: &[String], budgets: &StressRenderBudgets) -> Vec<String> {
    struct Group {
        key: String,
        display: String,
        count: usize,
    }
    let mut folded: Vec<Group> = Vec::new();
    for line in lines {
        let (head, repeats) = split_recorded_repeats(line);
        let key = normalize_flight_line(line, budgets);
        let display = display_flight_line(head);
        match folded.last_mut() {
            Some(group) if group.key == key => {
                group.count += repeats;
                group.display = display;
            }
            _ => folded.push(Group {
                key,
                display,
                count: repeats,
            }),
        }
    }
    folded
        .into_iter()
        .map(|group| {
            if group.count > 1 {
                format!("{} (x{})", group.display, group.count)
            } else {
                group.display
            }
        })
        .collect()
}

/// The newest run-length groups of a folded tail — the dossier's bounded view.
fn newest_groups(mut tail: Vec<String>) -> Vec<String> {
    let newest = tail.len().saturating_sub(FLIGHT_TAIL_GROUPS);
    tail.split_off(newest)
}

/// A tail line kept readable: real field values stay, only the recorder's
/// per-firing counters go — they order lines inside the ring and differ on
/// every firing, saying nothing about the state a group stands for.
fn display_flight_line(line: &str) -> String {
    static RECORDER_COUNTERS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\s+(?:seq|thread_id|thread_seq|install_id)=\d+\b")
            .expect("recorder counter regex")
    });
    let line = strip_ansi(line).replace(['\r', '\n'], " ");
    RECORDER_COUNTERS.replace_all(&line, "").trim().to_owned()
}

fn read_envelope(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length > MAX_ENVELOPE_BYTES {
        return None;
    }
    let capacity = usize::try_from(length).ok()?;
    let mut bytes = Vec::with_capacity(capacity.checked_add(1)?);
    let max_bytes = usize::try_from(MAX_ENVELOPE_BYTES).ok()?;
    file.take(MAX_ENVELOPE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> StressEvidenceConfig {
        StressEvidenceConfig {
            envelope_schema: Some("demo.hang.v1".to_owned()),
            envelope_text_field: Some("wait_graph".to_owned()),
            dump_marker: Some("[wait dump]".to_owned()),
            primitive_marker: Some("created_at=".to_owned()),
            holder_marker: Some("held by".to_owned()),
            wait_marker: Some("WAITING:".to_owned()),
            ..StressEvidenceConfig::default()
        }
    }

    fn key() -> AttemptKey {
        AttemptKey {
            suite: "demo::tests".to_owned(),
            name: "seek".to_owned(),
            iteration: 0,
        }
    }

    fn dossier(key: AttemptKey) -> BTreeMap<AttemptKey, AttemptDossier> {
        BTreeMap::from([(
            key,
            AttemptDossier {
                display: "failed-attempt".to_owned(),
                ..AttemptDossier::default()
            },
        )])
    }

    /// One stall is one row: the watchdog's tick counter differs on every
    /// attempt and must not split a cluster.
    #[test]
    fn tick_counts_do_not_split_a_diagnostic_signature() {
        let first = normalize_diagnostic(
            "audio_worker_loop: stuck at observer.rs:173 | last progress at observer.rs:166 | 118 tick(s) since progress | timeout 1s",
            &StressRenderBudgets::default(),
        );
        let second = normalize_diagnostic(
            "audio_worker_loop: stuck at observer.rs:173 | last progress at observer.rs:166 | 183 tick(s) since progress | timeout 1s",
            &StressRenderBudgets::default(),
        );

        assert_eq!(first, second);
        assert!(first.contains("<n> tick(s)"), "{first}");
        assert!(first.contains("timeout 1s"), "{first}");
    }

    /// The recorder folds a line that repeats inside its window, so the same
    /// event reaches the report already counted — and the count differs per
    /// attempt. Left in the signature it would split one cause per count.
    #[test]
    fn a_recorded_repeat_count_does_not_split_one_flight_line_into_many() {
        let first = normalize_flight_line(
            "DEBUG kithara_hls::settle: success (x7)",
            &StressRenderBudgets::default(),
        );
        let second = normalize_flight_line(
            "DEBUG kithara_hls::settle: success (x31)",
            &StressRenderBudgets::default(),
        );

        assert_eq!(first, second);
    }

    /// The count is evidence in its own right — how many firings the folded
    /// line stands for — so the tail must carry it, not drop it with the
    /// signature.
    #[test]
    fn a_folded_tail_sums_the_recorded_repeat_counts() {
        let folded = fold_flight_tail(
            &[
                "DEBUG kithara_hls::settle: success (x7)".to_owned(),
                "DEBUG kithara_hls::settle: success (x5)".to_owned(),
            ],
            &StressRenderBudgets::default(),
        );

        assert_eq!(folded.len(), 1, "{folded:?}");
        assert!(folded[0].ends_with("(x12)"), "{folded:?}");
    }

    /// The dossier tail is the verdict: a starving loop's last firing names
    /// the exact state it died in, so each folded group must keep real field
    /// values instead of `<n>` placeholders.
    #[test]
    fn a_folded_tail_keeps_the_last_firing_field_values() {
        let folded = fold_flight_tail(&[
            r#"TRACE kithara_hls_probe: probe="dispatch_from" caller_line=39 seq=100 variant=2 budget=2 queue_len=3 queue_head=26 cap=26"#.to_owned(),
            r#"TRACE kithara_hls_probe: probe="dispatch_from" caller_line=39 seq=101 variant=2 budget=2 queue_len=3 queue_head=27 cap=26"#.to_owned(),
        ], &StressRenderBudgets::default());

        assert_eq!(folded.len(), 1, "one form is one group: {folded:?}");
        assert!(folded[0].contains("queue_head=27"), "{folded:?}");
        assert!(folded[0].contains("cap=26"), "{folded:?}");
        assert!(!folded[0].contains("<n>"), "{folded:?}");
        assert!(folded[0].ends_with("(x2)"), "{folded:?}");
    }

    /// The recorder's per-firing counters order lines inside the ring and
    /// differ on every firing; in a tail group they are noise that buries the
    /// state fields the group exists to show.
    #[test]
    fn a_folded_tail_drops_the_recorder_counters() {
        let folded = fold_flight_tail(&[
            r#"TRACE kithara_hls_probe: probe="total_bytes" caller_line=11 seq=54634 thread_id=11876854719037224982 thread_seq=2249 install_id=1 variant=1 total=437160"#.to_owned(),
        ], &StressRenderBudgets::default());

        assert!(!folded[0].contains("seq="), "{folded:?}");
        assert!(!folded[0].contains("thread_id="), "{folded:?}");
        assert!(!folded[0].contains("install_id="), "{folded:?}");
        assert!(folded[0].contains("total=437160"), "{folded:?}");
    }

    /// Grouping still folds on the normalized form: values that drift between
    /// firings (a moving byte offset) must not break one loop into many rows.
    #[test]
    fn drifting_values_do_not_split_a_tail_group() {
        let folded = fold_flight_tail(
            &[
                "DEBUG demo: read byte_offset=100".to_owned(),
                "DEBUG demo: read byte_offset=200".to_owned(),
                "DEBUG demo: read byte_offset=300".to_owned(),
            ],
            &StressRenderBudgets::default(),
        );

        assert_eq!(folded.len(), 1, "{folded:?}");
        assert!(folded[0].contains("byte_offset=300"), "{folded:?}");
        assert!(folded[0].ends_with("(x3)"), "{folded:?}");
    }

    /// The event tail is the state chronology immediately preceding the
    /// failure — it must land in the failed attempt's dossier next to the
    /// probe tail instead of staying only in the raw dump.
    #[test]
    fn the_event_tail_lands_in_the_dossier() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("hang.json"),
            r#"{
  "schema":"demo.hang.v1",
  "label":"audio_worker_loop",
  "diagnostic":"stuck",
  "nextest":{
    "run_id":"run",
    "binary_id":"demo::tests",
    "test_name":"seek",
    "stress_current":"0"
  },
  "context":{},
  "flight_events":[
    "DEBUG kithara_abr: ABR: tick estimate_bps=26204656",
    "DEBUG kithara_abr: ABR: tick estimate_bps=18852415",
    "DEBUG kithara_audio: decoder returned EOF chunks=435 pos=2000000"
  ],
  "flight_probes":[]
}"#,
        )
        .expect("write event fixture");
        let key = key();
        let outcomes = BTreeMap::from([(key.clone(), AttemptOutcome::Failed)]);
        let mut dossiers = dossier(key);
        let mut markdown = String::new();
        let evidence = evidence();

        assert!(append(
            &mut markdown,
            temp.path(),
            Input::new(
                &outcomes,
                &BTreeSet::new(),
                Some("run"),
                &evidence,
                &StressRenderBudgets::default(),
            ),
            &mut BTreeMap::new(),
            &mut FlightClusters::default(),
            &mut dossiers,
        ));

        let tail = &dossiers.values().next().expect("dossier").event_tail;
        assert_eq!(
            tail.len(),
            2,
            "ticks fold, EOF stays its own group: {tail:?}"
        );
        assert!(
            tail[0].contains("estimate_bps=18852415") && tail[0].ends_with("(x2)"),
            "the folded group shows its last firing: {tail:?}"
        );
        assert!(tail[1].contains("pos=2000000"), "{tail:?}");
    }

    #[test]
    fn unknown_schema_is_incomplete() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("hang.json"),
            r#"{"schema":"other","label":"stalled","diagnostic":"none"}"#,
        )
        .expect("write hang fixture");
        let mut markdown = String::new();
        let outcomes = BTreeMap::new();
        let expected = BTreeSet::new();
        let evidence = evidence();

        assert!(!append(
            &mut markdown,
            temp.path(),
            Input::new(
                &outcomes,
                &expected,
                None,
                &evidence,
                &StressRenderBudgets::default(),
            ),
            &mut BTreeMap::new(),
            &mut FlightClusters::default(),
            &mut BTreeMap::new(),
        ));
        assert!(markdown.contains("`1` envelope artifacts"), "{markdown}");
    }

    #[test]
    fn oversized_envelope_is_rejected_without_unbounded_reading() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("hang.json");
        let file = File::create(&path).expect("create fixture");
        file.set_len(MAX_ENVELOPE_BYTES + 1).expect("size fixture");

        assert!(read_envelope(&path).is_none());
    }

    #[test]
    fn timeout_requires_an_exact_same_run_envelope() {
        let temp = tempfile::tempdir().expect("tempdir");
        let key = key();
        let expected = BTreeSet::from([key.clone()]);
        let outcomes = BTreeMap::from([(key.clone(), AttemptOutcome::Failed)]);
        let mut dossiers = dossier(key);
        let mut markdown = String::new();
        let evidence = evidence();

        assert!(!append(
            &mut markdown,
            temp.path(),
            Input::new(
                &outcomes,
                &expected,
                Some("run"),
                &evidence,
                &StressRenderBudgets::default(),
            ),
            &mut BTreeMap::new(),
            &mut FlightClusters::default(),
            &mut dossiers,
        ));
        assert!(
            markdown.contains("no exact same-run attempt envelope"),
            "{markdown}"
        );
    }

    #[test]
    fn configured_envelope_text_joins_the_failed_attempt() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("hang.json"),
            r##"{
  "schema":"demo.hang.v1",
  "label":"test-timeout",
  "diagnostic":"hard-timeout",
  "nextest":{
    "run_id":"run",
    "binary_id":"demo::tests",
    "test_name":"seek",
    "stress_current":"0"
  },
  "context":{},
  "wait_graph":"[wait dump] hard-timeout\n#7 Mutex created_at=crates/demo/src/state.rs:10\nheld by worker\nWAITING: reader"
}"##,
        )
        .expect("write hang fixture");
        let key = key();
        let expected = BTreeSet::from([key.clone()]);
        let outcomes = BTreeMap::from([(key.clone(), AttemptOutcome::Failed)]);
        let mut dossiers = dossier(key);
        let mut waits = BTreeMap::new();
        let mut markdown = String::new();
        let evidence = evidence();

        assert!(append(
            &mut markdown,
            temp.path(),
            Input::new(
                &outcomes,
                &expected,
                Some("run"),
                &evidence,
                &StressRenderBudgets::default(),
            ),
            &mut waits,
            &mut FlightClusters::default(),
            &mut dossiers,
        ));

        assert_eq!(waits.len(), 1);
        assert_eq!(
            dossiers.values().next().expect("dossier").wait_graph.len(),
            1
        );
        assert!(
            markdown.contains("demo::tests seek @stress-0"),
            "{markdown}"
        );
    }

    #[test]
    fn a_probe_signature_keeps_the_source_line_it_fired_from() {
        let line = normalize_flight_line(
            r#"TRACE kithara_abr_probe: probe="decide" caller_file="crates/kithara-abr/src/state/decision.rs" caller_line=104"#,
            &StressRenderBudgets::default(),
        );
        assert!(line.contains("caller_line=104"), "{line}");
    }

    #[test]
    fn a_probe_signature_still_masks_per_call_counters() {
        let line = normalize_flight_line(
            r#"TRACE kithara_abr_probe: probe="decide" caller_line=104 seq=7"#,
            &StressRenderBudgets::default(),
        );
        assert!(line.contains("seq=<n>"), "{line}");
    }

    #[test]
    fn flight_tails_cluster_across_attempts_and_order_the_dossier() {
        let temp = tempfile::tempdir().expect("tempdir");
        for (file, iteration, seq) in [("a.json", 0usize, 7u64), ("b.json", 3, 44)] {
            fs::write(
                temp.path().join(file),
                format!(
                    r#"{{
  "schema":"demo.hang.v1",
  "label":"audio_worker_loop",
  "diagnostic":"stuck",
  "nextest":{{
    "run_id":"run",
    "binary_id":"demo::tests",
    "test_name":"seek",
    "stress_current":"{iteration}"
  }},
  "context":{{}},
  "flight_events":["DEBUG kithara_hls: variant reader landed"],
  "flight_probes":[
    "TRACE kithara_hls_probe: dispatch_from seq={seq}",
    "TRACE kithara_audio_probe: waiting branch branch=decoding_transition_pending",
    "TRACE kithara_audio_probe: waiting branch branch=decoding_transition_pending",
    "TRACE kithara_audio_probe: waiting branch branch=decoding_transition_pending"
  ]
}}"#
                ),
            )
            .expect("write flight fixture");
        }
        let key = key();
        let expected = BTreeSet::new();
        let outcomes = BTreeMap::from([(key.clone(), AttemptOutcome::Failed)]);
        let mut dossiers = dossier(key);
        let mut flight = FlightClusters::default();
        let mut markdown = String::new();
        let evidence = evidence();

        assert!(append(
            &mut markdown,
            temp.path(),
            Input::new(
                &outcomes,
                &expected,
                Some("run"),
                &evidence,
                &StressRenderBudgets::default(),
            ),
            &mut BTreeMap::new(),
            &mut flight,
            &mut dossiers,
        ));

        let branch = flight
            .probes
            .keys()
            .find(|line| line.contains("decoding_transition_pending"))
            .expect("branch marker must cluster");
        let cluster = &flight.probes[branch];
        assert_eq!(
            cluster.failed_attempts.len() + cluster.unattributed_attempts.len(),
            2,
            "both attempts share one branch-marker cluster"
        );
        let dispatch = flight
            .probes
            .keys()
            .find(|line| line.contains("dispatch_from"))
            .expect("dispatch probe must cluster");
        assert!(
            dispatch.contains("seq=<n>"),
            "numeric fields must not split clusters: {dispatch}"
        );
        assert_eq!(
            flight
                .probes
                .keys()
                .filter(|line| line.contains("dispatch_from"))
                .count(),
            1,
            "seq=7 and seq=44 are one signature"
        );
        assert!(
            flight
                .events
                .keys()
                .any(|line| line.contains("variant reader landed")),
            "events land in their own lane"
        );
        let tail = &dossiers.values().next().expect("dossier").flight_tail;
        assert_eq!(tail.len(), 2, "consecutive repeats fold: {tail:?}");
        assert!(tail[0].contains("dispatch_from"), "{tail:?}");
        assert!(
            tail[1].contains("decoding_transition_pending") && tail[1].ends_with("(x3)"),
            "the newest group carries its streak count: {tail:?}"
        );
    }

    #[test]
    fn an_envelope_without_flight_fields_still_parses() {
        assert!(fold_flight_tail(&[], &StressRenderBudgets::default()).is_empty());

        let envelope: AttemptEnvelope = serde_json::from_str(
            r#"{"schema":"demo.hang.v1","label":"stalled","diagnostic":"none","nextest":{},"context":{}}"#,
        )
        .expect("legacy envelope without flight fields");
        assert!(envelope.flight_events.is_empty());
        assert!(envelope.flight_probes.is_empty());
    }

    #[test]
    fn foreign_or_duplicate_metadata_does_not_satisfy_expected_attempt() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(
            temp.path().join("foreign.json"),
            r#"{"schema":"demo.hang.v1","label":"stalled","diagnostic":"none","nextest":{"run_id":"foreign","binary_id":"demo::tests","attempt_id":"opaque/retry#2","test_name":"seek","stress_current":"0"},"context":{},"wait_graph":null}"#,
        )
        .expect("write foreign fixture");
        fs::write(
            temp.path().join("duplicate.json"),
            r#"{"schema":"demo.hang.v1","label":"stalled","diagnostic":"none","nextest":{"run_id":"run","binary_id":"demo::tests","binary_id":"other","test_name":"seek","stress_current":"0"},"context":{},"wait_graph":null}"#,
        )
        .expect("write duplicate fixture");
        let key = key();
        let mut markdown = String::new();
        let outcomes = BTreeMap::from([(key.clone(), AttemptOutcome::Failed)]);
        let expected = BTreeSet::from([key]);
        let evidence = evidence();

        assert!(!append(
            &mut markdown,
            temp.path(),
            Input::new(
                &outcomes,
                &expected,
                Some("run"),
                &evidence,
                &StressRenderBudgets::default(),
            ),
            &mut BTreeMap::new(),
            &mut FlightClusters::default(),
            &mut BTreeMap::new(),
        ));
        assert!(
            markdown.contains("Ignored `1` envelope records"),
            "{markdown}"
        );
        assert!(markdown.contains("`1` envelope artifacts"), "{markdown}");
        assert!(
            markdown.contains("no exact same-run attempt envelope"),
            "{markdown}"
        );
    }
}
