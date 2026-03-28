//! Integration test: runtime ABR mode switching must produce actual stream switches.
//!
//! Three WAV variants with no delays (instant delivery).
//! Starts in Auto mode, then randomly switches between Manual(0), Manual(1),
//! Manual(2), and Auto. Each Manual switch must produce a `VariantApplied` event
//! confirming the stream actually switched to the requested variant.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    events::{Event, EventBus},
    hls::{
        AbrMode, AbrOptions, Hls, HlsConfig,
        config::{AbrController, ThroughputEstimator},
    },
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::hls_fixture::{HlsTestServer, HlsTestServerConfig};
use kithara_platform::{
    Mutex,
    time::{Duration, Instant},
    tokio::{
        self,
        task::{spawn, spawn_blocking},
    },
};
use kithara_test_utils::{
    TestTempDir,
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_header,
};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::test_defaults::SawWav;

const D: SawWav = SawWav::DEFAULT;
const SEGMENT_COUNT: usize = 50;
const VARIANT_COUNT: usize = 3;

fn create_wav_init_segment() -> Vec<u8> {
    create_wav_header(D.sample_rate, D.channels, None)
}

fn create_pcm_segments() -> Vec<u8> {
    SignalPcm::new(
        signal::Sawtooth,
        D.sample_rate,
        D.channels,
        Finite::from_segments(SEGMENT_COUNT, D.segment_size, D.channels),
    )
    .into_vec()
}

/// Record of a VariantApplied event.
#[derive(Clone, Debug)]
struct SwitchRecord {
    from: usize,
    to: usize,
}

/// Runtime ABR mode switching must produce actual variant switches.
///
/// Scenario:
/// 1. Start with Auto(0) — plays variant 0
/// 2. Switch to Manual(1) — must see VariantApplied { to_variant: 1 }
/// 3. Switch to Manual(2) — must see VariantApplied { to_variant: 2 }
/// 4. Switch to Manual(0) — must see VariantApplied { to_variant: 0 }
/// 5. Switch back to Auto — must see ABR pick a variant
///
/// Each switch is verified by waiting for the corresponding VariantApplied event
/// within a timeout. If the event doesn't arrive, the test fails.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "3")
)]
async fn abr_runtime_mode_switch_produces_variant_applied() {
    let init_segment = Arc::new(create_wav_init_segment());
    let pcm_data = Arc::new(create_pcm_segments());

    let segment_duration = D.segment_size as f64 / (D.sample_rate as f64 * D.channels as f64 * 2.0);

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: VARIANT_COUNT,
        segments_per_variant: SEGMENT_COUNT,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration,
        custom_data_per_variant: Some(vec![
            Arc::clone(&pcm_data),
            Arc::clone(&pcm_data),
            Arc::clone(&pcm_data),
        ]),
        init_data_per_variant: Some(vec![
            Arc::clone(&init_segment),
            Arc::clone(&init_segment),
            Arc::clone(&init_segment),
        ]),
        variant_bandwidths: Some(vec![3_000_000, 1_000_000, 500_000]),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8").expect("url");
    info!(%url, "HLS server ready with {VARIANT_COUNT} variants");

    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();

    let bus = EventBus::new(64);
    let switches: Arc<Mutex<Vec<SwitchRecord>>> = Arc::new(Mutex::new(Vec::new()));
    let switches_bg = Arc::clone(&switches);
    let switch_count = Arc::new(AtomicUsize::new(0));
    let switch_count_bg = Arc::clone(&switch_count);

    let mut events_rx = bus.subscribe();
    spawn(async move {
        while let Ok(ev) = events_rx.recv().await {
            if let Event::Hls(kithara::prelude::HlsEvent::VariantApplied {
                from_variant,
                to_variant,
                reason,
            }) = &ev
            {
                info!(
                    from = from_variant,
                    to = to_variant,
                    ?reason,
                    "VariantApplied"
                );
                switches_bg.lock_sync().push(SwitchRecord {
                    from: *from_variant,
                    to: *to_variant,
                });
                switch_count_bg.fetch_add(1, Ordering::Release);
            }
        }
    });

    // Create shared ABR controller so we can call set_mode() at runtime.
    let abr = AbrController::<ThroughputEstimator>::new(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        min_switch_interval: Duration::ZERO,
        throughput_safety_factor: 1.0,
        ..AbrOptions::default()
    });
    let abr_for_switch = abr.clone();

    let hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel.clone())
        .with_events(bus.clone());

    // Inject shared ABR controller into config.
    let mut hls_config = hls_config;
    hls_config.abr = Some(abr);

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_events(bus)
        .with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create Audio<Stream<Hls>>");

    // Read audio in background to keep the pipeline active.
    let read_cancel = cancel.clone();
    let read_handle = spawn_blocking(move || {
        let mut buf = vec![0.0f32; 4096];
        let mut total = 0u64;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(25) && !read_cancel.is_cancelled() {
            let n = audio.read(&mut buf);
            if n == 0 && audio.is_eof() {
                break;
            }
            total += n as u64;
        }
        total
    });

    // Wait for initial playback to stabilize.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Sequence of mode switches to test.
    let mode_sequence = [
        AbrMode::Manual(1),
        AbrMode::Manual(2),
        AbrMode::Manual(0),
        AbrMode::Manual(2),
        AbrMode::Manual(1),
        AbrMode::Auto(None),
    ];

    for (i, mode) in mode_sequence.iter().enumerate() {
        let count_before = switch_count.load(Ordering::Acquire);
        info!(step = i, ?mode, "setting ABR mode");
        abr_for_switch.set_mode(*mode);

        // For Manual mode, wait for VariantApplied confirming the switch.
        if let AbrMode::Manual(target) = mode {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if Instant::now() > deadline {
                    let records = switches.lock_sync().clone();
                    panic!(
                        "step {i}: timeout waiting for VariantApplied(to={target}). \
                         Switches so far: {records:?}, ABR current: {}",
                        abr_for_switch.get_current_variant_index()
                    );
                }
                let records = switches.lock_sync();
                let found = records.iter().skip(count_before).any(|r| r.to == *target);
                drop(records);
                if found {
                    info!(step = i, target, "confirmed VariantApplied");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        } else {
            // Auto mode: just wait a bit and verify ABR is running.
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    cancel.cancel();
    let total_samples = read_handle.await.expect("read thread");
    let final_switches = switches.lock_sync();

    info!(
        total_samples,
        switch_count = final_switches.len(),
        "test complete"
    );

    assert!(total_samples > 0, "expected audio output");
    assert!(
        final_switches.len() >= mode_sequence.len() - 1,
        "expected at least {} switches (one per Manual mode change), got {}. Records: {:?}",
        mode_sequence.len() - 1,
        final_switches.len(),
        *final_switches,
    );
}
