use tempfile::TempDir;

use super::*;
use crate::common::project::{
    PerfLane, TestCargoOptions, TestLaneConfig, TestNetBackendConfig, TestNextestRunner, TestRunner,
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
            let error = run(&request, &project, &temp.path().join("absent-source"))
                .expect_err("collector-owned inputs cannot override the measured source or mode");
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
    let error =
        ensure_source(subject.path(), &commit).expect_err("another clean revision is not admitted");
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
    let error =
        run(&failed, &project, subject.path()).expect_err("failed child cannot inherit a report");
    assert!(
        error.to_string().contains("performance test invocation"),
        "{error:#}"
    );
    let failed_paths = RunPaths::new(&failed.data_dir, &failed.run_id);
    let dir = failed_paths.repeat_dir(LANE, 1);
    assert!(dir.join("inventory.json").is_file());
    assert!(dir.join("run.log").is_file());
    assert!(!dir.join("junit.xml").exists());
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("meta.json")).expect("failed attempt receipt"))
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
