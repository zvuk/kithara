use std::hint::black_box;

use criterion::{BatchSize, Criterion};
use kithara::{
    effects::filter::{
        Cutoff, FilterConfig, FilterEffect, FilterSettings, FilterSettingsChange, Kind,
    },
    play::{CurveShape, DeckFilterChange, FilterParameter, FilterProgram, ParameterKnot},
    signal::{AudioSpec, SessionFrame},
};
use kithara_command::When;
use kithara_config::ConfigOwner;
use kithara_integration_tests::offline::OfflinePlayer;
use num_traits::ToPrimitive;

const PRELUDE: usize = 529;
const FILTER_WARM: usize = 64;

fn settings() -> FilterSettings {
    FilterSettings::builder()
        .kind(Kind::LowPass)
        .cutoff_hz(Cutoff::checked(1_000.0).expect("cutoff"))
        .build()
        .expect("settings")
}

async fn prepare(harness: &OfflinePlayer) {
    harness
        .with_queue(|queue| queue.filter(When::Next, DeckFilterChange::Clear))
        .await
        .expect("clear previous state and programs");
    harness.seek(0.0).await;
    let warm = harness.render(4096).await;
    assert!(warm[warm.len() - 128..].iter().all(|sample| *sample == 0.5));
    let install = harness.host().position() + u64::try_from(PRELUDE).expect("prelude");
    harness
        .with_queue(move |queue| {
            queue.install_filter(
                When::At(SessionFrame::new(
                    i64::try_from(install).expect("install frame"),
                )),
                settings(),
            )
        })
        .await
        .expect("cold filter install");
    let installed = harness.render(PRELUDE + FILTER_WARM).await;
    assert!(installed[..PRELUDE * 2].iter().all(|sample| *sample == 0.5));
    assert_eq!(harness.deck_snapshot().filter.settings(), Some(settings()));
    assert_eq!(harness.metrics().underruns(), 0);
}

async fn submit(harness: &OfflinePlayer, shape: CurveShape, frames: usize) {
    let start = harness.host().position() + u64::try_from(PRELUDE).expect("prelude");
    let end = start + u64::try_from(frames).expect("curve length");
    let program = FilterProgram::new(
        FilterParameter::CutoffHz,
        vec![
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(start).expect("start")),
                200.0,
                shape,
            )
            .expect("left"),
            ParameterKnot::new(
                SessionFrame::new(i64::try_from(end).expect("end")),
                800.0,
                CurveShape::HOLD,
            )
            .expect("right"),
        ],
    )
    .expect("prepared cutoff trajectory");
    harness
        .with_queue(move |queue| queue.filter_program(program))
        .await
        .expect("public curve submission");
}

fn cutoff(frame: usize, frames: usize, shape: CurveShape) -> Cutoff {
    let value = if frame >= frames {
        800.0
    } else {
        200.0
            * 4.0_f64.powf(
                shape.progress(frame.to_f64().expect("offset") / frames.to_f64().expect("length")),
            )
    };
    Cutoff::checked(value).expect("bounded positive cutoff")
}

async fn oracle(
    harness: &OfflinePlayer,
    shape: CurveShape,
    frames: usize,
    prelude: usize,
) -> (Vec<f32>, Cutoff) {
    let rate = harness.host().spec().await.sample_rate;
    let mut filter = FilterEffect::new(
        AudioSpec::new(2, rate),
        FilterConfig::from((harness.worker().pools().clone(), settings())),
    )
    .expect("reference state");
    let warm = FILTER_WARM + if prelude == 0 { PRELUDE } else { 0 };
    let take = if prelude == 0 { frames } else { 1024 };
    let mut expected = Vec::with_capacity(take * 2);
    for frame in 0..warm + take {
        let output = frame.checked_sub(warm);
        if let Some(offset) = output.and_then(|offset| offset.checked_sub(prelude)) {
            filter
                .set_explicit(FilterSettingsChange::CutoffHz(cutoff(
                    offset, frames, shape,
                )))
                .expect("reference point without smoothing");
        }
        let (mut left, mut right) = ([0.5], [0.5]);
        filter
            .process(&mut [&mut left[..], &mut right[..]], 0..1)
            .expect("reference frame");
        if output.is_some() {
            expected.extend([left[0], right[0]]);
        }
    }
    (expected, filter.config().cutoff_hz())
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
                            })
                        },
                        |()| {
                            last = rt.block_on(harness.render(frames));
                            black_box(&last);
                        },
                        BatchSize::PerIteration,
                    );
                    assert_eq!(last, expected);
                    assert_eq!(
                        harness
                            .deck_snapshot()
                            .filter
                            .settings()
                            .expect("settings")
                            .cutoff_hz(),
                        target
                    );
                    assert!(
                        harness
                            .deck_snapshot()
                            .filter
                            .program(FilterParameter::CutoffHz)
                            .is_some()
                    );
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
            assert_eq!(
                harness
                    .deck_snapshot()
                    .filter
                    .settings()
                    .expect("settings")
                    .cutoff_hz(),
                target
            );
            assert!(
                harness
                    .deck_snapshot()
                    .filter
                    .program(FilterParameter::CutoffHz)
                    .is_some()
            );
            assert_eq!(harness.metrics().underruns(), 0);
        });
        group.finish();
    }
    rt.block_on(harness.close());
}
