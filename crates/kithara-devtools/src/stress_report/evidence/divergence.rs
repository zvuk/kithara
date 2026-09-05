//! Runtime lines whose presence or repetition separates failing attempts
//! from passing attempts of the same test.
use std::{collections::BTreeMap, fmt::Write as _, sync::LazyLock};

use regex::Regex;

use super::{clean_lines, line};
use crate::junit::CaseTiming;

const MAX_DIVERGENCE_ROWS: usize = 40;
const MAX_PASS_ONLY_ROWS_PER_TEST: usize = 5;
/// Below this repetition count a line is retry noise, not a spin.
const SPIN_MIN_REPEATS: u64 = 16;
/// Failing attempts must out-repeat passing ones by this factor to call a spin.
const SPIN_RATIO: u64 = 10;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Kind {
    OnlyFailures,
    SpinsInFailures,
    OnlyPasses,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::OnlyFailures => "only in failures",
            Self::SpinsInFailures => "spins in failures",
            Self::OnlyPasses => "failures never reach it",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Presence {
    max_reps: u64,
    min_reps: u64,
    attempts: usize,
}

#[derive(Debug, Default)]
struct Side {
    presence: BTreeMap<String, Presence>,
    stored: usize,
    total: usize,
}

impl Side {
    fn record(&mut self, output: &str) {
        self.total += 1;
        if output.trim().is_empty() {
            return;
        }
        self.stored += 1;
        for (template, reps) in templates(output) {
            self.presence
                .entry(template)
                .and_modify(|presence| {
                    presence.attempts += 1;
                    presence.min_reps = presence.min_reps.min(reps);
                    presence.max_reps = presence.max_reps.max(reps);
                })
                .or_insert(Presence {
                    attempts: 1,
                    min_reps: reps,
                    max_reps: reps,
                });
        }
    }
}

#[derive(Debug, Default)]
struct Group {
    failed: Side,
    passed: Side,
}

#[derive(Debug)]
struct Row {
    kind: Kind,
    failed_reps: Option<(u64, u64)>,
    passed_reps: Option<(u64, u64)>,
    template: String,
    test: String,
    failed_attempts: usize,
    failed_stored: usize,
    passed_attempts: usize,
    passed_stored: usize,
}

pub(super) fn append(out: &mut String, cases: &[CaseTiming]) -> bool {
    let mut groups = BTreeMap::<String, Group>::new();
    for case in cases {
        if case.iteration.is_none() {
            continue;
        }
        let group = groups
            .entry(format!("{} {}", case.suite, case.name))
            .or_default();
        let side = if case.failing() {
            &mut group.failed
        } else {
            &mut group.passed
        };
        side.record(&case.output);
    }

    let mut rows = Vec::new();
    let mut gaps = Vec::new();
    let mut dropped_pass_only = 0usize;
    for (test, group) in &groups {
        if group.failed.stored == 0 || group.passed.total == 0 {
            continue;
        }
        if group.passed.stored == 0 {
            if !group.failed.presence.is_empty() {
                gaps.push(test.clone());
            }
            continue;
        }
        collect_failure_rows(test, group, &mut rows);
        dropped_pass_only += collect_pass_only_rows(test, group, &mut rows);
    }

    if rows.is_empty() && gaps.is_empty() {
        return true;
    }
    rows.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then(right.failed_attempts.cmp(&left.failed_attempts))
            .then_with(|| left.test.cmp(&right.test))
            .then_with(|| left.template.cmp(&right.template))
    });
    render(out, &rows, &gaps, dropped_pass_only);
    gaps.is_empty()
}

fn collect_failure_rows(test: &str, group: &Group, rows: &mut Vec<Row>) {
    for (template, failed) in &group.failed.presence {
        if failed.attempts * 2 < group.failed.stored {
            continue;
        }
        let kind = match group.passed.presence.get(template) {
            None => Kind::OnlyFailures,
            Some(passed)
                if failed.max_reps >= SPIN_MIN_REPEATS
                    && failed.max_reps >= SPIN_RATIO.saturating_mul(passed.max_reps) =>
            {
                Kind::SpinsInFailures
            }
            Some(_) => continue,
        };
        let passed = group.passed.presence.get(template);
        rows.push(Row {
            kind,
            test: test.to_owned(),
            template: template.clone(),
            failed_attempts: failed.attempts,
            failed_stored: group.failed.stored,
            passed_attempts: passed.map_or(0, |presence| presence.attempts),
            passed_stored: group.passed.stored,
            failed_reps: Some((failed.min_reps, failed.max_reps)),
            passed_reps: passed.map(|presence| (presence.min_reps, presence.max_reps)),
        });
    }
}

