use std::hint::black_box;

use criterion::{BatchSize, Criterion};
use kithara::{
    play::{CurveShape, DeckParameter, DeckProgram, EqBandConfig, GainDb, ParameterKnot},
    signal::SessionFrame,
};
use kithara_integration_tests::offline::OfflinePlayer;
use num_traits::ToPrimitive;

const PRELUDE: usize = 529;

async fn player() -> OfflinePlayer {
    let harness = super::slot_gain::loaded_player().await;
    harness
        .with_queue(|queue| queue.set_eq_layout(vec![EqBandConfig::default()]))
        .await
        .expect("one-band layout");
    let warm = harness.render(4096).await;
    assert!(warm[warm.len() - 128..].iter().all(|sample| *sample == 0.5));
    harness
}

async fn reset(harness: &OfflinePlayer) {
    harness
        .with_queue(|queue| queue.set_eq_gain(0, 0.0))
        .await
        .expect("manual reset");
    harness.seek(0.0).await;
    let warm = harness.render(4096).await;
    assert!(warm[warm.len() - 128..].iter().all(|sample| *sample == 0.5));
}

async fn submit(harness: &OfflinePlayer, shape: CurveShape, frames: usize) {
    let start = harness.host().position() + u64::try_from(PRELUDE).expect("prelude");
    let end = start + u64::try_from(frames).expect("curve length");
    let program = DeckProgram::new(
        DeckParameter::EqBand {
            band: 0,
            layout: harness.deck_snapshot().eq.layout().expect("EQ layout"),
        },
        vec![
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(start).expect("start")),
                0.0,
                shape,
            )
            .expect("start knot"),
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(end).expect("end")),
                -12.0,
                CurveShape::HOLD,
            )
            .expect("end knot"),
        ],
    )
    .expect("EQ curve");
    harness
        .with_queue(move |queue| queue.deck_program(program))
        .await
        .expect("curve submission");
}

fn expected(frame: usize, frames: usize, shape: CurveShape) -> (GainDb, [f32; 2]) {
    let time = frame.to_f64().expect("offset") / frames.to_f64().expect("length");
    let db = if frame >= frames {
        -12.0
    } else {
        let mut values = [time];
        shape.process(&mut values, [0.0, -12.0]);
        values[0]
    };
    let gain = GainDb::checked(db.to_f32().expect("bounded dB")).expect("canonical gain");
    (gain, [0.5 * 10.0_f32.powf(f32::from(gain) / 20.0); 2])
}

pub(super) fn bench(c: &mut Criterion) {
    let rt = super::make_runtime();
    let harness = rt.block_on(player());
    {
        let mut group = c.benchmark_group("deck_eq_curves");
        group.sample_size(20);
        for (name, shape) in [
            ("hold", CurveShape::HOLD),
            ("linear", CurveShape::LINEAR),
            ("cubic", CurveShape::cubic(0.0, 1.0).expect("S shape")),
        ] {
            for frames in [13, 256] {
                let oracle: Vec<_> = (0..frames)
                    .flat_map(|frame| expected(frame, frames, shape).1)
                    .collect();
                group.bench_function(format!("render_{name}/{frames}"), |b| {
                    let mut last = Vec::new();
                    b.iter_batched(
                        || {
                            rt.block_on(async {
                                reset(&harness).await;
                                submit(&harness, shape, frames).await;
                                let prelude = harness.render(PRELUDE).await;
                                assert!(prelude.iter().all(|sample| *sample == 0.5));
                            });
                        },
                        |()| {
                            last = rt.block_on(harness.render(frames));
                            black_box(&last);
                        },
                        BatchSize::PerIteration,
                    );
                    assert_eq!(last, oracle);
                    assert_eq!(
                        harness.deck_snapshot().eq.gain(0),
                        Some(expected(frames - 1, frames, shape).0)
                    );
                    assert!(harness.deck_snapshot().eq.program(0).is_some());
                    assert_eq!(harness.metrics().underruns(), 0);
                });
            }
        }
        let shape = CurveShape::cubic(0.0, 1.0).expect("S shape");
        let oracle: Vec<_> = (0..1024_usize)
            .flat_map(|frame| {
                if frame < PRELUDE {
                    [0.5; 2]
                } else {
                    expected(frame - PRELUDE, 128, shape).1
                }
            })
            .collect();
        group.bench_function("public_host_cubic/1024", |b| {
            let mut last = Vec::new();
            b.iter_batched(
                || rt.block_on(reset(&harness)),
                |()| {
                    last = rt.block_on(async {
                        submit(&harness, shape, 128).await;
                        harness.render(1024).await
                    });
                    black_box(&last);
                },
                BatchSize::PerIteration,
            );
            assert_eq!(last, oracle);
            assert_eq!(harness.deck_snapshot().eq.gain(0), GainDb::checked(-12.0));
            assert!(harness.deck_snapshot().eq.program(0).is_some());
            assert_eq!(harness.metrics().underruns(), 0);
        });
        group.finish();
    }
    rt.block_on(harness.close());
}
