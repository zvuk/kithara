use std::{
    collections::BTreeSet,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};

use crate::{
    common::project::{PerfConfig, ProjectConfig, StressRenderBudgets},
    consts,
    junit::parse_junit_report,
    perf::{
        evidence::{self, MatrixPlan, PlannedLane, RepeatMeta},
        lanes::{Lane, RunPaths, primary_lane_name, sanitize},
    },
    stress::{run_output, run_stderr_output},
    stress_report::{read_bounded_utf8, read_primary_evidence, validate_inventory},
    test::{LaneChoice, LaneToggles, NextestAction, inventory_args, resolve},
    verdict::ChildFailure,
};

pub(crate) struct MatrixParams {
    pub(crate) data_dir: PathBuf,
    pub(crate) target_root: PathBuf,
    pub(crate) run_id: String,
    pub(crate) extra: Vec<String>,
    pub(crate) lanes: Vec<String>,
    pub(crate) repeats: u32,
}

pub(crate) fn run(params: &MatrixParams, project: &ProjectConfig, root: &Path) -> Result<()> {
    ensure!(
        params.repeats > 0,
        "performance repetitions must be greater than zero"
    );
    validate_arguments(&params.extra)?;
    let root = root
        .canonicalize()
        .context("resolve measurement workspace")?;
    let root = root.as_path();
    evidence::component(&params.run_id)?;
    let lanes = resolve_lanes(&project.perf, &params.lanes)?;
    let commit = git_head(root)?;
    ensure_source(root, &commit)?;
    let config_path = root
        .join(&project.test.nextest_config)
        .canonicalize()
        .context("resolve measurement nextest config")?;
    let config_relative = config_path
        .strip_prefix(root)
        .context("measurement nextest config must belong to the selected source")?;
    let tracked = Command::new("git")
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(config_relative)
        .current_dir(root)
        .output()
        .context("verify tracked nextest config")?;
    ensure!(
        tracked.status.success(),
        "measurement nextest config must be committed"
    );
    let config = read_bounded_utf8(
        &config_path,
        consts::MAX_MANIFEST_BYTES as u64,
        "nextest config",
    )?;
    let mut planned = Vec::new();
    for lane in lanes {
        let runner = resolve(
            &project.test,
            &LaneChoice {
                backend: &lane.backend,
                lane: &project.test.default_lane,
                toggles: LaneToggles {
                    flash: lane.flash,
                    no_block: false,
                    load: false,
                },
            },
        )?;
        let mut arguments = vec!["--profile".to_owned(), project.perf.nextest_profile.clone()];
        arguments.extend(params.extra.iter().cloned());
        arguments.extend(["--stress-count=1".to_owned(), "--retries=0".to_owned()]);
        planned.push(PlannedLane {
            name: lane.name(),
            flash: lane.flash,
            runner,
            arguments,
        });
    }
    let configured_primary = primary_lane_name(&project.perf)?;
    let primary_lane = planned
        .iter()
        .find(|lane| lane.name == configured_primary)
        .or_else(|| planned.first())
        .context("performance matrix has no lanes")?
        .name
        .clone();
    let plan = MatrixPlan {
        format_version: 1,
        run_id: params.run_id.clone(),
        commit,
        host: command_text(root, "hostname", &[])?,
        platform: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        toolchain: command_text(root, "rustc", &["-vV"])?,
        nextest: command_text(root, "cargo-nextest", &["nextest", "--version"])?,
        nextest_config_sha256: evidence::digest(
            &config_path,
            consts::MAX_MANIFEST_BYTES as u64,
            "nextest config",
        )?,
        primary_lane,
        repeats: params.repeats,
        lanes: planned,
    };
    evidence::validate_plan(&plan, &params.run_id)?;
    let data_dir = root.join(&params.data_dir);
    let paths = RunPaths::new(&data_dir, &params.run_id);
    fs::create_dir_all(&data_dir).context("create performance data directory")?;
    fs::create_dir(&paths.run_dir)
        .with_context(|| format!("create fresh measurement run {}", paths.run_dir.display()))?;
    evidence::write_json(&paths.run_dir.join("matrix.json"), &plan)?;
    fs::write(paths.run_dir.join("nextest.toml"), config)
        .context("retain nextest configuration")?;
    fs::create_dir(paths.matrix_dir()).context("create matrix directory")?;
    for lane in &plan.lanes {
        fs::create_dir(paths.matrix_dir().join(&lane.name)).context("create matrix lane")?;
        fs::create_dir_all(paths.profiles_dir(&sanitize(&lane.name)))
            .context("create profile directory")?;
        for repeat in 1..=plan.repeats {
            ensure_source(root, &plan.commit)?;
            run_repeat(root, project, params, &paths, &plan, lane, repeat)?;
        }
    }
    ensure_source(root, &plan.commit)?;
    evidence::read_matrix(&paths, &params.run_id)?;
    println!(
        "[perf matrix] complete selected matrix: {} lanes, {} invocations per lane -> {}",
        plan.lanes.len(),
        plan.repeats,
        paths.run_dir.display()
    );
    Ok(())
}

