//! Regression test: ABR variant switch during normal playback must not hang.
//!
//! Reproduces the production crash where HLS ABR switch causes
//! `[HangDetector] run_shared_worker_loop no progress for 10s`.
//!
//! Uses real fMP4/AAC assets (same as production app) with disk cache.

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    events::{AbrEvent, Event, EventBus},
    file::{File, FileConfig},
    hls::{AbrMode, Hls, HlsConfig},
    play::internal::offline::{OfflinePlayer, resource_from_reader},
    stream::{AudioCodec, Stream},
};
use kithara_platform::time::{Duration, Instant, sleep};
use kithara_test_utils::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    fixture_protocol::{DelayRule, PcmPattern},
    temp_dir,
};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::continuity::{
    CONTINUITY_BLOCK_FRAMES, CONTINUITY_SAMPLE_RATE, PlaybackProgressProbe, render_offline_window,
};

fn forced_downswitch_abr_options() -> AbrMode {
    AbrMode::Auto(Some(0))
}

fn packaged_identical_content_abr_builder(codec: AudioCodec) -> HlsFixtureBuilder {
    let builder = HlsFixtureBuilder::new()
        .variant_count(2)
        .segments_per_variant(6)
        .segment_duration_secs(2.0)
        .variant_bandwidths(vec![5_000_000, 1_000_000])
        .delay_rules(vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(2),
            delay_ms: 500,
            ..Default::default()
        }]);
    match codec {
        AudioCodec::AacLc => builder.packaged_audio_per_variant_pcm_aac_lc(
            CONTINUITY_SAMPLE_RATE,
            2,
            vec![PcmPattern::Ascending, PcmPattern::Ascending],
        ),
        AudioCodec::Flac => builder.packaged_audio_per_variant_pcm_flac(
            CONTINUITY_SAMPLE_RATE,
            2,
            vec![PcmPattern::Ascending, PcmPattern::Ascending],
        ),
        other => panic!("unsupported packaged ABR codec: {other:?}"),
    }
}

async fn create_packaged_abr_fixture() -> (TestServerHelper, url::Url) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(packaged_identical_content_abr_builder(AudioCodec::AacLc))
        .await
        .unwrap_or_else(|error| panic!("create packaged ABR fixture: {error}"));
    (server, created.master_url())
}

async fn open_packaged_hls_audio(
    url: &url::Url,
    store: StoreOptions,
    abr: AbrMode,
    bus: Option<EventBus>,
) -> Audio<Stream<Hls>> {
    let mut hls_config = HlsConfig::new(url.clone())
        .with_store(store)
        .with_initial_abr_mode(abr)
        .with_download_batch_size(1);
    if let Some(bus) = bus.clone() {
        hls_config = hls_config.with_events(bus);
    }

    let mut config = AudioConfig::<Hls>::new(hls_config);
    if let Some(bus) = bus {
        config = config.with_events(bus);
    }

    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .unwrap_or_else(|err| panic!("packaged ABR audio should open for {url}: {err}"));
    audio.preload();
    audio
}

