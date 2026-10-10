use kithara::audio::AudioSession;
use kithara_integration_tests::output_continuity::{
    CONTINUITY_BLOCK_FRAMES, PlaybackProgressProbe, render_until_audible,
};
use kithara_test_fixtures::Mp3Shape;

use super::*;

#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(5))]
#[cfg_attr(not(target_os = "android"), case::symphonia(DecoderBackend::Symphonia))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::Android))]
async fn player_resource_repeated_unavailable_mp3_does_not_panic(
    #[future(awt)] mp3_endpoints: (TestServerHelper, url::Url, url::Url),
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (_server, ok_url, bad_url) = mp3_endpoints;
    let region = pools();
    let store = asset_store(&temp_dir, true, &region);

    let mut ok = open_resource(&ok_url, store.clone(), play_worker(&region), backend).await;
    assert!(read_some(&mut ok, "initial_ok").await > 0);
    let forward_pos = seek_and_read(&mut ok, Duration::from_secs(2), "ok_seek_forward").await;
    assert!(
        forward_pos > 1.0,
        "forward seek should advance playback position, got {forward_pos}"
    );
    drop(ok);

    for attempt in 0..2 {
        let result = kithara_integration_tests::mock::open_resource(&resource_config(
            &bad_url,
            store.clone(),
            backend,
            Some("mp3"),
            play_worker(&region),
        ))
        .await;
        assert!(
            result.is_err(),
            "unavailable resource attempt {attempt} must return error"
        );
    }

    let mut ok_again = open_resource(&ok_url, store, play_worker(&region), backend).await;
    let replay_pos = seek_and_read(
        &mut ok_again,
        Duration::from_secs(1),
        "ok_after_unavailable_replay",
    )
    .await;
    assert!(
        replay_pos > 0.5,
        "reopened valid resource should remain seekable after failed transitions, got {replay_pos}"
    );
}

#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(5))]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case::disk_symphonia(false, DecoderBackend::Symphonia, Mp3Shape::Tagged)
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case::disk_symphonia_headerless(false, DecoderBackend::Symphonia, Mp3Shape::Headerless)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::disk_apple(false, DecoderBackend::Apple, Mp3Shape::Tagged)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::disk_apple_headerless(false, DecoderBackend::Apple, Mp3Shape::Headerless)
)]
#[cfg_attr(
    target_os = "android",
    case::disk_android(false, DecoderBackend::Android, Mp3Shape::Tagged)
)]
#[cfg_attr(
    not(target_os = "android"),
    case::ephemeral_symphonia(true, DecoderBackend::Symphonia, Mp3Shape::Tagged)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::ephemeral_apple(true, DecoderBackend::Apple, Mp3Shape::Tagged)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::ephemeral_apple_headerless(true, DecoderBackend::Apple, Mp3Shape::Headerless)
)]
#[cfg_attr(
    target_os = "android",
    case::ephemeral_android(true, DecoderBackend::Android, Mp3Shape::Tagged)
)]
async fn player_resource_mp3_reopen_same_cache_keeps_backward_seek(
    tone_mp3: &'static [u8],
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    #[case] shape: Mp3Shape,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (_server, ok_url, _) = mp3_endpoints_for_bytes(shape.apply(tone_mp3)).await;
    let region = pools();
    let store = asset_store(&temp_dir, ephemeral, &region);

    let mut first = open_resource(&ok_url, store.clone(), play_worker(&region), backend).await;
    assert!(read_some(&mut first, "first_initial").await > 0);
    let first_forward = seek_and_read(&mut first, Duration::from_secs(3), "first_forward").await;
    let first_backward =
        seek_and_read(&mut first, Duration::from_millis(500), "first_backward").await;
    assert!(
        first_backward < first_forward,
        "first session backward seek should move position back (forward={first_forward}, backward={first_backward})"
    );
    drop(first);

    let mut second = open_resource(&ok_url, store, play_worker(&region), backend).await;
    assert!(read_some(&mut second, "second_initial").await > 0);
    let second_forward = seek_and_read(&mut second, Duration::from_secs(3), "second_forward").await;
    let second_backward =
        seek_and_read(&mut second, Duration::from_millis(500), "second_backward").await;
    assert!(
        second_backward < second_forward,
        "reopened session backward seek should still move position back (forward={second_forward}, backward={second_backward})"
    );
}

