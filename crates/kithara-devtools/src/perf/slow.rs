use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    common::project::ProjectConfig,
    perf::{
        junit::{CaseTiming, parse_junit},
        lanes::{RunPaths, primary_lane_name},
    },
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
    runs: &[(String, Vec<CaseTiming>)],
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
            if case.failed {
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
            .filter(|stat| stat.median_ms >= threshold_ms)
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
        "# Slow tests (median >= {:.0}ms in any lane) - {}\n\n",
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

pub(crate) fn run(
    project: &ProjectConfig,
    data_dir: &Path,
    run_id: &str,
    threshold_ms: f64,
) -> Result<()> {
    let primary_lane = primary_lane_name(&project.perf)?;
    let paths = RunPaths::new(data_dir, run_id);
    let matrix_dir = paths.matrix_dir();
    if !matrix_dir.is_dir() {
        bail!("no matrix data at {}", matrix_dir.display());
    }
    let mut runs = Vec::new();
    for lane_entry in fs::read_dir(&matrix_dir).context("read matrix dir")? {
        let lane_dir = lane_entry.context("lane entry")?.path();
        let lane = lane_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !lane_dir.is_dir() {
            continue;
        }
        for rep_entry in fs::read_dir(&lane_dir).context("read lane dir")? {
            let junit = rep_entry.context("rep entry")?.path().join("junit.xml");
            if !junit.is_file() {
                continue;
            }
            let xml =
                fs::read_to_string(&junit).with_context(|| format!("read {}", junit.display()))?;
            runs.push((lane.clone(), parse_junit(&xml)?));
        }
    }
    let report = aggregate(&runs, run_id, threshold_ms, &primary_lane);
    fs::write(paths.slow_json(), serde_json::to_vec_pretty(&report)?).context("write slow.json")?;
    fs::write(paths.slow_md(), render_md(&report)).context("write slow.md")?;
    println!(
        "[perf slow] {} tests >= {:.0}ms -> {}",
        report.tests.len(),
        threshold_ms,
        paths.slow_md().display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::junit::CaseTiming;

    fn case(suite: &str, name: &str, secs: f64) -> CaseTiming {
        CaseTiming {
            secs,
            suite: suite.into(),
            name: name.into(),
            failed: false,
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
}
