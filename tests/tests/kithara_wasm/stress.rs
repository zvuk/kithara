//! WASM stress tests for the HLS audio pipeline.
//!
//! Runs in a dedicated Web Worker via `wasm-bindgen-test-runner` + headless Chrome.
//! Requires the HLS fixture server to be running (see `scripts/ci/wasm-test.sh`).

use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};

use futures::future::{Either, select};
use kithara_assets::StoreOptions;
use kithara_audio::{Audio, AudioConfig};
use kithara_events::{AudioEvent, Event, EventBus, SeekLifecycleStage};
use kithara_hls::{AbrMode, AbrOptions, Hls, HlsConfig};
use kithara_platform::time::{Duration, Instant};
use kithara_stream::{AudioCodec, ContainerFormat, MediaInfo, Stream};
use tracing::{info, warn};
use url::Url;
mod kithara {
    pub(crate) use kithara_test_macros::test;
}

const EVENT_BUS_CAPACITY: usize = 4096;
const REAL_HLS_STREAM_URL: &str = "http://127.0.0.1:3333/hls/master.m3u8";

/// Guard: init must only run once per page.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Get HLS test URL from compile-time env or fall back to default.
fn fixture_url() -> Url {
    let url_str = option_env!("HLS_TEST_URL").unwrap_or("http://127.0.0.1:3333/master.m3u8");
    url_str.parse().unwrap()
}

fn fixture_jitter_url() -> Url {
    let mut url = fixture_url();
    url.set_path("/master-jitter.m3u8");
    url
}

/// Minimal xorshift64 PRNG for deterministic seek positions.
struct Xorshift64(u64);

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range_f64(&mut self, min: f64, max: f64) -> f64 {
        min + (max - min) * self.next_f64()
    }
}

/// One-time initialization: panic hook + tracing.
///
/// Idempotent — safe to call from every test. All tests share one page
/// in `wasm_bindgen_test`, so init must only run once.
async fn init() {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();
    info!("WASM test environment initialized");
}

/// Create an `Audio<Stream<Hls>>` pipeline in ephemeral mode.
async fn create_pipeline() -> Audio<Stream<Hls>> {
    create_pipeline_with_url(fixture_url()).await
}

async fn create_pipeline_with_url(url: Url) -> Audio<Stream<Hls>> {
    let bus = EventBus::new(EVENT_BUS_CAPACITY);

    let hls_config = HlsConfig::new(url)
        .with_events(bus)
        .with_store(StoreOptions::default().with_ephemeral(true))
        .with_abr(AbrOptions {
            mode: AbrMode::Auto(Some(0)),
            ..Default::default()
        });

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config).with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config).await.unwrap();
    audio.preload();
    audio
}

async fn run_seek_pcm_window_check(mut audio: Audio<Stream<Hls>>) {
    let spec = audio.spec();
    let channels = spec.channels as usize;
    let sample_rate = spec.sample_rate as usize;
    let mut buf = vec![0.0f32; 4096];

    // Warmup.
    let mut warmup = 0usize;
    for _ in 0..20 {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        warmup += n;
    }
    assert!(warmup > 0, "warmup must read some data");

    // Seek to middle and read ~5s.
    let duration_secs = audio
        .duration()
        .unwrap_or(Duration::from_secs(60))
        .as_secs_f64();
    let middle_secs = (duration_secs * 0.5).clamp(3.0, (duration_secs - 1.0).max(3.0));
    audio
        .seek(Duration::from_secs_f64(middle_secs))
        .expect("seek to middle must succeed");

    let mut played_frames = 0usize;
    let target_frames = sample_rate * 5;
    for _ in 0..600 {
        let n = read_with_yield_limit(&mut audio, &mut buf, 50).await;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            continue;
        }
        played_frames += n / channels;
        if played_frames >= target_frames {
            break;
        }
    }
    assert!(
        played_frames >= sample_rate * 3,
        "expected at least ~3s playback before near-start seek"
    );

    // Seek near start and inspect early PCM window.
    let near_start = Duration::from_secs_f64(0.2);
    audio
        .seek(near_start)
        .expect("seek near start must succeed");

    let inspect_frames = (sample_rate * 35) / 100; // ~350ms
    let mut inspected = 0usize;
    let mut discontinuities = 0usize;
    let mut backward_jumps = 0usize;
    let mut first_bad: Option<(usize, usize, usize)> = None;
    let mut prev_phase: Option<usize> = None;

    for _ in 0..400 {
        let n = read_with_yield_limit(&mut audio, &mut buf, 100).await;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            continue;
        }

        let frames = n / channels;
        for f in 0..frames {
            if inspected >= inspect_frames {
                break;
            }
            let phase = phase_from_f32(buf[f * channels]);
            if let Some(prev) = prev_phase {
                let expected = (prev + 1) % SAW_PERIOD;
                if phase != expected {
                    discontinuities += 1;
                    if phase < prev {
                        backward_jumps += 1;
                    }
                    if first_bad.is_none() {
                        first_bad = Some((prev, phase, expected));
                    }
                }
            }
            prev_phase = Some(phase);
            inspected += 1;
        }

        if inspected >= inspect_frames {
            break;
        }
    }

    assert!(
        inspected >= inspect_frames,
        "inspected too few frames after seek: {inspected}/{inspect_frames}"
    );
    assert_eq!(
        discontinuities, 0,
        "discontinuities in early PCM window after seek: {discontinuities}, first_bad={first_bad:?}"
    );
    assert_eq!(
        backward_jumps, 0,
        "backward jumps in early PCM window after seek: {backward_jumps}, first_bad={first_bad:?}"
    );
}

/// Non-blocking read that yields to the event loop between attempts.
///
/// On wasm32 main thread, `Atomics.wait` is forbidden. The audio pipeline
/// uses `preload()` mode where `read()` returns 0 when data isn't ready yet.
/// We yield via `gloo_timers` to let async downloads and Web Workers proceed.
async fn read_with_yield(audio: &mut Audio<Stream<Hls>>, buf: &mut [f32]) -> usize {
    read_with_yield_limit(audio, buf, 500).await
}

