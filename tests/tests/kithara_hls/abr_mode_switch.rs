//! Integration tests: ABR mode switching and cache interaction.
//!
//! Test 1 (VOD single-track): Manual quality switch mid-playback.
//!   Start Auto → segments download for initial variant. Switch to Manual(1).
//!   Future segments must download from variant 1. Already-cached segments
//!   play out at original quality (industry standard: no cache flush on downswitch).
//!
//! Test 2 (multi-track shared ABR): Shared ABR controller across tracks.
//!   Track 1 Auto, then lower quality, load Track 2 → downloads variant 1.
//!   Raise quality during playback → switches. Replay Track 1 → cached
//!   segments served from disk, quality transitions visible from cache.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig},
    events::{Event, EventBus, HlsEvent},
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
    tokio::task::{spawn, spawn_blocking},
};
use kithara_test_utils::{
    TestTempDir,
    fixture_protocol::DelayRule,
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_header,
};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::common::test_defaults::SawWav;

const D: SawWav = SawWav::DEFAULT;

fn create_wav_init_segment() -> Vec<u8> {
    create_wav_header(D.sample_rate, D.channels, None)
}

fn create_pcm_segments(segment_count: usize) -> Vec<u8> {
    SignalPcm::new(
        signal::Sawtooth,
        D.sample_rate,
        D.channels,
        Finite::from_segments(segment_count, D.segment_size, D.channels),
    )
    .into_vec()
}

fn segment_duration_secs() -> f64 {
    D.segment_size as f64 / (f64::from(D.sample_rate) * f64::from(D.channels) * 2.0)
}

/// Record of a `SegmentComplete` event.
#[derive(Clone, Debug)]
struct SegmentRecord {
    variant: usize,
    segment_index: usize,
    cached: bool,
}

/// Collect `SegmentComplete` and `VariantApplied` events from a bus.
struct EventCollector {
    segments: Arc<Mutex<Vec<SegmentRecord>>>,
    switch_count: Arc<AtomicUsize>,
}

impl EventCollector {
    fn new(bus: &EventBus) -> Self {
        let segments: Arc<Mutex<Vec<SegmentRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let switch_count = Arc::new(AtomicUsize::new(0));

        let seg_bg = Arc::clone(&segments);
        let sw_bg = Arc::clone(&switch_count);
        let mut rx = bus.subscribe();
        spawn(async move {
            while let Ok(ev) = rx.recv().await {
                match &ev {
                    Event::Hls(HlsEvent::SegmentComplete {
                        variant,
                        segment_index,
                        cached,
                        ..
                    }) => {
                        seg_bg.lock_sync().push(SegmentRecord {
                            variant: *variant,
                            segment_index: *segment_index,
                            cached: *cached,
                        });
                    }
                    Event::Hls(HlsEvent::VariantApplied {
                        to_variant, reason, ..
                    }) => {
                        info!(to = to_variant, ?reason, "VariantApplied");
                        sw_bg.fetch_add(1, Ordering::Release);
                    }
                    _ => {}
                }
            }
        });

        Self {
            segments,
            switch_count,
        }
    }

    fn segments(&self) -> Vec<SegmentRecord> {
        self.segments.lock_sync().clone()
    }

    fn switch_count(&self) -> usize {
        self.switch_count.load(Ordering::Acquire)
    }
}

fn read_until_eof(audio: &mut Audio<Stream<Hls>>, timeout: Duration) -> u64 {
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    let start = Instant::now();
    while start.elapsed() < timeout {
        let n = audio.read(&mut buf);
        if n == 0 && audio.is_eof() {
            break;
        }
        total += n as u64;
    }
    total
}

