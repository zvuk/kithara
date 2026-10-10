//! What a lane's run left behind, and the verdict it supports.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use toml::Value;
use tracing::warn;

use crate::{
    common::project::{KnownFlake, TestCommandConfig},
    consts,
    junit::{CaseTiming, parse_junit},
    stress_report::read_bounded_utf8,
    verdict::ChildFailure,
};

/// What one nextest profile declares about hiding a failure.
struct Declared {
    /// Where the profile writes its report, relative to the profile's own
    /// store directory.
    junit: Option<String>,
    /// Attempts the profile grants after the first.
    retries: u64,
}

/// What a lane leaves behind to be judged by.
///
/// nextest keeps a failing attempt — its streams, its panic and its dump —
/// and exits zero when a retry passes, so a lane judged by its exit code
/// alone reads a defect that reproduced as a clean run. The retry makes that
/// attempt legible; it is not permission to pass. A red lane is read from the
/// same report, so its error names the tests instead of pointing thousands of
/// lines up.
#[derive(Debug)]
pub(crate) enum Evidence {
    /// The lane runs no nextest profile that keeps a report: its exit status
    /// is the whole verdict.
    Status,
    /// The report the lane's nextest profile writes to `path`, from a
    /// profile that grants `retries` attempts after the first.
    Report { path: PathBuf, retries: u64 },
}

impl Evidence {
    /// Reads what the lane's command will leave behind.
    ///
    /// # Errors
    ///
    /// When the nextest config cannot be read, or when the profile grants
    /// retries and keeps no report: a retried pass would leave nothing to
    /// judge the lane by.
    pub(crate) fn of(root: &Path, config: &TestCommandConfig, command: &Command) -> Result<Self> {
        let Some(profile) = nextest_profile(command) else {
            return Ok(Self::Status);
        };
        let declared = declared_profile(root, config, &profile)?;
        match declared.junit {
            Some(relative) => Ok(Self::Report {
                path: report_path(root, &profile, &relative),
                retries: declared.retries,
            }),
            None if declared.retries > 0 => bail!(
                "nextest profile `{profile}` grants {} retries and declares no JUnit report: a retried pass would leave nothing to judge the lane by",
                declared.retries
            ),
            None => Ok(Self::Status),
        }
    }

    /// Removes the report an earlier run left, so the report read after this
    /// run is this run's own. A shared build directory keeps another job's.
    ///
    /// A report that cannot be removed is logged, not returned: nextest
    /// overwrites it once it records a test, so only a lane that stopped
    /// before that can read it, and such a lane is red whatever it names.
    pub(crate) fn clear(&self) {
        let Self::Report { path, .. } = self else {
            return;
        };
        if let Err(error) = fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            warn!(
                "the previous report at {} stays: {error}; a lane that stops before nextest \
                 records a test may name its tests",
                path.display()
            );
        }
    }

    /// Judges the lane that exited with `code`.
    ///
    /// # Errors
    ///
    /// A [`ChildFailure`] with the lane's exit code when the lane is red or
    /// passed a test only on a retry that no `known` entry owns, naming the
    /// tests its report blames; an error when a green lane's profile declares
    /// a report the lane did not leave or that cannot be read. A red lane
    /// keeps its code whatever its report holds.
    pub(crate) fn verdict(
        &self,
        lane_name: &str,
        known: &[KnownFlake],
        code: Option<i32>,
    ) -> Result<()> {
        let label = format!("test lane `{lane_name}`");
        let failed = code != Some(0);
        let Self::Report { path, retries } = self else {
            return if failed {
                Err(ChildFailure::inherited(label, code))
            } else {
                Ok(())
            };
        };
        if failed {
            let detail = if path.is_file() {
                read_cases(path).map_or_else(
                    |error| {
                        format!(
                            "its report at {} could not be read ({error:#}); the cause is above",
                            path.display()
                        )
                    },
                    |cases| {
                        named(&cases, known, path).unwrap_or_else(|| {
                            format!(
                                "nextest exited non-zero and its report at {} names no failed test; the cause is above",
                                path.display()
                            )
                        })
                    },
                )
            } else {
                "the lane left no test report: it stopped before nextest recorded a test (a build error or an interruption); the cause is above".to_owned()
            };
            return Err(ChildFailure::explained(label, code, detail));
        }
        if *retries == 0 {
            return Ok(());
        }
        ensure!(
            path.is_file(),
            "{label} ran a nextest profile that declares a JUnit report at {}: without it a retried pass is unjudgeable",
            path.display()
        );
        named(&read_cases(path)?, known, path).map_or(Ok(()), |detail| {
            Err(ChildFailure::explained(label, code, detail))
        })
    }
}

