#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU32;

use kithara::{
    self,
    audio::mock::TestPcmReader,
    effects::{Ceiling, LimiterConfig, LookaheadMs, ReleaseMs},
    events::TrackId,
    host::{HostConfig, HostSettings, Tap},
    play::{PlayError, Resource, SessionError},
    queue::TrackSource,
    signal::AudioSpec,
};
use kithara_integration_tests::{
    bufpool_ext::{TestPools, pools},
    offline::{OfflinePlayer, OfflinePlayerOptions, asset_source, resource_from_reader},
};
use kithara_test_fixtures::assets;

use crate::underrun;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const BLOCKS: usize = 20;
pub(super) const ROOMY_CAPACITY: usize = 65_536;

#[kithara::test(tokio)]
async fn deck_eq_processes_the_crossfade_sum() {
    use kithara::effects::{
        GainDb,
        eq::{EqConfig, StereoEq, generate_log_spaced_bands},
    };

    let flat = crossfade_take(None).await;
    let cut = crossfade_take(Some(GainDb::MIN)).await;
    let crossfade = flat.iter().any(|sample| *sample > 0.2 && *sample < 0.5)
        && flat.last().map(|sample| sample.to_bits()) == Some(0.2_f32.to_bits());
    assert!(crossfade, "the take spans the whole queue crossfade");

    let pools = kithara_test_utils::bufpool::pools();
    let config = EqConfig::builder(pools).build();
    let rate = NonZeroU32::new(SAMPLE_RATE).expect("nonzero fixture rate");
    let layout = kithara::effects::eq::EqLayout::new(
        &config,
        &generate_log_spaced_bands(DECK_EQ_BANDS),
        rate,
    )
    .expect("pool fits the reference layout");
    let mut reference = StereoEq::with_layout(&config, rate, Box::new(layout));
    reference.set_gain(0, GainDb::MIN);
    let mut expected = Vec::with_capacity(flat.len());
    for block in flat.chunks(64 * 2) {
        let (mut left, mut right): (Vec<f32>, Vec<f32>) =
            block.chunks(2).map(|frame| (frame[0], frame[1])).unzip();
        reference.process(&mut left, &mut right);
        expected.extend(left.into_iter().zip(right).flat_map(|(l, r)| [l, r]));
    }
    assert_ne!(expected, flat, "the band cut changes the crossfade");
    assert_eq!(
        cut.iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        "one deck EQ state processes the gained sum of both faded slots"
    );
}

/// Bands of the deck's default equalizer layout.
const DECK_EQ_BANDS: usize = 10;

/// Plays one constant, crossfades to a second, and captures everything the
/// deck sounded from its first block, with `cut` on the deck's lowest band.
async fn crossfade_take(cut: Option<kithara::effects::GainDb>) -> Vec<f32> {
    use std::num::NonZeroUsize;

    use kithara::warp::WarpConfig;

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.05)
            .output_block_frames(NonZeroU32::new(64).expect("nonzero callback size"))
            .warp(
                WarpConfig::builder()
                    .render_quantum_frames(NonZeroUsize::new(64).expect("nonzero quantum"))
                    .build(),
            )
            .build(),
        SAMPLE_RATE,
    )
    .await;
    if let Some(gain) = cut {
        harness
            .with_queue(move |queue| queue.set_eq_gain(0, f32::from(gain)))
            .await
            .expect("the deck takes the band cut");
    }
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let next = harness
        .with_queue(|queue| {
            queue
                .append(TrackSource::Uri(asset_source(
                    &assets::fixed_constant_wav_cd_fifth_1323000(),
                )))
                .expect("append the incoming track")
        })
        .await;
    let mut take = render_effect_window(&harness, 2048).await;
    harness
        .with_queue(|queue| queue.next(kithara::queue::Transition::Crossfade))
        .await
        .expect("the queue crossfades to the next track");
    kithara_integration_tests::waits::wait_for_loader_done(
        harness.player(),
        next,
        kithara::platform::time::Duration::from_secs(30),
    )
    .await
    .expect("the incoming track loads");
    take.extend_from_slice(&render_effect_window(&harness, 44_100).await);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
    take
}

