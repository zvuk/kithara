#![cfg(not(target_arch = "wasm32"))]

use std::{
    num::NonZeroU32,
    sync::atomic::{AtomicUsize, Ordering},
};

use kithara::{
    decode::PcmSpec,
    platform::{
        sync::Arc,
        time::{self, Duration},
    },
    play::{
        Cmd, PlayError, PlayerConfig, PlayerImpl, Reply, SelectTransition, SessionDispatcher,
        apply_mix,
    },
};
use kithara_integration_tests::{
    audio_mock::TestPcmReader,
    offline::{OfflineSession, resource_from_reader},
};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const TRACK_SECS: f64 = 30.0;
const SETTLE_BLOCKS: usize = 60;
const MEASURE_BLOCKS: usize = 15;
const TOL: f32 = 2.0e-3;
const CEILING: f32 = 0.98;

struct CountingSession {
    inner: OfflineSession,
    batches: AtomicUsize,
    last_batch_len: AtomicUsize,
}

impl CountingSession {
    fn new() -> Self {
        Self {
            inner: OfflineSession::new_manual(),
            batches: AtomicUsize::new(0),
            last_batch_len: AtomicUsize::new(0),
        }
    }

    fn batches(&self) -> usize {
        self.batches.load(Ordering::SeqCst)
    }

    fn last_batch_len(&self) -> usize {
        self.last_batch_len.load(Ordering::SeqCst)
    }

    fn render(&self, frames: usize) -> Vec<f32> {
        self.inner.render(frames)
    }
}

impl SessionDispatcher for CountingSession {
    fn exec(&self, cmd: Cmd) -> Result<Reply, PlayError> {
        if let Cmd::SetPlayerMasterVolumes { levels } = &cmd {
            self.batches.fetch_add(1, Ordering::SeqCst);
            self.last_batch_len.store(levels.len(), Ordering::SeqCst);
        }
        self.inner.exec(cmd)
    }
}

struct MixHarness {
    session: Arc<CountingSession>,
    players: Vec<Arc<PlayerImpl>>,
}

impl MixHarness {
    // Players are built but not started, so a mix applied before `play` takes the
    // never-started path and each node is created at its level, unramped.
    fn new(count: usize) -> Self {
        let session = Arc::new(CountingSession::new());
        let players = (0..count)
            .map(|_| {
                let mut config = PlayerConfig::default();
                config.sample_rate = SAMPLE_RATE;
                config.crossfade_duration = 0.0;
                config.session = Some(Arc::clone(&session) as Arc<dyn SessionDispatcher>);
                Arc::new(PlayerImpl::new(config))
            })
            .collect();
        Self { session, players }
    }

    fn play(&self, values: &[f32]) {
        let spec = PcmSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("sample rate"));
        for (player, &value) in self.players.iter().zip(values) {
            player.insert(
                resource_from_reader(TestPcmReader::with_value(spec, TRACK_SECS, value)),
                None,
                None,
            );
            player
                .select_item_with_crossfade(
                    0,
                    SelectTransition {
                        autoplay: true,
                        crossfade_seconds: 0.0,
                    },
                )
                .expect("select item");
        }
    }

    fn apply(&self, levels: &[f32]) -> Result<(), PlayError> {
        let inputs: Vec<(&PlayerImpl, f32)> = self
            .players
            .iter()
            .zip(levels)
            .map(|(player, &level)| (player.as_ref(), level))
            .collect();
        apply_mix(inputs)
    }

    // Paced at the block's real audio duration, or the render outruns the decode
    // worker and samples underruns instead of the steady state.
    async fn render_block(&self) -> Vec<f32> {
        for player in &self.players {
            player.process_notifications();
        }
        let block = self.session.render(BLOCK_FRAMES);
        let budget = Duration::from_secs_f64(BLOCK_FRAMES as f64 / f64::from(SAMPLE_RATE));
        time::sleep(budget).await;
        block
    }

    async fn steady(&self) -> Vec<f32> {
        for _ in 0..SETTLE_BLOCKS {
            let _ = self.render_block().await;
        }
        let mut out = Vec::new();
        for _ in 0..MEASURE_BLOCKS {
            out.extend_from_slice(&self.render_block().await);
        }
        out
    }

    async fn steady_peak(&self) -> f32 {
        peak(&self.steady().await)
    }
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |acc, s| acc.max(s.abs()))
}

// The graph applies a constant gain to every player path. Measure it rather than
// hard-coding a backend's pan law, and express expectations relative to it.
async fn graph_gain() -> f32 {
    let harness = MixHarness::new(1);
    harness.apply(&[1.0]).expect("apply unity");
    harness.play(&[1.0]);
    let gain = harness.steady_peak().await;
    assert!(
        gain > 0.0,
        "reference render produced no audio - the harness is broken"
    );
    gain
}