fn run_repeat(
    root: &Path,
    project: &ProjectConfig,
    params: &MatrixParams,
    paths: &RunPaths,
    plan: &MatrixPlan,
    lane: &PlannedLane,
    repeat: u32,
) -> Result<()> {
    let dir = paths.repeat_dir(&lane.name, repeat);
    fs::create_dir(&dir).context("create fresh matrix repetition")?;
    let junit_path = dir.join("junit.xml");
    let config_path = dir.join("nextest.toml");
    isolated_config(
        &paths.run_dir.join("nextest.toml"),
        &config_path,
        &project.perf.nextest_profile,
        &junit_path,
    )?;
    let mut arguments = vec![
        "--config-file".to_owned(),
        config_path.to_string_lossy().into_owned(),
    ];
    arguments.extend(lane.arguments.iter().cloned());
    let target = root.join(&params.target_root).join(&lane.name);
    let inventory_path = dir.join("inventory.json");
    let mut inventory = lane
        .runner
        .command(NextestAction::List, &inventory_args(&arguments)?)?;
    inventory
        .current_dir(root)
        .env("CARGO_TARGET_DIR", &target)
        .stdout(Stdio::from(
            File::create_new(&inventory_path).context("create exact inventory")?,
        ));
    let prepared = Instant::now();
    let listed = run_stderr_output(&mut inventory, &dir.join("list.log"))?;
    if !listed.success() {
        return Err(ChildFailure::inherited(
            "performance inventory".to_owned(),
            listed.code(),
        ));
    }
    let json = read_bounded_utf8(
        &inventory_path,
        consts::MAX_INVENTORY_BYTES,
        "matrix inventory",
    )?;
    validate_inventory(&json)?;
    let preparation_secs = prepared.elapsed().as_secs_f64();
    let mut command = lane.runner.command(NextestAction::Run, &arguments)?;
    command.current_dir(root).env("CARGO_TARGET_DIR", &target);
    ensure!(
        !junit_path.exists(),
        "fresh invocation already contains a JUnit report"
    );
    println!(
        "[perf matrix] {} rep{repeat}: exact inventory prepared in {preparation_secs:.3}s",
        lane.name
    );
    let started_unix_ms = unix_ms()?;
    let clock = Instant::now();
    let status = run_output(&mut command, &dir.join("run.log"))?;
    let duration_secs = clock.elapsed().as_secs_f64();
    let finished_unix_ms = unix_ms()?;
    let (junit_sha256, nextest_run_id) = if junit_path.is_file() {
        let xml = read_bounded_utf8(&junit_path, consts::MAX_JUNIT_BYTES, "matrix JUnit")?;
        let execution = parse_junit_report(&xml)
            .ok()
            .and_then(|report| report.run_id);
        (
            Some(evidence::digest(
                &junit_path,
                consts::MAX_JUNIT_BYTES,
                "matrix JUnit",
            )?),
            execution,
        )
    } else {
        (None, None)
    };
    let meta = RepeatMeta {
        exit_code: status.code(),
        commit: plan.commit.clone(),
        lane: lane.name.clone(),
        run_id: plan.run_id.clone(),
        features: lane.runner.features.clone(),
        repeat,
        duration_secs,
        preparation_secs,
        started_unix_ms,
        finished_unix_ms,
        nextest_config_sha256: evidence::digest(
            &config_path,
            consts::MAX_MANIFEST_BYTES as u64,
            "invocation config",
        )?,
        inventory_sha256: evidence::digest(
            &inventory_path,
            consts::MAX_INVENTORY_BYTES,
            "matrix inventory",
        )?,
        junit_sha256,
        nextest_run_id,
    };
    evidence::write_json(&dir.join("meta.json"), &meta)?;
    if !status.success() {
        return Err(ChildFailure::inherited(
            "performance test invocation".to_owned(),
            status.code(),
        ));
    }
    read_primary_evidence(
        &inventory_path,
        &junit_path,
        1,
        &StressRenderBudgets::default(),
    )?;
    Ok(())
}

