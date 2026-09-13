use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use kithara_devtools::junit::parse_junit;
use roxmltree::Document;

pub(super) fn collect(results: &Path, report: &Path) -> Result<Vec<String>> {
    let pattern = format!(
        "{}/**/*.xml",
        glob::Pattern::escape(&results.to_string_lossy())
    );
    let mut files = glob::glob(&pattern)?.collect::<std::result::Result<Vec<_>, _>>()?;
    files.sort();
    if files.is_empty() {
        bail!(
            "Android instrumentation wrote no JUnit under {}",
            results.display()
        );
    }
    merge(&files, report)?;
    validate(&fs::read_to_string(report)?)
}

/// Preserve each runner's suites in one CI report.
pub(super) fn merge(files: &[PathBuf], report: &Path) -> Result<()> {
    let mut combined = String::from("<testsuites>\n");
    for file in files {
        let xml =
            fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let doc = Document::parse(&xml).with_context(|| format!("parsing {}", file.display()))?;
        for suite in doc.descendants().filter(|node| {
            node.has_tag_name("testsuite")
                && !node
                    .ancestors()
                    .skip(1)
                    .any(|parent| parent.has_tag_name("testsuite"))
        }) {
            combined.push_str(&xml[suite.range()]);
            combined.push('\n');
        }
    }
    combined.push_str("</testsuites>\n");
    fs::write(report, &combined).with_context(|| format!("writing {}", report.display()))?;
    Ok(())
}

fn validate(xml: &str) -> Result<Vec<String>> {
    let doc = Document::parse(xml).context("parsing Android JUnit")?;
    if doc.descendants().any(|node| node.has_tag_name("skipped")) {
        bail!("Android instrumentation skipped a test");
    }
    let cases = parse_junit(xml)?;
    if cases.is_empty() {
        bail!("Android instrumentation ran no tests");
    }
    let mut actual = BTreeSet::new();
    for case in cases {
        let name = format!("{}#{}", case.suite, case.name);
        if case.failing() {
            bail!("Android instrumentation case `{name}` failed or passed only after retry");
        }
        if !actual.insert(name.clone()) {
            bail!("Android instrumentation reported duplicate case `{name}`");
        }
    }
    Ok(actual.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RENDER: &str = "com.kithara.OfflineCaptureTest#rendersCleanWav";

    fn report(body: &str) -> String {
        format!("<testsuite>{body}</testsuite>")
    }

    fn render(outcome: &str) -> String {
        format!(
            "<testcase classname=\"com.kithara.OfflineCaptureTest\" name=\"rendersCleanWav\" time=\"1\">{outcome}</testcase>"
        )
    }

    #[test]
    fn instrumentation_requires_reported_cases_to_pass() {
        assert_eq!(validate(&report(&render(""))).unwrap(), [RENDER]);
        for outcome in ["<skipped/>", "<failure/>", "<error/>", "<flakyFailure/>"] {
            assert!(validate(&report(&render(outcome))).is_err(), "{outcome}");
        }
        assert!(validate(&report("")).is_err());
        let unrelated =
            "<testcase classname=\"com.kithara.PlayerTest\" name=\"createsPlayer\" time=\"1\"/>";
        assert_eq!(validate(&report(unrelated)).unwrap().len(), 1);
        assert_eq!(
            validate(&report(&(render("") + unrelated))).unwrap().len(),
            2
        );
    }

    #[test]
    fn instrumentation_rejects_empty_or_duplicate_execution() {
        assert!(validate(&report("")).is_err());
        assert!(validate(&report(&(render("") + &render("")))).is_err());
    }

    #[test]
    fn collection_cannot_use_a_previous_runs_report() {
        let root = tempfile::tempdir().unwrap();
        let previous = root.path().join("previous");
        let current = root.path().join("current");
        fs::create_dir_all(&previous).unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(previous.join("TEST.xml"), report(&render(""))).unwrap();
        let output = root.path().join("junit.xml");
        assert!(collect(&current, &output).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn failed_current_results_are_preserved_for_the_ci_verdict() {
        let root = tempfile::tempdir().unwrap();
        let results = root.path().join("results");
        fs::create_dir_all(&results).unwrap();
        fs::write(results.join("TEST.xml"), report(&render("<failure/>"))).unwrap();
        let output = root.path().join("junit.xml");
        assert!(collect(&results, &output).is_err());
        let cases = parse_junit(&fs::read_to_string(output).unwrap()).unwrap();
        assert_eq!(cases.len(), 1);
        assert!(cases[0].failed);
    }
}