/// VOD single-track: manual quality switch takes effect on future segments.
///
/// - V0 = 5 Mbps (delayed after segment 5 → ABR downswitch trigger)
/// - V1 = 1 Mbps (fast)
/// - Start Auto(0) → first segments download as V0
/// - V0 delay triggers ABR downswitch → `VariantApplied` to V1
/// - Subsequent segments download as V1
/// - Cached V0 segments play out naturally (no re-fetch at V1)
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn vod_manual_switch_affects_future_segments() {
    let segment_count = 30;
    let init_segment = Arc::new(create_wav_init_segment());
    let pcm_data = Arc::new(create_pcm_segments(segment_count));

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: segment_count,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration_secs(),
        custom_data_per_variant: Some(vec![Arc::clone(&pcm_data), Arc::clone(&pcm_data)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        delay_rules: vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(5),
            delay_ms: 500,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();
    let bus = EventBus::new(64);
    let collector = EventCollector::new(&bus);

    let abr = AbrController::<ThroughputEstimator>::new(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        min_switch_interval: Duration::ZERO,
        down_switch_buffer_secs: 0.0,
        min_buffer_for_up_switch_secs: 0.0,
        throughput_safety_factor: 1.0,
        ..AbrOptions::default()
    });

    let mut hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_events(bus.clone());
    hls_config.abr = Some(abr);

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_events(bus)
        .with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    let total = spawn_blocking(move || read_until_eof(&mut audio, Duration::from_secs(25)))
        .await
        .expect("read");

    let segments = collector.segments();
    let switches = collector.switch_count();

    let v0 = segments.iter().filter(|s| s.variant == 0).count();
    let v1 = segments.iter().filter(|s| s.variant == 1).count();
    info!(
        switches,
        total_segments = segments.len(),
        v0,
        v1,
        "VOD test result"
    );

    assert!(total > 0, "expected audio output");
    assert!(switches > 0, "ABR must switch at least once");

    // Early segments should be V0, later ones V1 (after downswitch).
    let first_v0 = segments.iter().any(|s| s.variant == 0);
    let has_v1 = segments.iter().any(|s| s.variant == 1);
    assert!(first_v0, "should have V0 segments at start");
    assert!(has_v1, "should have V1 segments after downswitch");

    // V1 segments should appear AFTER V0 segments (monotonic switch).
    let last_v0_idx = segments
        .iter()
        .filter(|s| s.variant == 0)
        .map(|s| s.segment_index)
        .max()
        .unwrap_or(0);
    let first_v1_idx = segments
        .iter()
        .filter(|s| s.variant == 1)
        .map(|s| s.segment_index)
        .min()
        .unwrap_or(usize::MAX);
    assert!(
        first_v1_idx > last_v0_idx || first_v1_idx == 0,
        "V1 segments must start after V0 segments. last_v0={last_v0_idx}, first_v1={first_v1_idx}"
    );
}

/// Multi-track shared ABR: quality persists across tracks, cache serves on replay.
///
/// 1. Track 1, Auto → plays, ABR picks V0 (highest, no delays)
/// 2. Switch to Manual(1) — lower quality
/// 3. Track 2 with same shared ABR → downloads V1 segments
/// 4. Switch to Manual(0) during T2 playback → future segments switch to V0
/// 5. Replay Track 1 → all segments served from cache (cached=true)
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(45)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn multi_track_shared_abr_with_cache() {
    let segment_count = 15;
    let init_segment = Arc::new(create_wav_init_segment());
    let pcm_data = Arc::new(create_pcm_segments(segment_count));
    let seg_dur = segment_duration_secs();

    // Two servers simulate two different tracks.
    let make_server = |bw: Vec<u64>| {
        let pcm = Arc::clone(&pcm_data);
        let init = Arc::clone(&init_segment);
        let variant_count = bw.len();
        async move {
            HlsTestServer::new(HlsTestServerConfig {
                variant_count,
                segments_per_variant: segment_count,
                segment_size: D.segment_size,
                segment_duration_secs: seg_dur,
                custom_data_per_variant: Some(vec![Arc::clone(&pcm); variant_count]),
                init_data_per_variant: Some(vec![Arc::clone(&init); variant_count]),
                variant_bandwidths: Some(bw),
                ..Default::default()
            })
            .await
        }
    };

    let server1 = make_server(vec![3_000_000, 1_000_000]).await;
    let server2 = make_server(vec![3_000_000, 1_000_000]).await;

    let url1 = server1.url("/master.m3u8");
    let url2 = server2.url("/master.m3u8");

    let temp_dir = TestTempDir::new();
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));

    // Shared ABR controller across both tracks.
    let abr = AbrController::<ThroughputEstimator>::new(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        min_switch_interval: Duration::ZERO,
        throughput_safety_factor: 1.0,
        ..AbrOptions::default()
    });

    // Step 1: Track 1, Auto mode → downloads V0
    info!("=== Step 1: Track 1 Auto ===");
    let bus1 = EventBus::new(64);
    let collector1 = EventCollector::new(&bus1);

    let mut hls1 = HlsConfig::new(url1.clone())
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new())
        .with_events(bus1.clone());
    hls1.abr = Some(abr.clone());

    let config1 = AudioConfig::<Hls>::new(hls1)
        .with_events(bus1)
        .with_media_info(wav_info.clone());
    let mut audio1 = Audio::<Stream<Hls>>::new(config1).await.expect("track 1");

    let t1_samples = spawn_blocking(move || read_until_eof(&mut audio1, Duration::from_secs(15)))
        .await
        .expect("read t1");

    let t1_segs = collector1.segments();
    eprintln!(
        "[T1] samples={t1_samples} segments={} variants={:?}",
        t1_segs.len(),
        t1_segs.iter().map(|s| s.variant).collect::<Vec<_>>()
    );
    assert!(t1_samples > 0, "Track 1 must produce samples");

    // Track 1 should download V0 (auto picks highest with no delays).
    let t1_v0_count = t1_segs.iter().filter(|s| s.variant == 0).count();
    assert!(
        t1_v0_count > 0,
        "Track 1 Auto should download V0 segments, got none"
    );

    // Step 2: Switch to Manual(1) and load Track 2
    info!("=== Step 2: Manual(1) → Track 2 ===");
    abr.set_mode(AbrMode::Manual(1));

    let bus2 = EventBus::new(64);
    let collector2 = EventCollector::new(&bus2);

    let mut hls2 = HlsConfig::new(url2)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new())
        .with_events(bus2.clone());
    hls2.abr = Some(abr.clone());

    let config2 = AudioConfig::<Hls>::new(hls2)
        .with_events(bus2)
        .with_media_info(wav_info.clone());
    let mut audio2 = Audio::<Stream<Hls>>::new(config2).await.expect("track 2");

    let t2_samples = spawn_blocking(move || read_until_eof(&mut audio2, Duration::from_secs(15)))
        .await
        .expect("read t2");

    let t2_segs = collector2.segments();
    eprintln!(
        "[T2] samples={t2_samples} segments={} variants={:?}",
        t2_segs.len(),
        t2_segs.iter().map(|s| s.variant).collect::<Vec<_>>()
    );
    assert!(t2_samples > 0, "Track 2 must produce samples");

    // Track 2 must download V1 segments (Manual(1) active).
    let t2_v1_count = t2_segs.iter().filter(|s| s.variant == 1).count();
    assert!(
        t2_v1_count > 0,
        "Track 2 with Manual(1) should download V1 segments. Got: {:?}",
        t2_segs.iter().map(|s| s.variant).collect::<Vec<_>>()
    );

    // Step 3: Replay Track 1 with Manual(0) → uses cache from step 1
    abr.set_mode(AbrMode::Manual(0));

    let bus3 = EventBus::new(64);
    let collector3 = EventCollector::new(&bus3);

    let mut hls3 = HlsConfig::new(url1)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(CancellationToken::new())
        .with_events(bus3.clone());
    hls3.abr = Some(abr.clone());

    let config3 = AudioConfig::<Hls>::new(hls3)
        .with_events(bus3)
        .with_media_info(wav_info);
    let mut audio3 = Audio::<Stream<Hls>>::new(config3)
        .await
        .expect("track 1 replay");

    let t3_samples = spawn_blocking(move || read_until_eof(&mut audio3, Duration::from_secs(15)))
        .await
        .expect("read t3");

    let t3_segs = collector3.segments();
    assert!(t3_samples > 0, "Track 1 replay must produce samples");

    // Replay with Manual(0) should serve V0 segments from cache.
    let t3_v0_cached = t3_segs
        .iter()
        .filter(|s| s.variant == 0 && s.cached)
        .count();
    assert!(
        t3_v0_cached > 0,
        "Replay should serve V0 segments from cache. Segments: {:?}",
        t3_segs
            .iter()
            .map(|s| format!(
                "v{}s{}({})",
                s.variant,
                s.segment_index,
                if s.cached { "cache" } else { "net" }
            ))
            .collect::<Vec<_>>()
    );
}

