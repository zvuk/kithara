use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use cargo_metadata::{Metadata, Package};
use glob::Pattern;

use super::{
    AssessArgs, AssessmentDepth, AssessmentProfile,
    adapters::{self, CoverageEvidence},
    model::{
        AnalysisStatus, Assessment, BaselineComparison, CoverageStatus, Finding, Scope,
        StageEvidence, StageStatus, Summary, ToolCoverage, Verdict,
    },
};
use crate::{
    Ctx,
    common::{
        baseline::Baseline, project::QualityAssessmentToolPolicyConfig, walker::walk_rs_files,
    },
};

const SCHEMA_VERSION: u32 = 1;
const WORKSPACE_DEBT_THRESHOLD: u64 = 100;

pub(super) struct Revision {
    pub(super) directory: String,
    pub(super) digest: Option<String>,
}

struct ScopeSelection {
    kind: &'static str,
    name: String,
    package_root: Option<PathBuf>,
    module_paths: Vec<PathBuf>,
}

pub(super) fn collect(
    args: &AssessArgs,
    ctx: &Ctx,
    stages: &[StageEvidence],
) -> Result<Assessment> {
    let metadata = ctx.metadata()?;
    let selection = select_scope(args, metadata, &ctx.root)?;
    let (loc, workspace_loc) = scope_loc(args.profile, &selection, ctx, metadata)?;
    let debt_threshold = scaled_threshold(loc, workspace_loc, selection.kind);
    let revision = revision(&ctx.root);
    let mut findings = baseline_findings(args.profile, &selection, ctx)?;
    let baseline_entries = findings.len();
    findings.extend(stage_findings(stages, &selection.name));
    let adapted = adapters::collect(ctx, &revision.directory, &selection.name, args, stages)?;
    findings.extend(adapted.findings);
    let debt_units = findings.iter().map(|finding| finding.debt_units).sum();
    let tool_coverage = tool_coverage(args, ctx, stages, &adapted.coverage);
    let evidence_gaps = tool_coverage
        .iter()
        .filter(|coverage| coverage.status == CoverageStatus::EvidenceGap)
        .count();
    let gap_findings = coverage_gap_findings(&tool_coverage, &selection.name, &findings);
    findings.extend(gap_findings);
    findings.sort_by(|left, right| left.id.cmp(&right.id));
    let comparison = args
        .baseline
        .as_deref()
        .map(|path| compare_baseline(path, debt_units, findings.len()))
        .transpose()?;
    let debt_regressed = comparison
        .as_ref()
        .is_some_and(|comparison| comparison.debt_delta > 0);
    let stage_findings = stages
        .iter()
        .filter(|stage| stage.status == StageStatus::Findings)
        .count();
    let hard_invariant = findings
        .iter()
        .any(|finding| finding.category == "hard-invariant");
    let corroborated = has_corroborated_location(&findings);
    let diagnostic_findings = findings
        .iter()
        .filter(|finding| finding.debt_units == 0 && finding.category != "evidence-gap")
        .count();
    let verdict = verdict(
        debt_units,
        debt_threshold,
        debt_regressed,
        hard_invariant,
        corroborated,
        stage_findings + diagnostic_findings,
        evidence_gaps,
    );
    let output_directory = ctx
        .root
        .join("target/quality-assessment")
        .join(&revision.directory)
        .join(format!("{}-{}", args.profile, args.depth));
    Ok(Assessment {
        schema_version: SCHEMA_VERSION,
        revision: revision.directory,
        content_digest: revision.digest,
        profile: args.profile,
        depth: args.depth,
        scope: Scope {
            kind: selection.kind.to_owned(),
            name: selection.name,
            loc,
            workspace_loc,
        },
        status: if stages
            .iter()
            .any(|stage| stage.status == StageStatus::Broken)
        {
            AnalysisStatus::Partial
        } else {
            AnalysisStatus::Complete
        },
        verdict,
        summary: Summary {
            debt_units,
            debt_threshold,
            baseline_entries,
            findings: findings.len(),
            evidence_gaps,
            signals: adapted.signals.len(),
        },
        findings,
        signals: adapted.signals,
        tool_coverage,
        stages: stages.to_vec(),
        comparison,
        output_directory,
    })
}

