#![forbid(unsafe_code)]

use std::{hint::black_box, num::NonZeroUsize};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kithara_dsp::delay::DelayLine;
use kithara_test_utils::bufpool::pools;

mod consts {
    /// Short slap and long throw on the integer path, and the long throw on
    /// the fractional chord path, at 48 kHz in frames.
    pub(super) const DELAYS: [(&str, f64); 3] = [
        ("integer", 480.0),
        ("integer", 96_000.0),
        ("fractional", 96_000.5),
    ];
    pub(super) const CAPACITY: usize = 192_000;
    pub(super) const BLOCK: usize = 256;
    pub(super) const FEEDBACK: f32 = 0.5;
}

/// One stereo block of a feedback echo: read the block at the delay, feed
/// `x + g·echo` back, in spans no longer than the delay.
fn feedback(c: &mut Criterion) {
    let mut group = c.benchmark_group("delay");
    group.throughput(Throughput::Elements(
        u64::try_from(2 * consts::BLOCK).expect("block fits u64"),
    ));
    let channels = NonZeroUsize::MIN.saturating_add(1);
    let capacity = NonZeroUsize::new(consts::CAPACITY).expect("nonzero capacity");
    let region = pools();
    for (path, frames) in consts::DELAYS {
        let mut line = DelayLine::new(channels, capacity, &region).expect("line fits");
        let tap = line.tap(frames).expect("tap in range");
        let input: Vec<f32> = (0..consts::BLOCK)
            .map(|n| if n == 0 { 1.0 } else { 0.0 })
            .collect();
        let mut planes = vec![vec![0.0_f32; consts::BLOCK]; 2];
        group.bench_with_input(
            BenchmarkId::new(format!("feedback_block/{path}"), frames),
            &frames,
            |b, _| {
                b.iter(|| {
                    for (channel, plane) in planes.iter_mut().enumerate() {
                        line.read(channel, tap, plane).expect("span within delay");
                        for (echo, x) in plane.iter_mut().zip(&input) {
                            *echo = consts::FEEDBACK.mul_add(*echo, *x);
                        }
                    }
                    line.write(black_box(&planes), 0..consts::BLOCK)
                        .expect("planes cover the block");
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, feedback);
criterion_main!(benches);
