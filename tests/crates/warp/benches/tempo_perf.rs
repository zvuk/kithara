#![forbid(unsafe_code)]

use std::{fs::OpenOptions, io::Write, num::NonZeroU32};

use criterion::{BenchmarkId, Criterion};
use kithara::{
    platform::time::{Duration, Instant},
    signal::{AudioChunk, AudioChunkInfo, AudioSpec, ExponentialApproach},
    stretch::StretchKind,
    sync::SpeedCurve,
    warp::{Pitch, Warp, WarpConfig, WarpControls},
};
use kithara_integration_tests::bufpool_ext::{Pools, pools, sample_buffer};
use num_traits::ToPrimitive;

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

fn spec() -> AudioSpec {
    AudioSpec::new(2, NonZeroU32::new(44_100).expect("tempo sample rate"))
}

fn chunk(pools: &Pools, spec: AudioSpec, samples: &[f32], offset: u64) -> AudioChunk {
    AudioChunk::new(
        AudioChunkInfo {
            spec,
            frame_offset: offset,
            frames: u32::try_from(samples.len() / 2).expect("chunk frames"),
            ..AudioChunkInfo::default()
        },
        sample_buffer(pools, samples),
    )
}

fn source_position(position: (u128, std::num::NonZeroU128)) -> f64 {
    position.0.to_f64().expect("source numerator fits f64")
        / position
            .1
            .get()
            .to_f64()
            .expect("source denominator fits f64")
}

fn total(durations: &[Duration]) -> f64 {
    durations.iter().map(Duration::as_secs_f64).sum()
}

fn observe(identity: &str, worker: f64, service: f64, render: f64) {
    let Some(path) = std::env::var_os("KITHARA_BENCH_OBSERVATIONS") else {
        return;
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open tempo observations");
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "identity": format!("tempo_worker/case/{identity}"),
            "observation": {
                "tempo": {
                    "worker_seconds": worker,
                    "service_seconds": service,
                    "render_seconds": render,
                    "decks": DECKS,
                    "measured_frames": MEASURED_FRAMES,
                }
            }
        }),
    )
    .expect("write tempo observation");
    file.write_all(b"\n").expect("terminate tempo observation");
}

fn execute(motion: Motion, keylock: bool, identity: &'static str) {
    let config = WarpConfig::builder()
        .backend(StretchKind::Signalsmith)
        .maybe_controls(
            (keylock.then_some(Pitch::HELD))
                .map(|pitch| WarpControls::builder().pitch(pitch).build()),
        )
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
        }
        output_frames += frames;
        std::hint::black_box(outputs);
    }
    assert_eq!(measured, MEASURED_FRAMES);
    for energy in energy {
        assert!(energy / (measured * 2).to_f64().expect("sample count fits f64") > 0.01);
    }
    observe(identity, total(&blocks), total(&service), total(&render));
}

#[metabench::benchmark(TEMPO_WORKER, "tempo_worker", "case")]
#[bench::constant_keylock(0_usize)]
#[bench::approach_keylock(1_usize)]
#[bench::retarget_keylock(2_usize)]
#[bench::constant_varispeed(3_usize)]
#[bench::approach_varispeed(4_usize)]
#[bench::retarget_varispeed(5_usize)]
fn native_tempo(index: usize) {
    let (motion, keylock, identity) = case(index);
    execute(motion, keylock, identity);
}

fn case(index: usize) -> (Motion, bool, &'static str) {
    [
        (Motion::Constant, true, "constant_keylock"),
        (Motion::Approach, true, "approach_keylock"),
        (Motion::Retarget, true, "retarget_keylock"),
        (Motion::Constant, false, "constant_varispeed"),
        (Motion::Approach, false, "approach_varispeed"),
        (Motion::Retarget, false, "retarget_varispeed"),
    ][index]
}

fn criterion_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("tempo_worker");
    group.sample_size(10);
    for (index, id) in [
        "constant_keylock",
        "approach_keylock",
        "retarget_keylock",
        "constant_varispeed",
        "approach_varispeed",
        "retarget_varispeed",
    ]
    .into_iter()
    .enumerate()
    {
        group.bench_function(BenchmarkId::new("case", id), |b| {
            let (motion, keylock, identity) = case(index);
            b.iter(|| execute(motion, keylock, identity));
        });
    }
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [TEMPO_WORKER],
    allocator = std::alloc::System,
);
