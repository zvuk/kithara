#![forbid(unsafe_code)]

use std::num::NonZeroUsize;

use criterion::{BenchmarkId, Criterion, Throughput};
use kithara_dsp::delay::{DelayLine, Tap};
use kithara_test_utils::bufpool::pools;

const DELAYS: [(&str, f64); 3] = [
    ("integer_480", 480.0),
    ("integer_96k", 96_000.0),
    ("fractional_96k", 96_000.5),
];
const CAPACITY: usize = 192_000;
const BLOCK: usize = 256;
const FEEDBACK: f32 = 0.5;

struct Prepared {
    line: DelayLine,
    tap: Tap,
    input: Vec<f32>,
    planes: Vec<Vec<f32>>,
}

fn prepare(frames: f64) -> Prepared {
    let channels = NonZeroUsize::MIN.saturating_add(1);
    let capacity = NonZeroUsize::new(CAPACITY).expect("nonzero capacity");
    let line = DelayLine::new(channels, capacity, &pools()).expect("line fits");
    let tap = line.tap(frames).expect("tap in range");
    let input: Vec<f32> = (0..BLOCK).map(|n| if n == 0 { 1.0 } else { 0.0 }).collect();
    let planes = vec![vec![0.0_f32; BLOCK]; 2];
    Prepared {
        line,
        tap,
        input,
        planes,
    }
}

/// One stereo block of a feedback echo: read the block at the delay, feed
/// `x + g·echo` back, in spans no longer than the delay.
fn process(prepared: &mut Prepared) {
    for (channel, plane) in prepared.planes.iter_mut().enumerate() {
        prepared
            .line
            .read(channel, prepared.tap, plane)
            .expect("span within delay");
        for (echo, x) in plane.iter_mut().zip(&prepared.input) {
            *echo = FEEDBACK.mul_add(*echo, *x);
        }
    }
    prepared
        .line
        .write(std::hint::black_box(&prepared.planes), 0..BLOCK)
        .expect("planes cover the block");
}

#[metabench::benchmark(DELAY_FEEDBACK, "delay", "feedback_block", gungraun_setup = prepare)]
#[bench::integer_480(480.0)]
#[bench::integer_96k(96_000.0)]
#[bench::fractional_96k(96_000.5)]
fn feedback_case(mut prepared: Prepared) -> Prepared {
    process(&mut prepared);
    prepared
}

fn criterion_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("delay");
    group.throughput(Throughput::Elements(
        u64::try_from(2 * BLOCK).expect("block fits u64"),
    ));
    for (id, frames) in DELAYS {
        let mut prepared = prepare(frames);
        group.bench_with_input(BenchmarkId::new("feedback_block", id), &frames, |b, _| {
            b.iter(|| process(&mut prepared));
        });
    }
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [DELAY_FEEDBACK],
    allocator = std::alloc::System,
);
