use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result};

use super::{
    AssessArgs, AssessmentProfile,
    model::{CoverageStatus, Finding, Signal, StageEvidence},
};
use crate::Ctx;

#[derive(Default)]
pub(super) struct AdapterOutput {
    pub(super) findings: Vec<Finding>,
    pub(super) signals: Vec<Signal>,
    pub(super) coverage: Vec<CoverageEvidence>,
}

pub(super) struct CoverageEvidence {
    pub(super) tool: String,
    pub(super) status: CoverageStatus,
    pub(super) owner: String,
    pub(super) note: String,
    pub(super) artifacts: Vec<String>,
}

pub(super) fn collect(
    ctx: &Ctx,
    assessment_revision: &str,
    scope: &str,
    args: &AssessArgs,
    stages: &[StageEvidence],
) -> Result<AdapterOutput> {
    let root = &ctx.root;
    let mut output = AdapterOutput::default();
    let Some(short_revision) = git_revision(root, true) else {
        return Ok(output);
    };
    let architecture_directory = root.join("target/architecture").join(&short_revision);
    let similarity_directory = root.join("target/similarity").join(&short_revision);
    let architecture_available = architecture_coverage(
        ctx,
        assessment_revision,
        args,
        stages,
        &architecture_directory,
        &mut output.coverage,
    )?;
    let similarity_available = similarity_coverage(
        ctx,
        assessment_revision,
        args,
        stages,
        &similarity_directory,
        &mut output.coverage,
    )?;
    if architecture_available {
        architecture(
            root,
            scope,
            &architecture_directory.join("metrics.json"),
            &mut output,
        )?;
    }
    if similarity_available {
        similarity(
            root,
            scope,
            &similarity_directory.join("report.json"),
            &mut output,
        )?;
    }
    let health = root.join("target/health-report.md");
    let current_health = stages.iter().any(|stage| {
        stage.name == "health"
            && stage
                .evidence_artifacts
                .iter()
                .any(|artifact| artifact == "target/health-report.md")
    });
    if current_health {
        output.findings.extend(health_findings(&health, scope)?);
        output.coverage.extend(health_coverage(root, &health)?);
    }
    let current_quality_lab = stages.iter().any(|stage| stage.name == "quality-lab")
        || stages.iter().any(|stage| stage.name == "coverage-risk");
    if (!assessment_revision.contains("-dirty-") || current_quality_lab)
        && let Some(revision) = git_revision(root, false)
    {
        quality_lab(
            root,
            &root.join("target/quality-lab").join(revision),
            scope,
            current_quality_lab,
            &mut output,
        )?;
    }
    Ok(output)
}

fn architecture_coverage(
    ctx: &Ctx,
    assessment_revision: &str,
    args: &AssessArgs,
    stages: &[StageEvidence],
    directory: &Path,
    coverage: &mut Vec<CoverageEvidence>,
) -> Result<bool> {
    let manifest = directory.join("manifest.json");
    let current = current_artifact(stages, "architecture", &ctx.root, &manifest);
    let reusable = !assessment_revision.contains("-dirty-")
        && architecture_manifest_compatible(ctx, args, &manifest)?;
    let available = current || reusable;
    let artifact = existing_artifact(&ctx.root, &manifest);
    coverage.push(CoverageEvidence {
        tool: "architecture".to_owned(),
        status: evidence_status(current, reusable),
        owner: "architecture".to_owned(),
        note: compatibility_note(available, "architecture"),
        artifacts: artifact.clone(),
    });
    for tool in ["cargo-depgraph", "cargo-modules-structure"] {
        coverage.push(CoverageEvidence {
            tool: tool.to_owned(),
            status: if available {
                CoverageStatus::CoveredBy
            } else {
                CoverageStatus::EvidenceGap
            },
            owner: "architecture".to_owned(),
            note: "the architecture graph owns the equivalent topology evidence".to_owned(),
            artifacts: artifact.clone(),
        });
    }
    if args.profile == AssessmentProfile::Complete {
        for tool in ["integration-test-surfaces", "tooling-surfaces"] {
            coverage.push(CoverageEvidence {
                tool: tool.to_owned(),
                status: if available {
                    CoverageStatus::CoveredBy
                } else {
                    CoverageStatus::EvidenceGap
                },
                owner: "architecture".to_owned(),
                note: "complete architecture evidence includes normally excluded source surfaces"
                    .to_owned(),
                artifacts: artifact.clone(),
            });
        }
    }
    Ok(available)
}

