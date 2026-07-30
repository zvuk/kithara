use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;

use super::model::{Assessment, ToolCoverage};

const MAX_RENDERED_FINDINGS: usize = 100;
const MAX_ARCHITECTURE_HOTSPOTS: usize = 30;

pub(super) struct ArtifactSet {
    pub(super) document: PathBuf,
}

#[derive(Serialize)]
struct Manifest<'a> {
    schema_version: u32,
    revision: &'a str,
    status: &'static str,
    profile: super::AssessmentProfile,
    depth: super::AssessmentDepth,
    scope: &'a str,
    verdict: &'static str,
    files: BTreeMap<&'static str, &'static str>,
}

pub(super) fn write(assessment: &Assessment, root: &Path) -> Result<ArtifactSet> {
    let output = &assessment.output_directory;
    fs::create_dir_all(output.join("stages"))
        .with_context(|| format!("create assessment stages: {}", output.display()))?;
    fs::create_dir_all(output.join("logs"))
        .with_context(|| format!("create assessment logs: {}", output.display()))?;
    write_json(&output.join("assessment.json"), assessment)?;
    write_json(
        &output.join("manifest.json"),
        &Manifest {
            schema_version: assessment.schema_version,
            revision: &assessment.revision,
            status: assessment.status.as_str(),
            profile: assessment.profile,
            depth: assessment.depth,
            scope: &assessment.scope.name,
            verdict: assessment.verdict.as_str(),
            files: BTreeMap::from([
                ("assessment", "assessment.json"),
                ("document", "assessment.md"),
                ("logs", "logs/"),
                ("manifest", "manifest.json"),
                ("stages", "stages/"),
            ]),
        },
    )?;
    let document = output.join("assessment.md");
    fs::write(&document, markdown(assessment, root))
        .with_context(|| format!("write assessment report: {}", document.display()))?;
    Ok(ArtifactSet { document })
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let json = serde_json::to_string_pretty(value).context("serialize quality assessment")?;
    fs::write(path, format!("{json}\n"))
        .with_context(|| format!("write quality assessment: {}", path.display()))
}

fn markdown(assessment: &Assessment, root: &Path) -> String {
    let mut output = header(assessment);
    output.push_str(verdict_guidance(assessment));
    append_comparison(&mut output, assessment);
    append_signals(&mut output, assessment);
    append_coverage(&mut output, assessment);
    append_stages(&mut output, assessment);
    append_findings(&mut output, assessment);
    output.push_str(&format!(
        "\n## Interpretation contract\n\n\
         The tool verdict is deterministic and based on repository evidence. An agent may add \
         contextual interpretation, but must report it separately and preserve links to the \
         evidence above. Existing baseline entries are debt, not accepted quality. The target is \
         zero; the workspace refactor threshold is 100 and smaller scopes use a LOC-proportional \
         threshold with a minimum of one.\n\n\
         Workspace root: `{}`\n",
        root.display()
    ));
    output
}

fn header(assessment: &Assessment) -> String {
    format!(
        "# Repository quality assessment\n\n\
         - Revision: `{}`\n\
         - Profile: `{}`\n\
         - Depth: `{}`\n\
         - Scope: `{}`\n\
         - Verdict: **{}**\n\n\
         ## Decision summary\n\n\
         | Signal | Value |\n\
         | --- | ---: |\n\
         | Technical debt | {} |\n\
         | Refactor threshold | {} |\n\
         | Baseline entries | {} |\n\
         | Findings | {} |\n\
         | Diagnostic signals | {} |\n\
         | Evidence gaps | {} |\n\
         | Scope LOC | {} |\n\
         | Workspace LOC | {} |\n\n",
        assessment.revision,
        assessment.profile,
        assessment.depth,
        assessment.scope.name,
        assessment.verdict.as_str(),
        assessment.summary.debt_units,
        assessment.summary.debt_threshold,
        assessment.summary.baseline_entries,
        assessment.summary.findings,
        assessment.summary.signals,
        assessment.summary.evidence_gaps,
        assessment.scope.loc,
        assessment.scope.workspace_loc,
    )
}

fn append_comparison(output: &mut String, assessment: &Assessment) {
    if let Some(comparison) = &assessment.comparison {
        output.push_str(&format!(
            "## Baseline comparison\n\n\
             - Source: `{}`\n\
             - State: **{}**\n\
             - Debt delta: {:+}\n\
             - Finding delta: {:+}\n\n",
            comparison.source, comparison.state, comparison.debt_delta, comparison.finding_delta,
        ));
    }
}

