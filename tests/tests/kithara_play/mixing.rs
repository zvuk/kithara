#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    events::TrackId,
    host::{HostConfig, HostOwned},
    platform::time::{self, Duration},
    play::{PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, SelectTransition},
    signal::AudioSpec,
};
use kithara_integration_tests::{
    audio_mock::TestPcmReader,
    offline::{OfflineHostHarness, peak, resource_from_reader},
};

use crate::bufpool_ext::{TestPools, pools};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const TRACK_SECS: f64 = 30.0;
const SETTLE_BLOCKS: usize = 60;
const MEASURE_BLOCKS: usize = 15;
const TOL: f32 = 2.0e-3;
const CEILING: f32 = 0.98;

struct MixHarness {
    host: OfflineHostHarness<TestPools>,
    players: Vec<HostOwned<PlayerImpl<TestPools>>>,
}

impl MixHarness {
    // Players are built but not started, so a mix applied before `play` takes the
    // never-started path and each node is created at its level, unramped.
    fn new(count: usize) -> Self {
        let pools = pools();
        let sample_rate = NonZeroU32::new(SAMPLE_RATE).expect("fixture sample rate is non-zero");
        let host = OfflineHostHarness::new(
            HostConfig::offline(pools.clone())
                .sample_rate(sample_rate)
                .build(),
        )
        .expect("create product offline Host");
        let players = (0..count)
            .map(|_| {
                let config = PlayerConfig::builder()
                    .worker(PlayWorker::new(
                        PlayWorkerConfig::builder(pools.clone()).build(),
                    ))
                    .sample_rate(sample_rate)
                    .crossfade_duration(0.0)
                    .build();
                host.insert(PlayerImpl::new(config))
                    .expect("insert player into product offline Host")
            })
            .collect();
        Self { host, players }
    }

    fn play(&self, values: &[f32]) {
        let spec = AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("sample rate"));
        for (player, &value) in self.players.iter().zip(values) {
            player.reserve_slots(1);
            player
                .replace_item(
                    0,
                    resource_from_reader(TestPcmReader::with_value(spec, TRACK_SECS, value)),
                    TrackId::allocate(),
                )
                .expect("replace player item");
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
        self.host.apply_mix(
            self.players
                .iter()
                .zip(levels)
                .map(|(player, &level)| player.level(level)),
        )
    }

    // Paced at the block's real audio duration, or the render outruns the decode
    // worker and samples underruns instead of the steady state.
    async fn render_block(&self) -> Vec<f32> {
        for player in &self.players {
            player.process_notifications();
        }
        let block = self.host.render(BLOCK_FRAMES);
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

fn assert_near(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < TOL,
        "{what}: got {actual}, expected {expected}"
    );
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
#[case::two(&[0.4, 0.2], &[0.5, 0.25], "two-player sum")]
#[case::four(
    &[0.4, 0.3, 0.2, 0.1],
    &[0.5, 0.5, 0.25, 0.25],
    "four-player sum"
)]
async fn players_render_exact_weighted_sum(
    #[case] values: &[f32],
    #[case] levels: &[f32],
    #[case] label: &str,
) {
    let harness = MixHarness::new(values.len());
    harness.apply(levels).expect("apply mix");
    harness.play(values);

    let expected = values.iter().zip(levels).map(|(v, l)| v * l).sum();
    assert_near(harness.steady_peak().await, expected, label);
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn zeroed_players_are_silent_and_gains_are_independent() {
    let values = [0.4, 0.3, 0.2, 0.1];
    let levels = [1.0, 0.0, 0.0, 0.0];

    let harness = MixHarness::new(values.len());
    harness.apply(&levels).expect("apply mix");
    harness.play(&values);

    assert_near(harness.steady_peak().await, 0.4, "independent gains");
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn limiter_holds_the_ceiling_when_players_overload_the_sum() {
    let values = [1.0_f32; 4];
    let levels = [1.0_f32; 4];

    let raw = values.len() as f32;
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
    let harness = MixHarness::new(1);
    harness.apply(&[1.0]).expect("apply mix");
    harness.play(&[0.4]);

    let rendered = harness.steady().await;
    let expected = 0.4;
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
    let harness = MixHarness::new(1);
    harness.play(&[0.4]);
    assert_near(
        harness.steady_peak().await,
        0.4,
        "single-player playback regressed",
    );
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn rejected_mix_changes_no_rendered_gain() {
    let values = [0.4, 0.2];
    let harness = MixHarness::new(values.len());
    harness.apply(&[0.5, 0.25]).expect("apply mix");
    harness.play(&values);

    let expected = 0.4 * 0.5 + 0.2 * 0.25;
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