fn validate_arguments(arguments: &[String]) -> Result<()> {
    const CONTROLLED: &[&str] = &[
        "--archive-file",
        "--archive-format",
        "--extract-to",
        "--extract-overwrite",
        "--persist-extract-tempdir",
        "--cargo-metadata",
        "--binaries-metadata",
        "--workspace-remap",
        "--target-dir-remap",
        "--build-dir-remap",
        "--manifest-path",
        "--config",
        "--config-file",
        "--tool-config-file",
        "--profile",
        "-P",
        "--stress-count",
        "--stress-duration",
        "--retries",
        "--no-run",
        "--",
        "--features",
        "-F",
        "--all-features",
        "--no-default-features",
    ];
    for argument in arguments {
        let flag = argument.split('=').next().context("measurement argument")?;
        ensure!(
            !CONTROLLED.contains(&flag)
                && !["-P", "-F"]
                    .iter()
                    .any(|prefix| argument.starts_with(prefix))
                && (!argument.starts_with('-')
                    || argument.starts_with("--")
                    || argument.len() == 2
                    || ["-E", "-p", "-j", "-Z"]
                        .iter()
                        .any(|prefix| argument.starts_with(prefix))),
            "source-bound matrix controls `{flag}`; use its configured lane instead"
        );
    }
    Ok(())
}

fn isolated_config(source: &Path, destination: &Path, profile: &str, junit: &Path) -> Result<()> {
    fn table(value: &mut toml::Value) -> Result<&mut toml::Table> {
        value
            .as_table_mut()
            .context("nextest configuration item must be a table")
    }
    let text = read_bounded_utf8(source, consts::MAX_MANIFEST_BYTES as u64, "nextest config")?;
    let mut config: toml::Table =
        toml::from_str(&text).context("parse measurement nextest config")?;
    let profiles = table(
        config
            .entry("profile")
            .or_insert_with(|| toml::Value::Table(toml::Table::new())),
    )?;
    ensure!(
        matches!(profile, "default" | "default-miri") || profiles.contains_key(profile),
        "configured nextest profile `{profile}` is not declared"
    );
    let selected = table(
        profiles
            .entry(profile)
            .or_insert_with(|| toml::Value::Table(toml::Table::new())),
    )?;
    let junit_config = table(
        selected
            .entry("junit")
            .or_insert_with(|| toml::Value::Table(toml::Table::new())),
    )?;
    junit_config.insert(
        "path".to_owned(),
        toml::Value::String(junit.to_string_lossy().into_owned()),
    );
    let store = table(
        config
            .entry("store")
            .or_insert_with(|| toml::Value::Table(toml::Table::new())),
    )?;
    store.insert(
        "dir".to_owned(),
        toml::Value::String(
            destination
                .with_extension("store")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    fs::write(
        destination,
        toml::to_string(&config).context("serialize isolated nextest config")?,
    )
    .context("write isolated nextest config")
}

fn resolve_lanes(config: &PerfConfig, names: &[String]) -> Result<Vec<Lane>> {
    let configured = Lane::configured(config);
    ensure!(!configured.is_empty(), "perf.lanes is empty");
    let mut unique = BTreeSet::new();
    for name in configured.iter().map(Lane::name) {
        ensure!(unique.insert(name.clone()), "duplicate perf lane `{name}`");
    }
    if names.is_empty() {
        return Ok(configured);
    }
    unique.clear();
    for name in names {
        ensure!(
            unique.insert(name.clone()),
            "duplicate requested perf lane `{name}`"
        );
    }
    names
        .iter()
        .map(|name| {
            Lane::by_name(config, name)
                .with_context(|| format!("unknown performance lane `{name}`"))
        })
        .collect()
}

fn command_text(root: &Path, program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("read measurement provenance from {program}"))?;
    ensure!(
        output.status.success(),
        "measurement provenance command failed: {program}"
    );
    let text = String::from_utf8(output.stdout).context("measurement provenance is not UTF-8")?;
    ensure!(
        !text.trim().is_empty(),
        "measurement provenance is empty: {program}"
    );
    Ok(text.trim().to_owned())
}

fn git_head(root: &Path) -> Result<String> {
    command_text(root, "git", &["rev-parse", "HEAD"])
}

fn ensure_source(root: &Path, expected: &str) -> Result<()> {
    ensure!(
        git_head(root)? == expected,
        "measurement source revision changed"
    );
    let status = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .context("read measurement source status")?;
    ensure!(status.status.success(), "measurement source status failed");
    ensure!(
        status.stdout.is_empty(),
        "matrix measurements require a clean committed source:\n{}",
        String::from_utf8_lossy(&status.stdout)
    );
    Ok(())
}

fn unix_ms() -> Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("clock before epoch")?;
    u64::try_from(elapsed.as_millis()).context("measurement timestamp overflows")
}