fn similarity_coverage(
    ctx: &Ctx,
    assessment_revision: &str,
    args: &AssessArgs,
    stages: &[StageEvidence],
    directory: &Path,
    coverage: &mut Vec<CoverageEvidence>,
) -> Result<bool> {
    let manifest = directory.join("manifest.json");
    let current = current_artifact(stages, "similarity", &ctx.root, &manifest);
    let reusable = !assessment_revision.contains("-dirty-")
        && similarity_manifest_compatible(ctx, args, &manifest)?;
    let available = current || reusable;
    let artifact = existing_artifact(&ctx.root, &manifest);
    for tool in ["similarity-native", "similarity-rs"] {
        coverage.push(CoverageEvidence {
            tool: tool.to_owned(),
            status: evidence_status(current, reusable),
            owner: "similarity".to_owned(),
            note: compatibility_note(available, "similarity"),
            artifacts: artifact.clone(),
        });
    }
    Ok(available)
}

fn current_artifact(stages: &[StageEvidence], stage_name: &str, root: &Path, path: &Path) -> bool {
    let expected = relative(root, path);
    stages.iter().any(|stage| {
        stage.name == stage_name
            && stage.status != super::model::StageStatus::Broken
            && stage
                .evidence_artifacts
                .iter()
                .any(|artifact| artifact == &expected)
    })
}

fn architecture_manifest_compatible(ctx: &Ctx, args: &AssessArgs, path: &Path) -> Result<bool> {
    let Some(manifest) = read_json(path)? else {
        return Ok(false);
    };
    if manifest["schema_version"].as_u64().unwrap_or(0) < 4
        || manifest["status"].as_str() == Some("incomplete")
    {
        return Ok(false);
    }
    let (expected_package, expected_module) = selected_scope(args);
    if manifest["package"].as_str() != expected_package
        || manifest["module"].as_str() != expected_module
    {
        return Ok(false);
    }
    let include_default_excluded = args.profile == AssessmentProfile::Complete
        || args.krate.is_some()
        || args.module.is_some();
    let expected_crates = if include_default_excluded {
        Vec::new()
    } else {
        ctx.config.architecture.filters.exclude_crates.clone()
    };
    let expected_modules = if include_default_excluded {
        Vec::new()
    } else {
        ctx.config.architecture.filters.exclude_modules.clone()
    };
    Ok(
        string_array(&manifest["filters"]["exclude_crates"]) == expected_crates
            && string_array(&manifest["filters"]["exclude_modules"]) == expected_modules,
    )
}

fn similarity_manifest_compatible(ctx: &Ctx, args: &AssessArgs, path: &Path) -> Result<bool> {
    let Some(manifest) = read_json(path)? else {
        return Ok(false);
    };
    let expected_profile = if args.profile == AssessmentProfile::Complete {
        "strict"
    } else {
        "advisory"
    };
    let expected_include = args.profile == AssessmentProfile::Complete
        || args.krate.is_some()
        || args.module.is_some();
    if manifest["schema_version"].as_u64().unwrap_or(0) < 2
        || manifest["profile"].as_str() != Some(expected_profile)
        || manifest["include_default_excluded"].as_bool() != Some(expected_include)
    {
        return Ok(false);
    }
    let expected_roots = super::orchestrator::similarity_roots(args, ctx)?;
    Ok(string_array(&manifest["roots"]) == expected_roots)
}

fn selected_scope(args: &AssessArgs) -> (Option<&str>, Option<&str>) {
    if let Some(module) = args.module.as_deref()
        && let Some((package, module)) = module.split_once("::")
    {
        return (Some(package), Some(module));
    }
    (args.krate.as_deref(), None)
}

