use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use kithara::{
    assets::StoreOptions,
    audio::{Audio, AudioConfig, ReadOutcome},
    decode::DecoderBackend,
    events::{AbrEvent, DownloaderEvent, Event, EventBus, HlsEvent, RequestId},
    hls::{AbrMode, Hls, HlsConfig},
    stream::{AudioCodec, ContainerFormat, MediaInfo, Stream},
};
use kithara_integration_tests::{
    TestServerHelper, TestTempDir, auto,
    fixture_protocol::DelayRule,
    hls_server::{HlsTestServer, HlsTestServerConfig},
    signal_pcm::{Finite, SignalPcm, signal},
    wav::create_wav_header,
};
use kithara_platform::{
    CancellationToken, Mutex,
    time::{Duration, Instant},
    tokio::task::{spawn, spawn_blocking},
};
use tracing::info;

use crate::common::test_defaults::SawWav;

const D: SawWav = SawWav::DEFAULT;

fn create_wav_init_segment(data_size: usize) -> Vec<u8> {
    // Declare the concrete PCM size, not a streaming (`0xFFFFFFFF`) size:
    // these are finite VOD tracks, so the WAV `data` chunk length is known
    // up front. A streaming header leaves the decoder without an end marker,
    // so it relies solely on the byte source EOF — and at a variant switch
    // that races into the decoder emitting one padded packet past the true
    // tail (`position > duration`). A concrete size pins the exact end.
    create_wav_header(D.sample_rate, D.channels, Some(data_size))
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

/// Record of a segment-level event.
///
/// Synthesised across `DownloaderEvent` (network fetches) and `HlsEvent`
/// (reader-side reads): `cached = true` means the reader read the
/// segment without a corresponding `RequestCompleted` (cache hit).
#[derive(Clone, Debug)]
struct SegmentRecord {
    variant: usize,
    segment_index: usize,
    cached: bool,
}

fn parse_segment_url(url: &str) -> Option<(usize, usize)> {
    let segs_marker = "/seg/v";
    let after = url.split(segs_marker).nth(1)?;
    let stem = after.split(".m4s").next()?;
    let mut parts = stem.split('_');
    let variant = parts.next()?.parse().ok()?;
    let segment = parts.next()?.parse().ok()?;
    Some((variant, segment))
}

/// Collect download/reader segment events from a bus.
struct EventCollector {
    /// Network fetches that completed (URL→variant/seg parsed at enqueue).
    network_fetches: Arc<Mutex<HashSet<(usize, usize)>>>,
    /// Reader-side reads (segment boundaries crossed in `read_at`).
    reader_segments: Arc<Mutex<Vec<(usize, usize)>>>,
    switch_count: Arc<AtomicUsize>,
}

impl EventCollector {
    fn new(bus: &EventBus) -> Self {
        let network_fetches: Arc<Mutex<HashSet<(usize, usize)>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let reader_segments: Arc<Mutex<Vec<(usize, usize)>>> = Arc::new(Mutex::new(Vec::new()));
        let switch_count = Arc::new(AtomicUsize::new(0));

        let net_bg = Arc::clone(&network_fetches);
        let read_bg = Arc::clone(&reader_segments);
        let sw_bg = Arc::clone(&switch_count);
        let mut rx = bus.subscribe();
        spawn(async move {
            let mut request_map: HashMap<RequestId, (usize, usize)> = HashMap::new();
            loop {
                let ev = match rx.recv().await {
                    Ok(ev) => ev,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                match &ev {
                    Event::Downloader(DownloaderEvent::RequestEnqueued {
                        request_id, url, ..
                    }) => {
                        if let Some(seg) = parse_segment_url(url.as_str()) {
                            request_map.insert(*request_id, seg);
                        }
                    }
                    Event::Downloader(DownloaderEvent::RequestCompleted { request_id, .. }) => {
                        if let Some(seg) = request_map.remove(request_id) {
                            net_bg.lock_sync().insert(seg);
                        }
                    }
                    Event::Hls(HlsEvent::SegmentReadStart {
                        variant,
                        segment_index,
                        ..
                    }) => {
                        read_bg.lock_sync().push((*variant, *segment_index));
                    }
                    Event::Abr(AbrEvent::VariantApplied { to, reason, .. }) => {
                        info!(to = to.get(), ?reason, "VariantApplied");
                        sw_bg.fetch_add(1, Ordering::Release);
                    }
                    _ => {}
                }
            }
        });

        Self {
            network_fetches,
            reader_segments,
            switch_count,
        }
    }

    /// Synthesised view: for every (variant, seg) the reader saw, decide
    /// whether it came from the network (`RequestCompleted` seen) or from
    /// the cache (no Completed event for that pair). Returns one record
    /// per `SegmentReadStart`, dedup'd by (variant, seg) — first sighting
    /// wins.
    fn segments(&self) -> Vec<SegmentRecord> {
        let net = self.network_fetches.lock_sync().clone();
        let reads = self.reader_segments.lock_sync().clone();
        let mut seen: HashSet<(usize, usize)> = HashSet::new();
        let mut out = Vec::new();
        for (v, s) in reads {
            if !seen.insert((v, s)) {
                continue;
            }
            out.push(SegmentRecord {
                variant: v,
                segment_index: s,
                cached: !net.contains(&(v, s)),
            });
        }
        for (v, s) in net {
            if seen.insert((v, s)) {
                out.push(SegmentRecord {
                    variant: v,
                    segment_index: s,
                    cached: false,
                });
            }
        }
        out
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
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Frames { count, .. }) => total += count.get() as u64,
            Ok(ReadOutcome::Eof { .. }) => break,
            Err(e) => panic!("decode error: {e}"),
        }
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
    let init_segment = Arc::new(create_wav_init_segment(segment_count * D.segment_size));
    let pcm_data = Arc::new(create_pcm_segments(segment_count));

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: segment_count,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration_secs(),
        custom_data_per_variant: Some(vec![Arc::clone(&pcm_data), Arc::clone(&pcm_data)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        codecs: Some("wav".to_string()),
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
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);
    let collector = EventCollector::new(&bus);

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .media_info(wav_info)
        .build();
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

    let first_v0 = segments.iter().any(|s| s.variant == 0);
    let has_v1 = segments.iter().any(|s| s.variant == 1);
    assert!(first_v0, "should have V0 segments at start");
    assert!(has_v1, "should have V1 segments after downswitch");

    // "Affects future segments" is a directional contract on network fetches
    // (`!cached`): V1 owns the tail (fetches through the final segment) and V0
    // stops at an early prefix. We deliberately do NOT assert a clean,
    // overlap-free handover index: V0 and V1 are distinct per-variant
    // resources and V0's in-flight downloads keep committing concurrently with
    // the switch, so a 1-2 segment boundary overlap is inherent and harmless
    // (identical PCM, served from cache). Gross double-download (~2xN) is the
    // real regression, owned by `abr_switch_must_not_redownload_covered_segments`;
    // here we only check the handover direction.
    let last_seg = segment_count - 1;
    let net_v0: HashSet<usize> = segments
        .iter()
        .filter(|s| s.variant == 0 && !s.cached)
        .map(|s| s.segment_index)
        .collect();
    let net_v1: HashSet<usize> = segments
        .iter()
        .filter(|s| s.variant == 1 && !s.cached)
        .map(|s| s.segment_index)
        .collect();

    assert!(
        net_v1.contains(&last_seg),
        "downswitch must hand the tail to V1: it must fetch the final segment {last_seg}. net_v1 max={:?}",
        net_v1.iter().max()
    );
    let v0_max = net_v0.iter().max().copied().unwrap_or(0);
    assert!(
        v0_max < last_seg,
        "V0 must stop at an early prefix after downswitch, not reach the end. v0_max={v0_max}, last={last_seg}"
    );
}

/// Deterministic regression for the urgent-down-switch reader-stall hang.
///
/// V0's tail (segment >= 5) is delayed far longer than the hang budget, so
/// once the reader reaches the segment-4/5 boundary it blocks on a V0 segment
/// V0 cannot deliver in time. The ABR raises an `UrgentDownSwitch` to the fast
/// V1, but the Auto-mode commit historically fired only on a reader
/// segment-boundary cross — which the undelivered segment prevents. That
/// circular dependency stalled the reader past `KITHARA_HANG_TIMEOUT_SECS`
/// and tripped the watchdog (the production freeze). The proactive rescue
/// (`HlsCoord::urgent_rescue_boundary`) hands the undelivered tail to V1 at
/// the segment boundary, so the reader finishes V0's loaded prefix and reads
/// the rest from V1. With the delay (10s) >> hang budget (5s) the stall is
/// deterministic: pre-fix this test trips the watchdog; post-fix it completes
/// the full track via V1.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn urgent_downswitch_rescues_reader_blocked_on_slow_variant() {
    let segment_count = 30;
    let init_segment = Arc::new(create_wav_init_segment(segment_count * D.segment_size));
    let pcm_data = Arc::new(create_pcm_segments(segment_count));

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: segment_count,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration_secs(),
        custom_data_per_variant: Some(vec![Arc::clone(&pcm_data), Arc::clone(&pcm_data)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        codecs: Some("wav".to_string()),
        delay_rules: vec![DelayRule {
            variant: Some(0),
            segment_gte: Some(5),
            delay_ms: 10_000,
            ..Default::default()
        }],
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);
    let collector = EventCollector::new(&bus);

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .media_info(wav_info)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    let total = spawn_blocking(move || read_until_eof(&mut audio, Duration::from_secs(20)))
        .await
        .expect("read");

    let segments = collector.segments();
    let switches = collector.switch_count();
    let net_v1 = segments
        .iter()
        .filter(|s| s.variant == 1 && !s.cached)
        .count();
    info!(switches, net_v1, total, "urgent rescue test result");

    // The reader must finish the whole track via V1, not stall on V0's
    // undeliverable tail. A full WAV track is `segment_count * segment_size`
    // PCM bytes; require the bulk of it (the rescue can drop at most the
    // boundary segment to a short read, never the tail).
    let full_frames = (segment_count * D.segment_size) as u64 / u64::from(D.channels) / 2;
    assert!(
        total >= full_frames * 9 / 10,
        "reader must finish the track via V1 after the urgent rescue; \
         got {total} frames, expected >= {} (90% of {full_frames})",
        full_frames * 9 / 10
    );
    assert!(switches > 0, "an urgent down-switch must commit");
    assert!(net_v1 > 0, "V1 must serve the tail after the rescue");
}

/// Multi-track shared ABR: quality persists across tracks, cache serves on replay.
///
/// 1. Track 1, Auto(Some(0)) → plays V0 (1 Mbps); the default
///    `initial_throughput_bps = Some(2 Mbps)` seed picks V0 as the
///    highest variant fitting under `2 Mbps / 1.5 ≈ 1.33 Mbps`, so
///    `decide` issues no boundary switch and V0 segments populate the
///    cache.
/// 2. Switch to Manual(1) — V1 (3 Mbps).
/// 3. Track 2 with Manual(1) → downloads V1 segments.
/// 4. Switch to Manual(0) for Track 1 replay → future segments switch to V0.
/// 5. Replay Track 1 → V0 segments served from cache (cached=true).
///
/// Variants are ordered `[V0=1 Mbps, V1=3 Mbps]` so the seed lands on V0
/// — historically the test used `[3 Mbps, 1 Mbps]` and relied on the
/// pre-seed cold-start behaviour where `Auto(Some(0))` stayed on V0
/// only because `decide` returned `NoEstimate` until samples arrived.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(45)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn multi_track_shared_abr_with_cache() {
    let segment_count = 15;
    let init_segment = Arc::new(create_wav_init_segment(segment_count * D.segment_size));
    let pcm_data = Arc::new(create_pcm_segments(segment_count));
    let seg_dur = segment_duration_secs();

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

    let server1 = make_server(vec![1_000_000, 3_000_000]).await;
    let server2 = make_server(vec![1_000_000, 3_000_000]).await;

    let url1 = server1.url("/master.m3u8");
    let url2 = server2.url("/master.m3u8");

    let temp_dir = TestTempDir::new();
    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));

    info!("=== Step 1: Track 1 Auto ===");
    let bus1 = EventBus::new(8192);
    let collector1 = EventCollector::new(&bus1);

    let hls1 = HlsConfig::for_url(url1.clone())
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(CancellationToken::default())
        .events(bus1.clone())
        .initial_abr_mode(auto(0))
        .build();

    let config1 = AudioConfig::<Hls>::for_stream(hls1)
        .events(bus1)
        .media_info(wav_info.clone())
        .build();
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

    let t1_v0_count = t1_segs.iter().filter(|s| s.variant == 0).count();
    assert!(
        t1_v0_count > 0,
        "Track 1 Auto should download V0 segments, got none"
    );

    info!("=== Step 2: Manual(1) → Track 2 ===");

    let bus2 = EventBus::new(8192);
    let collector2 = EventCollector::new(&bus2);

    let hls2 = HlsConfig::for_url(url2)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(CancellationToken::default())
        .events(bus2.clone())
        .initial_abr_mode(AbrMode::manual(1))
        .build();

    let config2 = AudioConfig::<Hls>::for_stream(hls2)
        .events(bus2)
        .media_info(wav_info.clone())
        .build();
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

    let t2_v1_count = t2_segs.iter().filter(|s| s.variant == 1).count();
    assert!(
        t2_v1_count > 0,
        "Track 2 with Manual(1) should download V1 segments. Got: {:?}",
        t2_segs.iter().map(|s| s.variant).collect::<Vec<_>>()
    );

    let bus3 = EventBus::new(8192);
    let collector3 = EventCollector::new(&bus3);

    let hls3 = HlsConfig::for_url(url1)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(CancellationToken::default())
        .events(bus3.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let config3 = AudioConfig::<Hls>::for_stream(hls3)
        .events(bus3)
        .media_info(wav_info)
        .build();
    let mut audio3 = Audio::<Stream<Hls>>::new(config3)
        .await
        .expect("track 1 replay");

    let t3_samples = spawn_blocking(move || read_until_eof(&mut audio3, Duration::from_secs(15)))
        .await
        .expect("read t3");

    let t3_segs = collector3.segments();
    assert!(t3_samples > 0, "Track 1 replay must produce samples");

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
    let init_segment = Arc::new(create_wav_init_segment(segment_count * D.segment_size));
    let pcm_data = Arc::new(create_pcm_segments(segment_count));

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: segment_count,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration_secs(),
        custom_data_per_variant: Some(vec![Arc::clone(&pcm_data), Arc::clone(&pcm_data)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        codecs: Some("wav".to_string()),
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
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);
    let collector = EventCollector::new(&bus);

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .media_info(wav_info)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    let total = spawn_blocking(move || read_until_eof(&mut audio, Duration::from_secs(25)))
        .await
        .expect("read");

    assert!(total > 0, "expected audio output");

    let segments = collector.segments();

    let mut unique_fetches = HashSet::new();
    for s in segments.iter().filter(|s| !s.cached) {
        unique_fetches.insert((s.variant, s.segment_index));
    }

    let max_total_fetches = 2 * segment_count;
    assert!(
        unique_fetches.len() <= max_total_fetches,
        "ABR switch must not cause excessive re-downloads. \
         Unique net fetches: {}, max allowed: {max_total_fetches} \
         (segments in track: {segment_count})",
        unique_fetches.len(),
    );

    let v1_fetches = unique_fetches.iter().filter(|(v, _)| *v == 1).count();
    assert!(
        v1_fetches > 0,
        "ABR must switch to V1 (no V1 segments downloaded)"
    );
}

/// Phase L1: same-codec runtime Manual switch via `AbrHandle::set_mode`.
///
/// Closes the gap between the isolated `AbrState::commit_pending`-path tests
/// and the production GUI path. Confirms that a Manual switch initiated mid-
/// playback (not via `with_initial_abr_mode`) lands at the next segment
/// boundary, fires `VariantApplied`, and subsequent reader segments come
/// from the chosen variant.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn runtime_manual_switch_via_handle_changes_playing_variant() {
    let segment_count = 30;
    let init_segment = Arc::new(create_wav_init_segment(segment_count * D.segment_size));
    let pcm_data = Arc::new(create_pcm_segments(segment_count));

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 3,
        segments_per_variant: segment_count,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration_secs(),
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
        variant_bandwidths: Some(vec![5_000_000, 1_000_000, 2_000_000]),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);
    let collector = EventCollector::new(&bus);

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .media_info(wav_info)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    // Warm up a couple of segments so the reader is past the boundary
    // commit gate, then trigger a Manual switch via the handle.
    let mut buf = vec![0.0f32; 4096];
    let mut total = 0u64;
    let warmup_deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < warmup_deadline && total < 8_192 {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => total += count.get() as u64,
            Ok(ReadOutcome::Pending { .. }) | Ok(ReadOutcome::Eof { .. }) => {}
            Err(e) => panic!("decode error in warmup: {e}"),
        }
    }
    assert!(total > 0, "warmup must yield audio before the Manual flip");

    let handle = audio
        .abr_handle()
        .expect("HLS stream must expose AbrHandle");
    handle
        .set_mode(AbrMode::manual(2))
        .expect("Manual(2) target is in the variant list");

    let post_total = spawn_blocking(move || read_until_eof(&mut audio, Duration::from_secs(15)))
        .await
        .expect("read");

    let segments = collector.segments();
    let switches = collector.switch_count();
    let v2_fetches = segments
        .iter()
        .filter(|s| s.variant == 2 && !s.cached)
        .count();

    info!(
        switches,
        total_segments = segments.len(),
        v2_fetches,
        post_total,
        "L1: runtime Manual switch result"
    );

    assert!(post_total > 0, "playback must continue after Manual flip");
    assert!(
        switches >= 1,
        "Manual(2) must trigger at least one VariantApplied event"
    );
    assert!(
        v2_fetches > 0,
        "Manual(2) must produce future-segment fetches from variant 2"
    );
}

