#![forbid(unsafe_code)]

use std::{fs, hint::black_box, num::NonZeroU32};

use criterion::{BatchSize, Criterion, SamplingMode, criterion_group, criterion_main};
use kithara::{
    StretchKind, TimeStretchProcessor,
    audio::{
        AnalysisParams, Audio, AudioConfig, AudioEffect, ReadOutcome, StretchControls,
        WaveformAnalyzer,
    },
    bufpool::{BytePool, PcmPool},
    decode::{PcmChunk, PcmMeta, PcmSpec},
    file::{File, FileConfig},
    platform::{
        sync::Arc,
        time::Duration,
        tokio::runtime::{Builder, Runtime},
    },
    stream::Stream,
};
use tempfile::TempDir;

struct Consts;

impl Consts {
    const TEST_MP3_BYTES: &'static [u8] = include_bytes!("../../assets/test.mp3");
    const CHANNELS: usize = 2;
    const SAMPLE_RATE: u32 = 44_100;
    const ANALYSIS_SECONDS: usize = 30;
    const STRETCH_FRAMES: usize = 8_192;
}

fn make_runtime() -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| panic!("failed to build tokio runtime: {e}"))
}

fn make_pcm(frames: usize) -> Vec<f32> {
    let mut state = 0x5eed_1234_u32;
    let mut pcm = Vec::with_capacity(frames * Consts::CHANNELS);
    for frame in 0..frames {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (state >> 8) as f32 / 16_777_215.0 - 0.5;
        let phase = frame as f32 * 440.0 * std::f32::consts::TAU / Consts::SAMPLE_RATE as f32;
        let sample = phase.sin() * 0.4 + noise * 0.1;
        pcm.push(sample);
        pcm.push(sample * 0.8);
    }
    pcm
}

fn make_chunk(pool: &PcmPool, pcm: &[f32]) -> PcmChunk {
    let spec = PcmSpec::new(
        u16::try_from(Consts::CHANNELS).unwrap_or_else(|_| panic!("bench channels")),
        NonZeroU32::new(Consts::SAMPLE_RATE).unwrap_or_else(|| panic!("bench sample rate")),
    );
    let mut samples = pool.get();
    samples
        .ensure_len(pcm.len())
        .unwrap_or_else(|error| panic!("bench PCM allocation failed: {error}"));
    samples.clone_from_slice(pcm);
    PcmChunk::new(
        PcmMeta {
            spec,
            frames: u32::try_from(pcm.len() / Consts::CHANNELS)
                .unwrap_or_else(|_| panic!("bench frame count")),
            ..PcmMeta::default()
        },
        samples,
    )
}

fn bench_gapless_trim(c: &mut Criterion) {
    let rt = make_runtime();
    let temp_dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir failed: {e}"));
    let file_path = temp_dir.path().join("audit-gapless.mp3");
    fs::write(&file_path, Consts::TEST_MP3_BYTES)
        .unwrap_or_else(|e| panic!("failed to write bench mp3: {e}"));

    let mut group = c.benchmark_group("audit_gapless_trim");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));

    group.bench_function("decode_mp3_to_eof", |b| {
        b.iter(|| {
            rt.block_on(async {
                let config =
                    AudioConfig::<File>::for_stream(FileConfig::new(file_path.clone().into()))
                        .byte_pool(BytePool::default())
                        .pcm_pool(PcmPool::default())
                        .hint("mp3".to_string())
                        .build();
                let mut audio = Audio::<Stream<File>>::new(config)
                    .await
                    .unwrap_or_else(|e| panic!("audio init failed: {e}"));
                let mut buf = [0.0_f32; 8_192];
                let mut total = 0_usize;
                loop {
                    match audio.read(&mut buf) {
                        Ok(ReadOutcome::Frames { count, .. }) => total += count.get(),
                        Ok(ReadOutcome::Pending { .. }) => continue,
                        Ok(ReadOutcome::Eof { .. }) => break,
                        Err(e) => panic!("audio read failed: {e}"),
                    }
                }
                black_box(total);
            });
        });
    });

    group.finish();
}

fn bench_beat_analysis(c: &mut Criterion) {
    let frames = Consts::SAMPLE_RATE as usize * Consts::ANALYSIS_SECONDS;
    let pcm = make_pcm(frames);

    let mut group = c.benchmark_group("audit_beat_analysis");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(20);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));

    group.bench_function("waveform_30s_stereo", |b| {
        b.iter(|| {
            let mut analyzer =
                WaveformAnalyzer::new(Consts::SAMPLE_RATE, AnalysisParams::default());
            analyzer.push_interleaved(black_box(&pcm), Consts::CHANNELS);
            black_box(analyzer.finalize(512));
        });
    });

    group.finish();
}

fn bench_stretch_process(c: &mut Criterion) {
    let pcm = make_pcm(Consts::STRETCH_FRAMES);
    let pool = PcmPool::default();
    let spec = PcmSpec::new(
        u16::try_from(Consts::CHANNELS).unwrap_or_else(|_| panic!("bench channels")),
        NonZeroU32::new(Consts::SAMPLE_RATE).unwrap_or_else(|| panic!("bench sample rate")),
    );
    let controls = StretchControls::new(0.8);
    controls.set_backend(StretchKind::Bungee);
    controls.set_keylock(true);

    let mut group = c.benchmark_group("audit_stretch_process");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(4));

    group.bench_function("bungee_ratio_1_25", |b| {
        b.iter_batched(
            || {
                let processor =
                    TimeStretchProcessor::new(Arc::clone(&controls), spec, pool.clone());
                let chunk = make_chunk(&pool, &pcm);
                (processor, chunk)
            },
            |(mut processor, chunk): (TimeStretchProcessor, PcmChunk)| {
                black_box(processor.process(chunk));
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_gapless_trim,
    bench_beat_analysis,
    bench_stretch_process
);
criterion_main!(benches);
