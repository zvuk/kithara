use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Args;
use serde_json::Value;

use crate::{Ctx, common::project::CiReportConfig};

/// Names the producing tools own. They are contracts with those tools rather
/// than policy, so they stay here while how much of each to carry lives in
/// `.config/xtask.toml` under `[ci_report]`.
struct Consts;
impl Consts {
    const ASSESSMENT_DIRECTORY: &'static str = "quality-assessment";
    const ASSESSMENT_MANIFEST: &'static str = "manifest.json";
    const CRAP_DIRECTORY: &'static str = "cargo-crap";
    const CRAP_REPORT: &'static str = "report.md";
    const HEALTH_REPORT: &'static str = "health-report.md";
    const METRICS: &'static str = "metrics.json";
    const SIMILARITY_ARTIFACT: &'static str = "similarity-report";
    const SIMILARITY_REPORT: &'static str = "report.md";
    /// Where the health report stops being a verdict and starts being logs.
    const STAGE_DETAILS: &'static str = "## Stage details";
}

#[derive(Debug, Args)]
pub struct CiReportArgs {
    /// Directory holding the quality artifacts of one run.
    #[arg(long, value_name = "DIR")]
    pub artifacts: PathBuf,
}

pub(crate) fn run(args: &CiReportArgs, ctx: &Ctx) -> Result<()> {
    let report = render(&args.artifacts, &ctx.config.ci_report)?;
    let target = ctx.root.join("target");
    fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
    let output = target.join("consolidated-quality-report.md");
    fs::write(&output, &report).with_context(|| format!("write {}", output.display()))?;
    print!("{report}");
    Ok(())
}

/// Every producer in the run, in the order a reader wants them: the verdict
/// first, then what the verdict was drawn from.
///
/// The assessment and the duplication report were archived by the run and read
/// by nobody - the collector waited for three of the five jobs and rendered
/// what those three left. A measurement taken, uploaded and never opened is
/// the same cost as one that was never taken.
fn render(artifacts: &Path, config: &CiReportConfig) -> Result<String> {
    let mut out = String::new();
    out.push_str(&assessment(artifacts)?);
    out.push_str(&health(artifacts)?);
    out.push_str(&coverage_risk(artifacts, config.crap_rows)?);
    out.push_str(&architecture(artifacts, config.top_contours)?);
    out.push_str(&duplication(artifacts, config.similarity_rows)?);
    Ok(out)
}

/// The assessment's own headline, read from the manifest it publishes for
/// exactly this purpose. The document behind it is far too long for a step
/// summary, so this says where it is rather than carrying it.
fn assessment(artifacts: &Path) -> Result<String> {
    let Some(manifest) = find(artifacts, &|path| {
        named(path, Consts::ASSESSMENT_MANIFEST) && under(path, Consts::ASSESSMENT_DIRECTORY)
    })?
    else {
        return Ok(missing("Repository assessment", "quality-assessment"));
    };
    let text = read(&manifest)?;
    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", manifest.display()))?;
    let mut out = String::from("\n## Repository assessment\n\n");
    for (label, name) in [
        ("Verdict", "verdict"),
        ("Status", "status"),
        ("Scope", "scope"),
        ("Profile", "profile"),
        ("Depth", "depth"),
        ("Revision", "revision"),
    ] {
        let _ = writeln!(out, "- {label}: {}", field(&value, name));
    }
    out.push_str(
        "\nThe assessed document is `assessment.md` in the `quality-assessment` artifact.\n",
    );
    Ok(out)
}

fn duplication(artifacts: &Path, rows: usize) -> Result<String> {
    let (report, flattened) = if let Some(report) = find(artifacts, &|path| {
        named(path, Consts::SIMILARITY_REPORT) && under(path, Consts::SIMILARITY_ARTIFACT)
    })? {
        (report, false)
    } else {
        let Some(report) = find(artifacts, &|path| named(path, Consts::SIMILARITY_REPORT))? else {
            return Ok(missing("Duplication", "similarity-report"));
        };
        (report, true)
    };
    let text = read(&report)?;
    if flattened && !text.starts_with("# Behavioral similarity") {
        return Ok(missing("Duplication", "similarity-report"));
    }
    let mut out = String::from("\n## Duplication\n\n");
    for line in text.lines().take(rows) {
        out.push_str(line);
        out.push('\n');
    }
    if text.lines().count() > rows {
        out.push_str(
            "\nTruncated here; the whole report is in the `similarity-report` artifact.\n",
        );
    }
    Ok(out)
}

