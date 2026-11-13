#![forbid(unsafe_code)]

use std::{
    hint::black_box,
    num::{NonZeroU32, NonZeroUsize},
};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use kithara_effects::{
    DeckEffect,
    delay::{DelayTime, Feedback, FeedbackDelay, FeedbackDelayConfig, FeedbackHighPass},
};
use kithara_test_utils::bufpool::pools;

fn feedback_comb(c: &mut Criterion) {
    let mut group = c.benchmark_group("feedback_comb");
    group.throughput(Throughput::Elements(512));
    let region = pools();
    for (path, frames) in [
        ("integer", 480.0),
        ("integer", 96_000.0),
        ("fractional", 96_000.5),
    ] {
        let config = FeedbackDelayConfig::builder()
            .sample_rate(NonZeroU32::new(48_000).expect("rate"))
            .capacity(NonZeroUsize::new(192_000).expect("capacity"))
            .time(DelayTime::checked(frames).expect("delay"))
            .feedback(Feedback::checked(0.5).expect("gain"))
            .high_pass(FeedbackHighPass::checked(1000.0).expect("cutoff"))
            .build();
        let mut delay = FeedbackDelay::new(config, &region).expect("delay fits");
        let mut input = [vec![0.0_f32; 256], vec![0.0; 256]];
        input[0][0] = 1.0;
        input[1][0] = -0.5;
        let mut wet = input.clone();
        group.bench_function(BenchmarkId::new(format!("high_pass/{path}"), frames), |b| {
            b.iter(|| {
                let [left, right] = &mut wet;
                delay.process(
                    black_box([input[0].as_slice(), input[1].as_slice()]),
                    black_box([left.as_mut_slice(), right.as_mut_slice()]),
                );
            });
        });
    }
    group.finish();
}

criterion_group!(benches, feedback_comb);
criterion_main!(benches);
