#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    events::{DownloaderEvent, Event, EventBus, FileEvent},
    file::{File, FileConfig},
    platform::{
        CancelToken,
        time::{Duration, Instant, timeout},
    },
    stream::Stream,
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir, temp_dir,
};

const CAPTIVE_PORTAL_HTML: &str = "<html><body>VPN required to access this resource</body></html>";

/// Walk `root` recursively and collect every file that is not inside the
/// `_index` directory.
fn collect_cache_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().and_then(|s| s.to_str()) == Some("_index") {
                    continue;
                }
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

/// After `Stream<File>::new` returns, the async download task races; wait on
/// the event bus for a terminal `DownloadError` (or `DownloadComplete`)
/// before inspecting the cache. `rx` must be subscribed BEFORE the
/// stream starts downloading — `RequestFailed` is fanned out
/// synchronously in the validator-reject path and a late subscriber
/// would race the publish.
async fn wait_for_download_terminal(
    rx: &mut kithara::events::EventReceiver,
    within: Duration,
) -> bool {
    let deadline = Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let recv = timeout(remaining, rx.recv());
        match recv.await {
            Ok(Ok(env)) => match env.event {
                Event::Downloader(DownloaderEvent::RequestFailed { .. }) => return true,
                Event::Downloader(DownloaderEvent::RequestCompleted { .. }) => return true,
                Event::File(FileEvent::Error { .. }) => return true,
                _ => {}
            },
            Ok(Err(_)) | Err(_) => return false,
        }
    }
}

/// Stream stays alive throughout the test. The captive-portal html response
/// must not leave a 64 KB orphan mmap parked in the cache directory.
///
/// Without the fix this test is RED: `FileInner.res` still holds a clone of
/// the pre-allocated `AssetResource`, so `LeaseResource::Drop` doesn't fire
/// until `Stream<File>` itself is dropped at app shutdown.
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn remote_file_html_response_does_not_leak_cache_file_while_stream_alive(
    temp_dir: TestTempDir,
) {
    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::HtmlError(CAPTIVE_PORTAL_HTML),
        delivery: Delivery::Normal,
    });

    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();
    let cancel = CancelToken::never();
    let config = FileConfig::for_src(handle.url().into())
        .events(bus.clone())
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel.clone())
        .build();

    let stream = Stream::<File>::new(config).await.unwrap();

    let saw_terminal = wait_for_download_terminal(&mut rx, Duration::from_secs(5)).await;
    assert!(
        saw_terminal,
        "expected DownloadError on html response within 5 s",
    );

    // The failure-path cleanup (LeaseResource::Drop → remove_resource) runs
    // synchronously on the download-failure path, before the terminal
    // DownloadError event the wait above already observed — so the cache is
    // drained by now. Assert directly: the terminal event IS the cleanup-done
    // signal; no extra wait (and no busy-poll).
    let leftover = collect_cache_files(temp_dir.path());
    assert!(
        leftover.is_empty(),
        "captive-portal html response left orphan cache file(s) while Stream<File> \
         was still live: {leftover:?}\n\
         expected: no files under the cache root (Drop-style cleanup must happen \
         eagerly on download failure, not at Stream-drop time)",
    );

    drop(stream);
}

/// A captive-portal endpoint must not trigger a retry storm on the
/// Downloader. `run_full_download` is a single attempt; asserting this
/// explicitly locks the invariant against future accidental retry loops.
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn remote_file_html_response_does_not_retry_storm(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let handle = helper.register_behavior(FixtureBehavior {
        content: Content::HtmlError(CAPTIVE_PORTAL_HTML),
        delivery: Delivery::Normal,
    });

    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();
    let cancel = CancelToken::never();
    let config = FileConfig::for_src(handle.url().into())
        .events(bus.clone())
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel.clone())
        .build();

    let stream = Stream::<File>::new(config).await.unwrap();
    let _ = wait_for_download_terminal(&mut rx, Duration::from_secs(5)).await;

    let baseline = handle.request_count();
    // A retry storm surfaces as a NEW `RequestStarted` after the terminal
    // failure. Wait for one rather than sleeping a fixed window: under flash the
    // retry backoff collapses, so a real storm fires within the (virtual)
    // timeout; its absence (timeout elapses with no `RequestStarted`) proves the
    // failed resource scheduled no retry.
    let retried = time::timeout(Duration::from_secs(3), async {
        loop {
            match rx.recv().await.map(|env| env.event) {
                Ok(Event::Downloader(DownloaderEvent::RequestStarted { .. })) => break true,
                Ok(_) => {}
                Err(_) => break false,
            }
        }
    })
    .await
    .unwrap_or(false);
    let after = handle.request_count();

    assert!(
        !retried && after - baseline <= 1,
        "retry storm detected while Stream<File> held a failed resource: \
         {baseline} → {after} hits (retried={retried})",
    );

    drop(stream);
}