// Advance one callback period between reads instead of draining an offline burst.
#[kithara::flash(true)]
async fn render_effect_window(harness: &OfflinePlayer, frames: usize) -> Vec<f32> {
    use kithara::platform::time::{self, Duration};

    let sample_rate = harness.host().spec().await.sample_rate.get();
    let mut output = Vec::with_capacity(frames * 2);
    for start in (0..frames).step_by(64) {
        let block = 64.min(frames - start);
        output.extend_from_slice(&harness.render(block).await);
        time::sleep(Duration::from_secs_f64(
            f64::from(u32::try_from(block).expect("callback size fits u32"))
                / f64::from(sample_rate),
        ))
        .await;
    }
    output
}

#[cfg(not(rtsan))]
#[kithara::test(tokio)]
async fn deck_multiband_eq_keeps_pcm_and_settings_across_a_host_rate_change() {
    deck_eq_format_change(true).await;
}

#[kithara::test(tokio)]
async fn nonblocking_deck_multiband_eq_keeps_pcm_and_settings_across_a_host_rate_change() {
    deck_eq_format_change(false).await;
}

async fn deck_eq_format_change(block_on_underrun: bool) {
    use std::num::NonZeroUsize;

    use kithara::{
        effects::{GainDb, eq::generate_log_spaced_bands},
        warp::WarpConfig,
    };

    let mut bands = generate_log_spaced_bands(3);
    for (band, gain) in bands.iter_mut().zip([-12.0, 6.0, -3.0]) {
        band.set_gain_db(GainDb::from(gain));
    }
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(block_on_underrun)
            .output_block_frames(NonZeroU32::new(64).expect("nonzero callback size"))
            .warp(
                WarpConfig::builder()
                    .render_quantum_frames(NonZeroUsize::new(64).expect("nonzero quantum"))
                    .build(),
            )
            .eq_layout(bands)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let mut underruns = underrun::Underruns::new(&harness);
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let _ = render_effect_window(&harness, 4096).await;
    assert_eq!(harness.metrics().underruns(), 0, "initial deck playback");
    underruns.checkpoint(harness.metrics(), true, "initial ready playback");
    assert_eq!(harness.player().eq_gain(1), Some(6.0));
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap attaches");
    harness
        .host()
        .set_sample_rate(NonZeroU32::new(48_000).expect("nonzero target rate"))
        .await
        .expect("Host accepts the new rate");
    let mut output = render_effect_window(&harness, 64).await;
    underruns.checkpoint(
        harness.metrics(),
        false,
        "first 64 frames after rate admission",
    );
    assert_eq!(harness.host().spec().await.sample_rate.get(), 48_000);
    output.extend_from_slice(&render_effect_window(&harness, 4032).await);
    underruns.checkpoint(
        harness.metrics(),
        true,
        "continued playback at the new rate",
    );
    assert_eq!(tap.drain(), output);
    assert!(output.iter().all(|sample| sample.is_finite()));
    assert!(
        output[output.len() - 128..]
            .iter()
            .any(|sample| *sample != 0.0)
    );
    assert_eq!(harness.player().eq_gain(1), Some(6.0));
    harness
        .with_queue(|queue| queue.set_eq_gain(1, f32::from(GainDb::MIN)))
        .await
        .expect("new EQ target is admitted");
    let output = render_effect_window(&harness, 2048).await;
    assert_eq!(tap.drain(), output);
    assert!(
        output[output.len() - 128..]
            .iter()
            .any(|sample| *sample != 0.0)
    );
    assert_eq!(harness.player().eq_gain(1), Some(f32::from(GainDb::MIN)));
    assert_eq!(tap.drops(), 0);
    underruns.checkpoint(harness.metrics(), true, "final deck playback");
    harness.close().await;
}

async fn play_constant(harness: OfflinePlayer) -> OfflinePlayer {
    play_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_4410()).into(),
    )
    .await
}

/// Queues the resource `make` builds on `harness`'s player, plays it and
/// renders the first block.
pub(super) async fn play_resource(
    harness: OfflinePlayer,
    make: impl FnOnce() -> Resource + Send + 'static,
) -> OfflinePlayer {
    let deck_source = harness.pcm_deck((make()).into());
    play_source(harness, deck_source).await
}

pub(super) async fn play_source(
    harness: OfflinePlayer,
    deck_source: TrackSource<TestPools>,
) -> OfflinePlayer {
    let harness = load_source(harness, deck_source).await;
    harness.render(BLOCK_FRAMES).await;
    let _ = harness.tick_and_drain().await;
    harness
}