fn read_cases(report: &Path) -> Result<Vec<CaseTiming>> {
    let xml = read_bounded_utf8(report, consts::MAX_JUNIT_BYTES, "test lane JUnit")?;
    parse_junit(&xml).with_context(|| format!("parse test lane JUnit at {}", report.display()))
}

/// What the report blames: the cases that failed every attempt, and the
/// retried passes no registry entry owns. `None` when it blames nothing.
fn named(cases: &[CaseTiming], known: &[KnownFlake], report: &Path) -> Option<String> {
    let failed = cases.iter().filter(|case| case.failed).collect::<Vec<_>>();
    let retried = unowned(cases, known);
    if failed.is_empty() && retried.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !failed.is_empty() {
        parts.push(format!(
            "{} test(s) failed on every attempt:\n{}",
            failed.len(),
            listing(&failed)
        ));
    }
    if !retried.is_empty() {
        parts.push(format!(
            "{} test(s) passed only on a retry, which is a defect that reproduced:\n{}",
            retried.len(),
            listing(&retried)
        ));
    }
    parts.push(format!(
        "Every failing attempt is printed above and kept in {}.",
        report.display()
    ));
    if !retried.is_empty() {
        parts.push(
            "Fix the defect, or name the test in `test.known_flakes` with the issue that owns it."
                .to_owned(),
        );
    }
    Some(parts.join("\n"))
}

/// nextest resolves `junit.path` against its own store directory, and that
/// store stays under the workspace root even when the build is sent elsewhere
/// through `CARGO_TARGET_DIR`.
fn report_path(root: &Path, profile: &str, relative: &str) -> PathBuf {
    root.join("target")
        .join("nextest")
        .join(profile)
        .join(relative)
}

/// The nextest profile a lane's command runs its tests under, or `None` when
/// the lane runs no nextest, or builds with `--no-run` and runs no test, and
/// so cannot retry anything.
///
/// `--profile` names the Cargo profile to `cargo test` and the runner profile
/// to `cargo nextest`, so it is only read past the `nextest` subcommand. The
/// last one wins, as it does on nextest's own command line.
fn nextest_profile(command: &Command) -> Option<String> {
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let start = args.iter().position(|arg| arg == "nextest")?;
    let mut profile = consts::DEFAULT_PROFILE.to_owned();
    let mut awaiting_value = false;
    for arg in args.iter().skip(start.saturating_add(1)) {
        if awaiting_value {
            profile = arg.clone();
            awaiting_value = false;
        } else if arg == "--no-run" {
            return None;
        } else if let Some(value) = arg.strip_prefix("--profile=") {
            profile = value.to_owned();
        } else {
            awaiting_value = matches!(arg.as_str(), "--profile" | "-P");
        }
    }
    Some(profile)
}

fn declared_profile(root: &Path, config: &TestCommandConfig, profile: &str) -> Result<Declared> {
    let path = root.join(&config.nextest_config);
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read nextest config: {}", path.display()))?;
    declaration(&text, profile)
        .with_context(|| format!("read nextest profile `{profile}` from {}", path.display()))
}

/// Reads one profile the way nextest resolves it: a key the profile does not
/// set is inherited from `default`.
fn declaration(config: &str, profile: &str) -> Result<Declared> {
    let config: toml::Table = toml::from_str(config).context("parse nextest config")?;
    let profiles = config.get("profile");
    let declared = |key: &str| -> Option<&Value> {
        let profiles = profiles?;
        profiles
            .get(profile)
            .and_then(|declared| declared.get(key))
            .or_else(|| {
                profiles
                    .get(consts::DEFAULT_PROFILE)
                    .and_then(|fallback| fallback.get(key))
            })
    };
    let retries = declared("retries").map_or(Ok(0), retry_count)?;
    let junit = declared("junit")
        .and_then(|junit| junit.get("path"))
        .map(|path| {
            path.as_str()
                .map(str::to_owned)
                .context("junit path is not a string")
        })
        .transpose()?;
    Ok(Declared { junit, retries })
}

/// nextest spells `retries` either as the count itself or as a table whose
/// `count` is that number with a backoff around it.
fn retry_count(value: &Value) -> Result<u64> {
    let count = match value {
        Value::Integer(count) => Some(*count),
        Value::Table(table) => table.get("count").and_then(Value::as_integer),
        _ => None,
    };
    let count = count.context("retries is neither a count nor a table declaring one")?;
    u64::try_from(count).context("retries is negative")
}