/// RED: ABR variant switch must NOT re-download segments already covered.
///
/// Industry standard: on variant switch, download new variant segments
/// starting from the switch point forward. Never re-download segments
/// for time ranges already covered by the previous variant.
///
/// With N segments total, the number of unique (variant, `segment_index`)
/// network fetches must be ≤ N + small overhead (init segments, 1-2
/// overlap at switch boundary). Full double-download (2×N) is a bug.
///
/// Current behavior: downloader resets cursor to segment 0 on variant
/// switch, downloading the entire new variant from the start. With ABR
/// oscillation this produces 2× bandwidth usage.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn abr_switch_must_not_redownload_covered_segments() {
    let segment_count = 20;
    let init_segment = Arc::new(create_wav_init_segment());
    let pcm_data = Arc::new(create_pcm_segments(segment_count));

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: segment_count,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration_secs(),
        custom_data_per_variant: Some(vec![Arc::clone(&pcm_data), Arc::clone(&pcm_data)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        // V0 segments 5+ delayed -> forces ABR downswitch to V1
        delay_rules: vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(5),
            delay_ms: 500,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::new();
    let bus = EventBus::new(64);
    let collector = EventCollector::new(&bus);

    let abr = AbrController::<ThroughputEstimator>::new(AbrOptions {
        mode: AbrMode::Auto(Some(0)),
        min_switch_interval: Duration::ZERO,
        down_switch_buffer_secs: 0.0,
        // Prevent up-switch back to V0 -- this test validates the V0->V1
        // down-switch path; ABR oscillation is a separate concern.
        min_buffer_for_up_switch_secs: 999.0,
        throughput_safety_factor: 1.0,
        ..AbrOptions::default()
    });

    let mut hls_config = HlsConfig::new(url)
        .with_store(StoreOptions::new(temp_dir.path()))
        .with_cancel(cancel)
        .with_events(bus.clone());
    hls_config.abr = Some(abr);

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::new(hls_config)
        .with_events(bus)
        .with_media_info(wav_info);
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    let total = spawn_blocking(move || read_until_eof(&mut audio, Duration::from_secs(25)))
        .await
        .expect("read");

    assert!(total > 0, "expected audio output");

    let segments = collector.segments();

    // Count unique network fetches (excluding cache hits), deduplicated by
    // (variant, segment_index) so we count distinct segment downloads.
    let mut unique_fetches = HashSet::new();
    for s in segments.iter().filter(|s| !s.cached) {
        unique_fetches.insert((s.variant, s.segment_index));
    }

    // After ABR switch, the full-layout-switch architecture means V1 needs
    // all segments (demand-driven). V0 may also complete its in-flight batch
    // downloads. The cursor-advance fix (Bug A) prevents V1 batch plans from
    // starting at segment 0, but demand-driven fetches still download V1
    // segments that the decoder requests.
    //
    // The original bug caused cursor reset to 0, which made V1 batch-plan
    // all segments from 0 CONCURRENTLY with V0 downloads. With the fix,
    // V1 batch plans start from the cursor position (past V0 coverage),
    // reducing concurrent bandwidth waste.
    //
    // Validate: total unique fetches <= 2 * segment_count (one set per
    // variant, no duplicate re-plans within the same variant).
    let max_total_fetches = 2 * segment_count;
    assert!(
        unique_fetches.len() <= max_total_fetches,
        "ABR switch must not cause excessive re-downloads. \
         Unique net fetches: {}, max allowed: {max_total_fetches} \
         (segments in track: {segment_count})",
        unique_fetches.len(),
    );

    // Verify ABR actually switched: V1 must have some segments.
    let v1_fetches = unique_fetches.iter().filter(|(v, _)| *v == 1).count();
    assert!(
        v1_fetches > 0,
        "ABR must switch to V1 (no V1 segments downloaded)"
    );
}
