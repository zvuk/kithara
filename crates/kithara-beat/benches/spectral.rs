#![forbid(unsafe_code)]

use std::path::Path;

use criterion::{Criterion, Throughput};
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

struct Prepared {
    detector: SpectralBeats<kithara_test_utils::bufpool::TestPools>,
    pcm: Vec<f32>,
    case: usize,
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

fn prepare(case: usize) -> Prepared {
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
    Prepared {
        detector,
        pcm,
        case,
    }
}

fn input(prepared: &Prepared) -> &[f32] {
    match prepared.case {
        0 => prepared.pcm.get(..1024).expect("short frame"),
        1 => prepared.pcm.get(..1537).expect("padded frame"),
        2 => prepared.pcm.as_slice(),
        other => panic!("unknown spectral case {other}"),
    }
}

fn analyze(prepared: &Prepared) {
    let input = input(prepared);
    let expected = prepared
        .detector
        .analyze(input)
        .expect("the analysis succeeds");
    let result = prepared.detector.analyze(std::hint::black_box(input));
    assert_eq!(result.expect("the measured analysis succeeds"), expected);
}

#[metabench::benchmark(SPECTRAL_ANALYZE, "spectral_beats", "analyze", gungraun_setup = prepare)]
#[bench::samples_1024(0_usize)]
#[bench::samples_1537(1_usize)]
#[bench::seconds_30(2_usize)]
fn spectral_case(prepared: Prepared) -> Prepared {
    analyze(&prepared);
    prepared
}

fn criterion_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("spectral_beats");
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(1));
    for (case, id) in [("samples_1024", 0), ("samples_1537", 1), ("seconds_30", 2)] {
        let case_prepared = prepare(id);
        let input = input(&case_prepared);
        let expected = case_prepared
            .detector
            .analyze(input)
            .expect("the analysis succeeds");
        group.throughput(Throughput::Elements(
            u64::try_from(input.len()).expect("frame count fits u64"),
        ));
        group.bench_function(format!("analyze/{case}"), |b| {
            b.iter_custom(|iterations| {
                let mut elapsed = Duration::ZERO;
                for _ in 0..iterations {
                    let start = Instant::now();
                    let result = std::hint::black_box(
                        case_prepared.detector.analyze(std::hint::black_box(input)),
                    );
                    elapsed += start.elapsed();
                    assert_eq!(result.expect("the measured analysis succeeds"), expected);
                }
                elapsed
            });
        });
    }
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [SPECTRAL_ANALYZE],
    allocator = std::alloc::System,
);