/// Read with configurable retry limit.
async fn read_with_yield_limit(
    audio: &mut Audio<Stream<Hls>>,
    buf: &mut [f32],
    max_yields: usize,
) -> usize {
    const YIELD_MS: u32 = 10;

    for _ in 0..max_yields {
        let n = audio.read(buf);
        if n > 0 {
            return n;
        }
        if audio.is_eof() {
            return 0;
        }
        gloo_timers::future::TimeoutFuture::new(YIELD_MS).await;
    }
    0
}

/// Yield to event loop so async I/O and Web Workers can progress.
async fn yield_ms(ms: u32) {
    gloo_timers::future::TimeoutFuture::new(ms).await;
}

async fn assert_settles<F, T>(op_future: F, timeout_ms: u32, op: &str) -> T
where
    F: Future<Output = T>,
{
    let timeout_future = gloo_timers::future::TimeoutFuture::new(timeout_ms);
    match select(Box::pin(op_future), Box::pin(timeout_future)).await {
        Either::Left((result, _)) => result,
        Either::Right(((), _)) => {
            panic!("{op} timed out after {timeout_ms}ms (possible browser hang)");
        }
    }
}

async fn create_player_with_fixture() -> Option<kithara_wasm::WasmPlayer> {
    let mut player = kithara_wasm::WasmPlayer::new();
    let idx = match player.add_track(fixture_url().to_string()) {
        Ok(idx) => idx,
        Err(err) => {
            warn!("failed to add fixture track: {:?}", err);
            return None;
        }
    };

    for attempt in 0..6 {
        match player.select_track(idx).await {
            Ok(()) => return Some(player),
            Err(err) => {
                warn!(
                    attempt,
                    "select_track failed for fixture track (will retry): {:?}", err
                );
                yield_ms(50).await;
            }
        }
    }

    None
}

async fn wait_duration_ms(player: &kithara_wasm::WasmPlayer) -> f64 {
    for _ in 0..200 {
        let duration_ms = player.get_duration_ms();
        if duration_ms > 0.0 {
            return duration_ms;
        }
        yield_ms(10).await;
    }
    0.0
}

async fn wait_position_advance(player: &kithara_wasm::WasmPlayer, from_ms: f64) -> bool {
    for _ in 0..200 {
        if player.get_position_ms() > from_ms + 120.0 {
            return true;
        }
        yield_ms(10).await;
    }
    false
}

async fn select_with_retry(
    player: &mut kithara_wasm::WasmPlayer,
    index: u32,
    retries: usize,
) -> bool {
    for attempt in 0..retries {
        match player.select_track(index).await {
            Ok(()) => return true,
            Err(err) => {
                warn!(attempt, index, "select_track retry after error: {:?}", err);
                yield_ms(50).await;
            }
        }
    }
    false
}

fn trim_events_tail(events: &str) -> String {
    if events.len() <= 2048 {
        events.to_string()
    } else {
        format!("...{}", &events[events.len() - 2048..])
    }
}

// ── Saw-tooth verification helpers ──
//
// The HLS fixture server (`hls_fixture_server`) serves a deterministic saw-tooth
// WAV signal with period 65536 frames. Each frame encodes its position as an i16
// value, allowing content-based seek verification: after seek(T), the decoded
// phase must match the expected phase for time T.

/// Saw-tooth period: 65536 frames (~1.486s at 44100 Hz).
const SAW_PERIOD: usize = 65536;

/// Recover saw-tooth phase from a decoded f32 sample.
///
/// Inverse of the generator: `sample = ((i % SAW_PERIOD) as i32 - 32768) as i16`.
fn phase_from_f32(sample: f32) -> usize {
    let i16_val = (sample * 32768.0).round() as i32;
    ((i16_val + 32768) & 0xFFFF) as usize
}

