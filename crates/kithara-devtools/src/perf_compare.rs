use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

#[derive(Debug, clap::Args)]
pub struct PerfCompareArgs {
    /// Path to the baseline canonical result.
    pub baseline: PathBuf,
    /// Path to the current canonical result.
    pub current: PathBuf,
    /// Regression threshold percentage.
    #[arg(long, default_value_t = 10)]
    pub threshold: u32,
}

#[derive(Deserialize)]
struct Report {
    mode: String,
    cases: Vec<Case>,
}

#[derive(Clone, Deserialize)]
struct Case {
    identity: String,
    allocations: u64,
    allocated_bytes: u64,
}

fn read(path: &PathBuf) -> Result<BTreeMap<String, Case>> {
    let bytes =
        fs::read(path).with_context(|| format!("read canonical result {}", path.display()))?;
    let report: Report = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse canonical result {}", path.display()))?;
    ensure!(
        report.mode == "allocation",
        "{} is not an allocation-mode canonical result",
        path.display()
    );
    ensure!(!report.cases.is_empty(), "{} has no cases", path.display());
    let mut cases = BTreeMap::new();
    for case in report.cases {
        ensure!(
            cases.insert(case.identity.clone(), case).is_none(),
            "{} contains a duplicate case",
            path.display()
        );
    }
    Ok(cases)
}

/// Signed hundredths of a percent, or `None` for growth from a zero baseline.
fn change(baseline: u64, current: u64) -> Option<i128> {
    if baseline == 0 {
        return if current == 0 { Some(0) } else { None };
    }
    let difference = i128::from(current) - i128::from(baseline);
    Some(difference * 10_000 / i128::from(baseline))
}

fn display_change(change: Option<i128>) -> String {
    let Some(change) = change else {
        return "infinite%".to_owned();
    };
    let sign = if change < 0 { "-" } else { "" };
    let magnitude = change.unsigned_abs();
    format!("{sign}{}.{:02}%", magnitude / 100, magnitude % 100)
}

pub(crate) fn run(args: &PerfCompareArgs) -> Result<()> {
    let baseline = read(&args.baseline)?;
    let current = read(&args.current)?;
    ensure!(
        baseline.keys().eq(current.keys()),
        "baseline/current case inventories differ"
    );

    let mut regressions = Vec::new();
    for (identity, current) in &current {
        let baseline = &baseline[identity];
        for (metric, before, after) in [
            ("allocations", baseline.allocations, current.allocations),
            (
                "allocated_bytes",
                baseline.allocated_bytes,
                current.allocated_bytes,
            ),
        ] {
            let change = change(before, after);
            let rendered = display_change(change);
            println!("{identity} | {metric} | {before} | {after} | {rendered}");
            if change.is_none_or(|change| change > i128::from(args.threshold) * 100) {
                regressions.push(format!("{identity} {metric}: {rendered}"));
            }
        }
    }
    if regressions.is_empty() {
        println!("No significant regression detected");
        return Ok(());
    }
    bail!(
        "performance regression detected: {}",
        regressions.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &PathBuf, cases: &[(&str, u64, u64)]) {
        let cases: Vec<_> = cases
            .iter()
            .map(|(identity, allocations, bytes)| {
                serde_json::json!({
                    "identity": identity,
                    "allocations": allocations,
                    "allocated_bytes": bytes,
                })
            })
            .collect();
        fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({"mode":"allocation","cases":cases})).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn compares_exact_cases_and_rejects_regressions() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = directory.path().join("baseline.json");
        let current = directory.path().join("current.json");
        write(&baseline, &[("a", 2, 128), ("b", 0, 0)]);
        write(&current, &[("a", 2, 128), ("b", 0, 0)]);
        run(&PerfCompareArgs {
            baseline: baseline.clone(),
            current: current.clone(),
            threshold: 10,
        })
        .unwrap();
        write(&current, &[("a", 3, 128), ("b", 0, 0)]);
        assert!(
            run(&PerfCompareArgs {
                baseline,
                current,
                threshold: 10,
            })
            .is_err()
        );
    }

    #[test]
    fn missing_baseline_or_mismatched_cases_fail() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = directory.path().join("missing.json");
        let current = directory.path().join("current.json");
        write(&current, &[("a", 0, 0)]);
        assert!(
            run(&PerfCompareArgs {
                baseline: baseline.clone(),
                current: current.clone(),
                threshold: 10,
            })
            .is_err()
        );
        write(&baseline, &[("other", 0, 0)]);
        assert!(
            run(&PerfCompareArgs {
                baseline,
                current,
                threshold: 10,
            })
            .is_err()
        );
    }

    #[test]
    fn zero_baseline_cannot_hide_new_allocations() {
        let directory = tempfile::tempdir().unwrap();
        let baseline = directory.path().join("baseline.json");
        let current = directory.path().join("current.json");
        write(&baseline, &[("a", 0, 0)]);
        write(&current, &[("a", 1, 64)]);
        assert!(
            run(&PerfCompareArgs {
                baseline,
                current,
                threshold: 10,
            })
            .is_err()
        );
    }
}
