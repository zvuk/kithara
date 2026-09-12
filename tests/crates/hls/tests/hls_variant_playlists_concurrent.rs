#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::collections::HashSet;

use kithara::{
    abr::AbrMode,
    assets::AssetStore,
    decode::DecoderBackend,
    download::{Downloader, DownloaderConfig, DownloaderEvent},
    events::{EventReceiver, TrackId},
    host::HostConfig,
    net::{HttpClient, NetOptions},
    platform::{
        CancelToken,
        time::{Duration, timeout},
        tokio::sync::broadcast::error::RecvError,
    },
    play::{PlayWorker, PlayWorkerConfig, PlayerConfig, PlayerImpl, ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, QueueEvent, TrackSource, TrackStatus, Transition},
};
use kithara_integration_tests::{
    HlsFixtureBuilder, TestServerHelper, TestTempDir,
    event::TestEvent,
    kithara,
    offline::{OfflineQueue, QueueTicker, RENDER_PACE},
    temp_dir,
    usdt_trace::{self, ProbeEvent},
};
use url::Url;

use crate::bufpool_ext::{TestPools, pools};

struct Consts;
impl Consts {
    const VARIANT_COUNT: usize = 3;
    const SEGMENTS_PER_VARIANT: usize = 4;
    const SEGMENT_DURATION_S: f64 = 4.0;
    const MAX_CONCURRENT: usize = 3;
    const LOAD_DEADLINE: Duration = Duration::from_secs(20);
}

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

async fn build_queue_with_tick(
    temp_dir: &TestTempDir,
) -> (
    OfflineQueue<TestPools>,
    Downloader,
    AssetStore<TestPools>,
    QueueTicker,
) {
    let store = kithara_integration_tests::disk_asset_store(temp_dir.path());
    let pools = pools();
    let session = HostConfig::offline(pools.clone()).build();
    let player = PlayerImpl::new(
        PlayerConfig::builder()
            .sample_rate(session.sample_rate())
            .worker(PlayWorker::new(
                PlayWorkerConfig::builder(pools.clone()).build(),
            ))
            .build(),
    );
    let queue = OfflineQueue::paced(
        session,
        Queue::new(
            QueueConfig::builder()
                .player(player)
                .store(store.clone())
                .build(),
        ),
        RENDER_PACE,
    )
    .await
    .expect("create product offline queue");
    let tick_handle = QueueTicker::spawn(queue.control(), Duration::from_millis(50));
    let downloader = Downloader::new(
        DownloaderConfig::for_client(HttpClient::new(
            NetOptions::default(),
            pools,
            CancelToken::never(),
        ))
        .max_concurrent(Consts::MAX_CONCURRENT)
        .build(),
    );
    (queue, downloader, store, tick_handle)
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
    rx: &mut EventReceiver<TestEvent>,
    queue: &QueueControl<TestPools>,
    track_id: TrackId,
    master_url: &Url,
) -> Result<HashSet<u64>, String> {
    let mut variant_request_ids = HashSet::new();

    timeout(Consts::LOAD_DEADLINE, async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(TestEvent::Downloader(DownloaderEvent::RequestEnqueued {
                    request_id,
                    url,
                    ..
                })) => {
                    if is_variant_media_playlist(&url, master_url) {
                        variant_request_ids.insert(request_id.get());
                    }
                }
                Ok(TestEvent::Queue(QueueEvent::TrackStatusChanged { id, status }))
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

/// Largest USDT-observed `BatchGroup::process` batch whose first request is a
/// variant media playlist. Serial playlist loading yields one request per
/// batch; concurrent loading produces a batch of at least two.
fn max_playlist_batch_size(records: &[ProbeEvent], variant_request_ids: &HashSet<u64>) -> Option<u64> {
    records
        .iter()
        .filter(|record| record.probe == "process")
        .filter_map(|record| {
            let batch_size = record.field("batch_size")?;
            let first_request_id = record.field("first_request_id")?;
            variant_request_ids.contains(&first_request_id).then_some(batch_size)
        })
        .max()
}

#[kithara::test(tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[case::symphonia(DecoderBackend::Symphonia)]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn variant_media_playlists_load_concurrently(
    #[future(awt)] prepared_hls: (TestServerHelper, Url),
    #[case] decoder: DecoderBackend,
) {
    let (_server, url) = prepared_hls;

    let temp = temp_dir();
    let (queue, downloader, store, mut tick_handle) = build_queue_with_tick(&temp).await;

    let mut rx = queue.subscribe();

    let cfg =
        ResourceConfig::for_src(ResourceSrc::parse(url.as_str()).expect("ResourceSrc::parse"))
            .downloader(downloader.clone())
            .store(store)
            .initial_abr_mode(AbrMode::Auto(None))
            .decoder(
                kithara::audio::AudioDecoderConfig::builder()
                    .backend(decoder)
                    .build(),
            )
            .build();

    let trace = usdt_trace::scope();
    let track_id = queue
        .run(move |q| q.append(TrackSource::Config(Box::new(cfg))))
        .await
        .expect("append multivariant HLS track");
    queue
        .run(move |q| q.select(track_id, Transition::None))
        .await
        .expect("select");

    let variant_request_ids = match observe_until_loaded(&mut rx, &queue, track_id, &url).await {
        Ok(request_ids) => request_ids,
        Err(error) => {
            tick_handle.stop().await;
            panic!("{error}");
        }
    };
    let records = trace.events();
    tick_handle.stop().await;

    let max_batch = max_playlist_batch_size(&records, &variant_request_ids);

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
         yields {}). variant_request_ids={}",
        Consts::VARIANT_COUNT,
        format_variant_request_ids(&variant_request_ids),
    );
    queue.close().await;
}

#[kithara::fixture]
async fn prepared_hls() -> (TestServerHelper, Url) {
    let helper = TestServerHelper::new().await;
    let url = build_hls(&helper).await;
    (helper, url)
}
