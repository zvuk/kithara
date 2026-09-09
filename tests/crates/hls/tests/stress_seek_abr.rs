use std::sync::atomic::{AtomicUsize, Ordering};

use kithara::{
    assets::{AssetStore, StorageBackend},
    audio::{AudioConfig, AudioControl, AudioRead, AudioSession, ReadOutcome},
    hls::{Hls, HlsConfig},
    platform::{
        sync::Arc,
        time::Duration,
        tokio::task::{spawn, spawn_blocking},
    },
    play::{PlayWorker, PlayWorkerConfig, RegisteredAudio},
    stream::Stream,
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir, abr_fast, auto,
    bufpool_ext::{TestPools, pools},
    mixed_encrypted, mixed_plain, temp_dir,
};
use tracing::info;
use url::Url;

fn warmup_until_first_frame(
    audio: &mut RegisteredAudio<Stream<Hls<TestPools>>, TestPools>,
    buf: &mut [f32],
) -> u64 {
    let mut warmup_samples = 0u64;
    while warmup_samples == 0 {
        match audio.read(buf) {
            Ok(ReadOutcome::Pending { .. }) => break,
            Ok(ReadOutcome::Frames { count, .. }) => warmup_samples += count.get() as u64,
            Ok(ReadOutcome::Eof { .. }) => break,
            Err(e) => panic!("warmup decode error: {e}"),
        }
    }
    warmup_samples
}

#[derive(Default)]
struct SeekStats {
    seek_count: u64,
    samples_after_seek: u64,
    seek_errors: u64,
    dead_seeks: u64,
}

fn run_rapid_random_seeks(
    audio: &mut RegisteredAudio<Stream<Hls<TestPools>>, TestPools>,
    buf: &mut [f32],
) -> SeekStats {
    let mut stats = SeekStats::default();
    let positions_secs: Vec<f64> = vec![
        147.0, 30.0, 200.0, 5.0, 180.0, 60.0, 210.0, 15.0, 100.0, 0.0, 170.0, 45.0, 195.0, 80.0,
        220.0, 10.0, 130.0, 25.0, 160.0, 90.0, 50.0, 110.0, 175.0, 35.0, 140.0, 70.0, 205.0, 55.0,
        120.0, 185.0, 20.0, 150.0,
    ];

    for i in 0..200 {
        let pos = positions_secs[i % positions_secs.len()];
        let position = Duration::from_secs_f64(pos);
        match audio.seek(position) {
            Ok(_) => {
                stats.seek_count += 1;
                match audio.read(buf) {
                    Ok(ReadOutcome::Frames { count, .. }) => {
                        stats.samples_after_seek += count.get() as u64;
                    }
                    Ok(_) => {
                        stats.dead_seeks += 1;
                    }
                    Err(e) => {
                        stats.seek_errors += 1;
                        info!(?e, pos, "read error after seek");
                    }
                }
            }
            Err(e) => {
                stats.seek_errors += 1;
                info!(?e, pos, "seek error");
            }
        }
    }

    stats
}

