#![cfg(not(target_arch = "wasm32"))]

use std::{
    f64::consts::{FRAC_1_SQRT_2, PI},
    num::{NonZeroU32, NonZeroUsize},
};

use kithara::{
    dsp::param::DEFAULT_SMOOTH_SECONDS,
    effects::{
        DeckEffect, FxParamChange, FxParamDescriptor, FxParamMapping, FxParamUnit,
        delay::{DelayTime, Feedback, FeedbackDelay, FeedbackDelayConfig, FeedbackHighPass},
    },
    platform::{
        sync::{Arc, Mutex},
        time::Duration,
    },
    play::{DeckFxChange, DeckMixSettings, FxMix, Trim},
    queue::{QueueControl, TrackSource},
    signal::SessionFrame,
    warp::WarpConfig,
};
use kithara_command::When;
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions, asset_source};
use kithara_test_fixtures::{asset::Asset, assets};
use kithara_test_utils::bufpool::pools;
use num_traits::ToPrimitive;

use crate::mix_tap::load_source;

const SAMPLE_RATE: u32 = 44_100;
const CALLBACK: usize = 64;
const CHANNELS: usize = 2;
/// Frames the test echo holds its input for.
const ECHO: usize = 2048;
/// Frames a take renders before its timed commands are due.
const LEAD: usize = 4096;
/// Frames a take renders.
const TAKE: usize = 16_384;
/// Frames a crossfade take renders: the first track's second, the queue's
/// crossfade at its end, and the echo of that crossfade.
const CROSSFADE_TAKE: usize = 49_152;
/// The frame a partitioned take turns its unit dry, inside a callback.
const DRY_AT: usize = LEAD + 1000 + 37;
/// The frame it turns the unit wet again, long after the dry ramp settled.
const WET_AT: usize = DRY_AT + 4096;
const DELAY: usize = 256;
const DELAY_CAPACITY: usize = 2 * DELAY;
/// The -120 dBFS floor at or under which a sample counts as silent.
const FLOOR: f32 = 1e-6;

/// A memoryless effect: its wet signal is `curve` of each input sample.
#[derive(Debug)]
pub(super) struct Curve(pub(super) fn(f32) -> f32);

impl DeckEffect for Curve {
    fn process(&mut self, input: [&[f32]; 2], wet: [&mut [f32]; 2]) {
        for (input, wet) in input.into_iter().zip(wet) {
            for (sample, out) in input.iter().zip(wet) {
                *out = (self.0)(*sample);
            }
        }
    }

    fn parameters(&self) -> &[FxParamDescriptor] {
        &[]
    }

    fn set(&mut self, _: FxParamChange) {}

    fn reset(&mut self) {}

    fn tail_exhausted(&self) -> bool {
        true
    }

    fn update_sample_rate(&mut self, _: NonZeroU32) {}
}

/// Its wet signal is its input `delay` frames late, the left input it hears
/// kept in `heard`.
pub(super) struct Echo {
    line: Vec<[f32; 2]>,
    cursor: usize,
    quiet: usize,
    heard: Option<Arc<Mutex<Vec<f32>>>>,
}

impl Echo {
    pub(super) fn new(delay: usize) -> Self {
        Self {
            line: vec![[0.0; 2]; delay],
            cursor: 0,
            quiet: delay,
            heard: None,
        }
    }

    /// An echo of no delay, its wet signal its input, that keeps what it hears.
    fn probe(heard: &Arc<Mutex<Vec<f32>>>) -> Self {
        Self {
            heard: Some(heard.clone()),
            ..Self::new(0)
        }
    }
}

impl DeckEffect for Echo {
    fn process(&mut self, [left, right]: [&[f32]; 2], [wet_left, wet_right]: [&mut [f32]; 2]) {
        if let Some(heard) = &self.heard {
            heard.lock().extend_from_slice(left);
        }
        for frame in 0..left.len() {
            let input = [left[frame], right[frame]];
            let [late_left, late_right] = self
                .line
                .get_mut(self.cursor)
                .map_or(input, |slot| std::mem::replace(slot, input));
            wet_left[frame] = late_left;
            wet_right[frame] = late_right;
            self.cursor = (self.cursor + 1) % self.line.len().max(1);
            self.quiet = if input == [0.0; 2] { self.quiet + 1 } else { 0 };
        }
    }

    fn parameters(&self) -> &[FxParamDescriptor] {
        &[]
    }

    fn set(&mut self, _: FxParamChange) {}

    fn reset(&mut self) {
        self.line.fill([0.0; 2]);
        self.cursor = 0;
        self.quiet = self.line.len();
    }

    fn tail_exhausted(&self) -> bool {
        self.quiet >= self.line.len()
    }

    fn update_sample_rate(&mut self, _: NonZeroU32) {}
}

/// Its wet signal counts the frames it has run, so a frame it missed shows in
/// every frame after.
#[derive(Default)]
struct Clock(f32);