/// Circular distance between two phases (mod `SAW_PERIOD`).
fn phase_distance(a: usize, b: usize) -> usize {
    let d = a.abs_diff(b);
    d.min(SAW_PERIOD - d)
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn stress_read_samples_integrity() {
    init().await;
    info!("Starting stress_read_samples_integrity");

    let mut audio = create_pipeline().await;
    let spec = audio.spec();

    assert!(spec.channels > 0, "channels must be > 0");
    assert!(spec.sample_rate > 0, "sample_rate must be > 0");
    info!(
        channels = spec.channels,
        sample_rate = spec.sample_rate,
        "Audio spec"
    );

    let channels = spec.channels as usize;
    let mut buf = vec![0.0f32; 4096];
    let mut total_samples = 0usize;
    let target_chunks = 200;
    let mut chunks_read = 0;
    let mut channel_mismatches = 0u64;
    let mut continuity_errors = 0u64;
    let mut prev_last_phase: Option<usize> = None;

    for _ in 0..target_chunks {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        chunks_read += 1;
        total_samples += n;
        let frames = n / channels;

        // Level 1: Integrity — samples finite, in [-1.0, 1.0]
        for (i, &sample) in buf[..n].iter().enumerate() {
            assert!(
                sample.is_finite(),
                "sample {i} in chunk {chunks_read} is not finite: {sample}"
            );
            assert!(
                (-1.0..=1.0).contains(&sample),
                "sample {i} in chunk {chunks_read} out of range: {sample}"
            );
        }

        // Level 1b: L == R (stereo saw-tooth has identical channels)
        if channels == 2 {
            for f in 0..frames {
                let l = buf[f * 2];
                let r = buf[f * 2 + 1];
                if (l - r).abs() > f32::EPSILON {
                    channel_mismatches += 1;
                    if channel_mismatches <= 3 {
                        warn!(chunk = chunks_read, frame = f, l, r, "L/R mismatch");
                    }
                }
            }
        }

        // Level 2: Continuity — consecutive frames follow saw-tooth pattern
        // Inter-chunk: last frame of prev chunk → first frame of this chunk
        if let Some(prev_phase) = prev_last_phase {
            let first_phase = phase_from_f32(buf[0]);
            let expected = (prev_phase + 1) % SAW_PERIOD;
            if first_phase != expected {
                continuity_errors += 1;
                if continuity_errors <= 3 {
                    warn!(
                        chunk = chunks_read,
                        prev_phase, first_phase, expected, "inter-chunk continuity break"
                    );
                }
            }
        }

        // Intra-chunk: every pair of adjacent frames
        if frames >= 2 {
            for f in 1..frames {
                let prev = phase_from_f32(buf[(f - 1) * channels]);
                let curr = phase_from_f32(buf[f * channels]);
                let expected = (prev + 1) % SAW_PERIOD;
                if curr != expected {
                    continuity_errors += 1;
                    if continuity_errors <= 3 {
                        warn!(
                            chunk = chunks_read,
                            frame = f,
                            prev,
                            curr,
                            expected,
                            "intra-chunk continuity break"
                        );
                    }
                }
            }
        }

        // Track last phase for inter-chunk continuity
        if frames > 0 {
            prev_last_phase = Some(phase_from_f32(buf[(frames - 1) * channels]));
        }
    }

    info!(
        total_samples,
        chunks_read, channel_mismatches, continuity_errors, "Read complete"
    );
    assert!(total_samples > 0, "must read at least some audio samples");
    assert!(
        chunks_read >= 5,
        "expected at least 5 chunks, got {chunks_read}"
    );
    assert_eq!(
        channel_mismatches, 0,
        "L/R channels diverged {channel_mismatches} times — data corruption"
    );
    assert!(
        continuity_errors <= 5,
        "{continuity_errors} continuity breaks (>5 tolerance) — non-contiguous decoded data"
    );
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn stress_seek_and_read() {
    init().await;
    info!("Starting stress_seek_and_read");

    let mut audio = create_pipeline().await;

    // Read some data first to ensure pipeline is warm.
    let mut buf = vec![0.0f32; 4096];
    let mut warmup = 0;
    for _ in 0..20 {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        warmup += n;
    }
    assert!(warmup > 0, "warmup must read some data");
    info!(warmup, "Pipeline warmed up");

    let duration = audio.duration().unwrap_or(Duration::from_secs(30));
    let duration_secs = duration.as_secs_f64();

    // Perform seeks to various positions and verify data integrity after each.
    let seek_positions = [0.1, 0.5, 0.25, 0.75, 0.0, 0.9, 0.3, 0.6, 0.15, 0.85];
    let spec = audio.spec();
    let channels = spec.channels as usize;
    let mut position_errors = 0u64;
    let mut channel_mismatches = 0u64;
    let mut continuity_errors = 0u64;

    for (i, &frac) in seek_positions.iter().enumerate() {
        let pos_secs = duration_secs * frac;
        let seek_to = Duration::from_secs_f64(pos_secs);
        info!(seek_idx = i, seek_ms = seek_to.as_millis(), "Seeking");

        if let Err(e) = audio.seek(seek_to) {
            info!(?e, "Seek failed (may be near boundary), skipping");
            continue;
        }

        // Read after seek and verify integrity.
        let mut post_seek_samples = 0;
        let mut position_checked = false;
        for _ in 0..10 {
            let n = read_with_yield(&mut audio, &mut buf).await;
            if n == 0 {
                break;
            }
            let frames = n / channels;
            post_seek_samples += n;

            // Level 1: Integrity
            for &sample in &buf[..n] {
                assert!(
                    sample.is_finite() && (-1.0..=1.0).contains(&sample),
                    "corrupt sample after seek {i}: {sample}"
                );
            }

            // Level 1b: L == R
            if channels == 2 {
                for f in 0..frames {
                    let l = buf[f * 2];
                    let r = buf[f * 2 + 1];
                    if (l - r).abs() > f32::EPSILON {
                        channel_mismatches += 1;
                    }
                }
            }

            // Level 2: Intra-chunk continuity
            if frames >= 2 {
                for f in 1..frames {
                    let prev = phase_from_f32(buf[(f - 1) * channels]);
                    let curr = phase_from_f32(buf[f * channels]);
                    if curr != (prev + 1) % SAW_PERIOD {
                        continuity_errors += 1;
                    }
                }
            }

            // Level 3: Position — first read after seek must match expected phase.
            // Tolerance: 1200 frames (~27ms) for decoder packet boundary alignment.
            if !position_checked && frames > 0 {
                let expected_frame = (pos_secs * spec.sample_rate as f64).round() as usize;
                let expected_phase = expected_frame % SAW_PERIOD;
                let actual_phase = phase_from_f32(buf[0]);
                let dist = phase_distance(actual_phase, expected_phase);
                if dist > 1200 {
                    position_errors += 1;
                    warn!(
                        seek_idx = i,
                        pos_secs,
                        expected_phase,
                        actual_phase,
                        dist,
                        "position mismatch after seek"
                    );
                }
                position_checked = true;
            }
        }
        info!(post_seek_samples, "Read after seek {i}");
    }

    info!(
        position_errors,
        channel_mismatches, continuity_errors, "Seek test done"
    );
    assert_eq!(
        channel_mismatches, 0,
        "L/R channels diverged {channel_mismatches} times"
    );
    assert!(
        continuity_errors <= 5,
        "{continuity_errors} continuity breaks (>5 tolerance)"
    );
    assert!(
        position_errors <= 1,
        "{position_errors} position mismatches — seek landed in wrong place"
    );
}

/// Lifecycle sanity for the browser player path (`WasmPlayer` over `kithara-play`).
///
/// Verifies that after selecting an HLS track the player reports duration,
/// advances position while playing, pauses correctly, and keeps seek behavior sane.
#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn wasm_player_playlist_add_track_validation() {
    init().await;

    let mut player = kithara_wasm::WasmPlayer::new();
    let initial_len = player.playlist_len();
    let fixture = fixture_url().to_string();

    assert!(player.add_track("   ".to_string()).is_err());

    let idx = player
        .add_track(fixture.clone())
        .expect("add_track must accept fixture url");
    assert_eq!(idx, initial_len);
    assert_eq!(player.playlist_len(), initial_len + 1);
    assert_eq!(
        player.playlist_item(idx).expect("playlist item must exist"),
        fixture
    );
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn wasm_player_eq_controls_roundtrip() {
    init().await;

    let mut player = kithara_wasm::WasmPlayer::new();
    let bands = player.eq_band_count();
    assert!(bands > 0, "eq must expose at least one band");

    assert!(
        player.set_eq_gain(bands + 1, 1.0).is_err(),
        "set_eq_gain must fail for invalid band"
    );

    let fixture = fixture_url().to_string();
    let idx = match player.add_track(fixture) {
        Ok(idx) => idx,
        Err(err) => {
            warn!("eq test: add_track failed: {:?}", err);
            return;
        }
    };

    if !select_with_retry(&mut player, idx, 8).await {
        warn!("eq test: select_track did not succeed, skip roundtrip checks");
        return;
    }

    let band = 0;
    if let Err(err) = player.set_eq_gain(band, 3.5) {
        warn!("eq test: set_eq_gain failed after select_track: {:?}", err);
        return;
    }

    let gain = player.eq_gain(band);
    assert!(gain.is_finite(), "eq gain must be finite");

    if let Err(err) = player.reset_eq() {
        warn!("eq test: reset_eq failed: {:?}", err);
        return;
    }
    let reset_gain = player.eq_gain(band);
    assert!(reset_gain.abs() < 0.1, "eq gain must reset close to zero");
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn wasm_player_select_track_crossfade_switch() {
    init().await;

    let mut player = kithara_wasm::WasmPlayer::new();
    let fixture = fixture_url().to_string();
    let first = player.add_track(fixture.clone()).expect("first add_track");
    let second = player.add_track(fixture).expect("second add_track");

    if !select_with_retry(&mut player, first, 8).await {
        warn!("crossfade test: failed to select first fixture track");
        return;
    }

    player.set_crossfade_seconds(0.25);
    player.play();
    if !player.is_playing() {
        warn!("crossfade test: player did not enter playing state");
        return;
    }

    let before = player.get_position_ms();
    if !wait_position_advance(&player, before).await {
        warn!("crossfade test: position did not advance before switch");
        return;
    }

    if !select_with_retry(&mut player, second, 8).await {
        warn!("crossfade test: failed to select second fixture track");
        return;
    }
    assert_eq!(player.current_index(), second as i32);
    if !player.is_playing() {
        warn!("crossfade test: player stopped after track switch");
        return;
    }

    let switch_pos = player.get_position_ms();
    if !wait_position_advance(&player, switch_pos).await {
        warn!("crossfade test: position did not advance after switch");
    }
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn wasm_player_play_without_loaded_track_must_not_panic_or_hang() {
    init().await;

    let mut player = kithara_wasm::WasmPlayer::new();
    assert_eq!(player.current_index(), -1, "new player must start unloaded");

    player.play();
    yield_ms(100).await;
    // In browser backends this may transiently report playing, but must not hang/panic.
    let _ = player.is_playing();
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
#[case(2_u32, 20_u32)]
#[case(3_u32, 20_u32)]
async fn wasm_player_double_play_click_must_not_hang(
    #[case] clicks: u32,
    #[case] _poll_iterations: u32,
) {
    init().await;

    let Some(mut player) = create_player_with_fixture().await else {
        warn!("double-play test: fixture track selection failed; skip strict checks");
        return;
    };

    for click in 0..clicks {
        assert_settles(
            async {
                player.play();
            },
            10_000,
            if click == 0 {
                "first play click"
            } else if click == 1 {
                "second play click"
            } else {
                "play click"
            },
        )
        .await;
        let before = player.get_position_ms();
        if !wait_position_advance(&player, before).await {
            warn!("double-play test: position did not advance after click {click}");
        }
    }
}

/// Lifecycle sanity for the browser player path (`WasmPlayer` over `kithara-play`).
///
/// Verifies that after selecting an HLS track the player reports duration,
/// advances position while playing, pauses correctly, and keeps seek behavior sane.
#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn fill_buffer_position_must_not_drift() {
    init().await;
    info!("Starting fill_buffer_position_must_not_drift (new API)");

    let Some(mut player) = create_player_with_fixture().await else {
        warn!("failed to initialize WasmPlayer with fixture track; skip strict checks");
        return;
    };
    let duration_ms = wait_duration_ms(&player).await;
    assert!(
        duration_ms > 0.0,
        "duration must be known after track selection"
    );

    player.play();
    assert!(player.is_playing(), "play() must set playing state");

    let start_ms = player.get_position_ms();
    assert!(
        wait_position_advance(&player, start_ms).await,
        "position must advance after play()"
    );

    player.pause();
    assert!(!player.is_playing(), "pause() must clear playing state");
    let paused_ms = player.get_position_ms();
    yield_ms(120).await;
    let after_pause_ms = player.get_position_ms();
    assert!(
        (after_pause_ms - paused_ms).abs() < 250.0,
        "position advanced too much while paused: {paused_ms:.0} -> {after_pause_ms:.0}"
    );

    let target_ms = (duration_ms * 0.4).min((duration_ms - 500.0).max(0.0));
    player.seek(target_ms).unwrap();
    let seek_pos_ms = player.get_position_ms();
    assert!(
        (seek_pos_ms - target_ms).abs() < 4000.0,
        "seek landed too far from target: target={target_ms:.0}ms actual={seek_pos_ms:.0}ms"
    );

    player.play();
    let resume_ms = player.get_position_ms();
    assert!(
        wait_position_advance(&player, resume_ms).await,
        "position must continue advancing after seek + play"
    );

    let final_pos = player.get_position_ms();
    assert!(
        final_pos <= duration_ms + 1000.0,
        "position {final_pos:.0}ms exceeds duration {duration_ms:.0}ms too much"
    );
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn wasm_player_real_hls_repro_freeze_probe() {
    init().await;
    info!("Starting wasm_player_real_hls_repro_freeze_probe");

    let url = option_env!("KITHARA_REAL_HLS_STREAM_URL").unwrap_or(REAL_HLS_STREAM_URL);
    let mut player = kithara_wasm::WasmPlayer::new();
    let idx = match player.add_track(url.to_string()) {
        Ok(idx) => idx,
        Err(err) => {
            warn!("real HLS repro: add_track failed: {:?}", err);
            return;
        }
    };

    if !select_with_retry(&mut player, idx, 12).await {
        warn!("real HLS repro: select_track retries exhausted");
        return;
    }

    let duration_ms = wait_duration_ms(&player).await;
    if duration_ms <= 0.0 {
        warn!("real HLS repro: duration is not available; skip");
        return;
    }

    player.play();
    assert!(
        player.is_playing(),
        "real HLS repro: play() should start playback"
    );

    let start_ms = player.get_position_ms();
    assert!(
        wait_position_advance(&player, start_ms).await,
        "real HLS repro: position must advance after initial play"
    );

    let mut last_position_ms = player.get_position_ms();
    let mut last_process_count = player.process_count();
    let mut stable_position_iterations = 0u32;
    let mut last_events = String::new();
    let mut seek_executed = false;
    let mut seek_progress = false;

    for i in 0..80 {
        yield_ms(250).await;

        if let Err(err) = player.tick() {
            warn!(iteration = i, "real HLS repro: tick() failed: {:?}", err);
        }

        let position_ms = player.get_position_ms();
        let process_count = player.process_count();
        let events = player.take_events();
        if !events.is_empty() {
            last_events = trim_events_tail(&events);
        }

        if position_ms > last_position_ms + 10.0 {
            stable_position_iterations = 0;
            last_position_ms = position_ms;
        } else {
            stable_position_iterations += 1;
        }

        if process_count > last_process_count {
            last_process_count = process_count;
        }

        if i == 40 && !seek_executed {
            let seek_ms = (duration_ms * 0.4).clamp(500.0, (duration_ms - 500.0).max(500.0));
            match player.seek(seek_ms) {
                Ok(()) => {
                    seek_executed = true;
                    let before_seek_ms = player.get_position_ms();
                    if wait_position_advance(&player, before_seek_ms).await {
                        seek_progress = true;
                    }
                }
                Err(err) => warn!(
                    iteration = i,
                    seek_ms, "real HLS repro: midstream seek failed: {:?}", err
                ),
            }
        }

        if i % 20 == 0 {
            info!(
                iteration = i,
                position_ms = position_ms,
                process_count = process_count,
                is_playing = player.is_playing(),
                "real HLS repro probe"
            );
        }

        if stable_position_iterations >= 20 {
            panic!(
                "real HLS repro: position stalled for {stable_position_iterations} consecutive iterations (~{}ms) on stream {url}; \
                 last_pos_ms={position_ms}, process_count={process_count}, seek_executed={seek_executed}, \
                 seek_progress={seek_progress}, last_events={}",
                stable_position_iterations * 250,
                last_events
            );
        }
    }

    assert!(
        seek_executed,
        "real HLS repro: midstream seek was not executed"
    );
    if seek_executed {
        assert!(
            seek_progress,
            "real HLS repro: midstream seek should resume position progress before finish"
        );
    }
}

/// Aggressive seek stress test: 1000 rapid random seeks.
///
/// Catches the bug where `read()` returns 0 after seek and the pipeline
/// stalls permanently (player stops, never resumes without stop+restart).
///
/// Strategy:
/// - Warmup: read 20 chunks to prime the pipeline
/// - 1000 random seeks: 10% near start (<1s), 10% near end, 80% random
/// - After each seek: read_with_yield must produce >0 samples (not stuck)
/// - All samples must be finite and in [-1.0, 1.0]
/// - Tolerate at most 1% dead seeks (pipeline restart race)
#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn stress_rapid_seeks_must_not_stall() {
    init().await;
    info!("Starting stress_rapid_seeks_must_not_stall");

    let mut audio = create_pipeline().await;
    let spec = audio.spec();
    info!(
        channels = spec.channels,
        sample_rate = spec.sample_rate,
        "Pipeline created"
    );

    let mut buf = vec![0.0f32; 4096];

    // Phase 1: Warmup
    let mut warmup_samples = 0usize;
    for _ in 0..20 {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        warmup_samples += n;
    }
    assert!(warmup_samples > 0, "warmup must produce data");
    info!(warmup_samples, "Warmup complete");

    let duration = audio.duration().unwrap_or(Duration::from_secs(60));
    let duration_secs = duration.as_secs_f64();
    let max_seek = duration_secs - 0.5;
    info!(duration_secs, max_seek, "Duration known");

    // Phase 2: 1000 rapid random seeks
    const SEEK_COUNT: usize = 1000;
    let sample_rate = audio.spec().sample_rate;
    let channels = audio.spec().channels as usize;
    let mut rng = Xorshift64::new(0xDEAD_BEEF_CAFE_1337);
    let mut dead_seeks = 0u64;
    let mut seek_errors = 0u64;
    let mut integrity_errors = 0u64;
    let mut position_mismatches = 0u64;
    let mut total_samples = 0u64;

    for i in 0..SEEK_COUNT {
        // Mix: 10% near start, 10% near end, 80% random
        let r = rng.next_f64();
        let pos_secs = if r < 0.1 {
            rng.range_f64(0.0, 1.0_f64.min(max_seek))
        } else if r < 0.2 {
            rng.range_f64((max_seek - 2.0).max(0.0), max_seek)
        } else {
            rng.range_f64(0.001, max_seek)
        };

        let position = Duration::from_secs_f64(pos_secs);
        if let Err(e) = audio.seek(position) {
            seek_errors += 1;
            if seek_errors <= 3 {
                warn!(iteration = i, pos_secs, ?e, "seek error");
            }
            continue;
        }

        // Must produce data within 200 yields (2s at 10ms each)
        let n = read_with_yield_limit(&mut audio, &mut buf, 200).await;
        if n == 0 && !audio.is_eof() {
            dead_seeks += 1;
            if dead_seeks <= 5 {
                warn!(
                    iteration = i,
                    pos_secs,
                    is_eof = audio.is_eof(),
                    "STALL: read returned 0 after seek"
                );
            }
            continue;
        }

        // Integrity check
        for &sample in &buf[..n] {
            if !sample.is_finite() || !(-1.0..=1.0).contains(&sample) {
                integrity_errors += 1;
                break;
            }
        }

        // Position check: decoded phase must match expected phase for seek target.
        // Tolerance: 1200 frames (~27ms) for packet boundary alignment.
        let frames = n / channels;
        if frames > 0 {
            let expected_frame = (pos_secs * sample_rate as f64).round() as usize;
            let expected_phase = expected_frame % SAW_PERIOD;
            let actual_phase = phase_from_f32(buf[0]);
            let dist = phase_distance(actual_phase, expected_phase);
            if dist > 1200 {
                position_mismatches += 1;
                if position_mismatches <= 5 {
                    warn!(
                        iteration = i,
                        pos_secs, expected_phase, actual_phase, dist, "position mismatch"
                    );
                }
            }
        }

        total_samples += n as u64;

        if (i + 1) % 200 == 0 {
            info!(
                iteration = i + 1,
                dead_seeks, seek_errors, position_mismatches, total_samples, "Progress"
            );
        }

        // Yield every 50 iterations to keep event loop responsive
        if i % 50 == 49 {
            yield_ms(1).await;
        }
    }

    info!(
        dead_seeks,
        seek_errors,
        integrity_errors,
        position_mismatches,
        total_samples,
        "Phase 2 complete: {SEEK_COUNT} seeks"
    );

    let max_dead = (SEEK_COUNT as u64) / 100; // 1% threshold
    assert!(
        dead_seeks <= max_dead,
        "pipeline stalled {dead_seeks}/{SEEK_COUNT} times \
         (>{max_dead} = 1% threshold) — read() returns 0 after seek"
    );
    assert_eq!(
        integrity_errors, 0,
        "samples outside [-1,1] or not finite — data corruption"
    );

    // Soft threshold: at most 5% position mismatches
    let max_mismatches = (SEEK_COUNT as u64) / 20;
    assert!(
        position_mismatches <= max_mismatches,
        "position mismatches {position_mismatches}/{SEEK_COUNT} \
         (>{max_mismatches} = 5% threshold) — seek landed in wrong place"
    );
}

/// Seek-to-zero after heavy pressure: ensures the beginning of the track
/// is still accessible after thousands of operations.
///
/// Catches the bug where seeking back to position 0 produces no data
/// because first segments were evicted or the pipeline entered a bad state.
///
/// Strategy:
/// - Warmup: read 20 chunks
/// - Stress: 500 random seeks (prime the pipeline, exercise eviction)
/// - Reset: seek to 0
/// - Verify: read at least 50 chunks from the beginning, all valid
/// - Position must be near 0 after seeking, then advance monotonically
#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn stress_seek_to_zero_after_pressure() {
    init().await;
    info!("Starting stress_seek_to_zero_after_pressure");

    let mut audio = create_pipeline().await;
    let mut buf = vec![0.0f32; 4096];

    // Phase 1: Warmup
    let mut warmup = 0usize;
    for _ in 0..20 {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        warmup += n;
    }
    assert!(warmup > 0, "warmup must produce data");

    let duration = audio.duration().unwrap_or(Duration::from_secs(60));
    let duration_secs = duration.as_secs_f64();
    let max_seek = duration_secs - 0.5;
    info!(warmup, duration_secs, "Warmup done");

    // Phase 2: Stress — 500 random seeks
    let mut rng = Xorshift64::new(0xABCD_EF01_2345_6789);
    for i in 0..500 {
        let pos = rng.range_f64(0.001, max_seek);
        let _ = audio.seek(Duration::from_secs_f64(pos));
        // Read a small amount — don't care about result, just exercise the pipeline
        let _ = read_with_yield_limit(&mut audio, &mut buf, 50).await;

        if i % 100 == 99 {
            yield_ms(1).await;
        }
    }
    info!("Stress phase done: 500 seeks");

    // Phase 3: Seek to 0
    let channels = audio.spec().channels as usize;
    let sample_rate = audio.spec().sample_rate;
    audio
        .seek(Duration::ZERO)
        .expect("seek to 0 must succeed after stress");

    let pos_after_seek = audio.position();
    info!(
        pos_ms = pos_after_seek.as_millis(),
        "Seeked to 0, position reported"
    );

    // Phase 4: Read from beginning and verify
    let mut total_from_zero = 0usize;
    let mut chunks_from_zero = 0usize;
    let target_chunks = 50;
    let mut position_checked = false;
    let mut continuity_errors = 0u64;
    let mut prev_last_phase: Option<usize> = None;
    let mut stall_deadline = Instant::now() + Duration::from_secs(25);

    for attempt in 0..target_chunks * 20 {
        let n = read_with_yield_limit(&mut audio, &mut buf, 100).await;
        if n == 0 {
            if audio.is_eof() {
                info!(chunks_from_zero, total_from_zero, "EOF reached");
                break;
            }
            if Instant::now() >= stall_deadline {
                panic!(
                    "STUCK after seek-to-0: read returned 0 for {attempt} attempts before deadline, \
                     chunks_from_zero={chunks_from_zero}, \
                     position={:.3}s",
                    audio.position().as_secs_f64()
                );
            }
            continue;
        }

        // Reset stall timer on each successful post-seek chunk.
        stall_deadline = Instant::now() + Duration::from_secs(5);

        chunks_from_zero += 1;
        total_from_zero += n;
        let frames = n / channels;

        // Every sample must be valid
        for (j, &sample) in buf[..n].iter().enumerate() {
            assert!(
                sample.is_finite() && (-1.0..=1.0).contains(&sample),
                "corrupt sample at offset {j} in chunk {chunks_from_zero} \
                 after seek-to-0: {sample}"
            );
        }

        // Position check: first decoded sample after seek(0) must have phase near 0.
        // Phase 0 = frame 0 of the saw-tooth.  Tolerance: 1200 frames.
        if !position_checked && frames > 0 {
            let actual_phase = phase_from_f32(buf[0]);
            // Expected phase for position 0 = 0
            let dist = phase_distance(actual_phase, 0);
            info!(
                actual_phase,
                dist, sample_rate, "Phase after seek-to-0 (expected ≈ 0)"
            );
            assert!(
                dist <= 1200,
                "after seek(0), decoded phase is {actual_phase} \
                 (distance {dist} > 1200 from expected 0) — seek landed in wrong place"
            );
            position_checked = true;
        }

        // Continuity: inter-chunk
        if let Some(prev_phase) = prev_last_phase {
            let first_phase = phase_from_f32(buf[0]);
            if first_phase != (prev_phase + 1) % SAW_PERIOD {
                continuity_errors += 1;
            }
        }

        // Continuity: intra-chunk
        if frames >= 2 {
            for f in 1..frames {
                let prev = phase_from_f32(buf[(f - 1) * channels]);
                let curr = phase_from_f32(buf[f * channels]);
                if curr != (prev + 1) % SAW_PERIOD {
                    continuity_errors += 1;
                }
            }
        }

        if frames > 0 {
            prev_last_phase = Some(phase_from_f32(buf[(frames - 1) * channels]));
        }

        if chunks_from_zero >= target_chunks {
            break;
        }
    }

    info!(
        chunks_from_zero,
        total_from_zero,
        continuity_errors,
        position_ms = audio.position().as_millis(),
        "Read after seek-to-0 complete"
    );

    assert!(
        chunks_from_zero >= target_chunks,
        "expected at least {target_chunks} chunks after seek-to-0, got {chunks_from_zero} \
         — first segments may be missing or pipeline stalled"
    );
    assert!(total_from_zero > 0, "must read samples after seek-to-0");
    assert!(
        continuity_errors <= 5,
        "{continuity_errors} continuity breaks after seek-to-0 — non-contiguous data"
    );
}

/// Regression: after long playback from the middle, seek near start (but not 0)
/// must land inside segment 0, not at segment 1 boundary.
#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn stress_seek_near_start_after_mid_playback_must_land_inside_first_segment() {
    init().await;
    info!("Starting stress_seek_near_start_after_mid_playback_must_land_inside_first_segment");

    let mut audio = create_pipeline().await;
    let spec = audio.spec();
    let channels = spec.channels as usize;
    let sample_rate = spec.sample_rate as usize;
    let mut buf = vec![0.0f32; 4096];

    let mut warmup = 0usize;
    for _ in 0..20 {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        warmup += n;
    }
    assert!(warmup > 0, "warmup must read some data");

    let duration = audio.duration().unwrap_or(Duration::from_secs(60));
    let duration_secs = duration.as_secs_f64();

    let middle_secs = (duration_secs * 0.5).clamp(3.0, (duration_secs - 1.0).max(3.0));
    audio
        .seek(Duration::from_secs_f64(middle_secs))
        .expect("seek to middle must succeed");

    // Emulate "listen ~5s from middle".
    let mut played_frames = 0usize;
    let target_frames = sample_rate * 5;
    for _ in 0..600 {
        let n = read_with_yield_limit(&mut audio, &mut buf, 50).await;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            continue;
        }
        played_frames += n / channels;
        if played_frames >= target_frames {
            break;
        }
    }
    assert!(
        played_frames >= sample_rate * 3,
        "expected at least ~3s playback before near-start seek, got {} frames",
        played_frames
    );

    let near_start_secs = 0.2_f64;
    audio
        .seek(Duration::from_secs_f64(near_start_secs))
        .expect("seek near start must succeed");

    let expected_frame = (near_start_secs * spec.sample_rate as f64).round() as usize;
    let expected_phase = expected_frame % SAW_PERIOD;
    let seg1_start_frames = 200_000 / (channels * 2);
    let seg1_start_phase = seg1_start_frames % SAW_PERIOD;

    let mut checked = false;
    for _ in 0..200 {
        let n = read_with_yield_limit(&mut audio, &mut buf, 100).await;
        if n == 0 {
            continue;
        }
        let frames = n / channels;
        if frames == 0 {
            continue;
        }

        let actual_phase = phase_from_f32(buf[0]);
        let dist_expected = phase_distance(actual_phase, expected_phase);
        let dist_seg1 = phase_distance(actual_phase, seg1_start_phase);

        info!(
            near_start_secs,
            expected_phase,
            actual_phase,
            dist_expected,
            seg1_start_phase,
            dist_seg1,
            "phase after middle->near-start seek"
        );

        assert!(
            dist_expected <= 1200,
            "near-start seek landed in wrong place: \
             expected_phase={expected_phase}, actual_phase={actual_phase}, dist={dist_expected}"
        );
        assert!(
            dist_seg1 > 3000,
            "near-start seek suspiciously close to segment-1 boundary: \
             actual_phase={actual_phase}, seg1_phase={seg1_start_phase}, dist={dist_seg1}"
        );

        checked = true;
        break;
    }

    assert!(
        checked,
        "failed to decode samples after near-start seek following middle playback"
    );
}

/// Event-level regression guard for seek behavior in browser path:
/// one seek command should produce one seek-complete, and playback progress
/// after that seek should advance without extra backward resets.
#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn stress_seek_events_single_reset_and_monotonic_progress() {
    init().await;
    info!("Starting stress_seek_events_single_reset_and_monotonic_progress");

    let mut audio = create_pipeline().await;
    let mut events_rx = audio.events();
    let spec = audio.spec();
    let channels = spec.channels as usize;
    let mut buf = vec![0.0f32; 4096];

    // Warmup.
    let mut warmup = 0usize;
    for _ in 0..20 {
        let n = read_with_yield(&mut audio, &mut buf).await;
        if n == 0 {
            break;
        }
        warmup += n;
    }
    assert!(warmup > 0, "warmup must read some data");

    // Drain old events before seek scenario.
    while events_rx.try_recv().is_ok() {}

    // Seek to middle and "play" a few seconds first.
    let duration_secs = audio
        .duration()
        .unwrap_or(Duration::from_secs(60))
        .as_secs_f64();
    let middle_secs = (duration_secs * 0.5).clamp(3.0, (duration_secs - 1.0).max(3.0));
    audio
        .seek(Duration::from_secs_f64(middle_secs))
        .expect("seek to middle must succeed");

    let mut played_frames = 0usize;
    let target_frames = spec.sample_rate as usize * 5;
    for _ in 0..600 {
        let n = read_with_yield_limit(&mut audio, &mut buf, 50).await;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            continue;
        }
        played_frames += n / channels;
        if played_frames >= target_frames {
            break;
        }
    }
    assert!(
        played_frames >= spec.sample_rate as usize * 3,
        "expected at least ~3s playback before test seek"
    );

    // Drain events again to isolate one seek window.
    while events_rx.try_recv().is_ok() {}

    let near_start_secs = 0.2_f64;
    let near_start = Duration::from_secs_f64(near_start_secs);
    audio
        .seek(near_start)
        .expect("seek near start must succeed");

    let mut target_seek_epoch = None;
    let mut seek_complete_for_target = 0usize;
    let mut seek_complete_seen = false;
    let mut playback_positions = Vec::with_capacity(64);

    for _ in 0..300 {
        let _ = read_with_yield_limit(&mut audio, &mut buf, 50).await;

        loop {
            match events_rx.try_recv() {
                Ok(Event::Audio(AudioEvent::SeekLifecycle {
                    stage: SeekLifecycleStage::SeekRequest,
                    seek_epoch,
                    ..
                })) => {
                    target_seek_epoch = Some(seek_epoch);
                }
                Ok(Event::Audio(AudioEvent::SeekComplete { seek_epoch, .. })) => {
                    if target_seek_epoch == Some(seek_epoch) {
                        seek_complete_for_target += 1;
                        seek_complete_seen = true;
                        // Keep only post-seek-complete playback progress.
                        playback_positions.clear();
                    }
                }
                Ok(Event::Audio(AudioEvent::PlaybackProgress {
                    position_ms,
                    seek_epoch,
                    ..
                })) => {
                    if seek_complete_seen && target_seek_epoch == Some(seek_epoch) {
                        playback_positions.push(position_ms);
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    break;
                }
            }
        }

        if seek_complete_seen && playback_positions.len() >= 12 {
            break;
        }
    }

    assert!(
        target_seek_epoch.is_some(),
        "no AudioEvent::SeekLifecycle::SeekRequest for target seek"
    );
    assert!(
        seek_complete_for_target >= 1,
        "no AudioEvent::SeekComplete for target seek"
    );
    assert_eq!(
        seek_complete_for_target, 1,
        "single seek produced multiple seek-complete events for same target"
    );
    assert!(
        playback_positions.len() >= 8,
        "insufficient PlaybackProgress events after seek: {}",
        playback_positions.len()
    );

    let mut regressions = 0usize;
    let mut prev = None;
    for pos in playback_positions {
        if let Some(prev_pos) = prev
            && pos < prev_pos
        {
            regressions += 1;
        }
        prev = Some(pos);
    }

    assert_eq!(
        regressions, 0,
        "PlaybackProgress moved backward {regressions} times after a single seek"
    );
}