fn string_array(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn evidence_status(current: bool, reusable: bool) -> CoverageStatus {
    if current {
        CoverageStatus::Executed
    } else if reusable {
        CoverageStatus::Reused
    } else {
        CoverageStatus::EvidenceGap
    }
}

fn compatibility_note(available: bool, owner: &str) -> String {
    if available {
        format!("compatible {owner} evidence matches this assessment scope and profile")
    } else {
        format!("no compatible {owner} evidence for this content, scope, and profile")
    }
}

fn existing_artifact(root: &Path, path: &Path) -> Vec<String> {
    path.is_file()
        .then(|| relative(root, path))
        .into_iter()
        .collect()
}

fn architecture(root: &Path, scope: &str, path: &Path, output: &mut AdapterOutput) -> Result<()> {
    let Some(value) = read_json(path)? else {
        return Ok(());
    };
    let artifact = relative(root, path);
    architecture_signals(scope, &value, &artifact, &mut output.signals);
    architecture_findings(scope, &value, &artifact, &mut output.findings);
    if let Some(contours) = value["contours"].as_object() {
        for (contour, metrics) in contours {
            architecture_signals(contour, metrics, &artifact, &mut output.signals);
            architecture_findings(contour, metrics, &artifact, &mut output.findings);
        }
    }
    Ok(())
}

fn architecture_signals(
    scope: &str,
    value: &serde_json::Value,
    artifact: &str,
    signals: &mut Vec<Signal>,
) {
    let confirmed = &value["confirmed"];
    for (name, unit) in [
        ("architecture_complexity_index", "score"),
        ("normalized_propagation", "ratio"),
        ("cyclic_nodes", "nodes"),
        ("normalized_depth", "ratio"),
        ("external_coupling_ratio", "ratio"),
        ("shared_resource_ratio", "ratio"),
        ("multi_owner_resources", "resources"),
        ("cohesion", "ratio"),
    ] {
        let metric = if name == "architecture_complexity_index" {
            &value[name]
        } else {
            &confirmed[name]
        };
        if let Some(metric) = metric.as_f64().or_else(|| {
            metric
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .map(f64::from)
        }) {
            signals.push(Signal {
                id: format!("architecture:{scope}:{name}"),
                tool: "architecture".to_owned(),
                scope: scope.to_owned(),
                category: name.to_owned(),
                value: metric,
                unit: unit.to_owned(),
                state: "diagnostic".to_owned(),
                confidence: 1.0,
                evidence_artifacts: vec![artifact.to_owned()],
            });
        }
    }
}

fn architecture_findings(
    scope: &str,
    value: &serde_json::Value,
    artifact: &str,
    findings: &mut Vec<Finding>,
) {
    let confirmed = &value["confirmed"];
    let cyclic_nodes = confirmed["cyclic_nodes"].as_u64().unwrap_or(0);
    if cyclic_nodes > 0 {
        findings.push(Finding {
            id: format!("architecture:{scope}:cycles"),
            tool: "architecture".to_owned(),
            scope: scope.to_owned(),
            category: "architecture-cycle".to_owned(),
            severity: "warning".to_owned(),
            confidence: 1.0,
            debt_units: 0,
            locations: vec![scope.to_owned()],
            evidence_artifacts: vec![artifact.to_owned()],
            baseline_state: "unbaselined".to_owned(),
            recommended_action: format!(
                "Inspect {cyclic_nodes} nodes in cyclic contours and remove the narrowest back-edge."
            ),
        });
    }
    let multi_owner = confirmed["multi_owner_resources"].as_u64().unwrap_or(0);
    if multi_owner > 0 {
        findings.push(Finding {
            id: format!("architecture:{scope}:multi-owner"),
            tool: "architecture".to_owned(),
            scope: scope.to_owned(),
            category: "shared-ownership".to_owned(),
            severity: "warning".to_owned(),
            confidence: 1.0,
            debt_units: 0,
            locations: vec![scope.to_owned()],
            evidence_artifacts: vec![artifact.to_owned()],
            baseline_state: "unbaselined".to_owned(),
            recommended_action: format!(
                "Name one canonical owner for {multi_owner} resources with multiple visible owners."
            ),
        });
    }
}

fn similarity(root: &Path, scope: &str, path: &Path, output: &mut AdapterOutput) -> Result<()> {
    let Some(value) = read_json(path)? else {
        return Ok(());
    };
    let artifact = relative(root, path);
    let candidates = value["candidates"].as_array().cloned().unwrap_or_default();
    let candidate_count =
        u32::try_from(candidates.len()).context("similarity candidate count exceeds u32")?;
    output.signals.push(Signal {
        id: format!("similarity:{scope}:candidates"),
        tool: "similarity-native".to_owned(),
        scope: scope.to_owned(),
        category: "candidate-count".to_owned(),
        value: f64::from(candidate_count),
        unit: "candidates".to_owned(),
        state: if candidates.is_empty() {
            "clean"
        } else {
            "diagnostic"
        }
        .to_owned(),
        confidence: 1.0,
        evidence_artifacts: vec![artifact.clone()],
    });
    for candidate in candidates {
        let left = &candidate["left"];
        let right = &candidate["right"];
        let left_location = location(left);
        let right_location = location(right);
        let score = candidate["overall_similarity"].as_f64().unwrap_or(0.0);
        let recommendation = candidate["recommendation"].as_str().unwrap_or("inspect");
        output.findings.push(Finding {
            id: format!("similarity:{left_location}:{right_location}"),
            tool: "similarity-native".to_owned(),
            scope: scope.to_owned(),
            category: "similarity-candidate".to_owned(),
            severity: "advisory".to_owned(),
            confidence: score,
            debt_units: 0,
            locations: vec![left_location, right_location],
            evidence_artifacts: vec![artifact.clone()],
            baseline_state: "unbaselined".to_owned(),
            recommended_action: format!(
                "Review `{recommendation}` at {:.1}% similarity; prove substitutability before refactoring.",
                score * 100.0
            ),
        });
    }
    Ok(())
}

fn location(value: &serde_json::Value) -> String {
    let path = value["path"].as_str().unwrap_or("unknown");
    value["line"]
        .as_u64()
        .map_or_else(|| path.to_owned(), |line| format!("{path}:{line}"))
}

pub(super) fn health_findings(path: &Path, scope: &str) -> Result<Vec<Finding>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read health report: {}", path.display()))?;
    let artifact = path.display().to_string();
    let mut findings = Vec::new();
    for (tool, status) in health_stages(&content) {
        if !matches!(status, "FAIL" | "WARN" | "SKIP") {
            continue;
        }
        let category = match status {
            "FAIL" => "hard-invariant",
            "SKIP" => "evidence-gap",
            _ => "advisory",
        };
        findings.push(Finding {
            id: format!("health:{tool}:{status}"),
            tool: tool.to_owned(),
            scope: scope.to_owned(),
            category: category.to_owned(),
            severity: if status == "FAIL" { "error" } else { "warning" }.to_owned(),
            confidence: 1.0,
            debt_units: 0,
            locations: Vec::new(),
            evidence_artifacts: vec![artifact.clone()],
            baseline_state: "unbaselined".to_owned(),
            recommended_action: match status {
                "FAIL" => format!("Restore the `{tool}` invariant before continuing."),
                "SKIP" => {
                    format!("Run `{tool}` in an environment where its evidence is available.")
                }
                _ => format!("Inspect the advisory `{tool}` output and seek corroboration."),
            },
        });
    }
    Ok(findings)
}

