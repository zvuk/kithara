use std::collections::BTreeMap;

use serde_json::json;
use tempfile::TempDir;

use super::*;
use crate::common::project::{TestCargoOptions, TestRunner};

const LANE: &str = "flash-on-http";

fn fixture() -> (TempDir, RunPaths) {
    let temp = tempfile::tempdir().expect("measurement fixture");
    let paths = RunPaths::new(temp.path(), "run");
    fs::create_dir_all(paths.matrix_dir().join(LANE)).expect("matrix directory");
    let config_path = paths.run_dir.join("nextest.toml");
    fs::write(&config_path, "[profile.perf.junit]\npath = 'junit.xml'\n").expect("nextest config");
    let plan = MatrixPlan {
        format_version: 1,
        run_id: "run".to_owned(),
        commit: "a".repeat(40),
        host: "fixture-host".to_owned(),
        platform: "linux".to_owned(),
        architecture: "x86_64".to_owned(),
        toolchain: "rustc fixture".to_owned(),
        nextest: "nextest fixture".to_owned(),
        nextest_config_sha256: digest(&config_path, consts::MAX_MANIFEST_BYTES as u64, "config")
            .expect("config digest"),
        primary_lane: LANE.to_owned(),
        repeats: 3,
        lanes: vec![PlannedLane {
            name: LANE.to_owned(),
            flash: true,
            runner: ResolvedLane {
                env: BTreeMap::new(),
                backend: "http".to_owned(),
                lane: "workspace".to_owned(),
                cargo: TestCargoOptions {
                    workspace: true,
                    ..TestCargoOptions::default()
                },
                runner: TestRunner::default(),
                features: vec!["flash".to_owned()],
            },
            arguments: vec!["--stress-count=1".to_owned(), "--retries=0".to_owned()],
        }],
    };
    write_json(&paths.run_dir.join("matrix.json"), &plan).expect("matrix plan");
    for (repeat, seconds) in [(1, 0.1), (2, 0.1), (3, 1.5)] {
        let dir = paths.repeat_dir(LANE, repeat);
        fs::create_dir(&dir).expect("repetition");
        fs::copy(&config_path, dir.join("nextest.toml")).expect("invocation config");
        let inventory = json!({"rust-suites": {"demo::tests": {
            "binary-id": "demo::tests", "status": "listed", "testcases": {
                "one": {"ignored": false, "filter-match": {"status": "matches"}}
            }
        }}});
        write_json(&dir.join("inventory.json"), &inventory).expect("inventory");
        let xml = format!(
            "<testsuites uuid=\"execution-{repeat}\" timestamp=\"1970-01-01T00:00:01Z\">\
             <testsuite name=\"demo::tests@stress-0\">\
             <testcase name=\"one\" classname=\"demo::tests\" time=\"{seconds}\" \
             timestamp=\"1970-01-01T00:00:01Z\"></testcase>\
             </testsuite></testsuites>"
        );
        fs::write(dir.join("junit.xml"), xml).expect("JUnit");
        let meta = RepeatMeta {
            exit_code: Some(0),
            commit: plan.commit.clone(),
            lane: LANE.to_owned(),
            run_id: "run".to_owned(),
            features: vec!["flash".to_owned()],
            duration_secs: 2.0,
            preparation_secs: 0.5,
            repeat,
            started_unix_ms: 900,
            finished_unix_ms: 3000,
            inventory_sha256: digest(
                &dir.join("inventory.json"),
                consts::MAX_INVENTORY_BYTES,
                "inventory",
            )
            .expect("inventory digest"),
            junit_sha256: Some(
                digest(&dir.join("junit.xml"), consts::MAX_JUNIT_BYTES, "JUnit")
                    .expect("JUnit digest"),
            ),
            nextest_run_id: Some(format!("execution-{repeat}")),
            nextest_config_sha256: plan.nextest_config_sha256.clone(),
        };
        write_json(&dir.join("meta.json"), &meta).expect("receipt");
    }
    (temp, paths)
}

