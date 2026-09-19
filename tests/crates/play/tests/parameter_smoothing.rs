#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use firewheel::dsp::filter::smoothing_filter::DEFAULT_SMOOTH_SECONDS;
use kithara::{
    assets::AssetStore,
    host::HostConfig,
    platform::time::{self, Duration},
    play::{
        EqBandConfig, PlayError, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl,
        ResourceConfig, ResourceSrc, SessionError,
        effects::eq::{FilterKind, GainDb},
    },
    queue::{Queue, QueueConfig, TrackSource, Transition},
    warp::{SyncGroup, WarpConfig},
};
use kithara_integration_tests::{
    TestServerHelper, kithara,
    offline::{OfflineHostHarness, OfflineQueue},
};
use kithara_test_fixtures::SignalAsset;

use crate::bufpool_ext::{TestPools, pools};

struct Consts;

impl Consts {
    const AUDIBLE_PEAK: f32 = 0.1;
    const BLOCK_FRAMES: usize = 480;
    const CHANNELS: usize = 2;
    const EQ_SMOOTH_SECONDS: f32 = 0.01;
    const FOUR_BAND: &[(f32, FilterKind, f32)] = &[
        (120.0, FilterKind::LowShelf, 0.0),
        (440.0, FilterKind::Peaking, -24.0),
        (2_400.0, FilterKind::Peaking, 0.0),
        (8_000.0, FilterKind::HighShelf, 0.0),
    ];
    const OBSERVE_BLOCKS: usize = 20;
    const SAMPLE_RATE: u32 = 48_000;
    const SETTLE_BLOCKS: usize = 200;
    const THREE_BAND: &[(f32, FilterKind, f32)] = &[
        (200.0, FilterKind::LowShelf, 0.0),
        (1_000.0, FilterKind::Peaking, 0.0),
        (5_000.0, FilterKind::HighShelf, 0.0),
    ];
}

#[derive(Clone, Copy)]
pub(super) struct SmoothingCase {
    eq_layout: Option<&'static [(f32, FilterKind, f32)]>,
}

fn layout(bands: &[(f32, FilterKind, f32)]) -> Vec<EqBandConfig> {
    bands
        .iter()
        .map(|(frequency, kind, gain_db)| {
            EqBandConfig::builder()
                .frequency(*frequency)
                .q_factor(0.707)
                .gain_db(GainDb::from(*gain_db))
                .kind(*kind)
                .build()
        })
        .collect()
}

pub(super) fn max_sample_jump(pcm: &[f32], channels: usize) -> f32 {
    (0..channels)
        .map(|channel| {
            pcm.iter()
                .skip(channel)
                .step_by(channels)
                .zip(pcm.iter().skip(channel + channels).step_by(channels))
                .map(|(a, b)| (b - a).abs())
                .fold(0.0_f32, f32::max)
        })
        .fold(0.0_f32, f32::max)
}

pub(super) fn ramp_bound(amplitude_delta: f32, smooth_seconds: f32, sample_rate: u32) -> f32 {
    amplitude_delta / (smooth_seconds * sample_rate as f32)
}

fn peak(pcm: &[f32]) -> f32 {
    pcm.iter()
        .fold(0.0_f32, |acc, sample| acc.max(sample.abs()))
}

