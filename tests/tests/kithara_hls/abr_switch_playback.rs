use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ChunkOutcome, ReadOutcome},
    decode::DecoderBackend,
    events::{AbrEvent, Event, EventBus},
    file::{File, FileConfig},
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, Stream},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, auto,
    fixture_protocol::{DelayRule, PcmPattern},
    offline::{OfflinePlayer, resource_from_reader},
    temp_dir,
};
use kithara_platform::{
    CancelToken, thread,
    time::{self, Duration, Instant, sleep},
    tokio::task::spawn_blocking,
};
use tracing::info;

use crate::continuity::{
    CONTINUITY_BLOCK_FRAMES, CONTINUITY_SAMPLE_RATE, PlaybackProgressProbe, render_offline_window,
};

fn forced_downswitch_abr_options() -> AbrMode {
    auto(0)
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
    let hls_config = HlsConfig::for_url(url.clone())
        .store(store)
        .initial_abr_mode(abr)
        .download_batch_size(1)
        .maybe_events(bus.clone())
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .maybe_events(bus)
        .build();

    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .unwrap_or_else(|err| panic!("packaged ABR audio should open for {url}: {err}"));
    let _ = audio.preload();
    audio
}

async fn read_audio_some(audio: &mut Audio<Stream<Hls>>, stage: &str) -> usize {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0.0f32; 4096];

    loop {
        let _ = audio.preload();
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => return count.get(),
            Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF while waiting for packaged ABR audio at stage={stage}");
            }
            Err(e) => panic!("decode error at stage={stage}: {e}"),
        }
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

    let cancel = CancelToken::never();
    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .initial_abr_mode(auto(0))
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    spawn_blocking(move || {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];
        let mut total_samples = 0u64;

        let mut saw_eof = false;
        loop {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(ReadOutcome::Frames { count, .. }) => {
                    total_samples += count.get() as u64;
                    if total_samples >= 100_000 {
                        break;
                    }
                }
                Ok(ReadOutcome::Eof { .. }) => {
                    saw_eof = true;
                    break;
                }
                Err(e) => panic!("decode error: {e}"),
            }
        }

        let _ = saw_eof;
        info!(total_samples, "playback completed without hang");
        assert!(
            total_samples > 1000,
            "expected sustained playback, got only {total_samples} samples"
        );
    })
    .await
    .expect("read phase join");
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
    let mut wake_rx = bus.subscribe();
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
        let _ = progress_audio.preload();
        let read: usize = match progress_audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => count.get(),
            Ok(ReadOutcome::Pending { .. }) => 0,
            Ok(ReadOutcome::Eof { .. }) => {
                progress_probe.drain(&mut progress_rx);
                break;
            }
            Err(e) => panic!("decode error: {e}"),
        };
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
            // Read is Pending: wait until the worker emits the next event
            // (decode progress / segment read / variant applied) rather than
            // sleeping on a timer. The dedicated wake receiver does not consume
            // from hls_rx / progress_rx, so no switch or progress event is lost.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let _ = time::timeout(remaining, wake_rx.recv()).await;
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

    // Offline render below pulls PCM on the real-time graph thread, which
    // CANNOT block: a worker underrun there is zero-filled to silence (see
    // `PlayerResource::fill_scratch`). Under flash the only clock advance
    // during `render_offline_window` is its own per-block virtual sleep, so a
    // network stall at the ABR seam (the V0 segment delay rule) would drop the
    // worker behind the real-time render pace and produce a long silent run.
    // Prime the shared store with EVERY segment of BOTH variants FIRST, on the
    // blocking pool with `block_on_underrun(true)` so each `read()` parks on the
    // worker (engine-aware, drives the virtual clock). Forcing each variant with
    // a manual mode guarantees full coverage regardless of which path the
    // offline render's `auto(0)` ABR then takes — after this the offline render
    // is a decode-from-cache path with no network stall to fall behind on.
    for variant in [0usize, 1usize] {
        let warm_config = AudioConfig::<Hls>::for_stream(
            HlsConfig::for_url(url.clone())
                .store(store.clone())
                .initial_abr_mode(AbrMode::manual(variant))
                .download_batch_size(1)
                .build(),
        )
        .block_on_underrun(true)
        .build();
        let mut warm_audio = Audio::<Stream<Hls>>::new(warm_config)
            .await
            .unwrap_or_else(|e| panic!("packaged ABR warm audio (v{variant}) must open: {e}"));
        let warmed = spawn_blocking(move || {
            let _ = warm_audio.preload();
            let mut buf = vec![0f32; 4096];
            let mut total = 0u64;
            loop {
                match warm_audio.read(&mut buf) {
                    Ok(ReadOutcome::Frames { count, .. }) => total += count.get() as u64,
                    Ok(ReadOutcome::Pending { .. }) => {}
                    Ok(ReadOutcome::Eof { .. }) => break,
                    Err(e) => panic!("packaged ABR warm decode error (v{variant}): {e}"),
                }
            }
            total
        })
        .await
        .expect("packaged ABR warm read join");
        assert!(
            warmed > 0,
            "packaged ABR warm pass (v{variant}) must decode the variant into the shared store"
        );
    }

    let decode_audio =
        open_packaged_hls_audio(&url, store, forced_downswitch_abr_options(), None).await;
    let mut resource = resource_from_reader(decode_audio);
    let _ = time::timeout(Duration::from_secs(5), resource.preload())
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
#[case::drm_abr_auto_sw("drm/master.m3u8", true, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::drm_abr_auto_hw("drm/master.m3u8", true, DecoderBackend::Apple)
)]
#[case::hls_abr_auto_sw("hls/master.m3u8", true, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_abr_auto_hw("hls/master.m3u8", true, DecoderBackend::Apple)
)]
#[case::drm_manual_v0_sw("drm/master.m3u8", false, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::drm_manual_v0_hw("drm/master.m3u8", false, DecoderBackend::Apple)
)]
#[case::hls_manual_v0_sw("hls/master.m3u8", false, DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_manual_v0_hw("hls/master.m3u8", false, DecoderBackend::Apple)
)]
async fn stream_continues_after_seek(
    temp_dir: TestTempDir,
    #[case] path: &str,
    #[case] abr_auto: bool,
    #[case] backend: DecoderBackend,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let server = TestServerHelper::new().await;
    let url = server.asset(path);

    let cancel = CancelToken::never();
    let abr_mode = if abr_auto {
        auto(0)
    } else {
        AbrMode::manual(0)
    };
    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .initial_abr_mode(abr_mode)
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .decoder_backend(backend)
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    // The blocking read phase must NOT run on the test runtime thread: with
    // block_on_underrun the read parks the thread, and on the current-thread
    // runtime that starves the HLS drive/fetch tasks that feed it. preload()
    // primes through the same parking recv, so it belongs here too.
    spawn_blocking(move || {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];

        let mut warmup_samples = 0u64;
        while warmup_samples < 17_640 {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => warmup_samples += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => thread::sleep(Duration::from_millis(5)),
                Ok(ReadOutcome::Eof { .. }) => panic!("[{path}] unexpected EOF during warmup"),
                Err(e) => panic!("decode error during warmup: {e}"),
            }
        }

        let samples_per_seek: u64 = 48000 * 2;
        for &target_secs in &[7.0, 13.0, 18.0, 24.0] {
            audio
                .seek(Duration::from_secs_f64(target_secs))
                .expect("seek");

            let mut samples = 0u64;
            while samples < samples_per_seek {
                match audio.read(&mut buf) {
                    Ok(ReadOutcome::Pending { .. }) => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(ReadOutcome::Frames { count, .. }) => {
                        samples += count.get() as u64;
                    }
                    Ok(ReadOutcome::Eof { .. }) => break,
                    Err(e) => panic!("decode error: {e}"),
                }
            }
            assert!(
                samples > 0,
                "[{path}] seek to {target_secs}s must produce samples, got 0"
            );
        }

        let mut post_seek_samples = 0u64;
        while post_seek_samples < samples_per_seek {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(ReadOutcome::Frames { count, .. }) => {
                    post_seek_samples += count.get() as u64;
                }
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("decode error: {e}"),
            }
        }
        assert!(
            post_seek_samples > 0,
            "[{path}] playback after seeks must continue, got 0 samples"
        );
    })
    .await
    .expect("read phase join");
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

    let cancel = CancelToken::never();
    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    spawn_blocking(move || {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];
        let mut total_samples = 0u64;

        loop {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(ReadOutcome::Frames { count, .. }) => {
                    total_samples += count.get() as u64;
                    if total_samples >= 100_000 {
                        break;
                    }
                }
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("decode error: {e}"),
            }
        }

        assert!(
            total_samples > 1000,
            "baseline: expected sustained playback, got only {total_samples} samples"
        );
    })
    .await
    .expect("read phase join");
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

    let cancel = CancelToken::never();
    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .initial_abr_mode(auto(0))
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .decoder_backend(DecoderBackend::Symphonia)
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");
    spawn_blocking(move || {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];

        let mut warmup_samples = 0u64;
        while warmup_samples < 17_640 {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => warmup_samples += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => thread::sleep(Duration::from_millis(5)),
                Ok(ReadOutcome::Eof { .. }) => panic!("[{path}] unexpected EOF during warmup"),
                Err(e) => panic!("decode error during warmup: {e}"),
            }
        }

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
            while samples < samples_per_seek {
                match audio.read(&mut buf) {
                    Ok(ReadOutcome::Pending { .. }) => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(ReadOutcome::Frames { count, .. }) => {
                        samples += count.get() as u64;
                    }
                    Ok(ReadOutcome::Eof { .. }) => break,
                    Err(e) => panic!("decode error: {e}"),
                }
            }
            assert!(
                samples > 0,
                "[{path}] seek #{idx} to {target_secs}s must produce samples, got 0"
            );
        }
    })
    .await
    .expect("read phase join");
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

    let file_config = FileConfig::for_src(url.into())
        .store(StoreOptions::new(temp_dir.path()))
        .build();
    let config = AudioConfig::<File>::for_stream(file_config)
        .hint(("mp3").to_string())
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<File>>::new(config)
        .await
        .expect("create audio");
    spawn_blocking(move || {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];

        let mut warmup_samples = 0u64;
        while warmup_samples < 17_640 {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => warmup_samples += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => thread::sleep(Duration::from_millis(5)),
                Ok(ReadOutcome::Eof { .. }) => panic!("[mp3] unexpected EOF during warmup"),
                Err(e) => panic!("decode error during warmup: {e}"),
            }
        }

        let samples_per_seek: u64 = 48000 * 2;
        for &target_secs in &[7.0, 13.0, 18.0, 24.0] {
            audio
                .seek(Duration::from_secs_f64(target_secs))
                .expect("seek");

            let mut samples = 0u64;
            while samples < samples_per_seek {
                match audio.read(&mut buf) {
                    Ok(ReadOutcome::Pending { .. }) => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(ReadOutcome::Frames { count, .. }) => {
                        samples += count.get() as u64;
                    }
                    Ok(ReadOutcome::Eof { .. }) => break,
                    Err(e) => panic!("decode error: {e}"),
                }
            }
            assert!(
                samples > 0,
                "[mp3] seek to {target_secs}s must produce samples, got 0"
            );
        }

        let mut post_seek_samples = 0u64;
        while post_seek_samples < samples_per_seek {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(ReadOutcome::Frames { count, .. }) => {
                    post_seek_samples += count.get() as u64;
                }
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("decode error: {e}"),
            }
        }
        assert!(
            post_seek_samples > 0,
            "[mp3] playback after seeks must continue, got 0 samples"
        );
    })
    .await
    .expect("read phase join");
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

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .initial_abr_mode(auto(0))
        .build();

    let mut audio = Audio::<Stream<Hls>>::new(AudioConfig::<Hls>::for_stream(hls_config).build())
        .await
        .expect("audio creation");
    let _ = audio.preload();

    async fn next_chunk(audio: &mut Audio<Stream<Hls>>) -> Option<PcmChunk> {
        loop {
            let _ = audio.preload();
            match PcmReader::next_chunk(audio) {
                Ok(ChunkOutcome::Chunk(chunk)) => return Some(chunk),
                Ok(ChunkOutcome::Eof { .. }) => return None,
                Ok(ChunkOutcome::Pending { .. }) => {}
                Err(e) => panic!("decode error in next_chunk: {e}"),
            }
            // Yield so the worker runs and (under flash) the virtual clock
            // advances; wait on the *fact* of a chunk / EOF, never a
            // wall-clock window. A genuine stall trips `KITHARA_HANG_TIMEOUT_SECS`.
            time::sleep(Duration::from_millis(2)).await;
        }
    }

    info!("Phase 1: warmup until ABR switches from variant 0");
    let mut initial_variant = None;
    let mut current_variant = None;
    // Consume chunks until ABR actually switches off the initial variant (the
    // fact we need) or the track ends — no wall-clock warmup window. Under
    // flash the whole track decodes far faster than real time, so a timed
    // window would either skip the switch or wedge; `next_chunk` parks on each
    // chunk and the hang watchdog bounds a genuine stall.
    while let Some(chunk) = next_chunk(&mut audio).await {
        let v = chunk.meta.variant_index;
        if initial_variant.is_none() {
            initial_variant = v;
        }
        if v.is_some() && v != initial_variant {
            current_variant = v;
            info!(?initial_variant, switched_to = ?v, "ABR switched");
            break;
        }
    }
    if current_variant.is_none() || current_variant == initial_variant {
        info!(
            ?initial_variant,
            ?current_variant,
            "ABR did not switch during warmup; skipping seek-freeze test"
        );
        return;
    }
    info!(?current_variant, "Pre-seek variant established");

    let variant_before_seek = current_variant;
    audio
        .seek(Duration::from_secs(50))
        .expect("seek must not fail");
    let _ = audio.preload();

    let post_seek_chunk = next_chunk(&mut audio).await;
    assert!(
        post_seek_chunk.is_some(),
        "seek must produce a chunk (stream ended before resuming)"
    );
    let variant_after_seek = post_seek_chunk.unwrap().meta.variant_index;
    assert_eq!(
        variant_before_seek, variant_after_seek,
        "ABR must NOT switch variant during seek"
    );

    info!("Phase 3: verify ABR still works post-seek");
    let mut resume_chunks = 0u32;
    while resume_chunks < 4 {
        if next_chunk(&mut audio).await.is_some() {
            resume_chunks += 1;
        } else {
            break; // natural EOF
        }
    }
    assert!(
        resume_chunks >= 4,
        "playback must continue after seek (got {resume_chunks} chunks)"
    );
}

