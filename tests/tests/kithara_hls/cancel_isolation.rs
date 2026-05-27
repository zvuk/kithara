#![cfg(not(target_arch = "wasm32"))]
#![forbid(unsafe_code)]

use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

use kithara::{
    assets::StoreOptions,
    hls::{Hls, HlsConfig},
    stream::Stream,
};
use kithara_integration_tests::{
    BehaviorHandle, Content, Delivery, FixtureBehavior, TestServerHelper, TestTempDir, temp_dir,
};
use kithara_platform::time::{Duration, Instant};
use tokio::task;
use tokio_util::sync::CancellationToken;

struct Consts;
impl Consts {
    const NUM_SEGMENTS: usize = 6;
    const HTML_SEGMENT_INDEX: usize = 3;
    const SEGMENT_SIZE: usize = 64 * 1024;
}

/// One html-bodied segment (segment 3) must NOT prevent the Downloader from
/// fetching the surrounding segments (0..2 and 4..5). Per the audit: the
/// validator on `download_atomic_bytes` is only applied to playlists / DRM
/// keys, not to media segments. Even if one segment writes html bytes into
/// its `AssetResource`, sibling fetches in the same `HlsPeer::poll_next`
/// batch share `peer_cancel + epoch_cancel` and neither token fires from
/// `on_complete` failure paths (`crates/kithara-hls/src/peer.rs:624..676`).
///
/// Failure mode the test would reveal: if a future change wires the
/// segment writer / `on_complete` failure into `epoch_cancel.cancel()` or
/// into a wider abort, sibling segments would never be requested. That
/// would manifest as `segments[i].request_count() == 0` for some `i != 3`.
#[kithara::test(tokio, timeout(Duration::from_secs(15)))]
async fn html_segment_does_not_cancel_sibling_fetches(temp_dir: TestTempDir) {
    let helper = TestServerHelper::new().await;

    // 6 segment behaviors: index 3 = HTML, others = 64KB of 0xAB.
    let segments: Vec<BehaviorHandle> = (0..Consts::NUM_SEGMENTS)
        .map(|i| {
            if i == Consts::HTML_SEGMENT_INDEX {
                helper.register_behavior(FixtureBehavior {
                    content: Content::HtmlError("<html><body>503 Backend Error</body></html>"),
                    delivery: Delivery::Normal,
                })
            } else {
                helper.register_behavior(FixtureBehavior {
                    content: Content::StaticBytes {
                        bytes: Arc::new(vec![0xABu8; Consts::SEGMENT_SIZE]),
                        content_type: Some("video/mp2t"),
                    },
                    delivery: Delivery::Range,
                })
            }
        })
        .collect();

    // media playlist references each segment by absolute url.
    let mut media_body = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n",
    );
    for seg in &segments {
        media_body.push_str(&format!("#EXTINF:4.0,\n{}\n", seg.url()));
    }
    media_body.push_str("#EXT-X-ENDLIST\n");
    let media = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(media_body.into_bytes()),
            content_type: Some("application/vnd.apple.mpegurl"),
        },
        delivery: Delivery::Normal,
    });

    // master references media by absolute url.
    let master_body = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:BANDWIDTH=128000\n{}\n",
        media.url()
    );
    let master = helper.register_behavior(FixtureBehavior {
        content: Content::StaticBytes {
            bytes: Arc::new(master_body.into_bytes()),
            content_type: Some("application/vnd.apple.mpegurl"),
        },
        delivery: Delivery::Normal,
    });

    let cancel = CancellationToken::new();
    let store = StoreOptions::new(temp_dir.path());
    let config = HlsConfig::for_url(master.url())
        .store(store)
        .cancel(cancel.clone())
        .build();

    let stream = Stream::<Hls>::new(config)
        .await
        .expect("HLS stream creation must succeed with valid playlists");

    let mut stream = stream;
    let segments_for_blocking = segments.clone();
    let _ = task::spawn_blocking(move || {
        let mut buf = vec![0u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(8);
        for (seg, handle) in segments_for_blocking.iter().enumerate() {
            if seg == Consts::HTML_SEGMENT_INDEX {
                continue;
            }
            let offset = (seg * Consts::SEGMENT_SIZE) as u64;
            let _ = stream.seek(SeekFrom::Start(offset));
            let _ = stream.read(&mut buf);
            while handle.request_count() == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    })
    .await;

    cancel.cancel();

    assert!(
        master.request_count() >= 1,
        "master playlist must have been fetched at least once",
    );
    assert!(
        media.request_count() >= 1,
        "media playlist must have been fetched at least once",
    );

    let snapshot: Vec<u64> = segments.iter().map(BehaviorHandle::request_count).collect();

    let mut missing: Vec<usize> = Vec::new();
    for (idx, hits) in snapshot.iter().enumerate() {
        if idx == Consts::HTML_SEGMENT_INDEX {
            continue;
        }
        if *hits == 0 {
            missing.push(idx);
        }
    }

    assert!(
        missing.is_empty(),
        "sibling segment fetch cancelled when one sibling returned html: \
         missing segments {missing:?}, full hit map {snapshot:?} \
         (html segment is index {html_idx})",
        html_idx = Consts::HTML_SEGMENT_INDEX,
    );

    assert!(
        segments[Consts::HTML_SEGMENT_INDEX].request_count() >= 1,
        "rigged html segment was never requested — test fixture broken",
    );
}