/// A playing sine deck, settled and confirmed audible.
///
/// Audibility is confirmed by waiting for an audible block rather than by
/// reading the settle window's last one: an offline render the decoder could
/// not fill is zero-filled, and one such block at the end of that window reads
/// exactly like a deck that never started.
pub(super) async fn sine_queue(case: SmoothingCase) -> (OfflineQueue<TestPools>, u64) {
    let pools = pools();
    let sample_rate = NonZeroU32::new(Consts::SAMPLE_RATE).expect("sample rate is non-zero");
    let session = HostConfig::offline(pools.clone())
        .sample_rate(sample_rate)
        .max_block_frames(
            NonZeroU32::new(Consts::BLOCK_FRAMES as u32).expect("block size is non-zero"),
        )
        .build();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(sample_rate)
            .worker(worker)
            .maybe_eq_layout(case.eq_layout.map(layout))
            .block_on_underrun(true)
            .build(),
    );
    let queue = Queue::new(QueueConfig::builder().player(player).build());
    let harness = OfflineQueue::new(session, queue)
        .await
        .expect("create offline queue");
    let deck = harness.control();
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::WAV_SINE440_60S);
    let src = ResourceSrc::parse(url.as_str()).expect("valid signal fixture URL");
    let id = deck
        .append(TrackSource::Config(Box::new(
            ResourceConfig::for_src(src)
                .store(AssetStore::builder(pools).build())
                .build(),
        )))
        .expect("append sine fixture");
    harness
        .run(move |deck| {
            deck.select(id, Transition::None)
                .expect("select sine fixture");
            deck.play();
        })
        .await;
    observe(&harness, Consts::SETTLE_BLOCKS).await;
    let (_, audible) = observe_until(&harness, |block| {
        last_block_peak(block) > Consts::AUDIBLE_PEAK
    })
    .await;
    assert!(audible, "sine must be audible before the parameter change");
    (harness, id.as_u64())
}

/// Render `blocks` of decoded output, one block of deck time per turn.
///
/// The sleep is the deck's clock rather than a tolerance: under the simulated
/// clock it is the only thing that advances time here, so a loop without it
/// renders every block before the deck has started and hands every oracle
/// silence. The fixture parks its reads on an underrun, so a block this
/// returns carries frames the decoder produced and a jump across a window
/// boundary can only have come from DSP.
async fn observe(harness: &OfflineQueue<TestPools>, blocks: usize) -> Vec<f32> {
    let block_budget =
        Duration::from_secs_f64(Consts::BLOCK_FRAMES as f64 / f64::from(Consts::SAMPLE_RATE));
    let mut pcm = Vec::with_capacity(blocks * Consts::BLOCK_FRAMES * Consts::CHANNELS);
    for _ in 0..blocks {
        harness
            .run(|deck| deck.tick())
            .await
            .expect("tick sine queue");
        pcm.extend(harness.render(Consts::BLOCK_FRAMES).await);
        time::sleep(block_budget).await;
    }
    pcm
}

/// Render at the block's pace until a rendered block satisfies `arrived`, or
/// `SETTLE_BLOCKS` have passed: the control path has latency, and the step is
/// measured on the window that contains it. Returns the window and whether the
/// change arrived inside it.
async fn observe_until(
    harness: &OfflineQueue<TestPools>,
    arrived: impl Fn(&[f32]) -> bool,
) -> (Vec<f32>, bool) {
    let mut pcm =
        Vec::with_capacity(Consts::SETTLE_BLOCKS * Consts::BLOCK_FRAMES * Consts::CHANNELS);
    for _ in 0..Consts::SETTLE_BLOCKS {
        let block = observe(harness, 1).await;
        let done = arrived(&block);
        pcm.extend(block);
        if done {
            return (pcm, true);
        }
    }
    (pcm, false)
}

fn last_block_peak(pcm: &[f32]) -> f32 {
    let block = Consts::BLOCK_FRAMES * Consts::CHANNELS;
    peak(&pcm[pcm.len().saturating_sub(block)..])
}

/// The last frame of `before` followed by `after`: an unsmoothed step lands on
/// the boundary between the two windows, not inside `after`.
fn across(before: &[f32], after: &[f32]) -> Vec<f32> {
    let frame = before.len().saturating_sub(Consts::CHANNELS);
    before[frame..].iter().chain(after).copied().collect()
}

fn assert_step_is_ramped(
    label: &str,
    before: &[f32],
    after: &[f32],
    amplitude_delta: f32,
    smooth_seconds: f32,
) {
    let baseline = max_sample_jump(before, Consts::CHANNELS);
    let bound = baseline * (1.0 + 2f32.powi(-8))
        + ramp_bound(amplitude_delta, smooth_seconds, Consts::SAMPLE_RATE);
    let observed = max_sample_jump(&across(before, after), Consts::CHANNELS);
    assert!(
        observed <= bound,
        "{label}: a step reached DSP unsmoothed: max jump {observed} > bound {bound} (baseline \
         {baseline}, amplitude delta {amplitude_delta}, smooth {smooth_seconds}s)"
    );
}