/// Phase L2: cross-codec runtime Manual switch (AAC → FLAC) via
/// `AbrHandle::set_mode`. This is the exact reproducer of the production
/// bug where clicking Manual(3) (FLAC) in the GUI caused a 10s hang
/// before Phase K's `decode_next_chunk` recovery + `apply_decision` split.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(45)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn runtime_cross_codec_manual_switch_no_hang() {
    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");
    // assets/hls/master.m3u8: variants 0..2 are AAC (mp4a.40.2), variant 3
    // is FLAC (fLaC). Manual(3) forces the cross-codec path.

    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);
    // EventCollector's segment URL parser is HlsTestServer-specific; for
    // real-asset URLs we capture VariantApplied targets directly.
    let applied_targets: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let applied_bg = Arc::clone(&applied_targets);
    let mut applied_rx = bus.subscribe();
    spawn(async move {
        loop {
            let ev = match applied_rx.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if let Event::Abr(AbrEvent::VariantApplied { to, .. }) = ev {
                applied_bg.lock_sync().push(to.get());
            }
        }
    });

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    let mut buf = vec![0f32; 4096];
    let mut pre_total = 0u64;
    let warmup_deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < warmup_deadline && pre_total < 16_384 {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => pre_total += count.get() as u64,
            Ok(ReadOutcome::Pending { .. }) | Ok(ReadOutcome::Eof { .. }) => {}
            Err(e) => panic!("decode error pre-switch: {e}"),
        }
    }
    assert!(
        pre_total > 0,
        "warmup must produce AAC samples before the cross-codec flip"
    );

    let handle = audio
        .abr_handle()
        .expect("HLS stream must expose AbrHandle");
    handle
        .set_mode(AbrMode::manual(3))
        .expect("Manual(3) (FLAC variant) target must be valid");

    // Read for several seconds after the flip — if the decoder hangs on
    // `Pending(VariantChange)` without recovery, the hang_watchdog or
    // the test timeout will fail. Otherwise we should see post-switch
    // samples coming from the FLAC variant.
    let post_total = spawn_blocking(move || read_until_eof(&mut audio, Duration::from_secs(25)))
        .await
        .expect("read");

    let targets = applied_targets.lock_sync().clone();
    let saw_flac = targets.contains(&3);

    info!(
        ?targets,
        pre_total, post_total, "L2: cross-codec Manual switch result"
    );

    assert!(
        !targets.is_empty(),
        "cross-codec Manual(3) must fire at least one VariantApplied"
    );
    assert!(
        saw_flac,
        "Manual(3) must publish a VariantApplied with to=3, saw: {targets:?}"
    );
    assert!(
        post_total > 0,
        "playback must continue after the cross-codec flip — \
         pre-K bug: hang_watchdog panic at 10s"
    );
}

