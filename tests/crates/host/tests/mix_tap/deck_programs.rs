use std::num::{NonZeroU32, NonZeroUsize};

use kithara::{
    play::{CurveShape, DeckParameter, DeckProgram, EqBandConfig, ParameterKnot, PlayError},
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

/// A deck control a partitioned take programs.
#[derive(Clone, Copy, Debug)]
enum Control {
    Trim,
    Volume,
    Crossfader,
    /// The one band of a one-band EQ layout, in dB.
    Band,
}

async fn prepared(callback: usize, bands: usize) -> OfflinePlayer {
    let block = NonZeroU32::new(u32::try_from(callback).expect("callback fits u32"))
        .expect("nonzero callback");
    let options = OfflinePlayerOptions::builder()
        .block_on_underrun(true)
        .eq_layout(vec![EqBandConfig::builder().build(); bands])
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

async fn programmed_take(control: Control, callback: usize) -> Vec<f32> {
    let harness = prepared(callback, usize::from(matches!(control, Control::Band))).await;
    let parameter = match control {
        Control::Trim => DeckParameter::Trim,
        Control::Volume => DeckParameter::Volume,
        Control::Crossfader => DeckParameter::Crossfader,
        Control::Band => DeckParameter::EqBand {
            band: 0,
            layout: harness
                .deck_snapshot()
                .eq
                .layout()
                .expect("observed EQ layout"),
        },
    };
    // Each program starts from the control's manual value: unity, or 0 dB.
    let [open, closed] = match control {
        Control::Band => [0.0, -12.0],
        Control::Trim | Control::Volume | Control::Crossfader => [1.0, 0.25],
    };
    let start = harness.host().position();
    let first = start + u64::try_from(FIRST).expect("first offset fits u64");
    let last = first + u64::try_from(SEGMENT).expect("segment fits u64");
    let program = DeckProgram::new(
        parameter,
        vec![
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(first).expect("first knot fits the clock")),
                open,
                CurveShape::cubic(0.0, 1.0).expect("bounded S curve"),
            )
            .expect("first knot"),
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(last).expect("last knot fits the clock")),
                closed,
                CurveShape::HOLD,
            )
            .expect("last knot"),
        ],
    )
    .expect("prepared native control program");
    harness
        .with_queue(move |queue| queue.deck_programs(vec![program]))
        .await
        .expect("the mixer can apply a deck program to buffered slot PCM");

    let mut take = Vec::with_capacity(CAPTURE * CHANNELS);
    for offset in (0..CAPTURE).step_by(callback) {
        take.extend(harness.render(callback.min(CAPTURE - offset)).await);
    }
    assert_eq!(take.len(), CAPTURE * CHANNELS);
    for (offset, channels) in take.as_chunks::<CHANNELS>().0.iter().enumerate() {
        let value = if offset < FIRST {
            open
        } else if offset >= FIRST + SEGMENT {
            closed
        } else {
            let t = f64::from(u32::try_from(offset - FIRST).expect("local frame fits u32")) / 128.0;
            open + (closed - open) * (3.0 * t * t - 2.0 * t * t * t)
        }
        .to_f32()
        .expect("control value fits f32");
        let gain = match control {
            Control::Volume => value * value,
            Control::Band => 10.0_f32.powf(value / 20.0),
            Control::Trim | Control::Crossfader => value,
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
#[case::trim(Control::Trim)]
#[case::volume(Control::Volume)]
#[case::crossfader(Control::Crossfader)]
#[case::eq_band(Control::Band)]
async fn public_deck_programs_apply_every_frame_and_hold_the_last_value(#[case] control: Control) {
    let regular = programmed_take(control, 64).await;
    let prime = programmed_take(control, 13).await;
    assert!(
        regular
            .iter()
            .map(|sample| sample.to_bits())
            .eq(prime.iter().map(|sample| sample.to_bits())),
        "the callback partition changes no output bit for {control:?}"
    );
}

#[kithara::test(tokio)]
async fn public_deck_programs_admit_the_next_unrendered_frame_and_refuse_the_previous_one() {
    let harness = prepared(64, 0).await;
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
            .with_queue(move |queue| queue.deck_programs(vec![past]))
            .await,
        Err(QueueError::Play(PlayError::Late))
    ));
    let due = program(frontier);
    harness
        .with_queue(move |queue| queue.deck_programs(vec![due]))
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

/// The frame `frames` after `frame`.
fn after(frame: SessionFrame, frames: i64) -> SessionFrame {
    SessionFrame::new(i64::from(frame).saturating_add(frames))
}

/// A one-knot program holding `value` on `parameter` from `at`.
fn held(parameter: DeckParameter, at: SessionFrame, value: f64) -> DeckProgram {
    DeckProgram::new(
        parameter,
        vec![ParameterKnot::new(at, value, CurveShape::HOLD).expect("native value")],
    )
    .expect("program")
}

#[kithara::test(tokio)]
async fn public_deck_programs_refuse_an_unknown_band_and_a_stale_layout_as_one_batch() {
    let harness = prepared(64, 1).await;
    let snapshot = harness.deck_snapshot();
    let layout = snapshot.eq.layout().expect("observed EQ layout");
    let next = after(snapshot.at, 64);
    let unknown = held(DeckParameter::EqBand { band: 1, layout }, next, -6.0);
    assert!(matches!(
        harness
            .with_queue(move |queue| queue.deck_programs(vec![unknown]))
            .await,
        Err(QueueError::Play(PlayError::EqBandOutOfRange {
            band: 1,
            bands: 1
        }))
    ));
    let band = DeckParameter::EqBand { band: 0, layout };
    let running = DeckProgram::new(
        band,
        [(next, 0.0), (after(next, 4096), -12.0)]
            .into_iter()
            .map(|(at, db)| ParameterKnot::new(at, db, CurveShape::LINEAR).expect("finite dB knot"))
            .collect(),
    )
    .expect("band program");
    harness
        .with_queue(move |queue| queue.deck_programs(vec![running]))
        .await
        .expect("current layout");
    let _ = render_effect_window(&harness, 1024).await;
    let moving = harness.deck_snapshot();
    assert!(moving.eq.program(0).is_some(), "the band program runs");
    let replaced = after(moving.at, 64);
    let trim = held(DeckParameter::Trim, replaced, 0.5);
    let stale = held(band, replaced, -6.0);
    harness
        .with_queue(move |queue| {
            queue.set_eq_layout(vec![EqBandConfig::builder().build()])?;
            queue.deck_programs(vec![trim, stale])
        })
        .await
        .expect("both batches reach the deck");
    let _ = render_effect_window(&harness, 1024).await;
    let snapshot = harness.deck_snapshot();
    assert_ne!(snapshot.eq.layout(), Some(layout), "the layout advanced");
    assert_eq!(
        snapshot.eq.program(0),
        None,
        "the stale band program stopped"
    );
    let trim = snapshot.parameter(DeckParameter::Trim).expect("trim");
    assert_eq!(trim.program(), None, "the refused batch started no part");
    assert_eq!(trim.value(), 1.0);
    let take = render_effect_window(&harness, 256).await;
    assert!(
        take.iter()
            .all(|sample| sample.to_bits() == 0.5_f32.to_bits()),
        "the deck sounds its neutral new layout and untouched trim"
    );
    let old = held(band, after(snapshot.at, 64), -6.0);
    assert!(
        harness
            .with_queue(move |queue| queue.deck_programs(vec![old]))
            .await
            .is_err(),
        "a program naming the replaced layout is refused"
    );
    assert_eq!(harness.metrics().underruns(), 0);
    harness.close().await;
}