fn health(artifacts: &Path) -> Result<String> {
    let Some(report) = find(artifacts, &|path| named(path, Consts::HEALTH_REPORT))? else {
        return Ok(missing("Workspace health", "health-report"));
    };
    let text = read(&report)?;
    let summary = text
        .split_once(Consts::STAGE_DETAILS)
        .map_or(text.as_str(), |(before, _)| before);
    let (title, body) = summary.split_once('\n').unwrap_or((summary, ""));
    if !title.starts_with("# ") || !title.ends_with(" health report") {
        anyhow::bail!("{} has an unexpected title {title:?}", report.display());
    }
    Ok(format!("\n## Workspace health\n\n{}\n", body.trim()))
}

fn coverage_risk(artifacts: &Path, rows: usize) -> Result<String> {
    let Some(report) = find(artifacts, &|path| {
        named(path, Consts::CRAP_REPORT) && parent_named(path, Consts::CRAP_DIRECTORY)
    })?
    else {
        return Ok(missing("Coverage risk (CRAP)", "coverage-risk"));
    };
    let text = read(&report)?;
    let mut out = String::from("\n## Coverage risk (CRAP)\n\n");
    for line in text.lines().take(rows) {
        out.push_str(line);
        out.push('\n');
    }
    if text.lines().count() > rows {
        out.push_str("\nTruncated here; the whole table is in the `coverage-risk` artifact.\n");
    }
    Ok(out)
}

fn architecture(artifacts: &Path, top_contours: usize) -> Result<String> {
    let Some(metrics) = find(artifacts, &|path| named(path, Consts::METRICS))? else {
        return Ok(missing("Architecture complexity", "architecture"));
    };
    let text = read(&metrics)?;
    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", metrics.display()))?;
    let mut out = String::from("\n## Architecture complexity\n\n");
    let _ = writeln!(
        out,
        "- Architecture complexity index: {}",
        index(&value, "architecture_complexity_index")
    );
    let _ = writeln!(
        out,
        "- Including candidate contours: {}",
        index(&value, "including_candidates_complexity_index")
    );
    let mut contours: Vec<(&String, f64)> = value
        .get("contours")
        .and_then(Value::as_object)
        .map(|contours| {
            contours
                .iter()
                .map(|(name, contour)| (name, aci(contour)))
                .collect()
        })
        .unwrap_or_default();
    if contours.is_empty() {
        return Ok(out);
    }
    // Ranked by the metric itself, so the worst contour is the first row a
    // reader lands on; ties keep the file's own order.
    contours.sort_by(|left, right| right.1.total_cmp(&left.1));
    out.push_str("\n| Contour | ACI |\n|---|---:|\n");
    for (name, score) in contours.iter().take(top_contours) {
        let _ = writeln!(out, "| `{name}` | {score} |");
    }
    Ok(out)
}

/// A section whose input never arrived says so. Dropping it silently would
/// read as "nothing to report" from a run that reported nothing.
fn missing(section: &str, artifact: &str) -> String {
    format!("\n## {section}\n\nNo `{artifact}` artifact in this run.\n")
}

/// A manifest field as a reader sees it: a string as itself, anything else as
/// the JSON it is, and an absent one named rather than skipped.
fn field(value: &Value, name: &str) -> String {
    match value.get(name) {
        None => "unavailable".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn index(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_f64)
        .map_or_else(|| "unavailable".to_owned(), |score| score.to_string())
}

fn aci(contour: &Value) -> f64 {
    contour
        .get("architecture_complexity_index")
        .and_then(Value::as_f64)
        .unwrap_or_default()
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

fn named(path: &Path, name: &str) -> bool {
    path.file_name().is_some_and(|found| found == name)
}

/// Whether the artifact this file came from is the one named. Downloads keep
/// the run id and attempt suffix added by the uploader.
fn under(path: &Path, name: &str) -> bool {
    path.components().any(|component| {
        component.as_os_str().to_str().is_some_and(|found| {
            found == name
                || found
                    .strip_prefix(name)
                    .is_some_and(|suffix| suffix.starts_with('-'))
        })
    })
}

fn parent_named(path: &Path, name: &str) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|found| found == name)
}