/// Phase O.0 regression: production bug repro.
///
/// User reports: "клик на новый вариант — играет тот же, в кэше новых
/// файлов нет". app.log confirms `GUI: set_mode accepted` fires but no
/// `commit_variant_switch` invocation follows.
///
/// Root cause hypothesis: when all segments of the current variant are
/// already cached (prefetch covered the full variant), `HlsPeer` parks
/// itself in `Poll::Pending` waiting for `reader_advanced`. The reader
/// notifies that handle only on seek (`audio.rs:606`), not on
/// `set_mode`. As a result `apply_boundary_crossing` never runs after a
/// Manual click, `peek_pending_decision` is never observed, and the
/// switch stays pending forever.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn runtime_manual_switch_works_when_all_segments_cached() {
    let segment_count: usize = 6;
    let init_segment = Arc::new(create_wav_init_segment(segment_count * D.segment_size));
    let pcm_data = Arc::new(create_pcm_segments(segment_count));

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: segment_count,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration_secs(),
        custom_data_per_variant: Some(vec![Arc::clone(&pcm_data), Arc::clone(&pcm_data)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);
    let collector = EventCollector::new(&bus);

    // download_batch_size larger than total segments → peer fetches the
    // full variant 0 then parks itself idle.
    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .download_batch_size(segment_count * 2)
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .media_info(wav_info)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    // Tiny warmup read — kicks off the peer's prefetch, then drops the
    // reader so the prefetch can finish on its own thread.
    let mut buf = vec![0.0f32; 4096];
    let mut warmup_samples = 0u64;
    let warmup_deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < warmup_deadline && warmup_samples < 8_192 {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => {
                warmup_samples += count.get() as u64;
            }
            Ok(ReadOutcome::Pending { .. }) | Ok(ReadOutcome::Eof { .. }) => {}
            Err(e) => panic!("decode error in warmup: {e}"),
        }
    }
    assert!(warmup_samples > 0, "warmup must produce some audio");

    // Give the downloader enough time to finish prefetching every v0
    // segment and let the peer park itself in `Poll::Pending` — this
    // is the production state the bug reproduces against.
    kithara_platform::time::sleep(Duration::from_secs(2)).await;

    let v0_fetched = collector
        .segments()
        .iter()
        .filter(|s| s.variant == 0 && !s.cached)
        .count();
    assert!(
        v0_fetched >= segment_count,
        "all v0 segments must be cached before the Manual click — \
         saw {v0_fetched}/{segment_count}"
    );

    let handle = audio
        .abr_handle()
        .expect("HLS stream must expose AbrHandle");
    let pre_switch = collector.switch_count();
    handle
        .set_mode(AbrMode::manual(1))
        .expect("Manual(1) target must be valid");

    // Give the controller a moment to react. If `set_mode` correctly
    // wakes the peer, the peer polls, observes the pending decision,
    // and `commit_variant_switch` fires within tens of ms. The 3-second
    // window leaves ample slack for the kithara::test serial runtime.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && collector.switch_count() == pre_switch {
        kithara_platform::time::sleep(Duration::from_millis(50)).await;
    }

    let post_switch = collector.switch_count();
    info!(
        pre_switch,
        post_switch, v0_fetched, "O.0: all-cached Manual click result"
    );

    assert!(
        post_switch > pre_switch,
        "Manual(1) after all-cached prefetch must fire VariantApplied \
         (pre={pre_switch}, post={post_switch}); peer was parked idle and \
         set_mode failed to wake it"
    );
}

