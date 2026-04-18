#![forbid(unsafe_code)]

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
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
use criterion::{BatchSize, Criterion, SamplingMode, black_box, criterion_group, criterion_main};
use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, AudioEffect, ResamplerQuality},
    bufpool::pcm_pool,
    decode::{PcmChunk, PcmMeta, PcmSpec},
    file::{File, FileConfig},
    hls::{AbrMode, AbrOptions, Hls, HlsConfig},
    net::NetOptions,
    stream::{
        Stream,
        dl::{Downloader, DownloaderConfig},
    },
};
use kithara_audio::internal::{ResamplerParams, ResamplerProcessor};
use kithara_platform::tokio::runtime::{Builder, Runtime};
use kithara_test_utils::TestHttpServer;
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

    const fn hls_total_bytes() -> usize {
        Self::HLS_SEGMENT_COUNT * Self::HLS_SEGMENT_SIZE
    }
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

fn make_chunk(sample_rate: u32, channels: u16, frames: usize) -> PcmChunk {
    PcmChunk::new(
        PcmMeta {
            spec: PcmSpec {
                channels,
                sample_rate,
            },
            ..Default::default()
        },
        pcm_pool().attach(make_pcm(frames, usize::from(channels))),
    )
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

    group.bench_function("passthrough_process", |b| {
        let host_rate = Arc::new(AtomicU32::new(44_100));
        let params = ResamplerParams::new(Arc::clone(&host_rate), 44_100, 2)
            .with_quality(ResamplerQuality::High);
        let mut processor = ResamplerProcessor::new(params);
        let chunk = make_chunk(44_100, 2, 4_096);

        b.iter(|| {
            let output = processor.process(chunk.clone());
            black_box(output);
            AudioEffect::reset(&mut processor);
        });
    });

    group.bench_function("rate_switch_process", |b| {
        let host_rate = Arc::new(AtomicU32::new(44_100));
        let params = ResamplerParams::new(Arc::clone(&host_rate), 48_000, 2)
            .with_quality(ResamplerQuality::High);
        let mut processor = ResamplerProcessor::new(params);
        let chunk = make_chunk(48_000, 2, 8_192);

        b.iter(|| {
            host_rate.store(44_100, Ordering::Relaxed);
            let output = processor.process(chunk.clone());
            black_box(output);
            host_rate.store(48_000, Ordering::Relaxed);
            AudioEffect::reset(&mut processor);
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
                    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
                    let mut audio = Audio::<Stream<File>>::new(config)
                        .await
                        .unwrap_or_else(|e| panic!("audio init failed: {e}"));

                    let mut buf = [0.0_f32; 4_096];
                    let mut total = 0usize;
                    while total < Consts::AUDIO_READ_TARGET_SAMPLES {
                        let n = audio.read(&mut buf);
                        if n == 0 {
                            break;
                        }
                        total += n;
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
            || TempDir::new().unwrap_or_else(|e| panic!("tempdir failed: {e}")),
            |temp_dir| {
                let url = master_url.clone();
                rt.block_on(async move {
                    let net = NetOptions {
                        pool_max_idle_per_host: 8,
                        ..NetOptions::default()
                    };
                    let downloader = Downloader::new(DownloaderConfig::default().with_net(net));
                    let store = StoreOptions::new(temp_dir.path())
                        .with_ephemeral(true)
                        .with_max_bytes(200_000);
                    let abr = AbrOptions {
                        mode: AbrMode::Auto(Some(1)),
                        ..AbrOptions::default()
                    };
                    let config = HlsConfig::new(url)
                        .with_store(store)
                        .with_abr_options(abr)
                        .with_downloader(downloader)
                        .with_download_batch_size(3)
                        .with_look_ahead_bytes(96_000);

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
                        // Size maps are populated via HEAD and can transiently under-report.
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