/// Artifact layouts differ by how many paths their upload listed, so the
/// report locates its inputs by name rather than by a path the workflow would
/// have to keep in step.
fn find(root: &Path, accept: &impl Fn(&Path) -> bool) -> Result<Option<PathBuf>> {
    if !root.is_dir() {
        return Ok(None);
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(root)
        .with_context(|| format!("read {}", root.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("walk {}", root.display()))?;
    entries.sort();
    for entry in &entries {
        if entry.is_dir() {
            if let Some(found) = find(entry, accept)? {
                return Ok(Some(found));
            }
        } else if accept(entry) {
            return Ok(Some(entry.clone()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::common::project::ProjectConfig;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create artifact directory");
        }
        fs::write(path, contents).expect("write artifact");
    }

    #[test]
    fn run_writes_the_consolidated_report_with_numeric_crap_metrics() {
        let temp = tempdir().expect("tempdir");
        let artifacts = temp.path().join("artifacts");
        write(
            &artifacts.join("coverage-risk/cargo-crap/report.md"),
            "## 3 function(s) exceed CRAP threshold 30\n\n| | CRAP | CC | Cov % | Function | Location |\n|---|---:|---:|---:|---|---|\n| high | 35.48 | 28 | 78.79 | prepare_planned_variant_reader | prepare.rs:22 |\n",
        );
        let ctx = Ctx::new(temp.path().to_path_buf(), ProjectConfig::default());

        run(&CiReportArgs { artifacts }, &ctx).expect("write consolidated report");

        let report = fs::read_to_string(temp.path().join("target/consolidated-quality-report.md"))
            .expect("read consolidated report");
        assert!(report.contains("## Coverage risk (CRAP)"), "{report}");
        assert!(report.contains("| | CRAP | CC | Cov % |"), "{report}");
        assert!(report.contains("| high | 35.48 | 28 | 78.79 |"), "{report}");
    }

    #[test]
    fn health_section_drops_the_stage_logs() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("health-report/health-report.md"),
            "# health report\n\n## Summary\n\n| 1 | orphans | FAIL |\n\n## Stage details\n\nlog tail\n",
        );

        let report = health(temp.path()).expect("health section");

        assert!(!report.contains("log tail"));
    }

    #[test]
    fn health_section_keeps_the_stage_table() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("health-report/health-report.md"),
            "# musicbox health report\n\n## Summary\n\n| 1 | orphans | FAIL |\n\n## Stage details\n\nlog tail\n",
        );

        let report = health(temp.path()).expect("health section");

        assert!(report.contains("## Workspace health"));
        assert!(report.contains("| 1 | orphans | FAIL |"));
    }

    #[test]
    fn health_section_rejects_an_unexpected_title() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("health-report/health-report.md"),
            "# unrelated report\n",
        );

        let error = health(temp.path()).expect_err("unexpected title");

        assert!(error.to_string().contains("unexpected title"), "{error:#}");
    }

    #[test]
    fn coverage_risk_section_caps_the_table() {
        let temp = tempdir().expect("tempdir");
        let rows = (0..40).fold(String::new(), |mut table, row| {
            let _ = writeln!(table, "| row {row} |");
            table
        });
        write(
            &temp.path().join("quality-lab/rev/cargo-crap/report.md"),
            &rows,
        );

        let report = coverage_risk(temp.path(), 5).expect("coverage-risk section");

        assert!(!report.contains("| row 6 |"));
    }

    #[test]
    fn coverage_risk_section_points_at_the_artifact_when_capped() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("quality-lab/rev/cargo-crap/report.md"),
            "| a |\n| b |\n| c |\n",
        );

        let report = coverage_risk(temp.path(), 1).expect("coverage-risk section");

        assert!(report.contains("coverage-risk` artifact"));
    }

    #[test]
    fn coverage_risk_section_reads_only_the_crap_report() {
        let temp = tempdir().expect("tempdir");
        write(&temp.path().join("similarity/report.md"), "duplication\n");

        let report = coverage_risk(temp.path(), 10).expect("coverage-risk section");

        assert!(report.contains("No `coverage-risk` artifact"));
    }

    #[test]
    fn architecture_section_ranks_the_worst_contour_first() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("architecture/rev/metrics.json"),
            r#"{
                "architecture_complexity_index": 15.6,
                "including_candidates_complexity_index": 15.6,
                "contours": {
                    "crates/quiet": {"architecture_complexity_index": 1.0},
                    "crates/loud": {"architecture_complexity_index": 9.0}
                }
            }"#,
        );

        let report = architecture(temp.path(), 10).expect("architecture section");
        let rows: Vec<&str> = report
            .lines()
            .filter(|line| line.starts_with("| `crates/"))
            .collect();

        assert_eq!(rows.first().copied(), Some("| `crates/loud` | 9 |"));
    }

    #[test]
    fn architecture_section_states_the_workspace_index() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("architecture/rev/metrics.json"),
            r#"{"architecture_complexity_index": 15.6, "contours": {}}"#,
        );

        let report = architecture(temp.path(), 10).expect("architecture section");

        assert!(report.contains("- Architecture complexity index: 15.6"));
    }

    #[test]
    fn a_missing_artifact_is_stated_rather_than_dropped() {
        let temp = tempdir().expect("tempdir");

        let report = render(temp.path(), &CiReportConfig::default()).expect("report");

        assert!(report.contains("No `health-report` artifact in this run."));
    }

    /// Deriving `Default` on the config would zero both knobs, and a report
    /// that inlines nothing still looks like a report.
    #[test]
    fn the_default_configuration_carries_crap_rows() {
        assert!(CiReportConfig::default().crap_rows > 0);
    }

    #[test]
    fn the_default_configuration_carries_contours() {
        assert!(CiReportConfig::default().top_contours > 0);
    }

    #[test]
    fn the_default_configuration_carries_similarity_rows() {
        assert!(CiReportConfig::default().similarity_rows > 0);
    }

    #[test]
    fn assessment_section_states_the_verdict() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp
                .path()
                .join("quality-assessment-shallow/quality-assessment/abc123def456/standard-shallow/manifest.json"),
            r#"{"verdict": "healthy", "status": "complete", "revision": "abc1234"}"#,
        );

        let report = assessment(temp.path()).expect("assessment section");

        assert!(report.contains("Verdict: healthy"), "{report}");
        assert!(report.contains("Revision: abc1234"), "{report}");
    }

    #[test]
    fn assessment_section_names_a_field_the_manifest_omits() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp
                .path()
                .join("quality-assessment-deep/quality-assessment/abc123def456/complete-deep/manifest.json"),
            r#"{"verdict": "healthy"}"#,
        );

        let report = assessment(temp.path()).expect("assessment section");

        assert!(report.contains("Depth: unavailable"), "{report}");
    }

    #[test]
    fn duplication_section_reads_the_report_under_its_revision() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("similarity-report-42-1/abc1234/report.md"),
            "# duplication\n\n| pair | score |\n",
        );

        let report = duplication(temp.path(), 10).expect("duplication section");

        assert!(report.contains("| pair | score |"), "{report}");
    }

    #[test]
    fn duplication_section_reads_a_flattened_single_artifact() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("abc1234/report.md"),
            "# Behavioral similarity\n\n- Candidates: 42\n",
        );

        let report = duplication(temp.path(), 10).expect("duplication section");

        assert!(report.contains("Candidates: 42"), "{report}");
    }

    #[test]
    fn duplication_section_does_not_read_the_crap_report() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("coverage-risk/cargo-crap/report.md"),
            "coverage risk\n",
        );

        let report = duplication(temp.path(), 10).expect("duplication section");

        assert!(
            report.contains("No `similarity-report` artifact"),
            "{report}"
        );
    }

    #[test]
    fn duplication_section_points_at_the_artifact_when_capped() {
        let temp = tempdir().expect("tempdir");
        write(
            &temp.path().join("similarity-report/abc1234/report.md"),
            "one\ntwo\nthree\n",
        );

        let report = duplication(temp.path(), 2).expect("duplication section");

        assert!(report.contains("Truncated here"), "{report}");
    }
}
