#![cfg(not(target_arch = "wasm32"))]

use std::{num::NonZeroU32, sync::Mutex};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::mock::TestPcmReader,
    host::{
        HostConfig, HostOwned, HostSettings, HostSettingsChange, HostSettingsControl,
        MetronomeConfigControl, Tap,
    },
    platform::time::{self, Duration},
    play::{PlayError, PlayWorker, PlayWorkerConfig, ResourcePrep, SessionDuckingMode},
    queue::{Queue, QueueConfig, QueueControl, QueueError, QueueSettings, Transition},
    signal::{AudioSpec, SessionFrame},
};
use kithara_command::When;
use kithara_config::Configure;
use kithara_integration_tests::{
    audio_artifact::{AudioArtifactTap, artifact_label},
    mock::PcmDeck,
    offline::OfflineHostHarness,
};
use kithara_test_fixtures::{
    analysis_beat_fixtures::sine_440_long,
    integration_fixtures::{
        constant_four, constant_quiet, constant_three, constant_two, constant_unity,
    },
    signal::peak,
};
use kithara_test_utils::{
    TestTempDir,
    bufpool::{TestPools, pools},
};
use num_traits::AsPrimitive;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const TRACK_SECS: f64 = 30.0;
const SETTLE_BLOCKS: usize = 60;
const MEASURE_BLOCKS: usize = 15;
const TOL: f32 = 2.0e-3;
const CEILING: f32 = 0.98;
const CHANNELS: usize = 2;
/// Frames into a block a timed ducking change lands on.
const DUCK_OFFSET_FRAMES: u64 = 400;
/// Blocks each ducking of a listening take sounds for: two bars at the
/// 120 BPM a Host never given a tempo counts.
const TAKE_BLOCKS: usize = 345;
/// The share of the session output `Soft` ducking leaves: 16 dB down.
const SOFT_DUCKED: f32 = 0.16;
/// The share of the session output `Hard` ducking leaves: 28 dB down.
const HARD_DUCKED: f32 = 0.04;
/// More parts than a deck's ring holds between two blocks.
const RING_OVERFILL: usize = 64;

#[kithara::test(tokio)]
async fn removing_a_non_last_offline_deck_settles_without_render_and_preserves_the_survivor(
    constant_two: &'static [u8],
) {
    let mut harness = MixHarness::new(2).await;
    harness.play(&[constant_two, constant_two]).await;
    assert!(
        harness.steady_peak().await > 0.1,
        "both decks sound before removal"
    );
    let removed = harness.players.remove(0);
    harness
        .host
        .with(move |host| host.remove(&removed))
        .await
        .expect("non-last removal settles without another render");
    assert_eq!(harness.players.len(), 1);
    let after = harness.steady().await;
    assert!(
        (peak(&after) - 0.2).abs() < TOL,
        "the surviving deck still renders its own signal"
    );
    harness.close().await;
}

struct MixHarness {
    host: OfflineHostHarness<TestPools>,
    players: Vec<HostOwned<Queue<TestPools>>>,
    pcm_decks: Mutex<Vec<PcmDeck>>,
    _store_dir: TestTempDir,
}

impl MixHarness {
    // Players are built but not started, so a level set before `play` is kept for
    // each player's first slot.
    async fn new(count: usize) -> Self {
        let pools = pools();
        let store_dir = TestTempDir::new();
        let store = AssetStore::builder(pools.clone())
            .backend(StorageBackend::Disk {
                root: store_dir.path().to_path_buf(),
            })
            .build();
        let sample_rate = NonZeroU32::new(SAMPLE_RATE).expect("fixture sample rate is non-zero");
        let host = OfflineHostHarness::new(
            HostConfig::offline(pools.clone())
                .settings(HostSettings::builder().sample_rate(sample_rate).build())
                .build(),
        )
        .await
        .expect("create product offline Host");
        let mut players = Vec::with_capacity(count);
        for _ in 0..count {
            let config = QueueConfig::builder()
                .store(store.clone())
                .prep(
                    ResourcePrep::builder()
                        .worker(PlayWorker::new(
                            PlayWorkerConfig::builder(pools.clone()).build(),
                        ))
                        .build(),
                )
                .settings(
                    QueueSettings::builder()
                        .crossfade(kithara::play::CrossfadeSettings {
                            duration: 0.0,
                            ..Default::default()
                        })
                        .build(),
                )
                .build();
            players.push(host.insert(Queue::new(config)).await.expect("insert deck"));
        }
        Self {
            host,
            players,
            pcm_decks: Mutex::new(Vec::new()),
            _store_dir: store_dir,
        }
    }

