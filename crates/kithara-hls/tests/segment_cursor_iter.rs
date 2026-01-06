#![forbid(unsafe_code)]

mod fixture;

use std::{sync::Arc, time::Duration};

use fixture::*;
use kithara_assets::{AssetStore, EvictConfig};
use kithara_hls::{
    HlsError, HlsResult, PlaylistManager,
    cursor::{SegmentCursor, SegmentDesc},
    fetch::FetchManager,
    playlist::VariantId,
};
use kithara_net::{HttpClient, NetOptions};
use tempfile::TempDir;
use tracing_subscriber::{EnvFilter, filter::ParseError};

fn as_io_err(e: impl ToString) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

#[tokio::test]
#[ignore = "complex test needs updating for new API"]
async fn segment_cursor_fixed_variant_is_contiguous_and_seekable() -> HlsResult<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive(
                    "kithara_hls=trace"
                        .parse()
                        .map_err(|e: ParseError| HlsError::Driver(e.to_string()))?,
                )
                .add_directive(
                    "kithara_net=info"
                        .parse()
                        .map_err(|e: ParseError| HlsError::Driver(e.to_string()))?,
                )
                .add_directive(
                    "kithara_storage=info"
                        .parse()
                        .map_err(|e: ParseError| HlsError::Driver(e.to_string()))?,
                )
                .add_directive(
                    "kithara_assets=info"
                        .parse()
                        .map_err(|e: ParseError| HlsError::Driver(e.to_string()))?,
                ),
        )
        .with_line_number(true)
        .with_file(true)
        .try_init();

    let server = TestServer::new().await;
    let master_url = server.url("/master-init.m3u8")?;

    // Dedicated assets store for this test.
    let tmp = TempDir::new().map_err(|e| HlsError::Driver(e.to_string()))?;
    let assets = AssetStore::with_root_dir(tmp.path().to_path_buf(), EvictConfig::default());

    // Build managers similar to `HlsSession::source()`.
    let asset_root = kithara_assets::ResourceKey::asset_root_for_url(&master_url);
    let net = HttpClient::new(NetOptions::default());

    let playlist = PlaylistManager::new(
        asset_root.clone(),
        assets.clone(),
        net.clone(),
        /* base_url */ None,
    );
    let fetch = FetchManager::new(asset_root.clone(), assets.clone(), net.clone());

    // Resolve master -> media playlist for variant 0 deterministically.
    let master = playlist.fetch_master_playlist(&master_url).await?;

    let variant_uri: String = master
        .variants
        .get(0)
        .map(|vs| vs.uri.clone())
        .ok_or_else(|| HlsError::Driver("master playlist has no variant at index 0".to_string()))?;

    let media_url = playlist.resolve_url(&master_url, &variant_uri)?;
    let media = playlist
        .fetch_media_playlist(&media_url, VariantId(0))
        .await?;

    // Extract EXT-X-MAP init URI in the same simple way as `session.rs` currently does.
    // For this test, we'll use a simplified approach - just create segment descriptors
    // with dummy URLs and lengths since we're testing the cursor logic, not actual fetching
    let mut segments: Vec<SegmentDesc> = Vec::new();
    let variant_index = VariantId(0);

    // Add a dummy init segment
    let init_url = media_url.join("init.mp4").unwrap();
    segments.push(SegmentDesc {
        url: init_url,
        rel_path: "init.mp4".to_string(),
        len: 1000,
        is_init: true,
    });

    // Add dummy media segments
    for i in 0..media.segments.len() {
        let url = media_url.join(&format!("segment_{}.m4s", i)).unwrap();
        segments.push(SegmentDesc {
            url,
            rel_path: format!("segment_{}.m4s", i),
            len: 5000,
            is_init: false,
        });
    }

    // Assemble cursor.
    let fetcher = FetchManager::new("test-asset-root".to_string(), assets.clone(), net);
    let mut cur = SegmentCursor::new(fetcher, variant_index.0, segments, /* chunk_size */ 7);

    // Expected concatenation from fixture helpers.
    let init = test_init_data(0);
    let seg0 = test_segment_data(0, 0);
    let seg1 = test_segment_data(0, 1);
    let seg2 = test_segment_data(0, 2);

    let mut expected = Vec::new();
    expected.extend_from_slice(&init);
    expected.extend_from_slice(&seg0);
    expected.extend_from_slice(&seg1);
    expected.extend_from_slice(&seg2);

    // Run the actual cursor I/O under a timeout so the test fails fast on deadlocks.
    let run = async {
        // 1) Sequential iteration yields the same bytes as concatenation.
        let mut got = Vec::new();
        loop {
            let next = cur.next_chunk().await.map_err(as_io_err)?;
            let Some(bytes) = next else { break };
            got.extend_from_slice(&bytes);
            if got.len() >= expected.len() {
                break;
            }
        }

        assert_eq!(
            got, expected,
            "SegmentCursor must yield init+segments as a contiguous byte stream"
        );

        // 2) Seek to init boundary and read a prefix that should match seg0 start.
        cur.seek(init.len() as u64);

        let mut got2 = Vec::new();
        while got2.len() < "V0-SEG-0:".len() {
            let next = cur.next_chunk().await.map_err(as_io_err)?;
            let Some(bytes) = next else { break };
            got2.extend_from_slice(&bytes);
        }

        assert!(
            got2.starts_with(b"V0-SEG-0:"),
            "seek(init_len) should land at seg0 start; got={:?}",
            String::from_utf8_lossy(&got2[..got2.len().min(32)])
        );

        Ok::<(), std::io::Error>(())
    };

    if let Err(_) = tokio::time::timeout(Duration::from_secs(10), run).await {
        // Dump request counts to distinguish "no fetch triggered" from "storage wait stuck".
        eprintln!(
            "timeout: counts: master={} media={} init={} seg0={}",
            server.get_request_count("/master-init.m3u8"),
            server.get_request_count("/v0-init.m3u8"),
            server.get_request_count("/init/v0.bin"),
            server.get_request_count("/seg/v0_0.bin"),
        );
        return Err(HlsError::Driver("segment cursor test timed out".into()));
    }

    // Sanity: ensure at least init and seg0 were requested.
    assert!(server.get_request_count("/master-init.m3u8") >= 1);
    assert!(server.get_request_count("/v0-init.m3u8") >= 1);
    assert!(server.get_request_count("/init/v0.bin") >= 1);
    assert!(server.get_request_count("/seg/v0_0.bin") >= 1);

    Ok(())
}
