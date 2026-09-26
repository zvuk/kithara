use std::{collections::BTreeMap, fmt::Write as _, ops::ControlFlow, path::Path};

use serde::Deserialize;

use super::{
    AttemptDossier, attempt::AttemptKey, duration_ms,
    line_reader::for_each_bounded_line_with_limit, parse_timestamp_ms,
};
use crate::{consts, junit::CaseTiming};

#[derive(Debug, Deserialize)]
struct PressureSample {
    metrics: BTreeMap<String, String>,
    load1: Option<f64>,
    primary_exit_code: Option<i32>,
    sampler_healthy: Option<bool>,
    marker: PressureMarker,
    schema: String,
    timestamp_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum PressureMarker {
    Start,
    Sample,
    End,
}

#[derive(Debug, Default)]
struct PressureSummary {
    counters: BTreeMap<String, CounterWindow>,
    psi: BTreeMap<String, f64>,
    first_timestamp_ms: Option<u64>,
    last_timestamp_ms: Option<u64>,
    max_load: f64,
    counter_regressions: usize,
    malformed: usize,
    nonmonotonic: usize,
    samples: usize,
    structure_errors: usize,
}

#[derive(Debug)]
struct CounterWindow {
    first: u64,
    last: u64,
}

#[derive(Debug)]
pub(super) struct PressurePoint {
    psi: BTreeMap<String, f64>,
    load1: Option<f64>,
    timestamp_ms: u64,
}

pub(super) fn append(out: &mut String, path: &Path) -> (Vec<PressurePoint>, bool) {
    const MAX_PRESSURE_RECORDS: usize = 100_000;

    let mut summary = PressureSummary::default();
    let mut points = Vec::new();
    let mut records = 0usize;
    let mut previous_timestamp = None::<u64>;
    let mut starts = 0usize;
    let mut ends = 0usize;
    let mut healthy_end = false;
    let mut last_marker = None::<PressureMarker>;
    let line_bytes = consts::PRESSURE_LINE_BYTES;
    let read = for_each_bounded_line_with_limit(path, line_bytes, MAX_PRESSURE_RECORDS, |line| {
        let Ok(sample) = serde_json::from_str::<PressureSample>(line) else {
            summary.malformed = summary.malformed.saturating_add(1);
            return ControlFlow::Continue(());
        };
        if sample.schema != consts::SCHEMA
            || sample
                .load1
                .is_some_and(|load| !load.is_finite() || load < 0.0)
        {
            summary.malformed = summary.malformed.saturating_add(1);
            return ControlFlow::Continue(());
        }
        if previous_timestamp.is_some_and(|previous| sample.timestamp_ms <= previous) {
            summary.nonmonotonic = summary.nonmonotonic.saturating_add(1);
            return ControlFlow::Continue(());
        }
        previous_timestamp = Some(sample.timestamp_ms);
        records = records.saturating_add(1);
        last_marker = Some(sample.marker);

        match sample.marker {
            PressureMarker::Start => {
                starts = starts.saturating_add(1);
                if records != 1 || starts != 1 || ends != 0 {
                    summary.structure_errors = summary.structure_errors.saturating_add(1);
                }
            }
            PressureMarker::Sample => {
                if starts != 1 || ends != 0 {
                    summary.structure_errors = summary.structure_errors.saturating_add(1);
                }
            }
            PressureMarker::End => {
                ends = ends.saturating_add(1);
                healthy_end = sample.sampler_healthy == Some(true);
                if starts != 1
                    || ends != 1
                    || !healthy_end
                    || sample.primary_exit_code.is_none()
                    || sample.load1.is_some()
                    || !sample.metrics.is_empty()
                {
                    summary.structure_errors = summary.structure_errors.saturating_add(1);
                }
                return ControlFlow::Continue(());
            }
        }

        summary.samples = summary.samples.saturating_add(1);
        summary
            .first_timestamp_ms
            .get_or_insert(sample.timestamp_ms);
        summary.last_timestamp_ms = Some(sample.timestamp_ms);
        if let Some(load) = sample.load1 {
            summary.max_load = summary.max_load.max(load);
        }
        let mut point_psi = BTreeMap::new();
        for (source, value) in sample.metrics {
            collect_psi(&mut summary.psi, &source, &value);
            collect_psi(&mut point_psi, &source, &value);
            for field in ["nr_throttled", "throttled_usec", "oom", "oom_kill", "max"] {
                if let Some(value) = read_counter(&value, field) {
                    update_counter(&mut summary, &source, field, value);
                }
            }
        }
        points.push(PressurePoint {
            timestamp_ms: sample.timestamp_ms,
            load1: sample.load1,
            psi: point_psi,
        });
        ControlFlow::Continue(())
    });
    let Ok(read) = read else {
        let _ = writeln!(
            out,
            "\n## Linux pressure context\n\nThe requested pressure artifact could not be read."
        );
        return (Vec::new(), false);
    };
    summary.malformed = summary
        .malformed
        .saturating_add(read.invalid_utf8_lines)
        .saturating_add(read.oversized_lines);
    if starts != 1 || ends != 1 || last_marker != Some(PressureMarker::End) {
        summary.structure_errors = summary.structure_errors.saturating_add(1);
    }

    if read.record_limit_exceeded {
        let _ = writeln!(
            out,
            "\n## Linux pressure context\n\nEvidence problem: the pressure artifact exceeds the deterministic limit of `{MAX_PRESSURE_RECORDS}` records. The raw artifact was left untouched."
        );
        return (Vec::new(), false);
    }

    if summary.samples == 0 {
        let _ = writeln!(
            out,
            "\n## Linux pressure context\n\nNo valid `devtools.pressure.v2` samples were available (`{}` malformed, `{}` nonmonotonic, `{}` capture-structure errors).",
            summary.malformed, summary.nonmonotonic, summary.structure_errors,
        );
        return (Vec::new(), false);
    }
    let _ = write!(
        out,
        "\n## Linux pressure context\n\nPressure is correlation, not a code-level cause. Confirm it with a same-SHA controlled concurrency run.\n\n- Samples: `{}`\n- Sample window (epoch milliseconds): `{}..{}`\n- Capture markers: start `{starts}`, end `{ends}`, healthy end `{}`\n- Maximum one-minute load average: `{:.2}`\n- Malformed samples rejected: `{}`\n- Nonmonotonic samples rejected: `{}`\n- Capture-structure errors: `{}`\n",
        summary.samples,
        summary.first_timestamp_ms.unwrap_or_default(),
        summary.last_timestamp_ms.unwrap_or_default(),
        starts == 1 && ends == 1 && healthy_end && last_marker == Some(PressureMarker::End),
        summary.max_load,
        summary.malformed,
        summary.nonmonotonic,
        summary.structure_errors,
    );
    for (key, value) in summary.psi {
        let _ = writeln!(out, "- Maximum PSI avg10 `{key}`: `{value:.2}`");
    }
    for (key, window) in summary.counters {
        let delta = window.last.saturating_sub(window.first);
        let _ = writeln!(
            out,
            "- Counter delta `{key}`: `{delta}` (absolute first `{}`, absolute last `{}`)",
            window.first, window.last,
        );
    }
    if summary.counter_regressions > 0 {
        let _ = writeln!(
            out,
            "- Evidence problem: `{}` cumulative counter observations regressed.",
            summary.counter_regressions,
        );
    }
    let complete = summary.malformed == 0
        && summary.nonmonotonic == 0
        && summary.counter_regressions == 0
        && summary.structure_errors == 0
        && !read.stopped_early;
    (points, complete)
}

fn collect_psi(maxima: &mut BTreeMap<String, f64>, source: &str, value: &str) {
    if !source.contains("pressure") {
        return;
    }
    for line in value.lines() {
        let mut fields = line.split_whitespace();
        let mode = fields.next().unwrap_or("unknown");
        if let Some(value) = fields
            .find_map(|field| field.strip_prefix("avg10="))
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            let key = format!("{source} {mode}");
            maxima
                .entry(key)
                .and_modify(|maximum| *maximum = maximum.max(value))
                .or_insert(value);
        }
    }
}

