use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Stdio,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{NextestAction, ResolvedLane};
use crate::{consts, verdict::ChildFailure};

pub(super) fn admit_expected(
    lane: &ResolvedLane,
    caller: &[String],
    root: &Path,
    expected_path: &Path,
) -> Result<()> {
    let expected = read_expected(expected_path)?;
    let args = inventory_args(caller)?;
    let mut command = lane.command(NextestAction::List, &args)?;
    let output = command
        .current_dir(root)
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("list exact selection for test lane `{}`", lane.lane))?;
    if !output.status.success() {
        return Err(ChildFailure::inherited(
            format!("test lane `{}` inventory", lane.lane),
            output.status.code(),
        ));
    }
    let json = String::from_utf8(output.stdout).context("nextest inventory is not UTF-8")?;
    let selected = selected_inventory(decode_inventory(&json)?)?;
    if expected != selected {
        let missing = expected.difference(&selected).collect::<Vec<_>>();
        let unexpected = selected.difference(&expected).collect::<Vec<_>>();
        let rows = |tests: &[&TestId]| {
            tests
                .iter()
                .map(|(binary, name)| format!("{binary}\t{name}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        bail!(
            "test selection differs from {}: expected {}, selected {}\nmissing ({}):\n{}\nunexpected ({}):\n{}",
            expected_path.display(),
            expected.len(),
            selected.len(),
            missing.len(),
            rows(&missing),
            unexpected.len(),
            rows(&unexpected),
        );
    }
    Ok(())
}

fn read_expected(path: &Path) -> Result<BTreeSet<TestId>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read --expected-tests identities at {}", path.display()))?;
    let mut expected = BTreeSet::new();
    for (index, row) in text.lines().enumerate() {
        let line = index.saturating_add(1);
        let Some((binary, name)) = row.split_once('\t') else {
            bail!("--expected-tests line {line} needs binary ID, tab, full test name");
        };
        for value in [binary, name] {
            if value.is_empty()
                || value.trim() != value
                || !value.is_ascii()
                || value.chars().any(char::is_control)
            {
                bail!("--expected-tests line {line} contains an empty or malformed identity");
            }
        }
        if !expected.insert((binary.to_owned(), name.to_owned())) {
            bail!("--expected-tests line {line} duplicates `{binary}\t{name}`");
        }
        validate_inventory_case_count(expected.len())?;
    }
    if expected.is_empty() {
        bail!("--expected-tests contains no identities");
    }
    Ok(expected)
}

