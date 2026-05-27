use kithara_test_utils::probe::capture::Recorder;
use tracing::{info, warn};

const NS_PER_SEC: f64 = 1_000_000_000.0;

/// Assert the committed playhead never jumps forward by more than
/// `max_step_secs` in a single commit — the FLAC "swallow" signature, where
/// the source timeline's `committed_position` leaps ahead by ~one segment
/// with no seek. Reads the `committed_ns` field off the `write_playhead`
/// USDT probe (the only surface that exposes the jump; the player's
/// served-frame position does not). Panics listing the offending jumps.
pub fn assert_no_committed_swallow(recorder: &Recorder, max_step_secs: f64) {
    let mut commits: Vec<(u64, u64)> = recorder
        .events_with_probe("write_playhead")
        .iter()
        .filter_map(|e| Some((e.seq()?, e.u64("committed_ns")?)))
        .collect();
    commits.sort_by_key(|&(seq, _)| seq);

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
        let prev = pair[0].1 as f64 / NS_PER_SEC;
        if prev <= 0.0 {
            continue;
        }
        let cur = pair[1].1 as f64 / NS_PER_SEC;
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
