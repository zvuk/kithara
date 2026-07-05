use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Profile {
    meta: Meta,
    threads: Vec<Thread>,
}

#[derive(Debug, Deserialize)]
struct Meta {
    interval: f64,
}

#[derive(Debug, Deserialize)]
struct Thread {
    samples: Samples,
    #[serde(rename = "stackTable")]
    stack_table: StackTable,
    #[serde(rename = "frameTable")]
    frame_table: FrameTable,
    #[serde(rename = "funcTable")]
    func_table: FuncTable,
    #[serde(rename = "stringArray")]
    string_array: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Samples {
    stack: Vec<Option<usize>>,
    #[serde(rename = "threadCPUDelta", default)]
    thread_cpu_delta: Vec<Option<f64>>,
}

#[derive(Debug, Deserialize)]
struct StackTable {
    prefix: Vec<Option<usize>>,
    frame: Vec<usize>,
}

#[derive(Debug, Deserialize)]
struct FrameTable {
    func: Vec<usize>,
}

#[derive(Debug, Deserialize)]
struct FuncTable {
    name: Vec<usize>,
}

#[derive(Debug)]
pub(crate) struct WaitBucket {
    pub(crate) class: String,
    pub(crate) attributed: String,
    pub(crate) ms: f64,
}

#[derive(Debug)]
pub(crate) struct FrameBucket {
    pub(crate) frame: String,
    pub(crate) ms: f64,
}

#[derive(Debug)]
pub(crate) struct ProfileSummary {
    pub(crate) on_cpu_ms: f64,
    pub(crate) off_cpu_ms: f64,
    pub(crate) waits: Vec<WaitBucket>,
    pub(crate) cpu: Vec<FrameBucket>,
}

const WAIT_CLASSES: [(&str, &[&str]); 7] = [
    ("condvar", &["psynch_cvwait", "pthread_cond"]),
    ("lock", &["ulock_wait", "psynch_mutexwait", "pthread_mutex"]),
    ("sleep", &["nanosleep", "semwait_signal", "usleep", "sleep"]),
    ("kevent", &["kevent", "kqueue"]),
    ("mach", &["mach_msg"]),
    ("park", &["park"]),
    ("channel", &["recv"]),
];

fn wait_class(leaf: &str) -> &'static str {
    for (class, needles) in WAIT_CLASSES {
        if needles.iter().any(|needle| leaf.contains(needle)) {
            return class;
        }
    }
    "other-wait"
}

fn func_name(thread: &Thread, stack_idx: usize) -> &str {
    let frame = thread.stack_table.frame[stack_idx];
    let func = thread.frame_table.func[frame];
    let name = thread.func_table.name[func];
    thread.string_array.get(name).map_or("", String::as_str)
}

fn owned_frame<'a>(
    thread: &'a Thread,
    mut stack: Option<usize>,
    frame_prefix: &str,
) -> Option<&'a str> {
    while let Some(idx) = stack {
        let name = func_name(thread, idx);
        if name.contains(frame_prefix) {
            return Some(name);
        }
        stack = thread.stack_table.prefix[idx];
    }
    None
}

pub(crate) fn summarize(
    profile_json: &str,
    top: usize,
    frame_prefix: &str,
) -> Result<ProfileSummary> {
    let profile: Profile = serde_json::from_str(profile_json).context("parse gecko profile")?;
    let interval = profile.meta.interval;
    let mut on_cpu_ms = 0.0;
    let mut off_cpu_ms = 0.0;
    let mut waits: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut cpu: BTreeMap<String, f64> = BTreeMap::new();
    for thread in &profile.threads {
        for (i, stack) in thread.samples.stack.iter().enumerate() {
            let Some(stack_idx) = *stack else { continue };
            let cpu_delta = thread
                .samples
                .thread_cpu_delta
                .get(i)
                .copied()
                .flatten()
                .unwrap_or(0.0);
            let leaf = func_name(thread, stack_idx);
            let attributed = owned_frame(thread, Some(stack_idx), frame_prefix)
                .unwrap_or("(no matching frame)")
                .to_owned();
            if cpu_delta > 0.0 {
                on_cpu_ms += interval;
                *cpu.entry(attributed).or_default() += interval;
            } else {
                off_cpu_ms += interval;
                let class = wait_class(leaf).to_owned();
                *waits.entry((class, attributed)).or_default() += interval;
            }
        }
    }
    let mut waits: Vec<WaitBucket> = waits
        .into_iter()
        .map(|((class, attributed), ms)| WaitBucket {
            class,
            attributed,
            ms,
        })
        .collect();
    waits.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    waits.truncate(top);
    let mut cpu: Vec<FrameBucket> = cpu
        .into_iter()
        .map(|(frame, ms)| FrameBucket { frame, ms })
        .collect();
    cpu.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    cpu.truncate(top);
    Ok(ProfileSummary {
        on_cpu_ms,
        off_cpu_ms,
        waits,
        cpu,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = r#"{
      "meta": { "interval": 1.0 },
      "threads": [{
        "name": "worker",
        "samples": {
          "length": 3,
          "stack": [1, 2, 2],
          "threadCPUDelta": [900, 0, 0]
        },
        "stackTable": { "length": 3, "prefix": [null, 0, 0], "frame": [0, 1, 2] },
        "frameTable": { "length": 3, "func": [0, 1, 2] },
        "funcTable": { "length": 3, "name": [0, 1, 2] },
        "stringArray": [
          "demo_hls::loading::fetch_segment",
          "core::hash::sip",
          "__psynch_cvwait"
        ]
      }]
    }"#;

    #[test]
    fn classifies_on_and_off_cpu() {
        let summary = summarize(PROFILE, 10, "demo").expect("summarize gecko");

        assert!((summary.on_cpu_ms - 1.0).abs() < 1e-9);
        assert!((summary.off_cpu_ms - 2.0).abs() < 1e-9);
        assert_eq!(summary.waits.len(), 1);
        assert_eq!(summary.waits[0].class, "condvar");
        assert_eq!(
            summary.waits[0].attributed,
            "demo_hls::loading::fetch_segment"
        );
        assert!((summary.waits[0].ms - 2.0).abs() < 1e-9);
        assert_eq!(summary.cpu[0].frame, "demo_hls::loading::fetch_segment");
    }
}
