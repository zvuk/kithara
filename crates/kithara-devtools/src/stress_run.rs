//! Runs a controller-defined nextest stress lane against the current workspace.

use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};

use crate::{
    common::project::StressRenderBudgets,
    stress::{run_output, run_stderr_output},
    stress_report::{
        MAX_INVENTORY_BYTES, read_bounded_utf8, validate_inventory, validate_primary_evidence,
    },
    test::{ConfiguredLane, NextestAction, nextest_configured_lane_command},
    verdict::ChildFailure,
};

#[derive(Debug)]
pub(crate) struct StressRunSpec {
    pub(crate) runner: ConfiguredLane,
    pub(crate) config_file: PathBuf,
    pub(crate) inventory: PathBuf,
    pub(crate) junit: PathBuf,
    pub(crate) filter: String,
    pub(crate) profile: String,
    pub(crate) test_threads: String,
    pub(crate) count: usize,
    pub(crate) max_count: usize,
    pub(crate) max_test_threads: usize,
    pub(crate) render: StressRenderBudgets,
}

/// Lists the exact selection, validates it, then runs every selected test.
///
/// # Errors
///
/// Returns an error when the run inputs are invalid, listing does not
/// produce a usable inventory, the `JUnit` destination is not fresh, nextest
/// cannot complete the stress run, or any recorded attempt failed.
pub(crate) fn run(
    args: &StressRunSpec,
    subject_root: &Path,
    log_path: &Path,
    configure: &dyn Fn(&mut Command),
) -> Result<()> {
    validate(args)?;
    let config_file = args
        .config_file
        .to_str()
        .context("nextest config path is not UTF-8")?;
    let inventory_args = vec![
        "--profile".to_owned(),
        args.profile.clone(),
        "--config-file".to_owned(),
        config_file.to_owned(),
        "-E".to_owned(),
        args.filter.clone(),
        "--message-format".to_owned(),
        "json".to_owned(),
    ];
    let (_, mut inventory) =
        nextest_configured_lane_command(&args.runner, &inventory_args, NextestAction::List)?;
    let output = create_inventory(&args.inventory)?;
    inventory
        .current_dir(subject_root)
        .stdout(Stdio::from(output));
    configure(&mut inventory);
    let status = run_stderr_output(&mut inventory, log_path)?;
    if !status.success() {
        return Err(ChildFailure::inherited(
            "nextest inventory".to_owned(),
            status.code(),
        ));
    }
    let json = read_bounded_utf8(&args.inventory, MAX_INVENTORY_BYTES, "stress inventory")?;
    validate_inventory(&json).context("validate nextest inventory")?;

    let run_args = vec![
        "--profile".to_owned(),
        args.profile.clone(),
        "--config-file".to_owned(),
        config_file.to_owned(),
        "-E".to_owned(),
        args.filter.clone(),
        "--stress-count".to_owned(),
        args.count.to_string(),
        "--test-threads".to_owned(),
        args.test_threads.clone(),
    ];
    let (_, mut run) =
        nextest_configured_lane_command(&args.runner, &run_args, NextestAction::Run)?;
    run.current_dir(subject_root);
    configure(&mut run);
    run_child(&mut run, log_path)?;
    validate_primary_evidence(&args.inventory, &args.junit, args.count, &args.render)
}

pub(crate) fn validate(args: &StressRunSpec) -> Result<()> {
    validate_stress_count(args.count, args.max_count)?;
    ensure!(!args.filter.trim().is_empty(), "stress filter is empty");
    ensure!(!args.profile.trim().is_empty(), "stress profile is empty");
    args.runner.validate()?;
    validate_test_threads(&args.test_threads, args.max_test_threads)?;
    ensure!(
        args.junit.is_absolute(),
        "stress JUnit path must be absolute: {}",
        args.junit.display()
    );
    // Whether the subject's JUnit path is clear is the whole run's business,
    // not one lane's: a run of several lanes writes that one path once per
    // lane, and a rule enforced here would stop its second lane before a
    // single test ran. The run demands an untouched path before it starts
    // and clears the previous lane's file before each run, which is also what
    // makes staging honest — a file found afterwards can only be this run's.
    ensure!(
        args.config_file.is_file(),
        "nextest config does not exist: {}",
        args.config_file.display()
    );
    Ok(())
}

fn validate_test_threads(value: &str, max: usize) -> Result<()> {
    ensure!(max > 0, "maximum test-threads must be greater than zero");
    if value == "num-cpus" {
        return Ok(());
    }
    let count = value
        .parse::<usize>()
        .with_context(|| format!("invalid test-threads value `{value}`"))?;
    ensure!(count > 0, "test-threads must be greater than zero");
    ensure!(count <= max, "test-threads must not exceed {max}",);
    Ok(())
}

