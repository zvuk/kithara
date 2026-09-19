//! Whether a lane that exited zero hid a failure inside a retry.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use toml::Value;

use crate::{
    common::project::{KnownFlake, TestCommandConfig},
    junit::{CaseTiming, parse_junit},
    stress_report::{MAX_JUNIT_BYTES, read_bounded_utf8},
};

/// The profile nextest runs when a command names none, and the profile every
/// other one inherits its unset keys from.
const DEFAULT_PROFILE: &str = "default";

/// What one nextest profile declares about hiding a failure.
struct Declared {
    /// Where the profile writes its report, relative to the profile's own
    /// store directory.
    junit: Option<String>,
    /// Attempts the profile grants after the first.
    retries: u64,
}

/// Fails a lane whose runner retried a test into a pass.
///
/// nextest keeps the failing attempt — its streams, its panic and its dump —
/// and exits zero, so a lane judged by its exit code alone reads a defect that
/// reproduced as a clean run. The retry makes that attempt legible; it is not
/// permission to pass. A profile granting no retry can hide nothing.
///
/// # Errors
///
/// Returns an error when the profile grants retries and keeps no report, when
/// the report it declares is absent, or when the report names a retried pass
/// that no `test.known_flakes` entry owns.
pub(crate) fn verdict(
    lane_name: &str,
    root: &Path,
    config: &TestCommandConfig,
    command: &Command,
) -> Result<()> {
    let Some(profile) = nextest_profile(command) else {
        return Ok(());
    };
    let declared = declared_profile(root, config, &profile)?;
    if declared.retries == 0 {
        return Ok(());
    }
    let Some(relative) = declared.junit else {
        bail!(
            "nextest profile `{profile}` grants {} retries and declares no JUnit report: a retried pass would leave nothing to judge the lane by",
            declared.retries
        );
    };
    let report = report_path(root, &profile, &relative);
    ensure!(
        report.is_file(),
        "test lane `{lane_name}` ran nextest profile `{profile}`, which declares a JUnit report at {}: without it a retried pass is unjudgeable",
        report.display()
    );
    let xml = read_bounded_utf8(&report, MAX_JUNIT_BYTES, "test lane JUnit")?;
    let cases = parse_junit(&xml)
        .with_context(|| format!("parse test lane JUnit at {}", report.display()))?;
    let retried = unowned(&cases, &config.known_flakes);
    if retried.is_empty() {
        return Ok(());
    }
    bail!(
        "test lane `{lane_name}` reports {} test(s) that passed only on a retry, which is a defect that reproduced:\n{}\
         Every failing attempt is printed above and kept in {}.\n\
         Fix the defect, or name the test in `test.known_flakes` with the issue that owns it.",
        retried.len(),
        listing(&retried),
        report.display()
    );
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

/// The nextest profile a lane's command selects, or `None` when the lane runs
/// no nextest and so cannot retry anything.
///
/// `--profile` names the Cargo profile to `cargo test` and the runner profile
/// to `cargo nextest`, so it is only read past the `nextest` subcommand. The
/// last one wins, as it does on nextest's own command line: a lane that names
/// a profile can still be asked for a different one.
fn nextest_profile(command: &Command) -> Option<String> {
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let start = args.iter().position(|arg| arg == "nextest")?;
    let mut profile = DEFAULT_PROFILE.to_owned();
    let mut awaiting_value = false;
    for arg in args.iter().skip(start.saturating_add(1)) {
        if awaiting_value {
            profile = arg.clone();
            awaiting_value = false;
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
                    .get(DEFAULT_PROFILE)
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
        .map(|case| format!("  - {}\n", case_id(case)))
        .collect()
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const RETRYING_PROFILE: &str = "\
[profile.default]
fail-fast = false

[profile.ci]
retries = 1

[profile.ci.junit]
path = \"junit.xml\"
";

    /// The shape nextest writes for a case it retried into a pass: the failing
    /// attempt lives inside `flakyFailure`, and the run reports no failures.
    const RETRIED_PASS: &str = "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<testsuites name=\"nextest-run\" tests=\"1\" failures=\"0\" errors=\"0\" uuid=\"1\" timestamp=\"t\" time=\"0.049\">
    <testsuite name=\"kithara_queue\" tests=\"1\" disabled=\"0\" errors=\"0\" failures=\"0\">
        <testcase name=\"delayed_target\" classname=\"kithara_queue\" time=\"0.019\">
            <flakyFailure message=\"panicked at delayed.rs:9\" type=\"test failure with exit code 101\">assertion failed</flakyFailure>
        </testcase>
    </testsuite>
</testsuites>
";

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

    #[test]
    fn a_retried_pass_fails_the_lane_that_reported_it() {
        let temp = lane(RETRYING_PROFILE, Some(RETRIED_PASS));

        let error = verdict(
            "workspace",
            temp.path(),
            &config("nextest.toml", &[]),
            &command(&["nextest", "run", "--profile", "ci"]),
        )
        .expect_err("a retried pass is not a clean lane");

        let error = error.to_string();
        assert!(
            error.contains("kithara_queue::delayed_target"),
            "the verdict names the retried test: {error}"
        );
    }

    #[test]
    fn a_registry_entry_owns_the_retried_pass_it_names() {
        let temp = lane(RETRYING_PROFILE, Some(RETRIED_PASS));

        verdict(
            "workspace",
            temp.path(),
            &config(
                "nextest.toml",
                &[("kithara_queue::delayed_target", "https://example.test/1")],
            ),
            &command(&["nextest", "run", "--profile", "ci"]),
        )
        .expect("an owned retried pass keeps the lane green");
    }

    #[test]
    fn a_profile_that_retries_without_a_report_cannot_be_judged() {
        let temp = lane("[profile.ci]\nretries = 1\n", None);

        let error = verdict(
            "workspace",
            temp.path(),
            &config("nextest.toml", &[]),
            &command(&["nextest", "run", "--profile", "ci"]),
        )
        .expect_err("a retry that leaves no trace is unjudgeable")
        .to_string();

        assert!(error.contains("declares no JUnit report"), "{error}");
    }

    #[test]
    fn a_declared_report_the_lane_did_not_leave_fails_it() {
        let temp = lane(RETRYING_PROFILE, None);

        let error = verdict(
            "workspace",
            temp.path(),
            &config("nextest.toml", &[]),
            &command(&["nextest", "run", "--profile", "ci"]),
        )
        .expect_err("a missing report is a lane that presented no evidence")
        .to_string();

        assert!(error.contains("junit.xml"), "{error}");
    }

    #[test]
    fn a_profile_that_grants_no_retry_is_not_judged() {
        let temp = lane(RETRYING_PROFILE, Some(RETRIED_PASS));

        verdict(
            "workspace",
            temp.path(),
            &config("nextest.toml", &[]),
            &command(&["nextest", "run"]),
        )
        .expect("the default profile retries nothing, so it hides nothing");
    }

    #[test]
    fn a_lane_that_runs_no_nextest_is_not_judged() {
        let temp = lane(RETRYING_PROFILE, Some(RETRIED_PASS));

        verdict(
            "browser",
            temp.path(),
            &config("nextest.toml", &[]),
            &command(&["test", "--profile", "ci"]),
        )
        .expect("`--profile` on `cargo test` names a Cargo profile");
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
