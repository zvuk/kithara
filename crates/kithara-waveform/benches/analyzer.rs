#![forbid(unsafe_code)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use kithara_test_fixtures::signal::Wave;
use kithara_test_utils::bufpool::pools;
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

/// A full-scale stereo tone, the right channel quieter than the left.
fn excerpt() -> Vec<f32> {
    let wave = Wave::Sine {
        hz: consts::TONE_HZ,
        peak: i16::MAX,
    };
    (0..consts::FRAMES)
        .flat_map(|frame| {
            let sample = f32::from(wave.sample(frame, consts::RATE)) / consts::FULL_SCALE;
            [sample, sample * consts::RIGHT_GAIN]
        })
        .collect()
}

fn waveform_analyzer(c: &mut Criterion) {
    let pcm = excerpt();
    let pools = pools();
    let extent = u64::try_from(consts::FRAMES).expect("the frame count fits u64");
    let mut group = c.benchmark_group("waveform_analyzer");
    group.sample_size(10);
    group.bench_function("push_snapshot/10s_stereo", |b| {
        b.iter(|| {
            let mut analyzer =
                WaveformAnalyzer::new(consts::RATE, AnalysisParams::default(), &pools)
                    .expect("the analyzer builds");
            analyzer
                .push(&pools, black_box(&pcm), consts::CHANNELS, 0)
                .expect("the excerpt fits the region");
            black_box(analyzer.snapshot(consts::FOLD, Some(extent)))
        });
    });
    group.finish();
}

criterion_group!(benches, waveform_analyzer);
criterion_main!(benches);