impl DeckEffect for Clock {
    fn process(&mut self, _: [&[f32]; 2], [wet_left, wet_right]: [&mut [f32]; 2]) {
        /// One frame of count, exact in an `f32` far beyond any take.
        const TICK: f32 = 1.0 / 65_536.0;

        for (left, right) in wet_left.iter_mut().zip(wet_right) {
            self.0 += TICK;
            (*left, *right) = (self.0, self.0);
        }
    }

    fn parameters(&self) -> &[FxParamDescriptor] {
        &[]
    }

    fn set(&mut self, _: FxParamChange) {}

    fn reset(&mut self) {
        self.0 = 0.0;
    }

    fn tail_exhausted(&self) -> bool {
        true
    }

    fn update_sample_rate(&mut self, _: NonZeroU32) {}
}

pub(super) fn delay_time(frames: usize) -> DelayTime {
    DelayTime::checked(f64::from(
        u32::try_from(frames).expect("a test delay fits u32"),
    ))
    .expect("a representable delay")
}

/// Frames the test delay waits after its last write above the floor before
/// reporting its tail spent: its capacity, then the frames the slowest pole of
/// its lowest high-pass takes to decay under the floor. The Butterworth
/// high-pass has a conjugate pole pair of radius `√a2`.
fn delay_window() -> usize {
    let omega = 2.0 * PI * f64::from(f32::from(FeedbackHighPass::MIN)) / f64::from(SAMPLE_RATE);
    let alpha = omega.sin() / (2.0 * FRAC_1_SQRT_2);
    let a2 = (1.0 - alpha) / (1.0 + alpha);
    let decay = (f64::from(FLOOR).ln() / a2.sqrt().ln()).ceil();
    DELAY_CAPACITY + decay.to_usize().expect("a whole count of frames")
}

pub(super) fn feedback_delay(frames: usize) -> FeedbackDelay {
    FeedbackDelay::new(
        FeedbackDelayConfig::builder()
            .sample_rate(NonZeroU32::new(SAMPLE_RATE).expect("nonzero sample rate"))
            .capacity(NonZeroUsize::new(2 * frames).expect("nonzero capacity"))
            .time(delay_time(frames))
            .feedback(Feedback::checked(0.5).expect("half is a feedback"))
            .high_pass(FeedbackHighPass::MIN)
            .build(),
        &pools(),
    )
    .expect("the delay fits the test pools")
}

/// `effect` installed on `unit`, fully wet and on.
pub(super) fn insert(unit: usize, effect: impl DeckEffect + 'static) -> Vec<DeckFxChange> {
    vec![
        DeckFxChange::Install {
            unit,
            effect: Box::new(effect),
        },
        DeckFxChange::Mix {
            unit,
            value: FxMix::MAX,
        },
        DeckFxChange::Power { unit, on: true },
    ]
}

/// The interleaved wet signal `effect` makes of the interleaved `take`.
fn wet(effect: &mut dyn DeckEffect, take: &[f32]) -> Vec<f32> {
    let (left, right): (Vec<f32>, Vec<f32>) = take
        .chunks(CHANNELS)
        .map(|frame| (frame[0], frame[1]))
        .unzip();
    let mut wet = [vec![0.0; left.len()], vec![0.0; right.len()]];
    let [wet_left, wet_right] = &mut wet;
    effect.process([&left, &right], [wet_left, wet_right]);
    wet_left
        .iter()
        .zip(wet_right.iter())
        .flat_map(|(left, right)| [*left, *right])
        .collect()
}

/// A deck of `fx_units` units at `mix`, called back in 64-frame blocks and
/// reading its tracks blocking.
async fn deck(fx_units: Option<usize>, mix: DeckMixSettings) -> OfflinePlayer {
    OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .crossfade_duration(0.05)
            .output_block_frames(NonZeroU32::new(64).expect("nonzero callback size"))
            .warp(
                WarpConfig::builder()
                    .render_quantum_frames(NonZeroUsize::new(64).expect("nonzero quantum"))
                    .build(),
            )
            .maybe_fx_units(fx_units)
            .mix(mix)
            .build(),
        SAMPLE_RATE,
    )
    .await
}

/// `frames` of a blocking deck, called back [`CALLBACK`] frames at a time.
/// Every block reads its own PCM, so no time passes between them.
async fn render(harness: &OfflinePlayer, frames: usize) -> Vec<f32> {
    let mut output = Vec::with_capacity(frames * CHANNELS);
    for start in (0..frames).step_by(CALLBACK) {
        output.extend(harness.render(CALLBACK.min(frames - start)).await);
    }
    output
}

async fn send(harness: &OfflinePlayer, at: When<SessionFrame>, changes: Vec<DeckFxChange>) {
    harness
        .with_queue(move |queue| queue.fx(at, changes))
        .await
        .expect("the deck takes the FX batch");
}