    async fn set_ducking(&self, mode: SessionDuckingMode) {
        self.host
            .with(move |host| host.set_ducking(mode))
            .await
            .expect("set session ducking");
    }

    async fn play(&self, values: &[&'static [u8]]) {
        self.play_readers(
            values
                .iter()
                .map(|value| TestPcmReader::with_pcm(spec(), TRACK_SECS, value))
                .collect(),
        )
        .await;
    }

    async fn play_readers(&self, readers: Vec<TestPcmReader>) {
        let players: Vec<_> = self
            .players
            .iter()
            .map(|player| player.control().clone())
            .collect();
        let decks: Vec<_> = readers
            .into_iter()
            .map(|reader| PcmDeck::new(Box::new(reader)))
            .collect();
        let sources: Vec<_> = decks.iter().map(PcmDeck::source).collect();
        self.pcm_decks
            .lock()
            .expect("PCM deck retention")
            .extend(decks);
        let selected = self
            .host
            .run(move || {
                let mut selected = Vec::new();
                for (player, source) in players.iter().zip(sources) {
                    let id = player.append(source).expect("append PCM deck");
                    player
                        .select(id, Transition::None)
                        .expect("select PCM deck");
                    player.play();
                    selected.push((player.clone(), id));
                }
                selected
            })
            .await;
        for (player, id) in selected {
            kithara_integration_tests::waits::wait_for_loader_done(
                &player,
                id,
                Duration::from_secs(30),
            )
            .await
            .expect("PCM deck loads before the first render");
        }
    }

    // Removing every item empties the deck; the Host still holds it.
    async fn remove_all_items(&self) {
        let players: Vec<_> = self
            .players
            .iter()
            .map(|player| player.control().clone())
            .collect();
        self.host
            .run(move || {
                for player in &players {
                    player.clear().expect("the deck takes the clear");
                }
            })
            .await;
    }

    async fn apply(&self, levels: &[f32]) -> Result<(), QueueError> {
        let players: Vec<_> = self
            .players
            .iter()
            .map(|player| player.control().clone())
            .collect();
        let levels = levels.to_vec();
        self.host
            .run(move || {
                players
                    .iter()
                    .zip(levels)
                    .try_for_each(|(player, level)| player.set_level(level))
            })
            .await
    }

    // The Host ticks its decks ahead of the block. Paced at the block's real
    // audio duration, or the render outruns the decode worker and samples
    // underruns instead of the steady state.
    async fn render_block(&self) -> Vec<f32> {
        let block = self.host.render(BLOCK_FRAMES).await;
        let block_frames: f64 = BLOCK_FRAMES.as_();
        let budget = Duration::from_secs_f64(block_frames / f64::from(SAMPLE_RATE));
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

    async fn close(self) {
        let Self {
            host,
            players,
            pcm_decks,
            _store_dir,
        } = self;
        drop(players);
        host.close().await;
        drop(pcm_decks);
        drop(_store_dir);
    }
}

/// Sends `player`'s deck parts until its ring refuses one, with no block
/// rendered in between; returns the refusal.
fn fill_the_deck(player: &QueueControl<TestPools>) -> Option<QueueError> {
    (0..RING_OVERFILL).find_map(|_| player.set_eq_gain(0, 0.0).err())
}

fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(SAMPLE_RATE).expect("sample rate"))
}

fn assert_near(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < TOL,
        "{what}: got {actual}, expected {expected}"
    );
}

