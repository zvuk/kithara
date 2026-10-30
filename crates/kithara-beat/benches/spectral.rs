#![forbid(unsafe_code)]

use std::{hint::black_box, path::Path};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use kithara_beat::{SpectralBeats, Tempo};
use kithara_test_utils::{
    bufpool::pools,
    kithara_platform::time::{Duration, Instant},
};

mod consts {
    /// The rate the crate contract fixes.
    pub(super) const RATE: usize = 22_050;
    /// Length of the excerpt one analysis runs over.
    pub(super) const SECONDS: usize = 30;
}

fn excerpt() -> Vec<f32> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/track_excerpt_mono_22050.f32le");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let (words, remainder) = bytes.as_chunks::<4>();
    assert!(
        remainder.is_empty(),
        "the fixture contains complete f32 samples"
    );
    assert!(
        words.len() >= consts::RATE * consts::SECONDS,
        "the full excerpt is present"
    );
    words
        .iter()
        .map(|word| f32::from_le_bytes(*word))
        .take(consts::RATE * consts::SECONDS)
        .collect()
}

fn spectral_beats(c: &mut Criterion) {
    let pcm = excerpt();
    let detector = SpectralBeats::new(pools(), Tempo::default()).expect("the detector builds");
    let expected = detector.analyze(&pcm).expect("the full analysis succeeds");
    assert!(!expected.beats.is_empty(), "the excerpt contains beats");
    assert!(
        expected.downbeats.is_empty(),
        "the spectral tracker reports no downbeats"
    );
    assert!(
        expected
            .beats
            .iter()
            .all(|mark| mark.at.is_finite() && mark.confidence.is_finite())
    );
    let silence = vec![0.0; pcm.len()];
    assert!(
        detector
            .analyze(&silence)
            .expect("silence analyzes")
            .beats
            .is_empty()
    );
    assert_eq!(
        detector.analyze(&pcm).expect("a reused detector analyzes"),
        expected
    );
    let mut group = c.benchmark_group("spectral_beats");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(1));
    for (case, input) in [
        (
            "1024_samples",
            pcm.get(..1024).expect("the excerpt holds a short frame"),
        ),
        (
            "1537_samples",
            pcm.get(..1537).expect("the excerpt holds a padded frame"),
        ),
        ("30s", pcm.as_slice()),
    ] {
        let expected = detector.analyze(input).expect("the analysis succeeds");
        let mut calls = Vec::new();
        group.throughput(Throughput::Elements(
            u64::try_from(input.len()).expect("frame count fits u64"),
        ));
        group.bench_function(format!("analyze/{case}"), |b| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                let mut samples = Vec::with_capacity(
                    usize::try_from(iterations).expect("iteration count fits usize"),
                );
                for _ in 0..iterations {
                    let start = Instant::now();
                    let result = black_box(detector.analyze(black_box(input)));
                    let duration = start.elapsed();
                    elapsed += duration;
                    samples.push(duration.as_nanos());
                    assert_eq!(result.expect("the measured analysis succeeds"), expected);
                }
                calls.push(samples);
                elapsed
            });
        });
        // Batch averages cannot give tail latency; retain individual calls outside elapsed.
        eprintln!("spectral_beats_consumer_calls_ns/{case}={calls:?}");
        let marks: Vec<_> = expected
            .beats
            .iter()
            .map(|mark| [mark.at.to_bits(), mark.confidence.to_bits()])
            .collect();
        eprintln!("spectral_beats_consumer_marks_bits/{case}={marks:?}");
    }
    group.finish();
}

criterion_group!(benches, spectral_beats);
criterion_main!(benches);
