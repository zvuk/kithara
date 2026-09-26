//! Oracles for parameter-smoothing suites: a settled, audible sine deck and
//! the jump bounds a smoothed move may not exceed.
//!
//! The deck belongs to no single domain — the player suite measures a deck
//! volume step with it and the effects suite an equaliser step — so it lives
//! here rather than inside either.

use std::num::NonZeroU32;

use kithara::{
    assets::AssetStore,
    effects::{GainDb, eq::FilterKind},
    host::HostConfig,
    platform::time::{self, Duration},
    play::{
        EqBandConfig, PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig,
        ResourceSrc,
    },
    queue::{Queue, QueueConfig, TrackSource, Transition},
};
use kithara_dsp::param::{MIN_SETTLE_RATIO, SmoothingFilterCoeff};
use kithara_test_fixtures::SignalAsset;

use crate::{
    TestServerHelper,
    bufpool_ext::{TestPools, pools},
    offline::OfflineQueue,
};

pub struct Consts;

impl Consts {
    pub const AUDIBLE_PEAK: f32 = 0.1;
    pub const BLOCK_FRAMES: usize = 480;
    pub const CHANNELS: usize = 2;
    pub const EQ_SETTLE_RATIO: f32 = MIN_SETTLE_RATIO;
    pub const EQ_SMOOTH_SECONDS: f32 = 0.01;
    pub const FOUR_BAND: &[(f32, FilterKind, f32)] = &[
        (120.0, FilterKind::LowShelf, 0.0),
        (440.0, FilterKind::Peaking, -24.0),
        (2_400.0, FilterKind::Peaking, 0.0),
        (8_000.0, FilterKind::HighShelf, 0.0),
    ];
    pub const OBSERVE_BLOCKS: usize = 20;
    pub const SAMPLE_RATE: u32 = 48_000;
    pub const SETTLE_BLOCKS: usize = 200;
    pub const THREE_BAND: &[(f32, FilterKind, f32)] = &[
        (200.0, FilterKind::LowShelf, 0.0),
        (1_000.0, FilterKind::Peaking, 0.0),
        (5_000.0, FilterKind::HighShelf, 0.0),
    ];
}

#[derive(Clone, Copy)]
pub struct SmoothingCase {
    pub eq_layout: Option<&'static [(f32, FilterKind, f32)]>,
}

pub fn layout(bands: &[(f32, FilterKind, f32)]) -> Vec<EqBandConfig> {
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

pub fn max_sample_jump(pcm: &[f32], channels: usize) -> f32 {
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

/// The largest single sample a smoothed move of `amplitude_delta` may step by.
///
/// The smoothing filter is exponential, not linear: `smooth_seconds` is the
/// time it takes to come within `settle_ratio` of the target, so its first —
/// and steepest — step is the filter's own coefficient times the move.
pub fn ramp_bound(
    amplitude_delta: f32,
    smooth_seconds: f32,
    settle_ratio: f32,
    sample_rate: NonZeroU32,
) -> f32 {
    amplitude_delta * SmoothingFilterCoeff::new(sample_rate, smooth_seconds, settle_ratio).a0
}

pub fn peak(pcm: &[f32]) -> f32 {
    pcm.iter()
        .fold(0.0_f32, |acc, sample| acc.max(sample.abs()))
}

/// A playing sine deck, settled and confirmed audible.
///
/// Audibility is confirmed by waiting for an audible block rather than by
/// reading the settle window's last one: an offline render the decoder could
/// not fill is zero-filled, and one such block at the end of that window reads
/// exactly like a deck that never started.
pub async fn sine_queue(case: SmoothingCase) -> (OfflineQueue<TestPools>, u64) {
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
pub async fn observe(harness: &OfflineQueue<TestPools>, blocks: usize) -> Vec<f32> {
    let block_budget =
        Duration::from_secs_f64(Consts::BLOCK_FRAMES as f64 / f64::from(Consts::SAMPLE_RATE));
    let mut pcm = Vec::with_capacity(blocks * Consts::BLOCK_FRAMES * Consts::CHANNELS);
    for _ in 0..blocks {
        harness
            .run(kithara::queue::QueueControl::tick)
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
pub async fn observe_until(
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

pub fn last_block_peak(pcm: &[f32]) -> f32 {
    let block = Consts::BLOCK_FRAMES * Consts::CHANNELS;
    peak(&pcm[pcm.len().saturating_sub(block)..])
}

/// The last frame of `before` followed by `after`: an unsmoothed step lands on
/// the boundary between the two windows, not inside `after`.
pub fn across(before: &[f32], after: &[f32]) -> Vec<f32> {
    let frame = before.len().saturating_sub(Consts::CHANNELS);
    before[frame..].iter().chain(after).copied().collect()
}

pub fn assert_step_is_ramped(
    label: &str,
    before: &[f32],
    after: &[f32],
    amplitude_delta: f32,
    smooth_seconds: f32,
    settle_ratio: f32,
) {
    let sample_rate = NonZeroU32::new(Consts::SAMPLE_RATE).expect("sample rate is non-zero");
    let baseline = max_sample_jump(before, Consts::CHANNELS);
    let bound = baseline * (1.0 + 2f32.powi(-8))
        + ramp_bound(amplitude_delta, smooth_seconds, settle_ratio, sample_rate);
    let observed = max_sample_jump(&across(before, after), Consts::CHANNELS);
    assert!(
        observed <= bound,
        "{label}: a step reached DSP unsmoothed: max jump {observed} > bound {bound} (baseline \
         {baseline}, amplitude delta {amplitude_delta}, smooth {smooth_seconds}s, settle {settle_ratio})"
    );
}