async fn load_source(harness: OfflinePlayer, deck_source: TrackSource<TestPools>) -> OfflinePlayer {
    let deck_id = harness
        .with_queue(move |player| {
            let deck_id = TrackId::allocate();
            player
                .append_with_id(deck_id, deck_source)
                .expect("append PCM deck");
            player
                .select(deck_id, kithara::queue::Transition::None)
                .expect("select the item");
            player.play();
            deck_id
        })
        .await;
    kithara_integration_tests::waits::wait_for_loader_done(
        harness.player(),
        deck_id,
        kithara::platform::time::Duration::from_secs(30),
    )
    .await
    .expect("PCM deck loads before the first render");
    harness
}

pub(super) async fn playing_harness() -> OfflinePlayer {
    let harness =
        OfflinePlayer::with_sample_rate(OfflinePlayerOptions::builder().build(), SAMPLE_RATE).await;
    play_constant(harness).await
}

pub(super) async fn render_blocks(harness: &OfflinePlayer, blocks: usize) -> Vec<f32> {
    let mut rendered = Vec::with_capacity(blocks * BLOCK_FRAMES * 2);
    for _ in 0..blocks {
        rendered.extend_from_slice(&harness.render(BLOCK_FRAMES).await);
        let _ = harness.tick_and_drain().await;
    }
    rendered
}

async fn render_with_tap() -> (Vec<f32>, Vec<f32>, u64) {
    let harness = playing_harness().await;
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("enable mix tap");
    let rendered = render_blocks(&harness, BLOCKS).await;
    let tapped = tap.drain();
    let drops = tap.drops();
    harness.close().await;
    (rendered, tapped, drops)
}

#[kithara::test(tokio)]
async fn zero_output_sink_is_processed_beside_graph_out() {
    let (rendered, tapped, drops) = render_with_tap().await;

    assert_eq!(
        rendered.len(),
        BLOCKS * BLOCK_FRAMES * 2,
        "graph_out keeps producing while the sink is attached"
    );
    assert_eq!(
        tapped.len(),
        rendered.len(),
        "the zero-output sink must process every block"
    );
    assert_eq!(drops, 0);
}

#[kithara::test(tokio)]
async fn mix_tap_matches_graph_out_bit_exactly() {
    let (rendered, tapped, _) = render_with_tap().await;

    assert!(
        rendered.iter().any(|sample| sample.abs() > 0.0),
        "the fixture must render audible signal"
    );
    assert!(
        rendered.iter().rev().take(BLOCK_FRAMES).all(|s| *s == 0.0),
        "the render must reach past the track into silence"
    );
    assert_eq!(tapped, rendered, "mix tap must be bit-exact with graph_out");
}

#[kithara::test(tokio)]
async fn a_tap_armed_before_playback_reaches_the_graph_it_waits_for() {
    let harness =
        OfflinePlayer::with_sample_rate(OfflinePlayerOptions::builder().build(), SAMPLE_RATE).await;
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("arm the mix tap before a session output exists");
    assert!(tap.drain().is_empty(), "an idle session feeds nothing");

    let deck_source = asset_source(&assets::fixed_constant_wav_cd_half_4410());
    let deck_id = harness
        .with_queue(move |player| {
            let deck_id = TrackId::allocate();
            player
                .append_with_id(deck_id, deck_source)
                .expect("append PCM deck");
            player
                .select(deck_id, kithara::queue::Transition::None)
                .expect("select the item");
            player.play();
            deck_id
        })
        .await;
    kithara_integration_tests::waits::wait_for_loader_done(
        harness.player(),
        deck_id,
        kithara::platform::time::Duration::from_secs(30),
    )
    .await
    .expect("PCM deck loads before the first render");

    let rendered = render_blocks(&harness, BLOCKS).await;
    assert_eq!(
        tap.drain(),
        rendered,
        "the waiting tap catches the mix from the first block the graph renders"
    );
    harness.close().await;
}