fn change_report(paths: &RunPaths, repeat: u32, edit: impl FnOnce(String) -> String) {
    let dir = paths.repeat_dir(LANE, repeat);
    let path = dir.join("junit.xml");
    fs::write(
        &path,
        edit(fs::read_to_string(&path).expect("read fixture JUnit")),
    )
    .expect("change fixture JUnit");
    let mut meta: RepeatMeta = read_json(&dir.join("meta.json")).expect("receipt");
    meta.junit_sha256 = Some(digest(&path, consts::MAX_JUNIT_BYTES, "JUnit").expect("digest"));
    write_json(&dir.join("meta.json"), &meta).expect("update fixture binding");
}

#[test]
fn complete_matrix_retains_every_measurement_and_outlier() {
    let (_temp, paths) = fixture();
    let (plan, runs) = read_matrix(&paths, "run").expect("complete matrix");
    assert_eq!(plan.repeats, 3);
    assert_eq!(runs.len(), 3);
    assert_eq!(
        runs.iter()
            .map(|(_, cases)| cases[0].secs)
            .collect::<Vec<_>>(),
        [0.1, 0.1, 1.5]
    );
}

#[test]
fn missing_repetition_and_unexpected_repetition_are_rejected() {
    let (_temp, paths) = fixture();
    fs::rename(paths.repeat_dir(LANE, 3), paths.repeat_dir(LANE, 4)).expect("change repetition");
    assert!(read_matrix(&paths, "run").is_err());
    fs::remove_dir_all(paths.repeat_dir(LANE, 4)).expect("remove fixture repetition");
    assert!(read_matrix(&paths, "run").is_err());
}

#[test]
fn missing_inventory_report_and_receipt_are_rejected() {
    for name in ["inventory.json", "junit.xml", "meta.json", "nextest.toml"] {
        let (_temp, paths) = fixture();
        fs::remove_file(paths.repeat_dir(LANE, 2).join(name)).expect("remove fixture artifact");
        assert!(read_matrix(&paths, "run").is_err(), "{name}");
    }
}

#[test]
fn original_and_invocation_configuration_are_content_bound() {
    for invocation in [false, true] {
        let (_temp, paths) = fixture();
        let path = if invocation {
            paths.repeat_dir(LANE, 2)
        } else {
            paths.run_dir.clone()
        }
        .join("nextest.toml");
        fs::write(path, "[profile.perf]\nretries = 5\n").expect("change retained configuration");
        assert!(read_matrix(&paths, "run").is_err());
    }
}

#[test]
fn changing_report_without_rebinding_is_rejected() {
    let (_temp, paths) = fixture();
    let path = paths.repeat_dir(LANE, 1).join("junit.xml");
    let xml = fs::read_to_string(&path).expect("fixture JUnit");
    fs::write(path, xml.replace("0.1", "0.2")).expect("change raw report");
    assert!(read_matrix(&paths, "run").is_err());
}

#[test]
fn a_reused_execution_is_rejected_even_with_matching_digests() {
    let (_temp, paths) = fixture();
    change_report(&paths, 2, |xml| xml.replace("execution-2", "execution-1"));
    let path = paths.repeat_dir(LANE, 2).join("meta.json");
    let mut meta: RepeatMeta = read_json(&path).expect("fixture receipt");
    meta.nextest_run_id = Some("execution-1".to_owned());
    write_json(&path, &meta).expect("rebind execution");
    assert!(read_matrix(&paths, "run").is_err());
}

#[test]
fn valid_junit_outside_its_execution_interval_is_rejected() {
    let (_temp, paths) = fixture();
    let path = paths.repeat_dir(LANE, 1).join("meta.json");
    let mut meta: RepeatMeta = read_json(&path).expect("fixture receipt");
    meta.started_unix_ms = 2000;
    meta.finished_unix_ms = 4000;
    write_json(&path, &meta).expect("change interval");
    assert!(read_matrix(&paths, "run").is_err());
}

#[test]
fn failed_retried_duplicate_and_invalid_duration_cases_are_rejected() {
    for replacement in [
        "<failure>broken</failure></testcase>",
        "<flakyFailure>broken</flakyFailure></testcase>",
    ] {
        let (_temp, paths) = fixture();
        change_report(&paths, 1, |xml| xml.replace("</testcase>", replacement));
        assert!(read_matrix(&paths, "run").is_err());
    }
    let (_temp, paths) = fixture();
    change_report(&paths, 1, |xml| xml.replace("time=\"0.1\"", "time=\"NaN\""));
    assert!(read_matrix(&paths, "run").is_err());
    let (_temp, paths) = fixture();
    change_report(&paths, 1, |xml| {
        let start = xml.find("<testcase ").expect("case start");
        let end = xml.find("</testcase>").expect("case end") + "</testcase>".len();
        let duplicate = xml[start..end].to_owned();
        xml.replace("</testsuite>", &(duplicate + "</testsuite>"))
    });
    assert!(read_matrix(&paths, "run").is_err());
}