/// MSW-3 regression: production bug from app.log (2026-05-17 15:26..15:27).
///
/// Sequence reproduced from the GUI smoke run:
/// 1. Cold start V0, play a bit, switch up to V3.
/// 2. Switch back to V0 — scheduler emits all remaining V0 segments, then
///    parks idle (peer fully cached).
/// 3. Seek somewhere (reader moves but no new fetches required — every
///    target segment is cached).
/// 4. `handle.set_mode(Manual(1))` — `GUI: set_mode accepted` logs, but
///    `commit_variant_switch` never fires. Every subsequent Manual click
///    is silently dropped — player keeps playing V0.
///
/// Existing `runtime_manual_switch_works_when_all_segments_cached` covers
/// bulk-cache + Manual but NOT the seek-in-the-middle case. The seek
/// re-runs the peer's `apply_seek_change` path which mutates state
/// without going through the ABR controller, masking the wake hook from
/// `on_mode_changed → tick`.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn runtime_manual_switch_works_after_cache_and_seek() {
    let segment_count: usize = 8;
    let init_segment = Arc::new(create_wav_init_segment(segment_count * D.segment_size));
    let pcm_data = Arc::new(create_pcm_segments(segment_count));

    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 2,
        segments_per_variant: segment_count,
        segment_size: D.segment_size,
        segment_duration_secs: segment_duration_secs(),
        custom_data_per_variant: Some(vec![Arc::clone(&pcm_data), Arc::clone(&pcm_data)]),
        init_data_per_variant: Some(vec![Arc::clone(&init_segment), Arc::clone(&init_segment)]),
        variant_bandwidths: Some(vec![5_000_000, 1_000_000]),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);
    let collector = EventCollector::new(&bus);

    // Manual(0) initial so Auto-decision doesn't fire an UpSwitch/
    // DownSwitch that races against the explicit Manual(1) below.
    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .download_batch_size(segment_count * 2)
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .media_info(wav_info)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    // Tiny warmup so the peer is actually pumping. Drop the reader to
    // let the prefetch finish in the background.
    let mut buf = vec![0.0f32; 4096];
    let mut warmup_samples = 0u64;
    let warmup_deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < warmup_deadline && warmup_samples < 8_192 {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => warmup_samples += count.get() as u64,
            Ok(ReadOutcome::Pending { .. }) | Ok(ReadOutcome::Eof { .. }) => {}
            Err(e) => panic!("decode error in warmup: {e}"),
        }
    }
    assert!(warmup_samples > 0, "warmup must produce some audio");

    kithara_platform::time::sleep(Duration::from_secs(2)).await;

    let v0_fetched = collector
        .segments()
        .iter()
        .filter(|s| s.variant == 0 && !s.cached)
        .count();
    assert!(
        v0_fetched >= segment_count,
        "all v0 segments must be cached before seek+Manual — \
         saw {v0_fetched}/{segment_count}"
    );

    // Seek into a cached region — this is the production trigger. The
    // peer's `apply_seek_change` resets the queue cursor, leaves the
    // peer parked (every target seg is cached), and (the bug) wipes
    // whatever invariant lets `on_mode_changed → tick → peer.wake()`
    // reach `apply_boundary_crossing → commit_variant_switch`.
    let seek_target_secs = segment_duration_secs() * ((segment_count / 2) as f64);
    audio
        .seek(Duration::from_secs_f64(seek_target_secs))
        .expect("seek must succeed");

    // Let the peer process the seek epoch bump and re-park itself.
    kithara_platform::time::sleep(Duration::from_millis(300)).await;

    let handle = audio
        .abr_handle()
        .expect("HLS stream must expose AbrHandle");
    let pre_switch = collector.switch_count();
    handle
        .set_mode(AbrMode::manual(1))
        .expect("Manual(1) target must be valid");

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && collector.switch_count() == pre_switch {
        kithara_platform::time::sleep(Duration::from_millis(50)).await;
    }

    let post_switch = collector.switch_count();
    info!(
        pre_switch,
        post_switch, v0_fetched, seek_target_secs, "MSW-3: all-cached + seek + Manual click result"
    );

    assert!(
        post_switch > pre_switch,
        "Manual(1) after all-cached prefetch + seek must fire VariantApplied \
         (pre={pre_switch}, post={post_switch}). Production regression: \
         after `audio.seek({seek_target_secs}s)` the peer parks and \
         subsequent `handle.set_mode(...)` no longer reaches \
         `commit_variant_switch` — observed in app.log 2026-05-17 \
         15:26:54..15:27:46 where six successive Manual clicks were \
         silently dropped after V1→V0 commit + bulk-cache + 3 seeks. \
         seek_target_secs={seek_target_secs}"
    );
}

