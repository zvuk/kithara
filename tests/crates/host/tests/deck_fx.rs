#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    effects::{DeckEffect, FxParamChange},
    platform::sync::{Arc, Mutex},
    play::{DeckFxChange, DeckMixSettings, FxMix, Trim},
    queue::{QueueControl, TrackSource, Transition},
    signal::SessionFrame,
    warp::WarpConfig,
};
use kithara_command::When;
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions, asset_source};
use kithara_test_fixtures::{asset::Asset, assets};

use crate::mix_tap::{load_source, render_effect_window};

const SAMPLE_RATE: u32 = 44_100;
const CALLBACK: usize = 64;
const CHANNELS: usize = 2;
/// Frames the test echo holds its input for.
const ECHO: usize = 2048;
/// Frames a take renders before its timed commands are due.
const LEAD: usize = 4096;
/// Frames a take renders.
const TAKE: usize = 16_384;

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

    fn set(&mut self, change: FxParamChange) {
        match change {}
    }

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

    fn set(&mut self, change: FxParamChange) {
        match change {}
    }

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

async fn send(harness: &OfflinePlayer, at: When<SessionFrame>, changes: Vec<DeckFxChange>) {
    harness
        .with_queue(move |queue| queue.fx(at, changes))
        .await
        .expect("the deck takes the FX batch");
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
    let mut output = render_effect_window(&harness, LEAD).await;
    for (offset, changes) in timed {
        send(&harness, at(base, offset), changes).await;
    }
    if let Some(pause) = pause {
        output.extend(render_effect_window(&harness, pause - LEAD).await);
        harness.with_queue(QueueControl::pause).await;
    }
    output.extend(render_effect_window(&harness, TAKE - output.len() / CHANNELS).await);
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

/// Plays one constant, crossfades to a second, and captures what the deck
/// sounded from its first block, `setup` sent before it plays.
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
    let mut take = render_effect_window(&harness, 2048).await;
    harness
        .with_queue(|queue| queue.next(Transition::Crossfade))
        .await
        .expect("the queue crossfades to the next track");
    kithara_integration_tests::waits::wait_for_loader_done(
        harness.player(),
        next,
        kithara::platform::time::Duration::from_secs(30),
    )
    .await
    .expect("the incoming track loads");
    take.extend_from_slice(&render_effect_window(&harness, 8192).await);
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
    const UP: usize = DOWN + 3000;
    /// Frames a fader move takes to settle on its target.
    const RAMP: usize = 2048;
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
    let mut fx = render_effect_window(&harness, PAUSE).await;
    harness.with_queue(QueueControl::pause).await;
    fx.extend(render_effect_window(&harness, DOWN - PAUSE).await);
    harness.set_volume(0.0).await.expect("the fader goes down");
    fx.extend(render_effect_window(&harness, UP - DOWN).await);
    harness.set_volume(1.0).await.expect("the fader comes up");
    fx.extend(render_effect_window(&harness, TAKE - UP).await);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;

    let closed = (DOWN + RAMP) * CHANNELS..UP * CHANNELS;
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
        &fx[(UP + RAMP) * CHANNELS..],
        &tail[(UP + RAMP) * CHANNELS..],
        "the echo kept running under the closed fader",
    );
}

#[kithara::test(tokio)]
async fn deck_fx_change_lands_on_its_frame() {
    const CHANGE: usize = LEAD + 1000 + 37;

    let regular = partitioned_take(&[CALLBACK]).await;
    let irregular = partitioned_take(&[37, 64, 13, 50, 1, 63]).await;
    let unchanged = plain_take(
        assets::fixed_constant_wav_cd_half_44100(),
        insert(0, Echo::new(ECHO)),
    )
    .await;
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
    assert_eq!(first, Some(CHANGE), "the change takes effect on its frame");
}

/// A take whose unit 0 echo turns half dry at a frame inside a block, rendered
/// in blocks of the sizes `blocks` cycles through.
async fn partitioned_take(blocks: &[usize]) -> Vec<f32> {
    const CHANGE: usize = LEAD + 1000 + 37;

    let harness = deck(None, DeckMixSettings::default()).await;
    send(&harness, When::Next, insert(0, Echo::new(ECHO))).await;
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let base = harness.host().position();
    let mut output = render_effect_window(&harness, LEAD).await;
    send(
        &harness,
        at(base, CHANGE),
        vec![DeckFxChange::Mix {
            unit: 0,
            value: FxMix::checked(0.5).expect("half is a dry/wet"),
        }],
    )
    .await;
    let mut rendered = LEAD;
    for &block in blocks.iter().cycle() {
        if rendered >= TAKE {
            break;
        }
        let block = block.min(TAKE - rendered);
        output.extend(render_effect_window(&harness, block).await);
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