/// The retried passes the registry does not own.
fn unowned<'a>(cases: &'a [CaseTiming], known: &[KnownFlake]) -> Vec<&'a CaseTiming> {
    let owned = known
        .iter()
        .map(|flake| flake.test.as_str())
        .collect::<BTreeSet<_>>();
    cases
        .iter()
        .filter(|case| case.flaky && !owned.contains(case_id(case).as_str()))
        .collect()
}

/// `<suite>::<name>`: the identity the report gives a case, and the one a
/// registry entry names it by.
fn case_id(case: &CaseTiming) -> String {
    format!("{}::{}", case.suite, case.name)
}

fn listing(cases: &[&CaseTiming]) -> String {
    cases
        .iter()
        .map(|case| format!("  - {}", case_id(case)))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::verdict::ChildFailure;

    fn config(nextest_config: &str, known: &[(&str, &str)]) -> TestCommandConfig {
        TestCommandConfig {
            nextest_config: nextest_config.to_owned(),
            known_flakes: known
                .iter()
                .map(|(test, issue)| KnownFlake {
                    test: (*test).to_owned(),
                    issue: (*issue).to_owned(),
                })
                .collect(),
            ..TestCommandConfig::default()
        }
    }

    fn lane(nextest_config: &str, report: Option<&str>) -> TempDir {
        let temp = TempDir::new().expect("temp root");
        fs::write(temp.path().join("nextest.toml"), nextest_config).expect("write nextest config");
        if let Some(report) = report {
            let dir = temp.path().join("target").join("nextest").join("ci");
            fs::create_dir_all(&dir).expect("create store");
            fs::write(dir.join("junit.xml"), report).expect("write report");
        }
        temp
    }

    fn command(args: &[&str]) -> Command {
        let mut command = Command::new("cargo");
        command.args(args);
        command
    }

    fn judge(root: &Path, known: &[(&str, &str)], args: &[&str], code: Option<i32>) -> Result<()> {
        let config = config("nextest.toml", known);
        Evidence::of(root, &config, &command(args))?.verdict(
            "workspace",
            &config.known_flakes,
            code,
        )
    }

    fn exit_code(error: &anyhow::Error) -> Option<i32> {
        error
            .downcast_ref::<ChildFailure>()
            .map(ChildFailure::exit_code)
    }

    const CI: &[&str] = &["nextest", "run", "--profile", "ci"];

    #[test]
    fn a_retried_pass_fails_the_lane_that_reported_it() {
        for outcome in ["flakyFailure", "flakyError"] {
            let report = consts::RETRIED_PASS.replace("flakyFailure", outcome);
            let temp = lane(consts::RETRYING_PROFILE, Some(&report));

            let error = judge(temp.path(), &[], CI, Some(0))
                .expect_err("a retried pass is not a clean lane")
                .to_string();

            assert!(error.contains("passed only on a retry"), "{error}");
            assert!(
                error.contains("  - kithara_queue::delayed_target"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_registry_entry_owns_the_retried_pass_it_names() {
        let temp = lane(consts::RETRYING_PROFILE, Some(consts::RETRIED_PASS));

        judge(
            temp.path(),
            &[("kithara_queue::delayed_target", "https://example.test/1")],
            CI,
            Some(0),
        )
        .expect("an owned retried pass keeps the lane green");
    }

    #[test]
    fn a_profile_that_retries_without_a_report_cannot_be_judged() {
        let temp = lane("[profile.ci]\nretries = 1\n", None);

        let error = judge(temp.path(), &[], CI, Some(0))
            .expect_err("a retry that leaves no trace is unjudgeable")
            .to_string();

        assert!(error.contains("declares no JUnit report"), "{error}");
    }

    #[test]
    fn a_declared_report_the_lane_did_not_leave_fails_it() {
        let temp = lane(consts::RETRYING_PROFILE, None);

        let error = judge(temp.path(), &[], CI, Some(0))
            .expect_err("a missing report is a lane that presented no evidence")
            .to_string();

        assert!(error.contains("junit.xml"), "{error}");
    }

    /// A rebuild check repeats a suite with `--no-run`: nextest builds, runs
    /// no test, and writes no report, so the build's status is the verdict.
    #[test]
    fn a_build_that_runs_no_test_is_judged_by_its_status() {
        let temp = lane(consts::RETRYING_PROFILE, None);

        judge(
            temp.path(),
            &[],
            &["nextest", "run", "--profile", "ci", "--no-run"],
            Some(0),
        )
        .expect("a build that ran no test has no report to present");
    }

    #[test]
    fn a_profile_that_grants_no_retry_is_not_judged() {
        let temp = lane(consts::RETRYING_PROFILE, Some(consts::RETRIED_PASS));

        judge(temp.path(), &[], &["nextest", "run"], Some(0))
            .expect("the default profile retries nothing, so it hides nothing");
    }

    #[test]
    fn a_lane_that_runs_no_nextest_is_not_judged() {
        let temp = lane(consts::RETRYING_PROFILE, Some(consts::RETRIED_PASS));

        judge(temp.path(), &[], &["test", "--profile", "ci"], Some(0))
            .expect("`--profile` on `cargo test` names a Cargo profile");
    }

    #[test]
    fn a_red_lane_that_runs_no_nextest_keeps_its_code() {
        let temp = lane(consts::RETRYING_PROFILE, None);

        let error = judge(temp.path(), &[], &["test"], Some(3)).expect_err("the lane is red");

        assert_eq!(exit_code(&error), Some(3));
        assert_eq!(
            error.to_string(),
            "test lane `workspace` failed (exit code 3)"
        );
    }

    #[test]
    fn a_red_lane_names_the_tests_that_failed_and_those_that_passed_only_on_a_retry() {
        let temp = lane(consts::RETRYING_PROFILE, Some(consts::FAILED_AND_RETRIED));

        let error = judge(temp.path(), &[], CI, Some(100)).expect_err("the lane is red");

        assert_eq!(exit_code(&error), Some(100));
        let error = error.to_string();
        assert!(
            error.contains("1 test(s) failed on every attempt:\n  - kithara_queue::stalled_target"),
            "{error}"
        );
        assert!(
            error.contains("1 test(s) passed only on a retry, which is a defect that reproduced:\n  - kithara_queue::delayed_target"),
            "{error}"
        );
    }

    #[test]
    fn a_red_lane_without_a_report_did_not_reach_its_tests() {
        let temp = lane(consts::RETRYING_PROFILE, None);

        let error = judge(temp.path(), &[], CI, Some(101)).expect_err("the lane is red");

        assert_eq!(exit_code(&error), Some(101));
        let error = error.to_string();
        assert!(error.contains("left no test report"), "{error}");
    }

    #[test]
    fn a_red_lane_whose_report_blames_no_test_says_so() {
        let temp = lane(consts::RETRYING_PROFILE, Some(consts::RETRIED_PASS));

        let error = judge(
            temp.path(),
            &[("kithara_queue::delayed_target", "https://example.test/1")],
            CI,
            Some(100),
        )
        .expect_err("the lane is red")
        .to_string();

        assert!(error.contains("names no failed test"), "{error}");
    }

    #[test]
    fn a_red_lane_whose_report_cannot_be_read_keeps_its_code() {
        let temp = lane(consts::RETRYING_PROFILE, Some("<testsuites><testsuite"));

        let error = judge(temp.path(), &[], CI, Some(100)).expect_err("the lane is red");

        assert_eq!(exit_code(&error), Some(100));
        let error = error.to_string();
        assert!(error.contains("could not be read"), "{error}");
        assert!(error.contains("junit.xml"), "{error}");
    }

    #[test]
    fn clearing_removes_the_report_a_previous_run_left() {
        let temp = lane(consts::RETRYING_PROFILE, Some(consts::FAILED_AND_RETRIED));
        let evidence = Evidence::of(temp.path(), &config("nextest.toml", &[]), &command(CI))
            .expect("read the profile");
        let report = temp.path().join("target/nextest/ci/junit.xml");

        evidence.clear();
        assert!(!report.exists());
        evidence.clear();
        assert!(!report.exists());
    }

    #[test]
    fn a_profile_inherits_the_retry_count_it_does_not_set() {
        let declared = declaration("[profile.default]\nretries = 2\n\n[profile.ci]\n", "ci")
            .expect("read profile");

        assert_eq!(declared.retries, 2);
        assert_eq!(declared.junit, None);
    }

    #[test]
    fn retries_may_be_declared_as_a_table_around_its_count() {
        let declared = declaration(
            "[profile.ci]\nretries = { backoff = \"exponential\", count = 3 }\n",
            "ci",
        )
        .expect("read profile");

        assert_eq!(declared.retries, 3);
    }
}