#[kithara::test(tokio)]
async fn the_tap_keeps_feeding_across_a_device_route_restart() {
    let harness = playing_harness().await;
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("enable mix tap");
    render_blocks(&harness, 2).await;
    assert!(!tap.drain().is_empty(), "the feed runs before the restart");

    harness
        .host()
        .invalidate_audio_route("mix tap route restart")
        .await
        .expect("invalidate the audio route");

    let rendered = render_blocks(&harness, 4).await;
    assert!(
        tap.writer_alive(),
        "a stream restart at the same rate keeps the producer"
    );
    assert_eq!(
        tap.drain(),
        rendered,
        "the same producer carries the mix rendered after the restart"
    );
    harness.close().await;
}

#[kithara::test(tokio)]
async fn mix_tap_overflow_drops_exactly_the_capacity_deficit() {
    const TIGHT_CAPACITY: usize = 2_048;

    let harness = playing_harness().await;
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, TIGHT_CAPACITY)
        .await
        .expect("enable mix tap");

    let rendered = render_blocks(&harness, BLOCKS).await;
    let tapped = tap.drain();

    assert_eq!(tapped.len(), TIGHT_CAPACITY, "the ring fills to capacity");
    assert_eq!(
        tapped,
        rendered[..TIGHT_CAPACITY],
        "what the ring holds is a contiguous prefix of the mix"
    );
    assert_eq!(
        tap.drops(),
        (rendered.len() - TIGHT_CAPACITY) as u64,
        "every sample the ring had no room for is counted"
    );
    harness.close().await;
}

#[kithara::test(tokio)]
async fn second_enable_is_rejected_and_disable_releases_the_writer() {
    let harness = playing_harness().await;
    let tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("enable mix tap");

    match harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .map(|_| ())
    {
        Err(PlayError::Session(SessionError::TapActive)) => {}
        other => panic!("a second consumer must be rejected, got {other:?}"),
    }
    assert!(tap.writer_alive());

    harness
        .host()
        .detach_tap(Tap::Master)
        .await
        .expect("disable mix tap");
    render_blocks(&harness, 2).await;
    assert!(
        !tap.writer_alive(),
        "disabling the tap must drop the producer so the consumer sees the feed end"
    );

    let re_enabled = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("re-enable mix tap after disable");
    assert!(re_enabled.writer_alive());
    harness.close().await;
}

#[kithara::test(tokio)]
async fn the_master_and_output_taps_run_together() {
    let harness = playing_harness().await;
    let mut master = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap");
    let mut output = harness
        .host()
        .attach_tap(Tap::Output, ROOMY_CAPACITY)
        .await
        .expect("output tap beside the master tap");

    let rendered = render_blocks(&harness, BLOCKS).await;

    assert_eq!(
        master.drain(),
        rendered,
        "the master tap carries the limited mix"
    );
    assert_eq!(
        output.drain(),
        rendered,
        "the output tap carries what graph_out plays"
    );
    harness.close().await;
}

/// Lookahead of the configured limiter cases: 441 whole frames at the fixture rate.
const LOOKAHEAD_MS: f32 = 10.0;
/// Silent deck frames before the transient, past the block `play_source` renders untapped.
const LEAD_FRAMES: usize = 2_000;
/// Blocks a restart case renders before it restarts the route: past the first impulse and
/// the lookahead, and before the second impulse.
const RESTART_BLOCKS: usize = 6;
/// Silent deck frames between the two impulses of a restart case.
const RESTART_GAP_FRAMES: usize = 3_000;

/// Master tap, output tap and `graph_out` of a deck playing `samples` through `limiter`.
struct LimitedTake {
    master: Vec<f32>,
    output: Vec<f32>,
    rendered: Vec<f32>,
}

impl LimitedTake {
    /// Frames of `graph_out` that carry any sound.
    fn sounding(&self) -> Vec<usize> {
        self.rendered
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .filter(|(_, frame)| frame.iter().any(|sample| *sample != 0.0))
            .map(|(index, _)| index)
            .collect()
    }
}