fn collect_pass_only_rows(test: &str, group: &Group, rows: &mut Vec<Row>) -> usize {
    let mut kept = 0usize;
    let mut dropped = 0usize;
    for (template, passed) in &group.passed.presence {
        if passed.attempts < group.passed.stored {
            continue;
        }
        let failed = group.failed.presence.get(template);
        let failed_attempts = failed.map_or(0, |presence| presence.attempts);
        if failed_attempts * 2 > group.failed.stored {
            continue;
        }
        if kept == MAX_PASS_ONLY_ROWS_PER_TEST {
            dropped += 1;
            continue;
        }
        kept += 1;
        rows.push(Row {
            failed_attempts,
            kind: Kind::OnlyPasses,
            test: test.to_owned(),
            template: template.clone(),
            failed_stored: group.failed.stored,
            passed_attempts: passed.attempts,
            passed_stored: group.passed.stored,
            failed_reps: failed.map(|presence| (presence.min_reps, presence.max_reps)),
            passed_reps: Some((passed.min_reps, passed.max_reps)),
        });
    }
    dropped
}

fn render(out: &mut String, rows: &[Row], gaps: &[String], dropped_pass_only: usize) {
    out.push_str(
        "\n## Outcome-divergent runtime lines\n\nEach row is a tracing line whose presence or repetition separates failing attempts from passing attempts of one test. The target in the line names the module that emitted it — the nearest stored evidence to a cause. Numbers, durations, and volatile ids are normalized; counts are over attempts with stored output.\n",
    );
    if !rows.is_empty() {
        out.push_str(
            "\n| test | divergence | runtime line | failed | passed | repeats in failed | repeats in passed |\n|---|---|---|---:|---:|---|---|\n",
        );
        for row in rows.iter().take(MAX_DIVERGENCE_ROWS) {
            let _ = writeln!(
                out,
                "| {} | {} | `{}` | {}/{} | {}/{} | {} | {} |",
                row.test,
                row.kind.label(),
                row.template,
                row.failed_attempts,
                row.failed_stored,
                row.passed_attempts,
                row.passed_stored,
                render_reps(row.failed_reps),
                render_reps(row.passed_reps),
            );
        }
        if rows.len() > MAX_DIVERGENCE_ROWS {
            let _ = writeln!(
                out,
                "\nShowing the first {MAX_DIVERGENCE_ROWS} of {} divergent lines. Raw artifacts are exhaustive.",
                rows.len(),
            );
        }
        if dropped_pass_only > 0 {
            let _ = writeln!(
                out,
                "\nDropped `{dropped_pass_only}` pass-only lines beyond the per-test bound of {MAX_PASS_ONLY_ROWS_PER_TEST}. Raw artifacts are exhaustive.",
            );
        }
    }
    if !gaps.is_empty() {
        let _ = writeln!(
            out,
            "\nEvidence problem: `{}` failing tests carry runtime lines in failed attempts, but no passing attempt stored any output, so divergence cannot be computed. Enable `store-success-output` in the stress nextest profile. Tests: {}.",
            gaps.len(),
            gaps.join(", "),
        );
    }
}

fn render_reps(reps: Option<(u64, u64)>) -> String {
    match reps {
        None => String::new(),
        Some((min, max)) if min == max => format!("x{min}"),
        Some((min, max)) => format!("x{min}-x{max}"),
    }
}

fn templates(output: &str) -> BTreeMap<String, u64> {
    let mut templates = BTreeMap::new();
    for line in clean_lines(output) {
        if let Some(template) = runtime_template(&line) {
            *templates.entry(template).or_insert(0u64) += 1;
        }
    }
    templates
}

fn runtime_template(line: &str) -> Option<String> {
    static RUNTIME: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"^(?:\S+\s+)??(TRACE|DEBUG|INFO|WARN|ERROR)\s+([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z0-9_]+)+:.*)$",
        )
        .expect("runtime line regex")
    });
    let captures = RUNTIME.captures(line)?;
    Some(normalize_template(&format!(
        "{} {}",
        &captures[1], &captures[2]
    )))
}

