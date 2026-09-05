use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use clap::Args;
use serde::Deserialize;

use super::{
    AssessmentDepth, AssessmentProfile, adapters, artifact, collect,
    model::{Assessment, CoverageStatus, StageStatus},
};
use crate::{Ctx, common::project::QualityRenderBudgets, quality_lab::CRAP_THRESHOLD};

#[derive(Deserialize)]
struct CrapReport {
    entries: Vec<CrapEntry>,
}

#[derive(Deserialize)]
struct CrapEntry {
    coverage: Option<f64>,
    file: String,
    function: String,
    crap: f64,
    cyclomatic: f64,
    line: u64,
}

/// Arguments for rendering an existing quality assessment.
#[derive(Debug, Args)]
pub struct SummaryArgs {
    /// Select the standard or deep assessment artifact.
    #[arg(long, value_enum, default_value_t)]
    depth: AssessmentDepth,

    /// Select the product or complete assessment artifact.
    #[arg(long, value_enum, default_value_t)]
    profile: AssessmentProfile,

    /// Read the assessment in this directory.
    #[arg(long)]
    directory: Option<PathBuf>,
}

pub(crate) fn run(args: &SummaryArgs, ctx: &Ctx) -> Result<()> {
    let directory = args.directory.clone().unwrap_or_else(|| {
        let revision = collect::revision(&ctx.root);
        collect::output_directory(&ctx.root, &revision.directory, args.profile, args.depth)
    });
    let crap_report = adapters::git_revision(&ctx.root, false).map(|revision| {
        ctx.root
            .join("target/quality-lab")
            .join(revision)
            .join("cargo-crap/report.json")
    });
    let output = render(
        &directory,
        crap_report.as_deref(),
        &ctx.config.quality.render,
    )?;
    std::io::stdout().lock().write_all(output.as_bytes())?;
    Ok(())
}

fn render(
    directory: &Path,
    crap_report: Option<&Path>,
    budgets: &QualityRenderBudgets,
) -> Result<String> {
    let assessment = artifact::read(directory)?;
    let mut output = header(&assessment);
    append_crap(&mut output, &assessment, crap_report, budgets)?;
    append_architecture(&mut output, &assessment, budgets)?;
    append_evidence_gaps(&mut output, &assessment, budgets)?;
    output.push_str(
        "Full details are in the uploaded quality assessment artifact (`assessment.md` and `assessment.json`).\n",
    );
    Ok(output)
}

fn header(assessment: &Assessment) -> String {
    format!(
        "# Quality assessment summary\n\n\
         Scope `{}`; revision `{}`; profile `{}`; depth `{}`; analysis status **{}**; verdict **{}**.\n\n\
         ## Decision\n\n\
         | Metric | Value |\n\
         | --- | ---: |\n\
         | Debt units / threshold | {} / {} |\n\
         | Baseline entries | {} |\n\
         | Findings | {} |\n\
         | Signals | {} |\n\
         | Evidence gaps | {} |\n\
         | Scope LOC | {} |\n\n",
        artifact::escape(&assessment.scope.name),
        artifact::escape(&assessment.revision),
        assessment.profile,
        assessment.depth,
        assessment.status.as_str(),
        assessment.verdict.as_str(),
        assessment.summary.debt_units,
        assessment.summary.debt_threshold,
        assessment.summary.baseline_entries,
        assessment.summary.findings,
        assessment.summary.signals,
        assessment.summary.evidence_gaps,
        assessment.scope.loc,
    )
}

