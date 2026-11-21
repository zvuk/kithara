#![cfg(not(target_arch = "wasm32"))]

use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    play::{
        CurveShape, DeckFxChange, DeckParameter, DeckProgram, FxMix, FxTarget, LayoutRevision,
        ParameterKnot, PlayError,
    },
    queue::QueueError,
    signal::SessionFrame,
    warp::WarpConfig,
};
use kithara_command::When;
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions, asset_source};
use kithara_test_fixtures::assets;
use num_traits::ToPrimitive;

use crate::{deck_fx::Curve, mix_tap::load_source};

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: usize = 2;
/// Frames a deck renders before its program is due.
const WARM: usize = 4096;
/// Frames into the capture the program's first knot lands on.
const FIRST: usize = 37;
/// Frames the dry/wet program moves over.
const SEGMENT: usize = 128;
/// Frames each take captures from the program's call.
const CAPTURE: usize = 1024;
/// Frames a long program runs, longer than any take.
const LONG: i64 = 1 << 20;
/// The constant every take plays.
const INPUT: f32 = 0.5;

/// The wet signal of the test effect: half its input.
fn halve(sample: f32) -> f32 {
    sample * 0.5
}

/// A deck of two FX units playing a constant `INPUT`, called back `callback`
/// frames at a time, with the halving effect installed on each unit, powered
/// on and fully dry.
async fn prepared(callback: usize) -> OfflinePlayer {
    let block = NonZeroU32::new(u32::try_from(callback).expect("callback fits u32"))
        .expect("nonzero callback");
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .block_on_underrun(true)
            .output_block_frames(block)
            .warp(
                WarpConfig::builder()
                    .render_quantum_frames(NonZeroUsize::new(callback).expect("nonzero quantum"))
                    .build(),
            )
            .fx_units(2)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let mut changes = Vec::new();
    for unit in 0..2 {
        changes.extend([
            DeckFxChange::Install {
                unit,
                effect: Box::new(Curve(halve)),
            },
            DeckFxChange::Mix {
                unit,
                value: FxMix::MIN,
            },
            DeckFxChange::Power { unit, on: true },
        ]);
    }
    harness
        .with_queue(move |queue| queue.fx(When::Next, changes))
        .await
        .expect("the deck takes its units");
    let harness = load_source(
        harness,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let warm = render(&harness, WARM, callback).await;
    assert!(
        warm[warm.len() - 128..]
            .iter()
            .all(|sample| sample.to_bits() == INPUT.to_bits()),
        "dry units leave the constant"
    );
    harness
}

async fn render(harness: &OfflinePlayer, frames: usize, callback: usize) -> Vec<f32> {
    let mut output = Vec::with_capacity(frames * CHANNELS);
    for start in (0..frames).step_by(callback) {
        output.extend(harness.render(callback.min(frames - start)).await);
    }
    output
}

fn frame(at: u64) -> SessionFrame {
    SessionFrame::new(i64::try_from(at).expect("a test frame fits the clock"))
}

fn after(at: SessionFrame, frames: i64) -> SessionFrame {
    SessionFrame::new(i64::from(at).saturating_add(frames))
}

fn layout(harness: &OfflinePlayer, unit: usize) -> LayoutRevision {
    harness.deck_snapshot().fx[unit]
        .layout
        .expect("the deck reports each unit's revision")
}

fn mix(unit: usize, layout: LayoutRevision) -> DeckParameter {
    DeckParameter::Fx {
        unit,
        layout,
        target: FxTarget::Mix,
    }
}

/// A program moving `parameter` linearly from `from` at `start` to `to`
/// [`LONG`] frames later.
fn long(parameter: DeckParameter, start: SessionFrame, [from, to]: [f64; 2]) -> DeckProgram {
    DeckProgram::new(
        parameter,
        vec![
            ParameterKnot::new(start, from, CurveShape::LINEAR).expect("first knot"),
            ParameterKnot::new(after(start, LONG), to, CurveShape::HOLD).expect("last knot"),
        ],
    )
    .expect("a long program")
}

/// What unit 0 sounds under a dry/wet program from fully dry to fully wet,
/// called back `callback` frames at a time.
async fn mix_take(callback: usize) -> Vec<f32> {
    let harness = prepared(callback).await;
    let start = harness.host().position();
    let first = frame(start + u64::try_from(FIRST).expect("offset fits u64"));
    let program = DeckProgram::new(
        mix(0, layout(&harness, 0)),
        vec![
            ParameterKnot::new(
                first,
                0.0,
                CurveShape::cubic(0.0, 1.0).expect("bounded S curve"),
            )
            .expect("first knot"),
            ParameterKnot::new(
                after(first, i64::try_from(SEGMENT).expect("segment fits i64")),
                1.0,
                CurveShape::HOLD,
            )
            .expect("last knot"),
        ],
    )
    .expect("a dry/wet program");
    harness
        .with_queue(move |queue| queue.deck_programs(vec![program]))
        .await
        .expect("the deck takes the dry/wet program");
    let take = render(&harness, CAPTURE, callback).await;
    for (offset, channels) in take.as_chunks::<CHANNELS>().0.iter().enumerate() {
        let wet = if offset < FIRST {
            0.0
        } else if offset >= FIRST + SEGMENT {
            1.0
        } else {
            let t = (offset - FIRST).to_f64().expect("local frame") / 128.0;
            3.0 * t * t - 2.0 * t * t * t
        }
        .to_f32()
        .expect("a dry/wet fits f32");
        let expected = INPUT * (1.0 - wet) + halve(INPUT) * wet;
        for sample in channels {
            assert!(
                (sample - expected).abs() <= f32::EPSILON,
                "frame {offset}, callback {callback}: {sample} against {expected}"
            );
        }
    }
    let snapshot = harness.deck_snapshot();
    let applied = snapshot
        .parameter(mix(0, layout(&harness, 0)))
        .expect("unit 0 dry/wet");
    assert_eq!(applied.value(), 1.0, "the last knot holds");
    assert!(applied.program().is_some());
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
    take
}

#[kithara::test(tokio)]
async fn a_dry_wet_program_applies_on_its_frames_under_two_partitions() {
    let regular = mix_take(64).await;
    let prime = mix_take(13).await;
    assert!(
        regular
            .iter()
            .map(|sample| sample.to_bits())
            .eq(prime.iter().map(|sample| sample.to_bits())),
        "the callback partition changes no output bit"
    );
}

#[kithara::test(tokio)]
async fn a_replaced_effect_returns_only_its_unit_dry_wet_program() {
    let harness = prepared(64).await;
    let start = after(harness.deck_snapshot().at, 64);
    let programs = vec![
        long(DeckParameter::Crossfader, start, [1.0, 0.5]),
        long(mix(0, layout(&harness, 0)), start, [0.0, 1.0]),
        long(mix(1, layout(&harness, 1)), start, [0.0, 1.0]),
    ];
    harness
        .with_queue(move |queue| queue.deck_programs(programs))
        .await
        .expect("the deck takes three programs");
    let _ = render(&harness, 1024, 64).await;
    let replaced = layout(&harness, 0);
    let swap = vec![DeckFxChange::Install {
        unit: 0,
        effect: Box::new(Curve(halve)),
    }];
    harness
        .with_queue(move |queue| queue.fx(When::Next, swap))
        .await
        .expect("the deck takes the new effect");
    let _ = render(&harness, 256, 64).await;
    let snapshot = harness.deck_snapshot();
    let unit = |unit: usize| snapshot.fx[unit].mix;
    assert_ne!(snapshot.fx[0].layout, Some(replaced), "unit 0 advanced");
    assert_eq!(
        unit(0).program(),
        None,
        "the replaced unit's program stopped"
    );
    assert!(unit(1).program().is_some(), "the other unit's program runs");
    let crossfader = snapshot
        .parameter(DeckParameter::Crossfader)
        .expect("crossfader");
    assert!(
        crossfader.program().is_some(),
        "the crossfader program runs"
    );
    let stale = long(mix(0, replaced), after(snapshot.at, 64), [0.0, 1.0]);
    assert!(
        matches!(
            harness
                .with_queue(move |queue| queue.deck_programs(vec![stale]))
                .await,
            Err(QueueError::Play(PlayError::NotReady))
        ),
        "a program naming the replaced effect is refused"
    );
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}

#[kithara::test(tokio)]
async fn a_dry_wet_program_on_an_empty_unit_refuses_its_whole_batch() {
    let harness = prepared(64).await;
    harness
        .with_queue(|queue| queue.fx(When::Next, vec![DeckFxChange::Clear { unit: 1 }]))
        .await
        .expect("the deck empties unit 1");
    let _ = render(&harness, 64, 64).await;
    let start = after(harness.deck_snapshot().at, 64);
    let programs = vec![
        long(DeckParameter::Trim, start, [1.0, 0.5]),
        long(mix(1, layout(&harness, 1)), start, [0.0, 1.0]),
    ];
    harness
        .with_queue(move |queue| queue.deck_programs(programs))
        .await
        .expect("the batch reaches the deck");
    let take = render(&harness, 1024, 64).await;
    assert!(
        take.iter()
            .all(|sample| sample.to_bits() == INPUT.to_bits()),
        "no part of the refused batch sounds"
    );
    let snapshot = harness.deck_snapshot();
    let trim = snapshot.parameter(DeckParameter::Trim).expect("trim");
    assert_eq!(trim.program(), None);
    assert_eq!(trim.value(), 1.0);
    assert_eq!(snapshot.fx[1].mix.program(), None);
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}