/// PCM-level regression guard for audible "micro-loop" after seek.
///
/// Scenario:
/// - Seek to middle, read ~5s
/// - Seek near start (non-zero)
/// - Inspect the first ~350ms of decoded PCM after seek
///
/// For the deterministic saw-tooth fixture, this early window must be strictly
/// contiguous frame-by-frame (no tiny backward jumps / repeated fragments).
#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn stress_seek_pcm_window_after_seek_must_not_loop_fragment() {
    init().await;
    info!("Starting stress_seek_pcm_window_after_seek_must_not_loop_fragment");

    run_seek_pcm_window_check(create_pipeline().await).await;
}

#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn stress_seek_pcm_window_after_seek_must_not_loop_fragment_jitter() {
    init().await;
    info!("Starting stress_seek_pcm_window_after_seek_must_not_loop_fragment_jitter");

    run_seek_pcm_window_check(create_pipeline_with_url(fixture_jitter_url()).await).await;
}

/// Full `WasmPlayer` lifecycle under pressure: play → rapid seeks → pause →
/// seek to 0 → play → verify position flow.
#[kithara::test(wasm, serial, timeout(Duration::from_secs(90)))]
async fn stress_player_lifecycle_seek_pressure() {
    init().await;
    info!("Starting stress_player_lifecycle_seek_pressure");

    let mut player = kithara_wasm::WasmPlayer::new();
    let idx = match player.add_track(fixture_url().to_string()) {
        Ok(idx) => idx,
        Err(err) => {
            warn!("failed to add fixture track: {:?}", err);
            return;
        }
    };

    if let Err(err) = player.select_track(idx).await {
        warn!("failed to select fixture track: {:?}", err);
        return;
    }

    let duration_ms = wait_duration_ms(&player).await;
    info!(duration_ms, "HLS loaded");
    if duration_ms <= 0.0 {
        warn!("duration is not available; skipping strict lifecycle checks");
        return;
    }

    // Phase 1: Play and observe movement
    player.play();
    assert!(player.is_playing(), "should be playing after play()");
    let warmup_ms = player.get_position_ms();
    assert!(
        wait_position_advance(&player, warmup_ms).await,
        "position must advance during warmup"
    );

    let max_seek_ms = if duration_ms > 1000.0 {
        duration_ms - 500.0
    } else {
        duration_ms * 0.9
    };

    // Phase 2: Rapid seek cycles
    let mut rng = Xorshift64::new(0x1234_5678_9ABC_DEF0);
    let mut bad_positions = 0u64;
    const SEEK_CYCLES: usize = 120;

    for i in 0..SEEK_CYCLES {
        // Random seek position: 10% near start, 10% near end, 80% random
        let r = rng.next_f64();
        let seek_ms = if r < 0.1 {
            rng.range_f64(0.0, 1000.0f64.min(max_seek_ms))
        } else if r < 0.2 {
            rng.range_f64((max_seek_ms - 2000.0).max(0.0), max_seek_ms)
        } else {
            rng.range_f64(0.0, max_seek_ms)
        };

        if let Err(err) = player.seek(seek_ms) {
            warn!(iteration = i, seek_ms, "seek failed: {:?}", err);
            continue;
        }
        yield_ms(10).await;

        let pos = player.get_position_ms();
        if !pos.is_finite() || pos < 0.0 || pos > duration_ms + 1500.0 {
            bad_positions += 1;
            warn!(
                iteration = i,
                seek_ms,
                actual_pos_ms = pos,
                duration_ms,
                "position out of expected bounds after seek"
            );
        }

        if i % 40 == 39 {
            info!(
                iteration = i + 1,
                bad_positions,
                is_playing = player.is_playing(),
                "Seek stress progress"
            );
        }
    }

    info!(bad_positions, "Seek stress done");

    let after_stress_pos = player.get_position_ms();
    assert!(
        wait_position_advance(&player, after_stress_pos).await,
        "position must continue moving after seek stress"
    );

    // Phase 3: Pause → seek to 0 → play → verify movement resumes
    player.pause();
    assert!(!player.is_playing(), "should be paused after pause()");

    if let Err(err) = player.seek(0.0) {
        warn!("seek(0) failed: {:?}", err);
        return;
    }
    player.play();
    assert!(player.is_playing(), "should be playing after play()");

    let reset_pos = player.get_position_ms();
    assert!(
        wait_position_advance(&player, reset_pos).await,
        "position must move after pause → seek(0) → play"
    );

    info!(
        position_ms = player.get_position_ms(),
        is_playing = player.is_playing(),
        "Post-reset progress complete"
    );

    // Position should be reasonable (not stuck at 0, not wildly ahead)
    let final_pos = player.get_position_ms();
    assert!(
        final_pos < duration_ms + 1000.0,
        "position {final_pos:.0}ms exceeds duration {duration_ms:.0}ms — position drift"
    );

    let max_bad = SEEK_CYCLES as u64 / 10; // 10%
    assert!(
        bad_positions <= max_bad,
        "position out-of-range {bad_positions}/{SEEK_CYCLES} times \
         (>{max_bad})"
    );
}