fn append_crap(
    output: &mut String,
    assessment: &Assessment,
    report_path: Option<&Path>,
    budgets: &QualityRenderBudgets,
) -> Result<()> {
    output.push_str("## Coverage risk (CRAP)\n\n");
    let Some(report_path) = report_path.filter(|path| path.is_file()) else {
        let coverage = assessment
            .tool_coverage
            .iter()
            .find(|coverage| coverage.tool == "cargo-crap")
            .ok_or_else(|| {
                anyhow!(
                    "quality assessment omits cargo-crap tool coverage: {}",
                    assessment.revision
                )
            })?;
        writeln!(
            output,
            "- cargo-crap: status `{}`; note: {}; produce it with `just quality assess --depth deep`.\n",
            coverage.status.as_str(),
            artifact::escape(&coverage.note),
        )?;
        return Ok(());
    };
    let text = fs::read_to_string(report_path)
        .with_context(|| format!("read cargo-crap report: {}", report_path.display()))?;
    let report: CrapReport = serde_json::from_str(&text)
        .with_context(|| format!("parse cargo-crap report: {}", report_path.display()))?;
    let mut entries = report.entries.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .crap
            .total_cmp(&left.crap)
            .then_with(|| left.function.cmp(&right.function))
            .then_with(|| left.file.cmp(&right.file))
            .then_with(|| left.line.cmp(&right.line))
    });
    let above_threshold = entries
        .iter()
        .filter(|entry| entry.crap > CRAP_THRESHOLD)
        .count();
    writeln!(output, "- Functions analysed: {}", entries.len())?;
    writeln!(
        output,
        "- Above threshold (> {CRAP_THRESHOLD:.1}): {above_threshold}"
    )?;
    if let Some(worst) = entries.first() {
        writeln!(output, "- Worst CRAP score: {:.2}\n", worst.crap)?;
        output.push_str(
            "| Function | Location | Cyclomatic | Coverage | CRAP |\n\
             | --- | --- | ---: | ---: | ---: |\n",
        );
        for entry in entries.iter().take(budgets.summary_rows) {
            let location = format!("{}:{}", entry.file, entry.line);
            let coverage = entry
                .coverage
                .map_or_else(|| "n/a".to_owned(), |value| format!("{value:.2}"));
            writeln!(
                output,
                "| {} | {} | {:.2} | {} | {:.2} |",
                artifact::escape(&entry.function),
                artifact::escape(&location),
                entry.cyclomatic,
                coverage,
                entry.crap,
            )?;
        }
        append_omitted(output, entries.len(), "CRAP", budgets)?;
    } else {
        output.push_str("- Worst CRAP score: n/a\n\n_No function entries were reported._\n\n");
    }
    Ok(())
}

fn append_omitted(
    output: &mut String,
    rows: usize,
    label: &str,
    budgets: &QualityRenderBudgets,
) -> Result<()> {
    let omitted = rows.saturating_sub(budgets.summary_rows);
    if omitted == 0 {
        output.push('\n');
    } else {
        writeln!(output, "\n_{omitted} additional {label} rows omitted._\n")?;
    }
    Ok(())
}

fn append_architecture(
    output: &mut String,
    assessment: &Assessment,
    budgets: &QualityRenderBudgets,
) -> Result<()> {
    output.push_str("## Architecture complexity\n\n");
    let mut signals = assessment
        .signals
        .iter()
        .filter(|signal| signal.category == "architecture_complexity_index")
        .collect::<Vec<_>>();
    signals.sort_by(|left, right| {
        right
            .value
            .total_cmp(&left.value)
            .then_with(|| left.scope.cmp(&right.scope))
            .then_with(|| left.id.cmp(&right.id))
    });
    if signals.is_empty() {
        output.push_str("No architecture complexity signals were reported.\n\n");
        return Ok(());
    }
    output.push_str(
        "| Scope | Architecture complexity index |\n\
         | --- | ---: |\n",
    );
    for signal in signals.iter().take(budgets.summary_rows) {
        writeln!(
            output,
            "| {} | {:.4} |",
            artifact::escape(&signal.scope),
            signal.value,
        )?;
    }
    append_omitted(output, signals.len(), "architecture signal", budgets)
}

