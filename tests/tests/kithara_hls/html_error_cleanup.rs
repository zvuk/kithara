#![forbid(unsafe_code)]

use kithara::{
    assets::StoreOptions,
    events::{DownloaderEvent, Event, EventBus},
    hls::{Hls, HlsConfig},
    platform::{CancelToken, sync::Arc, time::Duration},
    stream::Stream,
};
use kithara_integration_tests::{
    Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir, temp_dir,
};
use url::Url;

/// Walk `root` recursively and collect every file that is not inside the
/// `_index` directory (which holds `pins.bin` / `lru.bin` / `availability.bin`).
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

#[derive(Copy, Clone)]
enum HtmlScenario {
    /// Master playlist itself returns HTML.
    AllHtml,
    /// Master is a valid playlist; the variant-0 media playlist returns HTML.
    PartialHtml,
}

/// Build the HLS master URL for `scenario` on the shared server and return the
/// orphan-cache prefix to assert against (`None` → the whole cache must be
/// empty; `Some(prefix)` → no cache file may start with `prefix`).
fn build_scenario(
    helper: &TestServerHelper,
    scenario: HtmlScenario,
) -> (Url, Option<&'static str>) {
    match scenario {
        HtmlScenario::AllHtml => {
            let master = helper.register_behavior(FixtureBehavior {
                content: Content::HtmlError("<html><body>503 Service Unavailable</body></html>"),
                delivery: Delivery::Normal,
            });
            (master.url(), None)
        }
        HtmlScenario::PartialHtml => {
            let media = helper.register_behavior(FixtureBehavior {
                content: Content::HtmlError("<html><body>503 Backend Error</body></html>"),
                delivery: Delivery::Normal,
            });
            // Absolute URL whose final path segment is `v0.m3u8`, so the
            // failing media fetch keys its cache file by `v0` (see
            // `ResourceKey::from(&Url)`). A relative `v0.m3u8` would resolve
            // against the master's `/behavior/{token}/` and hit the same
            // behavior, so reference the media fixture absolutely instead.
            let media_url = media.child_url("v0.m3u8");
            let master_body = format!(
                "#EXTM3U\n\
                 #EXT-X-VERSION:3\n\
                 #EXT-X-STREAM-INF:BANDWIDTH=128000\n\
                 {media_url}\n"
            );
            let master = helper.register_behavior(FixtureBehavior {
                content: Content::StaticBytes {
                    bytes: Arc::new(master_body.into_bytes()),
                    content_type: Some("application/vnd.apple.mpegurl"),
                },
                delivery: Delivery::Normal,
            });
            (master.url(), Some("v0"))
        }
    }
}

/// After `Stream::new` fails with `text/html` at some layer, no orphan
/// pre-allocated cache file may remain for the failing URL. Current behaviour
/// keys the mmap file by URL — confirmed by both cases:
///
/// * `AllHtml` — master playlist itself is HTML; the whole cache must be empty.
/// * `PartialHtml` — master is valid, media playlist for variant 0 returns HTML;
///   the master may remain cached (it succeeded) but no `v0*` orphan may exist
///   (exercises the `acquire_resource` → `InvalidContent` path at
///   `atomic_fetch.rs:67`).
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
#[case::master_all_html(HtmlScenario::AllHtml, "HTML master playlist must fail Stream::new")]
#[case::media_after_valid_master(
    HtmlScenario::PartialHtml,
    "HTML on the media playlist must fail Stream::new"
)]
async fn html_playlist_failure_leaves_no_orphan_cache_files(
    temp_dir: TestTempDir,
    #[case] scenario: HtmlScenario,
    #[case] fail_msg: &str,
) {
    let helper = TestServerHelper::new().await;
    let (url, orphan_prefix) = build_scenario(&helper, scenario);

    let cancel = CancelToken::never();
    let config = HlsConfig::for_url(url)
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel.clone())
        .build();

    let result = Stream::<Hls>::new(config).await;
    assert!(result.is_err(), "{fail_msg}");

    // The failure-path cleanup (LeaseResource::Drop → remove_resource) runs
    // synchronously before `Stream::new` returns the Err asserted above, so the
    // orphan is already gone — assert directly, no wait/poll (the Err IS the
    // cleanup-done signal).
    let leftover = collect_cache_files(temp_dir.path());
    let suspicious: Vec<std::path::PathBuf> = match orphan_prefix {
        None => leftover.clone(),
        Some(prefix) => leftover
            .iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with(prefix))
            })
            .cloned()
            .collect(),
    };

    assert!(
        suspicious.is_empty(),
        "failed atomic fetch leaked cache file(s): {suspicious:?}\n\
         full cache contents: {leftover:?}",
    );
}

/// A dead HTML endpoint must not produce a retry storm on the shared
/// Downloader. The current behaviour issues a single `Stream::new` attempt
/// which fails immediately; if future code wires auto-retry into the
/// transport, the hit count must still be bounded.
#[kithara::test(tokio, timeout(Duration::from_secs(10)))]
async fn html_master_playlist_does_not_retry_storm(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;
    let master = helper.register_behavior(FixtureBehavior {
        content: Content::HtmlError("<html><body>503 Service Unavailable</body></html>"),
        delivery: Delivery::Normal,
    });

    let bus = EventBus::new(64);
    let cancel = CancelToken::never();
    let config = HlsConfig::for_url(master.url())
        .events(bus.clone())
        .store(StoreOptions::new(temp_dir.path()))
        .cancel(cancel.clone())
        .build();

    let _ = Stream::<Hls>::new(config).await;

    // Subscribe AFTER the initial (failed) attempt so `rx` only carries FUTURE
    // requests — a retry storm. The `request_count` delta is the backstop in
    // case a retry races the subscribe.
    let mut rx = bus.subscribe();
    let start_hits = master.request_count();
    // Wait for a NEW RequestStarted (the storm signal) rather than sleeping a
    // fixed window: under flash the retry backoff collapses so a real storm
    // fires within the virtual timeout; its absence proves no retry was scheduled.
    let retried = time::timeout(Duration::from_secs(3), async {
        loop {
            match rx.recv().await {
                Ok(Event::Downloader(DownloaderEvent::RequestStarted { .. })) => break true,
                Ok(_) => {}
                Err(_) => break false,
            }
        }
    })
    .await
    .unwrap_or(false);
    let end_hits = master.request_count();

    assert!(
        !retried && end_hits - start_hits <= 1,
        "retry storm detected: {start_hits} → {end_hits} (retried={retried})",
    );

    assert!(
        end_hits <= 10,
        "excessive master_hits={end_hits} from a single Stream::new attempt",
    );
}
