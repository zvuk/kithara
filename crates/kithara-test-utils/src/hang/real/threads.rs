//! Per-thread OS snapshot taken alongside a hang dump.
//!
//! The quiescence engine names the thread holding an `active` credit, but it
//! cannot say what that thread is doing: a holder spinning in a loop, a holder
//! parked in a blocking call the engine does not model, and a credit whose
//! thread already exited all read identically in the engine dump. The kernel
//! distinguishes them — a spinning thread reports `R` with climbing CPU ticks,
//! a parked one reports `S` or `D` with a `wchan` naming the wait, and an exited
//! thread has no entry at all.

use std::fs;

/// Upper bound on reported threads. A wedged audio process runs a few dozen; a
/// runaway one must not turn a bounded dump into an unbounded file.
const MAX_THREADS: usize = 128;

/// One line per live thread: name, id, scheduler state, CPU ticks, nice,
/// wait channel, syscall state and schedstat (runtime ns, runnable wait ns, timeslices).
/// Name first because readings join engine holders by name before truncation.
///
/// Empty on targets without `/proc`, and on any thread whose files vanish
/// mid-read — a thread exiting during the walk is expected, not an error.
pub(crate) fn snapshot() -> Vec<String> {
    let Ok(tasks) = fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    let mut lines: Vec<String> = tasks
        .flatten()
        .filter_map(|entry| describe(&entry.file_name().to_string_lossy()))
        .collect();
    lines.sort();
    lines.truncate(MAX_THREADS);
    lines
}

/// Render one thread, or `None` when it exited before its files were read.
fn describe(tid: &str) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/self/task/{tid}/stat")).ok()?;
    let (state, cpu_ticks, nice) = parse_stat(&stat)?;
    let name = read_trimmed(&format!("/proc/self/task/{tid}/comm"))
        .unwrap_or_else(|| "<unnamed>".to_owned());
    let wchan =
        read_trimmed(&format!("/proc/self/task/{tid}/wchan")).unwrap_or_else(|| "-".to_owned());
    let schedstat =
        read_trimmed(&format!("/proc/self/task/{tid}/schedstat")).unwrap_or_else(|| "-".to_owned());
    let syscall =
        read_trimmed(&format!("/proc/self/task/{tid}/syscall")).unwrap_or_else(|| "-".to_owned());
    Some(format!(
        "name={name} tid={tid} state={state} cpu_ticks={cpu_ticks} nice={nice} wchan={wchan} schedstat={schedstat:?} syscall={syscall:?}"
    ))
}

fn read_trimmed(path: &str) -> Option<String> {
    let value = fs::read_to_string(path).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Pull the scheduler state, CPU time and nice value out of a `stat` line.
///
/// The comm field is parenthesised and may itself contain spaces and
/// parentheses, so the fixed-position fields are counted from the LAST `)`:
/// state first, then `utime` and `stime` eleven and twelve fields later.
fn parse_stat(stat: &str) -> Option<(String, u64, i32)> {
    let after_comm = &stat[stat.rfind(')')? + 1..];
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let state = (*fields.first()?).to_owned();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let nice: i32 = fields.get(16)?.parse().ok()?;
    Some((state, utime + stime, nice))
}

#[cfg(test)]
mod tests {
    use super::{parse_stat, snapshot};

    #[test]
    fn stat_fields_are_counted_from_the_last_paren() {
        let stat = "7 (audio (worker) rt) S 1 7 7 0 -1 4194304 11 0 0 0 42 17 0 0 20 -16";
        assert_eq!(parse_stat(stat), Some(("S".to_owned(), 59, -16)));
    }

    #[test]
    fn a_truncated_stat_line_yields_no_reading() {
        assert_eq!(parse_stat("7 (audio) S 1 7"), None);
        assert_eq!(parse_stat("no parens here"), None);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn the_snapshot_names_every_live_thread() {
        let lines = snapshot();
        assert!(!lines.is_empty(), "at least this thread is live");
        assert!(
            lines.iter().all(|line| line.starts_with("name=")
                && line.contains(" tid=")
                && line.contains(" state=")
                && line.contains(" cpu_ticks=")
                && line.contains(" nice=")
                && line.contains(" schedstat=")
                && line.contains(" syscall=")),
            "every line carries the full reading: {lines:?}"
        );
    }

    #[test]
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn the_snapshot_is_empty_without_proc() {
        assert!(snapshot().is_empty());
    }
}
