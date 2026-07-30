use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};

use anyhow::{Context, Result};
use glob::Pattern;

use super::{
    AssessArgs, AssessmentDepth, AssessmentProfile,
    model::{StageEvidence, StageStatus},
};
use crate::{
    Ctx,
    common::project::QualityAssessmentStageConfig,
    stages::{SharedStage, StageCommand},
};

#[derive(Clone, Debug)]
struct StagePlan {
    name: String,
    command: Vec<String>,
    tools: Vec<String>,
    expected_artifacts: Vec<PathBuf>,
    hard_invariant: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArtifactStamp {
    modified: Option<SystemTime>,
    len: u64,
}

pub(super) fn refresh(args: &AssessArgs, ctx: &Ctx, output: &Path) -> Result<Vec<StageEvidence>> {
    let logs = output.join("logs");
    let stages = output.join("stages");
    fs::create_dir_all(&logs)
        .with_context(|| format!("create assessment logs: {}", logs.display()))?;
    fs::create_dir_all(&stages)
        .with_context(|| format!("create assessment stages: {}", stages.display()))?;
    let plans = plan(args, ctx)?;
    let mut evidence = Vec::with_capacity(plans.len());
    for stage in plans {
        let result = execute(&stage, &ctx.root, &logs)?;
        write_stage(&stages, &result)?;
        evidence.push(result);
    }
    Ok(evidence)
}

pub(super) fn reuse(output: &Path) -> Result<Vec<StageEvidence>> {
    let directory = output.join("stages");
    if !directory.is_dir() {
        return Ok(Vec::new());
    }

    let mut paths = fs::read_dir(&directory)
        .with_context(|| format!("read assessment stages: {}", directory.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .with_context(|| format!("read assessment stage entry: {}", directory.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let json = fs::read(&path)
                .with_context(|| format!("read assessment stage: {}", path.display()))?;
            serde_json::from_slice(&json)
                .with_context(|| format!("parse assessment stage: {}", path.display()))
        })
        .collect()
}

pub(super) fn record_revision_drift(
    root: &Path,
    output: &Path,
    before: &str,
    after: &str,
) -> Result<StageEvidence> {
    let log_path = output.join("logs/working-tree-stability.log");
    fs::write(
        &log_path,
        format!("working tree changed during assessment\nbefore: {before}\nafter: {after}\n"),
    )
    .with_context(|| format!("write assessment integrity log: {}", log_path.display()))?;
    let evidence = StageEvidence {
        name: "working-tree-stability".to_owned(),
        command: vec!["internal:working-tree-stability".to_owned()],
        tools: vec!["assessment-integrity".to_owned()],
        hard_invariant: false,
        status: StageStatus::Broken,
        exit_code: None,
        log: relative(root, &log_path),
        evidence_artifacts: Vec::new(),
        note: "the assessed content changed while analyzers were running".to_owned(),
    };
    write_stage(&output.join("stages"), &evidence)?;
    Ok(evidence)
}

fn plan(args: &AssessArgs, ctx: &Ctx) -> Result<Vec<StagePlan>> {
    let head = revision(&ctx.root).unwrap_or_else(|| "working-tree".to_owned());
    let full_revision = full_revision(&ctx.root).unwrap_or_else(|| head.clone());
    let mut stages = if args.depth == AssessmentDepth::Deep {
        let mut stages = vec![health_plan(ctx)];
        stages.extend(supplemental_plans(ctx, false));
        stages
    } else {
        standard_plans(ctx)
    };
    stages.push(similarity_plan(args, ctx, &head)?);
    stages.push(architecture_plan(args, ctx, &head)?);
    if args.depth == AssessmentDepth::Deep {
        for configured in &ctx.config.quality.assessment.deep_stages {
            if configured.complete_only && args.profile != AssessmentProfile::Complete
                || !platform_matches(&configured.platforms)
            {
                continue;
            }
            stages.push(configured_plan(
                configured,
                &ctx.root,
                &full_revision,
                &head,
            ));
        }
    }
    Ok(stages)
}

fn health_plan(ctx: &Ctx) -> StagePlan {
    StagePlan {
        name: "health".to_owned(),
        command: vec!["cargo".to_owned(), "xtask".to_owned(), "health".to_owned()],
        tools: vec![
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
            "arch-lint",
            "style-lint",
            "idioms-lint",
            "cargo-modules-orphans",
            "cargo-workspace-unused-pub",
            "cargo-deny",
            "cargo-machete",
            "cargo-shear",
            "cargo-hack",
            "cargo-semver-checks",
            "cargo-geiger",
            "quality-report",
            "workspace-tests",
            "cargo-nextest",
            "doc-tests",
            "lockbud",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        expected_artifacts: vec![
            ctx.root.join("target/health-report.md"),
            ctx.root.join("target/quality-report.md"),
        ],
        hard_invariant: true,
    }
}

fn standard_plans(ctx: &Ctx) -> Vec<StagePlan> {
    let mut stages = vec![
        shared_plan(
            "format-check",
            SharedStage::FmtCheck,
            &[
                "rustfmt",
                "cargo-sort",
                "taplo",
                "tidy-json",
                "md-formatter",
            ],
        ),
        shared_plan("clippy", SharedStage::Clippy, &["cargo-check", "clippy"]),
        shared_plan("ast-grep", SharedStage::AstGrep, &["ast-grep"]),
        shared_plan(
            "xtask-lint",
            SharedStage::Lint,
            &["arch-lint", "style-lint", "idioms-lint"],
        ),
        shared_plan("typos", SharedStage::Typos, &["typos"]),
        shared_plan("orphans", SharedStage::Orphans, &["cargo-modules-orphans"]),
    ];
    stages.extend(supplemental_plans(ctx, true));
    stages.extend([
        direct_plan(
            "workspace-unused-pub",
            &["cargo", "workspace-unused-pub"],
            &["cargo-workspace-unused-pub"],
            &[],
            true,
        ),
        direct_plan(
            "workspace-tests",
            &["cargo", "xtask", "test", "--lane=workspace"],
            &["workspace-tests", "cargo-nextest"],
            &[],
            true,
        ),
        direct_plan(
            "doc-tests",
            &["cargo", "xtask", "test", "--lane=doc"],
            &["doc-tests", "rustdoc"],
            &[],
            true,
        ),
    ]);
    stages
}

fn supplemental_plans(ctx: &Ctx, include_report: bool) -> Vec<StagePlan> {
    let mut stages = vec![
        direct_plan(
            "manifest-hygiene",
            &["cargo", "xtask", "manifest", "dependency-order"],
            &["manifest-hygiene"],
            &[],
            true,
        ),
        direct_plan(
            "rstest-audit",
            &["cargo", "xtask", "quality", "rstest-audit"],
            &["rstest-audit"],
            &[],
            true,
        ),
        direct_plan(
            "unimock-check",
            &["cargo", "xtask", "quality", "unimock-check"],
            &["unimock-check"],
            &[],
            true,
        ),
        direct_plan(
            "trait-mock-audit",
            &["cargo", "xtask", "quality", "trait-mock-audit"],
            &["trait-mock-audit"],
            &[],
            true,
        ),
        direct_plan(
            "trait-mock-exceptions",
            &["cargo", "xtask", "quality", "trait-mock-exceptions"],
            &["trait-mock-exceptions"],
            &[],
            false,
        ),
    ];
    if include_report {
        stages.push(direct_plan(
            "quality-report",
            &["cargo", "xtask", "quality", "report"],
            &["quality-report"],
            &[ctx.root.join("target/quality-report.md")],
            true,
        ));
    }
    stages
}

fn shared_plan(name: &str, stage: SharedStage, tools: &[&str]) -> StagePlan {
    let StageCommand { program, args } = stage.health_command();
    let command = std::iter::once(program.to_owned()).chain(args).collect();
    StagePlan {
        name: name.to_owned(),
        command,
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
        expected_artifacts: Vec::new(),
        hard_invariant: true,
    }
}

fn direct_plan(
    name: &str,
    command: &[&str],
    tools: &[&str],
    expected_artifacts: &[PathBuf],
    hard_invariant: bool,
) -> StagePlan {
    StagePlan {
        name: name.to_owned(),
        command: command.iter().map(|part| (*part).to_owned()).collect(),
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
        expected_artifacts: expected_artifacts.to_vec(),
        hard_invariant,
    }
}

fn architecture_plan(args: &AssessArgs, ctx: &Ctx, revision: &str) -> Result<StagePlan> {
    let mut command = vec![
        "cargo".to_owned(),
        "xtask".to_owned(),
        "viz".to_owned(),
        "--semantic".to_owned(),
        "off".to_owned(),
        "--runtime".to_owned(),
        "off".to_owned(),
    ];
    if args.profile == AssessmentProfile::Complete || args.krate.is_some() || args.module.is_some()
    {
        command.push("--include-default-excluded".to_owned());
    }
    if let Some(krate) = args.krate.as_deref() {
        command.extend(["--crate".to_owned(), krate.to_owned()]);
    }
    if let Some(module) = args.module.as_deref() {
        let (krate, module) = module
            .split_once("::")
            .ok_or_else(|| anyhow::anyhow!("module scope must be `package::module`"))?;
        command.extend([
            "--crate".to_owned(),
            krate.to_owned(),
            "--module".to_owned(),
            module.to_owned(),
        ]);
    }
    Ok(StagePlan {
        name: "architecture".to_owned(),
        command,
        tools: vec!["architecture".to_owned(), "cargo-metadata".to_owned()],
        expected_artifacts: vec![
            ctx.root
                .join("target/architecture")
                .join(revision)
                .join("manifest.json"),
        ],
        hard_invariant: false,
    })
}

fn similarity_plan(args: &AssessArgs, ctx: &Ctx, revision: &str) -> Result<StagePlan> {
    let mut command = vec![
        "cargo".to_owned(),
        "xtask".to_owned(),
        "similarity".to_owned(),
        "--profile".to_owned(),
        if args.profile == AssessmentProfile::Complete {
            "strict"
        } else {
            "advisory"
        }
        .to_owned(),
    ];
    if args.profile == AssessmentProfile::Complete || args.krate.is_some() || args.module.is_some()
    {
        command.push("--include-default-excluded".to_owned());
    }
    command.extend(similarity_roots(args, ctx)?);
    Ok(StagePlan {
        name: "similarity".to_owned(),
        command,
        tools: vec!["similarity-native".to_owned(), "similarity-rs".to_owned()],
        expected_artifacts: vec![
            ctx.root
                .join("target/similarity")
                .join(revision)
                .join("manifest.json"),
        ],
        hard_invariant: false,
    })
}

pub(super) fn similarity_roots(args: &AssessArgs, ctx: &Ctx) -> Result<Vec<String>> {
    if let Some(krate) = args.krate.as_deref() {
        return Ok(vec![package_scan_root(
            ctx,
            krate,
            args.profile == AssessmentProfile::Complete,
        )?]);
    }
    if let Some(module) = args.module.as_deref() {
        let (krate, module) = module
            .split_once("::")
            .ok_or_else(|| anyhow::anyhow!("module scope must be `package::module`"))?;
        let source = package_source(ctx, krate)?;
        let relative = format!("{source}/{}", module.replace("::", "/"));
        let file = format!("{relative}.rs");
        return Ok(vec![if ctx.root.join(&file).is_file() {
            file
        } else {
            relative
        }]);
    }
    let complete = args.profile == AssessmentProfile::Complete;
    let patterns = if complete {
        Vec::new()
    } else {
        ctx.config
            .architecture
            .filters
            .exclude_crates
            .iter()
            .map(|pattern| {
                Pattern::new(pattern)
                    .with_context(|| format!("invalid architecture exclusion glob `{pattern}`"))
            })
            .collect::<Result<Vec<_>>>()?
    };
    let mut roots = ctx
        .metadata()?
        .workspace_packages()
        .into_iter()
        .filter(|package| {
            !patterns
                .iter()
                .any(|pattern| pattern.matches(package.name.as_str()))
        })
        .map(|package| package_scan_root(ctx, package.name.as_str(), complete))
        .collect::<Result<Vec<_>>>()?;
    roots.retain(|root| ctx.root.join(root).is_dir());
    roots.sort();
    roots.dedup();
    Ok(roots)
}

fn package_source(ctx: &Ctx, package_name: &str) -> Result<String> {
    package_scan_root(ctx, package_name, false)
}

fn package_scan_root(ctx: &Ctx, package_name: &str, include_tests: bool) -> Result<String> {
    let package = ctx
        .metadata()?
        .workspace_packages()
        .into_iter()
        .find(|package| package.name == package_name)
        .ok_or_else(|| anyhow::anyhow!("workspace package not found: {package_name}"))?;
    let root = package
        .manifest_path
        .parent()
        .context("package manifest has no parent")?
        .as_std_path();
    let scan_root = if include_tests {
        root.to_path_buf()
    } else {
        root.join("src")
    };
    Ok(scan_root
        .strip_prefix(&ctx.root)
        .unwrap_or(&scan_root)
        .to_string_lossy()
        .replace('\\', "/"))
}

fn configured_plan(
    config: &QualityAssessmentStageConfig,
    root: &Path,
    revision: &str,
    short_revision: &str,
) -> StagePlan {
    StagePlan {
        name: config.name.clone(),
        command: config
            .command
            .iter()
            .map(|part| expand(part, revision, short_revision))
            .collect(),
        tools: config.tools.clone(),
        expected_artifacts: config
            .expected_artifacts
            .iter()
            .map(|artifact| root.join(expand(artifact, revision, short_revision)))
            .collect(),
        hard_invariant: config.hard_invariant,
    }
}

fn expand(value: &str, revision: &str, short_revision: &str) -> String {
    value
        .replace("{revision}", revision)
        .replace("{short_revision}", short_revision)
}

fn execute(plan: &StagePlan, root: &Path, logs: &Path) -> Result<StageEvidence> {
    let log_path = logs.join(format!("{}.log", plan.name));
    let before = plan
        .expected_artifacts
        .iter()
        .map(|artifact| artifact_stamp(artifact))
        .collect::<Vec<_>>();
    let stdout = fs::File::create(&log_path)
        .with_context(|| format!("create assessment log: {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("clone assessment log: {}", log_path.display()))?;
    let program = plan
        .command
        .first()
        .context("assessment stage command is empty")?;
    let status = Command::new(program)
        .args(&plan.command[1..])
        .current_dir(root)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status();
    let evidence_artifacts = plan
        .expected_artifacts
        .iter()
        .zip(&before)
        .filter(|(artifact, stamp)| artifact_is_fresh(artifact, **stamp))
        .map(|(artifact, _)| relative(root, artifact))
        .collect::<Vec<_>>();
    let expected_complete = plan.expected_artifacts.is_empty()
        || evidence_artifacts.len() == plan.expected_artifacts.len();
    let environment_failure = fs::read_to_string(&log_path).ok().is_some_and(|log| {
        [
            "no such command:",
            "command not found",
            "is not installed",
            "not found in registry",
        ]
        .iter()
        .any(|marker| log.to_ascii_lowercase().contains(marker))
    });
    let (stage_status, exit_code, note) = match status {
        Ok(status) if status.success() && expected_complete => (
            StageStatus::Clean,
            status.code(),
            "stage completed".to_owned(),
        ),
        Ok(status) if environment_failure => (
            StageStatus::Broken,
            status.code(),
            "required analyzer is unavailable".to_owned(),
        ),
        Ok(status) if expected_complete => (
            StageStatus::Findings,
            status.code(),
            format!(
                "stage exited with {:?}; evidence was preserved",
                status.code()
            ),
        ),
        Ok(status) => (
            StageStatus::Broken,
            status.code(),
            "stage did not produce every required artifact".to_owned(),
        ),
        Err(error) => (
            StageStatus::Broken,
            None,
            format!("stage could not start: {error}"),
        ),
    };
    Ok(StageEvidence {
        name: plan.name.clone(),
        command: plan.command.clone(),
        tools: plan.tools.clone(),
        hard_invariant: plan.hard_invariant,
        status: stage_status,
        exit_code,
        log: relative(root, &log_path),
        evidence_artifacts,
        note,
    })
}

fn artifact_stamp(path: &Path) -> Option<ArtifactStamp> {
    let metadata = path.metadata().ok()?;
    Some(ArtifactStamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

fn artifact_is_fresh(path: &Path, before: Option<ArtifactStamp>) -> bool {
    let Some(after) = artifact_stamp(path) else {
        return false;
    };
    before.is_none_or(|before| before != after)
}

fn write_stage(directory: &Path, stage: &StageEvidence) -> Result<()> {
    let path = directory.join(format!("{}.json", stage.name));
    let json = serde_json::to_string_pretty(stage).context("serialize assessment stage")?;
    fs::write(&path, format!("{json}\n"))
        .with_context(|| format!("write assessment stage: {}", path.display()))
}

fn platform_matches(platforms: &[String]) -> bool {
    platforms.is_empty()
        || platforms
            .iter()
            .any(|platform| platform == std::env::consts::OS)
}

fn revision(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|revision| revision.trim().to_owned())
        .filter(|revision| !revision.is_empty())
}

fn full_revision(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
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
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn context(config: &str) -> (tempfile::TempDir, Ctx) {
        let temp = tempdir().expect("tempdir");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"demo\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        fs::create_dir_all(temp.path().join("demo/src")).expect("source directory");
        fs::write(
            temp.path().join("demo/Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("package manifest");
        fs::write(temp.path().join("demo/src/lib.rs"), "pub struct Demo;\n").expect("source");
        fs::create_dir_all(temp.path().join(".config")).expect("config directory");
        fs::write(temp.path().join(".config/xtask.toml"), config).expect("xtask config");
        let ctx = Ctx::load_from_manifest(&temp.path().join("Cargo.toml")).expect("context");
        (temp, ctx)
    }

    #[test]
    fn deep_complete_plan_adds_configured_stages_without_operational_commands() {
        let (_temp, ctx) = context(
            r#"
[[quality.assessment.deep_stages]]
name = "mutation"
command = ["just", "test", "mutants", "ci", "target/mutants"]
tools = ["cargo-mutants"]
complete_only = true
"#,
        );
        let args = AssessArgs {
            profile: AssessmentProfile::Complete,
            depth: AssessmentDepth::Deep,
            krate: None,
            module: None,
            baseline: None,
            reuse_existing: false,
        };

        let plan = plan(&args, &ctx).expect("assessment plan");
        let names = plan
            .iter()
            .map(|stage| stage.name.as_str())
            .collect::<Vec<_>>();
        let commands = plan
            .iter()
            .flat_map(|stage| stage.command.iter())
            .map(String::as_str)
            .collect::<Vec<_>>();

        assert!(names.contains(&"health"));
        assert!(names.contains(&"architecture"));
        assert!(names.contains(&"manifest-hygiene"));
        assert!(names.contains(&"rstest-audit"));
        assert!(names.contains(&"mutation"));
        assert!(names.contains(&"similarity"));
        assert!(!names.contains(&"workspace-tests"));
        let similarity = plan
            .iter()
            .find(|stage| stage.name == "similarity")
            .expect("similarity stage");
        assert!(similarity.command.iter().any(|part| part == "demo"));
        assert!(
            !commands.iter().any(|argument| {
                matches!(*argument, "install" | "release" | "publish" | "open")
            })
        );
    }

    #[test]
    fn product_plan_runs_a_profile_specific_similarity_scan() {
        let (_temp, ctx) = context("");
        let args = AssessArgs {
            profile: AssessmentProfile::Product,
            depth: AssessmentDepth::Standard,
            krate: None,
            module: None,
            baseline: None,
            reuse_existing: false,
        };

        let plan = plan(&args, &ctx).expect("assessment plan");
        let names = plan
            .iter()
            .map(|stage| stage.name.as_str())
            .collect::<Vec<_>>();
        let similarity = plan
            .iter()
            .find(|stage| stage.name == "similarity")
            .expect("similarity stage");

        assert!(!names.contains(&"health"));
        assert!(names.contains(&"format-check"));
        assert!(names.contains(&"workspace-tests"));
        assert!(names.contains(&"quality-report"));
        assert!(
            similarity
                .command
                .windows(2)
                .any(|pair| pair == ["--profile", "advisory"])
        );
        assert!(similarity.command.iter().any(|part| part == "demo/src"));
        assert!(!similarity.hard_invariant);
    }

    #[test]
    fn a_new_expected_artifact_is_fresh() {
        let temp = tempdir().expect("tempdir");
        let artifact = temp.path().join("report.json");

        assert!(!artifact_is_fresh(&artifact, artifact_stamp(&artifact)));
        fs::write(&artifact, "{}").expect("artifact");
        assert!(artifact_is_fresh(&artifact, None));
    }

    #[test]
    fn reuse_reads_saved_stages_in_stable_order() {
        let temp = tempdir().expect("tempdir");
        let directory = temp.path().join("stages");
        fs::create_dir_all(&directory).expect("stages directory");
        for name in ["zeta", "alpha"] {
            write_stage(
                &directory,
                &StageEvidence {
                    name: name.to_owned(),
                    command: vec!["just".to_owned(), name.to_owned()],
                    tools: vec![name.to_owned()],
                    hard_invariant: false,
                    status: StageStatus::Findings,
                    exit_code: Some(1),
                    log: format!("logs/{name}.log"),
                    evidence_artifacts: vec![format!("target/{name}.json")],
                    note: "saved evidence".to_owned(),
                },
            )
            .expect("write stage");
        }

        let stages = reuse(temp.path()).expect("reuse stages");
        let names = stages
            .iter()
            .map(|stage| stage.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["alpha", "zeta"]);
        assert_eq!(stages[0].status, StageStatus::Findings);
        assert_eq!(stages[0].evidence_artifacts, ["target/alpha.json"]);
    }

    #[test]
    fn reuse_rejects_malformed_stage_evidence() {
        let temp = tempdir().expect("tempdir");
        let directory = temp.path().join("stages");
        fs::create_dir_all(&directory).expect("stages directory");
        fs::write(directory.join("broken.json"), "{").expect("broken stage");

        let error = reuse(temp.path()).expect_err("malformed stage must fail");

        assert!(error.to_string().contains("parse assessment stage"));
        assert!(error.to_string().contains("broken.json"));
    }
}
