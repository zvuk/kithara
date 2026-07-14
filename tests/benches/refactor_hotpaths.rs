#![forbid(unsafe_code)]

use std::{
    fs,
    hint::black_box,
    io::{Read, Seek, SeekFrom},
    num::{NonZeroU32, NonZeroUsize},
};

use axum::{
    Router,
    body::Body,
    extract::{Path, Request},
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use criterion::{BatchSize, Criterion, SamplingMode, criterion_group, criterion_main};
use kithara::{
    assets::{StorageBackend, StoreOptions},
    audio::{Audio, AudioConfig},
    bufpool::{BytePool, PcmPool},
    file::{File, FileConfig},
    hls::{Hls, HlsConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::Duration,
        tokio::runtime::{Builder, Runtime},
    },
    resampler::{
        Resampler, ResamplerConfig, ResamplerMode, ResamplerOptions, ResamplerQuality,
        ResamplerSettings, create_resampler, rubato::RubatoBackend,
    },
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_integration_tests::{TestHttpServer, auto};
use tempfile::TempDir;
use url::Url;

struct Consts;
impl Consts {
    const TEST_MP3_BYTES: &'static [u8] = include_bytes!("../../assets/test.mp3");
    const HLS_SEGMENT_COUNT: usize = 6;
    const HLS_SEGMENT_SIZE: usize = 96_000;
    const AUDIO_READ_TARGET_SAMPLES: usize = 32_768;
    const HLS_READ_TARGET_BYTES: usize = 196_608;
    const HLS_SEEK_POSITIONS: [u64; 5] = [0, 32_000, 128_000, 256_000, 384_000];
}

fn make_runtime() -> Runtime {
    Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| panic!("failed to build tokio runtime: {e}"))
}

fn make_pcm(frames: usize, channels: usize) -> Vec<f32> {
    let total = frames * channels;
    (0..total)
        .map(|i| {
            #[expect(
                clippy::cast_precision_loss,
                reason = "bounded small integer to f32 in deterministic test signal"
            )]
            let value = (i % 128) as f32;
            value / 128.0 - 0.5
        })
        .collect()
}

fn make_planar(frames: usize) -> [Vec<f32>; 2] {
    let interleaved = make_pcm(frames, 2);
    [
        interleaved.iter().step_by(2).copied().collect(),
        interleaved.iter().skip(1).step_by(2).copied().collect(),
    ]
}

fn make_output(resampler: &dyn Resampler) -> [Vec<f32>; 2] {
    [
        vec![0.0; resampler.output_frames_next()],
        vec![0.0; resampler.output_frames_next()],
    ]
}

fn build_resampler(source_rate: u32, target_rate: u32, frames: usize) -> Box<dyn Resampler> {
    let settings = ResamplerSettings::builder()
        .channels(NonZeroUsize::new(2).unwrap_or_else(|| panic!("bench channels")))
        .mode(ResamplerMode::FixedRatio {
            source_sample_rate: NonZeroU32::new(source_rate)
                .unwrap_or_else(|| panic!("bench source rate")),
            target_sample_rate: NonZeroU32::new(target_rate)
                .unwrap_or_else(|| panic!("bench target rate")),
        })
        .quality(ResamplerQuality::High)
        .options(ResamplerOptions::builder().chunk_size(frames).build())
        .pcm_pool(PcmPool::new(64, frames.saturating_mul(16)))
        .build();
    let config = ResamplerConfig::builder()
        .backend(RubatoBackend::new())
        .settings(settings)
        .build();
    Box::new(
        create_resampler(&config)
            .unwrap_or_else(|err| panic!("bench resampler should build: {err}")),
    )
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
        .unwrap_or_else(|err| panic!("bench resampler process should succeed: {err}"))
        .output_frames
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "axum handler requires owned request"
)]
fn serve_mp3_with_range(req: Request) -> Response {
    if req.method() == Method::HEAD {
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "audio/mpeg")
            .header(
                header::CONTENT_LENGTH,
                Consts::TEST_MP3_BYTES.len().to_string(),
            )
            .body(Body::empty())
            .unwrap_or_else(|e| panic!("failed to build head response: {e}"));
    }

    if let Some(range_header) = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        && let Some(range) = range_header.strip_prefix("bytes=")
    {
        let mut parts = range.split('-');
        let start = parts
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let end = parts
            .next()
            .and_then(|s| {
                if s.is_empty() {
                    None
                } else {
                    s.parse::<usize>().ok()
                }
            })
            .unwrap_or(Consts::TEST_MP3_BYTES.len().saturating_sub(1))
            .min(Consts::TEST_MP3_BYTES.len().saturating_sub(1));

        if start <= end && start < Consts::TEST_MP3_BYTES.len() {
            let chunk = &Consts::TEST_MP3_BYTES[start..=end];
            return Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, "audio/mpeg")
                .header(header::CONTENT_LENGTH, chunk.len().to_string())
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{}", Consts::TEST_MP3_BYTES.len()),
                )
                .body(Body::from(Bytes::from_static(chunk)))
                .unwrap_or_else(|e| panic!("failed to build partial response: {e}"));
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .header(
            header::CONTENT_LENGTH,
            Consts::TEST_MP3_BYTES.len().to_string(),
        )
        .body(Body::from(Bytes::from_static(Consts::TEST_MP3_BYTES)))
        .unwrap_or_else(|e| panic!("failed to build full response: {e}"))
}

