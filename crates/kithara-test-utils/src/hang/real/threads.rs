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
/// wait channel, syscall state, runtime counters, scheduling policy/switches,
/// and the mapping that holds the PC of a thread blocked outside a syscall.
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
    let sched = read_trimmed(&format!("/proc/self/task/{tid}/sched"))
        .and_then(|value| parse_sched(&value))
        .unwrap_or_else(|| "-".to_owned());
    let pc_map = fault_pc(&syscall)
        .and_then(|pc| {
            let maps = fs::read_to_string("/proc/self/maps").ok()?;
            map_containing(&maps, pc).map(str::to_owned)
        })
        .unwrap_or_else(|| "-".to_owned());
    Some(format!(
        "name={name} tid={tid} state={state} cpu_ticks={cpu_ticks} nice={nice} wchan={wchan} schedstat={schedstat:?} syscall={syscall:?} sched={sched:?} pc_map={pc_map:?}"
    ))
}

/// The user PC of a thread blocked outside any syscall, such as in a page
/// fault: the kernel then reports `syscall` as `-1 <sp> <pc>`.
fn fault_pc(syscall: &str) -> Option<u64> {
    let mut fields = syscall.split_whitespace();
    if fields.next()? != "-1" {
        return None;
    }
    let pc = fields.nth(1)?;
    u64::from_str_radix(pc.strip_prefix("0x").unwrap_or(pc), 16).ok()
}

/// The `maps` line whose half-open address range holds `pc`, which names the
/// mapped object and its file offset.
fn map_containing(maps: &str, pc: u64) -> Option<&str> {
    maps.lines().find(|line| {
        line.split_whitespace()
            .next()
            .and_then(|range| range.split_once('-'))
            .and_then(|(start, end)| {
                Some((
                    u64::from_str_radix(start, 16).ok()?,
                    u64::from_str_radix(end, 16).ok()?,
                ))
            })
            .is_some_and(|(start, end)| (start..end).contains(&pc))
    })
}

fn parse_sched(sched: &str) -> Option<String> {
    let mut fields = String::new();
    for line in sched.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if value.is_empty()
            || !matches!(
                name,
                "policy"
                    | "prio"
                    | "se.slice"
                    | "nr_voluntary_switches"
                    | "nr_involuntary_switches"
            )
        {
            continue;
        }
        if !fields.is_empty() {
            fields.push(' ');
        }
        fields.push_str(name);
        fields.push('=');
        fields.push_str(value);
    }
    (!fields.is_empty()).then_some(fields)
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
    use super::{fault_pc, map_containing, parse_sched, parse_stat, snapshot};

    #[test]
    fn a_fault_pc_resolves_to_the_mapping_whose_half_open_range_holds_it() {
        let maps = "garbage line\n\
            7f0000000000-7f0000001000 r--p 00000000 08:01 11 /usr/lib/libc.so.6\n\
            7f0000001000-7f0000002000 r-xp 00001000 08:01 11 /usr/lib/libc.so.6\n\
            7f0000400000-7f0000500000 r-xp 00028000 08:01 22 /usr/lib/libvulkan.so.1\n";
        let pc = fault_pc("-1 0x7ffc00000000 0x7f0000001000");
        assert_eq!(pc, Some(0x7f00_0000_1000));
        assert_eq!(
            pc.and_then(|pc| map_containing(maps, pc)),
            Some("7f0000001000-7f0000002000 r-xp 00001000 08:01 11 /usr/lib/libc.so.6")
        );
        assert_eq!(
            map_containing(maps, 0x7f00_004f_ffff),
            Some("7f0000400000-7f0000500000 r-xp 00028000 08:01 22 /usr/lib/libvulkan.so.1")
        );
        assert_eq!(map_containing(maps, 0x7f00_0050_0000), None);
        assert_eq!(fault_pc("202 0x1 0x2 0x3 0x4 0x5 0x6 0x7ffc 0x7f00"), None);
        assert_eq!(fault_pc("running"), None);
    }

    #[test]
    fn scheduler_fields_preserve_policy_slice_and_both_switch_causes() {
        let sched = "worker (7, #threads: 1)\nse.slice : 2800000\n\
            nr_involuntary_switches: 2\nse.vruntime: 123.5\n\
            prio: 104\npolicy: 0\nnr_voluntary_switches: 7\n";
        assert_eq!(
            parse_sched(sched).as_deref(),
            Some(
                "se.slice=2800000 nr_involuntary_switches=2 prio=104 policy=0 nr_voluntary_switches=7"
            )
        );
        assert_eq!(parse_sched("se.vruntime: 123.5\npolicy: \n"), None);
    }

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
                && line.contains(" syscall=")
                && line.contains(" sched=")
                && line.contains(" pc_map=")),
            "every line carries the full reading: {lines:?}"
        );
    }

    #[test]
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn the_snapshot_is_empty_without_proc() {
        assert!(snapshot().is_empty());
    }
}