#[kithara::test(
    tokio,
    browser,
    flash(false),
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(5)
)]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case::disk_symphonia(false, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::disk_apple(false, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::disk_android(false, DecoderBackend::Android)
)]
#[cfg_attr(
    not(target_os = "android"),
    case::ephemeral_symphonia(true, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::ephemeral_apple(true, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::ephemeral_android(true, DecoderBackend::Android)
)]
async fn player_worker_hls_then_unavailable_mp3_then_mp3_recovery(
    #[future(awt)] open_audio_hls_server: CreatedHls,
    #[future(awt)] mp3_endpoints: (TestServerHelper, url::Url, url::Url),
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let hls_server = open_audio_hls_server;
    let (_server, ok_url, bad_url) = mp3_endpoints;
    let region = pools();
    let worker = play_worker(&region);
    let store = asset_store(&temp_dir, ephemeral, &region);
    let hls_url = hls_server.master_url();

    let hls_pos = warm_hls_worker(
        &hls_url,
        store.clone(),
        worker.clone(),
        backend,
        Some(Duration::from_secs(2)),
    )
    .await;
    assert!(
        hls_pos > 1.0,
        "HLS warmup seek should advance playback position, got {hls_pos}"
    );

    for attempt in 0..2 {
        let result = kithara_integration_tests::mock::open_resource(&resource_config(
            &bad_url,
            store.clone(),
            backend,
            Some("mp3"),
            worker.clone(),
        ))
        .await;
        assert!(
            result.is_err(),
            "unavailable mp3 attempt {attempt} must return error"
        );
    }

    let mut ok = open_resource(&ok_url, store, worker, backend).await;
    assert!(read_some(&mut ok, "mp3_after_hls_initial").await > 0);
    let forward = seek_and_read(
        &mut ok,
        Duration::from_secs(3),
        "mp3_after_hls_seek_forward",
    )
    .await;
    let backward = seek_and_read(
        &mut ok,
        Duration::from_millis(500),
        "mp3_after_hls_seek_backward",
    )
    .await;
    assert!(
        backward < forward,
        "mp3 recovery path must keep backward seek after HLS transition (forward={forward}, backward={backward})"
    );
}

/// How the first warmup session ends before the second begins.
#[derive(Debug, Clone, Copy)]
enum WarmupTeardown {
    /// Explicit cancellation of the worker's parent token.
    Shutdown,
    /// Drop `worker_a` without shutdown.
    DropOnly,
    /// First session is a read-only warmup (no seek), then drop.
    ReadOnlyThenDrop,
}

/// Sequential HLS warmups from two isolated sessions must not poison each
/// other. Covers three teardown modes for the first session.
#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(5))]
#[cfg_attr(
    not(target_os = "android"),
    case::shutdown_symphonia(WarmupTeardown::Shutdown, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::shutdown_apple(WarmupTeardown::Shutdown, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::shutdown_android(WarmupTeardown::Shutdown, DecoderBackend::Android)
)]
#[cfg_attr(
    not(target_os = "android"),
    case::drop_only_symphonia(WarmupTeardown::DropOnly, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::drop_only_apple(WarmupTeardown::DropOnly, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::drop_only_android(WarmupTeardown::DropOnly, DecoderBackend::Android)
)]
#[cfg_attr(
    not(target_os = "android"),
    case::read_only_symphonia(WarmupTeardown::ReadOnlyThenDrop, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::read_only_apple(WarmupTeardown::ReadOnlyThenDrop, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::read_only_android(WarmupTeardown::ReadOnlyThenDrop, DecoderBackend::Android)
)]
async fn sequential_hls_warmup_does_not_poison_next_ephemeral_session(
    #[future(awt)] audio_hls_pair: (CreatedHls, CreatedHls),
    #[case] teardown: WarmupTeardown,
    #[case] backend: DecoderBackend,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (server_a, server_b) = audio_hls_pair;
    let temp_a = TestTempDir::new();
    let temp_b = TestTempDir::new();
    let region_a = pools();
    let region_b = pools();
    let worker_a_scope = CancelScope::new(None);
    let worker_a = play_worker_with_cancel(&region_a, worker_a_scope.token());
    let worker_b = play_worker(&region_b);
    let store_a = asset_store(&temp_a, false, &region_a);
    let store_b = asset_store(&temp_b, true, &region_b);
    let hls_url_a = server_a.master_url();
    let hls_url_b = server_b.master_url();

    let first_pos = match teardown {
        WarmupTeardown::Shutdown | WarmupTeardown::DropOnly => {
            warm_hls_worker(
                &hls_url_a,
                store_a,
                worker_a.clone(),
                backend,
                Some(Duration::from_secs(2)),
            )
            .await
        }
        WarmupTeardown::ReadOnlyThenDrop => {
            warm_hls_worker(&hls_url_a, store_a, worker_a.clone(), backend, None).await
        }
    };

    let expect_advance = !matches!(teardown, WarmupTeardown::ReadOnlyThenDrop);
    if expect_advance {
        assert!(
            first_pos > 1.0,
            "first HLS warmup must advance playback position, got {first_pos} \
             (teardown={teardown:?})",
        );
    } else {
        assert!(
            first_pos >= 0.0,
            "first HLS read-only warmup must produce samples, got {first_pos}",
        );
    }

    match teardown {
        WarmupTeardown::Shutdown => {
            worker_a_scope.cancel();
            drop(worker_a);
        }
        WarmupTeardown::DropOnly | WarmupTeardown::ReadOnlyThenDrop => drop(worker_a),
    }

    let second_pos = warm_hls_worker(
        &hls_url_b,
        store_b,
        worker_b.clone(),
        backend,
        Some(Duration::from_secs(2)),
    )
    .await;
    assert!(
        second_pos > 1.0,
        "second HLS warmup after a prior session ({teardown:?}) must still \
         advance playback position, got {second_pos}",
    );
    drop(worker_b);
}

