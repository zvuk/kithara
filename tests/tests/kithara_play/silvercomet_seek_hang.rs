//! Real-network integration test: silvercomet HLS/MP3/DRM seek hang detection.
//!
//! For each of 10 iterations, walks three live silvercomet streams (MP3,
//! HLS, DRM-HLS), plays each for 3 s wall, seeks to the middle of the
//! track, plays another 3 s, and asserts:
//!
//! 1. `render_offline_window` observes no long silence run — the player
//!    never parks the render thread waiting for decoded chunks.
//! 2. `OfflinePlayer::position` advances by ≥1.5 s over each 3 s wall
//!    window — the `Audio::seek` FSM eventually surfaces chunks at the
//!    new epoch.
//!
//! Rendered interleaved stereo f32 samples are captured and written to
//! `/tmp/kithara_silvercomet_seek_iter_<N>.wav` (IEEE float PCM) for
//! post-mortem inspection. Each iteration uses a fresh temp cache
//! directory so no downloaded segment survives between runs.
//!
//! `#[ignore]` — requires live network to silvercomet.top and ~3 minutes
//! wall clock. Run with:
//! `cargo nextest run -p kithara-integration-tests kithara_play::silvercomet_seek_hang --run-ignored only --no-capture`

#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{fs::File, io::Write, path::Path};

use kithara::{
    assets::StoreOptions,
    net::NetOptions,
    play::{Resource, ResourceConfig, internal::offline::OfflinePlayer},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_platform::{
    thread,
    time::{Duration, Instant},
};
use kithara_test_utils::temp_dir;

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = 44_100;
    const CHANNELS: u16 = 2;
    const BLOCK_FRAMES: usize = 512;
    const PLAY_WINDOW_SECS: f64 = 3.0;
    /// Render warmup burned before the first measurement window so
    /// decoder startup silence (the few hundred ms between `load_and_fadein`
    /// and the first produced PCM chunk under HLS segment fetch latency)
    /// doesn't get miscounted as a hang.
    const WARMUP_SECS: f64 = 2.0;
    /// OfflineBackend renders at ~40-50% realtime against a live network —
    /// each render block has to wait on HTTP byte arrivals. Floor lowered
    /// from 1.5 s to 1.0 s: a true hang produces 0.0 s of advance (all
    /// silence), so 1.0 s still separates "making progress" from "frozen".
    const MIN_POSITION_ADVANCE_SECS: f64 = 1.0;
    /// Consecutive all-silence blocks that still counts as healthy.
    /// 150 blocks at 512 frames / 44.1 kHz ≈ 1.7 s — half of the 3 s
    /// window. The user-reported hang produces the full window worth
    /// (~259 blocks) of silence; partial stalls under network jitter
    /// are tolerated below this threshold.
    const MAX_SILENCE_BLOCKS: u32 = 150;
    /// Global RMS floor for the captured WAV — any iteration whose
    /// total-file RMS falls below this was effectively silent, meaning
    /// playback never really started on at least one of the three
    /// tracks.
    const MIN_WAV_RMS: f32 = 0.01;
    const ITERATIONS: usize = 10;
}

// HLS-only: the user's hang reproduces on HLS tracks — dropping MP3/DRM
// keeps the test focused on the actual failure path and shortens the
// wall-clock cost from ~3 min to ~1 min per 10-iteration run.
const SILVERCOMET_URLS: &[&str] = &["https://stream.silvercomet.top/hls/master.m3u8"];