#[test]
fn threshold_check_retains_the_report_and_rejects_outlier() {
    let (temp, paths) = fixture();
    assert!(crate::perf::slow::run(temp.path(), "run", 1000.0, true).is_err());
    assert!(paths.slow_json().is_file());
    let report: serde_json::Value = read_json(&paths.slow_json()).expect("saved slow report");
    assert_eq!(report["tests"][0]["lanes"][LANE]["max_ms"], 1500.0);
    crate::perf::slow::run(temp.path(), "run", 1000.0, false).expect("diagnostic action");
}

#[test]
fn a_changed_selection_is_rejected_after_each_run_is_individually_valid() {
    let (_temp, paths) = fixture();
    let dir = paths.repeat_dir(LANE, 2);
    let inventory = dir.join("inventory.json");
    let text = fs::read_to_string(&inventory).expect("inventory");
    fs::write(&inventory, text.replace("\"one\"", "\"two\"")).expect("change selection");
    change_report(&paths, 2, |xml| xml.replace("name=\"one\"", "name=\"two\""));
    let path = dir.join("meta.json");
    let mut meta: RepeatMeta = read_json(&path).expect("receipt");
    meta.inventory_sha256 =
        digest(&inventory, consts::MAX_INVENTORY_BYTES, "inventory").expect("rebind inventory");
    write_json(&path, &meta).expect("update receipt");
    read_primary_evidence(
        &inventory,
        &dir.join("junit.xml"),
        1,
        &StressRenderBudgets::default(),
    )
    .expect("the changed invocation is individually complete");
    assert!(read_matrix(&paths, "run").is_err());
}

#[test]
fn receipt_coordinate_source_features_and_result_are_required() {
    for (field, value) in [
        ("repeat", json!(3)),
        ("lane", json!("flash-off-http")),
        ("commit", json!("b".repeat(40))),
        ("features", json!([])),
        ("exit_code", json!(1)),
    ] {
        let (_temp, paths) = fixture();
        let path = paths.repeat_dir(LANE, 1).join("meta.json");
        let mut meta: serde_json::Value = read_json(&path).expect("receipt");
        meta[field] = value;
        write_json(&path, &meta).expect("change receipt");
        assert!(read_matrix(&paths, "run").is_err(), "{field}");
    }
}

#[test]
fn a_threshold_equal_to_the_maximum_still_fails() {
    let (temp, _paths) = fixture();
    assert!(crate::perf::slow::run(temp.path(), "run", 1500.0, true).is_err());
    crate::perf::slow::run(temp.path(), "run", 1500.1, true)
        .expect("every measurement is strictly below the selected threshold");
}

#[test]
fn invalid_thresholds_are_rejected_before_reading_evidence() {
    let temp = tempfile::tempdir().expect("threshold fixture");
    for threshold in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let error = crate::perf::slow::run(temp.path(), "absent", threshold, true)
            .expect_err("invalid threshold");
        assert!(error.to_string().contains("finite and greater than zero"));
    }
}

#[test]
fn cached_results_cannot_replace_raw_measurements() {
    let (temp, paths) = fixture();
    crate::perf::slow::run(temp.path(), "run", 1000.0, false).expect("generate report");
    let mut cached: serde_json::Value = read_json(&paths.slow_json()).expect("cached report");
    cached["tests"] = json!([]);
    cached["primary_lane"] = json!("foreign-lane");
    write_json(&paths.slow_json(), &cached).expect("replace derived data");
    let report = crate::perf::slow::load_report(&paths, "run").expect("raw evidence recomputed");
    assert_eq!(report.tests.len(), 1);
    assert_eq!(report.primary_lane, LANE);
    fs::remove_file(paths.repeat_dir(LANE, 3).join("junit.xml")).expect("remove raw report");
    assert!(crate::perf::slow::load_report(&paths, "run").is_err());
}
