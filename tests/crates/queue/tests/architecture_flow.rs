#![cfg(not(target_os = "android"))]
#![cfg(not(target_arch = "wasm32"))]

use std::env;

use kithara::{
    assets::AssetStore,
    decode::DecoderBackend,
    download::Downloader,
    platform::time::Duration,
    play::{ResourceConfig, ResourceSrc},
    queue::{Queue, QueueConfig, QueueControl, TrackSource, Transition},
};
use kithara_devtools_trace_format::{TRACE_SCHEMA_VERSION, TraceRecordKind};
use kithara_integration_tests::{
    TestServerHelper, architecture_trace, disk_asset_store,
    hls_fixture::create_test_downloader,
    kithara,
    offline::{OfflinePlayer, OfflinePlayerOptions},
    served_mp3, usdt_trace,
    waits::wait_for_loader_done_event,
};
use kithara_test_utils::temp_dir;
use serde_json::json;
use serial_test::serial;
use url::Url;

use crate::bufpool_ext::TestPools;

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const RENDER_BLOCK_BUDGET: usize = 512;

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[ignore = "run through just arch viz --scenario queue-playback"]
#[serial]
async fn queue_playback_architecture(#[future(awt)] served_mp3: (TestServerHelper, Url)) {
    let trace_path = env::var_os("ARCHITECTURE_TRACE_PATH").expect("architecture trace path");
    let probes = usdt_trace::scope();
    let (_helper, url) = served_mp3;
    let temp = temp_dir();
    let store = disk_asset_store(temp.path());
    let harness = OfflinePlayer::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    )
    .await;
    let queue = harness
        .insert_control(Queue::new(
            QueueConfig::builder()
                .prep(harness.resource_prep().clone())
                .store(store.clone())
                .settings(
                    kithara::queue::QueueSettings::builder()
                        .crossfade(kithara::play::CrossfadeSettings {
                            duration: 0.0,
                            ..kithara::play::CrossfadeSettings::default()
                        })
                        .build(),
                )
                .build(),
        ))
        .await;
    let downloader = create_test_downloader();
    let config = resource_config(url.as_str(), downloader, store);
    let mut events = queue.subscribe();

    let track_id = harness
        .run(&queue, move |q| {
            q.append(TrackSource::Config(Box::new(config)))
        })
        .await
        .expect("append local MP3");
    wait_for_loader_done_event(&mut events, &queue, track_id, Duration::from_secs(30))
        .await
        .expect("local MP3 load");
    harness
        .run(&queue, move |q| q.select(track_id, Transition::None))
        .await
        .expect("select loaded MP3");
    harness.run(&queue, QueueControl::play).await;

    let peak = kithara::platform::time::timeout(Duration::from_secs(30), async {
        for _ in 0..RENDER_BLOCK_BUDGET {
            let _ = harness.run(&queue, QueueControl::tick).await;
            let block = harness.render(BLOCK_FRAMES).await;
            let peak = block.iter().copied().map(f32::abs).fold(0.0, f32::max);
            if peak > 0.005 {
                return Some(peak);
            }
            kithara::platform::tokio::task::yield_now().await;
        }
        None
    })
    .await
    .expect("render completes within time budget")
    .expect("non-silent PCM within render block budget");
    assert!(peak > 0.005, "decoded MP3 must produce non-silent PCM");

    let resource_id = track_id.as_u64().to_string();
    let records = vec![
        json!({
            "schema_version": TRACE_SCHEMA_VERSION, "sequence": 1,
            "kind": TraceRecordKind::SpanEnter, "name": "Queue::append",
            "span_id": "append", "resource_type": "Track", "resource_id": resource_id
        }),
        json!({
            "schema_version": TRACE_SCHEMA_VERSION, "sequence": 2,
            "kind": TraceRecordKind::TaskSpawn, "name": "Queue::append",
            "task_id": format!("loader-{resource_id}"),
            "resource_type": "Track", "resource_id": resource_id
        }),
        json!({
            "schema_version": TRACE_SCHEMA_VERSION, "sequence": 3,
            "kind": TraceRecordKind::ResourceCreate, "name": "Resource::new",
            "task_id": format!("loader-{resource_id}"),
            "resource_type": "Decoder", "resource_id": resource_id
        }),
        json!({
            "schema_version": TRACE_SCHEMA_VERSION, "sequence": 4,
            "kind": TraceRecordKind::Send, "name": "Resource::new",
            "correlation_id": format!("loaded-{resource_id}"),
            "resource_type": "Track", "resource_id": resource_id
        }),
        json!({
            "schema_version": TRACE_SCHEMA_VERSION, "sequence": 5,
            "kind": TraceRecordKind::Receive, "name": "Queue::select",
            "correlation_id": format!("loaded-{resource_id}"),
            "resource_type": "Track", "resource_id": resource_id
        }),
        json!({
            "schema_version": TRACE_SCHEMA_VERSION, "sequence": 6,
            "kind": TraceRecordKind::ResourceTransfer, "name": "PlayerImpl::select",
            "resource_type": "Decoder", "resource_id": resource_id
        }),
        json!({
            "schema_version": TRACE_SCHEMA_VERSION, "sequence": 7,
            "kind": TraceRecordKind::SpanEnter, "name": "PlayerImpl::play",
            "span_id": "playback", "resource_type": "Track", "resource_id": resource_id
        }),
        json!({
            "schema_version": TRACE_SCHEMA_VERSION, "sequence": 8,
            "kind": TraceRecordKind::ResourceTransfer, "name": "Host::render",
            "parent_span_id": "playback", "resource_type": "PCM", "resource_id": resource_id
        }),
    ];
    architecture_trace::write(trace_path.as_ref(), records, &probes)
        .expect("write queue architecture trace");
    harness.close().await;
}

fn resource_config(
    url: &str,
    downloader: Downloader,
    store: AssetStore<TestPools>,
) -> ResourceConfig<TestPools> {
    ResourceConfig::for_src(ResourceSrc::parse(url).expect("valid local fixture URL"))
        .downloader(downloader)
        .store(store)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .build(),
        )
        .build()
}