#[cfg(test)]
mod collector_tests {
    use tempfile::TempDir;

    use super::*;
    use crate::common::project::{
        PerfLane, TestCargoOptions, TestLaneConfig, TestNetBackendConfig, TestNextestRunner,
        TestRunner,
    };

    const LANE: &str = "flash-off-none";
    const CONFIG: &str = r#"
[store]
dir = "target/nextest"
[profile.default]
retries = 2
test-threads = 1
slow-timeout = { period = "9s", terminate-after = 3 }
[profile.default.junit]
path = "default.xml"
store-success-output = true
[profile.perf]
retries = 2
test-threads = 1
fail-fast = false
[profile.perf.junit]
path = "junit.xml"
store-failure-output = true
"#;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Matrix Fixture",
                "-c",
                "user.email=matrix@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(root)
            .output()
            .expect("fixture git command");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fixture() -> (TempDir, ProjectConfig) {
        let temp = tempfile::tempdir().expect("matrix subject");
        fs::create_dir(temp.path().join(".config")).expect("subject configuration");
        fs::write(temp.path().join(".config/nextest.toml"), CONFIG).expect("nextest policy");
        fs::write(temp.path().join(".gitignore"), "/target/\n").expect("ignore local build output");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"matrix_subject\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
         [workspace]\n[lib]\npath = \"lib.rs\"\n",
        )
        .expect("subject manifest");
        fs::write(
            temp.path().join("lib.rs"),
            "#[test]\nfn passes() { assert_eq!(2 + 2, 4); }\n",
        )
        .expect("subject test");
        let lock = Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(temp.path())
            .output()
            .expect("generate subject lockfile");
        assert!(
            lock.status.success(),
            "{}",
            String::from_utf8_lossy(&lock.stderr)
        );
        git(temp.path(), &["init", "--quiet"]);
        git(temp.path(), &["add", "."]);
        git(temp.path(), &["commit", "--quiet", "-m", "fixture"]);
        let mut project = ProjectConfig::default();
        project.perf.nextest_profile = "perf".to_owned();
        project.perf.lanes.push(PerfLane {
            backend: "none".to_owned(),
            flash: false,
        });
        project.test.default_lane = "workspace".to_owned();
        project.test.default_backend = "none".to_owned();
        project.test.nextest_config = ".config/nextest.toml".to_owned();
        project
            .test
            .net_backends
            .insert("none".to_owned(), TestNetBackendConfig::default());
        project.test.lanes.insert(
            "workspace".to_owned(),
            TestLaneConfig {
                cargo: TestCargoOptions {
                    workspace: true,
                    lib: true,
                    ..TestCargoOptions::default()
                },
                runner: TestRunner::Nextest(TestNextestRunner {
                    test_threads: Some(1),
                    ..TestNextestRunner::default()
                }),
                ..TestLaneConfig::default()
            },
        );
        (temp, project)
    }

    fn params(output: &Path, run_id: &str) -> MatrixParams {
        MatrixParams {
            data_dir: output.join("data"),
            target_root: output.join("targets"),
            run_id: run_id.to_owned(),
            extra: vec!["--offline".to_owned()],
            lanes: vec![LANE.to_owned()],
            repeats: 1,
        }
    }

    #[test]
    fn controlled_inputs_are_rejected_before_source_or_output_access() {
        let temp = tempfile::tempdir().expect("argument fixture");
        let project = ProjectConfig::default();
        for flag in [
            "--archive-file",
            "--archive-format",
            "--extract-to",
            "--extract-overwrite",
            "--persist-extract-tempdir",
            "--cargo-metadata",
            "--binaries-metadata",
            "--workspace-remap",
            "--target-dir-remap",
            "--build-dir-remap",
            "--manifest-path",
            "--config",
            "--config-file",
            "--tool-config-file",
            "--profile",
            "-P",
            "--stress-count",
            "--stress-duration",
            "--retries",
            "--no-run",
            "--features",
            "-F",
            "--all-features",
            "--no-default-features",
        ] {
            for arguments in [
                vec![flag.to_owned(), "foreign".to_owned()],
                vec![format!("{flag}=foreign")],
            ] {
                let mut request = params(temp.path(), "rejected");
                request.extra = arguments;
                let error = run(&request, &project, &temp.path().join("absent-source")).expect_err(
                    "collector-owned inputs cannot override the measured source or mode",
                );
                assert!(
                    error.to_string().contains("source-bound matrix controls"),
                    "{flag}: {error:#}"
                );
                assert!(!request.data_dir.exists());
                assert!(!request.target_root.exists());
            }
        }
        for argument in ["-Pforeign", "-Fforeign", "-vFflash", "-vPforeign", "--"] {
            assert!(
                validate_arguments(&[argument.to_owned()]).is_err(),
                "{argument}"
            );
        }
        validate_arguments(&[
            "--offline".to_owned(),
            "-p".to_owned(),
            "subject".to_owned(),
            "--lib".to_owned(),
            "-E".to_owned(),
            "test(=passes)".to_owned(),
        ])
        .expect("ordinary build and test selection remains available");
    }

    #[test]
    fn isolated_reports_preserve_policy_and_cannot_share_a_destination() {
        let temp = tempfile::tempdir().expect("configuration fixture");
        let source = temp.path().join("source.toml");
        fs::write(&source, CONFIG).expect("source policy");
        let original: toml::Table = toml::from_str(CONFIG).expect("parse source policy");
        let mut reports = BTreeSet::new();
        let mut stores = BTreeSet::new();
        for invocation in ["first", "second"] {
            let dir = temp.path().join(invocation);
            fs::create_dir(&dir).expect("invocation directory");
            let destination = dir.join("nextest.toml");
            let junit = dir.join("junit.xml");
            isolated_config(&source, &destination, "perf", &junit).expect("isolate report policy");
            let actual: toml::Table =
                toml::from_str(&fs::read_to_string(&destination).expect("isolated policy"))
                    .expect("parse isolated policy");
            let mut expected = original.clone();
            expected["profile"]["perf"]["junit"]["path"] =
                toml::Value::String(junit.to_string_lossy().into_owned());
            expected["store"]["dir"] = toml::Value::String(
                destination
                    .with_extension("store")
                    .to_string_lossy()
                    .into_owned(),
            );
            assert_eq!(actual, expected);
            reports.insert(
                actual["profile"]["perf"]["junit"]["path"]
                    .as_str()
                    .expect("report path")
                    .to_owned(),
            );
            stores.insert(
                actual["store"]["dir"]
                    .as_str()
                    .expect("store path")
                    .to_owned(),
            );
        }
        assert_eq!(reports.len(), 2);
        assert_eq!(stores.len(), 2);
        assert_eq!(
            fs::read_to_string(source).expect("retained source policy"),
            CONFIG
        );
    }

    #[test]
    fn output_isolation_rejects_missing_custom_profiles() {
        let temp = tempfile::tempdir().expect("profile fixture");
        let source = temp.path().join("source.toml");
        fs::write(&source, CONFIG).expect("source configuration");
        for (profile, valid) in [
            ("default", true),
            ("default-miri", true),
            ("perf", true),
            ("perff", false),
        ] {
            let destination = temp.path().join(format!("{profile}.toml"));
            let result = isolated_config(
                &source,
                &destination,
                profile,
                &temp.path().join("junit.xml"),
            );
            assert_eq!(result.is_ok(), valid, "{profile}: {result:?}");
            assert_eq!(destination.exists(), valid);
        }
        assert_eq!(
            fs::read_to_string(source).expect("retained source configuration"),
            CONFIG
        );
    }

    #[test]
    fn source_must_remain_clean_and_at_the_recorded_revision() {
        let (subject, _project) = fixture();
        let commit = git_head(subject.path()).expect("accepted subject revision");
        ensure_source(subject.path(), &commit).expect("clean committed subject");
        git(
            subject.path(),
            &["config", "status.showUntrackedFiles", "no"],
        );
        fs::write(subject.path().join("untracked.rs"), "changed\n").expect("untracked source");
        let error =
            ensure_source(subject.path(), &commit).expect_err("untracked source is not admitted");
        assert!(error.to_string().contains("clean committed source"));
        git(subject.path(), &["add", "untracked.rs"]);
        git(
            subject.path(),
            &["commit", "--quiet", "-m", "changed subject"],
        );
        let error = ensure_source(subject.path(), &commit)
            .expect_err("another clean revision is not admitted");
        assert!(error.to_string().contains("source revision changed"));
    }

    #[test]
    fn real_collector_binds_selected_workspace_and_preserves_failed_attempts() {
        let (subject, project) = fixture();
        let output = tempfile::tempdir().expect("external measurement output");
        let mut request = params(output.path(), "complete");
        request.repeats = 2;
        run(&request, &project, subject.path()).expect("complete selected fixture matrix");
        let paths = RunPaths::new(&request.data_dir, &request.run_id);
        let (plan, runs) = evidence::read_matrix(&paths, &request.run_id).expect("bound matrix");
        assert_eq!(
            plan.commit,
            git_head(subject.path()).expect("subject revision")
        );
        assert_eq!(runs.len(), 2);
        assert!(
            runs.iter()
                .all(|(lane, cases)| lane == LANE && cases.len() == 1 && cases[0].name == "passes")
        );
        let retained_plan = fs::read(paths.run_dir.join("matrix.json")).expect("original plan");
        let error =
            run(&request, &project, subject.path()).expect_err("existing run cannot be replaced");
        assert!(
            error.to_string().contains("fresh measurement run"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(paths.run_dir.join("matrix.json")).expect("retained plan"),
            retained_plan
        );
        evidence::read_matrix(&paths, &request.run_id).expect("original evidence remains valid");

        let shared = subject.path().join("target/nextest/perf/junit.xml");
        fs::create_dir_all(shared.parent().expect("shared report parent"))
            .expect("shared report directory");
        fs::write(&shared, "stale unrelated report").expect("unrelated shared report");
        let mut failed = params(output.path(), "failed-before-report");
        failed
            .extra
            .push("--status-level=invalid-matrix-status".to_owned());
        let error = run(&failed, &project, subject.path())
            .expect_err("failed child cannot inherit a report");
        assert!(
            error.to_string().contains("performance test invocation"),
            "{error:#}"
        );
        let failed_paths = RunPaths::new(&failed.data_dir, &failed.run_id);
        let dir = failed_paths.repeat_dir(LANE, 1);
        assert!(dir.join("inventory.json").is_file());
        assert!(dir.join("run.log").is_file());
        assert!(!dir.join("junit.xml").exists());
        let receipt: serde_json::Value = serde_json::from_slice(
            &fs::read(dir.join("meta.json")).expect("failed attempt receipt"),
        )
        .expect("parse failed attempt receipt");
        assert_ne!(receipt["exit_code"], serde_json::json!(0));
        assert!(receipt["junit_sha256"].is_null());
        assert!(receipt["nextest_run_id"].is_null());
        assert!(evidence::read_matrix(&failed_paths, &failed.run_id).is_err());
        assert_eq!(
            fs::read_to_string(shared).expect("shared report retained"),
            "stale unrelated report"
        );
        ensure_source(subject.path(), &plan.commit)
            .expect("collector used only external build and output directories");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::project::PerfLane;

    #[test]
    fn duplicate_requested_lanes_are_rejected() {
        let config = PerfConfig {
            lanes: vec![PerfLane {
                backend: "http".to_owned(),
                flash: true,
            }],
            ..PerfConfig::default()
        };
        let names = vec!["flash-on-http".to_owned(); 2];

        assert!(resolve_lanes(&config, &names).is_err());
    }

    #[test]
    fn zero_repetitions_are_rejected_before_execution() {
        let temp = tempfile::tempdir().expect("matrix fixture");
        let params = MatrixParams {
            data_dir: temp.path().to_owned(),
            target_root: temp.path().join("targets"),
            run_id: "zero".to_owned(),
            extra: Vec::new(),
            lanes: vec!["flash-on-http".to_owned()],
            repeats: 0,
        };
        let mut project = ProjectConfig::default();
        project.perf.lanes.push(PerfLane {
            backend: "http".to_owned(),
            flash: true,
        });

        let error =
            run(&params, &project, temp.path()).expect_err("zero repeats cannot measure a suite");

        assert_eq!(
            error.to_string(),
            "performance repetitions must be greater than zero"
        );
        assert!(!RunPaths::new(temp.path(), "zero").matrix_dir().exists());
    }
}
