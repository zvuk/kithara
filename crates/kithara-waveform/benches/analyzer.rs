#![forbid(unsafe_code)]

use criterion::Criterion;
use kithara_test_fixtures::signal::Wave;
use kithara_test_utils::bufpool::{Pools, pools};
use kithara_waveform::{AnalysisParams, WaveformAnalyzer};

mod consts {
    use std::num::NonZeroU32;
    /// Native analysis windows per bucket.
    pub(super) const FOLD: NonZeroU32 = NonZeroU32::MIN;
    /// Interleaved channels of the excerpt.
    pub(super) const CHANNELS: usize = 2;
    /// Frames in the excerpt: 10 s at [`RATE`].
    pub(super) const FRAMES: usize = 441_000;
    /// Full scale of a 16-bit sample.
    pub(super) const FULL_SCALE: f32 = 32_768.0;
    /// Rate the excerpt is rendered at.
    pub(super) const RATE: u32 = 44_100;
    /// Gain of the right channel against the left.
    pub(super) const RIGHT_GAIN: f32 = 0.5;
    /// Frequency of the tone.
    pub(super) const TONE_HZ: f64 = 440.0;
}

struct Prepared {
    pcm: Vec<f32>,
    pools: Pools,
}

/// A full-scale stereo tone, the right channel quieter than the left.
fn prepare() -> Prepared {
    let wave = Wave::Sine {
        hz: consts::TONE_HZ,
        peak: i16::MAX,
    };
    let pcm = (0..consts::FRAMES)
        .flat_map(|frame| {
            let sample = f32::from(wave.sample(frame, consts::RATE)) / consts::FULL_SCALE;
            [sample, sample * consts::RIGHT_GAIN]
        })
        .collect();
    Prepared {
        pcm,
        pools: pools(),
    }
}

fn analyze(prepared: &Prepared) {
    let extent = u64::try_from(consts::FRAMES).expect("the frame count fits u64");
    let mut analyzer =
        WaveformAnalyzer::new(consts::RATE, AnalysisParams::default(), &prepared.pools)
            .expect("the analyzer builds");
    analyzer
        .push(
            &prepared.pools,
            std::hint::black_box(&prepared.pcm),
            consts::CHANNELS,
            0,
        )
        .expect("the excerpt fits the region");
    std::hint::black_box(analyzer.snapshot(consts::FOLD, Some(extent)));
}

#[metabench::benchmark(WAVEFORM_ANALYZER, "waveform_analyzer", "push_snapshot_10s_stereo", gungraun_setup = prepare)]
fn waveform_case(prepared: Prepared) -> Prepared {
    analyze(&prepared);
    prepared
}

fn criterion_benchmarks(c: &mut Criterion) {
    let prepared = prepare();
    let mut group = c.benchmark_group("waveform_analyzer");
    group.sample_size(10);
    group.bench_function("push_snapshot_10s_stereo", |b| {
        b.iter(|| analyze(&prepared));
    });
    group.finish();
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [WAVEFORM_ANALYZER],
    allocator = std::alloc::System,
);