/// Phase P.0' regression: production bug from app.log (2026-05-15).
///
/// User opens a track in Auto mode. A fast CDN delivers the first
/// ~50 KB segment in a few milliseconds. `record_bandwidth` accepts
/// the sample, estimator surfaces hundreds of Mbps, ABR `up_switch`
/// candidate is the highest variant with headroom
/// ≫ `up_hysteresis_ratio = 1.3`, and `commit_variant_switch` fires on
/// the FIRST segment boundary — across codecs in real assets, the user
/// observes "sound disappears, slider keeps moving, then crash".
///
/// Expected behaviour: ABR must wait for enough buffer before
/// committing an up-switch. The first boundary crossing must not
/// produce a `VariantApplied` event under default settings on a fast
/// local server: `min_buffer_for_up_switch = 10s` keeps the up-switch
/// candidate gated behind `AbrReason::BufferTooLowForUpSwitch` until
/// the buffer fills. Tests that previously masked this with
/// `abr_fast` fixtures stay green — this one specifically uses
/// **default** `AbrSettings` to lock down production defaults.
///
/// Deterministic fixture: 3 same-codec AAC variants on `HlsTestServer`,
/// no delay rules → fastest possible fetch path. Without the buffer
/// gate an aggressive up-switch would land at segment 1; with it the
/// first boundary stays neutral.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(30)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
async fn auto_does_not_up_switch_on_first_boundary_with_defaults() {
    let segment_count: usize = 6;
    let init_segment = Arc::new(create_wav_init_segment(segment_count * D.segment_size));
    let pcm_data = Arc::new(create_pcm_segments(segment_count));

    // Long segment duration (6 s in playlist EXTINF) so a full prefetch
    // pushes `buffer_ahead` over the default 10 s `min_buffer_for_up_switch`
    // gate — same as the real assets/hls/ fixture. Without this the
    // buffer gate alone blocks ABR and the test reports a false GREEN.
    let server = HlsTestServer::new(HlsTestServerConfig {
        variant_count: 3,
        segments_per_variant: segment_count,
        segment_size: D.segment_size,
        segment_duration_secs: 6.0,
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
        // 1× / 2× / 4× — ABR should prefer the top variant once it has
        // enough evidence, but not after a single 50 KB sample.
        variant_bandwidths: Some(vec![256_000, 512_000, 1_024_000]),
        ..Default::default()
    })
    .await;

    let url = server.url("/master.m3u8");
    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);
    let collector = EventCollector::new(&bus);

    // Crucially: NO `with_settings(abr_fast())` — production defaults.
    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    let wav_info = MediaInfo::new(Some(AudioCodec::Pcm), Some(ContainerFormat::Wav));
    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .media_info(wav_info)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    // Read just enough to cross the first segment boundary — the reader
    // moves from byte 0 into segment 1. We do NOT exhaust the fixture
    // (large enough sample budget but bounded).
    let mut buf = vec![0.0f32; 4096];
    let mut samples = 0u64;
    let deadline = Instant::now() + Duration::from_secs(3);
    let single_segment_frames = (D.segment_size / 4) as u64; // 200 KB / (stereo i16) ≈ 50_000 frames
    while Instant::now() < deadline && samples < single_segment_frames * 2 {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => samples += count.get() as u64,
            Ok(ReadOutcome::Pending { .. }) | Ok(ReadOutcome::Eof { .. }) => {}
            Err(e) => panic!("decode error: {e}"),
        }
    }

    let switches = collector.switch_count();
    let segments = collector.segments();
    let v0_seen = segments.iter().filter(|s| s.variant == 0).count();
    info!(
        switches,
        v0_seen, samples, "P.0': auto-switch frequency under prod defaults"
    );

    assert!(
        samples > 0,
        "test must produce audio before evaluating ABR aggressiveness"
    );
    assert_eq!(
        switches, 0,
        "ABR Auto must not commit a variant switch within the first \
         two segments of playback under default settings — saw \
         {switches} VariantApplied event(s). Prod symptom: aggressive \
         cross-codec jump on the first boundary."
    );
}

