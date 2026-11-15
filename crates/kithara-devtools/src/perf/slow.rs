use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    fs,
    path::Path,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::{
    evidence::{self, LaneCases},
    lanes::RunPaths,
};

type TestKey = (String, String);
type LaneSamples = BTreeMap<String, (Vec<f64>, u32)>;
type TestSamples = BTreeMap<TestKey, LaneSamples>;

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct LaneStat {
    pub(crate) max_ms: f64,
    pub(crate) median_ms: f64,
    pub(crate) min_ms: f64,
    pub(crate) failed: u32,
    pub(crate) repeats: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SlowTest {
    pub(crate) lanes: BTreeMap<String, LaneStat>,
    pub(crate) name: String,
    pub(crate) suite: String,
    pub(crate) rank_score: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SlowReport {
    pub(crate) primary_lane: String,
    pub(crate) run_id: String,
    pub(crate) tests: Vec<SlowTest>,
    pub(crate) threshold_ms: f64,
}

pub(crate) fn median_ms(sorted_secs: &[f64]) -> f64 {
    let n = sorted_secs.len();
    let ms = |s: f64| s * 1000.0;
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        ms(sorted_secs[n / 2])
    } else {
        ms((sorted_secs[n / 2 - 1] + sorted_secs[n / 2]) / 2.0)
    }
}

pub(crate) fn aggregate(
    runs: &[LaneCases],
    run_id: &str,
    threshold_ms: f64,
    primary_lane: &str,
) -> SlowReport {
    let mut by_test: TestSamples = BTreeMap::new();
    for (lane, cases) in runs {
        for case in cases {
            let entry = by_test
                .entry((case.suite.clone(), case.name.clone()))
                .or_default()
                .entry(lane.clone())
                .or_insert_with(|| (Vec::new(), 0));
            entry.0.push(case.secs);
            if case.failing() {
                entry.1 += 1;
            }
        }
    }
    let mut tests = Vec::new();
    for ((suite, name), lanes_raw) in by_test {
        let mut lanes = BTreeMap::new();
        for (lane, (mut secs, failed)) in lanes_raw {
            secs.sort_by(f64::total_cmp);
            lanes.insert(
                lane,
                LaneStat {
                    failed,
                    median_ms: median_ms(&secs),
                    min_ms: secs.first().copied().unwrap_or(0.0) * 1000.0,
                    max_ms: secs.last().copied().unwrap_or(0.0) * 1000.0,
                    repeats: u32::try_from(secs.len()).unwrap_or(u32::MAX),
                },
            );
        }
        let over = lanes
            .values()
            .filter(|stat| stat.max_ms >= threshold_ms)
            .count();
        if over == 0 {
            continue;
        }
        let over = u32::try_from(over).unwrap_or(u32::MAX);
        let primary = lanes.get(primary_lane).map_or(0.0, |stat| stat.median_ms);
        tests.push(SlowTest {
            suite,
            name,
            lanes,
            rank_score: primary * f64::from(over),
        });
    }
    tests.sort_by(|a, b| {
        b.rank_score
            .total_cmp(&a.rank_score)
            .then_with(|| a.name.cmp(&b.name))
    });
    SlowReport {
        threshold_ms,
        tests,
        run_id: run_id.to_owned(),
        primary_lane: primary_lane.to_owned(),
    }
}

pub(crate) fn render_md(report: &SlowReport) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "# Slow tests (maximum >= {}ms in any lane) - {}\n\nSelection, source, host and invocation configuration: [matrix.json](matrix.json).\n\nThis report covers the declared selection; clock classification and timing exemptions require separate evidence.\n\n",
        report.threshold_ms, report.run_id
    );
    let lane_names: Vec<String> = report
        .tests
        .iter()
        .flat_map(|test| test.lanes.keys().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let header = |out: &mut String| {
        out.push_str("| test | rank |");
        for lane in &lane_names {
            let _ = write!(out, " {lane} med/min/max ms |");
        }
        out.push('\n');
        out.push_str("|---|---|");
        for _ in &lane_names {
            out.push_str("---|");
        }
        out.push('\n');
    };
    let row = |out: &mut String, test: &SlowTest| {
        let _ = write!(
            out,
            "| {} {} | {:.0} |",
            test.suite, test.name, test.rank_score
        );
        for lane in &lane_names {
            match test.lanes.get(lane) {
                Some(stat) => {
                    let _ = write!(
                        out,
                        " {:.0}/{:.0}/{:.0}{} |",
                        stat.median_ms,
                        stat.min_ms,
                        stat.max_ms,
                        if stat.failed > 0 { " FAIL" } else { "" }
                    );
                }
                None => out.push_str(" - |"),
            }
        }
        out.push('\n');
    };
    let _ = write!(out, "## Top 20 of {}\n\n", report.tests.len());
    header(&mut out);
    for test in report.tests.iter().take(20) {
        row(&mut out, test);
    }
    out.push_str("\n## Full list\n\n");
    header(&mut out);
    for test in &report.tests {
        row(&mut out, test);
    }
    out
}

