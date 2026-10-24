use std::num::NonZeroU32;

use kithara::{
    abr::{AbrEvent, AbrReason},
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, NoResamplerBackend, ReadOutcome},
    download::{Downloader, DownloaderConfig},
    events::{EventBus, EventReceiver},
    hls::{AbrMode, Hls, HlsConfig},
    host::{HostConfig, HostSettings},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        thread::paced_backoff,
        time,
        time::{Duration, Instant, WallInstant},
        tokio::task::spawn_blocking,
    },
    play::{PlayWorker, PlayWorkerConfig},
    stream::{AudioCodec, Stream},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, abr_fast, auto,
    bufpool_ext::{Pools, TestPools, pools},
    event::TestEvent,
    fixture_protocol::{DelayRule, PcmPattern},
    hls_fixture::create_test_downloader,
    mock::{LaneAudio, LaneLoader},
    offline::OfflinePlayer,
    output_continuity::{
        CONTINUITY_BLOCK_FRAMES, CONTINUITY_SAMPLE_RATE, PlaybackProgressProbe,
        render_offline_window, render_until_audible,
    },
};
use kithara_test_utils::{TestTempDir, pace};
use tracing::info;
use url::Url;

fn packaged_switch_abr_mode() -> AbrMode {
    auto(0)
}

fn packaged_identical_content_abr_builder(codec: AudioCodec) -> HlsFixtureBuilder {
    let builder = HlsFixtureBuilder::new()
        .variant_count(2)
        .segments_per_variant(6)
        .segment_duration_secs(2.0)
        .variant_bandwidths(vec![10_000, 50_000])
        .delay_rules(vec![DelayRule {
            variant: Some(0),
            segment_eq: Some(2),
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

#[kithara::fixture]
pub(crate) async fn create_packaged_abr_fixture() -> (TestServerHelper, Url) {
    let server = TestServerHelper::new().await;
    let created = server
        .create_hls(packaged_identical_content_abr_builder(AudioCodec::AacLc))
        .await
        .unwrap_or_else(|error| panic!("create packaged ABR fixture: {error}"));
    (server, created.master_url())
}

/// Open the packaged ABR fixture as decoded audio.
///
/// `block_on_underrun` decides what an empty producer ring returns. A caller
/// that drives the read loop itself and handles [`ReadOutcome::Pending`] wants
/// `false`; a caller that reads the rendered PCM back wants `true`, because a
/// zero-filled range is indistinguishable from rendered silence.
async fn open_packaged_hls_audio(
    loader: &mut LaneLoader<Hls<TestPools>, NoResamplerBackend, TestPools>,
    pools: &Pools,
    url: &Url,
    store: AssetStore<TestPools>,
    abr: AbrMode,
    bus: Option<EventBus>,
    block_on_underrun: bool,
) -> LaneAudio<Stream<Hls<TestPools>>, TestPools> {
    let cancel = CancelToken::never();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            cancel.child(),
        ))
        .runtime(kithara::platform::tokio::runtime::Handle::current())
        .abr_settings(abr_fast())
        .cancel(cancel.child())
        .build(),
    );
    let hls_config = HlsConfig::for_url(url.clone())
        .store(store)
        .pools(pools.clone())
        .initial_abr_mode(abr)
        .download_batch_size(1)
        .cancel(cancel)
        .downloader(downloader)
        .maybe_events(bus.clone())
        .build();

    let config = kithara::play::TrackConfig::for_audio(
        AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
            .maybe_events(bus)
            .build(),
    )
    .block_on_underrun(block_on_underrun)
    .build();

    let mut audio = loader
        .load(config)
        .await
        .unwrap_or_else(|err| panic!("packaged ABR audio should open for {url}: {err}"));
    let _ = audio.preload();
    audio
}

#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
async fn read_audio_some(
    audio: &mut LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    stage: &str,
) -> usize {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0.0f32; 4096];

    loop {
        hang_tick!();
        let _ = audio.preload();
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => {
                hang_reset!();
                return count.get();
            }
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
        time::sleep(Duration::from_millis(10)).await;
    }
}