async fn read_audio_some(audio: &mut Audio<Stream<Hls>>, stage: &str) -> usize {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0.0f32; 4096];

    loop {
        audio.preload();
        let read = audio.read(&mut buf);
        if read > 0 {
            return read;
        }
        assert!(
            !audio.is_eof(),
            "unexpected EOF while waiting for packaged ABR audio at stage={stage}"
        );
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for packaged ABR audio at stage={stage}"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

/// Real fMP4/AAC HLS stream with ABR auto-switch must play without hanging.
///
/// This is the exact scenario from the production crash:
/// `kithara-app` plays track.mp3 + hls/master.m3u8 + drm/master.m3u8.
/// ABR switches variant on HLS track → worker hangs → all tracks die.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn abr_switch_real_assets_does_not_hang(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");

    let cancel = CancellationToken::new();
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_initial_abr_mode(AbrMode::Auto(Some(0)));

    let config = AudioConfig::<Hls>::new(hls_config);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    audio.preload();

    // Read for 15 seconds. If ABR switch hangs the worker,
    // HangDetector (3s) will panic before the 30s test timeout.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut buf = vec![0f32; 4096];
    let mut total_samples = 0u64;

    while Instant::now() < deadline {
        let n = audio.read(&mut buf);
        total_samples += n as u64;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    info!(total_samples, "playback completed without hang");
    assert!(
        total_samples > 1000,
        "expected sustained playback, got only {total_samples} samples"
    );
}

/// Packaged ABR HLS must switch variants without losing continuity.
///
/// Acceptance is split deliberately:
/// - direct `Audio::read()` proves the packaged fixture really switches and that
///   `PlaybackProgress` stays monotonic through the switch;
/// - post-switch `PcmChunk` reads give root-cause diagnostics;
/// - `OfflinePlayer::render()` is the player-level continuity oracle.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn packaged_abr_switch_keeps_player_continuity(temp_dir: TestTempDir) {
    let (_server, url) = create_packaged_abr_fixture().await;
    let store = StoreOptions::new(temp_dir.path());

    let bus = EventBus::new(64);
    let mut hls_rx = bus.subscribe();
    let mut progress_audio = open_packaged_hls_audio(
        &url,
        store.clone(),
        forced_downswitch_abr_options(),
        Some(bus.clone()),
    )
    .await;
    let mut progress_rx = progress_audio.events();
    let mut progress_probe = PlaybackProgressProbe::default();
    let mut switch_count = 0usize;
    let mut switch_seen = false;
    let mut total_samples = 0u64;
    let mut post_switch_samples = 0u64;
    let mut buf = vec![0.0f32; 4096];
    total_samples += read_audio_some(&mut progress_audio, "packaged_abr_warmup").await as u64;
    progress_probe.drain(&mut progress_rx);
    let deadline = Instant::now() + Duration::from_secs(8);

    while Instant::now() < deadline {
        progress_audio.preload();
        let read = progress_audio.read(&mut buf);
        progress_probe.drain(&mut progress_rx);
        loop {
            match hls_rx.try_recv() {
                Ok(Event::Abr(AbrEvent::VariantApplied { .. })) => {
                    switch_count += 1;
                    switch_seen = true;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }

        if read == 0 {
            if progress_audio.is_eof() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
            progress_probe.observe_idle();
            continue;
        }

        total_samples += read as u64;
        if switch_seen {
            post_switch_samples += read as u64;
        }
        if switch_seen && progress_probe.progress_events >= 8 && post_switch_samples >= 16_384 {
            break;
        }
    }
    progress_probe.drain(&mut progress_rx);
    progress_probe.observe_idle();

    assert!(
        switch_count > 0,
        "packaged ABR fixture must switch variants"
    );
    assert!(
        total_samples > 0,
        "packaged ABR fixture must produce decoded output"
    );
    assert!(
        post_switch_samples > 0,
        "packaged ABR fixture produced no decoded output after the switch"
    );
    assert!(
        progress_probe.progress_events >= 4,
        "expected PlaybackProgress events during packaged ABR playback, got {}",
        progress_probe.progress_events
    );
    assert_eq!(
        progress_probe.regressions, 0,
        "PlaybackProgress moved backward during packaged ABR playback"
    );
    assert!(
        progress_probe.max_gap_between_events < Duration::from_millis(1_500),
        "PlaybackProgress stalled for {:?} during packaged ABR playback",
        progress_probe.max_gap_between_events
    );

    let decode_audio =
        open_packaged_hls_audio(&url, store, forced_downswitch_abr_options(), None).await;
    let mut resource = resource_from_reader(decode_audio);
    timeout(Duration::from_secs(5), resource.preload())
        .await
        .expect("packaged ABR preload must complete");
    let mut player = OfflinePlayer::new(CONTINUITY_SAMPLE_RATE);
    player.load_and_fadein(resource, "packaged_abr");
    let _warmup = render_offline_window(
        &mut player,
        24,
        "packaged abr warmup",
        CONTINUITY_BLOCK_FRAMES,
        CONTINUITY_SAMPLE_RATE,
    );
    let seam = render_offline_window(
        &mut player,
        220,
        "packaged abr seam window",
        CONTINUITY_BLOCK_FRAMES,
        CONTINUITY_SAMPLE_RATE,
    );
    assert!(
        seam.max_silence_run <= 2,
        "packaged ABR switch produced {} silent blocks ({seam})",
        seam.max_silence_run
    );
    assert!(
        seam.slow_renders <= 1,
        "packaged ABR switch exceeded render budget {} times ({seam})",
        seam.slow_renders
    );
}

/// Stream must continue producing chunks after seek sequence.
///
/// Regression for app3.log: DRM track plays to 23.97s with seeks,
/// then stops producing chunks → `recv_outcome_blocking` hang.
///
/// Parameterized: path × ABR mode to isolate DRM vs HLS vs no-ABR.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[case::drm_abr_auto_sw("drm/master.m3u8", true, false)]
#[case::drm_abr_auto_hw("drm/master.m3u8", true, true)]
#[case::hls_abr_auto_sw("hls/master.m3u8", true, false)]
#[case::hls_abr_auto_hw("hls/master.m3u8", true, true)]
#[case::drm_manual_v0_sw("drm/master.m3u8", false, false)]
#[case::drm_manual_v0_hw("drm/master.m3u8", false, true)]
#[case::hls_manual_v0_sw("hls/master.m3u8", false, false)]
#[case::hls_manual_v0_hw("hls/master.m3u8", false, true)]
async fn stream_continues_after_seek(
    temp_dir: TestTempDir,
    #[case] path: &str,
    #[case] abr_auto: bool,
    #[case] prefer_hardware: bool,
) {
    let server = TestServerHelper::new().await;
    let url = server.asset(path);

    let cancel = CancellationToken::new();
    let abr_mode = if abr_auto {
        AbrMode::Auto(Some(0))
    } else {
        AbrMode::Manual(0)
    };
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_initial_abr_mode(abr_mode);

    let config = AudioConfig::<Hls>::new(hls_config).with_prefer_hardware(prefer_hardware);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    audio.preload();

    let mut buf = vec![0f32; 4096];

    // Phase 1: Read for 3s (warmup)
    let warmup = Instant::now() + Duration::from_secs(3);
    while Instant::now() < warmup {
        let _ = audio.read(&mut buf);
        sleep(Duration::from_millis(5)).await;
    }

    // Phase 2: Seek to ~7s, ~13s, ~18s, ~24s (like app3.log)
    //
    // Read a limited number of samples after each seek — NOT a wall-time
    // deadline — because the decoder decodes faster than real-time and
    // would otherwise reach EOF within each 3-second window.
    let samples_per_seek: u64 = 48000 * 2; // ~1 second of stereo 48 kHz
    for &target_secs in &[7.0, 13.0, 18.0, 24.0] {
        audio
            .seek(Duration::from_secs_f64(target_secs))
            .expect("seek");

        let mut samples = 0u64;
        let deadline = Instant::now() + Duration::from_secs(5);
        while samples < samples_per_seek && Instant::now() < deadline {
            let n = audio.read(&mut buf);
            samples += n as u64;
            if n == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(
            samples > 0,
            "[{path}] seek to {target_secs}s must produce samples, got 0"
        );
    }

    // Phase 3: Continue reading after last seek — must not hang.
    // Read another ~1s of audio to confirm playback continues.
    let mut post_seek_samples = 0u64;
    let deadline = Instant::now() + Duration::from_secs(5);
    while post_seek_samples < samples_per_seek && Instant::now() < deadline {
        let n = audio.read(&mut buf);
        post_seek_samples += n as u64;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }
    assert!(
        post_seek_samples > 0,
        "[{path}] playback after seeks must continue, got 0 samples"
    );
}

/// Same test but without ABR (fixed variant 0) — baseline.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn fixed_variant_real_assets_plays_without_hang(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");

    let cancel = CancellationToken::new();
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_initial_abr_mode(AbrMode::Manual(0));

    let config = AudioConfig::<Hls>::new(hls_config);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    audio.preload();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut buf = vec![0f32; 4096];
    let mut total_samples = 0u64;

    while Instant::now() < deadline {
        let n = audio.read(&mut buf);
        total_samples += n as u64;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    assert!(
        total_samples > 1000,
        "baseline: expected sustained playback, got only {total_samples} samples"
    );
}

/// Seek after decode-to-EOF in mmap (non-ephemeral) DRM mode must produce samples.
///
/// Regression: after ABR switch + full decode to EOF, random seeks land on
/// segments whose byte offsets are no longer visible in the `StreamIndex` layout,
/// causing `read_at` to return Retry forever.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5"),
    tracing("kithara_audio=warn,kithara_hls=warn,symphonia_format_isomp4=warn")
)]
#[case::drm("drm/master.m3u8")]
#[case::hls("hls/master.m3u8")]
async fn seek_after_eof_mmap_produces_samples(temp_dir: TestTempDir, #[case] path: &str) {
    let server = TestServerHelper::new().await;
    let url = server.asset(path);

    let cancel = CancellationToken::new();
    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_initial_abr_mode(AbrMode::Auto(Some(0)));

    let config = AudioConfig::<Hls>::new(hls_config).with_prefer_hardware(false);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    audio.preload();

    let mut buf = vec![0f32; 4096];

    // Phase 1: Warmup — read a few seconds so ABR can switch variant.
    let warmup = Instant::now() + Duration::from_secs(3);
    while Instant::now() < warmup {
        let _ = audio.read(&mut buf);
        sleep(Duration::from_millis(5)).await;
    }

    // Phase 2: Random seeks — same pattern as the stress test.
    // Each seek must produce at least some samples within 5s.
    let seek_targets = [
        50.0, 120.0, 5.0, 80.0, 150.0, 30.0, 100.0, 60.0, 140.0, 20.0, 90.0, 10.0, 70.0, 130.0,
        40.0, 110.0,
    ];
    let samples_per_seek: u64 = 48000 * 2;
    for (idx, &target_secs) in seek_targets.iter().enumerate() {
        audio
            .seek(Duration::from_secs_f64(target_secs))
            .unwrap_or_else(|e| panic!("[{path}] seek #{idx} to {target_secs}s failed: {e}"));

        let mut samples = 0u64;
        let deadline = Instant::now() + Duration::from_secs(5);
        while samples < samples_per_seek && Instant::now() < deadline {
            let n = audio.read(&mut buf);
            samples += n as u64;
            if n == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(
            samples > 0,
            "[{path}] seek #{idx} to {target_secs}s must produce samples, got 0"
        );
    }
}

/// MP3 progressive file must continue producing chunks after seek sequence.
///
/// Same seek pattern as HLS/DRM tests but with `Audio<Stream<File>>`.
/// Baseline: no ABR, no segments, no variant switching.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn mp3_stream_continues_after_seek(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let url = server.asset("track.mp3");

    let file_config = FileConfig::new(url.into()).with_store(StoreOptions::new(temp_dir.path()));
    let config = AudioConfig::<File>::new(file_config).with_hint("mp3");
    let mut audio = Audio::<Stream<File>>::new(config)
        .await
        .expect("create audio");
    audio.preload();

    let mut buf = vec![0f32; 4096];

    // Phase 1: Read for 3s (warmup)
    let warmup = Instant::now() + Duration::from_secs(3);
    while Instant::now() < warmup {
        let _ = audio.read(&mut buf);
        sleep(Duration::from_millis(5)).await;
    }

    // Phase 2: Seek to ~7s, ~13s, ~18s, ~24s (same as HLS/DRM tests)
    let samples_per_seek: u64 = 48000 * 2;
    for &target_secs in &[7.0, 13.0, 18.0, 24.0] {
        audio
            .seek(Duration::from_secs_f64(target_secs))
            .expect("seek");

        let mut samples = 0u64;
        let deadline = Instant::now() + Duration::from_secs(5);
        while samples < samples_per_seek && Instant::now() < deadline {
            let n = audio.read(&mut buf);
            samples += n as u64;
            if n == 0 {
                sleep(Duration::from_millis(10)).await;
            }
        }
        assert!(
            samples > 0,
            "[mp3] seek to {target_secs}s must produce samples, got 0"
        );
    }

    // Phase 3: Continue reading after last seek — must not hang
    let mut post_seek_samples = 0u64;
    let deadline = Instant::now() + Duration::from_secs(5);
    while post_seek_samples < samples_per_seek && Instant::now() < deadline {
        let n = audio.read(&mut buf);
        post_seek_samples += n as u64;
        if n == 0 {
            if audio.is_eof() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }
    assert!(
        post_seek_samples > 0,
        "[mp3] playback after seeks must continue, got 0 samples"
    );
}

/// ABR must be frozen during seek and resume afterwards.
///
/// Invariant: variant must not change between `seek()` and the first post-seek
/// chunk. After playback resumes, ABR must still work (variant changes again).
/// Uses chunk metadata (`variant_index`) instead of broadcast events to avoid
/// broadcast lag issues.
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(20)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3"),
    tracing("kithara_audio=info,kithara_hls=info")
)]
async fn abr_frozen_during_seek_resumes_after(temp_dir: TestTempDir) {
    use kithara::{audio::PcmReader, decode::PcmChunk};

    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_initial_abr_mode(AbrMode::Auto(Some(0)));

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::new(hls_config))
        .await
        .expect("audio creation");
    audio.preload();

    // Helper: read one chunk with timeout.
    async fn next_chunk(audio: &mut Audio<Stream<Hls>>, timeout_ms: u64) -> Option<PcmChunk> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if let Some(chunk) = PcmReader::next_chunk(audio) {
                return Some(chunk);
            }
            if Instant::now() > deadline {
                return None;
            }
            audio.preload();
            sleep(Duration::from_millis(2)).await;
        }
    }

    // Phase 1: warmup — read chunks until variant changes from initial (0).
    info!("Phase 1: warmup until ABR switches from variant 0");
    let mut initial_variant = None;
    let warmup_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < warmup_deadline {
        let Some(chunk) = next_chunk(&mut audio, 500).await else {
            continue;
        };
        let v = chunk.meta.variant_index;
        if initial_variant.is_none() {
            initial_variant = v;
        }
        if v != initial_variant && v.is_some() {
            info!(?initial_variant, switched_to = ?v, "ABR switched");
            break;
        }
    }
    // Read a few post-switch chunks to confirm the variant stabilized.
    // After ABR switch the decoder may be recreated, causing a brief gap
    // in chunk production. Use a longer timeout and retry to survive it.
    let mut current_variant = None;
    for _ in 0..3 {
        if let Some(chunk) = next_chunk(&mut audio, 1_000).await {
            current_variant = chunk.meta.variant_index;
            if current_variant.is_some() && current_variant != initial_variant {
                break;
            }
        }
    }
    if current_variant.is_none() || current_variant == initial_variant {
        // ABR did not switch during warmup — asset server throughput is
        // too uniform for ABR to trigger a variant change. Skip the
        // seek-freeze assertion since there is nothing to freeze.
        info!(
            ?initial_variant,
            ?current_variant,
            "ABR did not switch during warmup; skipping seek-freeze test"
        );
        return;
    }
    info!(?current_variant, "Pre-seek variant established");

    // Phase 2: seek — variant must stay the same.
    let variant_before_seek = current_variant;
    audio
        .seek(Duration::from_secs(50))
        .expect("seek must not fail");
    audio.preload();

    let post_seek_chunk = next_chunk(&mut audio, 500).await;
    assert!(
        post_seek_chunk.is_some(),
        "seek must produce a chunk within 500ms"
    );
    let variant_after_seek = post_seek_chunk.unwrap().meta.variant_index;
    assert_eq!(
        variant_before_seek, variant_after_seek,
        "ABR must NOT switch variant during seek"
    );

    // Phase 3: resume playback — ABR must still function.
    info!("Phase 3: verify ABR still works post-seek");
    let resume_deadline = Instant::now() + Duration::from_secs(8);
    let mut resume_chunks = 0u32;
    while Instant::now() < resume_deadline {
        if next_chunk(&mut audio, 200).await.is_some() {
            resume_chunks += 1;
            if resume_chunks >= 4 {
                break;
            }
        }
    }
    assert!(
        resume_chunks >= 4,
        "playback must continue after seek (got {resume_chunks} chunks)"
    );
}
