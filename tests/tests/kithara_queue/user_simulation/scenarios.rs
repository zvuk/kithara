use kithara_platform::time::Duration;
use rand::{RngExt, SeedableRng, rngs::StdRng};

use super::actions::Action;

/// The "scripted" scenario from the Player Robustness plan
/// ("Understanding Lock" / "Обязательный scripted scenario"):
/// seek 90 % → play 1-5 s → seek 10 % → play 1-5 s → seek 50 % → read
/// to the end. Bugs #5 and #6 are expected to surface on the first two
/// jumps; the third is the "near-end" stress that lights up Bug #7
/// once the SeekRatio(0.5) → end transition has to walk the tail.
pub(crate) fn scripted_forward_back_end() -> Vec<Action> {
    // The exact sequence from the Player Robustness plan. Bug #5/#6
    // surface as: `SeekRatio(0.9)` lands close to EOF and false-EOF
    // auto-advances; `SeekRatio(0.1)` backward seek silently hangs.
    // Do NOT clamp these — that would defeat the test.
    vec![
        Action::SeekRatio(0.9),
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.1),
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.5),
        Action::PlayFor(Duration::from_secs(2)),
    ]
}

/// Targeted repro for Bug #5 — forward seek into the unbuffered tail
/// triggers a false EOF and auto-advances. Short warm-up so the
/// loader hasn't pre-fetched the destination range yet.
pub(crate) fn seek_forward_unbuffered_repro() -> Vec<Action> {
    vec![
        Action::PlayFor(Duration::from_millis(500)),
        // Far enough forward that the warmup never touched the bytes;
        // 0.85 dodges the SeekNearEnd Bug #7 territory.
        Action::SeekRatio(0.85),
        Action::PlayFor(Duration::from_millis(1500)),
    ]
}

/// Targeted repro for Bug #6 — backward seek causes silent hang. The
/// scenario warms up, jumps forward, plays again, then jumps back
/// to a region that *should* already be buffered. Hang shows as
/// `position stuck` in `PlayFor`.
pub(crate) fn seek_backward_repro() -> Vec<Action> {
    vec![
        Action::PlayFor(Duration::from_millis(800)),
        Action::SeekRatio(0.6),
        Action::PlayFor(Duration::from_millis(1000)),
        Action::SeekRatio(0.15),
        Action::PlayFor(Duration::from_millis(1500)),
    ]
}

/// Targeted repro for Bug #7 — seek to 95-99 % crashes the decode /
/// audio thread. Single `SeekNearEnd` after a brief warm-up so the
/// pipeline is mid-decode when the jump hits the tail.
pub(crate) fn seek_near_end_repro() -> Vec<Action> {
    vec![
        Action::PlayFor(Duration::from_millis(500)),
        Action::SeekNearEnd(0.97),
        Action::PlayFor(Duration::from_millis(800)),
    ]
}

/// Repro for "seek backward after long playback" — what the user
/// reproduces manually with a prod DRM track: play for a long
/// stretch, then drag the playhead back. Symptom: position stuck
/// or false-EOF auto-advance. Steps the pipeline first into the
/// near-end window so any residual state from the natural-EOF
/// approach is exercised, then forces a deep backward jump.
pub(crate) fn seek_backward_after_long_play_repro() -> Vec<Action> {
    vec![
        Action::SeekRatio(0.85),
        Action::PlayFor(Duration::from_secs(3)),
        // Big backward jump — must land and resume playback.
        Action::SeekRatio(0.15),
        Action::PlayFor(Duration::from_secs(2)),
    ]
}

/// Minimal repro for "seek backward after natural EOF". Plays the
/// track to its natural end, then seeks backward and expects the
/// reader to land + resume. Reproduces the silent-hang variant
/// of Bug #6 on the seeded random scenario (`random_seed_1337`).
pub(crate) fn seek_backward_after_natural_eof_repro() -> Vec<Action> {
    vec![
        // Skip almost all the way to the end so the natural-EOF
        // window is just a few PlayFor ticks away.
        Action::SeekNearEnd(0.97),
        // Wall-clock budget large enough to walk the track to EOF.
        Action::PlayFor(Duration::from_secs(4)),
        // Backward jump to mid-track — must land + resume.
        Action::SeekRatio(0.30),
        Action::PlayFor(Duration::from_secs(2)),
    ]
}

/// User-observed manual sequence: open the app, listen for 30+ s,
/// then drag the playhead back. Stresses the decoder's seek-from-
/// late-position path with substantial accumulated state.
pub(crate) fn long_play_then_seek_backward() -> Vec<Action> {
    vec![
        Action::RenderFor(Duration::from_secs(30)),
        Action::SeekRatio(0.15),
        Action::PlayFor(Duration::from_secs(3)),
    ]
}

/// User-observed manual sequence: open the app, listen for 30+ s,
/// then drag the playhead forward. Forward seek past buffered data
/// triggers Bug #5 (false EOF) in the user's manual repro.
pub(crate) fn long_play_then_seek_forward() -> Vec<Action> {
    vec![
        Action::RenderFor(Duration::from_secs(30)),
        Action::SeekRatio(0.85),
        Action::PlayFor(Duration::from_secs(3)),
    ]
}

/// Switch from track 0 (e.g. DRM) → track 1 (e.g. plain MP3), seek
/// inside the new track. Pins the cleanup of the previous track's
/// DRM key state and re-init of the audio pipeline.
pub(crate) fn switch_track_then_seek() -> Vec<Action> {
    vec![
        Action::PlayFor(Duration::from_secs(3)),
        Action::SelectAt(1),
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.5),
        Action::PlayFor(Duration::from_secs(2)),
    ]
}