/// Stress test: 20 seconds of rapid seeking after ABR switch.
///
/// Reproduces production bug: after ABR switch (V0 AAC → V3 FLAC),
/// seek causes deadlock because `detect_format_change` picks wrong
/// segment offset → decoder created at wrong position → "missing ftyp atom".
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(120)),
    hang_timeout_secs(3)
)]
#[case::hls("HLS", mixed_plain().await)]
#[case::drm("DRM", mixed_encrypted().await)]
async fn stress_seek_during_abr_switch_real_decoder(
    temp_dir: TestTempDir,
    #[case] label: &str,
    _abr_fast: kithara::abr::AbrSettings,
    #[case] prepared: (TestServerHelper, Url),
) {
    let (_server, url) = prepared;
    info!(label, %url, "Opening generated stream");

    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let hls_config = HlsConfig::for_url(url)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools)
        .initial_abr_mode(auto(0))
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config).build();

    let mut audio = worker.open(config).await.expect("audio creation");

    let mut events_rx = audio.event_bus().subscribe();

    let switches = Arc::new(AtomicUsize::new(0));
    let switches_bg = switches.clone();
    spawn(async move {
        while let Ok(ev) = events_rx.recv().await.map(|env| env.event) {
            let ev_str = format!("{:?}", ev);
            if ev_str.contains("VariantApplied") {
                switches_bg.fetch_add(1, Ordering::Relaxed);
                info!("ABR switch detected: {}", ev_str);
            }
        }
    });

    let result = spawn_blocking(move || {
        let mut buf = vec![0f32; 4096];
        let start = Instant::now();

        info!("Phase 1: warmup — reading PCM samples");
        let warmup_samples = warmup_until_first_frame(&mut audio, &mut buf);
        info!(
            warmup_samples,
            elapsed_ms = start.elapsed().as_millis(),
            "Warmup done"
        );

        info!("Phase 2: 200 rapid random seeks");
        let SeekStats {
            seek_count,
            samples_after_seek,
            seek_errors,
            dead_seeks,
        } = run_rapid_random_seeks(&mut audio, &mut buf);

        info!(
            seek_count,
            samples_after_seek, seek_errors, dead_seeks, "Stress test complete"
        );

        assert!(
            samples_after_seek > 0,
            "Audio died after ABR switch: {} seeks, {} errors, {} dead (0 samples), \
             0 total samples produced. Bug: seek after ABR switch kills audio.",
            seek_count,
            seek_errors,
            dead_seeks,
        );

        assert_eq!(
            dead_seeks, 0,
            "Dead seeks: {dead_seeks}/{seek_count}. \
             All seeks must produce audio with seek_pending retry.",
        );
    })
    .await;

    match result {
        Ok(()) => info!(label, "Stress test passed"),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}

/// Repro test for production issue: repeated seeks on the exact stream.
///
/// Uses seek positions observed in logs and asserts that each seek
/// still yields PCM samples (audio must stay alive).
#[kithara::test(
    tokio,
    native,
    serial,
    timeout(Duration::from_secs(120)),
    hang_timeout_secs(5)
)]
#[case::hls("HLS", mixed_plain().await)]
#[case::drm("DRM", mixed_encrypted().await)]
async fn seek_sequence_from_log_real_stream(
    temp_dir: TestTempDir,
    #[case] label: &str,
    _abr_fast: kithara::abr::AbrSettings,
    #[case] prepared: (TestServerHelper, Url),
) {
    let (_server, url) = prepared;
    let pools = pools();
    let worker = PlayWorker::new(PlayWorkerConfig::builder(pools.clone()).build());
    let hls_config = HlsConfig::for_url(url)
        .store(
            AssetStore::builder(pools.clone())
                .backend(StorageBackend::Disk {
                    root: temp_dir.path().to_path_buf(),
                })
                .build(),
        )
        .pools(pools)
        .initial_abr_mode(auto(0))
        .build();
    let config = AudioConfig::<Hls<TestPools>>::for_stream(hls_config).build();
    let mut audio = worker.open(config).await.expect("audio creation");

    let result = spawn_blocking(move || {
        let mut buf = vec![0f32; 4096];
        loop {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { .. }) => break,
                Ok(ReadOutcome::Eof { .. }) => break,
                Ok(ReadOutcome::Pending { .. }) => continue,
                Err(e) => panic!("warmup read error: {e}"),
            }
        }

        let seeks = [7.135_147_392, 12.279_818_594, 17.778_684_807];
        for (idx, seconds) in seeks.into_iter().enumerate() {
            let pos = Duration::from_secs_f64(seconds);
            audio.seek(pos).expect("seek must not fail");

            let mut samples_after_seek = 0usize;
            while samples_after_seek < 16_384 {
                match audio.read(&mut buf) {
                    Ok(ReadOutcome::Pending { .. }) => continue,
                    Ok(ReadOutcome::Frames { count, .. }) => {
                        samples_after_seek += count.get();
                    }
                    Ok(ReadOutcome::Eof { .. }) => break,
                    Err(e) => panic!("post-seek read error: {e}"),
                }
            }

            assert!(
                samples_after_seek > 0,
                "seek #{idx} at {seconds:.3}s produced no PCM samples"
            );
        }
    })
    .await;

    match result {
        Ok(()) => info!(label, "seek_sequence_from_log_real_stream passed"),
        Err(e) => panic!("spawn_blocking failed: {e}"),
    }
}