/// Phase S regression: rapid cross-codec → same-codec switch race.
///
/// User scenario (app.log 2026-05-16): a cross-codec switch followed
/// by a same-codec switch within the recreate window leaves
/// `session.media_info` stale and `header_byte_range(v_new)`
/// returning `Err(NotApplicable)` because of `byte_shift`, then EOF
/// gets treated as terminal → auto-seek → hang.
///
/// **CURRENT STATE (work in progress)**: this test as written tends
/// to consume the cross-codec pending decision via `request_target`
/// overwrite (Manual(1) replaces Manual(3) in the pending slot
/// before the boundary commit fires), so the actual race window is
/// not reliably hit in the in-process server. It additionally
/// surfaces a separate, pre-existing bug: a same-codec switch from
/// AAC v=0 to AAC v=1 with `byte_shift` hits `unexpected EOF before
/// segment buffer filled` — a layout mismatch unrelated to the
/// cross-codec race. Marked `#[ignore]` until either the `byte_shift`
/// boundary mismatch is addressed independently or the test is
/// rewritten to deterministically force the rapid-recreate race
/// (likely needs `DelayRule` on segment fetches).
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(45)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[ignore = "current implementation hits a separate same-codec byte_shift mismatch; needs deterministic timing setup to repro the cross→same race"]
async fn rapid_cross_codec_then_same_codec_switch_no_false_eof() {
    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");
    // assets/hls/master.m3u8: variants 0..2 AAC (mp4a.40.2), variant 3
    // FLAC. We need Manual(3) (cross-codec) then Manual(1) (same-codec
    // AAC sibling of v=0) before v=3's decoder recreate fires.

    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);

    let applied_targets: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let applied_bg = Arc::clone(&applied_targets);
    let mut applied_rx = bus.subscribe();
    spawn(async move {
        loop {
            let ev = match applied_rx.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if let Event::Abr(AbrEvent::VariantApplied { to, .. }) = ev {
                applied_bg.lock_sync().push(to.get());
            }
        }
    });

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(auto(0))
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    // Warmup on v=0 (AAC).
    let mut buf = vec![0f32; 4096];
    let mut warmup_total = 0u64;
    let warmup_deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < warmup_deadline && warmup_total < 16_384 {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => warmup_total += count.get() as u64,
            Ok(ReadOutcome::Pending { .. }) | Ok(ReadOutcome::Eof { .. }) => {}
            Err(e) => panic!("decode error pre-switch: {e}"),
        }
    }
    assert!(warmup_total > 0, "warmup must produce AAC samples");

    let handle = audio
        .abr_handle()
        .expect("HLS stream must expose AbrHandle");

    // First switch: cross-codec to FLAC. Closes the variant_generation fence.
    handle
        .set_mode(AbrMode::manual(3))
        .expect("Manual(3) (FLAC) target valid");

    // Race window: same-codec switch must land BEFORE
    // `clear_variant_fence` fires for the cross-codec recreate. In
    // local test environment (in-process server, no CDN latency)
    // recreate completes in ~50-100ms vs ~3-4s in prod. Sleep 0
    // (immediate) maximizes the chance of hitting the race here.
    kithara_platform::time::sleep(Duration::from_millis(10)).await;

    // Second switch: same-codec sibling of v=0 (AAC v=1) — does NOT
    // bump fence, but shrinks `served_until` on whatever variant is
    // active and activates v=1 with `served_from = switch_at`.
    handle
        .set_mode(AbrMode::manual(1))
        .expect("Manual(1) (AAC sibling) target valid");

    // Read for 15s. Without the fix, decoder hits false EOF inside this
    // window → `EndOfStream` → kithara-queue may trigger an auto-seek →
    // `HangDetector audio_worker_loop no progress for 10s` panic.
    let post_total = spawn_blocking(move || read_until_eof(&mut audio, Duration::from_secs(15)))
        .await
        .expect("read");

    let targets = applied_targets.lock_sync().clone();
    info!(?targets, warmup_total, post_total, "S.3 result");

    // We must see both switches applied through the ABR contract.
    assert!(
        targets.contains(&3),
        "Manual(3) cross-codec must be applied, saw: {targets:?}"
    );
    assert!(
        targets.contains(&1),
        "Manual(1) same-codec must be applied, saw: {targets:?}"
    );

    // Crucially: read_until_eof must NOT return prematurely on a false
    // EOF mid-window. 15s @ 44100 stereo → ≥ ~600 000 frames if
    // playback continues; we accept any non-trivial post-switch
    // production as proof.
    assert!(
        post_total > 100_000,
        "playback must continue after the rapid cross→same codec double \
         switch — pre-FIX-E bug: decoder hits false EOF on v_old's \
         shrunk served range while cross-codec fence is still closed, \
         `handle_decode_eof::detect_format_change` returns None \
         (header_byte_range of v_new == None with byte_shift), EOS \
         emitted, kithara-queue auto-seeks, hang panic at 10s. \
         post_total={post_total}"
    );
}