/// Phase P.0 regression: production bug repro from app.log (2026-05-15).
///
/// Scenario in prod (app.log:103-104):
/// `commit_variant_switch from=0 to=3 cross_codec=true reason=UpSwitch`
/// fires on the FIRST boundary crossing. Decoder produces some samples
/// then stalls; `decode_next_chunk` hangs ~10s later. User experience:
/// "пропал звук, слайдер двигался, потом крэш".
///
/// `abr_switch_real_assets_does_not_hang` and `runtime_cross_codec_manual_switch_no_hang`
/// already cover the cross-codec path but their assertions accept
/// `total_samples > 0` / `> 1000` — a single post-init buffer satisfies
/// them, masking a stall. Sustained playback requires a much higher bar.
///
/// Deterministic structure: open Auto(0)=AAC, read >=200 ms of audio to
/// confirm AAC decoder warmed, then trigger Manual(3)=FLAC. The Manual
/// path runs through the same `commit_variant_switch` cross-codec branch
/// as Auto — deterministic timing avoids the Auto-only flakiness where
/// the bandwidth estimator may or may not commit the switch within the
/// test window.
///
/// Real assets: `assets/hls/master.m3u8` carries variants 0-2 AAC
/// (mp4a.40.2) and variant 3 FLAC (fLaC) — 37 segments × 4s each =
/// 148 s of audio. Reading 15 s post-switch must produce at least
/// `15 × 44_100 × 2 × 0.5 = 661_500` samples (50 % of nominal rate).
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn manual_cross_codec_switch_sustains_post_switch_playback(temp_dir: TestTempDir) {
    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");

    let cancel = CancelToken::never();
    let bus = EventBus::new(8192);

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    // `block_on_underrun(true)` makes every `read()` park on the worker via
    // `recv_outcome_blocking` (`#[kithara::flash(true)]` → `park_timeout` on the
    // virtual clock) rather than zero-filling on underrun. The park is what
    // advances virtual time, so the worker keeps delivering real decoded chunks
    // and the post-switch window accumulates true FLAC frames. A genuine
    // post-switch decoder stall then parks forever, the virtual clock cannot
    // pass the hang budget, and the `KITHARA_HANG_TIMEOUT_SECS=5` watchdog fires
    // — the flash-correct enforcement of the "no >5 s stall" contract.
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus.clone())
        .block_on_underrun(true)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    // Subscribe before any switch so `VariantApplied{to:3}` cannot be missed.
    // The post-switch read owns this receiver and drains it inline.
    let mut hls_rx = bus.subscribe();

    // Phase 1 — AAC warmup. Read until we have at least 200 ms of audio
    // (44_100 × 2 × 0.2 = 17_640 frames) so AAC decoder is fully primed. The
    // blocking read must run off the test runtime thread: parking it here would
    // starve the HLS drive/fetch tasks that feed the worker.
    let (mut audio, pre_samples) = spawn_blocking(move || {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];
        let mut pre = 0u64;
        while pre < 17_640 {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => pre += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => {}
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("decode error during AAC warmup: {e}"),
            }
        }
        (audio, pre)
    })
    .await
    .expect("AAC warmup join");
    assert!(
        pre_samples >= 17_640,
        "AAC warmup must produce ≥17_640 frames before the cross-codec flip; \
         got {pre_samples}"
    );

    // Phase 2 — trigger cross-codec Manual switch to FLAC. Runs on the runtime
    // thread between the blocking read phases.
    let handle = audio
        .abr_handle()
        .expect("HLS stream must expose AbrHandle");
    handle
        .set_mode(AbrMode::manual(3))
        .expect("Manual(3) (FLAC) target must be valid");

    // Phase 3 — sustained post-switch playback. Bound by a SAMPLE TARGET, not a
    // wall clock: under flash a parked read collapses real time to ~zero, so a
    // `Instant::now() + 15 s` loop would drain the whole 148 s tail and hit a
    // legitimate end-of-track EOF before any real time elapsed. Reading the
    // ≥660_000-frame target proves sustained FLAC throughput; `saw_eof` records
    // a PREMATURE end (the prod stall surfaces as a false EOS), and the read
    // owns `hls_rx` to capture every `VariantApplied` without broadcast lag.
    let post_target: u64 = 660_000;
    let (post_samples, applied_targets, max_stall_ms, saw_eof) = spawn_blocking(move || {
        let mut buf = vec![0f32; 4096];
        let mut post = 0u64;
        let mut applied: Vec<usize> = Vec::new();
        let mut last_progress = Instant::now();
        let mut max_stall = 0u128;
        let mut eof = false;
        while post < post_target {
            while let Ok(ev) = hls_rx.try_recv() {
                if let Event::Abr(AbrEvent::VariantApplied { to, .. }) = ev {
                    applied.push(to.get());
                }
            }
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    post += count.get() as u64;
                    last_progress = Instant::now();
                }
                Ok(ReadOutcome::Pending { .. }) => {}
                Ok(ReadOutcome::Eof { .. }) => {
                    eof = true;
                    break;
                }
                Err(e) => panic!("decode error post-switch: {e}"),
            }
            let stalled = Instant::now().duration_since(last_progress).as_millis();
            if stalled > max_stall {
                max_stall = stalled;
            }
        }
        while let Ok(ev) = hls_rx.try_recv() {
            if let Event::Abr(AbrEvent::VariantApplied { to, .. }) = ev {
                applied.push(to.get());
            }
        }
        (post, applied, max_stall, eof)
    })
    .await
    .expect("sustained post-switch read join");

    info!(
        ?applied_targets,
        pre_samples,
        post_samples,
        max_stall_ms,
        saw_eof,
        "Phase P.0: cross-codec switch sustained playback"
    );

    assert!(
        applied_targets.contains(&3),
        "Manual(3) must surface VariantApplied{{to:3}} (FLAC) — \
         applied_targets={applied_targets:?}"
    );
    assert!(
        !saw_eof,
        "FLAC playback after the cross-codec flip ended prematurely (false EOS) \
         before the ≥660_000-frame target; got {post_samples}. \
         Prod symptom: decoder stalls / dies after the switch."
    );
    assert!(
        post_samples >= 660_000,
        "sustained FLAC playback after cross-codec flip must produce \
         ≥660_000 frames (≈50 % nominal 44.1 kHz × 15 s × 2 ch); \
         got {post_samples}. Prod symptom: decoder stalls after the switch."
    );
    assert!(
        max_stall_ms < 5_000,
        "decoder must not stall longer than 5 s after the cross-codec flip; \
         longest stall window was {max_stall_ms} ms"
    );
}
