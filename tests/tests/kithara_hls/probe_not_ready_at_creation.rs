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
//! Root fix (byte-stream layer): the cause is NOT in the audio layer, and the
//! fix is NOT a retry loop or readiness gate. Decoder construction reads
//! through the BLOCKING off-RT `Stream::read` adapter instead of the
//! non-blocking RT `probe_read`. The blocking adapter waits — off the RT
//! worker, waking the peer downloader — for the bytes the build touches, so a
//! late-but-arriving prefix no longer surfaces as a fatal `Interrupted`. The
//! audio band-aids (`InitNotReady`, `gate_init_range`, the rebuild loop) are
//! DELETED. (An earlier attempt additionally awaited an init-body prefetch in
//! `Hls::create`; that blocked startup — regressing the gapless "do not block
//! network startup" contract and a variant-switch recreate — and was redundant
//! with the blocking read, so it was removed.)
//! Contract:
//! - If the bytes arrive (slow start), the blocking read waits for them and
//!   the single build succeeds — this is the production flake's fix (bytes
//!   were arriving, just late under CPU load).
//! - If a construction-range byte genuinely never arrives, `Audio::new` FAILS
//!   bounded by the blocking read budget (never a hang), surfacing the STREAM
//!   layer's typed pending payload verbatim — never an audio-minted error type
//!   and never the earlier fix-of-fix's synthetic `Io(TimedOut)`.
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
    net::{NetOptions, RetryPolicy},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    TestTempDir, auto,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_header,
};
use kithara_platform::{
    CancelToken,
    time::{Duration, Instant},
    tokio,
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
    // Short stall + bounded retries so a withheld body settles the segment
    // terminally (the net resilient body owns the stall) well within the
    // 5s assertion: 3 stalls × 400ms + backoffs ≈ 1.3s, never a hang. The
    // happy-path sibling releases the body before the first stall, so this
    // does not perturb it. Real timers, real HTTP — no fake transport.
    let net = NetOptions::builder()
        .inactivity_timeout(Duration::from_millis(400))
        .retry_policy(
            RetryPolicy::builder()
                .max_retries(2)
                .base_delay(Duration::from_millis(20))
                .max_delay(Duration::from_millis(100))
                .build(),
        )
        .build();
    let hls_config = HlsConfig::for_url(server.url("/master.m3u8"))
        .store(store)
        .cancel(CancelToken::never())
        // auto(0) mirrors the F2 members (live_real_stream / hot_refetch).
        .initial_abr_mode(auto(0))
        .net_options(net)
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
///
/// Post network-layer fix (Option A): the active variant's init body is
/// prefetched-and-committed by `Hls::create`, and decoder construction reads
/// through the BLOCKING off-RT `Stream::read` adapter. So a construction-range
/// byte that never arrives makes `Audio::new` FAIL — bounded by the blocking
/// read budget (never a hang to the test timeout), surfacing the STREAM
/// layer's typed pending payload verbatim — and is NEVER masked by an
/// audio-minted error type or a synthetic `TimedOut`. The band-aid audio gate
/// (`InitNotReady`, `gate_init_range`, the rebuild loop) is gone; the typed
/// terminal comes from the stream layer, not the audio layer.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    tracing("kithara_audio=info,kithara_hls=info,kithara_stream=info")
)]
async fn audio_new_bounded_failure_when_first_segment_withheld() {
    // Withhold segment 0's BODY for the lifetime of the test; HEAD stays open
    // so size estimation completes at construction. The WAV init (a separate
    // resource) is reachable, so the `Hls::create` init prefetch commits; the
    // decoder probe's read window then spills past the init into the withheld
    // body, which the blocking read waits the full budget for and then fails.
    let (server, _gate) = HlsTestServer::with_segment_gate(fixture_config(), 0, 0).await;
    let temp_dir = TestTempDir::new();

    let started = Instant::now();
    let result = Audio::<Stream<Hls>>::new(audio_config(&server, &temp_dir)).await;
    let elapsed = started.elapsed();

    let err = result
        .err()
        .expect("withheld first-segment body must fail Audio::new, not succeed");
    let message = err.to_string();
    info!(?elapsed, %message, is_interrupted = err.is_interrupted(), "Audio::new failed");

    // Bounded: the blocking read budget caps construction; a never-arriving
    // range must NOT hang to the test timeout (the load flake's symptom).
    assert!(
        elapsed < Duration::from_secs(5),
        "Audio::new must fail bounded by the blocking read budget, not hang \
         ({elapsed:?})"
    );
    // The terminal is the stream layer's typed pending payload (the real
    // reason — "data not ready" / "wait budget" — snapshotted at the wrap),
    // surfaced verbatim through the blocking read. NOT minted in the audio
    // layer (the deleted band-aid `InitNotReady` types).
    let lower = message.to_ascii_lowercase();
    assert!(
        lower.contains("not ready") || lower.contains("wait budget"),
        "Audio::new must surface the stream layer's typed pending payload, got: {message}"
    );
    assert!(
        !message.contains("init range"),
        "Audio::new must NOT surface the deleted audio-layer init-range gate \
         terminal — the typed terminal comes from the stream layer: {message}"
    );
    // Never a synthetic `TimedOut` (the earlier fix-of-fix's mistake) — that
    // conflates slow with broken.
    assert!(
        !lower.contains("timed out") && !lower.contains("timeout"),
        "Audio::new must NOT surface a synthetic TimedOut: {message}"
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
    let releaser = tokio::task::spawn(async move {
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
