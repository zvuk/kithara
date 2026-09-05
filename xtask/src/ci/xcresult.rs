use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

use anyhow::{Context, Result};
use serde_json::Value;

use super::process::Process;

/// Turn the result bundle `xcodebuild` leaves behind into a `JUnit` report.
///
/// Xcode writes no `JUnit` of its own, so without this the simulator lane — the
/// one carrying every iOS regression — contributes nothing to a merge request's
/// test report, and a reviewer sees a green job with no tests behind it.
pub(crate) fn write_junit(
    process: &Process,
    program: &str,
    bundle: &Path,
    output: &Path,
) -> Result<()> {
    let path = bundle.to_string_lossy().into_owned();
    let json = process.capture(
        program,
        &[
            "xcresulttool",
            "get",
            "test-results",
            "tests",
            "--path",
            &path,
            "--format",
            "json",
        ],
        "reading the Xcode result bundle",
    )?;
    let parsed: Value =
        serde_json::from_str(&json).with_context(|| format!("parsing test results from {path}"))?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    fs::write(output, junit(&parsed)?).with_context(|| format!("writing {}", output.display()))
}

#[derive(Default)]
struct Case {
    name: String,
    seconds: f64,
    failures: Vec<String>,
    skipped: bool,
}

fn junit(results: &Value) -> Result<String> {
    let nodes = results["testNodes"]
        .as_array()
        .context("xcresulttool test results have no testNodes array")?;
    let mut suites: BTreeMap<String, Vec<Case>> = BTreeMap::new();
    for node in nodes {
        collect(node, "xcodebuild", &mut suites);
    }

    let total: usize = suites.values().map(Vec::len).sum();
    let failed: usize = suites
        .values()
        .flatten()
        .filter(|case| !case.failures.is_empty())
        .count();
    let mut report = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    writeln!(
        report,
        "<testsuites tests=\"{total}\" failures=\"{failed}\">"
    )?;
    for (suite, cases) in &suites {
        let suite_failed = cases
            .iter()
            .filter(|case| !case.failures.is_empty())
            .count();
        writeln!(
            report,
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{suite_failed}\">",
            escape(suite),
            cases.len()
        )?;
        for case in cases {
            writeln!(
                report,
                "    <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\">",
                escape(suite),
                escape(&case.name),
                case.seconds
            )?;
            if case.skipped {
                report.push_str("      <skipped/>\n");
            }
            for failure in &case.failures {
                writeln!(report, "      <failure message=\"{}\"/>", escape(failure))?;
            }
            report.push_str("    </testcase>\n");
        }
        report.push_str("  </testsuite>\n");
    }
    report.push_str("</testsuites>\n");
    Ok(report)
}

/// `suite` carries the closest enclosing suite or bundle name, which is what a
/// `JUnit` reader shows as the class a test belongs to.
fn collect(node: &Value, suite: &str, suites: &mut BTreeMap<String, Vec<Case>>) {
    let name = node["name"].as_str().unwrap_or_default();
    match node["nodeType"].as_str().unwrap_or_default() {
        "Test Case" => {
            let (suite, case) = split_identifier(node, suite, name);
            suites.entry(suite).or_default().push(Case {
                name: case,
                seconds: node["durationInSeconds"].as_f64().unwrap_or_default(),
                failures: failure_messages(node),
                skipped: node["result"].as_str() == Some("Skipped"),
            });
        }
        "Test Suite" | "Unit test bundle" | "UI test bundle" => {
            for child in children(node) {
                collect(child, name, suites);
            }
        }
        _ => {
            for child in children(node) {
                collect(child, suite, suites);
            }
        }
    }
}

/// A case identifies itself as `Suite/testName()`, which names the type that
/// holds the test rather than the display name of the suite it was grouped
/// under. Prefer it, and fall back to the enclosing node when it is absent.
fn split_identifier(node: &Value, suite: &str, name: &str) -> (String, String) {
    node["nodeIdentifier"]
        .as_str()
        .and_then(|id| id.split_once('/'))
        .map_or_else(
            || (suite.to_owned(), name.to_owned()),
            |(class, case)| (class.to_owned(), case.to_owned()),
        )
}

/// Only a `Failure Message` fails a case. A passing test can carry a
/// `Runtime Warning` child, and treating every child as a failure would report
/// green runs as red.
fn failure_messages(node: &Value) -> Vec<String> {
    children(node)
        .filter(|child| child["nodeType"].as_str() == Some("Failure Message"))
        .filter_map(|child| child["name"].as_str().map(str::to_owned))
        .collect()
}

fn children(node: &Value) -> impl Iterator<Item = &Value> {
    node["children"].as_array().into_iter().flatten()
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xcresult-tests.json");
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn every_case_reaches_the_report() {
        let report = junit(&fixture()).unwrap();
        assert!(report.contains("<testsuites tests=\"3\" failures=\"1\""));
        assert_eq!(report.matches("<testcase ").count(), 3);
    }

    #[test]
    fn a_case_is_classed_by_the_type_that_holds_it() {
        let report = junit(&fixture()).unwrap();
        assert!(report.contains("classname=\"LabaIOSTraps\""));
        assert!(report.contains("name=\"laba419OfflineResume()\""));
    }

    #[test]
    fn a_runtime_warning_on_a_passing_test_is_not_a_failure() {
        let report = junit(&fixture()).unwrap();
        assert!(
            !report.contains("priority inversions"),
            "a Runtime Warning child must not be reported as a failure"
        );
    }

    #[test]
    fn a_failure_message_reaches_the_report_escaped() {
        let report = junit(&fixture()).unwrap();
        assert!(report.contains("<failure message=\"Laba419OfflineResume.swift:68:"));
        assert!(!report.contains("<failure message=\"\"/>"));
    }
}
