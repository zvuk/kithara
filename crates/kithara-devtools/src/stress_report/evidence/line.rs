use std::{collections::BTreeMap, fmt::Write as _, ops::ControlFlow, path::Path, sync::LazyLock};

use regex::Regex;

use super::{
    AttemptDossier, SignatureCluster, add_signature,
    attempt::{AttemptKey, AttemptOutcome, foreign_run, outcome_for, render_test, split_prefix},
    line_reader::for_each_bounded_line,
    normalize_signature, render_clusters,
};
use crate::common::project::StressRenderBudgets;

const MAX_LINE_BYTES: usize = 64 * 1_024;
const MAX_RECORDS: usize = 250_000;

pub(super) fn append(
    out: &mut String,
    path: &Path,
    marker: Option<&str>,
    outcomes: &BTreeMap<AttemptKey, AttemptOutcome>,
    run_id: Option<&str>,
    dossiers: &mut BTreeMap<AttemptKey, AttemptDossier>,
    budgets: &StressRenderBudgets,
) -> bool {
    let mut clusters = BTreeMap::<String, SignatureCluster>::new();
    let mut foreign = 0usize;
    let mut invalid = 0usize;
    let mut records = 0usize;
    let read = for_each_bounded_line(path, MAX_LINE_BYTES, |line| {
        if !marker.is_some_and(|marker| line.contains(marker)) {
            return ControlFlow::Continue(());
        }
        if records == MAX_RECORDS {
            return ControlFlow::Break(());
        }
        records = records.saturating_add(1);
        let Ok((attempt, evidence)) = split_prefix(line) else {
            invalid = invalid.saturating_add(1);
            return ControlFlow::Continue(());
        };
        if foreign_run(&attempt, run_id) {
            foreign = foreign.saturating_add(1);
            return ControlFlow::Continue(());
        }
        let outcome = outcome_for(&attempt, outcomes);
        let test = render_test(&attempt.key);
        let signature = normalize(evidence, budgets);
        if outcome == AttemptOutcome::Failed
            && let Some(dossier) = dossiers.get_mut(&attempt.key)
        {
            dossier.lines.insert(signature.clone());
        }
        add_signature(
            &mut clusters,
            signature,
            &attempt.display,
            &test,
            outcome,
            None,
            budgets,
        );
        ControlFlow::Continue(())
    });
    let Ok(read) = read else {
        let _ = writeln!(
            out,
            "\n## Line-evidence cause signatures\n\nThe requested line artifact could not be read."
        );
        return false;
    };

    render_clusters(
        out,
        "Line-evidence cause signatures",
        &clusters,
        "A repeated CPU-spin poll is strong runtime evidence. A low-CPU blocked-wait signature remains a candidate until a wait-graph edge or same-revision low-pressure A/B confirms it. Durations are normalized so scheduling jitter does not split a cluster.",
        budgets,
    );
    if foreign > 0 {
        let _ = writeln!(
            out,
            "\nIgnored `{foreign}` line records from a different nextest run."
        );
    }
    let unreadable = read
        .invalid_utf8_lines
        .saturating_add(read.oversized_lines)
        .saturating_add(invalid);
    if unreadable > 0 {
        let _ = writeln!(
            out,
            "\nEvidence problem: `{unreadable}` line records were malformed, oversized, invalid UTF-8, or carried invalid nextest metadata."
        );
    }
    if read.stopped_early {
        let _ = writeln!(
            out,
            "\nEvidence problem: line parsing stopped at the bounded limit of `{MAX_RECORDS}` records."
        );
    }
    unreadable == 0 && !read.stopped_early
}

pub(super) fn normalize(text: &str, budgets: &StressRenderBudgets) -> String {
    static DURATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b\d+(?:\.\d+)?(?:ns|us|\x{00B5}s|ms|s)\b").expect("duration regex")
    });
    let normalized = DURATION.replace_all(text, "<duration>");
    normalize_signature(&normalized, budgets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::junit::CaseTiming;

    fn case(iteration: usize, failed: bool) -> CaseTiming {
        CaseTiming {
            failed,
            name: "seek".to_owned(),
            suite: "demo::tests".to_owned(),
            iteration: Some(iteration),
            secs: 0.1,
            timestamp: None,
            output: String::new(),
            output_truncated: false,
        }
    }

    #[test]
    fn records_join_failed_and_passed_attempts() {
        let cases = vec![case(0, true), case(1, false)];
        let outcomes = super::super::attempt::attempt_outcomes(&cases);
        let failed_key = AttemptKey {
            suite: "demo::tests".to_owned(),
            name: "seek".to_owned(),
            iteration: 0,
        };
        let mut dossiers = BTreeMap::from([(
            failed_key,
            AttemptDossier {
                display: "failed-attempt".to_owned(),
                ..AttemptDossier::default()
            },
        )]);
        let temp = tempfile::tempdir().expect("tempdir");
        let log = temp.path().join("no-block.log");
        std::fs::write(
            &log,
            "[nextest run_id=run binary_id=demo::tests test_name=seek stress_current=0] [no_block] CPU spin 10ms\n\
[nextest run_id=run attempt_id=opaque%2Fretry%232 binary_id=demo::tests test_name=seek stress_current=1] [no_block] CPU spin 20ms\n\
[nextest run_id=foreign attempt_id=opaque-foreign binary_id=demo::tests test_name=seek stress_current=0] [no_block] CPU spin 30ms\n\
[nextest attempt_id=opaque-no-run binary_id=demo::tests test_name=seek stress_current=0] [no_block] CPU spin 40ms\n",
        )
        .expect("write no-block fixture");
        let mut markdown = String::new();

        assert!(append(
            &mut markdown,
            &log,
            Some("[no_block]"),
            &outcomes,
            Some("run"),
            &mut dossiers,
            &StressRenderBudgets::default(),
        ));

        assert!(markdown.contains("| 1 | 1 | 0 |"), "{markdown}");
        assert!(
            markdown.contains("demo::tests seek @stress-0"),
            "{markdown}"
        );
        assert!(markdown.contains("opaque/retry#2"), "{markdown}");
        assert!(markdown.contains("Ignored `2` line records"), "{markdown}");
        assert_eq!(dossiers.values().next().expect("dossier").lines.len(), 1);
    }

    #[test]
    fn duplicate_metadata_field_makes_evidence_incomplete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log = temp.path().join("no-block.log");
        std::fs::write(
            &log,
            "[nextest run_id=run run_id=other binary_id=demo::tests test_name=seek stress_current=0] [no_block] CPU spin\n",
        )
        .expect("write no-block fixture");
        let mut markdown = String::new();

        assert!(!append(
            &mut markdown,
            &log,
            Some("[no_block]"),
            &BTreeMap::new(),
            Some("run"),
            &mut BTreeMap::new(),
            &StressRenderBudgets::default(),
        ));
        assert!(markdown.contains("invalid nextest metadata"));
    }

    #[test]
    fn normalization_removes_timing_jitter() {
        assert_eq!(
            normalize(
                "single poll took 12\u{b5}s budget 4.5ms",
                &StressRenderBudgets::default()
            ),
            "single poll took <duration> budget <duration>"
        );
    }
}