#[kithara::test(tokio)]
async fn a_deck_mix_shows_once_its_mixer_applies_it() {
    let harness = MixHarness::new(1).await;
    let player = harness.players[0].control().clone();
    let mut events = player.subscribe::<kithara::play::PlayerEvent>();
    let control = player.clone();
    harness
        .host
        .run(move || {
            control.set_volume(0.5).expect("send volume");
            control.set_muted(true).expect("send mute");
        })
        .await;
    assert_eq!(player.volume(), 1.0);
    assert!(!player.is_muted());
    harness.render_block().await;
    assert_eq!(player.volume(), 0.5);
    assert!(player.is_muted());
    assert!(matches!(
        events.try_recv().expect("volume event").event,
        kithara::play::PlayerEvent::VolumeChanged { volume: 0.5 }
    ));
    assert!(matches!(
        events.try_recv().expect("mute event").event,
        kithara::play::PlayerEvent::MuteChanged { muted: true }
    ));
    let control = player.clone();
    let error = harness.host.run(move || control.set_level(2.0)).await;
    assert!(matches!(
        error,
        Err(QueueError::Play(PlayError::MixLevel { level: 2.0 }))
    ));
    harness.render_block().await;
    assert_eq!(player.volume(), 0.5);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn an_eq_layout_and_gain_show_once_the_mixer_applies_them() {
    let harness = MixHarness::new(1).await;
    let player = harness.players[0].control().clone();
    assert_eq!(player.eq_band_count(), 10);
    assert_eq!(player.eq_gain(0), Some(0.0));
    let control = player.clone();
    harness
        .host
        .run(move || {
            control
                .set_eq_layout(kithara::effects::eq::generate_log_spaced_bands(3))
                .expect("send layout");
            control.set_eq_gain(1, -6.0).expect("send gain");
        })
        .await;
    assert_eq!(player.eq_band_count(), 10);
    assert_eq!(player.eq_gain(1), Some(0.0));
    harness.render_block().await;
    assert_eq!(player.eq_band_count(), 3);
    assert_eq!(player.eq_gain(1), Some(-6.0));
    assert_eq!(player.eq_gain(3), None);
    let control = player.clone();
    let error = harness.host.run(move || control.set_eq_gain(5, 0.0)).await;
    assert!(matches!(
        error,
        Err(QueueError::Play(PlayError::EqBandOutOfRange {
            band: 5,
            bands: 3
        }))
    ));
    let control = player.clone();
    harness
        .host
        .run(move || control.reset_eq().expect("send reset"))
        .await;
    harness.render_block().await;
    assert_eq!(player.eq_gain(1), Some(0.0));
    harness.close().await;
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
#[case::two(vec![constant_four(), constant_two()], &[0.4, 0.2], &[0.5, 0.25], "two-player sum")]
#[case::four(
    vec![constant_four(), constant_three(), constant_two(), constant_quiet()],
    &[0.4, 0.3, 0.2, 0.1],
    &[0.5, 0.5, 0.25, 0.25],
    "four-player sum"
)]
async fn players_render_exact_weighted_sum(
    #[case] sources: Vec<&'static [u8]>,
    #[case] values: &[f32],
    #[case] levels: &[f32],
    #[case] label: &str,
) {
    let harness = MixHarness::new(values.len()).await;
    harness.apply(levels).await.expect("apply mix");
    harness.play(&sources).await;

    let expected = values.iter().zip(levels).map(|(v, l)| v * l).sum();
    assert_near(harness.steady_peak().await, expected, label);
    harness.close().await;
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn zeroed_players_are_silent_and_gains_are_independent(
    constant_four: &'static [u8],
    constant_three: &'static [u8],
    constant_two: &'static [u8],
    constant_quiet: &'static [u8],
) {
    let values = [constant_four, constant_three, constant_two, constant_quiet];
    let levels = [1.0, 0.0, 0.0, 0.0];

    let harness = MixHarness::new(values.len()).await;
    harness.apply(&levels).await.expect("apply mix");
    harness.play(&values).await;

    assert_near(harness.steady_peak().await, 0.4, "independent gains");
    harness.close().await;
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn limiter_holds_the_ceiling_when_players_overload_the_sum(constant_unity: &'static [u8]) {
    let values = [constant_unity; 4];
    let levels = [1.0_f32; 4];

    let raw: f32 = values.len().as_();
    assert!(
        raw > CEILING,
        "test is vacuous: unlimited sum {raw} does not reach the ceiling {CEILING}"
    );

    let harness = MixHarness::new(values.len()).await;
    harness.apply(&levels).await.expect("apply mix");
    harness.play(&values).await;

    let rendered = harness.steady().await;
    for &s in &rendered {
        assert!(
            s.abs() <= CEILING + TOL,
            "sample {s} exceeds the session limiter ceiling {CEILING}"
        );
    }
    assert_near(peak(&rendered), CEILING, "limiter holds the ceiling");
    harness.close().await;
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn sub_threshold_mix_passes_through_untouched(constant_four: &'static [u8]) {
    let harness = MixHarness::new(1).await;
    harness.apply(&[1.0]).await.expect("apply mix");
    harness.play(&[constant_four]).await;

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
    harness.close().await;
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn single_player_without_a_mix_is_unchanged(constant_four: &'static [u8]) {
    let harness = MixHarness::new(1).await;
    harness.play(&[constant_four]).await;
    assert_near(
        harness.steady_peak().await,
        0.4,
        "single-player playback regressed",
    );
    harness.close().await;
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn rejected_mix_changes_no_rendered_gain(
    constant_four: &'static [u8],
    constant_two: &'static [u8],
) {
    let values = [constant_four, constant_two];
    let harness = MixHarness::new(values.len()).await;
    harness.apply(&[0.5, 0.25]).await.expect("apply mix");
    harness.play(&values).await;

    let expected = 0.4 * 0.5 + 0.2 * 0.25;
    assert_near(harness.steady_peak().await, expected, "baseline mix");

    let err = harness
        .apply(&[0.5, 2.0])
        .await
        .expect_err("invalid level must be rejected");
    assert!(matches!(err, QueueError::Play(PlayError::MixLevel { .. })));
    assert_near(
        harness.steady_peak().await,
        expected,
        "rejected mix changed a gain",
    );
    harness.close().await;
}

#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn a_mix_level_outlives_clearing_the_deck(constant_four: &'static [u8]) {
    let harness = MixHarness::new(1).await;
    harness.play(&[constant_four]).await;
    harness.apply(&[0.5]).await.expect("apply mix");
    assert_near(
        harness.steady_peak().await,
        0.2,
        "level set on the playing deck",
    );

    harness.remove_all_items().await;
    harness.play(&[constant_four]).await;
    assert_near(
        harness.steady_peak().await,
        0.2,
        "level after the deck was cleared",
    );
    harness.close().await;
}

/// Closing a player silences its deck though the Host still holds it.
#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn closing_a_playing_deck_silences_it(constant_four: &'static [u8]) {
    let harness = MixHarness::new(1).await;
    harness.play(&[constant_four]).await;
    assert_near(harness.steady_peak().await, 0.4, "the deck plays its track");

    let player = harness.players[0].control().clone();
    harness
        .host
        .run(move || player.close())
        .await
        .expect("the deck takes the close");
    assert_near(
        harness.steady_peak().await,
        0.0,
        "a closed player's deck is silent",
    );
    harness.close().await;
}

/// A deck that refuses the clear a close sends keeps its player open and
/// playing, so a later close still silences it.
#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn a_refused_close_leaves_the_player_open(constant_four: &'static [u8]) {
    let harness = MixHarness::new(1).await;
    harness.play(&[constant_four]).await;
    assert_near(harness.steady_peak().await, 0.4, "the deck plays its track");

    let player = harness.players[0].control().clone();
    let (refused, closed) = harness
        .host
        .run(move || (fill_the_deck(&player), player.close()))
        .await;
    assert!(
        matches!(refused, Some(QueueError::Play(PlayError::Full(_)))),
        "the deck's ring fills: {refused:?}"
    );
    assert!(
        matches!(closed, Err(QueueError::Play(PlayError::Full(_)))),
        "a full ring refuses the close: {closed:?}"
    );
    assert_near(
        harness.steady_peak().await,
        0.4,
        "the deck keeps playing its track",
    );

    let player = harness.players[0].control().clone();
    harness
        .host
        .run(move || player.close())
        .await
        .expect("the player stayed open for a later close");
    assert_near(
        harness.steady_peak().await,
        0.0,
        "the later close silences the deck",
    );
    harness.close().await;
}

/// Removing every item takes effect only when the deck takes the clear: a
/// refused clear leaves the player its track and the deck playing it.
#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn a_refused_clear_keeps_the_track_the_deck_plays(constant_four: &'static [u8]) {
    let harness = MixHarness::new(1).await;
    harness.play(&[constant_four]).await;
    assert_near(harness.steady_peak().await, 0.4, "the deck plays its track");

    let player = harness.players[0].control().clone();
    let (refused, cleared, kept) = harness
        .host
        .run(move || {
            let refused = fill_the_deck(&player);
            let cleared = player.clear();
            (refused, cleared, player.current())
        })
        .await;
    assert!(
        matches!(refused, Some(QueueError::Play(PlayError::Full(_)))),
        "the deck's ring fills: {refused:?}"
    );
    assert!(
        matches!(cleared, Err(QueueError::Play(PlayError::Full(_)))),
        "a full ring refuses the clear: {cleared:?}"
    );
    assert!(
        kept.is_some(),
        "a refused clear leaves the player its track"
    );
    assert_near(
        harness.steady_peak().await,
        0.4,
        "the deck keeps playing its track",
    );

    harness.remove_all_items().await;
    assert_near(
        harness.steady_peak().await,
        0.0,
        "a clear the deck takes silences it",
    );
    harness.close().await;
}

#[kithara::test(tokio)]
async fn session_mix_does_not_mirror_player_content_volume(constant_four: &'static [u8]) {
    let harness = MixHarness::new(1).await;
    harness.play(&[constant_four]).await;
    harness.apply(&[0.5]).await.expect("apply mix");

    assert_eq!(
        harness.players[0].volume(),
        1.0,
        "session mix must not mirror player content volume"
    );
    harness.close().await;
}

/// Ducking set for a frame inside a block leaves every frame of the block
/// before it as it was and lowers the output after it.
#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn ducking_set_for_a_frame_inside_a_block_leaves_the_frames_before_it_alone(
    constant_four: &'static [u8],
) {
    let harness = MixHarness::new(1).await;
    harness.play(&[constant_four]).await;
    let steady = harness.steady().await;
    let undocked = steady[steady.len() - 1];
    let start = harness.host.position();
    let duck = start + u64::from(harness.host.max_block_frames().get()) + DUCK_OFFSET_FRAMES;
    let at = When::At(SessionFrame::new(
        i64::try_from(duck).expect("a test frame fits the session clock"),
    ));
    harness
        .host
        .with(move |host| host.configure(HostSettingsChange::Ducking(SessionDuckingMode::Hard), at))
        .await
        .expect("a frame ahead is reachable");
    let mut take = Vec::new();
    while harness.host.position() < duck + BLOCK_FRAMES as u64 {
        take.extend(harness.render_block().await);
    }
    harness.close().await;

    let cut = usize::try_from(duck - start).expect("frames rendered") * CHANNELS;
    assert_eq!(
        take[..cut].iter().position(|sample| *sample != undocked),
        None,
        "the frames before the duck keep the undocked {undocked}"
    );
    assert!(
        take[cut + CHANNELS..]
            .iter()
            .all(|sample| *sample < undocked),
        "the output falls after the duck's frame"
    );
}

