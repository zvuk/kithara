use kithara_test_utils::probe::capture::Recorder;
use num_traits::ToPrimitive;
use tracing::{info, warn};

const NS_PER_SEC: f64 = 1_000_000_000.0;

fn committed_positions(recorder: &Recorder) -> Vec<(u64, u64)> {
    let mut commits: Vec<(u64, u64)> = recorder
        .events_with_probe("write_playhead")
        .iter()
        .filter_map(|e| Some((e.seq()?, e.u64("committed_ns")?)))
        .collect();
    commits.sort_by_key(|&(seq, _)| seq);
    commits
}

fn seconds_from_ns(ns: u64) -> f64 {
    let Some(ns) = ns.to_f64() else {
        panic!("committed_ns does not fit f64: {ns}");
    };
    ns / NS_PER_SEC
}

pub fn assert_committed_reached(recorder: &Recorder, min_secs: f64) {
    let commits = committed_positions(recorder);
    let max_secs = commits
        .iter()
        .map(|&(_, ns)| seconds_from_ns(ns))
        .fold(0.0f64, f64::max);
    assert!(
        max_secs >= min_secs,
        "committed playhead never reached delayed segment window: max={max_secs:.2}s, required={min_secs:.2}s",
    );
}

/// Assert the committed playhead never jumps forward by more than
/// `max_step_secs` in a single commit — the FLAC "swallow" signature, where
/// the source timeline's `committed_position` leaps ahead by ~one segment
/// with no seek. Reads the `committed_ns` field off the `write_playhead`
/// USDT probe (the only surface that exposes the jump; the player's
/// served-frame position does not). Panics listing the offending jumps.
pub fn assert_no_committed_swallow(recorder: &Recorder, max_step_secs: f64) {
    let commits = committed_positions(recorder);

    assert!(
        !commits.is_empty(),
        "zero write_playhead probe firings — the track never played \
         (is the usdt-probes feature enabled in this build?)"
    );

    let mut swallows: Vec<(f64, f64)> = Vec::new();
    let mut worst_step = 0.0f64;
    for pair in commits.windows(2) {
        // The first playhead write after preload/fade-in legitimately lands a
        // few seconds in; only mid-playback jumps are swallows.
        let prev = seconds_from_ns(pair[0].1);
        if prev <= 0.0 {
            continue;
        }
        let cur = seconds_from_ns(pair[1].1);
        let step = cur - prev;
        worst_step = worst_step.max(step);
        if step > max_step_secs {
            swallows.push((prev, cur));
            warn!(from = prev, to = cur, step, "committed playhead swallow");
        }
    }

    info!(
        committed_count = commits.len(),
        worst_step,
        swallow_count = swallows.len(),
        "committed playhead trajectory checked"
    );
    assert!(
        swallows.is_empty(),
        "committed_position swallowed: {} forward jump(s) > {max_step_secs}s \
         (worst +{worst_step:.2}s): {swallows:?}",
        swallows.len(),
    );
}