/// Render `blocks` audio blocks, collect the raw interleaved samples,
/// and track the longest consecutive silence run. Mirrors
/// `crate::continuity::render_offline_window` but retains the rendered
/// output so the caller can write it to disk and assert on the
/// resulting signal.
fn render_and_collect(player: &mut OfflinePlayer, blocks: u32, samples_out: &mut Vec<f32>) -> u32 {
    const ACTIVE_THRESHOLD: f32 = 0.001;
    let block_budget =
        Duration::from_secs_f64(Consts::BLOCK_FRAMES as f64 / f64::from(Consts::SAMPLE_RATE));

    let mut max_silence = 0u32;
    let mut current_silence = 0u32;

    for _ in 0..blocks {
        let started = Instant::now();
        let out = player.render(Consts::BLOCK_FRAMES);
        let elapsed = started.elapsed();

        if out.iter().any(|s| s.abs() > ACTIVE_THRESHOLD) {
            if current_silence > max_silence {
                max_silence = current_silence;
            }
            current_silence = 0;
        } else {
            current_silence += 1;
        }

        samples_out.extend_from_slice(&out);
        thread::sleep(block_budget.saturating_sub(elapsed));
    }

    if current_silence > max_silence {
        max_silence = current_silence;
    }
    max_silence
}

fn blocks_for_seconds(secs: f64) -> u32 {
    let blocks = (secs * f64::from(Consts::SAMPLE_RATE) / Consts::BLOCK_FRAMES as f64).ceil();
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "positive ceiling fits in u32 for second-scale windows"
    )]
    let result = blocks as u32;
    result
}

fn fresh_downloader() -> Downloader {
    let net = NetOptions {
        insecure: true,
        ..NetOptions::default()
    };
    Downloader::new(DownloaderConfig::default().with_net(net))
}

async fn build_resource(
    url: &str,
    downloader: &Downloader,
    iter_label: &str,
    store: StoreOptions,
) -> Resource {
    let mut cfg =
        ResourceConfig::new(url).unwrap_or_else(|e| panic!("ResourceConfig::new({url}): {e}"));
    cfg = cfg
        .with_downloader(downloader.clone())
        .with_name(format!("{iter_label}|{url}"));
    cfg.store = store;
    let mut resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new({url}): {e:?}"));
    // Wait for the first decoded PCM chunk so `OfflinePlayer::render` sees
    // audio on the very first block after `load_and_fadein`. Without this
    // preload the 3 s measurement window is dominated by cold-start
    // silence and can't distinguish "loading" from "hanged".
    tokio::time::timeout(Duration::from_secs(15), resource.preload())
        .await
        .unwrap_or_else(|_| panic!("Resource::preload({url}) timed out after 15s"));
    resource
}

/// Minimal IEEE-float 32-bit stereo WAV writer (RIFF/WAVE, fmt code 3).
///
/// `interleaved` is L,R,L,R,... — same layout
/// `OfflinePlayer::render` returns.
fn write_wav_f32(path: &Path, interleaved: &[f32], sample_rate: u32, channels: u16) {
    let byte_rate = sample_rate
        .checked_mul(u32::from(channels))
        .and_then(|v| v.checked_mul(4))
        .expect("byte_rate fits in u32");
    let block_align = channels * 4;
    let data_bytes = u32::try_from(interleaved.len() * 4).expect("WAV data length fits in u32");
    let riff_size = 36u32
        .checked_add(data_bytes)
        .expect("WAV RIFF size fits in u32");

    let mut file = File::create(path).expect("create WAV");
    file.write_all(b"RIFF").expect("write RIFF");
    file.write_all(&riff_size.to_le_bytes())
        .expect("write size");
    file.write_all(b"WAVE").expect("write WAVE");
    file.write_all(b"fmt ").expect("write fmt ");
    file.write_all(&16u32.to_le_bytes())
        .expect("fmt chunk size");
    file.write_all(&3u16.to_le_bytes())
        .expect("format IEEE float");
    file.write_all(&channels.to_le_bytes()).expect("channels");
    file.write_all(&sample_rate.to_le_bytes()).expect("sr");
    file.write_all(&byte_rate.to_le_bytes()).expect("byte rate");
    file.write_all(&block_align.to_le_bytes())
        .expect("block align");
    file.write_all(&32u16.to_le_bytes())
        .expect("bits per sample");
    file.write_all(b"data").expect("write data");
    file.write_all(&data_bytes.to_le_bytes()).expect("data len");
    for &s in interleaved {
        file.write_all(&s.to_le_bytes()).expect("sample");
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample count precision adequate"
    )]
    let n = samples.len() as f32;
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / n).sqrt()
}

