#![cfg(not(target_arch = "wasm32"))]

use kithara::{
    self,
    events::TrackId,
    host::Tap,
    play::{PlayError, Resource, SessionError},
    queue::TrackSource,
};
use kithara_integration_tests::{
    bufpool_ext::TestPools,
    offline::{OfflinePlayer, OfflinePlayerOptions, asset_source},
};
use kithara_test_fixtures::assets;

use crate::underrun;

pub(super) const SAMPLE_RATE: u32 = 44_100;
pub(super) const BLOCK_FRAMES: usize = 512;
const BLOCKS: usize = 20;
pub(crate) const ROOMY_CAPACITY: usize = 65_536;

#[cfg(not(rtsan))]
#[kithara::test(tokio)]
async fn public_effect_config_controls_reach_only_the_bound_track_at_the_due_frame() {
    effect_controls_reach_the_bound_track(true).await;
}

#[kithara::test(tokio)]
async fn nonblocking_effect_config_controls_reach_only_the_bound_track_at_the_due_frame() {
    effect_controls_reach_the_bound_track(false).await;
}

async fn effect_controls_reach_the_bound_track(block_on_underrun: bool) {
    use std::num::{NonZeroU32, NonZeroUsize};

    use kithara::{
        assets::AssetStore,
        effects::{
            AudioEffect, EffectConfig, GainDb,
            eq::{EqBandConfig, EqBandConfigChange, EqBandConfigControl, EqConfig, EqEffect},
        },
        play::{ResourceConfig, ResourceSrc},
        signal::{AudioChunk, AudioChunkInfo, AudioSpec, SessionFrame},
        warp::WarpConfig,
    };
    use kithara_command::When;
    use kithara_config::Configure;

    let options = OfflinePlayerOptions::builder()
        .block_on_underrun(block_on_underrun)
        .output_block_frames(NonZeroU32::new(64).expect("nonzero callback size"))
        .warp(
            WarpConfig::builder()
                .render_quantum_frames(NonZeroUsize::new(64).expect("nonzero quantum"))
                .build(),
        )
        .build();
    let harness = OfflinePlayer::with_sample_rate(options, SAMPLE_RATE).await;
    let config = ResourceConfig::for_src(
        ResourceSrc::parse(asset_source(&assets::fixed_constant_wav_cd_half_44100()))
            .expect("fixture URI parses"),
    )
    .store(AssetStore::builder(harness.worker().pools().clone()).build())
    .preload_chunks(NonZeroUsize::MIN)
    .audio_buffer_chunks(NonZeroUsize::new(2).expect("nonzero buffer chunks"))
    .effects(vec![EffectConfig::Equalizer(vec![EqBandConfig::default()])])
    .build();
    let harness = load_source(harness, TrackSource::Config(Box::new(config))).await;
    let _ = render_effect_window(&harness, BLOCK_FRAMES).await;
    let warmed = render_effect_window(&harness, 4096).await;
    assert!(
        warmed[warmed.len() - 128..]
            .iter()
            .all(|sample| *sample == 0.5)
    );
    let handle = harness
        .with_queue(|queue| queue.eq_band(0, 0))
        .await
        .expect("configured track has an EQ control");
    assert_eq!(handle.gain_db(), GainDb::DEFAULT);
    let start = harness.host().position();
    let target = start + 512 + 17;
    harness
        .run(&handle, move |effect| {
            effect.configure(
                EqBandConfigChange::GainDb(GainDb::MIN),
                When::At(SessionFrame::new(
                    i64::try_from(target).expect("target fits session frame"),
                )),
            )
        })
        .await
        .expect("a frame beyond the prepared two-packet lead is reachable");
    let late = harness
        .run(&handle, move |effect| {
            effect.configure(
                EqBandConfigChange::GainDb(GainDb::MAX),
                When::At(SessionFrame::new(
                    i64::try_from(start).expect("start fits session frame"),
                )),
            )
        })
        .await;
    assert!(matches!(
        late,
        Err(kithara::queue::QueueError::Play(PlayError::Late))
    ));
    let mut tap = harness
        .host()
        .attach_tap(Tap::Master, ROOMY_CAPACITY)
        .await
        .expect("master tap attaches");
    let take = render_effect_window(&harness, 4 * BLOCK_FRAMES).await;
    assert_eq!(tap.drain(), take);
    assert_eq!(tap.drops(), 0);
    let cut = usize::try_from(target - start).expect("capture offset fits usize") * 2;
    assert!(take[..cut].iter().all(|sample| *sample == 0.5));
    let region = harness.worker().pools().clone();
    let spec = AudioSpec::new(
        2,
        NonZeroU32::new(SAMPLE_RATE).expect("nonzero fixture rate"),
    );
    let make_chunk = |samples: &[f32]| {
        AudioChunk::new(
            AudioChunkInfo {
                spec,
                frames: u32::try_from(samples.len() / 2).expect("fixture frames fit u32"),
                ..Default::default()
            },
            kithara_integration_tests::bufpool_ext::sample_buffer(&region, samples),
        )
    };
    let mut reference = EqEffect::new(
        &EqConfig::builder(region.clone()).build(),
        vec![EqBandConfig::default()],
        SAMPLE_RATE,
        2,
    )
    .expect("pool fits the reference EQ");
    let _ = reference.process(make_chunk(&take[..cut]));
    reference.set_gain(0, GainDb::MIN);
    let expected = reference
        .process(make_chunk(&vec![0.5; take.len() - cut]))
        .expect("reference EQ emits PCM");
    assert!(expected.samples.iter().any(|sample| *sample < 0.5));
    assert_eq!(
        take[cut..]
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        expected
            .samples
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>()
    );
    assert_eq!(handle.gain_db(), GainDb::MIN);
    harness
        .run(&handle, |effect| effect.set_gain_db(GainDb::DEFAULT))
        .await
        .expect("unity target is admitted");
    let _ = render_effect_window(&harness, 1024).await;
    assert_eq!(handle.gain_db(), GainDb::DEFAULT);
    harness
        .with_queue(kithara::queue::QueueControl::clear)
        .await
        .expect("queue clears");
    let _ = render_effect_window(&harness, 1024).await;
    let stale = harness
        .run(&handle, |effect| effect.set_gain_db(GainDb::MIN))
        .await;
    assert!(matches!(
        stale,
        Err(kithara::queue::QueueError::Play(PlayError::NotReady))
    ));
    assert_eq!(handle.gain_db(), GainDb::DEFAULT);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

// Advance one callback period between reads instead of draining an offline burst.
#[kithara::flash(true)]
pub(super) async fn render_effect_window(harness: &OfflinePlayer, frames: usize) -> Vec<f32> {
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
async fn configured_multiband_eq_keeps_pcm_and_settings_across_a_host_rate_change() {
    effect_format_change(true).await;
}

#[kithara::test(tokio)]
async fn nonblocking_configured_multiband_eq_keeps_pcm_and_settings_across_a_host_rate_change() {
    effect_format_change(false).await;
}

async fn effect_format_change(block_on_underrun: bool) {
    use std::num::{NonZeroU32, NonZeroUsize};

    use kithara::{
        assets::AssetStore,
        effects::{
            EffectConfig, GainDb,
            eq::{EqBandConfigControl, generate_log_spaced_bands},
        },
        play::{ResourceConfig, ResourceSrc},
        warp::WarpConfig,
    };

    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(block_on_underrun)
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
    let mut underruns = underrun::Underruns::new(&harness);
    let mut bands = generate_log_spaced_bands(3);
    for (band, gain) in bands.iter_mut().zip([-12.0, 6.0, -3.0]) {
        band.set_gain_db(GainDb::from(gain));
    }
    let config = ResourceConfig::for_src(
        ResourceSrc::parse(asset_source(&assets::fixed_constant_wav_cd_half_44100()))
            .expect("fixture URI parses"),
    )
    .store(AssetStore::builder(harness.worker().pools().clone()).build())
    .preload_chunks(NonZeroUsize::MIN)
    .audio_buffer_chunks(NonZeroUsize::new(2).expect("nonzero buffer chunks"))
    .effects(vec![EffectConfig::Equalizer(bands)])
    .build();
    let harness = load_source(harness, TrackSource::Config(Box::new(config))).await;
    let _ = render_effect_window(&harness, 4096).await;
    assert_eq!(
        harness.metrics().underruns(),
        0,
        "initial configured-track playback"
    );
    underruns.checkpoint(harness.metrics(), true, "initial ready playback");
    let control = harness
        .with_queue(|queue| queue.eq_band(0, 1))
        .await
        .expect("configured track has an EQ control");
    assert_eq!(control.gain_db(), GainDb::from(6.0));
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
    assert_eq!(control.gain_db(), GainDb::from(6.0));
    harness
        .run(&control, |control| control.set_gain_db(GainDb::MIN))
        .await
        .expect("new EQ target is admitted");
    let output = render_effect_window(&harness, 2048).await;
    assert_eq!(tap.drain(), output);
    assert!(
        output[output.len() - 128..]
            .iter()
            .any(|sample| *sample != 0.0)
    );
    assert_eq!(control.gain_db(), GainDb::MIN);
    assert_eq!(tap.drops(), 0);
    underruns.checkpoint(harness.metrics(), true, "final configured playback");
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
pub(crate) async fn play_resource(
    harness: OfflinePlayer,
    make: impl FnOnce() -> Resource + Send + 'static,
) -> OfflinePlayer {
    let deck_source = harness.pcm_deck((make()).into());
    play_source(harness, deck_source).await
}

pub(crate) async fn play_source(
    harness: OfflinePlayer,
    deck_source: TrackSource<TestPools>,
) -> OfflinePlayer {
    let harness = load_source(harness, deck_source).await;
    harness.render(BLOCK_FRAMES).await;
    let _ = harness.tick_and_drain().await;
    harness
}

pub(super) async fn load_source(
    harness: OfflinePlayer,
    deck_source: TrackSource<TestPools>,
) -> OfflinePlayer {
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

pub(crate) async fn playing_harness() -> OfflinePlayer {
    let harness =
        OfflinePlayer::with_sample_rate(OfflinePlayerOptions::builder().build(), SAMPLE_RATE).await;
    play_constant(harness).await
}

pub(crate) async fn render_blocks(harness: &OfflinePlayer, blocks: usize) -> Vec<f32> {
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
