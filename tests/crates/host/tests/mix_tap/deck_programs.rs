use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    play::{CurveShape, DeckParameter, DeckProgram, ParameterKnot, PlayError},
    queue::QueueError,
    signal::SessionFrame,
    warp::WarpConfig,
};
use kithara_integration_tests::offline::{OfflinePlayer, OfflinePlayerOptions, asset_source};
use kithara_test_fixtures::assets;
use num_traits::ToPrimitive;

use super::cases::{SAMPLE_RATE, load_source, render_effect_window};

const FIRST: usize = 37;
const SEGMENT: usize = 128;
const CAPTURE: usize = 1024;
const CHANNELS: usize = 2;

async fn prepared(callback: usize) -> OfflinePlayer {
    let block = NonZeroU32::new(u32::try_from(callback).expect("callback fits u32"))
        .expect("nonzero callback");
    let options = OfflinePlayerOptions::builder()
        .block_on_underrun(true)
        .eq_layout(Vec::new())
        .output_block_frames(block)
        .warp(
            WarpConfig::builder()
                .render_quantum_frames(NonZeroUsize::new(callback).expect("nonzero quantum"))
                .build(),
        )
        .build();
    let harness = load_source(
        OfflinePlayer::with_sample_rate(options, SAMPLE_RATE).await,
        asset_source(&assets::fixed_constant_wav_cd_half_44100()).into(),
    )
    .await;
    let warm = render_effect_window(&harness, 4096).await;
    assert!(
        warm.get(warm.len().saturating_sub(128)..)
            .expect("warm suffix")
            .iter()
            .all(|sample| sample.to_bits() == 0.5_f32.to_bits()),
        "the neutral deck supplies the independent constant input"
    );
    harness
}

async fn programmed_take(parameter: DeckParameter, callback: usize) -> Vec<f32> {
    let harness = prepared(callback).await;
    let start = harness.host().position();
    let first = start + u64::try_from(FIRST).expect("first offset fits u64");
    let last = first + u64::try_from(SEGMENT).expect("segment fits u64");
    let program = DeckProgram::new(
        parameter,
        vec![
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(first).expect("first knot fits the clock")),
                1.0,
                CurveShape::cubic(0.0, 1.0).expect("bounded S curve"),
            )
            .expect("first knot"),
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(last).expect("last knot fits the clock")),
                0.25,
                CurveShape::HOLD,
            )
            .expect("last knot"),
        ],
    )
    .expect("prepared native control program");
    harness
        .with_queue(move |queue| queue.deck_program(program))
        .await
        .expect("the mixer can apply a deck program to buffered slot PCM");

    let mut take = Vec::with_capacity(CAPTURE * CHANNELS);
    for offset in (0..CAPTURE).step_by(callback) {
        take.extend(harness.render(callback.min(CAPTURE - offset)).await);
    }
    assert_eq!(take.len(), CAPTURE * CHANNELS);
    for (offset, channels) in take.as_chunks::<CHANNELS>().0.iter().enumerate() {
        let value = if offset < FIRST {
            1.0
        } else if offset >= FIRST + SEGMENT {
            0.25
        } else {
            let t = f64::from(u32::try_from(offset - FIRST).expect("local frame fits u32")) / 128.0;
            1.0 - 0.75 * (3.0 * t * t - 2.0 * t * t * t)
        }
        .to_f32()
        .expect("control value fits f32");
        let gain = if matches!(parameter, DeckParameter::Volume) {
            value * value
        } else {
            value
        };
        assert_eq!(
            channels.map(f32::to_bits),
            [0.5 * gain; CHANNELS].map(f32::to_bits),
            "{parameter:?} on frame {offset}, callback {callback}"
        );
    }
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
    take
}

#[kithara::test(tokio)]
#[case::trim(DeckParameter::Trim)]
#[case::volume(DeckParameter::Volume)]
#[case::crossfader(DeckParameter::Crossfader)]
async fn public_deck_programs_apply_every_frame_and_hold_the_last_value(
    #[case] parameter: DeckParameter,
) {
    let regular = programmed_take(parameter, 64).await;
    let prime = programmed_take(parameter, 13).await;
    assert!(
        regular
            .iter()
            .map(|sample| sample.to_bits())
            .eq(prime.iter().map(|sample| sample.to_bits())),
        "the callback partition changes no output bit for {parameter:?}"
    );
}

#[kithara::test(tokio)]
async fn public_deck_programs_admit_the_next_unrendered_frame_and_refuse_the_previous_one() {
    let harness = prepared(64).await;
    let frontier = harness.deck_snapshot().at;
    assert_eq!(
        i64::from(frontier),
        i64::try_from(harness.host().position()).expect("rendered frame")
    );
    let program = |at| {
        DeckProgram::new(
            DeckParameter::Trim,
            vec![ParameterKnot::new(at, 0.5, CurveShape::HOLD).expect("native value")],
        )
        .expect("program")
    };
    let past = program(SessionFrame::new(i64::from(frontier).saturating_sub(1)));
    assert!(matches!(
        harness
            .with_queue(move |queue| queue.deck_program(past))
            .await,
        Err(QueueError::Play(PlayError::Late))
    ));
    let due = program(frontier);
    harness
        .with_queue(move |queue| queue.deck_program(due))
        .await
        .expect("next unrendered frame needs no producer lead");
    assert_eq!(harness.render(1).await, [0.25; 2]);
    let snapshot = harness.deck_snapshot();
    let applied = snapshot.parameter(DeckParameter::Trim).expect("trim");
    assert_eq!(applied.value(), 0.5);
    assert!(applied.program().is_some());
    assert_eq!(
        snapshot.at,
        SessionFrame::new(i64::from(frontier).saturating_add(1))
    );
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}
