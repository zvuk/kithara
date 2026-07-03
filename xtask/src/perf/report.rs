use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result};

use crate::perf::{
    gecko::{ProfileSummary, summarize},
    lanes::RunPaths,
    profile::IsolatedRun,
    slow::SlowReport,
};

pub(crate) fn render_report(
    slow: &SlowReport,
    isolated: &[IsolatedRun],
    summaries: &BTreeMap<(String, String), ProfileSummary>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Test-suite perf report - {}\n\n", slow.run_id));
    let mut class_totals: BTreeMap<&str, f64> = BTreeMap::new();
    let (mut total_on, mut total_off) = (0.0, 0.0);
    for summary in summaries.values() {
        total_on += summary.on_cpu_ms;
        total_off += summary.off_cpu_ms;
        for wait in &summary.waits {
            *class_totals.entry(wait.class.as_str()).or_default() += wait.ms;
        }
    }
    out.push_str("## Totals across profiled tests\n\n");
    out.push_str(&format!(
        "- on-CPU: {:.0}ms, off-CPU (waiting): {:.0}ms\n\n",
        total_on, total_off
    ));
    out.push_str("| wait class | total ms |\n|---|---|\n");
    let mut classes: Vec<_> = class_totals.into_iter().collect();
    classes.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (class, ms) in classes {
        out.push_str(&format!("| {class} | {ms:.0} |\n"));
    }
    let iso: BTreeMap<(String, String), &IsolatedRun> = isolated
        .iter()
        .map(|run| ((run.suite.clone(), run.name.clone()), run))
        .collect();
    out.push_str("\n## Top-20 cards\n");
    for test in slow.tests.iter().take(20) {
        let key = (test.suite.clone(), test.name.clone());
        out.push_str(&format!("\n### {} {}\n\n", test.suite, test.name));
        for (lane, stat) in &test.lanes {
            out.push_str(&format!(
                "- {lane}: median {:.0}ms (min {:.0} / max {:.0}, {} reps{})\n",
                stat.median_ms,
                stat.min_ms,
                stat.max_ms,
                stat.repeats,
                if stat.failed > 0 { ", FAILURES" } else { "" }
            ));
        }
        if let Some(run) = iso.get(&key) {
            out.push_str(&format!(
                "- isolated: {:.1}s exit={:?}{}\n",
                run.secs,
                run.exit_code,
                if run.timed_out { " TIMED OUT" } else { "" }
            ));
        }
        if let Some(summary) = summaries.get(&key) {
            out.push_str(&format!(
                "- on-CPU {:.0}ms / off-CPU {:.0}ms\n",
                summary.on_cpu_ms, summary.off_cpu_ms
            ));
            for wait in summary.waits.iter().take(5) {
                out.push_str(&format!(
                    "  - wait {} {:.0}ms @ {}\n",
                    wait.class, wait.ms, wait.attributed
                ));
            }
            for frame in summary.cpu.iter().take(5) {
                out.push_str(&format!("  - cpu {:.0}ms @ {}\n", frame.ms, frame.frame));
            }
        } else {
            out.push_str("- (no profile captured)\n");
        }
    }
    out.push_str("\n## Ranked candidates\n\n| rank score | test |\n|---|---|\n");
    for test in &slow.tests {
        out.push_str(&format!(
            "| {:.0} | {} {} |\n",
            test.rank_score, test.suite, test.name
        ));
    }
    out
}

pub(crate) fn run(data_dir: &Path, run_id: &str, lane: &str) -> Result<()> {
    let paths = RunPaths::new(data_dir, run_id);
    let slow: SlowReport =
        serde_json::from_str(&fs::read_to_string(paths.slow_json()).context("read slow.json")?)
            .context("parse slow.json")?;
    let iso_path = paths.profiles_dir(lane).join("isolated.json");
    let isolated: Vec<IsolatedRun> = serde_json::from_str(
        &fs::read_to_string(&iso_path).with_context(|| format!("read {}", iso_path.display()))?,
    )
    .context("parse isolated.json")?;
    let mut summaries = BTreeMap::new();
    for run in &isolated {
        if run.timed_out || !run.profile_path.is_file() {
            continue;
        }
        let json = match fs::read_to_string(&run.profile_path) {
            Ok(json) => json,
            Err(err) => {
                println!(
                    "[perf report] skip profile {} ({err:#})",
                    run.profile_path.display()
                );
                continue;
            }
        };
        match summarize(&json, 20) {
            Ok(summary) => {
                summaries.insert((run.suite.clone(), run.name.clone()), summary);
            }
            Err(err) => println!(
                "[perf report] skip profile {} ({err:#})",
                run.profile_path.display()
            ),
        }
    }
    fs::write(
        paths.report_md(),
        render_report(&slow, &isolated, &summaries),
    )
    .context("write report.md")?;
    println!("[perf report] wrote {}", paths.report_md().display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::perf::{
        gecko::{ProfileSummary, WaitBucket},
        profile::IsolatedRun,
        slow::{LaneStat, SlowReport, SlowTest},
    };

    #[test]
    fn renders_cards_and_ranking() {
        let mut lanes = BTreeMap::new();
        lanes.insert(
            "flash-on-wreq".to_owned(),
            LaneStat {
                median_ms: 2500.0,
                min_ms: 2000.0,
                max_ms: 3000.0,
                repeats: 3,
                failed: 0,
            },
        );
        let slow = SlowReport {
            run_id: "run-1".to_owned(),
            threshold_ms: 1000.0,
            primary_lane: "flash-on-wreq".to_owned(),
            tests: vec![SlowTest {
                suite: "s".to_owned(),
                name: "slow_t".to_owned(),
                lanes,
                rank_score: 2500.0,
            }],
        };
        let isolated = vec![IsolatedRun {
            suite: "s".to_owned(),
            name: "slow_t".to_owned(),
            secs: 2.1,
            exit_code: Some(0),
            timed_out: false,
            profile_path: "/p".into(),
        }];
        let mut summaries = BTreeMap::new();
        summaries.insert(
            ("s".to_owned(), "slow_t".to_owned()),
            ProfileSummary {
                on_cpu_ms: 100.0,
                off_cpu_ms: 2000.0,
                waits: vec![WaitBucket {
                    class: "condvar".to_owned(),
                    attributed: "kithara_x::y".to_owned(),
                    ms: 1800.0,
                }],
                cpu: vec![],
            },
        );
        let out = render_report(&slow, &isolated, &summaries);
        assert!(out.contains("slow_t"));
        assert!(out.contains("condvar"));
        assert!(out.contains("kithara_x::y"));
        assert!(out.contains("## Ranked candidates"));
    }
}