/// Real fMP4/AAC HLS stream with ABR auto-switch must play without hanging.
///
/// This is the exact scenario from the production crash:
/// `kithara-app` plays one MP3 plus the plain and encrypted HLS masters.
/// ABR switches variant on HLS track → worker hangs → all tracks die.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub(crate) async fn abr_switch_on_production_ladder_does_not_hang(
    temp_dir: TestTempDir,
    mixed_plain: (TestServerHelper, Url),
) {
    let (_server, url) = mixed_plain;

    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );
    let hls_config = HlsConfig::for_url(url)
        .downloader(create_test_downloader())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools.clone())
        .cancel(cancel)
        .initial_abr_mode(auto(0))
        .build();

    let config = kithara::play::TrackConfig::for_audio(
        AudioConfig::<Hls<TestPools>>::for_stream(hls_config).build(),
    )
    .block_on_underrun(true)
    .build();
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("create audio");
    spawn_blocking(move || {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];
        let mut total_samples = 0u64;

        let mut saw_eof = false;
        loop {
            hang_tick!();
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => {
                    pace(Duration::from_millis(10));
                }
                Ok(ReadOutcome::Frames { count, .. }) => {
                    total_samples += count.get() as u64;
                    hang_reset!();
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

/// A packaged ABR variant switch leaves no gap in the rendered PCM.
///
/// The seam window reads its own output back, so it renders over a reader that
/// parks on an empty producer ring: a zero-filled underrun would otherwise
/// arrive as silence the decoder never produced, and the measurement would read
/// how promptly the machine scheduled the decode thread instead of what the
/// switch did to the signal. With the park in place a silent block can only be
/// decoded silence, so the run has no tolerance to spend.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub(crate) async fn packaged_abr_switch_keeps_player_continuity(
    temp_dir: TestTempDir,
    create_packaged_abr_fixture: (TestServerHelper, Url),
) {
    let (_server, url) = create_packaged_abr_fixture;
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let store = AssetStore::builder(pools.clone())
        .backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        })
        .build();

    let bus = EventBus::new(64);
    let mut hls_rx = bus.subscribe();
    let mut loader = LaneLoader::new(&worker).expect("packaged ABR lane loader");
    let mut progress_audio = open_packaged_hls_audio(
        &mut loader,
        &pools,
        &url,
        store.clone(),
        packaged_switch_abr_mode(),
        Some(bus.clone()),
        false,
    )
    .await;
    let abr = progress_audio
        .abr_handle()
        .expect("packaged HLS audio must expose AbrHandle");
    let mut progress_rx = progress_audio.event_bus().subscribe();
    let mut progress_probe = PlaybackProgressProbe::default();
    let mut switch_count = 0usize;
    let mut switch_seen = false;
    let mut total_samples = 0u64;
    let mut post_switch_samples = 0u64;
    let mut consumed = Duration::ZERO;
    let mut reevaluated = false;
    let mut buf = vec![0.0f32; 4096];
    total_samples += read_audio_some(&mut progress_audio, "packaged_abr_warmup").await as u64;
    progress_probe.drain(&mut progress_rx);
    {
        let _real_io = kithara::platform::flash::real_io();
        let deadline = WallInstant::now() + Duration::from_secs(8);

        while WallInstant::now() < deadline {
            hang_tick!();
            let _ = progress_audio.preload();
            let read: usize = match progress_audio.read(&mut buf) {
                Ok(ReadOutcome::Frames {
                    count, position, ..
                }) => {
                    if !switch_seen {
                        time::sleep(position.saturating_sub(consumed)).await;
                    }
                    consumed = position;
                    if !reevaluated && consumed >= Duration::from_secs(2) {
                        abr.reevaluate();
                        reevaluated = true;
                    }
                    hang_reset!();
                    count.get()
                }
                Ok(ReadOutcome::Pending { .. }) => 0,
                Ok(ReadOutcome::Eof { .. }) => {
                    progress_probe.drain(&mut progress_rx);
                    break;
                }
                Err(e) => panic!("decode error: {e}"),
            };
            progress_probe.drain(&mut progress_rx);
            loop {
                hang_tick!();
                match hls_rx.try_recv().map(|env| env.event) {
                    Ok(TestEvent::Abr(AbrEvent::VariantApplied { .. })) => {
                        hang_reset!();
                        switch_count += 1;
                        switch_seen = true;
                    }
                    Ok(_) => {
                        hang_reset!();
                    }
                    Err(_) => break,
                }
            }

            if read == 0 {
                spawn_blocking(|| paced_backoff(Duration::from_millis(10)))
                    .await
                    .expect("packaged ABR pending pace");
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

    drop(progress_audio);

    // Prime the shared store with EVERY segment of BOTH variants FIRST, on the
    // blocking pool, so the offline render below decodes from cache: the V0
    // segment delay rule would otherwise park the seam read past the render's
    // `hang_timeout_secs`. Forcing each variant with a manual mode guarantees
    // full coverage regardless of which path the offline render's `auto(0)` ABR
    // then takes.
    for variant in [0usize, 1usize] {
        let warm_config = kithara::play::TrackConfig::for_audio(
            AudioConfig::<Hls<TestPools>>::for_stream(
                HlsConfig::for_url(url.clone())
                    .downloader(create_test_downloader())
                    .store(store.clone())
                    .pools(pools.clone())
                    .initial_abr_mode(AbrMode::manual(variant))
                    .download_batch_size(1)
                    .build(),
            )
            .build(),
        )
        .block_on_underrun(true)
        .build();
        let warm_audio = loader
            .load(warm_config)
            .await
            .unwrap_or_else(|e| panic!("packaged ABR warm audio (v{variant}) must open: {e}"));
        #[kithara::flash(io)]
        #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
        fn warm_packaged_audio(
            mut warm_audio: LaneAudio<Stream<Hls<TestPools>>, TestPools>,
            variant: usize,
        ) -> u64 {
            let _ = warm_audio.preload();
            let mut buf = vec![0f32; 4096];
            let mut total = 0u64;
            loop {
                hang_tick!();
                match warm_audio.read(&mut buf) {
                    Ok(ReadOutcome::Frames { count, .. }) => {
                        total += count.get() as u64;
                        hang_reset!();
                    }
                    Ok(ReadOutcome::Pending { .. }) => {
                        hang_park!(|remaining: Duration| pace(
                            remaining.min(Duration::from_micros(500))
                        ));
                    }
                    Ok(ReadOutcome::Eof { .. }) => {
                        hang_reset!();
                        break;
                    }
                    Err(e) => panic!("packaged ABR warm decode error (v{variant}): {e}"),
                }
            }
            total
        }
        let warmed = spawn_blocking(move || warm_packaged_audio(warm_audio, variant))
            .await
            .expect("packaged ABR warm read join");
        assert!(
            warmed > 0,
            "packaged ABR warm pass (v{variant}) must decode the variant into the shared store"
        );
    }

    let mut player = OfflinePlayer::new(
        HostConfig::offline(pools.clone())
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
    let cancel = kithara_test_utils::cancel_token();
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            cancel.child(),
        ))
        .runtime(kithara::platform::tokio::runtime::Handle::current())
        .abr_settings(abr_fast())
        .cancel(cancel.child())
        .build(),
    );
    let mut hls = kithara::hls::HlsConfigPatch::default();
    hls.download_batch_size = Some(1);
    let resource =
        kithara::play::ResourceConfig::for_src(kithara::play::ResourceSrc::Url(url.clone()))
            .store(store)
            .downloader(downloader)
            .cancel(cancel)
            .initial_abr_mode(packaged_switch_abr_mode())
            .hls(hls)
            .build();
    player
        .load_and_fadein(kithara::queue::TrackSource::from(resource))
        .await;
    render_until_audible(
        &mut player,
        "packaged abr warmup",
        CONTINUITY_BLOCK_FRAMES,
        CONTINUITY_SAMPLE_RATE,
    )
    .await;
    let seam = render_offline_window(
        &mut player,
        220,
        "packaged abr seam window",
        CONTINUITY_BLOCK_FRAMES,
        CONTINUITY_SAMPLE_RATE,
    )
    .await;
    assert_eq!(
        seam.max_silence_run, 0,
        "packaged ABR switch produced {} silent blocks ({seam})",
        seam.max_silence_run
    );
    player.close().await;
}

#[derive(Debug)]
struct CrossCodecReadStats {
    post_samples: u64,
    applied_transitions: Vec<(usize, AbrReason)>,
    max_stall_ms: u128,
    saw_eof: bool,
}

#[kithara::flash(io)]
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn read_manual_cross_codec_phase(
    audio: &mut LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    hls_rx: &mut EventReceiver<TestEvent>,
    post_target: u64,
) -> CrossCodecReadStats {
    let mut buf = vec![0f32; 4096];
    let mut stats = CrossCodecReadStats {
        post_samples: 0,
        applied_transitions: Vec::new(),
        max_stall_ms: 0,
        saw_eof: false,
    };
    let mut transition_samples = 0u64;
    let mut consumed = Duration::ZERO;
    let mut manual_applied = false;
    let mut last_progress = Instant::now();

    while !manual_applied || stats.post_samples < post_target {
        hang_tick!();
        while let Ok(event) = hls_rx.try_recv().map(|envelope| envelope.event) {
            hang_tick!();
            hang_reset!();
            if let TestEvent::Abr(AbrEvent::VariantApplied { to, reason, .. }) = event {
                let transition = (to.get(), reason);
                stats.applied_transitions.push(transition);
                if !manual_applied && transition == (3, AbrReason::ManualOverride) {
                    manual_applied = true;
                    stats.post_samples = 0;
                    stats.max_stall_ms = 0;
                    last_progress = Instant::now();
                }
            }
        }

        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames {
                count, position, ..
            }) => {
                let count = u64::try_from(count.get())
                    .unwrap_or_else(|error| panic!("read count does not fit u64: {error}"));
                hang_reset!();
                if manual_applied {
                    stats.post_samples += count;
                    last_progress = Instant::now();
                } else {
                    transition_samples += count;
                    if transition_samples >= 17_640 {
                        pace(position.saturating_sub(consumed));
                    }
                    consumed = position;
                }
            }
            Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) if !manual_applied => {
                panic!(
                    "EOF before Manual(3) produced VariantApplied{{to:3, \
                     reason:ManualOverride}}; observed {:?}",
                    stats.applied_transitions
                );
            }
            Ok(ReadOutcome::Eof { .. }) => {
                stats.saw_eof = true;
                break;
            }
            Err(error) => panic!("decode error post-switch: {error}"),
        }

        if manual_applied {
            let stalled = Instant::now().duration_since(last_progress).as_millis();
            if stalled > stats.max_stall_ms {
                stats.max_stall_ms = stalled;
            }
        }
    }

    stats
}

