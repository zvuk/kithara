//! Reading a `JUnit` report for performance and stress evidence consumers.

use anyhow::{Context, Result, bail};

const MAX_JUNIT_CASES: usize = 750_000;
pub(crate) const MAX_CASE_OUTPUT_BYTES: usize = 8 * 1_024 * 1_024;

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct CaseTiming {
    pub iteration: Option<usize>,
    pub timestamp: Option<String>,
    pub name: String,
    pub output: String,
    pub suite: String,
    pub failed: bool,
    /// The case failed an attempt and passed a later one. It is not a failure
    /// and it is not a clean pass: a report that counts only failures reads a
    /// retried run as green.
    pub flaky: bool,
    /// The retained output hit the per-case budget and lost its tail. The case
    /// itself stays valid evidence; reports must name the truncation.
    pub output_truncated: bool,
    pub secs: f64,
}

impl CaseTiming {
    /// The case carries a failure payload in [`CaseTiming::output`]: it either
    /// failed outright, or it failed an attempt the runner retried into a
    /// pass. Both describe a defect that reproduced, so a pass that reads the
    /// output for a symptom, a wait graph or a dump envelope must take both.
    #[must_use]
    pub fn failing(&self) -> bool {
        self.failed || self.flaky
    }
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub(crate) struct JunitReport {
    pub(crate) run_id: Option<String>,
    pub(crate) timestamp: Option<String>,
    pub(crate) cases: Vec<CaseTiming>,
}

/// # Errors
///
/// Fails when the document is not `XML`, a `testcase` identity is empty, a
/// stress suite does not own its testcase, or a `time` attribute is missing,
/// negative, non-finite, or not a number. A skipped stress testcase is also
/// rejected because it is not per-iteration execution evidence. The testcase
/// limit rejects the artifact; an oversized retained output only truncates its
/// own case and marks it [`CaseTiming::output_truncated`]. A case that passed
/// on a retry is not a failure and is marked [`CaseTiming::flaky`].
pub fn parse_junit(xml: &str) -> Result<Vec<CaseTiming>> {
    parse_junit_report(xml).map(|report| report.cases)
}

/// Reads run-level identity and testcase evidence from nextest `JUnit`.
///
/// # Errors
///
/// Fails under the same conditions as [`parse_junit`].
pub(crate) fn parse_junit_report(xml: &str) -> Result<JunitReport> {
    let doc = roxmltree::Document::parse(xml).context("parse junit xml")?;
    let root = doc
        .descendants()
        .find(|node| node.has_tag_name("testsuites"));
    let run_id = root
        .and_then(|node| node.attribute("uuid"))
        .map(str::to_owned);
    let timestamp = root
        .and_then(|node| node.attribute("timestamp"))
        .map(str::to_owned);
    let mut cases = Vec::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("testcase")) {
        validate_case_count(cases.len().saturating_add(1))?;
        let name = node.attribute("name").unwrap_or_default().to_owned();
        let suite = node.attribute("classname").unwrap_or_default().to_owned();
        if name.trim().is_empty() {
            bail!("testcase name is empty");
        }
        if suite.trim().is_empty() {
            bail!("testcase classname is empty");
        }
        let parent_suite = node
            .ancestors()
            .find(|ancestor| ancestor.has_tag_name("testsuite"))
            .and_then(|parent| parent.attribute("name"));
        let iteration = parent_suite
            .and_then(stress_suite)
            .map(|(base, iteration)| {
                if base != suite.as_str() {
                    bail!("stress testsuite base does not match testcase classname");
                }
                Ok(iteration)
            })
            .transpose()?;
        let secs: f64 = node
            .attribute("time")
            .context("testcase time attribute is missing")?
            .parse()
            .with_context(|| format!("bad time attribute on {suite} {name}"))?;
        if !secs.is_finite() || secs < 0.0 {
            bail!("invalid time attribute on {suite} {name}");
        }
        let stress = iteration.is_some();
        if stress && node.children().any(|child| child.has_tag_name("skipped")) {
            bail!("selected testcase {suite} {name} was skipped");
        }
        let failed = node
            .children()
            .any(|c| c.has_tag_name("failure") || c.has_tag_name("error"));
        let flaky = !failed
            && node
                .children()
                .any(|child| child.has_tag_name("flakyFailure"));
        let timestamp = node.attribute("timestamp").map(str::to_owned);
        let (output, output_truncated) = if flaky {
            retried_failure_output(node)
        } else {
            failure_output(node)
        };
        cases.push(CaseTiming {
            iteration,
            timestamp,
            name,
            output,
            suite,
            failed,
            flaky,
            output_truncated,
            secs,
        });
    }
    Ok(JunitReport {
        run_id,
        timestamp,
        cases,
    })
}

