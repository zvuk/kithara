use super::*;

/// One crossfade of each kind on a shared worker leaves no silence gap.
///
/// MP3→HLS, HLS→MP3 and MP3→MP3 each get their own render window: the mix
/// must keep carrying audio across a transition that swaps the source under it.
/// Repeating one kind until a rare gap surfaces belongs to
/// `crossfade_hls_to_mp3_repeats`, not here.
#[kithara::test(
    tokio,
    timeout(Duration::from_secs(60)),
    hang_timeout_secs(10),
    tracing("kithara_audio=debug,kithara_decode=debug,kithara_play=debug,kithara_stream=debug")
)]
async fn stress_offline_crossfade_no_gaps(
    #[future(awt)] open_audio_hls_server: CreatedHls,
    tone_mp3: &'static [u8],
) {
    use kithara_integration_tests::offline::OfflinePlayer;

    const BLOCK: usize = 512;
    const SR: u32 = 44100;
    let block_frames: f64 = BLOCK.as_();
    let block_budget = Duration::from_secs_f64(block_frames / f64::from(SR));

    let hls_server = open_audio_hls_server;
    let region = pools();
    let store = asset_store(&temp_dir(), true, &region);
    let hls_url = hls_server.master_url();

    let master_scope = CancelScope::new(None);
    let master_cancel = master_scope.token();
    let worker = play_worker_with_cancel(&region, master_cancel.child());
    let mut player = OfflinePlayer::new(
        HostConfig::offline(region.clone())
            .settings(
                HostSettings::builder()
                    .sample_rate(NonZeroU32::new(SR).expect("sample rate is non-zero"))
                    .build(),
            )
            .build(),
    )
    .await;

    let media_dir = temp_dir();
    let local_mp3 = media_dir.write("track.mp3", tone_mp3);

    let make_mp3 = |w: PlayWorker<TestPools>, s: AssetStore<TestPools>, cancel: CancelToken| {
        let p = local_mp3.clone();
        async move {
            ResourceConfig::for_src(ResourceSrc::Path(p))
                .store(s)
                .worker(w)
                .hint("mp3".to_string())
                .cancel(cancel)
                .build()
        }
    };

    let make_hls = |w: PlayWorker<TestPools>, s: AssetStore<TestPools>, cancel: CancelToken| {
        let u = hls_url.clone();
        async move {
            ResourceConfig::for_src(ResourceSrc::Url(u))
                .store(s)
                .worker(w)
                .hint("wav")
                .cancel(cancel)
                .build()
        }
    };

    let mp3_1 = make_mp3(worker.clone(), store.clone(), master_cancel.child()).await;
    player.load_config(mp3_1).await;
    let s1a = render_offline_window(&mut player, 40, "MP3 solo", BLOCK, SR).await;

    let hls_1 = make_hls(worker.clone(), store.clone(), master_cancel.child()).await;
    player.load_config(hls_1).await;
    let s1b = render_offline_window(&mut player, 80, "MP3→HLS fade", BLOCK, SR).await;

    let mp3_2 = make_mp3(worker.clone(), store.clone(), master_cancel.child()).await;
    player.load_config(mp3_2).await;
    let s2 = render_offline_window(&mut player, 80, "HLS→MP3 fade", BLOCK, SR).await;

    let mp3_3 = make_mp3(worker.clone(), store.clone(), master_cancel.child()).await;
    player.load_config(mp3_3).await;
    let s3 = render_offline_window(&mut player, 80, "MP3→MP3 fade", BLOCK, SR).await;

    info!("\n=== Stress crossfade results (budget={block_budget:?}) ===");
    for s in [&s1a, &s1b, &s2, &s3] {
        info!("  {s}");
    }

    master_scope.cancel();
    drop(worker);

    let all = [&s1b, &s2, &s3];
    for s in &all {
        assert!(
            s.max_silence_run <= 2,
            "{}: silence gap {} blocks ({:.1}ms) — audio underrun during crossfade",
            s.label,
            s.max_silence_run,
            f64::from(s.max_silence_run) * block_frames / f64::from(SR) * 1000.0,
        );
    }
    player.close().await;
}

