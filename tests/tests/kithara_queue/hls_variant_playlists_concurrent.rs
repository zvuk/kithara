#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::collections::HashSet;

use kithara::{
    assets::StoreOptions,
    decode::DecoderBackend,
    events::{AbrMode, DownloaderEvent, Event, EventReceiver, QueueEvent, TrackId, TrackStatus},
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        sync::Arc,
        time::{Duration, sleep, timeout},
        tokio,
        tokio::sync::broadcast::error::RecvError,
    },
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir, kithara, offline::OfflineSession, temp_dir,
};
use kithara_test_utils::probe::capture as probe_capture;
use url::Url;

struct Consts;
impl Consts {
    const VARIANT_COUNT: usize = 3;
    const SEGMENTS_PER_VARIANT: usize = 4;
    const SEGMENT_DURATION_S: f64 = 4.0;
    const MAX_CONCURRENT: usize = 3;
    const LOAD_DEADLINE: Duration = Duration::from_secs(20);
}

/// `BatchGroup::process` probe (the downloader's batch processor). Fires once
/// per dispatched batch with `batch_size` + `first_request_id` wire fields.
const PROCESS_PROBE: &str = "process";

async fn build_hls(helper: &TestServerHelper) -> Url {
    let builder = HlsFixtureBuilder::new()
        .variant_count(Consts::VARIANT_COUNT)
        .segments_per_variant(Consts::SEGMENTS_PER_VARIANT)
        .segment_duration_secs(Consts::SEGMENT_DURATION_S)
        .packaged_audio_aac_lc(44_100, 2);
    helper
        .create_hls(builder)
        .await
        .expect("create HLS fixture")
        .master_url()
}

/// Queue tick driver, flash-coherent: spawned through the platform chokepoint
/// so the cadence rides the virtual clock in flash-enabled test runs.
#[kithara::flash(true)]
async fn drive_queue_ticks(queue: Arc<Queue>) {
    loop {
        sleep(Duration::from_millis(50)).await;
        if queue.tick().is_err() {
            break;
        }
    }
}

fn build_queue_with_tick(
    temp_dir: &TestTempDir,
) -> (
    Arc<Queue>,
    Arc<PlayerImpl>,
    Downloader,
    StoreOptions,
    tokio::task::JoinHandle<()>,
) {
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let queue = Arc::new(Queue::new(
        QueueConfig::default().with_player(Arc::clone(&player)),
    ));
    let tick_handle = tokio::task::spawn(drive_queue_ticks(Arc::clone(&queue)));
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .max_concurrent(Consts::MAX_CONCURRENT)
            .build(),
    );
    let store = StoreOptions::new(temp_dir.path());
    (queue, player, downloader, store, tick_handle)
}

fn is_variant_media_playlist(url: &Url, master_url: &Url) -> bool {
    if url == master_url || !url.path().ends_with(".m3u8") {
        return false;
    }

    let Some(file_name) = url.path_segments().and_then(Iterator::last) else {
        return false;
    };
    let Some(variant) = file_name
        .strip_prefix('v')
        .and_then(|name| name.strip_suffix(".m3u8"))
    else {
        return false;
    };
    variant.parse::<usize>().is_ok()
}

