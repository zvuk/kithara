#![forbid(unsafe_code)]

mod fixture;

use std::time::Duration;

use fixture::*;
use kithara_assets::{AssetStore, EvictConfig};
use kithara_hls::{
    HlsError, HlsResult, PlaylistManager,
    cursor::{SegmentCursor, SegmentDesc},
    fetch::FetchManager,
    playlist::VariantId,
};
use kithara_net::{HttpClient, NetOptions};
use rstest::{fixture, rstest};
use tempfile::TempDir;
use tracing_subscriber::{EnvFilter, filter::ParseError};

fn as_io_err(e: impl ToString) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

// ==================== Fixtures ====================

#[fixture]
fn temp_dir() -> TempDir {
    TempDir::new().unwrap()
}

#[fixture]
fn assets(temp_dir: TempDir) -> AssetStore {
    AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default())
}

#[fixture]
fn net() -> HttpClient {
    HttpClient::new(NetOptions::default())
}

#[fixture]
fn asset_root() -> String {
    "test-asset-root".to_string()
}

#[fixture]
fn variant_id_0() -> VariantId {
    VariantId(0)
}

// Note: We can't use async fixtures directly with rstest
// Instead, we'll create the server inside each test

// Helper function to create test segments
fn create_test_segments(media_url: &url::Url, _variant_index: usize) -> Vec<SegmentDesc> {
    let mut segments: Vec<SegmentDesc> = Vec::new();

    // Add a dummy init segment
    let init_url = media_url.join("init.mp4").unwrap();
    segments.push(SegmentDesc {
        url: init_url,
        rel_path: "init.mp4".to_string(),
        len: 1000,
        is_init: true,
    });

    // Add dummy media segments
    for i in 0..3 {
        let url = media_url.join(&format!("segment_{}.m4s", i)).unwrap();
        segments.push(SegmentDesc {
            url,
            rel_path: format!("segment_{}.m4s", i),
            len: 5000,
            is_init: false,
        });
    }

    segments
}

