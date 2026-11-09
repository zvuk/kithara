use ::kithara::{
    platform::time::{Duration, Instant},
    signal::ExponentialApproach,
};
use kithara_test_utils::{kithara, tracing};
use num_traits::ToPrimitive;

use super::{
    AudioChunk, AudioChunkInfo, SpeedCurve, StretchKind, Warp, WarpConfig, chunk, pools, spec,
};

const QUANTUM: usize = 512;
const UPDATE_FRAMES: usize = 882;
const WARM_FRAMES: usize = 44_100;
const MEASURED_FRAMES: usize = 529_200;
const DECKS: usize = 2;

#[derive(Clone, Copy, Debug)]
enum Motion {
    Constant,
    Approach,
    Retarget,
}

fn source_position(position: (u128, std::num::NonZeroU128)) -> f64 {
    position.0.to_f64().expect("source numerator fits f64")
        / position
            .1
            .get()
            .to_f64()
            .expect("source denominator fits f64")
}

fn summary(durations: &mut [Duration]) -> (f64, f64, f64) {
    durations.sort_unstable();
    let total = durations.iter().map(Duration::as_secs_f64).sum::<f64>();
    let p99 = durations[durations.len() * 99 / 100].as_secs_f64();
    let maximum = durations.last().expect("measured blocks").as_secs_f64();
    (total, p99, maximum)
}