fn health_coverage(root: &Path, path: &Path) -> Result<Vec<CoverageEvidence>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read health report: {}", path.display()))?;
    let artifact = relative(root, path);
    let mut coverage = Vec::new();
    for (stage, health_status) in health_stages(&content) {
        let status = if health_status == "SKIP" {
            CoverageStatus::EvidenceGap
        } else {
            CoverageStatus::Executed
        };
        for tool in health_stage_tools(stage) {
            coverage.push(CoverageEvidence {
                tool: (*tool).to_owned(),
                status,
                owner: format!("health:{stage}"),
                note: format!("health stage `{stage}` reported {health_status}"),
                artifacts: vec![artifact.clone()],
            });
        }
    }
    Ok(coverage)
}

fn health_stages(content: &str) -> Vec<(&str, &str)> {
    content
        .lines()
        .filter_map(|line| {
            if !line.starts_with('|') {
                return None;
            }
            let columns = line
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>();
            (columns.len() >= 5 && columns[0].parse::<usize>().is_ok())
                .then(|| (columns[1], columns[2]))
        })
        .collect()
}

fn health_stage_tools(stage: &str) -> &'static [&'static str] {
    match stage {
        "format-check" => &["rustfmt", "cargo-sort", "taplo", "tidy-json"],
        "markdown-format-check" => &["md-formatter"],
        "clippy" => &["cargo-check", "clippy"],
        "ast-grep-advisory" => &["ast-grep"],
        "xtask-lint" => &["arch-lint", "style-lint", "idioms-lint"],
        "quality-report" => &["quality-report"],
        "typos" => &["typos"],
        "orphans" => &["cargo-modules-orphans"],
        "machete" => &["cargo-machete"],
        "shear" => &["cargo-shear"],
        "deny" => &["cargo-deny"],
        "hack-feature-powerset" => &["cargo-hack"],
        "semver-checks" => &["cargo-semver-checks"],
        "geiger" => &["cargo-geiger"],
        "lockbud-deadlock" => &["lockbud"],
        "workspace-unused-pub" => &["cargo-workspace-unused-pub"],
        "workspace-tests" => &["workspace-tests", "cargo-nextest"],
        "doc-tests" => &["doc-tests", "rustdoc"],
        _ => &[],
    }
}