pub(crate) fn run(data_dir: &Path, run_id: &str, threshold_ms: f64, check: bool) -> Result<()> {
    validate_threshold(threshold_ms)?;
    let paths = RunPaths::new(data_dir, run_id);
    let (plan, runs) = evidence::read_matrix(&paths, run_id)?;
    let report = aggregate(&runs, run_id, threshold_ms, &plan.primary_lane);
    fs::write(paths.slow_json(), serde_json::to_vec_pretty(&report)?).context("write slow.json")?;
    fs::write(paths.slow_md(), render_md(&report)).context("write slow.md")?;
    println!(
        "[perf slow] complete declared selection: {} lanes, {} invocations per lane; {} tests reached {}ms -> {}",
        plan.lanes.len(),
        plan.repeats,
        report.tests.len(),
        threshold_ms,
        paths.slow_md().display()
    );
    println!(
        "[perf slow] selection and provenance: {}",
        paths.run_dir.join("matrix.json").display()
    );
    if check {
        for test in &report.tests {
            for (lane, stat) in &test.lanes {
                if stat.max_ms >= threshold_ms {
                    println!(
                        "[perf check] {} {} in {}: maximum {:.6}ms",
                        test.suite, test.name, lane, stat.max_ms
                    );
                }
            }
        }
        ensure!(
            report.tests.is_empty(),
            "selected measurement threshold was reached or exceeded"
        );
    }
    Ok(())
}

fn validate_threshold(threshold_ms: f64) -> Result<()> {
    ensure!(
        threshold_ms.is_finite() && threshold_ms > 0.0,
        "performance threshold must be finite and greater than zero"
    );
    Ok(())
}

pub(crate) fn load_report(paths: &RunPaths, run_id: &str) -> Result<SlowReport> {
    evidence::component(run_id)?;
    let cached: SlowReport = serde_json::from_str(&crate::stress_report::read_bounded_utf8(
        &paths.slow_json(),
        crate::consts::MAX_JUNIT_BYTES,
        "slow report",
    )?)
    .context("parse slow report settings")?;
    ensure!(cached.run_id == run_id, "slow report run identity mismatch");
    validate_threshold(cached.threshold_ms)?;
    let (plan, runs) = evidence::read_matrix(paths, run_id)?;
    Ok(aggregate(
        &runs,
        run_id,
        cached.threshold_ms,
        &plan.primary_lane,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::junit::CaseTiming;

    fn case(suite: &str, name: &str, secs: f64) -> CaseTiming {
        CaseTiming {
            secs,
            flaky: false,
            suite: suite.into(),
            name: name.into(),
            iteration: None,
            failed: false,
            timestamp: None,
            output: String::new(),
            output_truncated: false,
        }
    }

    #[test]
    fn aggregates_median_and_ranks() {
        let primary = "flash-on-http";
        let runs = vec![
            (
                primary.to_owned(),
                vec![case("s", "slow_t", 2.0), case("s", "fast_t", 0.1)],
            ),
            (
                primary.to_owned(),
                vec![case("s", "slow_t", 3.0), case("s", "fast_t", 0.1)],
            ),
            (
                primary.to_owned(),
                vec![case("s", "slow_t", 2.5), case("s", "fast_t", 0.1)],
            ),
            ("flash-off-http".to_owned(), vec![case("s", "slow_t", 4.0)]),
        ];
        let report = aggregate(&runs, "run-1", 1000.0, primary);

        assert_eq!(report.tests.len(), 1);
        let t = &report.tests[0];
        assert_eq!(t.name, "slow_t");
        let on = &t.lanes[primary];
        assert!((on.median_ms - 2500.0).abs() < 1e-6);
        assert!((on.min_ms - 2000.0).abs() < 1e-6);
        assert_eq!(on.repeats, 3);
        assert!((t.rank_score - 5000.0).abs() < 1e-6);
    }

    #[test]
    fn median_even_count_averages() {
        assert!((median_ms(&[1.0, 3.0]) - 2000.0).abs() < 1e-6);
        assert!((median_ms(&[1.0, 2.0, 4.0]) - 2000.0).abs() < 1e-6);
    }

    #[test]
    fn a_slow_repetition_is_retained_behind_a_fast_median() {
        let lane = "flash-on-http";
        let runs =
            [0.1, 0.1, 1.5].map(|seconds| (lane.to_owned(), vec![case("s", "outlier", seconds)]));
        let report = aggregate(&runs, "run-1", 1000.0, lane);

        assert_eq!(report.tests.len(), 1);
        let measured = &report.tests[0].lanes[lane];
        assert_eq!(measured.repeats, 3);
        assert_eq!(measured.min_ms, 100.0);
        assert_eq!(measured.median_ms, 100.0);
        assert_eq!(measured.max_ms, 1500.0);
    }

    #[test]
    fn an_exact_threshold_repetition_is_retained() {
        let lane = "flash-on-http";
        let runs =
            [0.1, 0.1, 1.0].map(|seconds| (lane.to_owned(), vec![case("s", "boundary", seconds)]));
        let report = aggregate(&runs, "run-1", 1000.0, lane);

        assert_eq!(report.tests.len(), 1);
        assert_eq!(report.tests[0].lanes[lane].max_ms, 1000.0);
    }

    #[test]
    fn a_retried_slow_case_is_not_counted_as_clean() {
        let lane = "flash-on-http";
        let mut retried = case("s", "retried", 2.0);
        retried.flaky = true;
        let report = aggregate(&[(lane.to_owned(), vec![retried])], "run-1", 1000.0, lane);

        assert_eq!(report.tests[0].lanes[lane].failed, 1);
    }

    #[test]
    fn an_empty_matrix_does_not_establish_complete_evidence() {
        let temp = tempfile::tempdir().expect("matrix fixture");
        let paths = RunPaths::new(temp.path(), "empty");
        fs::create_dir_all(paths.matrix_dir()).expect("empty matrix directory");
        let outcome = run(temp.path(), "empty", 1000.0, false);

        assert!(outcome.is_err(), "an empty matrix must not look complete");
        assert!(!paths.slow_json().exists());
    }
}