async fn mp3_endpoint(req: Request) -> Response {
    serve_mp3_with_range(req)
}

fn hls_master_playlist() -> &'static str {
    r#"#EXTM3U
#EXT-X-VERSION:6
#EXT-X-STREAM-INF:BANDWIDTH=640000,CODECS="mp4a.40.2"
v0.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=1280000,CODECS="mp4a.40.2"
v1.m3u8
"#
}

fn hls_media_playlist(variant: usize) -> String {
    let mut lines = String::from(
        "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n",
    );
    for segment in 0..Consts::HLS_SEGMENT_COUNT {
        lines.push_str("#EXTINF:4.0,\n");
        lines.push_str(&format!("seg/{variant}/{segment}.bin\n"));
    }
    lines.push_str("#EXT-X-ENDLIST\n");
    lines
}

fn hls_segment_data(variant: usize, segment: usize) -> Vec<u8> {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "synthetic fixture byte pattern is intentionally 8-bit"
    )]
    let pattern = ((variant * 37 + segment * 11) % 256) as u8;
    vec![pattern; Consts::HLS_SEGMENT_SIZE]
}

async fn hls_master_endpoint() -> &'static str {
    hls_master_playlist()
}

async fn hls_variant_0_endpoint() -> String {
    hls_media_playlist(0)
}

async fn hls_variant_1_endpoint() -> String {
    hls_media_playlist(1)
}

async fn hls_segment_endpoint(Path((variant, segment)): Path<(usize, String)>) -> Response {
    let Some(segment_index) = segment
        .strip_suffix(".bin")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if variant > 1 || segment_index >= Consts::HLS_SEGMENT_COUNT {
        return StatusCode::NOT_FOUND.into_response();
    }

    (
        [(header::CONTENT_TYPE, "application/octet-stream")],
        hls_segment_data(variant, segment_index),
    )
        .into_response()
}

fn app() -> Router {
    Router::new()
        .route("/test.mp3", get(mp3_endpoint).head(mp3_endpoint))
        .route("/master.m3u8", get(hls_master_endpoint))
        .route("/v0.m3u8", get(hls_variant_0_endpoint))
        .route("/v1.m3u8", get(hls_variant_1_endpoint))
        .route("/seg/{variant}/{segment}", get(hls_segment_endpoint))
}

fn bench_resampler_process(c: &mut Criterion) {
    let mut group = c.benchmark_group("refactor_resampler");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(8));

    group.bench_function("unity_ratio_process", |b| {
        let mut resampler = build_resampler(44_100, 44_100, 4_096);
        let input = make_planar(4_096);
        let mut output = make_output(&*resampler);

        b.iter(|| {
            let frames = process_stereo(&mut *resampler, &input, &mut output);
            black_box(frames);
            resampler.reset();
        });
    });

    group.bench_function("fixed_ratio_process", |b| {
        let mut resampler = build_resampler(48_000, 44_100, 8_192);
        let input = make_planar(8_192);
        let mut output = make_output(&*resampler);

        b.iter(|| {
            let frames = process_stereo(&mut *resampler, &input, &mut output);
            black_box(frames);
            resampler.reset();
        });
    });

    group.finish();
}