#[kithara::test(
    tokio,
    multi_thread,
    browser,
    timeout(Duration::from_secs(10)),
    hang_timeout_secs(5)
)]
async fn sequential_hls_stream_sessions_do_not_poison_next_ephemeral_session(
    #[future(awt)] audio_hls_pair: (CreatedHls, CreatedHls),
) {
    let (server_a, server_b) = audio_hls_pair;
    let temp_a = TestTempDir::new();
    let temp_b = TestTempDir::new();
    let pools_a = pools();
    let pools_b = pools();
    let store_a = asset_store(&temp_a, false, &pools_a);
    let store_b = asset_store(&temp_b, true, &pools_b);
    let hls_url_a = server_a.master_url();
    let hls_url_b = server_b.master_url();

    let first_read = read_hls_stream_some(&hls_url_a, store_a, &pools_a).await;
    assert!(first_read > 0, "first HLS stream session must read bytes");

    let second_read = read_hls_stream_some(&hls_url_b, store_b, &pools_b).await;
    assert!(second_read > 0, "second HLS stream session must read bytes");
}

#[kithara::test(tokio, native, timeout(Duration::from_secs(25)), hang_timeout_secs(3))]
#[cfg_attr(not(target_os = "android"), case::aac_symphonia(AudioCodec::AacLc, DecoderBackend::Symphonia, aac_source().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::aac_apple(AudioCodec::AacLc, DecoderBackend::Apple, aac_source().await)
)]
#[cfg_attr(
    target_os = "android",
    case::aac_android(AudioCodec::AacLc, DecoderBackend::Android, aac_source().await)
)]
#[cfg_attr(not(target_os = "android"), case::flac_symphonia(AudioCodec::Flac, DecoderBackend::Symphonia, flac_source().await))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::flac_apple(AudioCodec::Flac, DecoderBackend::Apple, flac_source().await)
)]
#[cfg_attr(
    target_os = "android",
    case::flac_android(AudioCodec::Flac, DecoderBackend::Android, flac_source().await)
)]
async fn packaged_hls_single_variant_continuity_is_stable(
    #[case] codec: AudioCodec,
    #[case] backend: DecoderBackend,
    #[case] packaged_source: (TestServerHelper, url::Url),
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    use kithara_integration_tests::offline::OfflinePlayer;

    let (_server, url) = packaged_source;
    let region = pools();
    let store = asset_store(&temp_dir, false, &region);

    let mut progress_audio =
        open_packaged_hls_audio(&url, store.clone(), play_worker(&region), codec, backend).await;
    let mut progress_rx = progress_audio.event_bus().subscribe();
    let mut progress_probe = PlaybackProgressProbe::default();
    let mut total_samples = 0u64;
    let mut buf = [0.0f32; 4096];
    total_samples += read_audio_some(&mut progress_audio, "packaged_progress_warmup").await as u64;
    progress_probe.drain(&mut progress_rx);
    let started = WallInstant::now();
    let deadline = started + Duration::from_secs(4);
    let mut frame_reads = 0u64;
    let mut pending_reads = 0u64;
    let mut saw_eof = false;
    while WallInstant::now() < deadline && progress_probe.progress_events < 10 {
        progress_audio.preload().expect("preload must succeed");
        let read_count = match progress_audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => count.get(),
            Ok(ReadOutcome::Pending { .. }) => 0,
            Ok(ReadOutcome::Eof { .. }) => {
                saw_eof = true;
                progress_probe.drain(&mut progress_rx);
                break;
            }
            Err(e) => panic!("decode error during progress tracking: {e}"),
        };
        progress_probe.drain(&mut progress_rx);
        if read_count == 0 {
            pending_reads += 1;
            sleep(Duration::from_millis(10)).await;
            progress_probe.observe_idle();
            continue;
        }
        frame_reads += 1;
        total_samples += read_count as u64;
    }
    progress_probe.drain(&mut progress_rx);
    progress_probe.observe_idle();
    let elapsed = started.elapsed();
    assert!(
        total_samples > 0,
        "{codec:?}: expected decoded output during progress tracking; \
         frame_reads={frame_reads}, pending_reads={pending_reads}, saw_eof={saw_eof}, \
         elapsed={elapsed:?}"
    );
    assert!(
        progress_probe.progress_events >= 4,
        "{codec:?}: expected PlaybackProgress events, got {}; total_samples={total_samples}, \
         frame_reads={frame_reads}, pending_reads={pending_reads}, saw_eof={saw_eof}, \
         elapsed={elapsed:?}",
        progress_probe.progress_events
    );
    assert_eq!(
        progress_probe.regressions, 0,
        "{codec:?}: PlaybackProgress moved backward"
    );
    assert!(
        progress_probe.max_gap_between_events < Duration::from_millis(1_200),
        "{codec:?}: PlaybackProgress stalled for {:?}; total_samples={total_samples}, \
         frame_reads={frame_reads}, pending_reads={pending_reads}, saw_eof={saw_eof}, \
         progress_events={}, elapsed={elapsed:?}",
        progress_probe.max_gap_between_events,
        progress_probe.progress_events
    );

    let resource = resource_config(&url, store, backend, None, play_worker(&region));
    let mut player = OfflinePlayer::new(
        HostConfig::offline(region.clone())
            .settings(
                HostSettings::builder()
                    .sample_rate(
                        NonZeroU32::new(CONTINUITY_SAMPLE_RATE).expect("sample rate is non-zero"),
                    )
                    .build(),
            )
            .build(),
    )
    .await;
    player.load_config(resource).await;
    render_until_audible(
        &mut player,
        "packaged warmup",
        CONTINUITY_BLOCK_FRAMES,
        CONTINUITY_SAMPLE_RATE,
    )
    .await;
    let steady = render_offline_window(
        &mut player,
        80,
        "packaged steady-state",
        CONTINUITY_BLOCK_FRAMES,
        CONTINUITY_SAMPLE_RATE,
    )
    .await;
    assert!(
        steady.max_silence_run <= 1,
        "{codec:?}: offline output produced {} silent blocks ({steady})",
        steady.max_silence_run
    );
    player.close().await;
}