pub(crate) fn inventory_args(caller: &[String]) -> Result<Vec<String>> {
    const FLAGS: &[&str] = &[
        "--no-run",
        "--fail-fast",
        "--ff",
        "--no-fail-fast",
        "--nff",
        "--no-capture",
        "--no-output-indent",
        "--hide-progress-bar",
        "--no-input-handler",
    ];
    const VALUES: &[&str] = &[
        "-j",
        "--test-threads",
        "--jobs",
        "--retries",
        "--flaky-result",
        "--max-fail",
        "--debugger",
        "--tracer",
        "--no-tests",
        "--stress-count",
        "--stress-duration",
        "--failure-output",
        "--success-output",
        "--status-level",
        "--final-status-level",
        "--show-progress",
        "--max-progress-running",
        "--message-format",
        "--message-format-version",
    ];
    const SHARED_VALUES: &[&str] = &[
        "-p",
        "--package",
        "--exclude",
        "--bin",
        "--example",
        "--test",
        "--bench",
        "-F",
        "--features",
        "--build-jobs",
        "--cargo-profile",
        "--target",
        "--target-dir",
        "--manifest-path",
        "--cargo-message-format",
        "--config",
        "-Z",
        "--partition",
        "--platform-filter",
        "-E",
        "--filterset",
        "--filter-expr",
        "-P",
        "--profile",
        "--config-file",
        "--user-config-file",
        "--tool-config-file",
        "--archive-file",
        "--archive-format",
        "--extract-to",
        "--cargo-metadata",
        "--workspace-remap",
        "--binaries-metadata",
        "--target-dir-remap",
        "--build-dir-remap",
        "--color",
    ];
    let mut args = vec!["--message-format=json".to_owned()];
    let mut caller = caller.iter();
    while let Some(arg) = caller.next() {
        if arg == "--" {
            args.push(arg.clone());
            while let Some(arg) = caller.next() {
                if matches!(arg.as_str(), "--ignored" | "--include-ignored") {
                    bail!("--expected-tests supports only the default non-ignored test selection");
                }
                args.push(arg.clone());
                if arg == "--skip" {
                    args.push(caller.next().context("`--skip` needs a value")?.clone());
                }
            }
            break;
        }
        if matches!(arg.as_str(), "--ignored" | "--include-ignored") {
            bail!("--expected-tests supports only the default non-ignored test selection");
        }
        if arg == "--rerun" || arg.starts_with("--rerun=") || arg.starts_with("-R") {
            bail!(
                "--expected-tests cannot be combined with --rerun: List cannot enumerate that selection"
            );
        }
        if arg == "--run-ignored" {
            let mode = caller
                .next()
                .context("--expected-tests needs a --run-ignored mode")?;
            if mode != "default" {
                bail!("--expected-tests supports only the default non-ignored test selection");
            }
            args.extend([arg.clone(), mode.clone()]);
            continue;
        }
        if let Some(mode) = arg.strip_prefix("--run-ignored=")
            && mode != "default"
        {
            bail!("--expected-tests supports only the default non-ignored test selection");
        }
        let (flag, attached) = arg
            .split_once('=')
            .map_or((arg.as_str(), false), |(flag, _)| (flag, true));
        if SHARED_VALUES.contains(&flag) {
            args.push(arg.clone());
            if !attached {
                args.push(
                    caller
                        .next()
                        .with_context(|| format!("`{arg}` needs a value"))?
                        .clone(),
                );
            }
        } else if VALUES.contains(&flag) {
            if !attached {
                caller
                    .next()
                    .with_context(|| format!("`{arg}` needs a value"))?;
            }
        } else if !FLAGS.contains(&flag)
            && !arg.strip_prefix("-j").is_some_and(|value| {
                value == "num-cpus"
                    || (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            })
        {
            args.push(arg.clone());
        }
    }
    Ok(args)
}

pub(crate) type TestId = (String, String);

#[derive(Debug, Deserialize)]
struct Inventory {
    #[serde(rename = "rust-suites")]
    rust_suites: BTreeMap<String, InventorySuite>,
}

#[derive(Debug, Deserialize)]
struct InventorySuite {
    testcases: BTreeMap<String, InventoryCase>,
    #[serde(rename = "binary-id")]
    binary_id: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct InventoryCase {
    #[serde(rename = "filter-match")]
    filter_match: InventoryMatch,
    ignored: bool,
}

#[derive(Debug, Deserialize)]
struct InventoryMatch {
    status: String,
}

/// A target nextest did not list is inventoried with a skip status and none of its cases, whether
/// the project's `default-filter` excluded it or the lane's own filter never reached its binary;
/// the suite is dropped whole rather than filtered case by case.
pub(crate) fn parse_inventory(json: &str) -> Result<BTreeSet<TestId>> {
    let inventory = decode_inventory(json)?;
    if inventory.rust_suites.is_empty() {
        bail!("stress inventory contains no Rust test suites");
    }
    let tests = selected_inventory(inventory)?;
    if tests.is_empty() {
        bail!("stress inventory contains no runnable selected tests");
    }
    Ok(tests)
}

fn decode_inventory(json: &str) -> Result<Inventory> {
    serde_json::from_str(json).context("parse stress inventory JSON")
}

fn selected_inventory(inventory: Inventory) -> Result<BTreeSet<TestId>> {
    let mut tests = BTreeSet::new();
    let mut inventory_cases = 0usize;
    for (suite, inventory) in inventory.rust_suites {
        if suite.trim().is_empty() || inventory.binary_id.trim().is_empty() {
            bail!("stress inventory contains an empty suite identity");
        }
        if suite != inventory.binary_id {
            bail!("stress inventory suite key does not match its binary-id");
        }
        match inventory.status.as_str() {
            "listed" => {}
            "skipped-default-filter" | "skipped" => continue,
            status => bail!("stress inventory suite `{suite}` has unsupported status `{status}`"),
        }
        for (name, case) in inventory.testcases {
            inventory_cases = inventory_cases.saturating_add(1);
            validate_inventory_case_count(inventory_cases)?;
            if name.trim().is_empty() {
                bail!("stress inventory contains an empty test name");
            }
            match case.filter_match.status.as_str() {
                "matches" if !case.ignored => {
                    tests.insert((suite.clone(), name));
                }
                "matches" | "mismatch" => {}
                status => bail!("stress inventory contains unknown filter status `{status}`"),
            }
        }
    }
    Ok(tests)
}

pub(crate) fn validate_inventory_case_count(count: usize) -> Result<()> {
    if count > consts::MAX_INVENTORY_CASES {
        bail!(
            "stress inventory exceeds the deterministic limit of {MAX_INVENTORY_CASES} testcases",
            MAX_INVENTORY_CASES = consts::MAX_INVENTORY_CASES
        );
    }
    Ok(())
}
