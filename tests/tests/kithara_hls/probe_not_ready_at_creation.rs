#![cfg(not(target_arch = "wasm32"))]

//! Deterministic repro for flake F2 (`Audio::new() -> Err(Interrupted)` at
//! creation under load): the construction-time decoder probe reads the
//! container header through the source's non-blocking single-probe `Read`, so
//! any byte it touches that has not downloaded yet surfaces immediately as the
//! transient `Interrupted` retry-signal. At construction there is no decode
//! loop to park and re-tick, so the signal propagated as a FATAL
//! `DecodeError::Interrupted` (`is_interrupted() == true`) — the
//! `.expect("audio creation")` in `live_real_stream_seek_resume_native_drm`
//! and the cap=1 hot-refetch repro both panicked on it under CPU contention,
//! before any seek/playback ran.
//!
//! Contract (post F2-fix-of-fix, Option A): `Audio::new` runs a deterministic
//! init-range readiness gate before the single decoder build. The gate awaits
//! the exact range the probe reads (`0..max(init_size, probe_buffer)`), waking
//! the downloader peer, bounded by the construction probe budget. So:
//! - The transient `Interrupted` retry-signal is NEVER surfaced as the
//!   creation error (`is_interrupted() == false` always).
//! - If the init bytes genuinely never arrive, the gate surfaces a TYPED
//!   terminal ("init not available") — NOT the cooperative-retry `Interrupted`
//!   signal a caller `.expect()`s away, and NOT the prior synthetic
//!   `Io(TimedOut)` that conflated "slow" with "broken".
//! - If the bytes arrive (slow start), the gate resolves and the build
//!   succeeds.
//!
//! Determinism: no `sleep`, no real-time pacing, no CPU-load dependence. The
//! `HlsTestServer` segment gate withholds segment 0's BODY while its size
//! (HEAD) stays known, so up-front size estimation still completes at
//! construction but the decoder probe's read window (which spills past the
//! 44-byte WAV init into the withheld body) reads not-ready data — exactly the
//! race the load flake hits non-deterministically.

use std::{num::NonZeroUsize, sync::Arc};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    hls::{Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    TestTempDir, auto,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_header,
};
use kithara_platform::{
    CancellationToken,
    time::{Duration, Instant},
};
use tracing::info;

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const SEGMENT_SIZE: usize = 32_768;
const SEGMENT_COUNT: usize = 8;

fn fixture_config() -> HlsTestServerConfig {
    let init_segment = Arc::new(create_wav_header(SAMPLE_RATE, CHANNELS, None));
    let pcm = Arc::new(
        SignalPcm::new(
            signal::Sawtooth,
            SAMPLE_RATE,
            CHANNELS,
            Finite::from_segments(SEGMENT_COUNT, SEGMENT_SIZE, CHANNELS),
        )
        .into_vec(),
    );
    let segment_duration = SEGMENT_SIZE as f64
        / (f64::from(SAMPLE_RATE) * f64::from(CHANNELS) * size_of::<i16>() as f64);
    HlsTestServerConfig {
        variant_count: 1,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: SEGMENT_SIZE,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![pcm]),
        init_data_per_variant: Some(vec![init_segment]),
        variant_bandwidths: Some(vec![1_000_000]),
        ..Default::default()
    }
}

fn audio_config(server: &HlsTestServer, temp_dir: &TestTempDir) -> AudioConfig<Hls> {
    let store = StoreOptions::builder()
        .cache_dir(temp_dir.path().into())
        .is_ephemeral(true)
        .cache_capacity(NonZeroUsize::new(8).expect("nonzero"))
        .build();
    let hls_config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(store)
        .cancel(CancellationToken::default())
        // auto(0) mirrors the F2 members (live_real_stream / hot_refetch).
        .initial_abr_mode(auto(0))
        .build();
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    AudioConfig::<Hls>::for_stream(hls_config)
        .media_info(wav_info)
        .build()
}

/// The first segment's body never arrives (the WAV init's 44 bytes are open,
/// but the probe window spills past them into the withheld body). The
/// init-range gate spins its bounded budget and then `Audio::new` surfaces a
/// TYPED terminal error — never the transient `Interrupted` retry-signal that
/// callers `.expect()` away, and never the prior synthetic `Io(TimedOut)`.
/// RED before the F2 fix: the one-shot decoder probe returned
/// `DecodeError::Interrupted` (`is_interrupted() == true`). RED before the
/// fix-of-fix: the gate-exhaustion path re-mapped to a synthetic
/// `Io(TimedOut)`.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
async fn audio_new_never_surfaces_interrupted_when_first_segment_withheld() {
    // Withhold segment 0's BODY for the lifetime of the test; HEAD stays open
    // so size estimation completes at construction.
    let (server, _gate) = HlsTestServer::with_segment_gate(fixture_config(), 0, 0).await;
    let temp_dir = TestTempDir::new();

    let started = Instant::now();
    let result = Audio::<Stream<Hls>>::new(audio_config(&server, &temp_dir)).await;
    let elapsed = started.elapsed();

    let err = result
        .err()
        .expect("withheld init body must fail Audio::new, not succeed");
    let message = err.to_string();
    info!(?elapsed, %message, is_interrupted = err.is_interrupted(), "Audio::new failed");

    assert!(
        !err.is_interrupted(),
        "Audio::new surfaced the transient `Interrupted` retry-signal as its \
         creation error ({message}) — the not-ready init read must become a \
         typed terminal error, never the cooperative-retry signal a caller \
         `.expect()`s away (flake F2: live_real_stream_seek_resume_native_drm / \
         red_flaky_small_cache_hot_refetch_behind_reader)"
    );
    // Typed terminal contract: the gate surfaces an "init range did not become
    // ready" / "init range was readable" message, not a synthetic timeout.
    assert!(
        message.contains("init range"),
        "Audio::new must surface the typed init-not-available terminal, got: {message}"
    );
    assert!(
        !message.to_ascii_lowercase().contains("timed out")
            && !message.to_ascii_lowercase().contains("timeout"),
        "Audio::new must NOT surface a synthetic TimedOut — that conflates slow with broken: {message}"
    );
}

/// Happy path under the same gate: once the withheld body is released (modelling
/// a slow-but-arriving first segment), `Audio::new` succeeds. Guards that the
/// warm-the-window fix did not turn a recoverable slow start into a hard failure.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
async fn audio_new_succeeds_when_first_segment_released_during_probe() {
    let (server, gate) = HlsTestServer::with_segment_gate(fixture_config(), 0, 0).await;
    let temp_dir = TestTempDir::new();

    // Release the body as soon as its GET reaches the gate (no timer): the
    // construction probe must then await the data and build the decoder.
    let release_gate = gate.clone();
    let releaser = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if release_gate.requested() > 0 {
                release_gate.release();
                return;
            }
            assert!(
                Instant::now() < deadline,
                "withheld GET never reached the gate"
            );
            tokio::task::yield_now().await;
        }
    });

    let result = Audio::<Stream<Hls>>::new(audio_config(&server, &temp_dir)).await;
    releaser.await.expect("releaser joins");

    assert!(
        result.is_ok(),
        "Audio::new must succeed once the slow first segment arrives, got {:?}",
        result.err().map(|e| e.to_string())
    );
}
