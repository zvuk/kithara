//! RED test: HLS→MP3 crossfade blocks the render thread.
//!
//! # Hypothesis
//!
//! When a freshly-loaded MP3 resource (no `preload()` call — production timing)
//! is crossfaded in while a shared `AudioWorkerHandle` is busy stepping an HLS
//! track that itself waits for network data, the MP3's `Audio::read()` path
//! enters `recv_outcome_blocking()` and parks the render thread in a tight
//! `park_timeout(RECV_BACKOFF)` loop (see
//! `crates/kithara-audio/src/audio.rs:481` and `:516`). Because the single
//! shared worker round-robins tracks and spends ~10ms per HLS step inside
//! `wait_range()` (see `step_track took too long` warnings emitted by
//! `crates/kithara-audio/src/worker/handle.rs:258`), the MP3 ringbuf stays
//! empty, and `OfflinePlayer::render(block_frames)` blocks past the block
//! budget (~11.6 ms at 512 frames / 44.1 kHz).
//!
//! The parent test
//! `kithara_play::resource_regressions::stress_offline_crossfade_no_gaps`
//! flakes on the same condition — it asserts `slow_renders <= 1` and observes
//! up to 4 blocks exceeding budget with `max_render` reaching 65–99 ms during
//! HLS→MP3 transitions.
//!
//! # Why this fails
//!
//! The offline render thread synchronously calls `PlayerResource::read` →
//! `Audio::read` → `recv_outcome_blocking` on the non-preloaded MP3. While the
//! shared worker is busy in an HLS `step_track` for ≥10 ms, no PCM chunks are
//! delivered to the MP3 ring buffer, so the render thread sits in
//! `park_timeout(100µs)` for dozens of iterations — well past the 11.6 ms audio
//! block budget.

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::sync::Arc;

use kithara::{
    audio::{Audio, AudioConfig, AudioWorkerHandle},
    hls::{Hls, HlsConfig},
    play::{Resource, internal::offline::resource_from_reader},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_file::{File as FileSource, FileConfig, FileSrc};
use kithara_platform::time::{Duration, Instant, sleep};
use tokio::time::timeout;
use tracing::info;

use crate::continuity::render_offline_window;

struct Consts;
impl Consts {
    const READ_TIMEOUT: Duration = Duration::from_secs(5);
    const BLOCK: usize = 512;
    const SR: u32 = 44_100;
}

#[kithara::test(
    tokio,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "10")
)]
async fn red_hls_to_mp3_crossfade_no_render_budget_violations() {
    use kithara::{assets::StoreOptions, play::internal::offline::OfflinePlayer};
    use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
    use kithara_test_utils::{create_wav_exact_bytes, signal_pcm::signal, temp_dir};

    const HLS_SEGMENT_COUNT: usize = 3;
    const HLS_SEGMENT_SIZE: usize = 200_000;
    const HLS_TOTAL_BYTES: usize = HLS_SEGMENT_COUNT * HLS_SEGMENT_SIZE;
    const HLS_SAMPLE_RATE: f64 = 44_100.0;
    const HLS_CHANNELS: f64 = 2.0;

    let segment_duration = HLS_SEGMENT_SIZE as f64 / (HLS_SAMPLE_RATE * HLS_CHANNELS * 2.0);
    let hls_server = HlsTestServer::new(HlsTestServerConfig {
        custom_data: Some(Arc::new(create_wav_exact_bytes(
            signal::Sawtooth,
            44_100u32,
            2u16,
            HLS_TOTAL_BYTES,
        ))),
        segment_duration_secs: segment_duration,
        segment_size: HLS_SEGMENT_SIZE,
        segments_per_variant: HLS_SEGMENT_COUNT,
        ..Default::default()
    })
    .await;
    let temp = temp_dir();
    let mut store = StoreOptions::new(temp.path());
    store.ephemeral = true;
    store.cache_capacity = Some(std::num::NonZeroUsize::new(4).expect("nonzero"));
    store.max_assets = Some(8);
    let hls_url = hls_server.url("/master.m3u8");

    let worker = AudioWorkerHandle::new();
    let mut player = OfflinePlayer::new(Consts::SR);

    let local_mp3 = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/test.mp3");

    // Build MP3 resource WITHOUT preload — matches production timing.
    let make_mp3 = |w: AudioWorkerHandle| {
        let p = local_mp3.clone();
        async move {
            let file_cfg = FileConfig::new(FileSrc::Local(p));
            let audio_cfg = AudioConfig::<FileSource>::new(file_cfg)
                .with_hint("mp3")
                .with_worker(w);
            let audio = Audio::<Stream<FileSource>>::new(audio_cfg)
                .await
                .expect("create local MP3 audio");
            resource_from_reader(audio)
        }
    };

    // Build and preload HLS resource (as in production).
    let make_hls = |w: AudioWorkerHandle, s: StoreOptions| {
        let u = hls_url.clone();
        async move {
            let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
            let cfg = HlsConfig::new(u).with_store(s);
            let audio_cfg = AudioConfig::<Hls>::new(cfg)
                .with_media_info(wav_info)
                .with_worker(w);
            let audio = Audio::<Stream<Hls>>::new(audio_cfg)
                .await
                .expect("create HLS audio");
            let mut r: Resource = resource_from_reader(audio);
            timeout(Consts::READ_TIMEOUT, r.preload())
                .await
                .expect("HLS preload");
            r
        }
    };

    // Try 10 iterations; fail as soon as any HLS→MP3 fade trips the budget,
    // so a single deterministic slow render is enough evidence.
    let mut worst_slow_renders: u32 = 0;
    let mut worst_max_render: Duration = Duration::ZERO;
    let mut worst_label = String::new();

    for iter in 0..10 {
        // HLS phase: play for a bit to get it producing chunks.
        let hls = make_hls(worker.clone(), store.clone()).await;
        sleep(Duration::from_millis(200)).await;
        player.load_and_fadein(hls, &format!("red_hls_{iter}"));
        let _hls_warmup = render_offline_window(
            &mut player,
            40,
            &format!("HLS warmup #{iter}"),
            Consts::BLOCK,
            Consts::SR,
        );

        // Crossfade HLS→MP3. This is where the render thread can block.
        let mp3 = make_mp3(worker.clone()).await;
        sleep(Duration::from_millis(200)).await;
        let before_fade = Instant::now();
        player.load_and_fadein(mp3, &format!("red_mp3_{iter}"));
        let fade_stats = render_offline_window(
            &mut player,
            60,
            &format!("HLS→MP3 red #{iter}"),
            Consts::BLOCK,
            Consts::SR,
        );
        info!(
            "iter {iter}: {fade_stats}, wall={:?}",
            before_fade.elapsed()
        );

        if fade_stats.slow_renders > worst_slow_renders {
            worst_slow_renders = fade_stats.slow_renders;
            worst_max_render = fade_stats.max_render;
            worst_label = fade_stats.label.clone();
        }
    }

    // Same contract as the parent test: at most 1 slow render per window.
    assert!(
        worst_slow_renders <= 1,
        "red: HLS→MP3 crossfade exceeded render budget on {} blocks \
         (worst label={worst_label}, max_render={worst_max_render:?}) — \
         render thread was blocked synchronously waiting for MP3 PCM chunks \
         while the shared worker was busy on HLS",
        worst_slow_renders,
    );
}