fn read_counter(value: &str, field: &str) -> Option<u64> {
    value.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let key = fields.next()?;
        let value = fields.next()?;
        (key == field).then(|| value.parse().ok()).flatten()
    })
}

fn update_counter(summary: &mut PressureSummary, source: &str, field: &str, value: u64) {
    let key = format!("{source} {field}");
    let regressed = {
        let window = summary.counters.entry(key).or_insert(CounterWindow {
            first: value,
            last: value,
        });
        if value < window.last {
            true
        } else {
            window.last = value;
            false
        }
    };
    if regressed {
        summary.counter_regressions = summary.counter_regressions.saturating_add(1);
    }
}

pub(super) fn correlate(
    dossiers: &mut BTreeMap<AttemptKey, AttemptDossier>,
    cases: &[CaseTiming],
    points: &[PressurePoint],
) {
    if points.is_empty() {
        return;
    }
    for case in cases.iter().filter(|case| case.failed) {
        let Some(iteration) = case.iteration else {
            continue;
        };
        let Some(start) = case.timestamp.as_deref().and_then(parse_timestamp_ms) else {
            continue;
        };
        let end = start.saturating_add(duration_ms(case.secs));
        let window_start = start.saturating_sub(1_000);
        let window_end = end.saturating_add(1_000);
        let first = points.partition_point(|point| point.timestamp_ms < window_start);
        let last = points.partition_point(|point| point.timestamp_ms <= window_end);
        let window = &points[first..last];
        let mut max_load = None::<f64>;
        let mut max_host_cpu = None::<f64>;
        let mut max_cgroup_cpu = None::<f64>;
        for point in window {
            update_max(&mut max_load, point.load1);
            update_max(
                &mut max_host_cpu,
                point.psi.get("proc.pressure.cpu some").copied(),
            );
            update_max(
                &mut max_cgroup_cpu,
                point.psi.get("cgroup.cpu.pressure some").copied(),
            );
        }
        let key = AttemptKey {
            iteration,
            suite: case.suite.clone(),
            name: case.name.clone(),
        };
        if let Some(dossier) = dossiers.get_mut(&key) {
            dossier.pressure = if window.is_empty() {
                "no contemporaneous sample".to_owned()
            } else {
                format!(
                    "{} samples; load1 {}; host CPU PSI {}; cgroup CPU PSI {}",
                    window.len(),
                    render_metric(max_load),
                    render_metric(max_host_cpu),
                    render_metric(max_cgroup_cpu),
                )
            };
        }
    }
}