/// Play `samples`, one synthetic value per stereo frame at the fixture rate, through
/// `limiter`, rendering each of `segments` blocks in turn with a route restart between two
/// segments, with or without blocking on underrun.
///
/// Each lookahead case compares its take with the same limiter at zero lookahead: the
/// sound moves by exactly the reported frames, audio below the ceiling keeps its bits,
/// a transient over the ceiling stays at or under it, and every other frame is exact
/// silence.
async fn limited_take(
    block_on_underrun: bool,
    limiter: LimiterConfig,
    samples: Vec<f32>,
    segments: &[usize],
) -> LimitedTake {
    let rate = NonZeroU32::new(SAMPLE_RATE).expect("fixture rate");
    let session = HostConfig::offline(pools())
        .limiter(limiter)
        .settings(HostSettings::builder().sample_rate(rate).build())
        .build();
    let options = OfflinePlayerOptions::builder()
        .crossfade_duration(0.0)
        .block_on_underrun(block_on_underrun)
        .build();
    let harness = play_resource(
        OfflinePlayer::with_options(options, session).await,
        move || {
            resource_from_reader(TestPcmReader::with_samples(
                AudioSpec::new(2, rate),
                samples,
            ))
        },
    )
    .await;
    let mut master = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap");
    let mut output = harness
        .host()
        .attach_tap(Tap::Output, ROOMY_CAPACITY)
        .await
        .expect("output tap");
    let mut rendered = Vec::new();
    for (index, blocks) in segments.iter().enumerate() {
        if index > 0 {
            harness
                .host()
                .invalidate_audio_route("lookahead route restart")
                .await
                .expect("invalidate the audio route");
        }
        rendered.extend(render_blocks(&harness, *blocks).await);
    }
    assert_eq!(
        harness.metrics().underruns(),
        0,
        "the PCM deck never runs short"
    );
    let take = LimitedTake {
        master: master.drain(),
        output: output.drain(),
        rendered,
    };
    harness.close().await;
    take
}

fn lookahead_config(release_ms: f32) -> LimiterConfig {
    LimiterConfig::builder()
        .ceiling(Ceiling::DEFAULT)
        .release_ms(ReleaseMs::try_from(release_ms).expect("limiter release"))
        .lookahead_ms(LookaheadMs::try_from(LOOKAHEAD_MS).expect("limiter lookahead"))
        .build()
}

fn zero_lookahead(config: LimiterConfig) -> LimiterConfig {
    LimiterConfig::builder()
        .ceiling(config.ceiling())
        .release_ms(config.release_ms())
        .build()
}

#[cfg(not(rtsan))]
#[kithara::test(tokio)]
async fn a_nonzero_lookahead_delays_the_limited_mix_by_the_reported_frames() {
    lookahead_delays_the_mix(true).await;
}

#[kithara::test(tokio)]
async fn nonblocking_a_nonzero_lookahead_delays_the_limited_mix_by_the_reported_frames() {
    lookahead_delays_the_mix(false).await;
}

async fn lookahead_delays_the_mix(block_on_underrun: bool) {
    let config = lookahead_config(50.0);
    let latency = config.latency_frames(NonZeroU32::new(SAMPLE_RATE).expect("fixture rate"));
    assert_eq!(latency, 441);
    let samples: Vec<f32> = std::iter::repeat_n(0.0, LEAD_FRAMES)
        .chain([0.5])
        .chain(std::iter::repeat_n(0.0, 3_000))
        .collect();
    let reference = limited_take(
        block_on_underrun,
        zero_lookahead(config),
        samples.clone(),
        &[BLOCKS],
    )
    .await;
    let delayed = limited_take(block_on_underrun, config, samples, &[BLOCKS]).await;
    for take in [&reference, &delayed] {
        assert_eq!(
            take.master, take.rendered,
            "the master tap carries the limited mix"
        );
        assert_eq!(
            take.output, take.rendered,
            "the output tap carries what graph_out plays"
        );
    }
    let [heard] = reference.sounding()[..] else {
        panic!(
            "the reference plays the impulse once: {:?}",
            reference.sounding()
        );
    };
    assert_eq!(
        delayed.sounding(),
        [heard + latency],
        "delayed by the reported frames"
    );
    assert_eq!(
        delayed.rendered[2 * (heard + latency)..2 * (heard + latency + 1)],
        reference.rendered[2 * heard..2 * (heard + 1)],
        "audio below the ceiling keeps its bits through the delay"
    );
}

#[cfg(not(rtsan))]
#[kithara::test(tokio)]
async fn a_final_transient_leaves_the_lookahead_once_at_the_ceiling() {
    final_transient_drains_once(true).await;
}

#[kithara::test(tokio)]
async fn nonblocking_a_final_transient_leaves_the_lookahead_once_at_the_ceiling() {
    final_transient_drains_once(false).await;
}