async fn observe_until_loaded(
    rx: &mut EventReceiver,
    queue: &Queue,
    track_id: TrackId,
    master_url: &Url,
) -> Result<HashSet<u64>, String> {
    let mut variant_request_ids = HashSet::new();

    timeout(Consts::LOAD_DEADLINE, async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(Event::Downloader(DownloaderEvent::RequestEnqueued {
                    request_id, url, ..
                })) => {
                    if is_variant_media_playlist(&url, master_url) {
                        variant_request_ids.insert(request_id.get());
                    }
                }
                Ok(Event::Queue(QueueEvent::TrackStatusChanged { id, status }))
                    if id == track_id =>
                {
                    match status {
                        TrackStatus::Loaded => return Ok(()),
                        TrackStatus::Failed(error) => {
                            return Err(format!(
                                "track {track_id} failed while loading: {error}; \
                                 variant_request_ids: {}",
                                format_variant_request_ids(&variant_request_ids),
                            ));
                        }
                        _ => {}
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => {
                    return Err(format!(
                        "event bus closed before track {track_id} loaded; \
                         variant_request_ids: {}",
                        format_variant_request_ids(&variant_request_ids),
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| {
        format!(
            "track {track_id} did not reach Loaded within {:?} (last status: {:?}); \
             variant_request_ids: {}",
            Consts::LOAD_DEADLINE,
            queue.track(track_id).map(|entry| entry.status),
            format_variant_request_ids(&variant_request_ids),
        )
    })??;

    Ok(variant_request_ids)
}

/// Largest `process` batch whose first request is a variant media playlist.
/// Serial playlist loading yields 1 (each playlist is its own batch);
/// concurrent `try_join_all` loading batches them, yielding `VARIANT_COUNT`.
/// `None` means no such probe fired (probe feature off or request-id mismatch).
fn max_playlist_batch_size(
    probes: &[probe_capture::ProbeEvent],
    variant_request_ids: &HashSet<u64>,
) -> Option<u64> {
    process_probes(probes)
        .filter(|probe| {
            probe
                .u64("first_request_id")
                .is_some_and(|request_id| variant_request_ids.contains(&request_id))
        })
        .filter_map(|probe| probe.u64("batch_size"))
        .max()
}

fn process_probes(
    probes: &[probe_capture::ProbeEvent],
) -> impl Iterator<Item = &probe_capture::ProbeEvent> {
    probes
        .iter()
        .filter(|probe| probe.target == "kithara_stream_probe")
        .filter(|probe| probe.probe_name() == Some(PROCESS_PROBE))
}

fn format_process_probes(probes: &[probe_capture::ProbeEvent]) -> String {
    let entries = process_probes(probes)
        .filter_map(|probe| {
            Some(format!(
                "(seq={}, batch_size={}, first_request_id={})",
                probe.seq()?,
                probe.u64("batch_size")?,
                probe.u64("first_request_id")?,
            ))
        })
        .collect::<Vec<_>>();
    format!("[{}]", entries.join(", "))
}

fn format_variant_request_ids(request_ids: &HashSet<u64>) -> String {
    let mut request_ids: Vec<_> = request_ids.iter().copied().collect();
    request_ids.sort_unstable();
    format!("{request_ids:?}")
}

#[kithara::test(tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn variant_media_playlists_load_concurrently(#[case] decoder: DecoderBackend) {
    let recorder = probe_capture::install();

    let helper = TestServerHelper::new().await;
    let url = build_hls(&helper).await;

    let temp = temp_dir();
    let (queue, player, downloader, store, tick_handle) = build_queue_with_tick(&temp);

    let mut rx = player.bus().subscribe();

    let cfg = ResourceConfig::for_src(url.as_str())
        .expect("ResourceConfig::for_src")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader.clone())
        .store(store)
        .initial_abr_mode(AbrMode::Auto(None))
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(decoder)
                .build(),
        )
        .build();

    let track_id = queue.append(TrackSource::Config(Box::new(cfg)));
    queue.select(track_id, Transition::None).expect("select");

    let variant_request_ids = match observe_until_loaded(&mut rx, &queue, track_id, &url).await {
        Ok(request_ids) => request_ids,
        Err(error) => {
            tick_handle.abort();
            panic!("{error}");
        }
    };
    let probes = recorder.snapshot();
    tick_handle.abort();

    let process_dump = format_process_probes(&probes);
    let max_batch = max_playlist_batch_size(&probes, &variant_request_ids);

    eprintln!(
        "DIAG variant_request_ids={} max_playlist_batch={max_batch:?} process()={process_dump}",
        format_variant_request_ids(&variant_request_ids),
    );

    assert!(
        variant_request_ids.len() >= 2,
        "expected at least 2 variant media playlists to load (fixture has {}); got {}",
        Consts::VARIANT_COUNT,
        format_variant_request_ids(&variant_request_ids),
    );

    assert!(
        max_batch.is_some_and(|size| size >= 2),
        "variant media playlists were not batched at the downloader's batch processor \
         (`BatchGroup::process`): largest batch starting at a variant-playlist request = \
         {max_batch:?}, expected >= 2 (serial loading yields 1, concurrent `try_join_all` \
         yields {}). variant_request_ids={}, process()={process_dump}",
        Consts::VARIANT_COUNT,
        format_variant_request_ids(&variant_request_ids),
    );
}
