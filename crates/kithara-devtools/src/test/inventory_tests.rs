#[cfg(unix)]
use std::{collections::BTreeMap, fs};

#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
use super::command::run_lane;
use super::request::TestRequest;
#[cfg(unix)]
use crate::{common::project::TestCommandConfig, test::tests::synthetic_project};

#[cfg(unix)]
fn fixture(
    selected: &[(&str, &str)],
    expected: &[u8],
) -> (TempDir, TestCommandConfig, TestRequest) {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("fixture directory");
    let root = temp.path();
    let mut suites = BTreeMap::<&str, serde_json::Value>::new();
    for &(binary, name) in selected {
        let suite = suites
            .entry(binary)
            .or_insert_with(|| json!({"binary-id": binary, "status": "listed", "testcases": {}}));
        suite["testcases"][name] = json!({"ignored": false, "filter-match": {"status": "matches"}});
    }
    if suites.is_empty() {
        suites.insert(
            "demo::tests",
            json!({"binary-id": "demo::tests", "status": "listed", "testcases": {
                "excluded": {"ignored": false, "filter-match": {"status": "mismatch"}}
            }}),
        );
    }
    fs::write(
        root.join("selected.json"),
        json!({"rust-suites": suites}).to_string(),
    )
    .expect("write inventory");
    fs::write(root.join("expected.tsv"), expected).expect("write expected identities");
    fs::write(
        root.join("nextest.toml"),
        "[profile.default]\nretries = 0\n",
    )
    .expect("write nextest configuration");
    let nextest = root.join("cargo-nextest");
    fs::write(
        &nextest,
        r#"#!/bin/sh
printf '%s\n' "$2" >> "$TEST_CHILDREN"
case "$2" in
  list)
    for value in "$@"; do
      [ "$value" = -- ] && break
      case "$value" in
        --no-tests|--no-tests=*|--test-threads|--test-threads=*|--retries|--retries=*) exit 98 ;;
        --failure-output|--failure-output=*|--success-output|--success-output=*) exit 98 ;;
        --no-fail-fast|-j|-j[0-9]*) exit 98 ;;
      esac
    done
    /bin/cat "$TEST_INVENTORY"
    exit "${TEST_LIST_CODE:-0}"
    ;;
  run) exit 0 ;;
  *) exit 99 ;;
esac
"#,
    )
    .expect("write nextest fixture");
    fs::set_permissions(&nextest, fs::Permissions::from_mode(0o755)).expect("executable fixture");
    let mut test = synthetic_project().test;
    test.nextest_config = "nextest.toml".to_owned();
    let lane = test.lanes.get_mut("workspace").expect("workspace lane");
    lane.env = BTreeMap::from([
        ("PATH".to_owned(), root.display().to_string()),
        (
            "TEST_CHILDREN".to_owned(),
            root.join("children").display().to_string(),
        ),
        (
            "TEST_INVENTORY".to_owned(),
            root.join("selected.json").display().to_string(),
        ),
    ]);
    let request = TestRequest::parse(&[
        "--expected-tests".to_owned(),
        root.join("expected.tsv").display().to_string(),
        "--narrow=test(expected)".to_owned(),
        "--no-tests=fail".to_owned(),
    ])
    .expect("request");
    (temp, test, request)
}

#[cfg(unix)]
fn children(temp: &TempDir) -> String {
    fs::read_to_string(temp.path().join("children")).unwrap_or_default()
}

#[test]
#[cfg(unix)]
fn expected_selection_rejects_one_of_four_before_running_any_test() {
    let expected = b"demo::tests\tone\ndemo::tests\ttwo\ndemo::tests\tthree\ndemo::tests\tfour\n";
    let (temp, test, request) = fixture(&[("demo::tests", "one")], expected);
    let error = run_lane(&test, temp.path(), "workspace", &request)
        .expect_err("a partial selection is not the requested four tests");
    let message = format!("{error:#}");
    for name in ["two", "three", "four"] {
        assert!(
            message.contains(&format!("demo::tests\t{name}")),
            "{message}"
        );
    }
    assert!(message.contains("missing (3)"), "{message}");
    assert_eq!(children(&temp), "list\n");
}

#[test]
#[cfg(unix)]
fn expected_selection_reports_unexpected_tests_without_running_them() {
    let (temp, test, request) = fixture(
        &[("demo::tests", "one"), ("demo::tests", "extra")],
        b"demo::tests\tone\n",
    );
    let error = run_lane(&test, temp.path(), "workspace", &request).expect_err("unexpected test");
    let message = format!("{error:#}");
    assert!(message.contains("unexpected (1)"), "{message}");
    assert!(message.contains("demo::tests\textra"), "{message}");
    assert_eq!(children(&temp), "list\n");
}