fn assert_near(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < TOL,
        "{what}: got {actual}, expected {expected}"
    );
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn two_players_render_exact_weighted_sum() {
    let gain = graph_gain().await;
    let values = [0.4, 0.2];
    let levels = [0.5, 0.25];

    let harness = MixHarness::new(values.len());
    harness.apply(&levels).expect("apply mix");
    harness.play(&values);

    let expected = gain * (0.4 * 0.5 + 0.2 * 0.25);
    assert_near(harness.steady_peak().await, expected, "two-player sum");
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn four_players_render_exact_weighted_sum() {
    let gain = graph_gain().await;
    let values = [0.4, 0.3, 0.2, 0.1];
    let levels = [0.5, 0.5, 0.25, 0.25];

    let harness = MixHarness::new(values.len());
    harness.apply(&levels).expect("apply mix");
    harness.play(&values);

    let raw: f32 = values.iter().zip(&levels).map(|(v, l)| v * l).sum::<f32>();
    assert_near(harness.steady_peak().await, gain * raw, "four-player sum");
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn zeroed_players_are_silent_and_gains_are_independent() {
    let gain = graph_gain().await;
    let values = [0.4, 0.3, 0.2, 0.1];
    let levels = [1.0, 0.0, 0.0, 0.0];

    let harness = MixHarness::new(values.len());
    harness.apply(&levels).expect("apply mix");
    harness.play(&values);

    assert_near(harness.steady_peak().await, gain * 0.4, "independent gains");
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn limiter_holds_the_ceiling_when_players_overload_the_sum() {
    let gain = graph_gain().await;
    let values = [1.0_f32; 4];
    let levels = [1.0_f32; 4];

    let raw: f32 = gain * values.len() as f32;
    assert!(
        raw > CEILING,
        "test is vacuous: unlimited sum {raw} does not reach the ceiling {CEILING}"
    );

    let harness = MixHarness::new(values.len());
    harness.apply(&levels).expect("apply mix");
    harness.play(&values);

    let rendered = harness.steady().await;
    for &s in &rendered {
        assert!(
            s.abs() <= CEILING + TOL,
            "sample {s} exceeds the session limiter ceiling {CEILING}"
        );
    }
    assert_near(peak(&rendered), CEILING, "limiter holds the ceiling");
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn sub_threshold_mix_passes_through_untouched() {
    let gain = graph_gain().await;
    let harness = MixHarness::new(1);
    harness.apply(&[1.0]).expect("apply mix");
    harness.play(&[0.4]);

    let rendered = harness.steady().await;
    let expected = gain * 0.4;
    assert!(
        expected < CEILING,
        "sub-threshold test must stay below the ceiling"
    );
    assert_near(peak(&rendered), expected, "sub-threshold peak");

    // Constant in, constant out: the limiter is at unity, with no gain ripple.
    for &s in &rendered {
        assert_near(s.abs(), expected, "sub-threshold sample is unmodulated");
    }
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn single_player_without_a_mix_is_unchanged() {
    let gain = graph_gain().await;
    let harness = MixHarness::new(1);
    harness.play(&[0.4]);
    assert_near(
        harness.steady_peak().await,
        gain * 0.4,
        "single-player playback regressed",
    );
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn rejected_mix_changes_no_rendered_gain() {
    let gain = graph_gain().await;
    let values = [0.4, 0.2];
    let harness = MixHarness::new(values.len());
    harness.apply(&[0.5, 0.25]).expect("apply mix");
    harness.play(&values);

    let expected = gain * (0.4 * 0.5 + 0.2 * 0.25);
    assert_near(harness.steady_peak().await, expected, "baseline mix");

    let err = harness
        .apply(&[0.5, 2.0])
        .expect_err("invalid level must be rejected");
    assert!(matches!(err, PlayError::MixLevel { .. }));
    assert_near(
        harness.steady_peak().await,
        expected,
        "rejected mix changed a gain",
    );
}

#[kithara::test]
fn mix_update_costs_one_session_command_per_batch() {
    for count in [2_usize, 4] {
        let values: Vec<f32> = (0..count).map(|i| 0.1 * (i as f32 + 1.0)).collect();
        let levels: Vec<f32> = vec![0.5; count];
        let harness = MixHarness::new(count);
        harness.play(&values);

        let before = harness.session.batches();
        harness.apply(&levels).expect("apply mix");

        assert_eq!(
            harness.session.batches() - before,
            1,
            "{count} players must cost one session command, not {count}"
        );
        assert_eq!(
            harness.session.last_batch_len(),
            count,
            "the single command must carry every player's level"
        );
    }
}

#[kithara::test]
fn session_mix_does_not_mirror_player_content_volume() {
    let harness = MixHarness::new(1);
    harness.play(&[0.4]);
    harness.apply(&[0.5]).expect("apply mix");

    assert_eq!(
        harness.players[0].volume(),
        1.0,
        "session mix must not mirror player content volume"
    );
}
