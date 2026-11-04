#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    dsp::param::DEFAULT_SMOOTH_SECONDS,
    effects::{DeckEffect, FxParamChange},
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

    fn accepts(&self, _: &FxParamChange) -> bool {
        false
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

    fn accepts(&self, _: &FxParamChange) -> bool {
        false
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

    fn accepts(&self, _: &FxParamChange) -> bool {
        false
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

/// What a deck sounds playing `asset` for [`TAKE`] frames from its first
/// block: `setup` sent before it plays, each of `timed` at its offset into the
/// take, and the deck paused once `pause` frames have rendered when set.
async fn take(
    harness: OfflinePlayer,
    asset: Asset,
    setup: Vec<DeckFxChange>,
    timed: Vec<(usize, Vec<DeckFxChange>)>,
    pause: Option<usize>,
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
    output.extend(render(&harness, TAKE - output.len() / CHANNELS).await);
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
    )
    .await;
    let unity = unity.lock().clone();
    let half = half.lock().clone();
    assert!(unity.iter().any(|sample| *sample != 0.0));
    let scaled: Vec<f32> = unity.iter().map(|sample| sample * 0.5).collect();
    assert_same(&half, &scaled, "the effect hears the trimmed mix");
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
    )
    .await;
    let empty = take(
        deck(Some(2), DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        Vec::new(),
        Vec::new(),
        Some(LEAD + 1000),
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
    )
    .await;
    let fx = take(
        deck(None, DeckMixSettings::default()).await,
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::new(ECHO)),
        Vec::new(),
        Some(PAUSE),
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
    )
    .await;
    assert_same(
        &refused,
        &echoed,
        "a batch naming a unit the deck lacks changes no unit",
    );
}