fn append_evidence_gaps(
    output: &mut String,
    assessment: &Assessment,
    budgets: &QualityRenderBudgets,
) -> Result<()> {
    output.push_str("## Broken stages and evidence gaps\n\n### Broken stages\n\n");
    let mut broken = assessment
        .stages
        .iter()
        .filter(|stage| stage.status == StageStatus::Broken)
        .collect::<Vec<_>>();
    broken.sort_by(|left, right| left.name.cmp(&right.name));
    if broken.is_empty() {
        output.push_str("None.\n\n");
    } else {
        output.push_str(
            "| Stage | Exit code | Note |\n\
             | --- | ---: | --- |\n",
        );
        for stage in broken.iter().take(budgets.summary_rows) {
            let exit_code = stage
                .exit_code
                .map_or_else(|| "n/a".to_owned(), |code| code.to_string());
            writeln!(
                output,
                "| {} | {} | {} |",
                artifact::escape(&stage.name),
                exit_code,
                artifact::escape(&stage.note),
            )?;
        }
        append_omitted(output, broken.len(), "broken stage", budgets)?;
    }

    output.push_str("### Evidence-gap tools\n\n");
    let mut gaps = assessment
        .tool_coverage
        .iter()
        .filter(|coverage| coverage.status == CoverageStatus::EvidenceGap)
        .collect::<Vec<_>>();
    gaps.sort_by(|left, right| left.tool.cmp(&right.tool));
    if gaps.is_empty() {
        output.push_str("None.\n\n");
    } else {
        output.push_str(
            "| Tool | Note |\n\
             | --- | --- |\n",
        );
        for coverage in gaps.iter().take(budgets.summary_rows) {
            writeln!(
                output,
                "| {} | {} |",
                artifact::escape(&coverage.tool),
                artifact::escape(&coverage.note),
            )?;
        }
        append_omitted(output, gaps.len(), "evidence-gap tool", budgets)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn manifest_assessment_renders_decision_and_verdict() {
        let temp = tempdir().expect("tempdir");
        let directory = write_assessment(temp.path(), &[]);

        let output = render(&directory, None, &QualityRenderBudgets::default()).expect("summary");

        assert!(output.contains("verdict **refactor**"));
        assert!(output.contains("| Debt units / threshold | 229 / 100 |"));
        assert!(output.contains("| Baseline entries | 227 |"));
        assert!(output.contains("| Findings | 259 |"));
        assert!(output.contains("| Signals | 522 |"));
        assert!(output.contains("| Evidence gaps | 23 |"));
        assert!(output.contains("| Scope LOC | 162702 |"));
        let decision = output.find("## Decision").expect("decision section");
        let crap = output.find("## Coverage risk").expect("CRAP section");
        let architecture = output
            .find("## Architecture complexity")
            .expect("architecture section");
        let gaps = output
            .find("## Broken stages and evidence gaps")
            .expect("evidence section");
        let artifact = output
            .find("uploaded quality assessment artifact")
            .expect("artifact pointer");
        assert!(decision < crap && crap < architecture && architecture < gaps && gaps < artifact);
    }

    #[test]
    fn cargo_crap_report_renders_risk_count_and_null_coverage() {
        let temp = tempdir().expect("tempdir");
        let directory = write_assessment(temp.path(), &[]);
        let report = temp.path().join("cargo-crap/report.json");
        fs::create_dir_all(report.parent().expect("report parent")).expect("cargo-crap directory");
        write_json(
            &report,
            &json!({
                "$schema": "https://raw.githubusercontent.com/minikin/cargo-crap/main/schemas/report-v1.json",
                "version": "0.3.1",
                "entries": [
                    {
                        "file": "crates/kithara-stream/src/stream.rs",
                        "function": "Stream::try_read_with",
                        "line": 357,
                        "cyclomatic": 19.0,
                        "coverage": null,
                        "crap": 380.0,
                        "status": "regressed"
                    },
                    {
                        "file": "crates/kithara-play/src/player.rs",
                        "function": "Player::play",
                        "line": 42,
                        "cyclomatic": 4.0,
                        "coverage": 75.0,
                        "crap": 30.0,
                        "status": "unchanged"
                    }
                ],
                "removed": []
            }),
        );

        let output =
            render(&directory, Some(&report), &QualityRenderBudgets::default()).expect("summary");

        assert!(output.contains("Functions analysed: 2"));
        assert!(output.contains("Above threshold (> 30.0): 1"));
        assert!(output.contains("Worst CRAP score: 380.00"));
        assert!(output.contains(
            "| Stream::try_read_with | crates/kithara-stream/src/stream.rs:357 | 19.00 | n/a | 380.00 |"
        ));
    }

    #[test]
    fn absent_cargo_crap_report_renders_tool_coverage_without_failing() {
        let temp = tempdir().expect("tempdir");
        let directory = write_assessment(temp.path(), &[]);
        let missing_report = temp.path().join("cargo-crap/report.json");

        let output = render(
            &directory,
            Some(&missing_report),
            &QualityRenderBudgets::default(),
        )
        .expect("summary without CRAP");

        assert!(output.contains(
            "- cargo-crap: status `not-applicable`; note: deep profile only; produce it with `just quality assess --depth deep`."
        ));
        assert!(!output.contains("Functions analysed:"));
    }

    #[test]
    fn cargo_crap_table_reports_omitted_rows_and_escapes_cells() {
        let temp = tempdir().expect("tempdir");
        let directory = write_assessment(temp.path(), &[]);
        let report = temp.path().join("cargo-crap/report.json");
        fs::create_dir_all(report.parent().expect("report parent")).expect("cargo-crap directory");
        let entries: Vec<Value> = (1..=7)
            .map(|value| {
                json!({
                    "file": if value == 7 { "crate|file.rs" } else { "crate/file.rs" },
                    "function": if value == 7 { "worst|function\ncontinued" } else { "function" },
                    "line": value,
                    "cyclomatic": f64::from(value),
                    "coverage": null,
                    "crap": f64::from(value)
                })
            })
            .collect();
        write_json(&report, &json!({"entries": entries}));

        let output =
            render(&directory, Some(&report), &QualityRenderBudgets::default()).expect("summary");

        assert!(
            output
                .contains("| worst\\|function continued | crate\\|file.rs:7 | 7.00 | n/a | 7.00 |")
        );
        assert!(output.contains("_2 additional CRAP rows omitted._"));
    }

    /// The CRAP table is bounded by config, not by a constant: a project that
    /// lowers the budget renders fewer rows and reports the wider remainder.
    #[test]
    fn a_lowered_summary_row_budget_shortens_the_crap_table() {
        let temp = tempdir().expect("tempdir");
        let directory = write_assessment(temp.path(), &[]);
        let report = temp.path().join("cargo-crap/report.json");
        fs::create_dir_all(report.parent().expect("report parent")).expect("cargo-crap directory");
        let entries: Vec<Value> = (1..=7)
            .map(|value| {
                json!({
                    "file": "crate/file.rs",
                    "function": format!("function{value}"),
                    "line": value,
                    "cyclomatic": f64::from(value),
                    "coverage": null,
                    "crap": f64::from(value)
                })
            })
            .collect();
        write_json(&report, &json!({"entries": entries}));
        let budgets = QualityRenderBudgets {
            summary_rows: 1,
            ..QualityRenderBudgets::default()
        };

        let output = render(&directory, Some(&report), &budgets).expect("summary");

        assert!(output.contains("| function7 | crate/file.rs:7 | 7.00 | n/a | 7.00 |"));
        assert!(!output.contains("| function6 | crate/file.rs:6 | 6.00 | n/a | 6.00 |"));
        assert!(output.contains("_6 additional CRAP rows omitted._"));
    }

    #[test]
    fn missing_assessment_directory_error_names_the_path() {
        let temp = tempdir().expect("tempdir");
        let missing = temp.path().join("missing-assessment");

        let error = render(&missing, None, &QualityRenderBudgets::default())
            .expect_err("missing directory must fail");

        assert!(error.to_string().contains(&missing.display().to_string()));
    }

    #[test]
    fn missing_assessment_manifest_error_names_the_path() {
        let temp = tempdir().expect("tempdir");
        let directory = temp.path().join("assessment");
        fs::create_dir_all(&directory).expect("assessment directory");
        let manifest = directory.join("manifest.json");

        let error = render(&directory, None, &QualityRenderBudgets::default())
            .expect_err("missing manifest must fail");

        assert!(error.to_string().contains(&manifest.display().to_string()));
    }

    #[test]
    fn assessment_pointer_cannot_escape_the_manifest_directory() {
        for assessment_file in [
            "../assessment.json".to_owned(),
            env::temp_dir()
                .join("outside-assessment.json")
                .display()
                .to_string(),
        ] {
            let temp = tempdir().expect("tempdir");
            let directory = write_assessment(temp.path(), &[]);
            let manifest_path = directory.join("manifest.json");
            let mut manifest: Value =
                serde_json::from_slice(&fs::read(&manifest_path).expect("assessment manifest"))
                    .expect("manifest JSON");
            manifest["files"]["assessment"] = json!(assessment_file);
            write_json(&manifest_path, &manifest);

            let error = render(&directory, None, &QualityRenderBudgets::default())
                .expect_err("unsafe pointer must fail");

            assert!(error.to_string().contains("files.assessment"));
            assert!(
                error
                    .to_string()
                    .contains(&manifest_path.display().to_string())
            );
        }
    }

    #[test]
    fn architecture_table_reports_omitted_rows() {
        let temp = tempdir().expect("tempdir");
        let signals: Vec<Value> = (1..=7)
            .map(|value| {
                json!({
                    "category": "architecture_complexity_index",
                    "id": format!("architecture:crate-{value}"),
                    "scope": format!("crate-{value}"),
                    "state": "diagnostic",
                    "tool": "architecture",
                    "unit": "index",
                    "evidence_artifacts": [],
                    "confidence": 1.0,
                    "value": f64::from(value)
                })
            })
            .collect();
        let directory = write_assessment(temp.path(), &signals);

        let output = render(&directory, None, &QualityRenderBudgets::default()).expect("summary");

        assert!(output.contains("| crate-7 | 7.0000 |"));
        assert!(!output.contains("| crate-1 | 1.0000 |"));
        assert!(output.contains("_2 additional architecture signal rows omitted._"));
    }

    #[test]
    fn broken_stages_and_evidence_gap_tools_are_rendered_and_escaped() {
        let temp = tempdir().expect("tempdir");
        let directory = write_assessment(temp.path(), &[]);
        let assessment_path = directory.join("assessment.json");
        let mut assessment: Value =
            serde_json::from_slice(&fs::read(&assessment_path).expect("assessment fixture"))
                .expect("assessment JSON");
        let mut stages = vec![json!({
            "exit_code": 2,
            "status": "broken",
            "log": "logs/portable.log",
            "name": "00-portable|gates",
            "note": "first line\nsecond line",
            "command": ["just", "ci", "portable"],
            "evidence_artifacts": ["logs/portable.log"],
            "tools": ["clippy"],
            "hard_invariant": true
        })];
        stages.extend((1..=6).map(|value| {
            json!({
                "exit_code": value,
                "status": "broken",
                "log": format!("logs/stage-{value}.log"),
                "name": format!("stage-{value}"),
                "note": "broken",
                "command": ["just", "check"],
                "evidence_artifacts": [],
                "tools": ["check"],
                "hard_invariant": false
            })
        }));
        assessment["stages"] = Value::Array(stages);
        let mut coverage = vec![json!({
            "status": "not-applicable",
            "note": "deep profile only",
            "tool": "cargo-crap",
            "evidence_artifacts": []
        })];
        coverage.extend((1..=7).map(|value| {
            json!({
                "status": "evidence-gap",
                "note": "tool unavailable",
                "tool": if value == 1 { "00-cargo|geiger".to_owned() } else { format!("tool-{value}") },
                "evidence_artifacts": ["logs/geiger.log"]
            })
        }));
        assessment["tool_coverage"] = Value::Array(coverage);
        write_json(&assessment_path, &assessment);

        let output = render(&directory, None, &QualityRenderBudgets::default()).expect("summary");

        assert!(output.contains("| 00-portable\\|gates | 2 | first line second line |"));
        assert!(output.contains("| 00-cargo\\|geiger | tool unavailable |"));
        assert!(output.contains("_2 additional broken stage rows omitted._"));
        assert!(output.contains("_2 additional evidence-gap tool rows omitted._"));
        assert!(output.contains("uploaded quality assessment artifact"));
    }

    fn write_assessment(root: &Path, signals: &[Value]) -> PathBuf {
        let directory = root.join("assessment");
        fs::create_dir_all(&directory).expect("assessment directory");
        write_json(
            &directory.join("manifest.json"),
            &json!({
                "revision": "a8a67acb3135",
                "scope": "workspace",
                "status": "complete",
                "verdict": "refactor",
                "depth": "standard",
                "profile": "product",
                "files": {
                    "assessment": "assessment.json",
                    "document": "assessment.md",
                    "logs": "logs/",
                    "manifest": "manifest.json",
                    "stages": "stages/"
                },
                "schema_version": 1
            }),
        );
        write_json(
            &directory.join("assessment.json"),
            &json!({
                "status": "complete",
                "depth": "standard",
                "profile": "product",
                "scope": {
                    "kind": "workspace",
                    "name": "workspace",
                    "loc": 162702,
                    "workspace_loc": 162702
                },
                "revision": "a8a67acb3135",
                "summary": {
                    "debt_threshold": 100,
                    "debt_units": 229,
                    "baseline_entries": 227,
                    "evidence_gaps": 23,
                    "findings": 259,
                    "signals": 522
                },
                "findings": [],
                "signals": signals,
                "stages": [],
                "tool_coverage": [{
                    "status": "not-applicable",
                    "note": "deep profile only",
                    "tool": "cargo-crap",
                    "evidence_artifacts": []
                }],
                "verdict": "refactor",
                "schema_version": 1
            }),
        );
        directory
    }

    fn write_json(path: &Path, value: &Value) {
        fs::write(
            path,
            serde_json::to_vec_pretty(value).expect("serialize fixture"),
        )
        .expect("write fixture");
    }
}