fn bench_audio_file_new_and_read(c: &mut Criterion) {
    let rt = make_runtime();

    let mut group = c.benchmark_group("refactor_audio_pipeline");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("new_and_read", |b| {
        b.iter_batched(
            || {
                let temp_dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir failed: {e}"));
                let file_path = temp_dir.path().join("bench.mp3");
                fs::write(&file_path, Consts::TEST_MP3_BYTES)
                    .unwrap_or_else(|e| panic!("failed to write bench mp3: {e}"));
                (temp_dir, file_path)
            },
            |(_temp_dir, file_path)| {
                rt.block_on(async move {
                    let file_config = FileConfig::new(file_path.into());
                    let config = AudioConfig::<File>::for_stream(file_config)
                        .byte_pool(BytePool::default())
                        .pcm_pool(PcmPool::default())
                        .hint(("mp3").to_string())
                        .byte_pool(BytePool::default())
                        .pcm_pool(PcmPool::default())
                        .build();
                    let mut audio = Audio::<Stream<File>>::new(config)
                        .await
                        .unwrap_or_else(|e| panic!("audio init failed: {e}"));

                    let mut buf = [0.0_f32; 4_096];
                    let mut total = 0usize;
                    while total < Consts::AUDIO_READ_TARGET_SAMPLES {
                        match audio.read(&mut buf) {
                            Ok(kithara::audio::ReadOutcome::Frames { count, .. }) => {
                                total += count.get();
                            }
                            Ok(kithara::audio::ReadOutcome::Pending { .. }) => continue,
                            Ok(kithara::audio::ReadOutcome::Eof { .. }) => break,
                            Err(e) => panic!("audio read failed: {e}"),
                        }
                    }
                    black_box(total);
                });
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_hls_stream_seek_read(c: &mut Criterion) {
    let rt = make_runtime();
    let server = rt.block_on(TestHttpServer::new(app()));
    let master_url: Url = server.url("/master.m3u8");
    let server_guard = server;

    let mut group = c.benchmark_group("refactor_hls_downloader");
    group.sampling_mode(SamplingMode::Flat);
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("new_seek_read_cycle", |b| {
        let _ = &server_guard;
        b.iter_batched(
            || (),
            |()| {
                let url = master_url.clone();
                rt.block_on(async move {
                    let net = NetOptions::builder().pool_max_idle_per_host(8).build();
                    let downloader = Downloader::new(
                        DownloaderConfig::builder()
                            .client(HttpClient::new(net, CancelToken::never()))
                            .build(),
                    );
                    let store = StoreOptions::builder()
                        .backend(StorageBackend::Memory)
                        .max_bytes(200_000)
                        .build();
                    let config = HlsConfig::for_url(url)
                        .store(store)
                        .initial_abr_mode(auto(1))
                        .downloader(downloader)
                        .download_batch_size(3)
                        .look_ahead_bytes(96_000)
                        .build();

                    let mut stream = Stream::<Hls>::new(config)
                        .await
                        .unwrap_or_else(|e| panic!("stream init failed: {e}"));

                    let mut buf = [0_u8; 8_192];
                    let mut total = 0usize;
                    while total < Consts::HLS_READ_TARGET_BYTES {
                        let n = stream
                            .read(&mut buf)
                            .unwrap_or_else(|e| panic!("stream read failed: {e}"));
                        if n == 0 {
                            break;
                        }
                        total += n;
                    }

                    for seek_pos in Consts::HLS_SEEK_POSITIONS {
                        if let Some(len) = stream.len()
                            && seek_pos > len
                        {
                            continue;
                        }
                        stream
                            .seek(SeekFrom::Start(seek_pos))
                            .unwrap_or_else(|e| panic!("seek failed at {seek_pos}: {e}"));
                        let n = stream
                            .read(&mut buf)
                            .unwrap_or_else(|e| panic!("read after seek failed: {e}"));
                        black_box(n);
                    }

                    black_box(total);
                });
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_resampler_process,
    bench_audio_file_new_and_read,
    bench_hls_stream_seek_read
);
criterion_main!(benches);