fn normalize_template(text: &str) -> String {
    static NUMBER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b\d+(?:\.\d+)?\b").expect("number regex"));
    NUMBER
        .replace_all(&line::normalize(text), "<n>")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &str, iteration: usize, failed: bool, output: &str) -> CaseTiming {
        CaseTiming {
            failed,
            flaky: false,
            name: name.to_owned(),
            suite: "demo::tests".to_owned(),
            iteration: Some(iteration),
            secs: 1.0,
            timestamp: None,
            output: output.to_owned(),
            output_truncated: false,
        }
    }

    fn recover_line(epoch: u32, position: &str) -> String {
        format!(
            "2026-08-16T01:23:45.678901Z DEBUG kithara_audio::pipeline::seek::recover: decoder seek failed; applying one-shot recovery error=Interrupted epoch={epoch} position={position} offset=0"
        )
    }

    fn spin(count: usize, epoch: u32) -> String {
        (0..count)
            .map(|index| recover_line(epoch, &format!("4.5616{index:04}s")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    const STEADY: &str =
        "2026-08-16T01:23:45.700000Z INFO kithara_play::session: playback completed frames=220500";

    /// The whole point of the section: a line every failure emits and no green
    /// does is the closest stored evidence to a cause, and the reader should
    /// not have to diff attempt outputs by hand to find it.
    #[test]
    fn a_line_only_failures_emit_is_reported_with_coverage_and_repetition() {
        let cases = vec![
            case("seek", 0, true, &spin(3, 1)),
            case("seek", 1, true, &spin(5, 2)),
            case("seek", 2, false, STEADY),
            case("seek", 3, false, STEADY),
        ];
        let mut out = String::new();

        assert!(append(&mut out, &cases));

        assert!(out.contains("only in failures"), "{out}");
        assert!(out.contains("seek::recover"), "{out}");
        assert!(out.contains("| 2/2 | 0/2 |"), "{out}");
        assert!(out.contains("x3-x5"), "{out}");
    }

    /// Jitter must not split a template: epochs, offsets, and positions differ
    /// on every attempt, yet the emitting statement is one code site and the
    /// report must show one row for it.
    #[test]
    fn numeric_jitter_does_not_split_a_template() {
        let cases = vec![
            case("seek", 0, true, &spin(3, 1)),
            case("seek", 1, true, &spin(5, 2)),
            case("seek", 2, false, STEADY),
        ];
        let mut out = String::new();

        append(&mut out, &cases);

        assert_eq!(out.matches("only in failures").count(), 1, "{out}");
    }

    /// A line both outcomes emit at similar rates separates nothing and must
    /// not dilute the section.
    #[test]
    fn a_line_both_outcomes_emit_is_not_divergent() {
        let with_recover = format!("{STEADY}\n{}", spin(2, 1));
        let cases = vec![
            case("seek", 0, true, &with_recover),
            case("seek", 1, false, &with_recover),
        ];
        let mut out = String::new();

        assert!(append(&mut out, &cases));

        assert!(!out.contains("Outcome-divergent"), "{out}");
    }

    /// One recovery is normal, hundreds are a spin. When greens also emit the
    /// line, only the repetition gap can separate the outcomes.
    #[test]
    fn a_repetition_gap_is_reported_as_a_spin() {
        let green = format!("{STEADY}\n{}", spin(2, 1));
        let cases = vec![
            case("seek", 0, true, &spin(300, 1)),
            case("seek", 1, true, &spin(1700, 1)),
            case("seek", 2, false, &green),
            case("seek", 3, false, &green),
        ];
        let mut out = String::new();

        assert!(append(&mut out, &cases));

        assert!(out.contains("spins in failures"), "{out}");
        assert!(out.contains("x300-x1700"), "{out}");
        assert!(out.contains("x2"), "{out}");
    }

    /// A failure that dies early never reaches the lines every green emits;
    /// naming the first missing line places the death before that code site.
    #[test]
    fn a_line_every_green_emits_that_failures_never_reach_is_reported() {
        let cases = vec![
            case("seek", 0, true, "test failure: boom without runtime lines"),
            case("seek", 1, false, STEADY),
            case("seek", 2, false, STEADY),
        ];
        let mut out = String::new();

        assert!(append(&mut out, &cases));

        assert!(out.contains("failures never reach it"), "{out}");
        assert!(out.contains("playback completed"), "{out}");
        assert!(out.contains("| 0/1 | 2/2 |"), "{out}");
    }

    /// Without stored green output the section cannot claim anything, and
    /// silence here is exactly the blind spot that forced hand-diffing junit.
    /// The report must say what is missing and how to turn it on.
    #[test]
    fn missing_green_output_is_a_declared_evidence_gap() {
        let cases = vec![
            case("seek", 0, true, &spin(3, 1)),
            case("seek", 1, false, ""),
            case("seek", 2, false, ""),
        ];
        let mut out = String::new();

        assert!(!append(&mut out, &cases));

        assert!(out.contains("store-success-output"), "{out}");
        assert!(out.contains("demo::tests seek"), "{out}");
        assert!(!out.contains("only in failures"), "{out}");
    }

    /// Prose that merely mentions a level word is not runtime evidence; a
    /// template requires a level and a module-path target.
    #[test]
    fn only_level_and_target_shaped_lines_become_templates() {
        assert!(runtime_template("error: package ID specification").is_none());
        assert!(runtime_template("ERROR without a target follows").is_none());
        let template =
            runtime_template(&recover_line(1, "4.5s")).expect("tracing line is a template");
        assert!(template.contains("epoch=<n>"), "{template}");
        assert!(template.contains("position=<duration>"), "{template}");
        let bare = runtime_template("DEBUG kithara_audio::pipeline: tick 12")
            .expect("line without timestamp is a template");
        assert!(bare.contains("tick <n>"), "{bare}");
    }
}