fn coverage_gap_findings(
    coverage: &[ToolCoverage],
    scope: &str,
    existing_findings: &[Finding],
) -> Vec<Finding> {
    coverage
        .iter()
        .filter(|entry| entry.status == CoverageStatus::EvidenceGap)
        .filter(|entry| {
            !existing_findings
                .iter()
                .any(|finding| finding.tool == entry.tool && finding.category == "evidence-gap")
        })
        .map(|entry| Finding {
            id: format!("coverage:{}:evidence-gap", entry.tool),
            tool: entry.tool.clone(),
            scope: scope.to_owned(),
            category: "evidence-gap".to_owned(),
            severity: "warning".to_owned(),
            confidence: 1.0,
            debt_units: 0,
            locations: Vec::new(),
            evidence_artifacts: entry.evidence_artifacts.clone(),
            baseline_state: "unbaselined".to_owned(),
            recommended_action: format!(
                "Run or supply `{}` evidence before making a complete health claim.",
                entry.tool
            ),
        })
        .collect()
}

fn select_scope(args: &AssessArgs, metadata: &Metadata, root: &Path) -> Result<ScopeSelection> {
    if let Some(module) = args.module.as_deref() {
        let (package_name, module_name) = module
            .split_once("::")
            .ok_or_else(|| anyhow::anyhow!("module scope must be `package::module`"))?;
        let package = package(metadata, package_name)?;
        let package_root = package_root(package)?;
        let relative = package_root
            .strip_prefix(root)
            .unwrap_or(&package_root)
            .to_path_buf();
        let module_path = module_name.replace("::", "/");
        return Ok(ScopeSelection {
            kind: "module",
            name: module.to_owned(),
            package_root: Some(relative.clone()),
            module_paths: vec![
                relative.join("src").join(format!("{module_path}.rs")),
                relative.join("src").join(module_path),
            ],
        });
    }
    if let Some(package_name) = args.krate.as_deref() {
        let package = package(metadata, package_name)?;
        let package_root = package_root(package)?;
        return Ok(ScopeSelection {
            kind: "crate",
            name: package_name.to_owned(),
            package_root: Some(
                package_root
                    .strip_prefix(root)
                    .unwrap_or(&package_root)
                    .to_path_buf(),
            ),
            module_paths: Vec::new(),
        });
    }
    Ok(ScopeSelection {
        kind: "workspace",
        name: "workspace".to_owned(),
        package_root: None,
        module_paths: Vec::new(),
    })
}

fn package<'a>(metadata: &'a Metadata, name: &str) -> Result<&'a Package> {
    metadata
        .workspace_packages()
        .into_iter()
        .find(|package| package.name == name)
        .ok_or_else(|| anyhow::anyhow!("workspace package not found: {name}"))
}

fn package_root(package: &Package) -> Result<PathBuf> {
    package
        .manifest_path
        .parent()
        .map(|path| path.as_std_path().to_path_buf())
        .context("package manifest has no parent directory")
}

fn baseline_findings(
    profile: AssessmentProfile,
    selection: &ScopeSelection,
    ctx: &Ctx,
) -> Result<Vec<Finding>> {
    let excluded_packages = excluded_package_roots(ctx)?;
    let mut findings = Vec::new();
    for namespace in ["arch", "style", "idioms"] {
        let directory = ctx.root.join(".config").join(namespace);
        let baseline = Baseline::load(&directory)?;
        let artifact = relative(&ctx.root, &directory.join("baseline.toml"));
        for (check, entries) in baseline.checks {
            for (key, count) in entries {
                if !key_in_scope(&key, selection)
                    || profile == AssessmentProfile::Product
                        && selection.kind == "workspace"
                        && !is_product_key(&key, &excluded_packages)
                {
                    continue;
                }
                findings.push(Finding {
                    id: format!("baseline:{namespace}:{check}:{key}"),
                    tool: format!("{namespace}-lint"),
                    scope: selection.name.clone(),
                    category: "lint-debt".to_owned(),
                    severity: "warning".to_owned(),
                    confidence: 1.0,
                    debt_units: count,
                    locations: vec![key.clone()],
                    evidence_artifacts: vec![artifact.clone()],
                    baseline_state: "existing".to_owned(),
                    recommended_action: format!(
                        "Remove the `{check}` violation and ratchet the {namespace} baseline down."
                    ),
                });
            }
        }
    }
    Ok(findings)
}

