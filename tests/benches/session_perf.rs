#![forbid(unsafe_code)]

use std::{
    io::Cursor,
    num::{NonZeroU32, NonZeroUsize},
    thread,
};

use criterion::{BatchSize, BenchmarkId, Criterion};
use kithara::{
    abr::{
        AbrController, AbrMode, AbrSettings, AbrState, AbrView, Estimator, ThroughputEstimator,
        VariantDuration, VariantIndex, VariantInfo,
    },
    decode::{Decoder, DecoderConfig, DecoderFactory},
    platform::{
        CancelToken,
        time::{Duration, Instant},
    },
    resampler::{
        NoResamplerBackend, Resampler, ResamplerConfig, ResamplerMode, ResamplerOptions,
        ResamplerQuality, ResamplerSettings, create_resampler, rubato::RubatoBackend,
    },
    signal::AudioChunk,
    storage::{MmapOptions, MmapResource, OpenMode, Resource, ResourceRead},
};
use kithara_integration_tests::bufpool_ext::{Pools, TestPools, pools};
use kithara_test_fixtures::integration_fixtures::{
    audio_wav_44100, perf_interleaved, perf_planar, perf_wav,
};
use tempfile::NamedTempFile;

struct Work {
    action: Box<dyn FnMut()>,
}

impl Work {
    fn new(action: impl FnMut() + 'static) -> Self {
        Self {
            action: Box::new(action),
        }
    }

    fn run(&mut self) {
        (self.action)();
    }
}

fn decoder_config() -> DecoderConfig<NoResamplerBackend, TestPools> {
    DecoderConfig::builder().pools(pools()).build()
}

fn create_wav_decoder(wav_data: &'static [u8]) -> Box<dyn Decoder> {
    DecoderFactory::create_with_probe(Cursor::new(wav_data), Some("wav"), decoder_config())
        .expect("WAV decoder")
}

fn prepare_decoder(index: usize) -> Work {
    if index == 0 {
        let mut decoder = create_wav_decoder(audio_wav_44100());
        return Work::new(move || {
            let mut chunks = 0;
            while let Ok(outcome) = decoder.next_chunk() {
                let Ok(chunk) = AudioChunk::try_from(outcome) else {
                    break;
                };
                std::hint::black_box(chunk);
                chunks += 1;
            }
            assert!(chunks > 0);
        });
    }
    if index == 1 {
        return Work::new(|| {
            for _ in 0..10 {
                let cursor = Cursor::new(audio_wav_44100().to_vec());
                let decoder =
                    DecoderFactory::create_with_probe(cursor, Some("wav"), decoder_config())
                        .expect("probe decoder");
                std::hint::black_box(decoder);
            }
        });
    }
    if index == 2 {
        let mut decoder = create_wav_decoder(audio_wav_44100());
        return Work::new(move || {
            let mut samples = 0;
            while let Ok(outcome) = decoder.next_chunk() {
                let Ok(chunk) = AudioChunk::try_from(outcome) else {
                    break;
                };
                samples += chunk.samples.len();
            }
            assert!(samples > 0);
        });
    }
    let mut decoder = create_wav_decoder(perf_wav());
    Work::new(move || {
        let mut samples = 0;
        while let Ok(outcome) = decoder.next_chunk() {
            let Ok(chunk) = AudioChunk::try_from(outcome) else {
                break;
            };
            samples += chunk.samples.len();
        }
        assert!(samples > 0);
    })
}

#[metabench::benchmark(SESSION_DECODER, "session_decoder", "case", gungraun_setup = prepare_decoder)]
#[bench::decoder_wav(0_usize)]
#[bench::decoder_probe(1_usize)]
#[bench::decoder_f32_conversion(2_usize)]
#[bench::decoder_throughput(3_usize)]
fn native_decoder(mut work: Work) -> Work {
    work.run();
    work
}

fn variants() -> Vec<VariantInfo> {
    [500_000, 1_000_000, 2_000_000, 5_000_000]
        .into_iter()
        .enumerate()
        .map(|(index, bandwidth)| VariantInfo {
            variant_index: VariantIndex::new(index),
            bandwidth_bps: Some(bandwidth),
            duration: VariantDuration::Total(Duration::ZERO),
            name: None,
            codecs: None,
            container: None,
        })
        .collect()
}