// `multi_thread` is required: the test body enters a synchronous
// `render_and_collect` loop (with `thread::sleep` between blocks) for each
// 3 s measurement window. On a `current_thread` runtime the Downloader
// tokio task cannot run while the main thread is inside that sync loop, so
// post-seek fetches for the new target segment never progress and the
// decoder observes a full window of silence even when the HLS logic itself
// is correct.
#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(600)))]
#[ignore = "real network to silvercomet.top; run with --run-ignored only"]
async fn silvercomet_3tracks_seek_middle_hang_10x() {
    // Pipe kithara_* trace output to /tmp/silvercomet-trace.log so failures
    // surface the FSM seek transitions and HLS peer fetches that a nextest
    // capture typically loses after the panic. `try_init()` keeps the test
    // re-runnable — a second init would otherwise panic.
    let trace_log = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("/tmp/silvercomet-trace.log")
        .expect("open trace log");
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "kithara_audio=debug,kithara_hls=debug,kithara_stream=debug",
                )
            }),
        )
        .with_writer(std::sync::Mutex::new(trace_log))
        .with_ansi(false)
        .try_init();

    // One OfflinePlayer per iteration — gives each iteration a fresh
    // FirewheelCtx, PlayerNode state, and shared atomics. No global
    // session_client involvement, so this test doesn't need
    // `init_offline_backend` (and therefore doesn't race with any
    // other test that might be using the global offline session).
    let window_blocks = blocks_for_seconds(Consts::PLAY_WINDOW_SECS);
    let warmup_blocks = blocks_for_seconds(Consts::WARMUP_SECS);
    let mut next_seek_epoch = 1u64;

    for iter in 0..Consts::ITERATIONS {
        let iter_label = format!("iter-{iter}");
        let temp = temp_dir();
        let store = StoreOptions::new(temp.path());
        let downloader = fresh_downloader();

        let mut player = OfflinePlayer::new(Consts::SAMPLE_RATE);
        let mut iteration_samples: Vec<f32> = Vec::new();

        for (track_idx, url) in SILVERCOMET_URLS.iter().enumerate() {
            eprintln!("[iter {iter}][t{track_idx}] building resource: {url}");
            let resource = build_resource(url, &downloader, &iter_label, store.clone()).await;
            eprintln!("[iter {iter}][t{track_idx}] resource built, load_and_fadein");
            player.load_and_fadein(resource, &format!("{iter_label}|t{track_idx}"));

            // Warmup: absorb cold-start latency (decoder spin-up, first
            // segment fetch) so the measurement window starts with audio
            // already flowing. Without this the initial-play silence run
            // is dominated by load latency, not by any stall.
            eprintln!("[iter {iter}][t{track_idx}] warmup ({warmup_blocks} blocks)");
            let _ = render_and_collect(&mut player, warmup_blocks, &mut iteration_samples);
            eprintln!(
                "[iter {iter}][t{track_idx}] warmup done, position={:.2}",
                player.position()
            );

            // (a) Measurement window — 3 s of continuous audio. A hang
            // here means the track never produced chunks even after the
            // warmup ran out.
            let pos_before_initial = player.position();
            let silence_initial =
                render_and_collect(&mut player, window_blocks, &mut iteration_samples);
            let pos_after_initial = player.position();
            eprintln!(
                "[iter {iter}][t{track_idx}] initial window: silence={silence_initial} \
                 pos_before={pos_before_initial:.2} pos_after={pos_after_initial:.2}"
            );

            assert!(
                silence_initial <= Consts::MAX_SILENCE_BLOCKS,
                "[iter {iter}] {url}: initial-play silence run = {silence_initial} blocks \
                 (max {} allowed) — track never produced audio before seek",
                Consts::MAX_SILENCE_BLOCKS,
            );
            assert!(
                pos_after_initial - pos_before_initial >= Consts::MIN_POSITION_ADVANCE_SECS,
                "[iter {iter}] {url}: initial-play position advanced only {:.2}s \
                 (expected ≥{:.2}s) — decoder stalled before any seek happened",
                pos_after_initial - pos_before_initial,
                Consts::MIN_POSITION_ADVANCE_SECS,
            );

            // (b) Seek to the middle of whatever the decoder currently
            // thinks the track is. We don't know the exact duration from
            // OfflinePlayer, so use the position after 3 s of play as a
            // lower bound on duration and jump to +30 s beyond it —
            // silvercomet tracks are long enough to absorb that and the
            // target always lands in undownloaded segments.
            let seek_target = pos_after_initial + 30.0;
            let seek_epoch = next_seek_epoch;
            next_seek_epoch += 1;
            eprintln!("[iter {iter}][t{track_idx}] seek to {seek_target:.2}s epoch={seek_epoch}");
            player.seek(seek_target, seek_epoch);

            // (c) Play ~3 s after the seek. This is the window that
            // reproduces the user-reported hang: if `Audio::seek`
            // transitions to `TrackState::Failed` on decoder
            // `SeekFailed("timestamp out-of-range")`, the render thread
            // will see only silence and `position` will stay frozen at
            // the stale seek target for the entire window.
            let pos_before_after_seek = player.position();
            let silence_after_seek =
                render_and_collect(&mut player, window_blocks, &mut iteration_samples);
            let pos_after_seek = player.position();

            assert!(
                silence_after_seek <= Consts::MAX_SILENCE_BLOCKS,
                "[iter {iter}] {url} after seek→{seek_target:.1}s: silence run = \
                 {silence_after_seek} blocks (max {} allowed) — HANG: decoder \
                 never produced chunks after seek",
                Consts::MAX_SILENCE_BLOCKS,
            );
            assert!(
                pos_after_seek - pos_before_after_seek >= Consts::MIN_POSITION_ADVANCE_SECS,
                "[iter {iter}] {url} after seek→{seek_target:.1}s: position advanced \
                 only {:.2}s (expected ≥{:.2}s) — HANG: playback frozen post-seek",
                pos_after_seek - pos_before_after_seek,
                Consts::MIN_POSITION_ADVANCE_SECS,
            );
        }

        // Dump the full iteration's output for post-mortem inspection.
        let wav_path =
            std::env::temp_dir().join(format!("kithara_silvercomet_seek_iter_{iter}.wav"));
        write_wav_f32(
            &wav_path,
            &iteration_samples,
            Consts::SAMPLE_RATE,
            Consts::CHANNELS,
        );
        eprintln!(
            "[iter {iter}] wrote {} samples ({:.1}s stereo) to {}",
            iteration_samples.len(),
            iteration_samples.len() as f64
                / (f64::from(Consts::SAMPLE_RATE) * f64::from(Consts::CHANNELS)),
            wav_path.display(),
        );

        let global_rms = rms(&iteration_samples);
        assert!(
            global_rms >= Consts::MIN_WAV_RMS,
            "[iter {iter}] captured WAV RMS = {global_rms:.4} (min {:.4} required) — \
             at least one track rendered pure silence for its entire window",
            Consts::MIN_WAV_RMS,
        );

        drop(player);
        drop(downloader);
        // temp_dir::TestTempDir drops here → cache wiped before the next
        // iteration's Resource::new builds its Downloader + Stream.
        drop(temp);
        // Arc::strong_count(&()) unused; keeps iter_label alive until now.
        let _ = iter_label;
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn rms_of_silence_is_zero() {
        let silence = vec![0.0_f32; 1024];
        assert!(rms(&silence).abs() < f32::EPSILON);
    }

    #[test]
    fn rms_of_unit_signal_is_one() {
        let signal: Vec<f32> = (0..1024)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        assert!((rms(&signal) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn blocks_for_three_seconds_matches_expected() {
        let blocks = blocks_for_seconds(3.0);
        // 3 s × 44100 / 512 = 258.4 → ceil 259
        assert_eq!(blocks, 259);
    }
}
