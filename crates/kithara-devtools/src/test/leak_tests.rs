use std::{
    fs::{self, File},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use crate::{
    common::project::StressRenderBudgets,
    junit::parse_junit,
    stress_report::{self, StressReportArgs},
    test::repository_tests::root,
};

#[test]
fn every_declared_leak_timeout_fails_the_test() {
    let config: toml::Value = toml::from_str(
        &fs::read_to_string(root().join(".config/nextest.toml")).expect("nextest configuration"),
    )
    .expect("nextest TOML");
    let profiles = config["profile"].as_table().expect("nextest profiles");
    let mut checked = 0;
    for (name, profile) in profiles {
        let Some(timeout) = profile.get("leak-timeout") else {
            continue;
        };
        assert_eq!(
            timeout.get("result").and_then(toml::Value::as_str),
            Some("fail"),
            "profile `{name}` must fail tests whose output handles leak past its timeout"
        );
        checked += 1;
    }
    assert!(checked > 0, "a leak timeout policy must be declared");
}

#[test]
fn leaked_output_fails_the_lane_and_stress_report() {
    let temp = tempfile::tempdir().expect("tempdir");
    let fixture = crate::testing::package().join("tests/fixtures/nextest-leak");
    for name in ["Cargo.toml", "leak.rs"] {
        fs::copy(fixture.join(name), temp.path().join(name)).expect("copy leak fixture");
    }
    let cargo = std::env::var_os("CARGO").expect("cargo supplied by the test runner");
    let repository: toml::Value = toml::from_str(
        &fs::read_to_string(root().join(".config/nextest.toml")).expect("nextest configuration"),
    )
    .expect("nextest TOML");
    let mut stress = repository["profile"]["stress"]
        .as_table()
        .expect("stress profile")
        .clone();
    stress.remove("overrides");
    let profiles = toml::Table::from_iter([("stress".to_owned(), toml::Value::Table(stress))]);
    let isolated = toml::Table::from_iter([("profile".to_owned(), toml::Value::Table(profiles))]);
    let config = temp.path().join("nextest.toml");
    fs::write(
        &config,
        toml::to_string(&isolated).expect("isolated stress TOML"),
    )
    .expect("write isolated stress profile");
    let inventory = temp.path().join("inventory.json");
    let release = temp.path().join("release-output");
    let released = temp.path().join("output-holder-released");
    let started = temp.path().join("output-holder-started");
    let command = |action: &str| {
        let mut command = Command::new(&cargo);
        command
            .current_dir(temp.path())
            .args(["nextest", action, "--offline", "--profile", "stress"])
            .arg("--config-file")
            .arg(&config)
            .args(["-E", "test(=leaks_output)"])
            .env("CARGO_TARGET_DIR", temp.path().join("build"))
            .env("CARGO_HOME", temp.path().join("cargo-home"))
            .env("LEAK_RELEASE_PATH", &release)
            .env("LEAK_RELEASED_PATH", &released)
            .env("LEAK_STARTED_PATH", &started);
        command
    };
    let listed = command("list")
        .args(["--message-format", "json"])
        .stdout(File::create(&inventory).expect("inventory file"))
        .output()
        .expect("nextest inventory");
    assert!(
        listed.status.success(),
        "inventory failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    let outcome = command("run")
        .args(["--stress-count", "1", "--test-threads", "1"])
        .output()
        .expect("run the output leak fixture");
    fs::write(&release, "release").expect("release the deliberate output holder");
    let cleanup_started = Instant::now();
    while !released.is_file() && cleanup_started.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        released.is_file(),
        "the output holder must acknowledge release before fixture cleanup"
    );

    assert!(started.is_file(), "the fixture spawned its output holder");
    let junit = temp.path().join("target/nextest/stress/junit.xml");
    let xml = fs::read_to_string(&junit).expect("nextest JUnit report");
    let cases = parse_junit(&xml).expect("parse generated leak report");
    assert_eq!(cases.len(), 1, "exactly the leaking test was selected");
    assert_eq!(cases[0].name, "leaks_output");
    assert!(
        !outcome.status.success(),
        "nextest passed a real output leak: {}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    assert!(
        cases[0].failed,
        "the leaked attempt must fail in JUnit: {xml}"
    );
    assert!(cases[0].output.contains("leaked handles"), "{xml}");
    stress_report::validate_primary_evidence(
        &inventory,
        &junit,
        1,
        &StressRenderBudgets::default(),
    )
    .expect_err("a leaked attempt cannot be primary clean stress evidence");
    let report = stress_report::lane_report(&StressReportArgs::new(
        junit,
        inventory,
        temp.path().join("report.md"),
        1,
    ))
    .expect("render leak evidence");
    assert!(report.verdict.is_err(), "the stress report must fail");
    assert!(
        report.markdown.contains("leaks_output"),
        "{}",
        report.markdown
    );
    assert!(
        report.markdown.contains("leaked handles"),
        "{}",
        report.markdown
    );
}