fn prepare_abr(index: usize) -> Work {
    match index {
        0 => Work::new(|| {
            for _ in 0..100 {
                std::hint::black_box(AbrController::new(AbrSettings::default()));
            }
        }),
        1 => Work::new(|| {
            let controller = AbrController::new(AbrSettings::default());
            let state = AbrState::new(AbrMode::Auto(None));
            let variants = variants();
            for _ in 0..100 {
                let view = AbrView {
                    settings: &AbrSettings::default(),
                    variants: &variants,
                    buffer_ahead: Some(Duration::from_secs(2)),
                    estimate_bps: Some(1_000_000),
                    bytes_downloaded: 64_000,
                };
                std::hint::black_box(state.decide(&view, Instant::now()));
            }
            std::hint::black_box(controller);
        }),
        2 => Work::new(|| {
            let state = AbrState::new(AbrMode::Auto(None));
            let variants = variants();
            let settings = AbrSettings::default();
            let view = AbrView {
                settings: &settings,
                variants: &variants,
                buffer_ahead: Some(Duration::from_secs(2)),
                estimate_bps: Some(1_000_000),
                bytes_downloaded: 64_000,
            };
            std::hint::black_box(state.decide(&view, Instant::now()));
        }),
        3 => Work::new(|| {
            let estimator = ThroughputEstimator::new();
            for _ in 0..32 {
                estimator.push_sample(
                    96_000,
                    Duration::from_millis(250),
                    kithara::abr::BandwidthSource::Network,
                );
            }
            std::hint::black_box(estimator.estimate_bps());
        }),
        _ => unreachable!(),
    }
}

#[metabench::benchmark(SESSION_ABR, "session_abr", "case", gungraun_setup = prepare_abr)]
#[bench::abr_creation(0_usize)]
#[bench::abr_decision(1_usize)]
#[bench::abr_pure_decision(2_usize)]
#[bench::abr_estimator_loop(3_usize)]
fn native_abr(mut work: Work) -> Work {
    work.run();
    work
}

fn pool_cycle(pools: &Pools) {
    let mut buffer = pools.get_with_len::<f32>(2048).expect("pool buffer");
    for sample in buffer.iter_mut().take(16) {
        *sample = 0.25;
    }
    std::hint::black_box(buffer.len());
}

fn pool_threads(pools: &Pools, threads: usize, iterations: usize) {
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let pools = pools.clone();
            thread::spawn(move || {
                for _ in 0..iterations {
                    pool_cycle(&pools);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("joined pool worker");
    }
}

fn prepare_pool(index: usize) -> Work {
    match index {
        0 => {
            let pools = pools();
            Work::new(move || {
                for _ in 0..10_000 {
                    pool_cycle(&pools);
                }
            })
        }
        1 => {
            let pools = pools();
            Work::new(move || pool_threads(&pools, 8, 1_000))
        }
        2 => {
            let pools = pools();
            Work::new(move || {
                for _ in 0..1_000 {
                    pool_cycle(&pools);
                }
            })
        }
        3..=7 => {
            let threads = [1, 2, 4, 8, 16][index - 3];
            let pools = pools();
            Work::new(move || pool_threads(&pools, threads, 1_000))
        }
        _ => unreachable!(),
    }
}

#[metabench::benchmark(SESSION_POOL, "session_pool", "case", gungraun_setup = prepare_pool)]
#[bench::pool_single_thread(0_usize)]
#[bench::pool_multi_thread(1_usize)]
#[bench::pool_allocation_rate(2_usize)]
#[bench::pool_scalability_1(3_usize)]
#[bench::pool_scalability_2(4_usize)]
#[bench::pool_scalability_4(5_usize)]
#[bench::pool_scalability_8(6_usize)]
#[bench::pool_scalability_16(7_usize)]
fn native_pool(mut work: Work) -> Work {
    work.run();
    work
}

fn build_resampler(
    source_rate: u32,
    target_rate: u32,
    quality: ResamplerQuality,
    chunk_size: usize,
) -> Box<dyn Resampler> {
    let settings = ResamplerSettings::builder()
        .channels(NonZeroUsize::new(2).expect("stereo"))
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(source_rate).expect("source rate"),
            target_sample_rate: NonZeroU32::new(target_rate).expect("target rate"),
        })
        .quality(quality)
        .options(ResamplerOptions::builder().chunk_size(chunk_size).build())
        .pools(pools())
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    Box::new(create_resampler(&config).expect("resampler"))
}

fn process_stereo(
    resampler: &mut dyn Resampler,
    input: &[Vec<f32>; 2],
    output: &mut [Vec<f32>; 2],
) -> usize {
    let input_refs = [&input[0][..], &input[1][..]];
    let (left, right) = output.split_at_mut(1);
    let mut output_refs = [&mut left[0][..], &mut right[0][..]];
    resampler
        .process_into_buffer(&input_refs, &mut output_refs)
        .expect("resample")
        .second()
}