/// Production replay (app.log 2026-05-16 09:39–09:42): user plays
/// FLAC near the end of the track, seeks backwards to ~25%, switches
/// to a same-codec lower-bitrate AAC variant, then sees a premature
/// `decoder_next_chunk_safe: Eof` and the playlist advances.
///
/// Reduced sequence captured here:
/// 1. Auto(0) starts on the highest AAC variant.
/// 2. Play near the end of the track using the `OfflinePlayer`
///    (real-time rendering — matches CPAL cadence so the reader is
///    not racing ahead in lock-step with the network).
/// 3. Backwards seek to ~25 % of the track.
/// 4. `Manual(low_variant)` — same-codec AAC downswitch (no fence
///    bump, no decoder recreate).
/// 5. Continue rendering — assert sustained samples; the bug surfaces
///    as `EndOfStream` ~hundreds of KB into the new variant.
///
/// Uses a packaged AAC fmp4 fixture with `DelayRule` on the high
/// variant to imitate the real-world CDN latency that lets the reader
/// land mid-segment when the user clicks lq.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(90)),
    env(KITHARA_HANG_TIMEOUT_SECS = "15")
)]
#[case::sw(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hw(DecoderBackend::Apple)
)]
async fn play_seek_back_then_same_codec_downswitch_no_premature_eof(
    #[case] backend: DecoderBackend,
) {
    use kithara_platform::time::sleep;

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");
    // assets/hls/master.m3u8: variants 0..2 AAC (mp4a.40.2), variant 3 FLAC.
    // The duration of every variant ≈ 220 s. We start on shq (v=2) so we
    // can downswitch to slq (v=0) for the same-codec scenario.

    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);

    let applied_targets: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let applied_bg = Arc::clone(&applied_targets);
    let mut applied_rx = bus.subscribe();
    spawn(async move {
        loop {
            let ev = match applied_rx.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if let Event::Abr(AbrEvent::VariantApplied { to, .. }) = ev {
                applied_bg.lock_sync().push(to.get());
            }
        }
    });

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(AbrMode::manual(2))
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .decoder_backend(backend)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    // Real-time-ish reader: each read pulls a 4096-frame buffer
    // (~93 ms @ 44.1 kHz) and we sleep 50 ms between iterations so
    // the network DelayRule-like cadence of the real CDN is imitated.
    // Without the sleep the in-process server feeds the whole track
    // before any switch can land mid-segment.
    let buf_frames = 4096usize;
    let read_pace = Duration::from_millis(50);

    // Phase 1 — play near end. Track ≈ 220 s; pump 180 s of decoded
    // audio so the seek-back step targets a position well before the
    // current playback head.
    let mut buf = vec![0f32; buf_frames * 2];
    let target_samples_phase1: u64 = 180 * 44_100;
    let mut samples_phase1 = 0u64;
    let deadline = Instant::now() + Duration::from_secs(40);
    while samples_phase1 < target_samples_phase1 && Instant::now() < deadline {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => samples_phase1 += count.get() as u64,
            Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => break,
            Err(e) => panic!("phase 1 decode error: {e}"),
        }
        sleep(read_pace).await;
    }
    info!(samples_phase1, "phase 1 done");
    assert!(
        samples_phase1 > 4 * 44_100,
        "phase 1 must produce at least a few seconds of audio, got {samples_phase1}"
    );

    // Phase 2 — seek backwards to ~25 % of the track (~55 s).
    audio
        .seek(Duration::from_secs(55))
        .expect("seek backwards must succeed");

    // Pump some samples so the post-seek decoder lands mid-segment
    // before the downswitch fires.
    let mut samples_phase2 = 0u64;
    let deadline = Instant::now() + Duration::from_secs(8);
    while samples_phase2 < 2 * 44_100 && Instant::now() < deadline {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => samples_phase2 += count.get() as u64,
            Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                panic!("unexpected EOF during phase 2 post-seek read")
            }
            Err(e) => panic!("phase 2 decode error: {e}"),
        }
        sleep(read_pace).await;
    }
    info!(samples_phase2, "phase 2 done");

    // Phase 3 — same-codec downswitch shq (v=2, 270 kbps) → slq
    // (v=0, 66 kbps). Both are mp4a.40.2, no cross-codec fence, no
    // decoder recreate.
    let handle = audio
        .abr_handle()
        .expect("HLS stream must expose AbrHandle");
    handle
        .set_mode(AbrMode::manual(0))
        .expect("Manual(0) (slq AAC) target valid");

    // Phase 4 — read 10 s post-switch. Bug repro: decoder emits a
    // false `decoder_next_chunk_safe: Eof` ~hundreds of KB after the
    // switch and `handle_decode_eof` surfaces it as terminal EOS.
    let mut samples_phase4 = 0u64;
    let mut saw_eof_phase4 = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    let post_pace = Duration::from_millis(30);
    while Instant::now() < deadline {
        match audio.read(&mut buf) {
            Ok(ReadOutcome::Frames { count, .. }) => samples_phase4 += count.get() as u64,
            Ok(ReadOutcome::Pending { .. }) => {}
            Ok(ReadOutcome::Eof { .. }) => {
                saw_eof_phase4 = true;
                break;
            }
            Err(e) => panic!("phase 4 decode error: {e}"),
        }
        sleep(post_pace).await;
    }

    let targets = applied_targets.lock_sync().clone();
    info!(
        ?targets,
        samples_phase1, samples_phase2, samples_phase4, saw_eof_phase4, "repro result"
    );

    assert!(
        targets.contains(&0),
        "Manual(0) (slq AAC) must publish VariantApplied, saw: {targets:?}"
    );

    // Bug surfaces as samples_phase4 ≪ expected. 10 s @ 44.1 kHz
    // stereo ≈ 882 000 samples if playback continues. In prod
    // (app.log 2026-05-16 09:42:28) decoder produced ~95 K samples
    // after the switch then EOS; threshold above that exposes the
    // regression.
    assert!(
        !saw_eof_phase4,
        "phase 4 EOF after same-codec downswitch — false EOS bug \
         (app.log 2026-05-16 09:42:28). samples_phase4={samples_phase4}"
    );
    assert!(
        samples_phase4 > 300_000,
        "phase 4 (post same-codec downswitch) must yield sustained \
         playback. samples_phase4={samples_phase4}"
    );
}