#[kithara::test(native, flash(false), tracing("info"))]
#[case::constant_keylock(Motion::Constant, true)]
#[case::approach_keylock(Motion::Approach, true)]
#[case::retarget_keylock(Motion::Retarget, true)]
#[case::constant_varispeed(Motion::Constant, false)]
#[case::approach_varispeed(Motion::Approach, false)]
#[case::retarget_varispeed(Motion::Retarget, false)]
#[ignore = "opt-in CPU profile; run alone with --ignored --nocapture"]
fn tempo_worker_profile(#[case] motion: Motion, #[case] keylock: bool) {
    let config = WarpConfig::builder()
        .backend(StretchKind::Signalsmith)
        .keylock(keylock)
        .speed(1.05)
        .build();
    let pools: [_; DECKS] = std::array::from_fn(|_| pools());
    let mut renderers: [_; DECKS] =
        std::array::from_fn(|deck| Warp::new((), &config).renderer(spec(), pools[deck].clone()));
    let tone = kithara_test_fixtures::unit_fixtures::warp_sine();
    let source_frames = (WARM_FRAMES + MEASURED_FRAMES) * 2 + 65_536;
    let samples: Vec<_> = tone
        .iter()
        .copied()
        .cycle()
        .take(source_frames * 2)
        .collect();
    let mut blocks = Vec::with_capacity(32_768);
    let mut service = Vec::with_capacity(32_768);
    let mut render = Vec::with_capacity(32_768);
    let mut source = [0_usize; DECKS];
    let mut output_frames = 0;
    let mut next_update = UPDATE_FRAMES;
    let mut revision = 0;
    let mut previous_end: [Option<f64>; DECKS] = [None; DECKS];
    let mut energy = [0.0_f64; DECKS];
    let mut measured = 0;
    let mut over_budget_blocks = 0;
    let mut moving_rate = 1.05;
    while output_frames < WARM_FRAMES + MEASURED_FRAMES {
        let begin = Instant::now();
        if output_frames == next_update {
            revision += 1;
            let target = if revision % 2 == 0 { 1.05 } else { 1.10 };
            let curve = match motion {
                Motion::Constant => None,
                Motion::Approach => {
                    let approach = ExponentialApproach::new(moving_rate, target, 2_205.0)
                        .expect("finite tempo slew");
                    moving_rate = approach
                        .rate_at(UPDATE_FRAMES.to_f64().expect("update interval fits f64"))
                        .expect("next tempo rate");
                    Some(SpeedCurve::Approach(approach))
                }
                Motion::Retarget => Some(SpeedCurve::Constant(
                    target.to_f32().expect("target fits f32"),
                )),
            };
            if let Some(curve) = curve {
                for renderer in &mut renderers {
                    renderer
                        .set_speed(curve.clone(), revision)
                        .expect("supported shared tempo curve");
                }
            }
            next_update += UPDATE_FRAMES;
        }
        let mut prepared = begin.elapsed();
        let budget = QUANTUM
            .min(next_update - output_frames)
            .min(WARM_FRAMES + MEASURED_FRAMES - output_frames)
            .min(if output_frames < WARM_FRAMES {
                WARM_FRAMES - output_frames
            } else {
                QUANTUM
            });
        let mut inputs = [None, None];
        for (deck, renderer) in renderers.iter_mut().enumerate() {
            let service_begin = Instant::now();
            renderer
                .prepare_engine_latency(spec())
                .expect("prepared tempo engine");
            let meta = AudioChunkInfo {
                spec: spec(),
                frame_offset: u64::try_from(source[deck]).expect("source frame count fits u64"),
                visible_origin: Some(Duration::ZERO),
                source_revision: 7,
                timestamp: Duration::from_secs_f64(
                    source[deck].to_f64().expect("source frame count fits f64") / 44_100.0,
                ),
                ..AudioChunkInfo::default()
            };
            let available = (source_frames - source[deck]).min(65_536);
            let frames = renderer
                .prepare_source_window(meta, available, budget)
                .expect("bounded tempo quantum")
                .get();
            prepared += service_begin.elapsed();
            let mut input = chunk(
                &pools[deck],
                spec(),
                &samples[source[deck] * 2..(source[deck] + frames) * 2],
                meta.frame_offset,
            );
            input.meta = AudioChunkInfo {
                frames: u32::try_from(frames).expect("source quantum fits u32"),
                ..meta
            };
            source[deck] += frames;
            inputs[deck] = Some(input);
        }
        let render_begin = Instant::now();
        let outputs: [AudioChunk; DECKS] = std::array::from_fn(|deck| {
            renderers[deck]
                .render_quantum(inputs[deck].take().expect("prepared deck source"))
                .continue_value()
                .expect("prepared tempo input")
                .expect("tempo output")
        });
        let rendered = render_begin.elapsed();
        let elapsed = begin.elapsed();
        let frames = outputs[0].frames();
        for (deck, output) in outputs.iter().enumerate() {
            assert_eq!(output.frames(), frames, "decks share one output deadline");
            assert!(output.frames() > 0 && output.frames() <= budget);
            let span = output.meta.source_span.expect("published source mapping");
            let first = source_position(span.source_ratio_at(0).expect("source start"));
            if let Some(previous) = previous_end[deck] {
                assert!((first - previous).abs() < 1e-6, "continuous source mapping");
            }
            previous_end[deck] = Some(source_position(
                span.source_ratio_at(span.output_frames())
                    .expect("source end"),
            ));
            assert!(output.samples.iter().all(|sample| sample.is_finite()));
            if output_frames >= WARM_FRAMES {
                energy[deck] += output
                    .samples
                    .iter()
                    .map(|sample| f64::from(*sample).powi(2))
                    .sum::<f64>();
            }
        }
        if output_frames >= WARM_FRAMES {
            service.push(prepared);
            render.push(rendered);
            blocks.push(elapsed);
            measured += frames;
            let audio_seconds = frames.to_f64().expect("output frames fit f64") / 44_100.0;
            over_budget_blocks += usize::from(elapsed.as_secs_f64() > audio_seconds);
        }
        output_frames += frames;
        std::hint::black_box(outputs);
    }
    assert_eq!(measured, MEASURED_FRAMES);
    for energy in energy {
        assert!(energy / (measured * 2).to_f64().expect("sample count fits f64") > 0.01);
    }
    let worker = summary(&mut blocks);
    let service = summary(&mut service);
    let render = summary(&mut render);
    tracing::info!(
        ?motion,
        keylock,
        decks = DECKS,
        audio_seconds = 12.0,
        measured_blocks = blocks.len(),
        over_budget_blocks,
        worker_total_seconds = worker.0,
        worker_p99_seconds = worker.1,
        worker_max_seconds = worker.2,
        service_total_seconds = service.0,
        service_p99_seconds = service.1,
        service_max_seconds = service.2,
        render_total_seconds = render.0,
        render_p99_seconds = render.1,
        render_max_seconds = render.2,
        "tempo worker profile"
    );
}