fn validate_stress_count(count: usize, max: usize) -> Result<()> {
    ensure!(max > 0, "maximum stress count must be greater than zero");
    ensure!(count > 0, "stress count must be greater than zero");
    ensure!(count <= max, "stress count must not exceed {max}",);
    Ok(())
}

fn create_inventory(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create stress inventory directory {}", parent.display()))?;
    }
    File::create(path).with_context(|| format!("create stress inventory {}", path.display()))
}

fn run_child(command: &mut Command, log_path: &Path) -> Result<()> {
    let status = run_output(command, log_path)?;
    if status.success() {
        return Ok(());
    }
    Err(ChildFailure::inherited(
        "nextest stress run".to_owned(),
        status.code(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVENTORY: &str = r#"{
  "rust-suites": {
    "demo::tests": {
      "binary-id": "demo::tests",
      "status": "listed",
      "testcases": {
        "seek": {"ignored": false, "filter-match": {"status": "matches"}}
      }
    }
  }
}"#;

    const PASSED_JUNIT: &str = r#"<testsuites uuid="run" timestamp="2026-08-13T12:00:00Z">
  <testsuite name="demo::tests@stress-0">
    <testcase name="seek" classname="demo::tests" time="0.1" timestamp="2026-08-13T12:00:00Z"/>
  </testsuite>
</testsuites>"#;

    const EARLY_FAILED_JUNIT: &str = r#"<testsuites uuid="run" timestamp="2026-08-13T12:00:00Z">
  <testsuite name="demo::tests@stress-0">
    <testcase name="seek" classname="demo::tests" time="0.1" timestamp="2026-08-13T12:00:00Z">
      <failure type="test failure">boom</failure>
    </testcase>
  </testsuite>
  <testsuite name="demo::tests@stress-1">
    <testcase name="seek" classname="demo::tests" time="0.1" timestamp="2026-08-13T12:00:01Z"/>
  </testsuite>
</testsuites>"#;

    fn evidence_paths(temp: &tempfile::TempDir, junit: &str) -> (PathBuf, PathBuf) {
        let inventory = temp.path().join("inventory.json");
        let junit_path = temp.path().join("junit.xml");
        fs::write(&inventory, INVENTORY).expect("write inventory fixture");
        fs::write(&junit_path, junit).expect("write JUnit fixture");
        (inventory, junit_path)
    }

    #[test]
    fn test_threads_accepts_nextest_concurrency_values() {
        for value in ["num-cpus", "1", "32"] {
            validate_test_threads(value, 256).expect("valid test thread count");
        }
    }

    #[test]
    fn test_threads_rejects_invalid_or_unsupported_values() {
        for value in ["", "0", "257", "all"] {
            assert!(validate_test_threads(value, 256).is_err(), "{value}");
        }
    }

    #[test]
    fn stress_count_is_limited_to_supported_run_sizes() {
        for count in [1, 50, 100] {
            validate_stress_count(count, 100).expect("valid stress count");
        }
        for count in [0, 101, usize::MAX] {
            assert!(validate_stress_count(count, 100).is_err(), "{count}");
        }
    }

    #[test]
    fn passed_junit_is_a_clean_stress_outcome() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (inventory, junit) = evidence_paths(&temp, PASSED_JUNIT);

        validate_primary_evidence(&inventory, &junit, 1, &StressRenderBudgets::default())
            .expect("passed JUnit must be clean");
    }

    #[test]
    fn failed_attempt_before_passing_last_attempt_is_not_clean() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (inventory, junit) = evidence_paths(&temp, EARLY_FAILED_JUNIT);

        let error =
            validate_primary_evidence(&inventory, &junit, 2, &StressRenderBudgets::default())
                .expect_err("failed attempt must fail closed");

        assert!(
            error.downcast_ref::<crate::verdict::NotClean>().is_some(),
            "{error:?}"
        );
    }

    #[test]
    fn empty_or_partial_junit_is_not_clean() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (inventory, empty) = evidence_paths(
            &temp,
            r#"<testsuites uuid="run" timestamp="2026-08-13T12:00:00Z"/>"#,
        );

        let empty_error =
            validate_primary_evidence(&inventory, &empty, 2, &StressRenderBudgets::default())
                .expect_err("empty JUnit must fail closed");

        assert!(
            empty_error
                .downcast_ref::<crate::verdict::NotClean>()
                .is_some(),
            "{empty_error:?}"
        );

        let partial = temp.path().join("partial.xml");
        fs::write(&partial, PASSED_JUNIT).expect("write partial JUnit fixture");
        let partial_error =
            validate_primary_evidence(&inventory, &partial, 2, &StressRenderBudgets::default())
                .expect_err("partial JUnit must fail closed");

        assert!(
            partial_error
                .downcast_ref::<crate::verdict::NotClean>()
                .is_some(),
            "{partial_error:?}"
        );
    }
}