fn prepare_resampler(index: usize) -> Work {
    if index == 4 {
        let mut resampler = build_resampler(44_100, 44_100, ResamplerQuality::Good, 2048);
        let input = [
            perf_planar()[0][..2048].to_vec(),
            perf_planar()[1][..2048].to_vec(),
        ];
        let mut output = [vec![0.0; 4096], vec![0.0; 4096]];
        return Work::new(move || {
            for _ in 0..1_000 {
                std::hint::black_box(process_stereo(&mut *resampler, &input, &mut output));
            }
        });
    }
    if index == 8 {
        let interleaved = perf_interleaved();
        return Work::new(move || {
            for _ in 0..1_000 {
                let mut planar: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
                for (channel, buffer) in planar.iter_mut().enumerate() {
                    buffer.resize(2048, 0.0);
                    for frame in 0..2048 {
                        buffer[frame] = interleaved[frame * 2 + channel];
                    }
                }
                let mut interleaved = vec![0.0_f32; 4096];
                for frame in 0..2048 {
                    for channel in 0..2 {
                        interleaved[frame * 2 + channel] = planar[channel][frame];
                    }
                }
                std::hint::black_box(interleaved);
            }
        });
    }
    let (quality, chunk) = match index {
        0 => (ResamplerQuality::Fast, 2048),
        1 => (ResamplerQuality::Normal, 2048),
        2 => (ResamplerQuality::Good, 2048),
        3 | 9 => (ResamplerQuality::High, 2048),
        5 => (ResamplerQuality::High, 512),
        6 => (ResamplerQuality::High, 1024),
        7 => (ResamplerQuality::High, 4096),
        _ => unreachable!(),
    };
    let mut resampler = build_resampler(48_000, 44_100, quality, chunk);
    let input = [
        perf_planar()[0][..chunk].to_vec(),
        perf_planar()[1][..chunk].to_vec(),
    ];
    let mut output = [
        vec![0.0; resampler.output_frames_max()],
        vec![0.0; resampler.output_frames_max()],
    ];
    Work::new(move || {
        for _ in 0..100 {
            std::hint::black_box(process_stereo(&mut *resampler, &input, &mut output));
        }
    })
}

#[metabench::benchmark(SESSION_RESAMPLER, "session_resampler", "case", gungraun_setup = prepare_resampler)]
#[bench::resampler_quality_fast(0_usize)]
#[bench::resampler_quality_normal(1_usize)]
#[bench::resampler_quality_good(2_usize)]
#[bench::resampler_quality_high(3_usize)]
#[bench::resampler_passthrough(4_usize)]
#[bench::resampler_breakdown_512(5_usize)]
#[bench::resampler_breakdown_1024(6_usize)]
#[bench::resampler_breakdown_4096(7_usize)]
#[bench::resampler_deinterleave(8_usize)]
#[bench::resampler_breakdown_2048(9_usize)]
fn native_resampler(mut work: Work) -> Work {
    work.run();
    work
}

fn create_test_storage(size: usize) -> (MmapResource, NamedTempFile) {
    use std::io::Write;
    let mut file = NamedTempFile::new().expect("storage tempfile");
    file.write_all(&vec![0_u8; size]).expect("storage data");
    file.flush().expect("storage flush");
    let options = MmapOptions::for_path(file.path().to_path_buf())
        .mode(OpenMode::ReadWrite)
        .build();
    let storage = Resource::open(CancelToken::never(), options).expect("mmap storage");
    (storage, file)
}

fn prepare_storage(index: usize) -> Work {
    let (size, offset) = match index {
        0 | 8 => (4_096, 0),
        1 => (16_384, 0),
        2 => (65_536, 0),
        3 => (262_144, 0),
        4 => (4_096, 4_096),
        5 => (16_384, 16_384),
        6 => (65_536, 65_536),
        7 => (4_096, 7_919 * 4_096),
        _ => unreachable!(),
    };
    let (storage, temp_file) = create_test_storage(100 * 1024 * 1024);
    Work::new(move || {
        let _ = &temp_file;
        match index {
            0..=3 => {
                let mut buffer = vec![0_u8; size];
                storage
                    .read_at(offset, &mut buffer)
                    .expect("sequential read");
                std::hint::black_box(buffer);
            }
            4..=6 => {
                let data = vec![42_u8; size];
                storage.write_at(offset, &data).expect("write");
            }
            7 => {
                let mut buffer = vec![0_u8; 4_096];
                storage.read_at(offset, &mut buffer).expect("random read");
                std::hint::black_box(buffer);
            }
            _ => {
                let _ = storage.wait_range(0..4_096);
            }
        }
    })
}

#[metabench::benchmark(SESSION_STORAGE, "session_storage", "case", gungraun_setup = prepare_storage)]
#[bench::storage_sequential_4096(0_usize)]
#[bench::storage_sequential_16384(1_usize)]
#[bench::storage_sequential_65536(2_usize)]
#[bench::storage_sequential_262144(3_usize)]
#[bench::storage_write_4096(4_usize)]
#[bench::storage_write_16384(5_usize)]
#[bench::storage_write_65536(6_usize)]
#[bench::storage_random_read(7_usize)]
#[bench::storage_wait_range(8_usize)]
fn native_storage(mut work: Work) -> Work {
    work.run();
    work
}