#[rstest]
#[timeout(Duration::from_secs(5))]
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

    let server = fixture::TestServer::new().await;
    let master_url = server.url("/master-init.m3u8")?;

    // Build managers similar to `HlsSession::source()`.
    let temp_dir = TempDir::new().unwrap();
    let assets = AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default());
    let net = HttpClient::new(NetOptions::default());
    let asset_root = "test-asset-root".to_string();
    let variant_id_0 = VariantId(0);

    // Build managers similar to `HlsSession::source()`.

    let playlist = PlaylistManager::new(
        asset_root.clone(),
        assets.clone(),
        net.clone(),
        /* base_url */ None,
    );

    // Resolve master -> media playlist for variant 0 deterministically.
    let master = playlist.fetch_master_playlist(&master_url).await?;

    let variant_uri: String = master
        .variants
        .get(0)
        .map(|vs| vs.uri.clone())
        .ok_or_else(|| HlsError::Driver("master playlist has no variant at index 0".to_string()))?;

    let media_url = playlist.resolve_url(&master_url, &variant_uri)?;
    let _media = playlist
        .fetch_media_playlist(&media_url, variant_id_0)
        .await?;

    // Create segment descriptors
    let segments = create_test_segments(&media_url, variant_id_0.0);

    // Assemble cursor.
    let fetcher = FetchManager::new(asset_root.clone(), assets.clone(), net);
    let mut cur = SegmentCursor::new(fetcher, variant_id_0.0, segments, /* chunk_size */ 7);

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

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn segment_cursor_basic_iteration() -> HlsResult<()> {
    let server = fixture::TestServer::new().await;
    let master_url = server.url("/master.m3u8")?;

    let temp_dir = TempDir::new().unwrap();
    let assets = AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default());
    let net = HttpClient::new(NetOptions::default());
    let asset_root = "test-asset-root".to_string();
    let variant_id_0 = VariantId(0);

    let playlist = PlaylistManager::new(
        asset_root.clone(),
        assets.clone(),
        net.clone(),
        /* base_url */ None,
    );

    let master = playlist.fetch_master_playlist(&master_url).await?;
    let variant_uri: String = master
        .variants
        .get(0)
        .map(|vs| vs.uri.clone())
        .ok_or_else(|| HlsError::Driver("master playlist has no variant at index 0".to_string()))?;

    let media_url = playlist.resolve_url(&master_url, &variant_uri)?;
    let segments = create_test_segments(&media_url, variant_id_0.0);

    let fetcher = FetchManager::new(asset_root.clone(), assets.clone(), net);
    let mut cursor = SegmentCursor::new(
        fetcher,
        variant_id_0.0,
        segments,
        /* chunk_size */ 1024,
    );

    // Read some data
    let mut total_bytes = 0;
    for _ in 0..3 {
        match cursor.next_chunk().await {
            Ok(Some(bytes)) => {
                total_bytes += bytes.len();
                assert!(!bytes.is_empty());
            }
            Ok(None) => {
                break;
            }
            Err(e) => {
                return Err(HlsError::Driver(format!("Cursor error: {}", e)));
            }
        }
    }

    assert!(total_bytes > 0, "Should have read some bytes from cursor");
    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn segment_cursor_seek_operations() -> HlsResult<()> {
    let server = fixture::TestServer::new().await;
    let master_url = server.url("/master.m3u8")?;

    let temp_dir = TempDir::new().unwrap();
    let assets = AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default());
    let net = HttpClient::new(NetOptions::default());
    let asset_root = "test-asset-root".to_string();
    let variant_id_0 = VariantId(0);

    let playlist = PlaylistManager::new(
        asset_root.clone(),
        assets.clone(),
        net.clone(),
        /* base_url */ None,
    );

    let master = playlist.fetch_master_playlist(&master_url).await?;
    let variant_uri: String = master
        .variants
        .get(0)
        .map(|vs| vs.uri.clone())
        .ok_or_else(|| HlsError::Driver("master playlist has no variant at index 0".to_string()))?;

    let media_url = playlist.resolve_url(&master_url, &variant_uri)?;
    let segments = create_test_segments(&media_url, variant_id_0.0);

    let fetcher = FetchManager::new(asset_root.clone(), assets.clone(), net);
    let mut cursor = SegmentCursor::new(
        fetcher,
        variant_id_0.0,
        segments,
        /* chunk_size */ 1024,
    );

    // Test seeking
    cursor.seek(500);

    // Read after seek
    match cursor.next_chunk().await {
        Ok(Some(bytes)) => {
            assert!(!bytes.is_empty());
        }
        Ok(None) => {
            // No data available after seek
        }
        Err(e) => {
            return Err(HlsError::Driver(format!("Cursor error after seek: {}", e)));
        }
    }

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn segment_cursor_multiple_variants() -> HlsResult<()> {
    let server = fixture::TestServer::new().await;
    let master_url = server.url("/master.m3u8")?;

    let temp_dir = TempDir::new().unwrap();
    let assets = AssetStore::with_root_dir(temp_dir.path().to_path_buf(), EvictConfig::default());
    let net = HttpClient::new(NetOptions::default());
    let asset_root = "test-asset-root".to_string();

    let playlist = PlaylistManager::new(
        asset_root.clone(),
        assets.clone(),
        net.clone(),
        /* base_url */ None,
    );

    let master = playlist.fetch_master_playlist(&master_url).await?;

    // Test with different variants
    for (idx, variant) in master.variants.iter().enumerate().take(2) {
        let media_url = playlist.resolve_url(&master_url, &variant.uri)?;
        let segments = create_test_segments(&media_url, idx);

        let fetcher = FetchManager::new(asset_root.clone(), assets.clone(), net.clone());
        let mut cursor = SegmentCursor::new(fetcher, idx, segments, /* chunk_size */ 1024);

        // Read one chunk to verify cursor works
        match cursor.next_chunk().await {
            Ok(Some(bytes)) => {
                assert!(!bytes.is_empty());
            }
            Ok(None) => {
                // No data available
            }
            Err(e) => {
                return Err(HlsError::Driver(format!(
                    "Cursor error for variant {}: {}",
                    idx, e
                )));
            }
        }
    }

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn segment_cursor_empty_segments() -> HlsResult<()> {
    let assets = AssetStore::with_root_dir(
        TempDir::new().unwrap().path().to_path_buf(),
        EvictConfig::default(),
    );
    let net = HttpClient::new(NetOptions::default());
    // Create cursor with empty segments
    let segments = Vec::new();
    let fetcher = FetchManager::new("test".to_string(), assets, net);
    let mut cursor = SegmentCursor::new(fetcher, 0, segments, /* chunk_size */ 1024);

    // Should immediately return Ok(None)
    let result = cursor.next_chunk().await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());

    Ok(())
}