/// Phase P.0 regression: production bug repro from app.log (2026-05-15).
///
/// Scenario in prod (app.log:103-104):
/// `commit_variant_switch from=0 to=3 cross_codec=true reason=UpSwitch`
/// fires on the FIRST boundary crossing. Decoder produces some samples
/// then stalls; `decode_next_chunk` hangs ~10s later. User experience:
/// "audio disappeared, the slider kept moving, then the app crashed".
///
/// `abr_switch_on_production_ladder_does_not_hang` and `runtime_cross_codec_manual_switch_no_hang`
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
/// The mixed-codec ladder carries variants 0-2 AAC (mp4a.40.2) and
/// variant 3 FLAC — 37 segments × 6 s each, so about 220 s of audio.
/// Reading 15 s post-switch must produce at least
/// `15 × 44_100 × 2 × 0.5 = 661_500` samples (50 % of nominal rate).
pub(crate) async fn manual_cross_codec_switch_sustains_post_switch_playback(
    temp_dir: TestTempDir,
    mixed_plain: (TestServerHelper, Url),
) {
    let (_server, url) = mixed_plain;

    let cancel = CancelToken::never();
    let bus = EventBus::new(8192);
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );

    let hls_config = HlsConfig::for_url(url)
        .downloader(create_test_downloader())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools.clone())
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
    let config = kithara::play::TrackConfig::for_audio(
        AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
            .events(bus.clone())
            .build(),
    )
    .block_on_underrun(true)
    .build();
    let audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("create audio");

    // Subscribe before any switch so `VariantApplied{to:3}` cannot be missed.
    // The post-switch read owns this receiver and drains it inline.
    let mut hls_rx = bus.subscribe();

    // Phase 1 — AAC warmup. Read until we have at least 200 ms of audio
    // (44_100 × 2 × 0.2 = 17_640 frames) so AAC decoder is fully primed. The
    // blocking read must run off the test runtime thread: parking it here would
    // starve the HLS drive/fetch tasks that feed the worker.
    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    fn warm_cross_codec_audio(
        mut audio: LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    ) -> (LaneAudio<Stream<Hls<TestPools>>, TestPools>, u64) {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];
        let mut pre = 0u64;
        while pre < 17_640 {
            hang_tick!();
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    pre += count.get() as u64;
                    hang_reset!();
                }
                Ok(ReadOutcome::Pending { .. }) => {}
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("decode error during AAC warmup: {e}"),
            }
        }
        (audio, pre)
    }
    let (mut audio, pre_samples) = spawn_blocking(move || warm_cross_codec_audio(audio))
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
    //
    // The sample target alone does not bound this window, though: a consumer
    // reading a warm cache produces those frames in less wall-clock time than a
    // single fetch takes, so the whole budget can be spent on the OUTGOING
    // variant and the window closes before the switch it is meant to observe
    // has issued one request. That measures nothing "post-switch". The blocking
    // phase therefore paces the outgoing reader after the same 200 ms sample
    // floor as warmup, then resets its counter only after the exact Manual(3)
    // commit event. The harness timeout, not the sample budget, fails a switch
    // that never comes.
    let post_target: u64 = 660_000;
    let stats =
        spawn_blocking(move || read_manual_cross_codec_phase(&mut audio, &mut hls_rx, post_target))
            .await
            .expect("sustained post-switch read join");

    info!(
        applied_transitions = ?stats.applied_transitions,
        pre_samples,
        post_samples = stats.post_samples,
        max_stall_ms = stats.max_stall_ms,
        saw_eof = stats.saw_eof,
        "Phase P.0: cross-codec switch sustained playback"
    );

    assert!(
        stats
            .applied_transitions
            .contains(&(3, AbrReason::ManualOverride)),
        "Manual(3) must surface VariantApplied{{to:3, reason:ManualOverride}} (FLAC) — \
         applied_transitions={:?}",
        stats.applied_transitions
    );
    assert!(
        !stats.saw_eof,
        "FLAC playback after the cross-codec flip ended prematurely (false EOS) \
         before the ≥660_000-frame target; got {}. \
         Prod symptom: decoder stalls / dies after the switch.",
        stats.post_samples
    );
    assert!(
        stats.post_samples >= 660_000,
        "sustained FLAC playback after cross-codec flip must produce \
         ≥660_000 frames (≈50 % nominal 44.1 kHz × 15 s × 2 ch); \
         got {}. Prod symptom: decoder stalls after the switch.",
        stats.post_samples
    );
    assert!(
        stats.max_stall_ms < 5_000,
        "decoder must not stall longer than 5 s after the cross-codec flip; \
         longest stall window was {} ms",
        stats.max_stall_ms
    );
}