fn criterion_case(name: &str, id: &str, b: &mut criterion::Bencher) {
    let index = match name {
        "session_decoder" => [
            "decoder_wav",
            "decoder_probe",
            "decoder_f32_conversion",
            "decoder_throughput",
        ]
        .iter()
        .position(|candidate| *candidate == id)
        .expect("decoder case"),
        "session_abr" => [
            "abr_creation",
            "abr_decision",
            "abr_pure_decision",
            "abr_estimator_loop",
        ]
        .iter()
        .position(|candidate| *candidate == id)
        .expect("ABR case"),
        "session_pool" => [
            "pool_single_thread",
            "pool_multi_thread",
            "pool_allocation_rate",
            "pool_scalability_1",
            "pool_scalability_2",
            "pool_scalability_4",
            "pool_scalability_8",
            "pool_scalability_16",
        ]
        .iter()
        .position(|candidate| *candidate == id)
        .expect("pool case"),
        "session_resampler" => [
            "resampler_quality_fast",
            "resampler_quality_normal",
            "resampler_quality_good",
            "resampler_quality_high",
            "resampler_passthrough",
            "resampler_breakdown_512",
            "resampler_breakdown_1024",
            "resampler_breakdown_4096",
            "resampler_deinterleave",
            "resampler_breakdown_2048",
        ]
        .iter()
        .position(|candidate| *candidate == id)
        .expect("resampler case"),
        _ => [
            "storage_sequential_4096",
            "storage_sequential_16384",
            "storage_sequential_65536",
            "storage_sequential_262144",
            "storage_write_4096",
            "storage_write_16384",
            "storage_write_65536",
            "storage_random_read",
            "storage_wait_range",
        ]
        .iter()
        .position(|candidate| *candidate == id)
        .expect("storage case"),
    };
    match name {
        "session_decoder" => {
            b.iter_batched(
                || prepare_decoder(index),
                |mut work| {
                    work.run();
                    work
                },
                BatchSize::SmallInput,
            );
        }
        "session_abr" => {
            let mut work = prepare_abr(index);
            b.iter(|| work.run());
        }
        "session_pool" => {
            let mut work = prepare_pool(index);
            b.iter(|| work.run());
        }
        "session_resampler" => {
            let mut work = prepare_resampler(index);
            b.iter(|| work.run());
        }
        _ => {
            let mut work = prepare_storage(index);
            b.iter(|| work.run());
        }
    }
}

fn criterion_benchmarks(c: &mut Criterion) {
    for (name, ids) in [
        (
            "session_decoder",
            [
                "decoder_wav",
                "decoder_probe",
                "decoder_f32_conversion",
                "decoder_throughput",
            ]
            .as_slice(),
        ),
        (
            "session_abr",
            [
                "abr_creation",
                "abr_decision",
                "abr_pure_decision",
                "abr_estimator_loop",
            ]
            .as_slice(),
        ),
        (
            "session_pool",
            [
                "pool_single_thread",
                "pool_multi_thread",
                "pool_allocation_rate",
                "pool_scalability_1",
                "pool_scalability_2",
                "pool_scalability_4",
                "pool_scalability_8",
                "pool_scalability_16",
            ]
            .as_slice(),
        ),
        (
            "session_resampler",
            [
                "resampler_quality_fast",
                "resampler_quality_normal",
                "resampler_quality_good",
                "resampler_quality_high",
                "resampler_passthrough",
                "resampler_breakdown_512",
                "resampler_breakdown_1024",
                "resampler_breakdown_4096",
                "resampler_deinterleave",
                "resampler_breakdown_2048",
            ]
            .as_slice(),
        ),
        (
            "session_storage",
            [
                "storage_sequential_4096",
                "storage_sequential_16384",
                "storage_sequential_65536",
                "storage_sequential_262144",
                "storage_write_4096",
                "storage_write_16384",
                "storage_write_65536",
                "storage_random_read",
                "storage_wait_range",
            ]
            .as_slice(),
        ),
    ] {
        let mut group = c.benchmark_group(name);
        group.sample_size(10);
        for id in ids {
            group.bench_function(BenchmarkId::new("case", id), |b| {
                criterion_case(name, id, b);
            });
        }
        group.finish();
    }
}

metabench::main!(
    criterion = criterion_benchmarks,
    benchmarks = [
        SESSION_DECODER,
        SESSION_ABR,
        SESSION_POOL,
        SESSION_RESAMPLER,
        SESSION_STORAGE
    ],
    allocator = std::alloc::System,
);