fn key_in_scope(key: &str, selection: &ScopeSelection) -> bool {
    if selection.kind == "workspace" {
        return true;
    }
    let path = key.split("::").next().unwrap_or(key);
    if selection.kind == "crate" {
        return selection.package_root.as_ref().is_some_and(|root| {
            let root = normalize(root);
            path == root || path.starts_with(&format!("{root}/"))
        });
    }
    selection.module_paths.iter().any(|module| {
        let module = normalize(module);
        path == module || path.starts_with(&format!("{module}/"))
    })
}

fn is_product_key(key: &str, excluded_packages: &[String]) -> bool {
    let normalized = key.replace('\\', "/");
    if normalized.contains("/tests/")
        || normalized.contains("/benches/")
        || normalized.contains("::tests::")
        || normalized.contains("::test::")
    {
        return false;
    }
    !excluded_packages.iter().any(|root| {
        normalized == *root
            || normalized.starts_with(&format!("{root}/"))
            || normalized.starts_with(&format!("{root}::"))
    })
}

fn scope_loc(
    profile: AssessmentProfile,
    selection: &ScopeSelection,
    ctx: &Ctx,
    metadata: &Metadata,
) -> Result<(u64, u64)> {
    let excluded = excluded_package_patterns(ctx);
    let selected_package = match selection.kind {
        "crate" => Some(selection.name.as_str()),
        "module" => selection.name.split_once("::").map(|(package, _)| package),
        _ => None,
    };
    let mut package_files: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for package in metadata.workspace_packages() {
        let name = package.name.to_string();
        let explicitly_selected = selected_package == Some(name.as_str());
        if profile == AssessmentProfile::Product
            && !explicitly_selected
            && excluded.iter().any(|pattern| pattern.matches(&name))
        {
            continue;
        }
        let root = package_root(package)?;
        let scan_root = if profile == AssessmentProfile::Product {
            root.join("src")
        } else {
            root
        };
        let files = walk_rs_files(&scan_root)?
            .into_iter()
            .filter(|path| {
                !path.components().any(|component| {
                    let name = component.as_os_str();
                    name == "target" || name == ".git"
                }) && (profile == AssessmentProfile::Complete || !is_test_file(path))
            })
            .collect();
        package_files.insert(name, files);
    }
    let workspace_loc = loc_for_files(package_files.values().flatten())?;
    let scope_files = package_files
        .iter()
        .flat_map(|(name, files)| {
            files
                .iter()
                .filter(move |path| file_in_scope(name, path, selection, &ctx.root))
        })
        .collect::<Vec<_>>();
    let loc = if selection.kind == "workspace" {
        workspace_loc
    } else {
        loc_for_files(scope_files)?
    };
    Ok((loc, workspace_loc))
}

fn file_in_scope(name: &str, path: &Path, selection: &ScopeSelection, root: &Path) -> bool {
    if selection.kind == "workspace" {
        return true;
    }
    if selection.kind == "crate" {
        return selection.name == name;
    }
    let relative = path.strip_prefix(root).unwrap_or(path);
    selection
        .module_paths
        .iter()
        .any(|module| relative == module || relative.starts_with(module))
}

fn loc_for_files<'a>(files: impl IntoIterator<Item = &'a PathBuf>) -> Result<u64> {
    let mut total = 0u64;
    for file in files {
        total += fs::read_to_string(file)
            .with_context(|| format!("read Rust source for LOC: {}", file.display()))?
            .lines()
            .count() as u64;
    }
    Ok(total)
}

fn is_test_file(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("tests" | "benches" | "examples")
        )
    }) || matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("test.rs" | "tests.rs" | "bench.rs" | "benches.rs")
    )
}

fn scaled_threshold(loc: u64, workspace_loc: u64, kind: &str) -> u64 {
    if kind == "workspace" || workspace_loc == 0 {
        return WORKSPACE_DEBT_THRESHOLD;
    }
    WORKSPACE_DEBT_THRESHOLD
        .saturating_mul(loc)
        .div_ceil(workspace_loc)
        .max(1)
}