#[test]
#[cfg(unix)]
fn expected_selection_distinguishes_binary_ids_with_the_same_test_name() {
    let (temp, test, request) = fixture(&[("demo::tests", "seek")], b"demo::other\tseek\n");
    let error = run_lane(&test, temp.path(), "workspace", &request).expect_err("wrong binary");
    let message = format!("{error:#}");
    for identity in ["demo::tests\tseek", "demo::other\tseek"] {
        assert!(message.contains(identity), "{message}");
    }
    assert!(message.contains("missing (1)"), "{message}");
    assert!(message.contains("unexpected (1)"), "{message}");
    assert_eq!(children(&temp), "list\n");
}

#[test]
#[cfg(unix)]
fn expected_selection_reports_every_missing_identity_for_an_empty_selection() {
    let (temp, test, request) = fixture(&[], b"demo::tests\tone\ndemo::tests\ttwo\n");
    let error = run_lane(&test, temp.path(), "workspace", &request).expect_err("empty selection");
    let message = format!("{error:#}");
    assert!(message.contains("missing (2)"), "{message}");
    assert!(message.contains("demo::tests\tone"), "{message}");
    assert!(message.contains("demo::tests\ttwo"), "{message}");
    assert_eq!(children(&temp), "list\n");
}

#[test]
#[cfg(unix)]
fn expected_selection_rejects_invalid_ledgers_before_starting_a_child() {
    for expected in [
        b"".as_slice(),
        b"\n".as_slice(),
        b"demo::tests\tone\ndemo::tests\tone\n".as_slice(),
        b"demo::tests one\n".as_slice(),
        b"demo::tests\tone\textra\n".as_slice(),
        b"\tone\n".as_slice(),
        b"demo::tests\t\n".as_slice(),
        b" demo::tests\tone\n".as_slice(),
        b"demo::tests\tone \n".as_slice(),
        b"demo::tests\t\xff\n".as_slice(),
        "demo::tests\t\u{e9}\n".as_bytes(),
    ] {
        let (temp, test, request) = fixture(&[("demo::tests", "one")], expected);
        run_lane(&test, temp.path(), "workspace", &request)
            .expect_err("invalid expected identities");
        assert_eq!(children(&temp), "", "{expected:?}");
    }
}

#[test]
#[cfg(unix)]
fn expected_selection_rejects_a_missing_ledger_before_starting_a_child() {
    let (temp, test, request) = fixture(&[("demo::tests", "one")], b"demo::tests\tone\n");
    fs::remove_file(temp.path().join("expected.tsv")).expect("remove ledger fixture");
    run_lane(&test, temp.path(), "workspace", &request).expect_err("missing expected identities");
    assert_eq!(children(&temp), "");
}

#[test]
#[cfg(unix)]
fn expected_selection_requires_a_successful_native_inventory() {
    let (temp, mut test, request) = fixture(&[("demo::tests", "one")], b"demo::tests\tone\n");
    test.lanes
        .get_mut("workspace")
        .expect("workspace")
        .env
        .insert("TEST_LIST_CODE".to_owned(), "17".to_owned());
    run_lane(&test, temp.path(), "workspace", &request).expect_err("failed inventory command");
    assert_eq!(children(&temp), "list\n");
}

#[test]
#[cfg(unix)]
fn expected_selection_admits_exact_sets_independent_of_input_order() {
    let (temp, test, request) = fixture(
        &[("demo::tests", "one"), ("demo::tests", "two")],
        b"demo::tests\ttwo\ndemo::tests\tone\n",
    );
    run_lane(&test, temp.path(), "workspace", &request).expect("exact selection");
    assert_eq!(children(&temp), "list\nrun\n");
}

#[test]
#[cfg(unix)]
fn unguarded_runs_do_not_add_an_inventory_process() {
    let (temp, test, _) = fixture(&[("demo::tests", "one")], b"demo::tests\tone\n");
    let request = TestRequest::parse(&["--narrow=test(one)".to_owned()]).expect("request");
    run_lane(&test, temp.path(), "workspace", &request).expect("unguarded run");
    assert_eq!(children(&temp), "run\n");
}

#[test]
#[cfg(unix)]
fn expected_selection_admits_runner_options_in_separate_and_attached_forms() {
    for extra in [
        vec![
            "--test-threads",
            "2",
            "--retries",
            "0",
            "--failure-output",
            "immediate",
        ],
        vec![
            "--test-threads=2",
            "--retries=0",
            "--failure-output=immediate",
        ],
        vec!["-j2", "--no-fail-fast", "--success-output", "never"],
    ] {
        let (temp, test, mut request) = fixture(&[("demo::tests", "one")], b"demo::tests\tone\n");
        request
            .passthrough
            .extend(extra.into_iter().map(str::to_owned));
        run_lane(&test, temp.path(), "workspace", &request)
            .expect("exact selection with runner options");
        assert_eq!(children(&temp), "list\nrun\n");
    }
}

