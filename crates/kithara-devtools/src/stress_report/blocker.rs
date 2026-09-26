//! Reads, out of a lane's own log, what stopped it from producing evidence.
//!
//! A lane whose inventory or `JUnit` cannot be parsed has nothing to say about
//! tests, so the report states the parser's complaint and stops. The parser is
//! the last casualty, not the cause: what stopped the lane is in the log the
//! run already collected beside the artifact it could not read. One run
//! excluded two of its six lanes as "evidence artifact missing or invalid"
//! while the `E0599` that kept them from compiling sat in the same directory,
//! so the reader reconstructed by hand a cause the run had in hand.
use std::collections::BTreeSet;

use super::evidence::strip_ansi;
use crate::common::project::StressRenderBudgets;

/// What stopped a lane, as its log states it.
pub(crate) struct Blocker {
    /// One line, for the run summary where a lane gets a single cell.
    pub(crate) reason: String,
    /// Every diagnostic the log carries, for the lane's own section.
    pub(crate) lines: Vec<String>,
}

/// Names what stopped the lane, or `None` when its log carries no diagnostic.
///
/// The first diagnostic is the reason: `rustc` prints the defect it found
/// before `cargo` prints that it could not compile, and `cargo` prints that
/// before the runner prints the exit code it propagated. Reading the last line
/// would name the messenger.
pub(crate) fn blocker(log: &str, budgets: &StressRenderBudgets) -> Option<Blocker> {
    let clean = strip_ansi(log);
    let mut seen = BTreeSet::new();
    let mut lines = Vec::new();
    for line in clean
        .lines()
        .map(str::trim_end)
        .filter(|line| diagnostic(line))
    {
        if seen.insert(line.to_owned()) {
            lines.push(line.to_owned());
        }
    }
    let reason = lines.first()?.clone();
    lines.truncate(budgets.problem_rows);
    Some(Blocker { reason, lines })
}

/// `cargo` prefixes its own failures and every `rustc` diagnostic it forwards
/// with `error`, so those two shapes are all a log needs to name the cause: the
/// coded diagnostic that broke the build, and the command that gave up on it.
fn diagnostic(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("error[") || line.starts_with("error:")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILD_FAILURE: &str = "\
   Compiling kithara-app v0.0.1-alpha4 (/runner/subject/crates/kithara-app)
error[E0599]: no method named `send_modify` found for struct `Sender<T>`
   --> crates/kithara-app/src/analysis/fixtures.rs:419:24
error: could not compile `kithara-app` (lib test) due to 1 previous error
warning: build failed, waiting for other jobs to finish...
error: command `cargo test --no-run` exited with code 101
";

    #[test]
    fn the_first_diagnostic_is_the_reason_and_the_messengers_follow() {
        let found = blocker(BUILD_FAILURE, &StressRenderBudgets::default())
            .expect("a log that failed to compile names its defect");
        assert_eq!(
            found.reason,
            "error[E0599]: no method named `send_modify` found for struct `Sender<T>`"
        );
        assert_eq!(found.lines.len(), 3, "lines: {:?}", found.lines);
        assert!(found.lines[2].contains("exited with code 101"));
    }

    #[test]
    fn a_log_without_a_diagnostic_names_nothing() {
        assert!(
            blocker(
                "   Compiling kithara-app v0.0.1-alpha4\n",
                &StressRenderBudgets::default()
            )
            .is_none()
        );
    }

    #[test]
    fn a_repeated_diagnostic_is_stated_once() {
        let log =
            "error: command `x` exited with code 101\nerror: command `x` exited with code 101\n";
        let found = blocker(log, &StressRenderBudgets::default()).expect("one diagnostic");
        assert_eq!(found.lines.len(), 1);
    }

    #[test]
    fn the_row_budget_bounds_what_a_lane_section_carries() {
        let log = (0..20).fold(String::new(), |mut log, index| {
            use std::fmt::Write as _;
            let _ = writeln!(log, "error: failure {index}");
            log
        });
        let budgets = StressRenderBudgets {
            problem_rows: 4,
            ..StressRenderBudgets::default()
        };
        let found = blocker(&log, &budgets).expect("diagnostics");
        assert_eq!(found.lines.len(), 4);
        assert_eq!(found.reason, "error: failure 0");
    }
}