fn append_signals(output: &mut String, assessment: &Assessment) {
    output.push_str(
        "## Diagnostic signals\n\n\
         | Scope | Tool | Metric | Value | State | Evidence |\n\
         | --- | --- | --- | ---: | --- | --- |\n",
    );
    if assessment.signals.is_empty() {
        output.push_str("| n/a | n/a | n/a | 0 | unavailable | n/a |\n");
    } else {
        let mut signals = assessment
            .signals
            .iter()
            .filter(|signal| {
                signal.scope == assessment.scope.name
                    || signal.tool != "architecture"
                    || signal.category == "architecture_complexity_index"
            })
            .collect::<Vec<_>>();
        signals.sort_by(|left, right| {
            let left_primary = left.scope == assessment.scope.name;
            let right_primary = right.scope == assessment.scope.name;
            right_primary
                .cmp(&left_primary)
                .then_with(|| {
                    if left.category == "architecture_complexity_index"
                        && right.category == "architecture_complexity_index"
                    {
                        right.value.total_cmp(&left.value)
                    } else {
                        left.category.cmp(&right.category)
                    }
                })
                .then_with(|| left.scope.cmp(&right.scope))
        });
        let primary_count = signals
            .iter()
            .filter(|signal| signal.scope == assessment.scope.name)
            .count();
        let rendered_limit = primary_count + MAX_ARCHITECTURE_HOTSPOTS;
        signals.truncate(rendered_limit);
        for signal in signals {
            output.push_str(&format!(
                "| {} | {} | {} | {:.4} {} | {} | {} |\n",
                escape(&signal.scope),
                escape(&signal.tool),
                escape(&signal.category),
                signal.value,
                escape(&signal.unit),
                escape(&signal.state),
                escape(&signal.evidence_artifacts.join(", ")),
            ));
        }
        output.push_str(&format!(
            "\nThe table keeps scope-level signals and the top \
             {MAX_ARCHITECTURE_HOTSPOTS} architecture hotspots. All {} signals remain in \
             `assessment.json`.\n\n",
            assessment.signals.len()
        ));
    }
}

fn append_coverage(output: &mut String, assessment: &Assessment) {
    output.push_str(
        "## Tool coverage\n\n\
         | Tool | Status | Owner | Evidence | Note |\n\
         | --- | --- | --- | --- | --- |\n",
    );
    for coverage in &assessment.tool_coverage {
        output.push_str(&coverage_row(coverage));
    }
}

fn append_stages(output: &mut String, assessment: &Assessment) {
    output.push_str(
        "\n## Executed stages\n\n\
         | Stage | Kind | Status | Exit | Evidence | Command |\n\
         | --- | --- | --- | ---: | --- | --- |\n",
    );
    if assessment.stages.is_empty() {
        output.push_str("| reuse-existing | n/a | n/a | n/a | existing artifacts only | n/a |\n");
    } else {
        for stage in &assessment.stages {
            output.push_str(&format!(
                "| {} | {} | {} | {} | {} | `{}` |\n",
                escape(&stage.name),
                if stage.hard_invariant {
                    "hard invariant"
                } else {
                    "advisory"
                },
                stage.status.as_str(),
                stage
                    .exit_code
                    .map_or_else(|| "n/a".to_owned(), |code| code.to_string()),
                escape(&stage.evidence_artifacts.join(", ")),
                escape(&stage.command.join(" ")),
            ));
        }
    }
}

fn append_findings(output: &mut String, assessment: &Assessment) {
    output.push_str(
        "\n## Prioritized findings\n\n\
         | Debt | Tool | Category | Location | Recommended action |\n\
         | ---: | --- | --- | --- | --- |\n",
    );
    if assessment.findings.is_empty() {
        output.push_str("| 0 | n/a | n/a | n/a | No baseline debt in this scope. |\n");
    } else {
        let mut findings = assessment.findings.iter().collect::<Vec<_>>();
        findings.sort_by(|left, right| {
            right
                .debt_units
                .cmp(&left.debt_units)
                .then_with(|| left.id.cmp(&right.id))
        });
        for finding in findings.iter().take(MAX_RENDERED_FINDINGS) {
            output.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                finding.debt_units,
                escape(&finding.tool),
                escape(&finding.category),
                escape(&finding.locations.join(", ")),
                escape(&finding.recommended_action),
            ));
        }
        if findings.len() > MAX_RENDERED_FINDINGS {
            output.push_str(&format!(
                "\nThe table shows the first {MAX_RENDERED_FINDINGS} of {} prioritized findings. \
                 Every finding remains available in `assessment.json`; no evidence was removed \
                 from analysis.\n",
                findings.len()
            ));
        }
    }
}

fn verdict_guidance(assessment: &Assessment) -> &'static str {
    match assessment.verdict.as_str() {
        "refactor" => {
            "The debt threshold or regression policy is exceeded. Start a bounded refactor from \
             the highest-debt findings and use the baseline comparison to prove improvement.\n\n"
        }
        "investigate" => {
            "Signals need corroboration. Inspect the strongest source locations before choosing a \
             refactor boundary.\n\n"
        }
        "evidence-gap" => {
            "The available evidence is insufficient for a health claim. Fill the listed tool gaps \
             before making an architecture decision.\n\n"
        }
        "stable-with-debt" => {
            "Debt remains below the proportional refactor threshold. Do not grow it; reduce the \
             highest-value entries opportunistically.\n\n"
        }
        _ => "No debt or evidence gap currently triggers action.\n\n",
    }
}

fn coverage_row(coverage: &ToolCoverage) -> String {
    format!(
        "| {} | {} | {} | {} | {} |\n",
        escape(&coverage.tool),
        coverage.status.as_str(),
        escape(coverage.owner.as_deref().unwrap_or("")),
        escape(&coverage.evidence_artifacts.join(", ")),
        escape(&coverage.note),
    )
}

fn escape(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}