fn validate_case_count(count: usize) -> Result<()> {
    if count > MAX_JUNIT_CASES {
        bail!("JUnit exceeds the deterministic limit of {MAX_JUNIT_CASES} testcases");
    }
    Ok(())
}

fn failure_output(node: roxmltree::Node<'_, '_>) -> (String, bool) {
    let mut output = String::new();
    let mut truncated = false;
    for child in node
        .children()
        .filter(|child| child.has_tag_name("failure") || child.has_tag_name("error"))
    {
        truncated |= append_failure_description(&mut output, child);
    }
    truncated |= append_streams(&mut output, node);
    (output, truncated)
}

/// The failing attempt of a case the runner retried into a pass.
///
/// That attempt is described entirely inside `flakyFailure`, streams included.
/// The `testcase` keeps its own `system-out` and `system-err`, but they belong
/// to the attempt that passed, so reading the case the ordinary way returns
/// the green run and drops the red one it is evidence of.
fn retried_failure_output(node: roxmltree::Node<'_, '_>) -> (String, bool) {
    let mut output = String::new();
    let mut truncated = false;
    for child in node
        .children()
        .filter(|child| child.has_tag_name("flakyFailure"))
    {
        truncated |= append_failure_description(&mut output, child);
        truncated |= append_streams(&mut output, child);
    }
    (output, truncated)
}

fn append_streams(output: &mut String, node: roxmltree::Node<'_, '_>) -> bool {
    let mut truncated = false;
    for text in node
        .children()
        .filter(|child| child.has_tag_name("system-out") || child.has_tag_name("system-err"))
        .filter_map(|child| child.text())
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        truncated |= append_output(output, "\n", text);
    }
    truncated
}

fn append_failure_description(output: &mut String, node: roxmltree::Node<'_, '_>) -> bool {
    let kind = node
        .attribute("type")
        .unwrap_or_else(|| node.tag_name().name());
    let message = node.attribute("message").unwrap_or_default().trim();
    let body = node.text().unwrap_or_default().trim();
    // A Rust panic puts the same header in both: nextest lifts the first line
    // of the body into `message`. Keeping both spent the retained output — and
    // every signature derived from it — on saying the header twice, which
    // pushed the assertion's own values past the width a report row has.
    let message = if body.starts_with(message) {
        ""
    } else {
        message
    };
    let mut first = true;
    let mut truncated = false;
    for part in [kind, message, body]
        .into_iter()
        .filter(|part| !part.is_empty())
    {
        let separator = if first {
            first = false;
            "\n"
        } else {
            ": "
        };
        truncated |= append_output(output, separator, part);
    }
    truncated
}

/// Appends within the per-case budget and drops the tail beyond it, reporting
/// whether anything was dropped. One case's runaway output must not invalidate
/// the artifact: the head — where the assertion text lives — stays evidence,
/// and the whole lane keeps its other cases.
fn append_output(output: &mut String, separator: &str, text: &str) -> bool {
    let separator = if output.is_empty() { "" } else { separator };
    let budget = MAX_CASE_OUTPUT_BYTES.saturating_sub(output.len());
    if budget < separator.len() {
        return true;
    }
    output.push_str(separator);
    let budget = budget - separator.len();
    if text.len() <= budget {
        output.push_str(text);
        return false;
    }
    let mut cut = budget;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    output.push_str(&text[..cut]);
    true
}