/// Ducking lowers the whole session output to its share, `Soft` 16 dB and
/// `Hard` 28 dB down, under the Host metronome, and `Off` restores the
/// undocked level on a playing session.
#[kithara::test(native, tokio, timeout(Duration::from_secs(60)))]
async fn ducking_lowers_and_restores_the_session_output(sine_440_long: Vec<f32>) {
    let segment_samples = TAKE_BLOCKS * BLOCK_FRAMES * CHANNELS;
    let tone: Vec<f32> = sine_440_long.into_iter().step_by(CHANNELS).collect();
    let tone_peak = peak(&tone);
    let harness = MixHarness::new(1).await;
    let mut master = harness
        .host
        .attach_tap(Tap::Master, 2 * segment_samples)
        .await
        .expect("master tap");
    harness
        .host
        .with(|host| host.metronome().set_enabled(true))
        .await
        .expect("metronome on");
    let mut artifact = AudioArtifactTap::from_env(
        &artifact_label(),
        SAMPLE_RATE,
        u16::try_from(CHANNELS).expect("channel count"),
    )
    .expect("listening artifact");
    harness
        .play_readers(vec![TestPcmReader::with_samples(spec(), tone)])
        .await;

    let mut levels = Vec::new();
    for (mode, label) in [
        (SessionDuckingMode::Off, "off"),
        (SessionDuckingMode::Soft, "soft"),
        (SessionDuckingMode::Hard, "hard"),
        (SessionDuckingMode::Off, "restored"),
    ] {
        harness.set_ducking(mode).await;
        let mut take = Vec::with_capacity(segment_samples);
        for _ in 0..TAKE_BLOCKS {
            take.extend(harness.render_block().await);
        }
        if let Some(artifact) = artifact.as_mut() {
            artifact.mark(label);
            artifact.push(&take);
        }
        let mix = master.drain();
        levels.push(peak(
            &mix[mix.len() - MEASURE_BLOCKS * BLOCK_FRAMES * CHANNELS..],
        ));
    }
    drop(artifact);
    assert_eq!(master.drops(), 0, "the master tap keeps every frame");
    harness.close().await;

    let [undocked, soft, hard, restored] = levels[..] else {
        unreachable!("four ducking levels");
    };
    assert_near(undocked, tone_peak, "undocked playback");
    assert_near(soft, undocked * SOFT_DUCKED, "soft ducking");
    assert_near(hard, undocked * HARD_DUCKED, "hard ducking");
    assert_near(restored, undocked, "ducking off restores the level");
}