/// Every window the smoothing oracles read is decoded output.
///
/// A render the decoder cannot fill writes the frames it had and zero-fills the
/// rest, and the silence lands inside the window as a jump no ramp bound
/// allows: the oracle then reports an unsmoothed step the DSP never produced.
/// The fixture parks its reads instead, so the window its siblings measure
/// carries no block the decoder never produced.
#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn the_observation_window_carries_no_silent_block() {
    let (harness, _) = sine_queue(SmoothingCase { eq_layout: None }).await;

    let pcm = observe(&harness, Consts::OBSERVE_BLOCKS).await;

    let block = Consts::BLOCK_FRAMES * Consts::CHANNELS;
    let quietest = pcm
        .chunks_exact(block)
        .map(peak)
        .fold(f32::INFINITY, f32::min);
    assert!(
        quietest > Consts::AUDIBLE_PEAK,
        "the observation window carried a block at peak {quietest}; the window the step \
         oracles measure carries frames the decoder never produced"
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn deck_volume_step_is_ramped() {
    let (harness, _) = sine_queue(SmoothingCase { eq_layout: None }).await;
    let before = observe(&harness, Consts::OBSERVE_BLOCKS).await;
    harness.run(|deck| deck.set_volume(0.0)).await;
    let (after, silent) = observe_until(&harness, |block| peak(block) == 0.0).await;
    assert!(
        silent,
        "volume 0 must reach silence within the settle window (last block peak {})",
        last_block_peak(&after)
    );
    assert_step_is_ramped(
        "deck volume 1→0",
        &before,
        &after,
        peak(&before),
        DEFAULT_SMOOTH_SECONDS,
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn eq_gain_step_is_ramped() {
    let (harness, _) = sine_queue(SmoothingCase {
        eq_layout: Some(Consts::THREE_BAND),
    })
    .await;
    let before = observe(&harness, Consts::OBSERVE_BLOCKS).await;
    let before_peak = peak(&before);
    harness
        .run(|deck| deck.set_eq_gain(1, -24.0))
        .await
        .expect("band 1 exists");
    let (after, arrived) = observe_until(&harness, |block| peak(block) < before_peak * 0.75).await;
    assert!(
        arrived,
        "band 1 at -24 dB takes a share of the tone within the settle window (last block peak {} \
         vs {before_peak})",
        last_block_peak(&after)
    );
    assert_step_is_ramped(
        "eq band 1 0→-24 dB",
        &before,
        &after,
        before_peak,
        Consts::EQ_SMOOTH_SECONDS,
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn eq_layout_switch_is_crossed_over() {
    let (harness, _) = sine_queue(SmoothingCase {
        eq_layout: Some(Consts::THREE_BAND),
    })
    .await;
    let before = observe(&harness, Consts::OBSERVE_BLOCKS).await;
    let before_peak = peak(&before);
    harness
        .run(|deck| deck.set_eq_layout(layout(Consts::FOUR_BAND)))
        .await
        .expect("layout accepted");
    let (after, arrived) = observe_until(&harness, |block| peak(block) < before_peak * 0.5).await;
    assert!(
        arrived,
        "the new layout is audible: a -24 dB band on the tone more than halves its peak within \
         the settle window (last block peak {} vs {before_peak})",
        last_block_peak(&after)
    );
    assert_step_is_ramped(
        "eq layout 3 bands at 0 dB → 4 bands with 440 Hz at -24 dB",
        &before,
        &after,
        before_peak,
        Consts::EQ_SMOOTH_SECONDS,
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn eq_layout_change_during_crossover_stays_continuous() {
    let (harness, _) = sine_queue(SmoothingCase {
        eq_layout: Some(Consts::THREE_BAND),
    })
    .await;
    let before = observe(&harness, Consts::OBSERVE_BLOCKS).await;
    harness
        .run(|deck| deck.set_eq_layout(layout(Consts::FOUR_BAND)))
        .await
        .expect("first layout accepted");
    let mut after = observe(&harness, 1).await;
    harness
        .run(|deck| deck.set_eq_layout(layout(Consts::THREE_BAND)))
        .await
        .expect("second layout accepted");
    after.extend(observe(&harness, Consts::OBSERVE_BLOCKS).await);
    assert!(
        last_block_peak(&after) > peak(&before) * 0.95,
        "the final unity layout must become audible"
    );
    assert_step_is_ramped(
        "layout replaced during crossover",
        &before,
        &after,
        peak(&before),
        Consts::EQ_SMOOTH_SECONDS,
    );
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn queue_append_while_playing_does_not_wait_for_host() {
    let (harness, first_id) = sine_queue(SmoothingCase { eq_layout: None }).await;
    let server = TestServerHelper::new().await;
    let url = server.signal(SignalAsset::WAV_SINE440_60S);
    let src = ResourceSrc::parse(url.as_str()).expect("valid signal fixture URL");
    let second = harness
        .control()
        .append(TrackSource::Config(Box::new(
            ResourceConfig::for_src(src)
                .store(AssetStore::builder(pools()).build())
                .build(),
        )))
        .expect("append while first track is playing");
    assert_ne!(second.as_u64(), first_id);
    harness.close().await;
}

#[kithara::test(tokio, timeout(Duration::from_secs(120)))]
async fn prepared_deck_preserves_play_pause_order() {
    let (harness, _) = sine_queue(SmoothingCase { eq_layout: None }).await;
    let deck = harness.control();
    deck.pause();
    deck.play();
    deck.pause();
    let (paused, silent) = observe_until(&harness, |block| peak(block) == 0.0).await;
    assert!(
        silent,
        "the last pause must reach silence: {}",
        last_block_peak(&paused)
    );
    deck.play();
    let (resumed, audible) = observe_until(&harness, |block| peak(block) > 0.1).await;
    assert!(
        audible,
        "resume must become audible: {}",
        last_block_peak(&resumed)
    );
    harness.close().await;
}

#[kithara::test(tokio)]
async fn failed_deck_preparation_releases_host_membership() {
    let region = pools();
    let sample_rate = NonZeroU32::new(Consts::SAMPLE_RATE).expect("sample rate");
    let config = HostConfig::offline(region.clone())
        .sample_rate(sample_rate)
        .max_block_frames(NonZeroU32::new(Consts::BLOCK_FRAMES as u32).expect("block size"))
        .build();
    let host = OfflineHostHarness::new(config).await.expect("offline host");
    let worker = PlayWorker::new(PlayWorkerConfig::builder(region).build());
    let invalid = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(sample_rate)
            .worker(worker.clone())
            .warp(
                WarpConfig::builder()
                    .render_quantum_frames(NonZeroUsize::new(32).expect("quantum"))
                    .build(),
            )
            .response_budget_frames(NonZeroUsize::new(1).expect("budget"))
            .build(),
    );
    assert!(matches!(
        host.insert(invalid).await,
        Err(PlayError::Session(
            SessionError::ResponseBudgetExceeded { .. }
        ))
    ));
    host.with(|host| {
        assert!(host.topology().expect("host topology").members().is_empty());
        assert!(
            host.sample_rate()
                .expect("host sample rate")
                .measured
                .is_none(),
            "failed preparation must close an otherwise idle stream"
        );
    })
    .await;
    let valid = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(sample_rate)
            .worker(worker)
            .build(),
    );
    let deck = host
        .insert(valid)
        .await
        .expect("host can prepare the next deck");
    host.with(move |host| {
        assert!(
            host.sample_rate().expect("sample rate").measured.is_none(),
            "inserting an idle deck must not start the output stream"
        );
        deck.set_eq_gain(0, -6.0).expect("configure idle EQ");
        assert_eq!(deck.eq_gain(0), Some(-6.0));
        deck.play();
        assert!(host.sample_rate().expect("sample rate").measured.is_some());
        assert_eq!(deck.eq_gain(0), Some(-6.0));
        deck.pause();
    })
    .await;
    host.close().await;
}