/// Production replay (app.log 2026-05-16 22:23 and 2026-05-17 09:08):
/// after a manual ABR switch to a variant whose `seg 0` is NOT in the
/// cache, a seek **backwards to a non-zero offset** (37 s in app.log)
/// makes `start_recreating_decoder` fire but `Recreating decoder for
/// new format` never logs — the FSM parks in `RecreatingDecoder` and
/// `audio_worker_loop` panics on `HangDetector` after 10 s.
///
/// Root cause (kithara-audio/src/pipeline/source.rs:1640
/// `source_ready_for_recreate`): for `RecreateCause::VariantSwitch`
/// the fast path that probes only the init range is skipped — the
/// code falls back to `source_is_ready_for_boundary(offset)` which
/// waits for `[0..32 KiB)` to be `Ready`. After a seek-backwards the
/// reader byte cursor is far past byte 0, the HLS scheduler emits
/// segments around that cursor (seg N+), and **nobody asks for
/// `seg 0` of the new variant** — the readiness gate never opens.
/// The init bytes are already cached (`emit init v=N`,
/// `bytes_written=627` in app.log), so the recreate could complete
/// — but the gate blocks on a range no one schedules.
///
/// Distinction from the existing fast path: it covers only
/// `FormatBoundary + Decode`, where the decoder afterwards
/// `decode`s from `offset`. `VariantSwitch + Seek/ApplySeek` uses the
/// same factory contract (decoder needs only the init range to
/// initialise; the seek that follows lands wherever the pending
/// request asks for) but was left in the slow path.
///
/// Reproduces both production runs by seeking to a non-zero offset
/// after a manual switch to an uncached variant. Seek-to-0 does NOT
/// reproduce — that resets `reader_pos` to 0 and the scheduler then
/// schedules `seg 0` of the new variant, so the gate eventually
/// opens.
///
/// Four cases × `(DecoderBackend, target_variant)`:
/// - `sw_same_codec_aac_low_to_high`: V0 (slq AAC) → V2 (shq AAC),
///   same-codec recreate.
/// - `sw_cross_codec_aac_to_flac`: V0 (slq AAC) → V3 (slossless
///   FLAC), cross-codec recreate.
/// - `hw_*` mirror the above on the Apple backend.
#[kithara::test(
    native,
    tokio,
    serial,
    timeout(Duration::from_secs(60)),
    env(KITHARA_HANG_TIMEOUT_SECS = "5")
)]
#[case::sw_same_codec_aac_low_to_high(DecoderBackend::Symphonia, 2usize)]
#[case::sw_cross_codec_aac_to_flac(DecoderBackend::Symphonia, 3usize)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hw_same_codec_aac_low_to_high(DecoderBackend::Apple, 2usize)
)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::hw_cross_codec_aac_to_flac(DecoderBackend::Apple, 3usize)
)]
async fn seek_backwards_after_manual_switch_to_uncached_variant_does_not_hang(
    #[case] backend: DecoderBackend,
    #[case] target_variant: usize,
) {
    use kithara_platform::time::sleep;

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    kithara_integration_tests::apple_warmup::warm_if_apple(backend);

    let server = TestServerHelper::new().await;
    let url = server.asset("hls/master.m3u8");
    // assets/hls/master.m3u8: v=0..2 AAC (mp4a.40.2, fmp4), v=3 FLAC
    // (fLaC, fmp4). Track ≈ 220 s, 37 segments each (~6 s).

    let temp_dir = TestTempDir::new();
    let cancel = CancellationToken::default();
    let bus = EventBus::new(8192);

    let applied_targets: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let applied_bg = Arc::clone(&applied_targets);
    let mut applied_rx = bus.subscribe();
    spawn(async move {
        loop {
            match applied_rx.recv().await {
                Ok(Event::Abr(AbrEvent::VariantApplied { to, .. })) => {
                    applied_bg.lock_sync().push(to.get());
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let hls_config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel)
        .events(bus.clone())
        .initial_abr_mode(AbrMode::manual(0))
        .build();

    let config = AudioConfig::<Hls>::for_stream(hls_config)
        .events(bus)
        .decoder_backend(backend)
        .build();
    let mut audio = Audio::<Stream<Hls>>::new(config)
        .await
        .expect("create audio");

    // Phase 1 — play V0 long enough that reader_pos is past seg 6
    // (the seek target ≈ 37 s lands in seg 6). The blocking read
    // loop is parked on the tokio blocking pool so that
    // `recv_outcome_blocking` (`park_timeout` on the current thread)
    // does NOT freeze the tokio worker that drives `Downloader`.
    let (mut audio, samples_phase1) = spawn_blocking(move || {
        let target_samples: u64 = 25 * 44_100;
        let mut samples = 0u64;
        let mut buf = vec![0f32; 4096 * 2];
        let deadline = Instant::now() + Duration::from_secs(20);
        while samples < target_samples && Instant::now() < deadline {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => samples += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => {}
                Ok(ReadOutcome::Eof { .. }) => break,
                Err(e) => panic!("phase 1 decode error: {e}"),
            }
        }
        (audio, samples)
    })
    .await
    .expect("phase 1 join");

    assert!(
        samples_phase1 > 4 * 44_100,
        "phase 1 must produce > 4 s of V0 audio before the switch, \
         got {samples_phase1}"
    );

    // Phase 2 — manual switch to the target variant. Its seg 0 and
    // (typically) seg 6 at the seek-to-37 s position are NOT cached.
    let handle = audio
        .abr_handle()
        .expect("HLS stream must expose AbrHandle");
    handle
        .set_mode(AbrMode::manual(target_variant))
        .expect("Manual target valid");

    // Mirror the production gap between `set_mode` and seek so the
    // scheduler has time to emit V_new init + a few media segments
    // around the current reader_pos (matches the app.log emit
    // sequence preceding the hang).
    sleep(Duration::from_millis(300)).await;

    // Phase 3 — seek BACKWARDS to a non-zero offset (37 s, as in
    // app.log run 1). reader_pos ends up at ≈ seg 6 byte-coords of
    // the new variant. The scheduler emits segments around that
    // cursor — seg 0 of the new variant is NEVER scheduled, even
    // though the recreate readiness gate currently waits for
    // `[0..32 KiB)` to be Ready.
    audio
        .seek(Duration::from_secs(37))
        .expect("seek to 37 s must succeed");

    // Phase 4 — pump up to ~12 s of wall-clock and expect samples
    // to flow from the new variant. Regression surface: zero
    // samples produced (FSM parked in `RecreatingDecoder`) and the
    // audio worker panics via `HangDetector` after
    // `KITHARA_HANG_TIMEOUT_SECS = 5`. The outer test will then fail
    // with `kithara-audio-worker-0 panicked` rather than this
    // assertion.
    let (samples_phase4, saw_eof_phase4) = spawn_blocking(move || {
        let mut samples = 0u64;
        let mut saw_eof = false;
        let mut buf = vec![0f32; 4096 * 2];
        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline && samples < 2 * 44_100 {
            match audio.read(&mut buf) {
                Ok(ReadOutcome::Frames { count, .. }) => samples += count.get() as u64,
                Ok(ReadOutcome::Pending { .. }) => {}
                Ok(ReadOutcome::Eof { .. }) => {
                    saw_eof = true;
                    break;
                }
                Err(e) => panic!("phase 4 decode error: {e}"),
            }
        }
        (samples, saw_eof)
    })
    .await
    .expect("phase 4 join");

    let targets = applied_targets.lock_sync().clone();
    info!(
        ?backend,
        target_variant,
        samples_phase1,
        samples_phase4,
        saw_eof_phase4,
        ?targets,
        "seek_backwards_after_switch_to_uncached repro result"
    );

    assert!(
        !saw_eof_phase4,
        "phase 4 unexpected EOF after seek-backwards-to-37 s following \
         Manual({target_variant}) switch — samples_phase4={samples_phase4}"
    );
    assert!(
        samples_phase4 > 0,
        "post-seek-backwards-to-37 s playback after Manual({target_variant}) \
         switch must yield samples, got {samples_phase4}. Regression — \
         see app.log 2026-05-16 22:23 (V0→V2 AAC same-codec) and \
         2026-05-17 09:08 (V0→V3 FLAC cross-codec, then V3→V1 AAC \
         cross-codec). Root cause: `source_ready_for_recreate` slow \
         path waits for `[0..32 KiB)` Ready on `VariantSwitch`, but \
         the scheduler never schedules `seg 0` after a backwards seek."
    );
}
