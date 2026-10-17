#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::collections::HashSet;

use kithara::{
    abr::AbrMode,
    decode::DecoderBackend,
    download::DownloaderEvent,
    events::{EventReceiver, TrackId},
    platform::{
        time::{Duration, timeout},
        tokio::sync::broadcast::error::RecvError,
    },
    play::{ResourceConfig, ResourceSrc},
    queue::{QueueControl, QueueEvent, TrackSource, TrackStatus, Transition},
};
use kithara_integration_tests::{
    CreatedHls, HlsFixtureBuilder, TestServerHelper,
    bufpool_ext::TestPools,
    event::TestEvent,
    kithara,
    offline::DiskQueue,
    test_server::MediaPlaylistGateHandle,
    usdt_trace::{self, ProbeEvent},
};
use kithara_test_utils::{temp_dir, wait_until};
use url::Url;

mod consts {
    use super::Duration;

    pub(super) const VARIANT_BANDWIDTHS: [u64; 3] = [192_000, 64_000, 128_000];
    pub(super) const VARIANT_COUNT: usize = VARIANT_BANDWIDTHS.len();
    pub(super) const SEGMENTS_PER_VARIANT: usize = 4;
    pub(super) const SEGMENT_DURATION_S: f64 = 4.0;
    pub(super) const MAX_CONCURRENT: usize = 3;
    pub(super) const LOAD_DEADLINE: Duration = Duration::from_secs(20);
}

async fn build_hls(helper: &TestServerHelper) -> CreatedHls {
    let builder = HlsFixtureBuilder::new()
        .variant_count(consts::VARIANT_COUNT)
        .variant_bandwidths(consts::VARIANT_BANDWIDTHS.to_vec())
        .segments_per_variant(consts::SEGMENTS_PER_VARIANT)
        .segment_duration_secs(consts::SEGMENT_DURATION_S)
        .packaged_audio_aac_lc(44_100, 2);
    helper
        .create_hls(builder)
        .await
        .expect("create HLS fixture")
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
    gates: &[MediaPlaylistGateHandle],
) -> Result<HashSet<u64>, String> {
    let mut variant_request_ids = HashSet::new();

    timeout(consts::LOAD_DEADLINE, async {
        wait_until(
            consts::LOAD_DEADLINE,
            "all three withheld variant GETs",
            || gates.iter().all(|gate| gate.requested() > 0),
        )
        .await
        .map_err(|error| {
            format!(
                "{error}; withheld variant GET counts: {:?}",
                gates
                    .iter()
                    .map(MediaPlaylistGateHandle::requested)
                    .collect::<Vec<_>>(),
            )
        })?;
        for gate in gates {
            gate.release();
        }
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
                        TrackStatus::Pending
                        | TrackStatus::Loading
                        | TrackStatus::Slow
                        | TrackStatus::Consumed
                        | TrackStatus::Cancelled => {}
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
             withheld variant GET counts: {:?}; variant_request_ids: {}",
            consts::LOAD_DEADLINE,
            queue.track(track_id).map(|entry| entry.status),
            gates
                .iter()
                .map(MediaPlaylistGateHandle::requested)
                .collect::<Vec<_>>(),
            format_variant_request_ids(&variant_request_ids),
        )
    })??;

    Ok(variant_request_ids)
}

fn assert_playlist_requests_started(records: &[ProbeEvent], request_ids: &HashSet<u64>) {
    for request_id in request_ids {
        for probe in ["start_request", "establish", "deliver"] {
            assert!(
                records.iter().any(|record| {
                    record.probe == probe && record.field("request_id") == Some(*request_id)
                }),
                "variant request {request_id} did not execute {probe}",
            );
        }
    }
}

fn format_variant_request_ids(request_ids: &HashSet<u64>) -> String {
    let mut request_ids: Vec<_> = request_ids.iter().copied().collect();
    request_ids.sort_unstable();
    format!("{request_ids:?}")
}

#[kithara::test(tokio, multi_thread, serial, timeout(Duration::from_secs(60)))]
#[cfg_attr(not(target_os = "android"), case::symphonia(DecoderBackend::Symphonia))]
#[cfg_attr(target_os = "android", case::android(DecoderBackend::default()))]
#[cfg_attr(
    any(target_os = "macos", target_os = "ios"),
    case::apple(DecoderBackend::Apple)
)]
async fn variant_media_playlists_load_concurrently(
    #[future(awt)] prepared_hls: (TestServerHelper, CreatedHls),
    #[case] decoder: DecoderBackend,
) {
    let (server, hls) = prepared_hls;
    let url = hls.master_url();
    let gates: Vec<_> = (0..consts::VARIANT_COUNT)
        .map(|variant| server.register_media_playlist_gate(hls.token(), variant))
        .collect();
    assert_eq!(consts::MAX_CONCURRENT, consts::VARIANT_COUNT);
    assert!(gates.iter().all(|gate| gate.requested() == 0));

    let temp = temp_dir();
    let DiskQueue {
        queue,
        downloader,
        store,
        ticker: mut tick_handle,
        ..
    } = DiskQueue::builder(temp.path())
        .max_concurrent_downloads(consts::MAX_CONCURRENT)
        .open()
        .await;

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

    let variant_request_ids =
        match observe_until_loaded(&mut rx, &queue, track_id, &url, &gates).await {
            Ok(request_ids) => request_ids,
            Err(error) => {
                drop(gates);
                tick_handle.stop().await;
                queue.close().await;
                panic!("{error}");
            }
        };
    let records = trace.events();
    drop(trace);
    tick_handle.stop().await;

    assert_eq!(
        variant_request_ids.len(),
        consts::VARIANT_COUNT,
        "all variants must load; got {}",
        format_variant_request_ids(&variant_request_ids),
    );
    assert_playlist_requests_started(&records, &variant_request_ids);

    let variants = queue
        .current_abr_handle()
        .expect("loaded track must expose its variants")
        .variants();
    let actual: Vec<_> = variants
        .iter()
        .map(|variant| (variant.variant_index.get(), variant.bandwidth_bps))
        .collect();
    let expected: Vec<_> = consts::VARIANT_BANDWIDTHS
        .iter()
        .copied()
        .enumerate()
        .map(|(index, bandwidth)| (index, Some(bandwidth)))
        .collect();
    assert_eq!(
        actual, expected,
        "variants must retain master playlist order"
    );
    queue.close().await;
}

#[kithara::fixture]
async fn prepared_hls() -> (TestServerHelper, CreatedHls) {
    let helper = TestServerHelper::new().await;
    let hls = build_hls(&helper).await;
    (helper, hls)
}
