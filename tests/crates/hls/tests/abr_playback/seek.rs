use kithara::{
    abr::AbrEvent,
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, ChunkOutcome, ReadOutcome},
    decode::DecoderBackend,
    download::{Downloader, DownloaderConfig},
    events::{EventBus, EventReceiver},
    file::{File, FileConfig},
    hls::{AbrMode, Hls, HlsConfig},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken, time,
        time::{Duration, Instant},
        tokio::{sync::broadcast::error::TryRecvError, task::spawn_blocking},
    },
    play::{PlayWorker, PlayWorkerConfig},
    stream::Stream,
};
use kithara_integration_tests::{
    TestServerHelper, auto,
    bufpool_ext::{TestPools, pools},
    event::TestEvent,
    fixture_protocol::DelayRule,
    mixed_codec_ladder,
    mock::LaneAudio,
};
use kithara_test_utils::{TestTempDir, pace};
use url::Url;

/// Stream must continue producing chunks after seek sequence.
///
/// Regression for app3.log: DRM track plays to 23.97s with seeks,
/// then stops producing chunks → `recv_outcome_blocking` hang.
///
/// Parameterized: path × ABR mode to isolate DRM vs HLS vs no-ABR.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub(crate) async fn stream_continues_after_seek(
    temp_dir: TestTempDir,
    encrypted: bool,
    abr_auto: bool,
    backend: DecoderBackend,
    prepared: (TestServerHelper, Url),
) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let (_server, url) = prepared;
    let label = if encrypted { "DRM" } else { "HLS" };

    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );
    let abr_mode = if abr_auto {
        auto(0)
    } else {
        AbrMode::manual(0)
    };
    let hls_config = HlsConfig::for_url(url)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools.clone())
        .cancel(cancel)
        .initial_abr_mode(abr_mode)
        .build();

    let config = kithara::play::TrackConfig::for_audio(
        AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(backend)
                    .build(),
            )
            .build(),
    )
    .block_on_underrun(true)
    .build();
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
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
            hang_tick!();
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    warmup_samples += count.get() as u64;
                    hang_reset!();
                }
                Ok(ReadOutcome::Pending { .. }) => pace(Duration::from_millis(5)),
                Ok(ReadOutcome::Eof { .. }) => panic!("[{label}] unexpected EOF during warmup"),
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
                hang_tick!();
                match audio.read(&mut buf) {
                    Ok(ReadOutcome::Pending { .. }) => {
                        pace(Duration::from_millis(10));
                    }
                    Ok(ReadOutcome::Frames { count, .. }) => {
                        samples += count.get() as u64;
                        hang_reset!();
                    }
                    Ok(ReadOutcome::Eof { .. }) => break,
                    Err(e) => panic!("decode error: {e}"),
                }
            }
            assert!(
                samples > 0,
                "[{label}] seek to {target_secs}s must produce samples, got 0"
            );
        }

        let mut post_seek_samples = 0u64;
        while post_seek_samples < samples_per_seek {
            hang_tick!();
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => {
                    pace(Duration::from_millis(10));
                }
                Ok(ReadOutcome::Frames { count, .. }) => {
                    post_seek_samples += count.get() as u64;
                    hang_reset!();
                }
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("decode error: {e}"),
            }
        }
        assert!(
            post_seek_samples > 0,
            "[{label}] playback after seeks must continue, got 0 samples"
        );
    })
    .await
    .expect("read phase join");
}

