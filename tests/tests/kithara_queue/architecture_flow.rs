#![cfg(not(target_arch = "wasm32"))]

use std::env;

use kithara::{
    assets::AssetStore,
    decode::DecoderBackend,
    platform::{sync::Arc, time::Duration},
    play::ResourceConfig,
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::Downloader,
};
use kithara_devtools::viz::trace::{TraceRecord, TraceRecordKind};
use kithara_integration_tests::{
    TestServerHelper, architecture_trace, disk_asset_store, hls_fixture::create_test_downloader,
    kithara, temp_dir, waits::wait_for_loader_done_event,
};
use kithara_test_utils::probe::capture as probe_capture;
use serial_test::serial;

use super::offline_player_harness::{OfflinePlayerHarness, OfflinePlayerOptions};

const SAMPLE_RATE: u32 = 44_100;
const BLOCK_FRAMES: usize = 512;
const RENDER_BLOCK_BUDGET: usize = 512;

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(120)))]
#[ignore = "run through just arch viz --scenario queue-playback"]
#[serial]
async fn queue_playback_architecture() {
    let trace_path = env::var_os("ARCHITECTURE_TRACE_PATH").expect("architecture trace path");
    let probes = probe_capture::install();
    let helper = TestServerHelper::new().await;
    let url = helper.asset("track.mp3");
    let temp = temp_dir();
    let store = disk_asset_store(temp.path());
    let harness = OfflinePlayerHarness::with_sample_rate(
        OfflinePlayerOptions::builder()
            .crossfade_duration(0.0)
            .build(),
        SAMPLE_RATE,
    );
    let queue = Queue::new(
        QueueConfig::default()
            .with_player(Arc::clone(harness.player()))
            .with_store(store.clone()),
    );
    let downloader = create_test_downloader();
    let config = resource_config(url.as_str(), downloader, store);
    let mut events = queue.subscribe();

    let track_id = queue.append(TrackSource::Config(Box::new(config)));
    wait_for_loader_done_event(&mut events, &queue, track_id, Duration::from_secs(30))
        .await
        .expect("local MP3 load");
    queue
        .select(track_id, Transition::None)
        .expect("select loaded MP3");

    let peak = kithara::platform::time::timeout(Duration::from_secs(30), async {
        for _ in 0..RENDER_BLOCK_BUDGET {
            let _ = queue.tick();
            let block = harness.render(BLOCK_FRAMES);
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
        TraceRecord::new(1, TraceRecordKind::SpanEnter, "Queue::append")
            .with_span("append")
            .with_resource("Track", &resource_id),
        TraceRecord::new(2, TraceRecordKind::TaskSpawn, "Queue::append")
            .with_task(format!("loader-{resource_id}"))
            .with_resource("Track", &resource_id),
        TraceRecord::new(3, TraceRecordKind::ResourceCreate, "Resource::new")
            .with_task(format!("loader-{resource_id}"))
            .with_resource("Decoder", &resource_id),
        TraceRecord::new(4, TraceRecordKind::Send, "Resource::new")
            .with_correlation(format!("loaded-{resource_id}"))
            .with_resource("Track", &resource_id),
        TraceRecord::new(5, TraceRecordKind::Receive, "Queue::select")
            .with_correlation(format!("loaded-{resource_id}"))
            .with_resource("Track", &resource_id),
        TraceRecord::new(
            6,
            TraceRecordKind::ResourceTransfer,
            "PlayerImpl::replace_item",
        )
        .with_resource("Decoder", &resource_id),
        TraceRecord::new(7, TraceRecordKind::SpanEnter, "PlayerImpl::play")
            .with_span("playback")
            .with_resource("Track", &resource_id),
        TraceRecord::new(
            8,
            TraceRecordKind::ResourceTransfer,
            "OfflineSession::render",
        )
        .with_parent_span("playback")
        .with_resource("PCM", &resource_id),
    ];
    architecture_trace::write(trace_path.as_ref(), records, &probes)
        .expect("write queue architecture trace");
}

fn resource_config(url: &str, downloader: Downloader, store: AssetStore) -> ResourceConfig {
    ResourceConfig::for_src(url)
        .expect("valid local fixture URL")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader)
        .store(store)
        .decoder(
            kithara::audio::AudioDecoderConfig::builder()
                .backend(DecoderBackend::Symphonia)
                .build(),
        )
        .build()
}