fn verdict(
    debt_units: u64,
    debt_threshold: u64,
    debt_regressed: bool,
    hard_invariant: bool,
    corroborated: bool,
    diagnostic_findings: usize,
    evidence_gaps: usize,
) -> Verdict {
    if hard_invariant || corroborated || debt_regressed || debt_units >= debt_threshold {
        Verdict::Refactor
    } else if diagnostic_findings > 0 {
        Verdict::Investigate
    } else if debt_units > 0 {
        Verdict::StableWithDebt
    } else if evidence_gaps > 0 {
        Verdict::EvidenceGap
    } else {
        Verdict::Healthy
    }
}

fn compare_baseline(path: &Path, debt_units: u64, findings: usize) -> Result<BaselineComparison> {
    let assessment_path = if path.is_dir() {
        path.join("assessment.json")
    } else {
        path.to_path_buf()
    };
    let text = fs::read_to_string(&assessment_path)
        .with_context(|| format!("read assessment baseline: {}", assessment_path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("parse assessment baseline: {}", assessment_path.display()))?;
    let prior_debt = value["summary"]["debt_units"]
        .as_u64()
        .context("assessment baseline is missing summary.debt_units")?;
    let prior_findings = value["summary"]["findings"]
        .as_u64()
        .context("assessment baseline is missing summary.findings")?;
    let debt_delta = debt_units as i64 - prior_debt as i64;
    let finding_delta = findings as i64 - prior_findings as i64;
    Ok(BaselineComparison {
        source: assessment_path.display().to_string(),
        debt_delta,
        finding_delta,
        state: if debt_delta > 0 {
            "regressed"
        } else if debt_delta < 0 {
            "improved"
        } else {
            "unchanged"
        }
        .to_owned(),
    })
}

fn tool_coverage(
    args: &AssessArgs,
    ctx: &Ctx,
    stages: &[StageEvidence],
    adapted: &[CoverageEvidence],
) -> Vec<ToolCoverage> {
    let mut coverage = base_coverage(ctx);
    append_core_coverage(&mut coverage);
    append_deep_coverage(&mut coverage, args);
    apply_tool_policy(
        &mut coverage,
        &ctx.config.quality.assessment.not_applicable_tools,
    );
    apply_stage_coverage(&mut coverage, stages);
    apply_adapter_coverage(&mut coverage, adapted);
    coverage.sort_by(|left, right| left.tool.cmp(&right.tool));
    coverage
}

fn base_coverage(ctx: &Ctx) -> Vec<ToolCoverage> {
    let mut coverage = Vec::new();
    for namespace in ["arch", "style", "idioms"] {
        let artifact = ctx
            .root
            .join(".config")
            .join(namespace)
            .join("baseline.toml");
        coverage.push(tool(
            &format!("{namespace}-lint"),
            if artifact.is_file() {
                CoverageStatus::Reused
            } else {
                CoverageStatus::EvidenceGap
            },
            None,
            "ratcheted lint baseline is counted as technical debt",
            existing(&ctx.root, &[artifact]),
        ));
    }
    coverage.push(tool(
        "cargo-metadata",
        CoverageStatus::Executed,
        None,
        "workspace topology and package ownership resolved by the assessment",
        Vec::new(),
    ));
    coverage
}

fn append_core_coverage(coverage: &mut Vec<ToolCoverage>) {
    for name in [
        "rustfmt",
        "cargo-sort",
        "taplo",
        "tidy-json",
        "md-formatter",
        "cargo-check",
        "clippy",
        "rustdoc",
        "ast-grep",
        "typos",
        "cargo-modules-orphans",
        "cargo-workspace-unused-pub",
        "manifest-hygiene",
        "quality-report",
        "rstest-audit",
        "unimock-check",
        "trait-mock-audit",
        "trait-mock-exceptions",
        "workspace-tests",
        "cargo-nextest",
        "doc-tests",
    ] {
        coverage.push(tool(
            name,
            CoverageStatus::EvidenceGap,
            None,
            "the current assessment has not produced this standard-stage evidence",
            Vec::new(),
        ));
    }
}

fn append_deep_coverage(coverage: &mut Vec<ToolCoverage>, args: &AssessArgs) {
    let deep_only = [
        "extended-clippy",
        "cargo-deny",
        "cargo-machete",
        "cargo-shear",
        "cargo-hack",
        "cargo-semver-checks",
        "cargo-geiger",
        "lockbud",
        "cha",
        "rustqual",
        "pmat",
        "cargo-dupes",
        "cargo-public-api",
        "cargo-bloat",
        "e2e-tests",
        "e2e-resamplers",
        "selenium-tests",
        "feature-gate",
        "cargo-llvm-cov",
        "cargo-crap",
        "cargo-mutants",
        "loom",
        "rtsan",
        "perf-suite",
        "perf-compare",
        "benchmarks",
        "memory",
        "wasm-check",
        "wasm-build",
        "wasm-tests",
        "wasm-size",
        "wasm-slim",
        "apple-build",
        "apple-xcframework",
        "apple-docs",
        "android-build",
        "android-aar",
    ];
    for name in deep_only {
        let excluded_by_profile = args.profile == AssessmentProfile::Product
            && matches!(
                name,
                "e2e-tests" | "e2e-resamplers" | "selenium-tests" | "feature-gate"
            );
        let status = if args.depth == AssessmentDepth::Standard || excluded_by_profile {
            CoverageStatus::NotApplicable
        } else {
            CoverageStatus::EvidenceGap
        };
        coverage.push(tool(
            name,
            status,
            None,
            if excluded_by_profile {
                "excluded by the product profile"
            } else if args.depth == AssessmentDepth::Standard {
                "deep profile only"
            } else {
                "deep evidence must be executed or supplied"
            },
            Vec::new(),
        ));
    }
    if args.profile == AssessmentProfile::Product {
        coverage.push(tool(
            "integration-test-surfaces",
            CoverageStatus::NotApplicable,
            None,
            "excluded by the product profile",
            Vec::new(),
        ));
        coverage.push(tool(
            "tooling-surfaces",
            CoverageStatus::NotApplicable,
            None,
            "xtask, devtools, and test utilities are excluded by the product profile",
            Vec::new(),
        ));
    }
}

fn apply_tool_policy(
    coverage: &mut Vec<ToolCoverage>,
    policies: &[QualityAssessmentToolPolicyConfig],
) {
    for policy in policies {
        upsert_coverage(
            coverage,
            tool(
                &policy.tool,
                CoverageStatus::NotApplicable,
                Some("project-policy"),
                &policy.reason,
                Vec::new(),
            ),
        );
    }
}

fn apply_stage_coverage(coverage: &mut Vec<ToolCoverage>, stages: &[StageEvidence]) {
    for stage in stages {
        for owned_tool in &stage.tools {
            let status = match stage.status {
                StageStatus::Clean | StageStatus::Findings => CoverageStatus::Executed,
                StageStatus::Broken => CoverageStatus::EvidenceGap,
            };
            let evidence = std::iter::once(stage.log.clone())
                .chain(stage.evidence_artifacts.iter().cloned())
                .collect();
            upsert_coverage(
                coverage,
                tool(owned_tool, status, Some(&stage.name), &stage.note, evidence),
            );
        }
    }
}

fn apply_adapter_coverage(coverage: &mut Vec<ToolCoverage>, adapted: &[CoverageEvidence]) {
    for evidence in adapted {
        upsert_coverage(
            coverage,
            tool(
                &evidence.tool,
                evidence.status,
                Some(&evidence.owner),
                &evidence.note,
                evidence.artifacts.clone(),
            ),
        );
    }
}

fn upsert_coverage(coverage: &mut Vec<ToolCoverage>, replacement: ToolCoverage) {
    if let Some(existing) = coverage
        .iter_mut()
        .find(|entry| entry.tool == replacement.tool)
    {
        *existing = replacement;
    } else {
        coverage.push(replacement);
    }
}

fn has_corroborated_location(findings: &[Finding]) -> bool {
    let mut seen: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for finding in findings {
        for location in &finding.locations {
            let path = canonical_location(location);
            if !path.is_empty() {
                seen.entry(path).or_default().insert(&finding.tool);
            }
        }
    }
    seen.values().any(|tools| tools.len() >= 2)
}

fn canonical_location(location: &str) -> String {
    let symbolic = location.split("::").next().unwrap_or(location);
    let mut parts = symbolic.split(':');
    parts.next().unwrap_or("").to_owned()
}

fn stage_findings(stages: &[StageEvidence], scope: &str) -> Vec<Finding> {
    stages
        .iter()
        .filter(|stage| stage.status != StageStatus::Clean)
        .map(|stage| Finding {
            id: format!("stage:{}", stage.name),
            tool: stage.tools.join(","),
            scope: scope.to_owned(),
            category: if stage.status == StageStatus::Broken {
                "evidence-gap"
            } else if stage.hard_invariant {
                "hard-invariant"
            } else {
                "tool-finding"
            }
            .to_owned(),
            severity: if stage.status == StageStatus::Broken {
                "error"
            } else {
                "warning"
            }
            .to_owned(),
            confidence: 1.0,
            debt_units: 0,
            locations: Vec::new(),
            evidence_artifacts: std::iter::once(stage.log.clone())
                .chain(stage.evidence_artifacts.iter().cloned())
                .collect(),
            baseline_state: "unbaselined".to_owned(),
            recommended_action: if stage.status == StageStatus::Broken {
                format!(
                    "Restore the `{}` analysis stage before making a health claim.",
                    stage.name
                )
            } else if stage.hard_invariant {
                format!(
                    "Restore the `{}` invariant before making a health claim.",
                    stage.name
                )
            } else {
                format!(
                    "Inspect the `{}` evidence and corroborate it before refactoring.",
                    stage.name
                )
            },
        })
        .collect()
}

fn tool(
    name: &str,
    status: CoverageStatus,
    owner: Option<&str>,
    note: &str,
    evidence_artifacts: Vec<String>,
) -> ToolCoverage {
    ToolCoverage {
        tool: name.to_owned(),
        status,
        owner: owner.map(str::to_owned),
        note: note.to_owned(),
        evidence_artifacts,
    }
}

fn existing<P: AsRef<Path>>(root: &Path, paths: &[P]) -> Vec<String> {
    paths
        .iter()
        .map(AsRef::as_ref)
        .filter(|path| path.exists())
        .map(|path| relative(root, path))
        .collect()
}

fn excluded_package_patterns(ctx: &Ctx) -> Vec<Pattern> {
    ctx.config
        .architecture
        .filters
        .exclude_crates
        .iter()
        .filter_map(|pattern| Pattern::new(pattern).ok())
        .collect()
}

fn excluded_package_roots(ctx: &Ctx) -> Result<Vec<String>> {
    let patterns = excluded_package_patterns(ctx);
    let mut roots = Vec::new();
    for package in ctx.metadata()?.workspace_packages() {
        if !patterns
            .iter()
            .any(|pattern| pattern.matches(package.name.as_str()))
        {
            continue;
        }
        let root = package_root(package)?;
        roots.push(relative(&ctx.root, &root));
    }
    roots.sort();
    Ok(roots)
}

pub(super) fn revision(root: &Path) -> Revision {
    let Some(head) = head_revision(root) else {
        return Revision {
            directory: "working-tree".to_owned(),
            digest: None,
        };
    };
    let status = git_output(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .unwrap_or_default();
    if status.is_empty() {
        return Revision {
            directory: head,
            digest: None,
        };
    }
    let mut evidence = status.clone();
    evidence.extend(git_output(root, &["diff", "--binary", "HEAD"]).unwrap_or_default());
    let untracked = untracked_paths(&status);
    for path in untracked {
        evidence.extend(path.as_bytes());
        if let Ok(bytes) = fs::read(root.join(&path)) {
            evidence.extend(bytes);
        }
    }
    let digest = stable_digest(&evidence);
    Revision {
        directory: format!("{head}-dirty-{digest}"),
        digest: Some(digest),
    }
}

fn head_revision(root: &Path) -> Option<String> {
    let output = git_output(root, &["rev-parse", "--short=12", "HEAD"])?;
    String::from_utf8(output)
        .ok()
        .map(|revision| revision.trim().to_owned())
        .filter(|revision| !revision.is_empty())
}

fn git_output(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| output.stdout)
}

fn untracked_paths(status: &[u8]) -> BTreeSet<String> {
    status
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(b"?? "))
        .filter_map(|path| String::from_utf8(path.to_vec()).ok())
        .collect()
}

