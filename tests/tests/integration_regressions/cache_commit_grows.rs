#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;

use kithara::{
    assets::{AssetStore, AssetStoreBuilder, StorageBackend},
    events::{AssetEvent, DownloaderEvent, Event},
    net::{HttpClient, NetOptions},
    platform::{CancelToken, sync::Arc, time::Duration},
    play::{PlayerConfig, PlayerImpl, ResourceConfig},
    queue::{Queue, QueueConfig, TrackSource, Transition},
    stream::dl::{Downloader, DownloaderConfig},
};
use kithara_integration_tests::{
    BehaviorHandle, Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir,
    audio_fixture::EmbeddedAudio, kithara, offline::OfflineSession, temp_dir,
};

#[derive(Default)]
struct Transfer {
    committed_len: Option<u64>,
    completed_bytes: u64,
    saw_first_byte: bool,
}

async fn observe_transfer(rx: &mut kithara::events::EventReceiver, deadline: Duration) -> Transfer {
    let mut transfer = Transfer::default();
    let _ = kithara::platform::time::timeout(deadline, async {
        loop {
            let Ok(envelope) = rx.recv().await else {
                return;
            };
            match envelope.event {
                Event::Downloader(DownloaderEvent::FirstByte { status: 200, .. }) => {
                    transfer.saw_first_byte = true;
                }
                Event::Downloader(DownloaderEvent::RequestCompleted {
                    bytes_transferred, ..
                }) => {
                    transfer.completed_bytes = transfer.completed_bytes.max(bytes_transferred);
                }
                Event::Asset(AssetEvent::Committed {
                    final_len: Some(final_len),
                    ..
                }) if final_len > 0 => {
                    transfer.committed_len = Some(final_len);
                }
                _ => {}
            }
            if transfer.saw_first_byte
                && transfer.completed_bytes > 0
                && transfer.committed_len.is_some()
            {
                return;
            }
        }
    })
    .await;
    transfer
}

fn resource_config(
    handle: &BehaviorHandle,
    downloader: &Downloader,
    store: &AssetStore,
    name: &str,
) -> ResourceConfig {
    let url = handle.child_url(name);
    ResourceConfig::for_src(url.as_str())
        .expect("valid fixture URL")
        .byte_pool(kithara::bufpool::BytePool::default())
        .pcm_pool(kithara::bufpool::PcmPool::default())
        .downloader(downloader.clone())
        .store(store.clone())
        .build()
}

fn dir_size_bytes(root: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            total = total.saturating_add(dir_size_bytes(&entry.path()));
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    total
}

async fn load_and_observe(
    queue: &Queue,
    rx: &mut kithara::events::EventReceiver,
    handle: &BehaviorHandle,
    downloader: &Downloader,
    store: &AssetStore,
    name: &str,
) -> Transfer {
    let id = queue.append(TrackSource::Config(Box::new(resource_config(
        handle, downloader, store, name,
    ))));
    queue
        .select(id, Transition::None)
        .unwrap_or_else(|error| panic!("select {name}: {error}"));
    observe_transfer(rx, Duration::from_secs(20)).await
}

#[kithara::test(tokio, multi_thread, timeout(Duration::from_secs(90)))]
async fn played_tracks_land_in_the_disk_cache(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let handles: Vec<_> = (0..2)
        .map(|_| {
            helper.register_behavior(FixtureBehavior {
                content: Content::StaticBytes {
                    bytes: Arc::new(EmbeddedAudio::TEST_MP3_BYTES.to_vec()),
                    content_type: Some("audio/mpeg"),
                },
                delivery: Delivery::Throttle {
                    chunk: 32 * 1024,
                    delay_ms: 10,
                },
            })
        })
        .collect();
    let downloader = Downloader::new(
        DownloaderConfig::builder()
            .client(HttpClient::new(NetOptions::default(), CancelToken::never()))
            .build(),
    );
    let player = Arc::new(PlayerImpl::new(
        PlayerConfig::builder()
            .byte_pool(kithara::bufpool::BytePool::default())
            .pcm_pool(kithara::bufpool::PcmPool::default())
            .session(OfflineSession::arc_auto())
            .build(),
    ));
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: temp_dir.path().to_path_buf(),
        })
        .event_bus(player.bus().clone())
        .build();
    let queue = Queue::new(
        QueueConfig::default()
            .with_player(player)
            .with_store(store.clone()),
    );
    let mut rx = queue.subscribe();

    let before = dir_size_bytes(temp_dir.path());
    let first = load_and_observe(
        &queue,
        &mut rx,
        &handles[0],
        &downloader,
        &store,
        "first.mp3",
    )
    .await;
    assert!(
        handles[0].request_count() > 0 && first.saw_first_byte && first.completed_bytes > 0,
        "precondition: first throttled transfer did not complete with bytes \
         (requests={}, first_byte={}, bytes={})",
        handles[0].request_count(),
        first.saw_first_byte,
        first.completed_bytes
    );
    let first_commit = first.committed_len.unwrap_or_else(|| {
        panic!(
            "first transfer completed {} bytes but no AssetEvent::Committed arrived",
            first.completed_bytes
        )
    });
    let after_first = dir_size_bytes(temp_dir.path());
    assert!(
        after_first > before,
        "first asset committed {first_commit} bytes but cache root did not grow \
         ({before} -> {after_first} bytes)"
    );

    let second = load_and_observe(
        &queue,
        &mut rx,
        &handles[1],
        &downloader,
        &store,
        "second.mp3",
    )
    .await;
    assert!(
        handles[1].request_count() > 0 && second.saw_first_byte && second.completed_bytes > 0,
        "precondition: second throttled transfer did not complete with bytes \
         (requests={}, first_byte={}, bytes={})",
        handles[1].request_count(),
        second.saw_first_byte,
        second.completed_bytes
    );
    let second_commit = second.committed_len.unwrap_or_else(|| {
        panic!(
            "second transfer completed {} bytes but no AssetEvent::Committed arrived",
            second.completed_bytes
        )
    });
    let after_second = dir_size_bytes(temp_dir.path());
    assert!(
        after_second > after_first,
        "second asset committed {second_commit} bytes but cache did not grow \
         ({after_first} -> {after_second} bytes)"
    );

    queue.clear();
}