/// Frames a deck ramp takes to settle on its target: the product's smoothing
/// time, and one callback more for a fader, which settles at a callback's end.
fn ramp() -> usize {
    let smoothing =
        Duration::from_secs_f32(DEFAULT_SMOOTH_SECONDS).as_nanos() * u128::from(SAMPLE_RATE);
    usize::try_from(smoothing.div_ceil(1_000_000_000)).expect("a ramp fits usize") + CALLBACK
}

/// A frame `offset` frames after `base`, as a deck command's moment.
fn at(base: u64, offset: usize) -> When<SessionFrame> {
    let frame = base + u64::try_from(offset).expect("offset fits u64");
    When::At(SessionFrame::new(
        i64::try_from(frame).expect("a test frame fits the session clock"),
    ))
}

/// What a deck sounds playing `asset` for `frames` frames from its first
/// block: `setup` sent before it plays, each of `timed` at its offset into the
/// take, and the deck paused once `pause` frames have rendered when set.
async fn take(
    harness: OfflinePlayer,
    asset: Asset,
    setup: Vec<DeckFxChange>,
    timed: Vec<(usize, Vec<DeckFxChange>)>,
    pause: Option<usize>,
    frames: usize,
) -> Vec<f32> {
    if !setup.is_empty() {
        send(&harness, When::Next, setup).await;
    }
    let harness = load_source(harness, asset_source(&asset).into()).await;
    let base = harness.host().position();
    let mut output = render(&harness, LEAD).await;
    for (offset, changes) in timed {
        send(&harness, at(base, offset), changes).await;
    }
    if let Some(pause) = pause {
        output.extend(render(&harness, pause - LEAD).await);
        harness.with_queue(QueueControl::pause).await;
    }
    output.extend(render(&harness, frames - output.len() / CHANNELS).await);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
    output
}

async fn plain_take(asset: Asset, setup: Vec<DeckFxChange>) -> Vec<f32> {
    take(
        deck(None, DeckMixSettings::default()).await,
        asset,
        setup,
        Vec::new(),
        None,
        TAKE,
    )
    .await
}

fn assert_same(actual: &[f32], expected: &[f32], what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: length");
    let first = actual
        .iter()
        .zip(expected)
        .position(|(actual, expected)| actual != expected);
    assert_eq!(
        first,
        None,
        "{what}: first differing sample {:?}",
        first.map(|index| (index, actual[index], expected[index]))
    );
}

#[kithara::test(tokio)]
async fn deck_chain_neutral_is_slot_sum() {
    let take = plain_take(assets::fixed_constant_wav_cd_fifth_1323000(), Vec::new()).await;
    assert!(
        take[take.len() - LEAD..]
            .iter()
            .all(|sample| sample.to_bits() == 0.2_f32.to_bits()),
        "a neutral deck sounds its one slot exactly"
    );
}

#[kithara::test(tokio)]
async fn deck_chain_trim_scales_fx_input() {
    let unity = Arc::new(Mutex::new(Vec::with_capacity(TAKE)));
    let half = Arc::new(Mutex::new(Vec::with_capacity(TAKE)));
    let _ = plain_take(
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::probe(&unity)),
    )
    .await;
    let trimmed = DeckMixSettings::builder()
        .trim(Trim::checked(0.5).expect("half is a trim"))
        .build();
    let _ = take(
        deck(None, trimmed).await,
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::probe(&half)),
        Vec::new(),
        None,
        TAKE,
    )
    .await;
    let unity = unity.lock().clone();
    let half = half.lock().clone();
    assert!(unity.iter().any(|sample| *sample != 0.0));
    let scaled: Vec<f32> = unity.iter().map(|sample| sample * 0.5).collect();
    assert_same(&half, &scaled, "the effect hears the trimmed mix");
}

