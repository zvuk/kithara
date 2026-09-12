use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum Verdict {
    Passed,
    Failed,
    Ignored,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Outcome {
    pub(super) verdict: Verdict,
    pub(super) output: String,
}

/// Verdicts a single device invocation reported, and how far it got.
#[derive(Debug, Default, Deserialize, Serialize)]
pub(super) struct Report {
    pub(super) tests: BTreeMap<String, Outcome>,
    pub(super) in_flight: Option<String>,
    pub(super) finished: bool,
}

pub(super) fn parse(output: &str) -> Report {
    let mut report = Report::default();
    let mut section: Option<(String, String)> = None;
    let mut running: Option<String> = None;
    for line in output.lines() {
        if let Some(name) = section_name(line) {
            close(section.take(), &mut report);
            section = Some((name.to_owned(), String::new()));
            continue;
        }
        if line == "failures:" || line.starts_with("test result: ") {
            close(section.take(), &mut report);
        }
        if let Some((_, body)) = &mut section {
            body.push_str(line);
            body.push('\n');
            continue;
        }
        if line.starts_with("test result: ") {
            report.finished = true;
            running = None;
            continue;
        }
        if let Some((name, rest)) = started(line) {
            running = Some(name);
            record(&mut running, rest, &mut report);
            continue;
        }
        record(&mut running, line, &mut report);
    }
    close(section.take(), &mut report);
    report.in_flight = running;
    report
}

/// libtest announces a test before running it, so the verdict reaches the same
/// line only when nothing the device writes lands in between.
fn started(line: &str) -> Option<(String, &str)> {
    let (name, rest) = line.strip_prefix("test ")?.split_once(" ... ")?;
    let name = name.strip_suffix(" - should panic").unwrap_or(name);
    Some((name.to_owned(), rest))
}

fn record(running: &mut Option<String>, reported: &str, report: &mut Report) {
    let Some(verdict) = verdict_of(reported) else {
        return;
    };
    let Some(name) = running.take() else {
        return;
    };
    report.tests.insert(
        name,
        Outcome {
            verdict,
            output: String::new(),
        },
    );
}

fn section_name(line: &str) -> Option<&str> {
    line.strip_prefix("---- ")?.strip_suffix(" stdout ----")
}

fn verdict_of(reported: &str) -> Option<Verdict> {
    match reported {
        "ok" => Some(Verdict::Passed),
        "FAILED" => Some(Verdict::Failed),
        _ if reported.starts_with("ignored") => Some(Verdict::Ignored),
        _ => None,
    }
}

fn close(section: Option<(String, String)>, report: &mut Report) {
    let Some((name, body)) = section else {
        return;
    };
    if let Some(outcome) = report.tests.get_mut(&name) {
        outcome.output = body.trim_end().to_owned();
    }
}

#[cfg(test)]
mod tests {
    use fixture::{ANDROID, BATCH, INTERRUPTED, NATIVE_NOISE, SHOULD_PANIC};

    use super::*;

    mod fixture {
        pub(super) const BATCH: &str = include_str!("../../../tests/fixtures/libtest-batch.txt");
        pub(super) const ANDROID: &str =
            include_str!("../../../tests/fixtures/libtest-android-batch.txt");
        pub(super) const INTERRUPTED: &str =
            include_str!("../../../tests/fixtures/libtest-interrupted.txt");
        pub(super) const SHOULD_PANIC: &str =
            include_str!("../../../tests/fixtures/libtest-should-panic.txt");
        pub(super) const NATIVE_NOISE: &str =
            include_str!("../../../tests/fixtures/libtest-native-noise.txt");
    }

    #[test]
    fn a_batch_that_finished_names_a_verdict_for_every_test() {
        let report = parse(ANDROID);
        assert!(report.finished);
        assert_eq!(report.in_flight, None);
        assert_eq!(report.tests.len(), 50);
        assert!(
            report
                .tests
                .values()
                .all(|outcome| outcome.verdict == Verdict::Passed)
        );
        assert_eq!(
            report.tests["decoder_seek_tests::decoder_file_reads_samples"].output,
            ""
        );
    }

    #[test]
    fn a_failure_carries_the_output_the_device_captured_for_it() {
        let report = parse(BATCH);
        assert!(report.finished);
        assert_eq!(report.tests["alpha_passes"].verdict, Verdict::Passed);
        assert_eq!(report.tests["delta_passes"].verdict, Verdict::Passed);

        let failed = &report.tests["beta_fails"];
        assert_eq!(failed.verdict, Verdict::Failed);
        assert!(
            failed
                .output
                .starts_with("captured line one\ncaptured line two\n")
        );
        assert!(failed.output.contains("beta went wrong"));
        assert!(!failed.output.contains("failures:"));
    }

    #[test]
    fn an_ignored_test_keeps_its_reason_out_of_the_verdict() {
        let report = parse(BATCH);
        assert_eq!(report.tests["gamma_is_ignored"].verdict, Verdict::Ignored);
    }

    #[test]
    fn output_that_stops_mid_run_names_the_test_in_flight() {
        let report = parse(INTERRUPTED);
        assert!(!report.finished);
        assert_eq!(report.in_flight.as_deref(), Some("bbb_slow"));
        assert_eq!(report.tests["aaa_quick"].verdict, Verdict::Passed);
        assert!(!report.tests.contains_key("bbb_slow"));
    }

    #[test]
    fn a_should_panic_test_carries_the_name_nextest_asks_for() {
        let report = parse(SHOULD_PANIC);

        assert_eq!(report.tests.len(), 21);
        assert_eq!(
            report
                .tests
                .get("a_slot_cannot_escape_into_another_region")
                .map(|outcome| outcome.verdict),
            Some(Verdict::Passed)
        );
    }

    #[test]
    fn device_output_before_a_verdict_leaves_the_verdict_readable() {
        let report = parse(NATIVE_NOISE);

        assert!(report.finished);
        assert_eq!(report.in_flight, None);
        assert_eq!(report.tests.len(), 20);
        assert!(
            report
                .tests
                .values()
                .all(|outcome| outcome.verdict == Verdict::Passed)
        );
    }
}