/// Bounce between tracks repeatedly with seeks in between — covers
/// the worst-case combinator: every (DRM ↔ plain) combination of
/// transitions, decoder recreates, `AssetStore` key state carryover.
pub(crate) fn bounce_between_tracks_with_seeks() -> Vec<Action> {
    vec![
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.5),
        Action::PlayFor(Duration::from_millis(800)),
        Action::SelectAt(1),
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.4),
        Action::PlayFor(Duration::from_millis(800)),
        Action::SelectAt(0),
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.7),
        Action::PlayFor(Duration::from_millis(800)),
        Action::SelectAt(1),
        Action::PlayFor(Duration::from_secs(2)),
    ]
}

/// Long play on track 0, switch to track 1, seek immediately. Pins
/// the "carryover of post-EOF state when the new track is a
/// different content type" path the user might be tripping.
pub(crate) fn long_play_then_switch_then_seek() -> Vec<Action> {
    vec![
        Action::PlayFor(Duration::from_secs(15)),
        Action::SelectAt(1),
        Action::SeekRatio(0.4),
        Action::PlayFor(Duration::from_secs(3)),
    ]
}

/// Auto-ABR up-switch repro: long warm-up so the bandwidth probe
/// decides to `UpSwitch`, then a sequence of seeks across the track.
/// User reports that **every** seek after Auto's up-switch returns
/// `SeekOutOfRange` or false-EOF — Manual ABR (no switch) plays + seeks
/// fine. So the post-switch decoder is desynced from the new variant's
/// init segment. This scenario stresses that exact path on prod DRM.
pub(crate) fn auto_abr_upswitch_then_seek_burst() -> Vec<Action> {
    vec![
        // Play long enough for the ABR throughput estimator to decide
        // the connection can sustain the top variant. On the prod
        // playlist this happens at ~4 s; we give it 15 s to be safe.
        Action::PlayFor(Duration::from_secs(15)),
        // Burst of seeks across the track — each one is the seek the
        // user describes as "просто eof или hang". If any one of them
        // breaks the contract (auto-advance, position never lands,
        // track Failed), the harness panics.
        Action::SeekRatio(0.50),
        Action::PlayFor(Duration::from_secs(3)),
        Action::SeekRatio(0.20),
        Action::PlayFor(Duration::from_secs(3)),
        Action::SeekRatio(0.80),
        Action::PlayFor(Duration::from_secs(3)),
        Action::SeekRatio(0.35),
        Action::PlayFor(Duration::from_secs(3)),
    ]
}

/// Aggressive seek storm — many seeks rapidly. Mimics a user
/// dragging the slider; the loader has to cancel + restart fetches
/// many times in quick succession.
pub(crate) fn seek_storm() -> Vec<Action> {
    vec![
        Action::PlayFor(Duration::from_secs(2)),
        Action::SeekRatio(0.5),
        Action::PlayFor(Duration::from_millis(300)),
        Action::SeekRatio(0.2),
        Action::PlayFor(Duration::from_millis(300)),
        Action::SeekRatio(0.7),
        Action::PlayFor(Duration::from_millis(300)),
        Action::SeekRatio(0.1),
        Action::PlayFor(Duration::from_millis(300)),
        Action::SeekRatio(0.6),
        Action::PlayFor(Duration::from_secs(2)),
    ]
}

/// Seeded random scenario. Produces a `len`-element mix of seek, play,
/// pause/resume and quality-switch actions; the seed is the test-name
/// label so failures stay reproducible across CI re-runs.
///
/// Pause/Resume pairs are emitted together so the queue never spends
/// long in the paused state (which would trip the `is_playing` guard
/// inside `PlayFor`).
pub(crate) fn random_seed(seed: u64, len: usize) -> Vec<Action> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut out = Vec::with_capacity(len + 1);
    // Always start with a small warm-up so the first action sees a
    // playing track (the harness `enter_track` warm-up is half a second,
    // a deliberate minimum).
    out.push(Action::PlayFor(Duration::from_millis(700)));
    for _ in 0..len {
        let pick: u8 = rng.random_range(0..10);
        match pick {
            // Full 0.05..0.90 SeekRatio range. We want the trajectory
            // to land in the same positions a user manually drags the
            // playhead through — including after long playback, where
            // bug #6 (backward seek silent hang) reproduces.
            0..=3 => {
                let ratio: f64 = rng.random_range(0.05..0.90);
                out.push(Action::SeekRatio(ratio));
                let play_ms: u64 = rng.random_range(800..2_500);
                out.push(Action::PlayFor(Duration::from_millis(play_ms)));
            }
            4 => {
                let ratio: f64 = rng.random_range(0.95..0.99);
                out.push(Action::SeekNearEnd(ratio));
                out.push(Action::PlayFor(Duration::from_millis(800)));
            }
            5 => {
                out.push(Action::Pause);
                out.push(Action::Resume);
                let play_ms: u64 = rng.random_range(500..1_500);
                out.push(Action::PlayFor(Duration::from_millis(play_ms)));
            }
            6 => {
                let q: usize = rng.random_range(0..4);
                out.push(Action::SetQuality(q));
                out.push(Action::PlayFor(Duration::from_millis(1_000)));
            }
            7 => {
                out.push(Action::QualityAuto);
                out.push(Action::PlayFor(Duration::from_millis(1_000)));
            }
            _ => {
                let play_ms: u64 = rng.random_range(800..2_000);
                out.push(Action::PlayFor(Duration::from_millis(play_ms)));
            }
        }
    }
    out
}