fn stable_digest(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_threshold_is_proportional_and_never_zero() {
        assert_eq!(scaled_threshold(50, 1_000, "crate"), 5);
        assert_eq!(scaled_threshold(1, 100_000, "module"), 1);
        assert_eq!(scaled_threshold(50, 1_000, "workspace"), 100);
    }

    #[test]
    fn dirty_digest_is_stable_for_the_same_content() {
        assert_eq!(stable_digest(b"same"), stable_digest(b"same"));
        assert_ne!(stable_digest(b"same"), stable_digest(b"different"));
    }

    #[test]
    fn dirty_digest_tracks_files_inside_untracked_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let run_git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(temp.path())
                .args(args)
                .status()
                .expect("run git");
            assert!(status.success(), "git command failed: {args:?}");
        };
        run_git(&["init", "--quiet"]);
        run_git(&["config", "user.email", "quality@example.invalid"]);
        run_git(&["config", "user.name", "Quality Test"]);
        fs::write(temp.path().join("tracked"), "tracked").expect("tracked file");
        run_git(&["add", "tracked"]);
        run_git(&["commit", "--quiet", "-m", "baseline"]);
        fs::create_dir_all(temp.path().join("new/nested")).expect("untracked directory");
        let untracked = temp.path().join("new/nested/value.txt");
        fs::write(&untracked, "first").expect("first content");
        let first = revision(temp.path());

        fs::write(&untracked, "second").expect("second content");
        let second = revision(temp.path());

        assert_ne!(first.digest, second.digest);
        assert_ne!(first.directory, second.directory);
    }

    #[test]
    fn standard_depth_marks_heavy_tools_not_applicable() {
        let args = AssessArgs {
            profile: AssessmentProfile::Product,
            depth: AssessmentDepth::Standard,
            krate: None,
            module: None,
            baseline: None,
            reuse_existing: false,
        };
        let mut coverage = Vec::new();

        append_deep_coverage(&mut coverage, &args);

        for tool_name in ["cargo-deny", "cargo-crap", "cargo-mutants", "apple-build"] {
            let entry = coverage
                .iter()
                .find(|entry| entry.tool == tool_name)
                .expect("deep tool coverage");
            assert_eq!(entry.status, CoverageStatus::NotApplicable);
        }
    }

    #[test]
    fn project_policy_marks_deep_tool_not_applicable() {
        let mut coverage = Vec::new();
        append_deep_coverage(
            &mut coverage,
            &AssessArgs {
                profile: AssessmentProfile::Complete,
                depth: AssessmentDepth::Deep,
                krate: None,
                module: None,
                baseline: None,
                reuse_existing: false,
            },
        );

        apply_tool_policy(
            &mut coverage,
            &[QualityAssessmentToolPolicyConfig {
                tool: "cargo-mutants".to_owned(),
                reason: "not actionable for this workspace".to_owned(),
            }],
        );

        let entry = coverage
            .iter()
            .find(|entry| entry.tool == "cargo-mutants")
            .expect("cargo-mutants coverage");
        assert_eq!(entry.status, CoverageStatus::NotApplicable);
        assert_eq!(entry.owner.as_deref(), Some("project-policy"));
        assert_eq!(entry.note, "not actionable for this workspace");
    }

    #[test]
    fn failing_hard_stage_is_a_hard_invariant() {
        let stage = StageEvidence {
            name: "clippy".to_owned(),
            command: vec!["cargo".to_owned(), "clippy".to_owned()],
            tools: vec!["clippy".to_owned()],
            hard_invariant: true,
            status: StageStatus::Findings,
            exit_code: Some(1),
            log: "target/logs/clippy.log".to_owned(),
            evidence_artifacts: Vec::new(),
            note: "stage exited with findings".to_owned(),
        };

        let findings = stage_findings(&[stage], "workspace");

        assert_eq!(findings[0].category, "hard-invariant");
    }
}