#[kithara::test(tokio)]
async fn deck_fx_observes_the_slot_gain_program_at_its_frame() {
    use kithara::play::{CurveShape, ParameterKnot, SlotGainProgram};

    let harness = deck(None, DeckMixSettings::default()).await;
    send(
        &harness,
        When::Next,
        insert(0, Curve(|sample| sample * sample)),
    )
    .await;
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let warm = render(&harness, LEAD).await;
    assert!(
        warm[warm.len() - CALLBACK * CHANNELS..]
            .iter()
            .all(|sample| *sample == 0.25)
    );
    let item = harness.player().current().expect("sounding item").id;
    let offset = LEAD + 37;
    let start = harness.host().position() + u64::try_from(offset).expect("gain offset");
    let start = SessionFrame::new(i64::try_from(start).expect("gain frame"));
    let program = SlotGainProgram::new(vec![
        ParameterKnot::new(start, 0.5, CurveShape::HOLD).expect("half amplitude"),
    ])
    .expect("constant slot gain program");
    harness
        .with_queue(move |queue| queue.gain_program(item, program))
        .await
        .expect("the slot takes the gain program");
    let output = render(&harness, LEAD + 2 * CALLBACK).await;
    for (index, frame) in output.as_chunks::<CHANNELS>().0.iter().enumerate() {
        let expected = if index < offset { 0.25_f32 } else { 0.0625_f32 };
        assert!(
            frame
                .iter()
                .all(|sample| sample.to_bits() == expected.to_bits()),
            "slot gain reaches the nonlinear FX at capture frame {index}"
        );
    }
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn deck_chain_fader_zero_leaves_fx_input_and_silences_output() {
    let open = Arc::new(Mutex::new(Vec::with_capacity(TAKE)));
    let closed = Arc::new(Mutex::new(Vec::with_capacity(TAKE)));
    let sounded = plain_take(
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::probe(&open)),
    )
    .await;
    let down = DeckMixSettings::builder()
        .volume(kithara::signal::FaderValue::from(0.0))
        .build();
    let silent = take(
        deck(None, down).await,
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::probe(&closed)),
        Vec::new(),
        None,
        TAKE,
    )
    .await;
    assert!(sounded.iter().any(|sample| *sample != 0.0));
    assert!(
        silent.iter().all(|sample| *sample == 0.0),
        "a closed fader silences the deck"
    );
    assert_same(
        &closed.lock(),
        &open.lock(),
        "the fader leaves the effect's input alone",
    );
}