#[kithara::test(tokio, browser, timeout(Duration::from_secs(10)), hang_timeout_secs(5))]
#[cfg_attr(
    all(not(target_arch = "wasm32"), not(target_os = "android")),
    case::disk_symphonia(false, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::disk_apple(false, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::disk_android(false, DecoderBackend::Android)
)]
#[cfg_attr(
    not(target_os = "android"),
    case::ephemeral_symphonia(true, DecoderBackend::Symphonia)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::ephemeral_apple(true, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::ephemeral_android(true, DecoderBackend::Android)
)]
async fn player_worker_hls_then_mp3_reopen_keeps_backward_seek(
    #[future(awt)] open_audio_hls_server: CreatedHls,
    #[future(awt)] mp3_endpoints: (TestServerHelper, url::Url, url::Url),
    #[case] ephemeral: bool,
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let hls_server = open_audio_hls_server;
    let (_server, ok_url, _) = mp3_endpoints;
    let region = pools();
    let worker = play_worker(&region);
    let store = asset_store(&temp_dir, ephemeral, &region);
    let hls_url = hls_server.master_url();

    let hls_seek = warm_hls_worker(
        &hls_url,
        store.clone(),
        worker.clone(),
        backend,
        Some(Duration::from_secs(2)),
    )
    .await;
    assert!(
        hls_seek > 1.0,
        "HLS warmup should advance playback position before mp3 transition, got {hls_seek}"
    );

    let mut first = open_resource(&ok_url, store.clone(), worker.clone(), backend).await;
    assert!(read_some(&mut first, "mp3_first_initial").await > 0);
    let first_forward = seek_and_read(
        &mut first,
        Duration::from_secs(3),
        "mp3_first_forward_after_hls",
    )
    .await;
    let first_backward = seek_and_read(
        &mut first,
        Duration::from_millis(500),
        "mp3_first_backward_after_hls",
    )
    .await;
    assert!(
        first_backward < first_forward,
        "first mp3 session after HLS must keep backward seek (forward={first_forward}, backward={first_backward})"
    );
    drop(first);

    let mut second = open_resource(&ok_url, store, worker, backend).await;
    assert!(read_some(&mut second, "mp3_second_initial").await > 0);
    let second_forward = seek_and_read(
        &mut second,
        Duration::from_secs(3),
        "mp3_second_forward_after_hls",
    )
    .await;
    let second_backward = seek_and_read(
        &mut second,
        Duration::from_millis(500),
        "mp3_second_backward_after_hls",
    )
    .await;
    assert!(
        second_backward < second_forward,
        "reopened mp3 session after HLS must keep backward seek (forward={second_forward}, backward={second_backward})"
    );
}