async fn final_transient_drains_once(block_on_underrun: bool) {
    // WHY: The transient is the deck's last frame, so it leaves the delay while the
    // limiter's input is already silent. A fast release forgets the gain it needs well
    // within the lookahead.
    let config = lookahead_config(1.0);
    let latency = config.latency_frames(NonZeroU32::new(SAMPLE_RATE).expect("fixture rate"));
    let samples: Vec<f32> = std::iter::repeat_n(0.0, LEAD_FRAMES)
        .chain([1.96])
        .collect();
    let reference = limited_take(
        block_on_underrun,
        zero_lookahead(config),
        samples.clone(),
        &[BLOCKS],
    )
    .await;
    let delayed = limited_take(block_on_underrun, config, samples, &[BLOCKS]).await;
    for take in [&reference, &delayed] {
        assert_eq!(
            take.master, take.rendered,
            "the master tap carries the limited mix"
        );
        assert_eq!(
            take.output, take.rendered,
            "graph_out keeps the delayed tail"
        );
    }
    let [heard] = reference.sounding()[..] else {
        panic!(
            "the reference plays the transient once: {:?}",
            reference.sounding()
        );
    };
    assert_eq!(
        delayed.sounding(),
        [heard + latency],
        "the transient drains once"
    );
    let ceiling = f32::from(Ceiling::DEFAULT);
    for sample in &delayed.rendered[2 * (heard + latency)..2 * (heard + latency + 1)] {
        assert!(
            sample.abs() <= ceiling,
            "{sample} exceeds the ceiling {ceiling}"
        );
    }
}

#[cfg(not(rtsan))]
#[kithara::test(tokio)]
async fn a_nonzero_lookahead_delays_the_mix_by_its_frames_across_a_route_restart() {
    lookahead_across_a_restart(true).await;
}

#[kithara::test(tokio)]
async fn nonblocking_a_nonzero_lookahead_delays_the_mix_by_its_frames_across_a_route_restart() {
    lookahead_across_a_restart(false).await;
}

async fn lookahead_across_a_restart(block_on_underrun: bool) {
    // WHY: The first impulse leaves the delay before the restart; the second, the deck's
    // last frame, enters the retuned delay after it and drains while the input is silent.
    let config = lookahead_config(50.0);
    let latency = config.latency_frames(NonZeroU32::new(SAMPLE_RATE).expect("fixture rate"));
    assert_eq!(latency, 441);
    let samples: Vec<f32> = std::iter::repeat_n(0.0, LEAD_FRAMES)
        .chain([0.5])
        .chain(std::iter::repeat_n(0.0, RESTART_GAP_FRAMES))
        .chain([0.5])
        .collect();
    let segments = [RESTART_BLOCKS, BLOCKS - RESTART_BLOCKS];
    let reference = limited_take(
        block_on_underrun,
        zero_lookahead(config),
        samples.clone(),
        &segments,
    )
    .await;
    let delayed = limited_take(block_on_underrun, config, samples, &segments).await;
    for take in [&reference, &delayed] {
        assert_eq!(
            take.rendered.len(),
            BLOCKS * BLOCK_FRAMES * 2,
            "every block renders across the restart"
        );
        assert_eq!(
            take.master, take.rendered,
            "the master tap carries the limited mix"
        );
        assert_eq!(
            take.output, take.rendered,
            "the output tap carries what graph_out plays"
        );
    }
    let [first, second] = reference.sounding()[..] else {
        panic!(
            "the reference plays both impulses once: {:?}",
            reference.sounding()
        );
    };
    let restart = RESTART_BLOCKS * BLOCK_FRAMES;
    assert!(
        first + latency < restart && second >= restart,
        "the restart falls after the first impulse leaves the delay and before the second \
         enters it: {first}, {second}, restart at {restart}"
    );
    assert_eq!(
        delayed.sounding(),
        [first + latency, second + latency],
        "both impulses are delayed by the reported frames"
    );
    for heard in [first, second] {
        assert_eq!(
            delayed.rendered[2 * (heard + latency)..2 * (heard + latency + 1)],
            reference.rendered[2 * heard..2 * (heard + 1)],
            "audio below the ceiling keeps its bits through the delay"
        );
    }
}
