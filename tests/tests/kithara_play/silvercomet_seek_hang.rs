#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{fs::File, io::Write, path::Path};

use kithara::{
    assets::StoreOptions,
    events::AbrMode,
    net::{HttpClient, NetOptions},
    play::{Resource, ResourceConfig},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_decode::DecoderBackend;
use kithara_integration_tests::offline::OfflinePlayer;
use kithara_platform::{
    thread,
    time::{Duration, Instant},
};
use kithara_test_utils::temp_dir;

use crate::common::test_defaults::Consts as Shared;

struct Consts;
impl Consts {
    const SAMPLE_RATE: u32 = Shared::SAMPLE_RATE;
    const CHANNELS: u16 = Shared::CHANNELS;
    const BLOCK_FRAMES: usize = Shared::OFFLINE_BLOCK_FRAMES;
    const PLAY_WINDOW_SECS: f64 = 3.0;
    /// Render warmup burned before the first measurement window so
    /// decoder startup silence (the few hundred ms between `load_and_fadein`
    /// and the first produced PCM chunk under HLS segment fetch latency)
    /// doesn't get miscounted as a hang.
    const WARMUP_SECS: f64 = 2.0;
    /// Fraction of a window allowed to be silent. A real "playing" window
    /// keeps this well below 20%; a phantom-playback hang (decoder emits
    /// chunks full of zeros, or occasional clicks between long silent runs)
    /// pushes it above.
    const MAX_SILENCE_FRACTION: f32 = 0.20;
    /// Per-window RMS floor. Real music against a full window sits at
    /// ~0.1-0.3; isolated clicks between long silences produce <0.02.
    /// 0.03 cleanly separates "music is playing" from "decoder only
    /// emitted a couple of click-frames".
    const MIN_WINDOW_RMS: f32 = 0.03;
    const ITERATIONS: usize = 10;
}

const SILVERCOMET_URLS: &[&str] = &["https://stream.silvercomet.top/hls/master.m3u8"];

/// Outcome of rendering one measurement window.
struct WindowStats {
    /// Number of blocks whose peak amplitude was below `ACTIVE_THRESHOLD`.
    /// Counts **every** silent block in the window, not just the longest run —
    /// phantom playback (clicks between long silences) drives this up.
    silent_blocks: u32,
    /// Total rendered blocks in the window.
    total_blocks: u32,
    /// Start index (in samples) into `samples_out` where this window begins.
    /// Lets callers slice out just the after-seek portion to compute a
    /// window-local RMS.
    window_start_sample: usize,
}

/// Render `blocks` audio blocks, collect the raw interleaved samples,
/// and return statistics covering exactly this window.
fn render_and_collect(
    player: &mut OfflinePlayer,
    blocks: u32,
    samples_out: &mut Vec<f32>,
) -> WindowStats {
    const ACTIVE_THRESHOLD: f32 = 0.001;
    let block_budget =
        Duration::from_secs_f64(Consts::BLOCK_FRAMES as f64 / f64::from(Consts::SAMPLE_RATE));

    let window_start_sample = samples_out.len();
    let mut silent_blocks = 0u32;

    for _ in 0..blocks {
        let started = Instant::now();
        let out = player.render(Consts::BLOCK_FRAMES);
        let elapsed = started.elapsed();

        if !out.iter().any(|s| s.abs() > ACTIVE_THRESHOLD) {
            silent_blocks += 1;
        }

        samples_out.extend_from_slice(&out);
        thread::sleep(block_budget.saturating_sub(elapsed));
    }

    WindowStats {
        silent_blocks,
        total_blocks: blocks,
        window_start_sample,
    }
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
    let net = NetOptions::default().with_is_insecure(true);
    Downloader::new(DownloaderConfig::default().with_client(HttpClient::new(net)))
}

async fn build_resource(
    url: &str,
    downloader: &Downloader,
    iter_label: &str,
    store: StoreOptions,
    backend: DecoderBackend,
    abr: AbrMode,
) -> Resource {
    let mut cfg =
        ResourceConfig::new(url).unwrap_or_else(|e| panic!("ResourceConfig::new({url}): {e}"));
    cfg = cfg
        .with_downloader(downloader.clone())
        .with_name(format!("{iter_label}|{url}"));
    cfg.store = store;
    cfg.decoder_backend = backend;
    cfg.initial_abr_mode = abr;
    let mut resource = Resource::new(cfg)
        .await
        .unwrap_or_else(|e| panic!("Resource::new({url}): {e:?}"));
    tokio::time::timeout(Duration::from_secs(15), resource.preload())
        .await
        .unwrap_or_else(|_| panic!("Resource::preload({url}) timed out after 15s"))
        .unwrap_or_else(|err| panic!("Resource::preload({url}) failed: {err}"));
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

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(600)))]
#[case::symphonia_auto(DecoderBackend::Symphonia, AbrMode::Auto(None))]
#[case::symphonia_locked_low(DecoderBackend::Symphonia, AbrMode::Manual(0))]
#[case::symphonia_locked_high(DecoderBackend::Symphonia, AbrMode::Manual(2))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_auto(DecoderBackend::Apple, AbrMode::Auto(None))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_low(DecoderBackend::Apple, AbrMode::Manual(0))
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple_locked_high(DecoderBackend::Apple, AbrMode::Manual(2))
)]
#[cfg_attr(
    target_os = "android",
    case::android(DecoderBackend::Android, AbrMode::Auto(None))
)]
#[ignore = "real network to silvercomet.top; run with --run-ignored only"]
async fn silvercomet_3tracks_seek_middle_hang_10x(
    #[case] backend: DecoderBackend,
    #[case] abr: AbrMode,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

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
            let resource =
                build_resource(url, &downloader, &iter_label, store.clone(), backend, abr).await;
            eprintln!("[iter {iter}][t{track_idx}] resource built, load_and_fadein");
            player.load_and_fadein(resource, &format!("{iter_label}|t{track_idx}"));

            eprintln!("[iter {iter}][t{track_idx}] warmup ({warmup_blocks} blocks)");
            let _ = render_and_collect(&mut player, warmup_blocks, &mut iteration_samples);
            eprintln!(
                "[iter {iter}][t{track_idx}] warmup done, position={:.2}",
                player.position()
            );

            let initial = render_and_collect(&mut player, window_blocks, &mut iteration_samples);
            let initial_samples = &iteration_samples[initial.window_start_sample..];
            let initial_rms = rms(initial_samples);
            let initial_silence_fraction =
                f32::from(u16::try_from(initial.silent_blocks).unwrap_or(u16::MAX))
                    / f32::from(
                        u16::try_from(initial.total_blocks)
                            .unwrap_or(u16::MAX)
                            .max(1),
                    );
            eprintln!(
                "[iter {iter}][t{track_idx}] initial window: silent={}/{} ({:.1}%) rms={:.4}",
                initial.silent_blocks,
                initial.total_blocks,
                initial_silence_fraction * 100.0,
                initial_rms,
            );

            assert!(
                initial_silence_fraction <= Consts::MAX_SILENCE_FRACTION,
                "[iter {iter}] {url}: initial-play silent fraction = {:.1}% \
                 (max {:.0}% allowed) — track produced mostly silence before seek",
                initial_silence_fraction * 100.0,
                Consts::MAX_SILENCE_FRACTION * 100.0,
            );
            assert!(
                initial_rms >= Consts::MIN_WINDOW_RMS,
                "[iter {iter}] {url}: initial-play window RMS = {initial_rms:.4} \
                 (min {:.4} required) — no real audio in the pre-seek window",
                Consts::MIN_WINDOW_RMS,
            );

            let seek_target = player.position() + 30.0;
            let seek_epoch = next_seek_epoch;
            next_seek_epoch += 1;
            eprintln!("[iter {iter}][t{track_idx}] seek to {seek_target:.2}s epoch={seek_epoch}");
            player.seek(seek_target, seek_epoch);

            let after = render_and_collect(&mut player, window_blocks, &mut iteration_samples);
            let after_samples = &iteration_samples[after.window_start_sample..];
            let after_rms = rms(after_samples);
            let after_silence_fraction =
                f32::from(u16::try_from(after.silent_blocks).unwrap_or(u16::MAX))
                    / f32::from(u16::try_from(after.total_blocks).unwrap_or(u16::MAX).max(1));
            eprintln!(
                "[iter {iter}][t{track_idx}] after-seek window: silent={}/{} ({:.1}%) rms={:.4}",
                after.silent_blocks,
                after.total_blocks,
                after_silence_fraction * 100.0,
                after_rms,
            );

            assert!(
                after_silence_fraction <= Consts::MAX_SILENCE_FRACTION,
                "[iter {iter}] {url} after seek→{seek_target:.1}s: silent fraction = \
                 {:.1}% (max {:.0}% allowed) — HANG: decoder produced mostly \
                 silence after seek",
                after_silence_fraction * 100.0,
                Consts::MAX_SILENCE_FRACTION * 100.0,
            );
            assert!(
                after_rms >= Consts::MIN_WINDOW_RMS,
                "[iter {iter}] {url} after seek→{seek_target:.1}s: window RMS = \
                 {after_rms:.4} (min {:.4} required) — HANG: post-seek window \
                 has no real audio, only silence or isolated clicks",
                Consts::MIN_WINDOW_RMS,
            );
        }

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

        drop(player);
        drop(downloader);
        drop(temp);
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
        assert_eq!(blocks, 259);
    }
}