/// Same test but without ABR (fixed variant 0) — baseline.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub(crate) async fn fixed_variant_on_production_ladder_plays_without_hang(
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
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools.clone())
        .cancel(cancel)
        .initial_abr_mode(AbrMode::manual(0))
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
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub(crate) async fn seek_after_eof_mmap_produces_samples(
    temp_dir: TestTempDir,
    encrypted: bool,
    prepared: (TestServerHelper, Url),
) {
    let (_server, url) = prepared;
    let label = if encrypted { "DRM" } else { "HLS" };

    let cancel = CancelToken::never();
    let pools = pools();
    let worker = PlayWorker::new(
        PlayWorkerConfig::builder(pools.clone())
            .cancel(cancel.clone())
            .build(),
    );
    let hls_config = HlsConfig::for_url(url)
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
        AudioConfig::<Hls<TestPools>>::for_stream(hls_config)
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(DecoderBackend::default())
                    .build(),
            )
            .build(),
    )
    .block_on_underrun(true)
    .build();
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("create audio");
    spawn_blocking(move || {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];

        let mut warmup_samples = 0u64;
        while warmup_samples < 17_640 {
            hang_tick!();
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    warmup_samples += count.get() as u64;
                    hang_reset!();
                }
                Ok(ReadOutcome::Pending { .. }) => pace(Duration::from_millis(5)),
                Ok(ReadOutcome::Eof { .. }) => panic!("[{label}] unexpected EOF during warmup"),
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
                .unwrap_or_else(|e| panic!("[{label}] seek #{idx} to {target_secs}s failed: {e}"));

            let mut samples = 0u64;
            while samples < samples_per_seek {
                hang_tick!();
                match audio.read(&mut buf) {
                    Ok(ReadOutcome::Pending { .. }) => {
                        pace(Duration::from_millis(10));
                    }
                    Ok(ReadOutcome::Frames { count, .. }) => {
                        samples += count.get() as u64;
                        hang_reset!();
                    }
                    Ok(ReadOutcome::Eof { .. }) => break,
                    Err(e) => panic!("decode error: {e}"),
                }
            }
            assert!(
                samples > 0,
                "[{label}] seek #{idx} to {target_secs}s must produce samples, got 0"
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
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
pub(crate) async fn mp3_stream_continues_after_seek(
    temp_dir: TestTempDir,
    served_mp3: (TestServerHelper, Url),
) {
    let (_server, url) = served_mp3;

    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let file_config = FileConfig::for_src(url.into())
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools.clone())
        .build();
    let config = kithara::play::TrackConfig::for_audio(
        AudioConfig::<File<TestPools>>::for_stream(file_config)
            .hint(("mp3").to_string())
            .build(),
    )
    .block_on_underrun(true)
    .build();
    let mut audio = kithara_integration_tests::mock::load_audio(&worker, config)
        .await
        .expect("create audio");
    spawn_blocking(move || {
        let _ = audio.preload();
        let mut buf = vec![0f32; 4096];

        let mut warmup_samples = 0u64;
        while warmup_samples < 17_640 {
            hang_tick!();
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => {
                    warmup_samples += count.get() as u64;
                    hang_reset!();
                }
                Ok(ReadOutcome::Pending { .. }) => pace(Duration::from_millis(5)),
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
                hang_tick!();
                match audio.read(&mut buf) {
                    Ok(ReadOutcome::Pending { .. }) => {
                        pace(Duration::from_millis(10));
                    }
                    Ok(ReadOutcome::Frames { count, .. }) => {
                        samples += count.get() as u64;
                        hang_reset!();
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
            hang_tick!();
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Pending { .. }) => {
                    pace(Duration::from_millis(10));
                }
                Ok(ReadOutcome::Frames { count, .. }) => {
                    post_seek_samples += count.get() as u64;
                    hang_reset!();
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

/// Collect the ABR events that carry a decision, dropping the measurement
/// stream that reports throughput, estimates and buffer depth.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn drain_abr_decisions(events: &mut EventReceiver<TestEvent>, sink: &mut Vec<AbrEvent>) {
    loop {
        hang_tick!();
        match events.try_recv() {
            Ok(envelope) => {
                hang_reset!();
                if let TestEvent::Abr(event) = envelope.event
                    && !matches!(
                        event,
                        AbrEvent::ThroughputSample { .. }
                            | AbrEvent::BandwidthEstimate { .. }
                            | AbrEvent::BufferAhead { .. }
                    )
                {
                    sink.push(event);
                }
            }
            Err(TryRecvError::Lagged(_)) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
        }
    }
}

#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
fn lowest_pending_estimate(events: &mut EventReceiver<TestEvent>) -> Option<u64> {
    let mut lowest: Option<u64> = None;
    loop {
        hang_tick!();
        match events.try_recv() {
            Ok(envelope) => {
                hang_reset!();
                if let TestEvent::Abr(AbrEvent::BandwidthEstimate { bps }) = envelope.event {
                    lowest = Some(lowest.map_or(bps, |low| low.min(bps)));
                }
            }
            Err(TryRecvError::Lagged(_)) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => return lowest,
        }
    }
}

/// ABR is locked while a seek is pending, so the seek lands on the variant that
/// played before it even after the estimate fell below that variant. The lock
/// ends with the seek and a switch splices at the decode head, so the first
/// tick after the landing may already move the rest of the landing segment.
///
/// The assertion is on the decision ABR makes, not on how many segment
/// boundaries play before it takes effect. Segments requested while the lock
/// was held still arrive, and how many of them there are depends on how long
/// the seek stayed pending — not on ABR.
#[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(3))]
pub(crate) async fn abr_frozen_during_seek_resumes_after(temp_dir: TestTempDir) {
    use kithara::{audio::AudioRead, signal::AudioChunk};

    const INITIAL_VARIANT: usize = TOP_VARIANT;
    const TOP_VARIANT: usize = 3;
    const PREFETCH_SEGMENTS: usize = 5;
    const SEEK_SEGMENT: usize = 25;
    const SLOW_FROM_SEGMENT: usize = SEEK_SEGMENT + 1;
    const SEEK_TARGET: Duration = Duration::from_secs(153);
    const SOFT_TIMEOUT: Duration = Duration::from_secs(10);
    const EVENT_BUS_CAPACITY: usize = 4096;

    let server = TestServerHelper::new().await;
    let pools = pools();
    let cancel = CancelToken::never();
    let net = HttpClient::new(NetOptions::default(), pools.clone(), cancel.child());
    let plain = server
        .create_hls(
            mixed_codec_ladder()
                .variant_bandwidths(vec![1_056_080, 2_145_712, 4_318_880, 15_820_128]),
        )
        .await
        .expect("create the ladder");
    let master = net
        .get_bytes(plain.master_url(), None)
        .await
        .expect("fetch the master playlist");
    let top_bandwidth: u64 = String::from_utf8_lossy(&master)
        .split("BANDWIDTH=")
        .skip(1)
        .filter_map(|rest| {
            rest.split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
        .max()
        .expect("the master playlist advertises bandwidths");
    let slow_segment_bytes = net
        .get_bytes(plain.segment_url(TOP_VARIANT, SLOW_FROM_SEGMENT), None)
        .await
        .expect("fetch a top-variant segment")
        .len();
    let slow_segment_bytes = u64::try_from(slow_segment_bytes).expect("segment size fits u64");
    // Each slow segment then measures half the top bandwidth.
    let slow_segment_delay_ms = slow_segment_bytes * 8 * 2 * 1000 / top_bandwidth;
    let created = server
        .create_hls(
            mixed_codec_ladder()
                .variant_bandwidths(vec![1_056_080, 2_145_712, 4_318_880, 15_820_128])
                .delay_rules(vec![
                    DelayRule {
                        variant: Some(TOP_VARIANT),
                        segment_gte: Some(SLOW_FROM_SEGMENT),
                        delay_ms: slow_segment_delay_ms,
                        ..Default::default()
                    },
                    DelayRule {
                        variant: Some(TOP_VARIANT),
                        delay_ms: slow_segment_delay_ms / 3,
                        ..Default::default()
                    },
                ]),
        )
        .await
        .expect("create the ladder with slow top segments after the seek target");
    // Holds the seek target open, so the seek stays pending while the slow
    // segments after it lower the estimate.
    let seek_segment = server.register_segment_gate(created.token(), TOP_VARIANT, SEEK_SEGMENT);

    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools.clone(),
            cancel.child(),
        ))
        .abr_settings(
            kithara::abr::AbrSettings::builder()
                .initial_throughput_bps(Some(top_bandwidth * 2))
                .bandwidth_emit_min_interval(Duration::ZERO)
                .min_buffer_for_up_switch(Duration::ZERO)
                .urgent_downswitch_buffer(Duration::ZERO)
                .min_switch_interval(Duration::ZERO)
                .throughput_safety_factor(1.0)
                .up_hysteresis_ratio(1.0)
                .down_hysteresis_ratio(1.0)
                .build(),
        )
        .soft_timeout(SOFT_TIMEOUT)
        .cancel(cancel.child())
        .build(),
    );
    let bus = EventBus::new(EVENT_BUS_CAPACITY);
    let mut events = bus.subscribe();
    let hls_config = HlsConfig::for_url(created.master_url())
        .download_batch_size(PREFETCH_SEGMENTS)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools.clone())
        .initial_abr_mode(auto(INITIAL_VARIANT))
        .cancel(cancel)
        .downloader(downloader)
        .events(bus)
        .build();

    let mut audio = kithara_integration_tests::mock::load_audio(
        &worker,
        AudioConfig::<Hls<TestPools>>::for_stream(hls_config).build(),
    )
    .await
    .expect("audio creation");
    let _ = audio.preload();

    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    #[kithara_test_utils::kithara::flash(io)]
    async fn next_chunk(
        audio: &mut LaneAudio<Stream<Hls<TestPools>>, TestPools>,
    ) -> Option<AudioChunk> {
        if !audio.current_segment_ready() {
            kithara_integration_tests::mock::wait_for_preload(audio, "ABR seek landing").await;
        }
        loop {
            hang_tick!();
            let _ = audio.preload();
            match AudioRead::next_chunk(audio) {
                Ok(ChunkOutcome::Chunk(chunk)) => {
                    hang_reset!();
                    return Some(*chunk);
                }
                Ok(ChunkOutcome::Eof { .. }) => {
                    hang_reset!();
                    return None;
                }
                Ok(ChunkOutcome::Pending { .. }) => {}
                Err(e) => panic!("decode error in next_chunk: {e}"),
            }
            kithara::platform::tokio::task::yield_runnable().await;
        }
    }

    while next_chunk(&mut audio)
        .await
        .expect("ABR must reach the top variant before the track ends")
        .meta
        .variant_index
        != Some(TOP_VARIANT)
    {
        hang_tick!();
        hang_reset!();
    }

    let _ = lowest_pending_estimate(&mut events);
    audio.seek(SEEK_TARGET).expect("seek must not fail");
    {
        let _real_io = kithara::platform::flash::real_io();
        let pressure_deadline = Instant::now() + Duration::from_millis(slow_segment_delay_ms * 4);
        let mut lowest_while_pending: Option<u64> = None;
        while seek_segment.requested() == 0
            || lowest_while_pending.is_none_or(|bps| bps >= top_bandwidth)
        {
            hang_tick!();
            assert!(
                Instant::now() <= pressure_deadline,
                "the estimate did not fall below the top bandwidth {top_bandwidth} while the seek \
             was pending: lowest {lowest_while_pending:?}, seek target requested {} times",
                seek_segment.requested()
            );
            let _ = audio.preload();
            assert!(
                matches!(
                    AudioRead::next_chunk(&mut audio)
                        .expect("read while the seek target is withheld"),
                    ChunkOutcome::Pending { .. }
                ),
                "the gated seek target must not produce PCM before release"
            );
            if let Some(bps) = lowest_pending_estimate(&mut events) {
                lowest_while_pending = Some(lowest_while_pending.map_or(bps, |low| low.min(bps)));
                hang_reset!();
            }
            time::sleep(Duration::from_millis(2)).await;
        }
    }
    seek_segment.release();

    #[kithara_test_utils::kithara::flash(io)]
    #[kithara_test_utils::kithara::hang_watchdog(timeout = Duration::from_secs(1))]
    fn read_after_seek(
        audio: &mut LaneAudio<Stream<Hls<TestPools>>, TestPools>,
        mut events: EventReceiver<TestEvent>,
    ) -> Vec<AbrEvent> {
        let landing = match AudioRead::next_chunk(audio).expect("read the seek landing") {
            ChunkOutcome::Chunk(chunk) => Some(*chunk),
            ChunkOutcome::Eof { .. } => None,
            ChunkOutcome::Pending { .. } => panic!("blocking seek landing remained pending"),
        }
        .expect("playback must resume after the seek");
        assert_eq!(
            landing.meta.segment_index,
            Some(u32::try_from(SEEK_SEGMENT).expect("the seek segment fits u32")),
            "the seek must land on its target segment"
        );
        assert_eq!(
            landing.meta.variant_index,
            Some(TOP_VARIANT),
            "the seek must land on the variant that played before it, although the \
         estimate fell below it while the seek was pending"
        );
        let mut decisions = Vec::new();
        loop {
            hang_tick!();
            let chunk = match AudioRead::next_chunk(audio).expect("read after the seek landing") {
                ChunkOutcome::Chunk(chunk) => Some(*chunk),
                ChunkOutcome::Eof { .. } => None,
                ChunkOutcome::Pending { .. } => panic!("blocking post-seek read remained pending"),
            }
            .unwrap_or_else(|| {
                drain_abr_decisions(&mut events, &mut decisions);
                panic!(
                    "ABR must move off the top variant before the track ends; decisions={decisions:?}"
                );
            });
            hang_reset!();
            let variant = chunk.meta.variant_index.expect("HLS chunk has a variant");
            drain_abr_decisions(&mut events, &mut decisions);
            if variant != TOP_VARIANT {
                assert!(variant < TOP_VARIANT, "ABR must switch down");
                break;
            }
        }
        decisions
    }
    let (_audio, decisions) = kithara_integration_tests::reads::blocking_audio(
        kithara_integration_tests::mock::with_blocking_reads(audio),
        move |audio| read_after_seek(audio, events),
    )
    .await;
    let unlocked = decisions
        .iter()
        .rposition(|event| matches!(event, AbrEvent::Unlocked))
        .expect("the seek lock must end once the landing output is committed");
    let decided = decisions[unlocked + 1..]
        .iter()
        .find(|event| {
            matches!(
                event,
                AbrEvent::VariantApplied { .. } | AbrEvent::DecisionSkipped { .. }
            )
        })
        .expect("ABR must decide once the seek lock ended");
    assert!(
        matches!(
            decided,
            AbrEvent::VariantApplied { from, to, .. }
                if from.get() == TOP_VARIANT && to.get() < TOP_VARIANT
        ),
        "the first ABR decision after the seek lock ended must leave the top variant, got {decided:?}"
    );
}