#[kithara::test(tokio)]
async fn deck_fx_empty_units_are_transparent() {
    let none = take(
        deck(Some(0), DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        Vec::new(),
        Vec::new(),
        Some(LEAD + 1000),
        TAKE,
    )
    .await;
    let empty = take(
        deck(Some(2), DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        Vec::new(),
        Vec::new(),
        Some(LEAD + 1000),
        TAKE,
    )
    .await;
    assert!(none.iter().any(|sample| *sample != 0.0));
    assert_eq!(
        none.iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        empty
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        "units holding no effect leave the deck bit for bit"
    );
}

#[kithara::test(tokio)]
async fn deck_fx_units_run_in_order() {
    let dry = plain_take(assets::fixed_constant_wav_cd_fifth_1323000(), Vec::new()).await;
    let mut setup = insert(0, Curve(|sample| sample * 0.5));
    setup.extend(insert(1, Curve(|sample| sample * sample)));
    let fx = plain_take(assets::fixed_constant_wav_cd_fifth_1323000(), setup).await;
    let halved = wet(&mut Curve(|sample| sample * 0.5), &dry);
    let expected = wet(&mut Curve(|sample| sample * sample), &halved);
    let reversed = wet(
        &mut Curve(|sample| sample * 0.5),
        &wet(&mut Curve(|sample| sample * sample), &dry),
    );
    assert_ne!(expected, reversed, "the two effects do not commute");
    assert_same(&fx, &expected, "unit 2 runs on unit 1's output");
}

#[kithara::test(tokio)]
async fn deck_fx_off_is_bypass_on_is_reset() {
    const OFF: usize = LEAD + 3000;
    const ON: usize = LEAD + 7000;

    let dry = plain_take(assets::fixed_constant_wav_cd_half_44100(), Vec::new()).await;
    let fx = take(
        deck(None, DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::new(ECHO)),
        vec![
            (OFF, vec![DeckFxChange::Power { unit: 0, on: false }]),
            (ON, vec![DeckFxChange::Power { unit: 0, on: true }]),
        ],
        None,
        TAKE,
    )
    .await;
    let echoed = wet(&mut Echo::new(ECHO), &dry[..OFF * CHANNELS]);
    let fresh = wet(&mut Echo::new(ECHO), &dry[ON * CHANNELS..]);
    assert_same(&fx[..OFF * CHANNELS], &echoed, "the unit runs while on");
    assert_same(
        &fx[OFF * CHANNELS..ON * CHANNELS],
        &dry[OFF * CHANNELS..ON * CHANNELS],
        "an unpowered unit passes its input",
    );
    assert_same(
        &fx[ON * CHANNELS..],
        &fresh,
        "a unit switched on starts its effect clean",
    );
}

#[kithara::test(tokio)]
async fn deck_fx_unit_turned_dry_while_off_switches_on_dry() {
    const OFF: usize = LEAD + 3000;
    const ON: usize = LEAD + 7000;

    let dry = plain_take(assets::fixed_constant_wav_cd_half_44100(), Vec::new()).await;
    let fx = take(
        deck(None, DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Curve(|sample| -sample)),
        vec![
            (
                OFF,
                vec![
                    DeckFxChange::Power { unit: 0, on: false },
                    DeckFxChange::Mix {
                        unit: 0,
                        value: FxMix::MIN,
                    },
                ],
            ),
            (ON, vec![DeckFxChange::Power { unit: 0, on: true }]),
        ],
        None,
        TAKE,
    )
    .await;
    assert_same(
        &fx[OFF * CHANNELS..],
        &dry[OFF * CHANNELS..],
        "a unit switched on at dry/wet zero passes its input through",
    );
}

#[kithara::test(tokio)]
async fn deck_fx_one_state_across_queue_crossfade() {
    let dry = crossfade_take(Vec::new()).await;
    let fx = crossfade_take(insert(0, Echo::new(ECHO))).await;
    assert!(
        dry.iter().any(|sample| *sample > 0.2 && *sample < 0.5),
        "the take spans the whole queue crossfade"
    );
    assert_same(
        &fx,
        &wet(&mut Echo::new(ECHO), &dry),
        "one echo state runs over the faded sum of both tracks",
    );
}

/// Plays one constant into a second through the queue's crossfade at the end
/// of the first, and captures what the deck sounded from its first block,
/// `setup` sent before it plays.
async fn crossfade_take(setup: Vec<DeckFxChange>) -> Vec<f32> {
    let harness = deck(None, DeckMixSettings::default()).await;
    if !setup.is_empty() {
        send(&harness, When::Next, setup).await;
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
    let mut take = render(&harness, LEAD).await;
    // Once the incoming track has loaded, the queue fixes its crossfade to
    // the first track's end, wherever the load finished.
    kithara_integration_tests::waits::wait_for_loader_done(
        harness.player(),
        next,
        Duration::from_secs(30),
    )
    .await
    .expect("the incoming track loads");
    take.extend_from_slice(&render(&harness, CROSSFADE_TAKE - LEAD).await);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
    take
}

#[kithara::test(tokio)]
async fn deck_fx_stop_rings_out() {
    const PAUSE: usize = LEAD + 2000;

    let dry = take(
        deck(None, DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        Vec::new(),
        Vec::new(),
        Some(PAUSE),
        TAKE,
    )
    .await;
    let fx = take(
        deck(None, DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::new(ECHO)),
        Vec::new(),
        Some(PAUSE),
        TAKE,
    )
    .await;
    let silent = dry
        .chunks(CHANNELS)
        .rposition(|frame| frame != [0.0; 2])
        .expect("the dry take sounds")
        + 1;
    assert!(
        fx[silent * CHANNELS..].iter().any(|sample| *sample != 0.0),
        "the echo sounds after the deck's slots stop"
    );
    assert!(
        fx[(silent + ECHO) * CHANNELS..]
            .iter()
            .all(|sample| *sample == 0.0),
        "the deck falls silent once the echo is spent"
    );
    assert_same(
        &fx,
        &wet(&mut Echo::new(ECHO), &dry),
        "the stopped deck rings its echo out",
    );
}

#[kithara::test(tokio)]
async fn deck_fx_fader_cuts_tail() {
    const PAUSE: usize = LEAD + 2000;
    const DOWN: usize = PAUSE + 1000;
    const UP: usize = DOWN + 4000;
    const LONG: usize = ECHO * 4;

    let dry = take(
        deck(None, DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        Vec::new(),
        Vec::new(),
        Some(PAUSE),
        TAKE,
    )
    .await;
    let tail = wet(&mut Echo::new(LONG), &dry);
    let harness = deck(None, DeckMixSettings::default()).await;
    send(&harness, When::Next, insert(0, Echo::new(LONG))).await;
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let mut fx = render(&harness, PAUSE).await;
    harness.with_queue(QueueControl::pause).await;
    fx.extend(render(&harness, DOWN - PAUSE).await);
    harness.set_volume(0.0).await.expect("the fader goes down");
    fx.extend(render(&harness, UP - DOWN).await);
    harness.set_volume(1.0).await.expect("the fader comes up");
    fx.extend(render(&harness, TAKE - UP).await);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;

    let ramp = ramp();
    let closed = (DOWN + ramp) * CHANNELS..UP * CHANNELS;
    assert!(
        tail[closed.clone()].iter().any(|sample| *sample != 0.0),
        "the echo rings while the fader is down"
    );
    assert!(
        fx[closed].iter().all(|sample| *sample == 0.0),
        "a closed fader silences the tail"
    );
    assert_same(
        &fx[..DOWN * CHANNELS],
        &tail[..DOWN * CHANNELS],
        "the open fader sounds the tail",
    );
    assert_same(
        &fx[(UP + ramp) * CHANNELS..],
        &tail[(UP + ramp) * CHANNELS..],
        "the echo kept running under the closed fader",
    );
}

#[kithara::test(tokio)]
async fn deck_fx_change_lands_on_its_frame() {
    let regular = partitioned_take(&[CALLBACK]).await;
    let irregular = partitioned_take(&[37, 64, 13, 50, 1, 63]).await;
    let unchanged = plain_take(
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Clock::default()),
    )
    .await;
    let dry = plain_take(assets::fixed_constant_wav_cd_half_44100(), Vec::new()).await;
    assert_eq!(
        regular
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        irregular
            .iter()
            .map(|sample| sample.to_bits())
            .collect::<Vec<_>>(),
        "the change renders the same in any block partition"
    );
    let first = regular
        .iter()
        .zip(&unchanged)
        .position(|(changed, unchanged)| changed != unchanged)
        .map(|sample| sample / CHANNELS);
    assert_eq!(first, Some(DRY_AT), "the change takes effect on its frame");
    let settled = DRY_AT
        + regular[DRY_AT * CHANNELS..]
            .as_chunks::<CHANNELS>()
            .0
            .iter()
            .zip(dry[DRY_AT * CHANNELS..].as_chunks::<CHANNELS>().0)
            .position(|(changed, dry)| changed == dry)
            .expect("the unit settles dry");
    assert!(
        settled <= DRY_AT + ramp(),
        "the dry ramp settles within its smoothing, on frame {settled}"
    );
    assert_same(
        &regular[settled * CHANNELS..WET_AT * CHANNELS],
        &dry[settled * CHANNELS..WET_AT * CHANNELS],
        "the dry unit passes its input through",
    );
    let skipped = (WET_AT - settled) * CHANNELS;
    let wet = (WET_AT + ramp()) * CHANNELS;
    assert_same(
        &regular[wet..],
        &unchanged[wet - skipped..unchanged.len() - skipped],
        "the effect sat out exactly the frames its unit was settled dry",
    );
}

#[kithara::test(tokio)]
async fn deck_fx_ramp_outlives_a_unit_settling_dry() {
    let coarse = settling_take(CREST.end..CREST.end).await;
    let fine = settling_take(CREST).await;
    assert!(
        coarse[again() * CHANNELS..]
            .iter()
            .any(|sample| *sample != 0.0),
        "the echo sounds again once its unit turns wet"
    );
    assert_same(
        &fine,
        &coarse,
        "a ramping unit renders the same in any block partition while another settles dry",
    );
}

/// The frame [`settling_take`] pauses its deck on, on a callback's start.
const HUSH: usize = LEAD + 2048;
/// The frame it turns its echo dry, long after the deck's slot stopped and
/// before the echo is spent.
const SPENT: usize = HUSH + 4096;
/// The frame it starts its second unit's ramp, which outlasts the echo's
/// ramp to dry.
const HALF: usize = SPENT + 1500;
/// Callback-aligned frames around the one the echo's unit settles dry on and
/// [`again`].
const CREST: std::ops::Range<usize> = SPENT + 2048..SPENT + 2816;

/// The frame [`settling_take`] turns its echo wet again, while its second unit
/// still ramps: just after the echo's unit settles dry, in the same callback,
/// so a deck called back in whole callbacks never renders that unit idle.
fn again() -> usize {
    (SPENT + ramp() - CALLBACK).next_multiple_of(CALLBACK) - 4
}

/// A paused deck whose echo on unit 0 turns dry and back to wet while unit 1
/// ramps its dry/wet, rendered a frame at a time over `fine` and in
/// [`CALLBACK`] blocks elsewhere.
async fn settling_take(fine: std::ops::Range<usize>) -> Vec<f32> {
    let harness = deck(None, DeckMixSettings::default()).await;
    let mut setup = insert(0, Echo::new(ECHO * 4));
    setup.extend(insert(1, Curve(|sample| sample * 0.5)));
    send(&harness, When::Next, setup).await;
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let base = harness.host().position();
    let mut output = render(&harness, HUSH).await;
    harness.with_queue(QueueControl::pause).await;
    let half = FxMix::checked(0.5).expect("half is a dry/wet");
    for (offset, unit, value) in [
        (SPENT, 0, FxMix::MIN),
        (HALF, 1, half),
        (again(), 0, FxMix::MAX),
    ] {
        send(
            &harness,
            at(base, offset),
            vec![DeckFxChange::Mix { unit, value }],
        )
        .await;
    }
    output.extend(render(&harness, fine.start - HUSH).await);
    for _ in fine.clone() {
        output.extend(harness.render(1).await);
    }
    output.extend(render(&harness, TAKE - fine.end).await);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
    output
}

/// A take whose unit 0 clock turns dry at a frame inside a block and wet
/// again later, rendered in blocks of the sizes `blocks` cycles through.
async fn partitioned_take(blocks: &[usize]) -> Vec<f32> {
    let harness = deck(None, DeckMixSettings::default()).await;
    send(&harness, When::Next, insert(0, Clock::default())).await;
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let base = harness.host().position();
    let mut output = render(&harness, LEAD).await;
    for (offset, value) in [(DRY_AT, FxMix::MIN), (WET_AT, FxMix::MAX)] {
        send(
            &harness,
            at(base, offset),
            vec![DeckFxChange::Mix { unit: 0, value }],
        )
        .await;
    }
    let mut rendered = LEAD;
    for &block in blocks.iter().cycle() {
        if rendered >= TAKE {
            break;
        }
        let block = block.min(TAKE - rendered);
        output.extend(render(&harness, block).await);
        rendered += block;
    }
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
    output
}

#[kithara::test(tokio)]
async fn deck_fx_missing_unit_refuses_batch() {
    let echoed = plain_take(
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::new(ECHO)),
    )
    .await;
    let refused = take(
        deck(Some(2), DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::new(ECHO)),
        vec![(
            LEAD + 500,
            vec![
                DeckFxChange::Mix {
                    unit: 0,
                    value: FxMix::MIN,
                },
                DeckFxChange::Power { unit: 2, on: true },
            ],
        )],
        None,
        TAKE,
    )
    .await;
    assert_same(
        &refused,
        &echoed,
        "a batch naming a unit the deck lacks changes no unit",
    );
}

#[kithara::test(tokio)]
async fn deck_fx_delay_echoes_outlive_deck_stop() {
    const PAUSE: usize = LEAD + 2000;
    /// Frames the takes render: past the report of the tail behind the
    /// lowest high-pass.
    const FRAMES: usize = 2 * TAKE;

    let dry = take(
        deck(None, DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        Vec::new(),
        Vec::new(),
        Some(PAUSE),
        FRAMES,
    )
    .await;
    let fx = take(
        deck(None, DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, feedback_delay(DELAY)),
        Vec::new(),
        Some(PAUSE),
        FRAMES,
    )
    .await;
    let tail = wet(&mut feedback_delay(DELAY), &dry);
    let silent = dry
        .chunks(CHANNELS)
        .rposition(|frame| frame != [0.0; 2])
        .expect("the dry take sounds")
        + 1;
    // The history the delay wrote is its wet signal one delay later.
    let written = tail[DELAY * CHANNELS..]
        .chunks(CHANNELS)
        .rposition(|frame| frame.iter().any(|sample| sample.abs() > FLOOR))
        .expect("the delay writes its echoes")
        + 1;
    let spent = (written + delay_window()) * CHANNELS;
    assert!(
        spent + CALLBACK * CHANNELS < fx.len(),
        "the takes outlast the delay reporting its tail spent"
    );
    assert!(
        fx[silent * CHANNELS..].iter().any(|sample| *sample != 0.0),
        "the echoes sound after the deck's slots stop"
    );
    assert_same(
        &fx[..spent],
        &tail[..spent],
        "the stopped deck rings its echoes out on their frames and at their levels",
    );
    assert!(
        tail[spent..].iter().all(|sample| sample.abs() <= FLOOR),
        "the delay reports its tail spent only under -120 dBFS"
    );
    assert!(
        fx[spent + CALLBACK * CHANNELS..]
            .iter()
            .all(|sample| *sample == 0.0),
        "the deck falls silent within a callback of the delay reporting its tail spent"
    );
}

/// A controllable wet gain with one ratio valid at every sample rate.
struct ParameterGain {
    parameters: [FxParamDescriptor; 1],
    gain: f64,
}

impl ParameterGain {
    fn new() -> Self {
        Self {
            parameters: [FxParamDescriptor::checked(
                0.0..=1.0,
                FxParamUnit::Ratio,
                Some(FxParamMapping::Linear),
            )
            .expect("finite gain domain")],
            gain: 0.75,
        }
    }
}

impl DeckEffect for ParameterGain {
    fn process(&mut self, input: [&[f32]; 2], wet: [&mut [f32]; 2]) {
        for (input, wet) in input.into_iter().zip(wet) {
            for (sample, out) in input.iter().zip(wet) {
                *out = (f64::from(*sample) * self.gain)
                    .to_f32()
                    .expect("bounded gain output fits f32");
            }
        }
    }

    fn parameters(&self) -> &[FxParamDescriptor] {
        &self.parameters
    }

    fn set(&mut self, change: FxParamChange) {
        self.gain = change.value;
    }

    fn reset(&mut self) {
        self.gain = 0.75;
    }

    fn tail_exhausted(&self) -> bool {
        true
    }

    fn update_sample_rate(&mut self, _: NonZeroU32) {}
}

#[kithara::test(tokio)]
async fn deck_fx_mismatched_param_refuses_batch() {
    let setup = || {
        let mut gain = ParameterGain::new();
        let parameters = gain.parameters().to_vec();
        for rate in [44_100, 96_000, 768_000] {
            gain.update_sample_rate(NonZeroU32::new(rate).expect("test rate"));
            assert_eq!(gain.parameters(), parameters.as_slice());
        }
        let mut changes = insert(0, Curve(|sample| sample * 0.5));
        changes.extend(insert(1, gain));
        changes
    };
    let asset = assets::signal_wav_sine440_1s;
    let expected = plain_take(asset(), setup()).await;
    let valid = take(
        deck(Some(2), DeckMixSettings::default()).await,
        asset(),
        setup(),
        vec![
            (
                LEAD + 100,
                vec![DeckFxChange::Install {
                    unit: 1,
                    effect: Box::new(Curve(|sample| sample * 0.75)),
                }],
            ),
            (
                LEAD + 500,
                vec![
                    DeckFxChange::Clear { unit: 1 },
                    DeckFxChange::Install {
                        unit: 1,
                        effect: Box::new(ParameterGain::new()),
                    },
                    DeckFxChange::Param {
                        unit: 1,
                        change: FxParamChange {
                            index: 0,
                            value: 0.25,
                        },
                    },
                ],
            ),
        ],
        None,
        TAKE,
    )
    .await;
    assert_eq!(
        valid
            .iter()
            .zip(&expected)
            .position(|(a, b)| a != b)
            .map(|i| i / CHANNELS),
        Some(LEAD + 500),
        "a declared parameter reaches its effect on its frame",
    );
    let mut timed = Vec::new();
    for (index, change) in [
        FxParamChange {
            index: 1,
            value: 0.25,
        },
        FxParamChange {
            index: 0,
            value: 2.0,
        },
        FxParamChange {
            index: 0,
            value: f64::NAN,
        },
    ]
    .into_iter()
    .enumerate()
    {
        timed.push((
            LEAD + 500 + index * 1000,
            vec![
                DeckFxChange::Mix {
                    unit: 0,
                    value: FxMix::MIN,
                },
                DeckFxChange::Param { unit: 1, change },
            ],
        ));
    }
    for (offset, layout) in [
        (LEAD + 3500, vec![DeckFxChange::Clear { unit: 1 }]),
        (
            LEAD + 4500,
            vec![DeckFxChange::Install {
                unit: 1,
                effect: Box::new(Curve(|sample| sample)),
            }],
        ),
        (
            LEAD + 5500,
            vec![
                DeckFxChange::Install {
                    unit: 1,
                    effect: Box::new(ParameterGain::new()),
                },
                DeckFxChange::Clear { unit: 1 },
            ],
        ),
    ] {
        let mut changes = vec![DeckFxChange::Mix {
            unit: 0,
            value: FxMix::MIN,
        }];
        changes.extend(layout);
        changes.push(DeckFxChange::Param {
            unit: 1,
            change: FxParamChange {
                index: 0,
                value: 0.25,
            },
        });
        timed.push((offset, changes));
    }
    let refused = take(
        deck(Some(2), DeckMixSettings::default()).await,
        asset(),
        setup(),
        timed,
        None,
        TAKE,
    )
    .await;
    assert_same(
        &refused,
        &expected,
        "a mismatched parameter changes neither unit",
    );
}

#[kithara::test(tokio)]
async fn deck_fx_delay_mismatched_param_refuses_batch() {
    let units = || {
        let mut units = insert(0, Echo::new(ECHO));
        units.extend(insert(1, feedback_delay(DELAY)));
        units
    };
    let param = |unit, index, value| DeckFxChange::Param {
        unit,
        change: FxParamChange { index, value },
    };
    let gain = f64::from(f32::from(Feedback::MAX));
    let invalid = [
        (0, vec![param(0, 1, gain)]),
        (
            1,
            vec![param(1, 0, f64::from(delay_time(DELAY_CAPACITY + 1)))],
        ),
        (0, vec![param(1, 3, gain)]),
        (0, vec![param(1, 2, f64::NAN)]),
        (0, vec![DeckFxChange::Clear { unit: 1 }, param(1, 1, gain)]),
        (
            0,
            vec![
                DeckFxChange::Install {
                    unit: 1,
                    effect: Box::new(Echo::new(ECHO)),
                },
                param(1, 1, gain),
            ],
        ),
        (
            0,
            vec![
                DeckFxChange::Install {
                    unit: 1,
                    effect: Box::new(feedback_delay(DELAY)),
                },
                DeckFxChange::Clear { unit: 1 },
                param(1, 1, gain),
            ],
        ),
    ];
    let kept = plain_take(assets::metronome_sine_wav_full(), units()).await;
    let timed = invalid
        .into_iter()
        .enumerate()
        .map(|(index, (unit, mut changes))| {
            changes.insert(
                0,
                DeckFxChange::Mix {
                    unit,
                    value: FxMix::MIN,
                },
            );
            (LEAD + 500 + index * 1000, changes)
        })
        .collect();
    let refused = take(
        deck(None, DeckMixSettings::default()).await,
        assets::metronome_sine_wav_full(),
        units(),
        timed,
        None,
        TAKE,
    )
    .await;
    assert_same(
        &refused,
        &kept,
        "every invalid descriptor change refuses its whole batch",
    );
}