fn stress_suite(suite: &str) -> Option<(&str, usize)> {
    let (base, iteration) = suite.rsplit_once("@stress-")?;
    let iteration = iteration.parse().ok()?;
    Some((base, iteration))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="2" failures="1">
  <testsuite name="demo-tests::suite_light" tests="2" failures="1">
    <testcase name="offline::gapless" classname="demo-tests::suite_light" time="1.532"/>
    <testcase name="offline::seek" classname="demo-tests::suite_light" time="0.201">
      <failure type="test failure">boom</failure>
    </testcase>
  </testsuite>
</testsuites>"#;

    /// What nextest writes for a test that failed an attempt and passed a later
    /// one: the failing attempt is described inside `flakyFailure`, streams
    /// included, while the `testcase` keeps the streams of the attempt that
    /// passed.
    const RETRIED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="1" failures="0">
  <testsuite name="demo-tests::suite_stress" tests="1" failures="0">
    <testcase name="abr::switch" classname="demo-tests::suite_stress" time="2.100">
      <flakyFailure type="test failure" message="boom">panicked at abr.rs:7
        <system-out>red stdout</system-out>
        <system-err>red stderr</system-err>
      </flakyFailure>
      <system-out>green stdout</system-out>
      <system-err></system-err>
    </testcase>
  </testsuite>
</testsuites>"#;

    const STRESS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="1" failures="1">
  <testsuite name="demo-tests::suite_stress@stress-7" tests="1" failures="1">
    <testcase name="offline::seek" classname="demo-tests::suite_stress" time="0.201" timestamp="2026-08-13T12:34:56.789Z">
      <failure type="test failure">boom</failure>
    </testcase>
  </testsuite>
</testsuites>"#;

    /// What nextest writes for a failed `assert_eq!`: the panic header is
    /// lifted into `message` and the body repeats it verbatim.
    const PANIC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="1" failures="1">
  <testsuite name="demo-tests::suite_light" tests="1" failures="1">
    <testcase name="audio::warms_pool" classname="demo-tests::suite_light" time="0.536">
      <failure message="thread 'audio::warms_pool' (971370) panicked at tests/demo.rs:166:5" type="test failure with exit code 101">thread 'audio::warms_pool' (971370) panicked at tests/demo.rs:166:5:
assertion `left == right` failed: a warmed pool must serve decode-sized buffers without allocating
  left: 0
 right: 1
stack backtrace:
   0: __rustc::rust_begin_unwind</failure>
    </testcase>
  </testsuite>
</testsuites>"#;

    /// The header is worth keeping once. Kept twice it consumed the retained
    /// output a report row can show, and what fell off the end was the only
    /// part that says which defect this is: the assertion's own values.
    #[test]
    fn a_panic_header_lifted_into_the_message_is_not_kept_twice() {
        let cases = parse_junit(PANIC).expect("parse junit");

        let output = &cases[0].output;
        assert_eq!(output.matches("panicked at").count(), 1, "{output}");
        assert!(output.contains("left: 0"), "{output}");
        assert!(output.contains("right: 1"), "{output}");
    }

    /// A message the body does NOT repeat still carries information, and
    /// dropping it would lose the only description such a failure has.
    #[test]
    fn a_message_the_body_does_not_repeat_is_kept() {
        let cases = parse_junit(&PANIC.replace(
            "message=\"thread 'audio::warms_pool' (971370) panicked at tests/demo.rs:166:5\"",
            "message=\"the runner killed it\"",
        ))
        .expect("parse junit");

        assert!(
            cases[0].output.contains("the runner killed it"),
            "{}",
            cases[0].output
        );
    }

    /// Retries buy nothing if a passed-on-retry case still reads as failed.
    #[test]
    fn a_test_that_passed_on_a_retry_is_not_a_failure() {
        let cases = parse_junit(RETRIED).expect("parse junit");

        assert_eq!(cases.len(), 1);
        assert!(!cases[0].failed);
    }

    /// A retried pass is the one outcome a failure count cannot see, and it is
    /// the outcome a stress run is looking for.
    #[test]
    fn a_test_that_passed_on_a_retry_is_recorded_as_flaky() {
        let cases = parse_junit(RETRIED).expect("parse junit");

        assert_eq!(cases.len(), 1);
        assert!(cases[0].flaky);
    }

    /// The retry is the only run a report can describe; without its streams a
    /// flaky row names a test and says nothing about why it broke.
    #[test]
    fn a_retried_pass_keeps_the_failing_attempts_streams() {
        let cases = parse_junit(RETRIED).expect("parse junit");

        assert_eq!(cases.len(), 1);
        assert!(
            cases[0].output.contains("red stdout"),
            "output must carry the failing attempt: {}",
            cases[0].output
        );
        assert!(
            cases[0].output.contains("red stderr"),
            "output must carry the failing attempt: {}",
            cases[0].output
        );
    }

    /// The `testcase` streams belong to the attempt that passed. Retaining them
    /// as the case's output describes the green run under a red heading.
    #[test]
    fn a_retried_pass_drops_the_passing_attempts_streams() {
        let cases = parse_junit(RETRIED).expect("parse junit");

        assert_eq!(cases.len(), 1);
        assert!(
            !cases[0].output.contains("green stdout"),
            "output must not carry the passing attempt: {}",
            cases[0].output
        );
    }

    #[test]
    fn a_case_that_passed_on_a_retry_is_failing() {
        let cases = parse_junit(RETRIED).expect("parse junit");

        assert_eq!(cases.len(), 1);
        assert!(cases[0].failing());
    }

    #[test]
    fn a_failed_case_is_failing() {
        let cases = parse_junit(XML).expect("parse junit");

        let failed = cases.iter().find(|case| case.failed).expect("failed case");
        assert!(failed.failing());
    }

    #[test]
    fn a_clean_pass_is_not_failing() {
        let cases = parse_junit(XML).expect("parse junit");

        let passed = cases.iter().find(|case| !case.failed).expect("passed case");
        assert!(!passed.failing());
    }

    #[test]
    fn a_failed_case_is_not_also_flaky() {
        let cases = parse_junit(XML).expect("parse junit");

        let failed = cases.iter().find(|case| case.failed).expect("failed case");
        assert!(!failed.flaky);
    }

    #[test]
    fn parses_cases_and_failures() {
        let cases = parse_junit(XML).expect("parse junit");

        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].suite, "demo-tests::suite_light");
        assert_eq!(cases[0].name, "offline::gapless");
        assert_eq!(cases[0].iteration, None);
        assert!((cases[0].secs - 1.532).abs() < 1e-9);
        assert!(!cases[0].failed);
        assert!(cases[1].failed);
        assert_eq!(cases[1].output, "test failure: boom");
    }

    #[test]
    fn retains_the_zero_based_stress_iteration() {
        let report = parse_junit_report(STRESS).expect("parse stress junit");
        let cases = report.cases;

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].iteration, Some(7));
        assert!(cases[0].failed);
        assert_eq!(
            cases[0].timestamp.as_deref(),
            Some("2026-08-13T12:34:56.789Z")
        );
    }

    #[test]
    fn retains_run_identity_when_nextest_provides_it() {
        let xml = r#"<testsuites uuid="run-id" timestamp="2026-08-13T12:34:56+00:00">
  <testsuite name="demo@stress-0">
    <testcase name="seek" classname="demo" time="0.1"/>
  </testsuite>
</testsuites>"#;

        let report = parse_junit_report(xml).expect("parse report identity");

        assert_eq!(report.run_id.as_deref(), Some("run-id"));
        assert_eq!(
            report.timestamp.as_deref(),
            Some("2026-08-13T12:34:56+00:00")
        );
    }

    #[test]
    fn rejects_empty_testcase_identity() {
        for xml in [
            r#"<testsuite name="demo@stress-0"><testcase classname="demo"/></testsuite>"#,
            r#"<testsuite name="demo@stress-0"><testcase name="seek"/></testsuite>"#,
        ] {
            let error = parse_junit(xml).expect_err("empty identity must be rejected");

            assert!(error.to_string().contains("is empty"), "{error:?}");
        }
    }

    #[test]
    fn rejects_mismatched_stress_suite_identity() {
        let xml = r#"<testsuite name="other@stress-0">
  <testcase name="seek" classname="demo"/>
</testsuite>"#;

        let error = parse_junit(xml).expect_err("suite mismatch must be rejected");

        assert!(
            error
                .to_string()
                .contains("testsuite base does not match testcase classname"),
            "{error:?}"
        );
    }

    #[test]
    fn rejects_missing_or_invalid_timing() {
        for time in [None, Some("-1"), Some("NaN"), Some("inf"), Some("bad")] {
            let attribute = time.map_or_else(String::new, |time| format!(r#" time="{time}""#));
            let xml = format!(
                r#"<testsuite name="demo@stress-0"><testcase name="seek" classname="demo"{attribute}/></testsuite>"#
            );

            let error = parse_junit(&xml).expect_err("invalid timing must be rejected");

            assert!(error.to_string().contains("time attribute"), "{error:?}");
        }
    }

    #[test]
    fn rejects_a_selected_testcase_that_was_skipped() {
        let xml = r#"<testsuite name="demo@stress-0">
  <testcase name="seek" classname="demo" time="0.1"><skipped/></testcase>
</testsuite>"#;

        let error = parse_junit(xml).expect_err("skipped evidence is incomplete");

        assert!(error.to_string().contains("was skipped"), "{error:?}");
    }

    #[test]
    fn keeps_failure_kind_message_and_captured_streams() {
        let xml = r#"<testsuite name="demo@stress-0">
  <testcase name="seek" classname="demo" time="0.1">
    <failure type="test timeout" message="after 120s">watchdog</failure>
    <system-err>stack backtrace</system-err>
  </testcase>
</testsuite>"#;

        let cases = parse_junit(xml).expect("parse failed case");

        assert_eq!(
            cases[0].output,
            "test timeout: after 120s: watchdog\nstack backtrace"
        );
    }

    #[test]
    fn testcase_limit_is_inclusive() {
        validate_case_count(MAX_JUNIT_CASES).expect("case limit is inclusive");
        assert!(validate_case_count(MAX_JUNIT_CASES + 1).is_err());
    }

    /// One case's runaway output loses its tail, never the artifact: the head
    /// keeps the assertion text and the case reports the truncation.
    #[test]
    fn oversized_retained_output_is_truncated_not_rejected() {
        let mut output = String::from("assert head");
        let head = output.len();

        assert!(append_output(
            &mut output,
            "\n",
            &"\u{1f980}".repeat(MAX_CASE_OUTPUT_BYTES)
        ));
        assert!(output.len() <= MAX_CASE_OUTPUT_BYTES);
        assert!(output.is_char_boundary(output.len()), "cut on a boundary");
        assert!(output.starts_with("assert head\n"), "head survives");
        assert!(output.len() > head, "budget is spent, not abandoned");
        assert!(!append_output(&mut String::new(), "\n", "small"));
    }

    #[test]
    fn a_truncated_case_still_parses_and_is_named() {
        let oversized = "y".repeat(MAX_CASE_OUTPUT_BYTES);
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="1" failures="1">
  <testsuite name="demo-tests::suite_light" tests="1" failures="1">
    <testcase name="offline::seek" classname="demo-tests::suite_light" time="0.2">
      <failure type="test failure">boom</failure>
      <system-out>{oversized}</system-out>
    </testcase>
  </testsuite>
</testsuites>"#
        );

        let cases = parse_junit(&xml).expect("one noisy case must not erase the artifact");

        assert_eq!(cases.len(), 1);
        assert!(cases[0].output_truncated);
        assert!(cases[0].output.starts_with("test failure"), "head survives");
        assert!(cases[0].output.len() <= MAX_CASE_OUTPUT_BYTES);
    }
}