#[test]
#[cfg(unix)]
fn expected_selection_treats_shared_option_values_as_values() {
    let (temp, test, _) = fixture(&[("demo::tests", "one")], b"demo::tests\tone\n");
    let request = TestRequest::parse(&[
        format!(
            "--expected-tests={}",
            temp.path().join("expected.tsv").display()
        ),
        "-E".to_owned(),
        "-Rfilter".to_owned(),
        "--profile".to_owned(),
        "-Rprofile".to_owned(),
        "--test".to_owned(),
        "-Rtarget".to_owned(),
        "--config-file".to_owned(),
        "-Rconfig".to_owned(),
        "--run-ignored".to_owned(),
        "default".to_owned(),
    ])
    .expect("values do not introduce rerun options");
    run_lane(&test, temp.path(), "workspace", &request).expect("exact selection");
    assert_eq!(children(&temp), "list\nrun\n");
}

#[test]
#[cfg(unix)]
fn expected_selection_preserves_libtest_filters_and_skip_values_after_the_separator() {
    let (temp, test, _) = fixture(&[("demo::tests", "one")], b"demo::tests\tone\n");
    let request = TestRequest::parse(&[
        format!(
            "--expected-tests={}",
            temp.path().join("expected.tsv").display()
        ),
        "--".to_owned(),
        "-Rfilter".to_owned(),
        "--skip".to_owned(),
        "--ignored".to_owned(),
    ])
    .expect("postseparator filters and skip values do not introduce runner options");
    run_lane(&test, temp.path(), "workspace", &request).expect("exact selection");
    assert_eq!(children(&temp), "list\nrun\n");
}

#[test]
#[cfg(unix)]
fn expected_selection_excludes_ignored_mismatched_and_skipped_inventory_entries() {
    let (temp, test, request) = fixture(&[("demo::tests", "one")], b"demo::tests\tone\n");
    fs::write(temp.path().join("selected.json"), json!({"rust-suites": {
        "demo::tests": {"binary-id": "demo::tests", "status": "listed", "testcases": {
            "one": {"ignored": false, "filter-match": {"status": "matches"}},
            "ignored": {"ignored": true, "filter-match": {"status": "matches"}},
            "filtered": {"ignored": false, "filter-match": {"status": "mismatch"}}
        }},
        "other::tests": {"binary-id": "other::tests", "status": "skipped-default-filter", "testcases": {}}
    }}).to_string()).expect("write filtered inventory");
    run_lane(&test, temp.path(), "workspace", &request)
        .expect("only runnable matching identities count");
    assert_eq!(children(&temp), "list\nrun\n");
}

#[test]
#[cfg(unix)]
fn expected_selection_reports_missing_identities_when_no_binary_is_listed() {
    let (temp, test, request) = fixture(&[], b"demo::tests\tone\n");
    fs::write(temp.path().join("selected.json"), "{\"rust-suites\": {}}")
        .expect("write empty inventory");
    let error =
        run_lane(&test, temp.path(), "workspace", &request).expect_err("no selected binary");
    let message = format!("{error:#}");
    assert!(message.contains("missing (1)"), "{message}");
    assert!(message.contains("demo::tests\tone"), "{message}");
    assert_eq!(children(&temp), "list\n");
}

#[test]
fn expected_selection_rejects_unsupported_request_combinations() {
    for extra in [
        vec!["--draft"],
        vec!["--rerun", "latest"],
        vec!["--rerun=latest"],
        vec!["-Rlatest"],
        vec!["--run-ignored", "all"],
        vec!["--run-ignored=only"],
        vec!["--", "--ignored"],
        vec!["--", "--include-ignored"],
    ] {
        let mut args = vec!["--expected-tests=expected.tsv".to_owned()];
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        let error = TestRequest::parse(&args).expect_err("unsupported guarded selection");
        assert!(
            format!("{error:#}").contains("--expected-tests"),
            "{error:#}"
        );
    }
    TestRequest::parse(&[
        "--expected-tests=expected.tsv".to_owned(),
        "--run-ignored=default".to_owned(),
    ])
    .expect("default ignored behavior is supported");
}

#[test]
fn expected_selection_rejects_missing_empty_or_repeated_paths() {
    for args in [
        vec!["--expected-tests"],
        vec!["--expected-tests="],
        vec!["--expected-tests", ""],
        vec!["--expected-tests=one.tsv", "--expected-tests=two.tsv"],
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        TestRequest::parse(&args).expect_err("invalid expected identity path");
    }
}