fn update_max(maximum: &mut Option<f64>, value: Option<f64>) {
    if let Some(value) = value {
        *maximum = Some(maximum.map_or(value, |maximum| maximum.max(value)));
    }
}

fn render_metric(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".to_owned(), |value| format!("{value:.2}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_is_streamed_and_counters_are_run_window_deltas() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log = temp.path().join("pressure.jsonl");
        std::fs::write(
            &log,
            r#"{"schema":"devtools.pressure.v2","marker":"start","timestamp_ms":1000,"load1":4.5,"metrics":{"proc.pressure.cpu":"some avg10=12.50 avg60=1.0 avg300=0.1 total=10","cgroup.cpu.stat":"nr_throttled 7\nthrottled_usec 42"}}
{"schema":"devtools.pressure.v2","marker":"sample","timestamp_ms":2000,"load1":6.5,"metrics":{"proc.pressure.cpu":"some avg10=20.00 avg60=1.0 avg300=0.1 total=20","cgroup.cpu.stat":"nr_throttled 10\nthrottled_usec 50"}}
{"schema":"devtools.pressure.v2","marker":"end","timestamp_ms":3000,"load1":null,"metrics":{},"sampler_healthy":true,"primary_exit_code":0}
"#,
        )
        .expect("write pressure fixture");
        let mut markdown = String::new();

        let (points, complete) = append(&mut markdown, &log);

        assert!(complete, "{markdown}");
        assert_eq!(points.len(), 2);
        assert!(markdown.contains("Maximum one-minute load average: `6.50`"));
        assert!(markdown.contains("Maximum PSI avg10 `proc.pressure.cpu some`: `20.00`"));
        assert!(markdown.contains(
            "Counter delta `cgroup.cpu.stat nr_throttled`: `3` (absolute first `7`, absolute last `10`)"
        ));
    }

    #[test]
    fn malformed_and_nonmonotonic_samples_make_evidence_incomplete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log = temp.path().join("pressure.jsonl");
        std::fs::write(
            &log,
            "{\"schema\":\"devtools.pressure.v2\",\"marker\":\"start\",\"timestamp_ms\":2000,\"load1\":1.0,\"metrics\":{}}\n\
{not-json}\n\
{\"schema\":\"devtools.pressure.v2\",\"marker\":\"sample\",\"timestamp_ms\":1000,\"load1\":2.0,\"metrics\":{}}\n\
{\"schema\":\"devtools.pressure.v2\",\"marker\":\"end\",\"timestamp_ms\":3000,\"load1\":null,\"metrics\":{},\"sampler_healthy\":true,\"primary_exit_code\":0}\n",
        )
        .expect("write pressure fixture");
        let mut markdown = String::new();

        let (points, complete) = append(&mut markdown, &log);

        assert!(!complete, "{markdown}");
        assert_eq!(points.len(), 1);
        assert!(markdown.contains("Malformed samples rejected: `1`"));
        assert!(markdown.contains("Nonmonotonic samples rejected: `1`"));
    }

    #[test]
    fn regressing_cumulative_counter_is_incomplete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let log = temp.path().join("pressure.jsonl");
        std::fs::write(
            &log,
            "{\"schema\":\"devtools.pressure.v2\",\"marker\":\"start\",\"timestamp_ms\":1000,\"load1\":1.0,\"metrics\":{\"cgroup.memory.events\":\"oom 4\"}}\n\
{\"schema\":\"devtools.pressure.v2\",\"marker\":\"sample\",\"timestamp_ms\":2000,\"load1\":1.0,\"metrics\":{\"cgroup.memory.events\":\"oom 3\"}}\n\
{\"schema\":\"devtools.pressure.v2\",\"marker\":\"end\",\"timestamp_ms\":3000,\"load1\":null,\"metrics\":{},\"sampler_healthy\":true,\"primary_exit_code\":0}\n",
        )
        .expect("write pressure fixture");
        let mut markdown = String::new();

        let (_, complete) = append(&mut markdown, &log);

        assert!(!complete, "{markdown}");
        assert!(markdown.contains("cumulative counter observations regressed"));
    }
}