fn quality_lab(
    root: &Path,
    directory: &Path,
    scope: &str,
    current: bool,
    output: &mut AdapterOutput,
) -> Result<()> {
    for profile in ["manual", "scheduled", "coverage"] {
        let path = directory.join(format!("{profile}-summary.json"));
        let Some(value) = read_json(&path)? else {
            continue;
        };
        let artifact = relative(root, &path);
        let Some(tools) = value["tools"].as_array() else {
            continue;
        };
        for tool in tools {
            let name = tool["tool"].as_str().unwrap_or("quality-lab");
            let state = tool["status"].as_str().unwrap_or("tool_error");
            let status = if matches!(state, "clean" | "findings") {
                if current {
                    CoverageStatus::Executed
                } else {
                    CoverageStatus::Reused
                }
            } else {
                CoverageStatus::EvidenceGap
            };
            output.coverage.push(CoverageEvidence {
                tool: name.to_owned(),
                status,
                owner: format!("quality-lab-{profile}"),
                note: format!("Quality Lab status: {state}"),
                artifacts: vec![artifact.clone()],
            });
            if state != "clean" {
                output.findings.push(Finding {
                    id: format!("quality-lab:{profile}:{name}:{state}"),
                    tool: name.to_owned(),
                    scope: scope.to_owned(),
                    category: if matches!(state, "skipped" | "tool_error") {
                        "evidence-gap"
                    } else {
                        "quality-lab-finding"
                    }
                    .to_owned(),
                    severity: if state == "tool_error" {
                        "error"
                    } else {
                        "warning"
                    }
                    .to_owned(),
                    confidence: 1.0,
                    debt_units: 0,
                    locations: Vec::new(),
                    evidence_artifacts: vec![artifact.clone()],
                    baseline_state: "unbaselined".to_owned(),
                    recommended_action: if matches!(state, "skipped" | "tool_error") {
                        format!("Restore `{name}` evidence before making a deep health claim.")
                    } else {
                        format!("Inspect `{name}` findings and correlate them with another tool.")
                    },
                });
            }
        }
    }
    Ok(())
}

fn read_json(path: &Path) -> Result<Option<serde_json::Value>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("read artifact: {}", path.display()))?;
    let value = serde_json::from_str(&text)
        .with_context(|| format!("parse artifact JSON: {}", path.display()))?;
    Ok(Some(value))
}

fn git_revision(root: &Path, short: bool) -> Option<String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).arg("rev-parse");
    if short {
        command.arg("--short=12");
    }
    let output = command.arg("HEAD").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|revision| revision.trim().to_owned())
        .filter(|revision| !revision.is_empty())
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn health_failures_are_normalized_as_hard_invariants() {
        let temp = tempdir().expect("tempdir");
        let report = temp.path().join("health.md");
        fs::write(
            &report,
            "# health\n\n| # | Stage | Status | Duration | Notes |\n\
             |---|---|---|---|---|\n\
             | 1 | clippy | FAIL | 1s | exit 1 |\n\
             | 2 | geiger | WARN | 1s | exit 1 |\n",
        )
        .expect("health report");

        let findings = health_findings(&report, "workspace").expect("health findings");

        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].category, "hard-invariant");
        assert_eq!(findings[0].tool, "clippy");
        assert_eq!(findings[1].category, "advisory");
    }

    #[test]
    fn health_coverage_preserves_successes_when_other_tools_are_skipped() {
        let temp = tempdir().expect("tempdir");
        let report = temp.path().join("health.md");
        fs::write(
            &report,
            "# health\n\n| # | Stage | Status | Duration | Notes |\n\
             |---|---|---|---|---|\n\
             | 1 | clippy | PASS | 1s | |\n\
             | 2 | xtask-lint | FAIL | 1s | exit 1 |\n\
             | 3 | geiger | SKIP | 0s | unavailable |\n",
        )
        .expect("health report");

        let coverage = health_coverage(temp.path(), &report).expect("health coverage");
        let status = |tool: &str| {
            coverage
                .iter()
                .find(|entry| entry.tool == tool)
                .map(|entry| entry.status)
        };

        assert_eq!(status("clippy"), Some(CoverageStatus::Executed));
        assert_eq!(status("cargo-check"), Some(CoverageStatus::Executed));
        assert_eq!(status("arch-lint"), Some(CoverageStatus::Executed));
        assert_eq!(status("cargo-geiger"), Some(CoverageStatus::EvidenceGap));
    }
}