/// MP3 through `ResourceConfig` (same path as kithara-app) must probe, decode,
/// and report correct duration — with and without extension/hint.
#[kithara::test(tokio, timeout(Duration::from_secs(15)), hang_timeout_secs(5))]
#[cfg_attr(not(target_os = "android"), case::with_extension_symphonia(mp3_extension().await, DecoderBackend::Symphonia))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::with_extension_apple(mp3_extension().await, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::with_extension_android(mp3_extension().await, DecoderBackend::Android)
)]
#[cfg_attr(not(target_os = "android"), case::no_extension_symphonia(mp3_no_extension().await, DecoderBackend::Symphonia))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::no_extension_apple(mp3_no_extension().await, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::no_extension_android(mp3_no_extension().await, DecoderBackend::Android)
)]
async fn resource_mp3_no_hint_decodes_with_duration(
    #[case] mp3_source: (TestServerHelper, url::Url),
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (_helper, url) = mp3_source;
    let region = pools();
    let store = asset_store(&temp_dir, true, &region);
    let path = url.as_str();

    let config = resource_config(&url, store, backend, None, play_worker(&region));
    let mut resource = kithara_integration_tests::mock::open_resource(&config)
        .await
        .unwrap_or_else(|e| panic!("Resource::new failed for path={path}: {e}"));

    let duration = resource.duration();
    assert!(
        duration.is_some(),
        "path={path}: duration must be reported (got None)"
    );
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        (dur_secs - consts::EXPECTED_DURATION_SECS).abs() < 2.0,
        "path={path}: expected ~{}s, got {dur_secs:.1}s",
        consts::EXPECTED_DURATION_SECS
    );

    let (samples, position) = {
        let mut total = 0usize;
        let mut buf = [0.0f32; 4096];
        let deadline = WallInstant::now() + consts::READ_TIMEOUT;
        let mut saw_eof = false;
        loop {
            match resource.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    let count = count.get();
                    if count > 0 {
                        total += count;
                    }
                }
                Ok(ReadOutcome::Eof { .. }) => {
                    saw_eof = true;
                    break;
                }
                Ok(ReadOutcome::Pending { .. }) => {}
                Err(e) => panic!("path={path}: decode error: {e}"),
            }
            if resource.position() >= Duration::from_secs(2) {
                break;
            }
            assert!(
                WallInstant::now() <= deadline,
                "path={path}: timed out waiting for PCM data"
            );
            sleep(Duration::from_millis(5)).await;
        }
        let _ = saw_eof;
        (total, resource.position())
    };

    assert!(samples > 0, "path={path}: must decode PCM samples");
    assert!(
        position >= Duration::from_secs(2),
        "path={path}: must decode at least 2s, got {position:?}"
    );
}

/// Local fixture (the generated ~187s MPEG clip, packaged AAC HLS ~64s) through
/// `ResourceConfig` — same code path as kithara-app. Mirrors
/// `live_remote_resource_decodes_with_duration` (now in
/// `live_remote_network.rs`) but against `TestServerHelper`, so it stays in the
/// regular suite: no VPN, no internet.
#[kithara::test(tokio, timeout(Duration::from_secs(30)), hang_timeout_secs(10))]
#[cfg_attr(not(target_os = "android"), case::mp3_symphonia(local_mp3().await, DecoderBackend::Symphonia))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::mp3_apple(local_mp3().await, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::mp3_android(local_mp3().await, DecoderBackend::Android)
)]
#[cfg_attr(not(target_os = "android"), case::hls_aac_symphonia(local_hls().await, DecoderBackend::Symphonia))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hls_aac_apple(local_hls().await, DecoderBackend::Apple)
)]
#[cfg_attr(
    target_os = "android",
    case::hls_aac_android(local_hls().await, DecoderBackend::Android)
)]
async fn local_resource_decodes_with_duration(
    #[case] local_source: (TestServerHelper, url::Url),
    #[case] backend: DecoderBackend,
    temp_dir: TestTempDir,
) {
    let (_helper, url) = local_source;
    let region = pools();
    let store = asset_store(&temp_dir, true, &region);
    let config: ResourceConfig<TestPools> =
        ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("valid URL"))
            .store(store)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .worker(play_worker(&region))
            .build();

    let mut resource = kithara_integration_tests::mock::open_resource(&config)
        .await
        .unwrap_or_else(|e| panic!("{url}: Resource::new failed: {e}"));

    let duration = resource.duration();
    assert!(duration.is_some(), "{url}: duration must be reported");
    let dur_secs = duration.expect("checked").as_secs_f64();
    assert!(
        dur_secs > 30.0,
        "{url}: expected duration > 30s, got {dur_secs:.1}s"
    );

    let deadline = WallInstant::now() + Duration::from_secs(20);
    let mut samples = 0usize;
    let mut buf = [0.0f32; 4096];
    loop {
        match resource.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => {
                let count = count.get();
                if count > 0 {
                    samples += count;
                }
            }
            Ok(ReadOutcome::Eof { .. }) => break,
            Ok(ReadOutcome::Pending { .. }) => {}
            Err(e) => panic!("{url}: decode error: {e}"),
        }
        if resource.position() >= Duration::from_secs(2) {
            break;
        }
        assert!(
            WallInstant::now() <= deadline,
            "{url}: timed out waiting for PCM (pos={:?}, samples={samples})",
            resource.position()
        );
        sleep(Duration::from_millis(5)).await;
    }

    assert!(samples > 0, "{url}: must decode PCM samples");
    assert!(
        resource.position() >= Duration::from_secs(2),
        "{url}: must decode at least 2s, got {:?}",
        resource.position()
    );
}
