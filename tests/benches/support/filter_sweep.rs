use std::hint::black_box;

use criterion::{BatchSize, Criterion};
use kithara::{
    effects::filter::{FilterEffect, FilterKnob, FilterSettings},
    play::{CurveShape, DeckParameter, DeckProgram, ParameterKnot},
    signal::SessionFrame,
};
use kithara_command::When;
use kithara_integration_tests::offline::OfflinePlayer;
use num_traits::ToPrimitive;

const PRELUDE: usize = 529;
const FILTER_WARM: usize = 64;

const START: f64 = -0.2;
const END: f64 = -0.8;

fn knob(value: f64) -> FilterKnob {
    FilterKnob::checked(value).expect("knob position")
}

fn at(frame: u64) -> When<SessionFrame> {
    When::At(SessionFrame::new(
        i64::try_from(frame).expect("session frame"),
    ))
}

/// Centers the knob, then turns it to the program's start and lets the
/// glide settle, so every iteration starts from the same filter state.
async fn prepare(harness: &OfflinePlayer) {
    harness
        .with_queue(|queue| queue.filter(When::Next, FilterKnob::DEFAULT))
        .await
        .expect("center the knob and cancel programs");
    harness.seek(0.0).await;
    let warm = harness.render(4096).await;
    assert!(warm[warm.len() - 128..].iter().all(|sample| *sample == 0.5));
    let turn = harness.host().position() + u64::try_from(PRELUDE).expect("prelude");
    harness
        .with_queue(move |queue| queue.filter(at(turn), knob(START)))
        .await
        .expect("turn to the program start");
    let turned = harness.render(PRELUDE + FILTER_WARM).await;
    assert!(turned[..PRELUDE * 2].iter().all(|sample| *sample == 0.5));
    assert_eq!(harness.deck_snapshot().filter.knob(), START);
    assert_eq!(harness.metrics().underruns(), 0);
}

async fn submit(harness: &OfflinePlayer, shape: CurveShape, frames: usize) {
    let start = harness.host().position() + u64::try_from(PRELUDE).expect("prelude");
    let end = start + u64::try_from(frames).expect("curve length");
    let program = DeckProgram::new(
        DeckParameter::Filter,
        vec![
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(start).expect("start")),
                START,
                shape,
            )
            .expect("left"),
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(end).expect("end")),
                END,
                CurveShape::HOLD,
            )
            .expect("right"),
        ],
    )
    .expect("prepared knob trajectory");
    harness
        .with_queue(move |queue| queue.deck_program(program))
        .await
        .expect("public curve submission");
}

fn position(frame: usize, frames: usize, shape: CurveShape) -> f64 {
    if frame >= frames {
        return END;
    }
    START
        + (END - START)
            * shape.progress(frame.to_f64().expect("offset") / frames.to_f64().expect("length"))
}

async fn oracle(
    harness: &OfflinePlayer,
    shape: CurveShape,
    frames: usize,
    prelude: usize,
) -> (Vec<f32>, f64) {
    let rate = harness.host().spec().await.sample_rate;
    let mut filter = FilterEffect::new(rate, FilterSettings::default());
    filter.configure(knob(START));
    let warm = FILTER_WARM + if prelude == 0 { PRELUDE } else { 0 };
    let take = if prelude == 0 { frames } else { 1024 };
    let mut expected = Vec::with_capacity(take * 2);
    for frame in 0..warm + take {
        let output = frame.checked_sub(warm);
        let (mut left, mut right) = ([0.5], [0.5]);
        let planes = &mut [&mut left[..], &mut right[..]];
        match output.and_then(|offset| offset.checked_sub(prelude)) {
            Some(offset) => {
                filter.process_program(planes, 0..1, &[position(offset, frames, shape)])
            }
            None => filter.process(planes, 0..1),
        }
        .expect("reference frame");
        if output.is_some() {
            expected.extend([left[0], right[0]]);
        }
    }
    (expected, filter.knob())
}

pub(super) fn bench(c: &mut Criterion) {
    let rt = super::make_runtime();
    let harness = rt.block_on(super::slot_gain::loaded_player());
    {
        let mut group = c.benchmark_group("filter_sweep");
        group.sample_size(20);
        for (name, shape) in [
            ("hold", CurveShape::HOLD),
            ("linear", CurveShape::LINEAR),
            ("cubic", CurveShape::cubic(0.0, 1.0).expect("S shape")),
        ] {
            for frames in [13, 256] {
                let (expected, target) = rt.block_on(oracle(&harness, shape, frames, 0));
                group.bench_function(format!("render_{name}/{frames}"), |b| {
                    let mut last = Vec::new();
                    b.iter_batched(
                        || {
                            rt.block_on(async {
                                prepare(&harness).await;
                                submit(&harness, shape, frames).await;
                                let _ = harness.render(PRELUDE).await;
                            });
                        },
                        |()| {
                            last = rt.block_on(harness.render(frames));
                            black_box(&last);
                        },
                        BatchSize::PerIteration,
                    );
                    assert_eq!(last, expected);
                    let snapshot = harness.deck_snapshot().filter;
                    assert_eq!(snapshot.knob().to_bits(), target.to_bits());
                    assert!(snapshot.program().is_some());
                    assert_eq!(harness.metrics().underruns(), 0);
                });
            }
        }
        let shape = CurveShape::cubic(0.0, 1.0).expect("S shape");
        let (expected, target) = rt.block_on(oracle(&harness, shape, 128, PRELUDE));
        group.bench_function("public_host_cubic/1024", |b| {
            let mut last = Vec::new();
            b.iter_batched(
                || rt.block_on(prepare(&harness)),
                |()| {
                    last = rt.block_on(async {
                        submit(&harness, shape, 128).await;
                        harness.render(1024).await
                    });
                    black_box(&last);
                },
                BatchSize::PerIteration,
            );
            assert_eq!(last, expected);
            let snapshot = harness.deck_snapshot().filter;
            assert_eq!(snapshot.knob().to_bits(), target.to_bits());
            assert!(snapshot.program().is_some());
            assert_eq!(harness.metrics().underruns(), 0);
        });
        group.finish();
    }
    rt.block_on(harness.close());
}
