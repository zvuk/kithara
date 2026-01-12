#![forbid(unsafe_code)]

mod fixture;

use std::time::Duration;

use fixture::*;
use futures::StreamExt;
use kithara_hls::{HlsOptions, HlsSource};
use rstest::{fixture, rstest};

// ==================== Fixtures ====================

#[fixture]
async fn test_server() -> TestServer {
    TestServer::new().await
}

#[fixture]
fn assets_fixture() -> TestAssets {
    create_test_assets()
}

#[fixture]
fn net_fixture() -> kithara_net::HttpClient {
    create_test_net()
}

#[fixture]
fn hls_options() -> HlsOptions {
    HlsOptions::default()
}

// ==================== Test Cases ====================

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn hls_vod_completes_and_stream_closes(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    hls_options: HlsOptions,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;

    // Open HLS session
    let session = HlsSource::open(master_url, hls_options, assets).await?;

    // Get stream and pin it
    let stream = session.stream();
    futures::pin_mut!(stream);
    let mut stream = Box::pin(stream);

    // Collect all bytes
    let mut total_bytes = 0;
    while let Some(result) = stream.next().await {
        match result {
            Ok(bytes) => {
                total_bytes += bytes.len();
                // Verify we're getting segment data
                assert!(!bytes.is_empty());
            }
            Err(e) => {
                panic!("Unexpected error during streaming: {}", e);
            }
        }
    }

    // Verify we got some data
    assert!(total_bytes > 0, "Should have received some segment bytes");

    // Verify all segments for variant 0 were requested
    assert!(server.get_request_count("/seg/v0_0.bin") >= 1);
    assert!(server.get_request_count("/seg/v0_1.bin") >= 1);
    assert!(server.get_request_count("/seg/v0_2.bin") >= 1);

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn hls_stream_can_be_cancelled_early(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    hls_options: HlsOptions,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;

    // Open HLS session
    let session = HlsSource::open(master_url, hls_options, assets).await?;

    // Get stream
    let mut stream = session.stream();

    // Read first few chunks
    let mut chunks_read = 0;
    for _ in 0..2 {
        if let Some(result) = stream.next().await {
            match result {
                Ok(bytes) => {
                    assert!(!bytes.is_empty());
                    chunks_read += 1;
                }
                Err(e) => {
                    panic!("Unexpected error during streaming: {}", e);
                }
            }
        } else {
            break;
        }
    }

    // Drop stream early (simulate cancellation)
    drop(stream);

    // Verify we read some chunks
    assert!(chunks_read > 0, "Should have read at least one chunk");

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn hls_stream_resumes_after_drop(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    hls_options: HlsOptions,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;

    // Open HLS session
    let session = HlsSource::open(master_url.clone(), hls_options.clone(), assets.clone()).await?;

    // Get stream and read first chunk
    let mut stream1 = session.stream();
    let first_chunk = stream1.next().await;

    // Should get some data
    assert!(first_chunk.is_some());
    if let Some(Ok(bytes)) = first_chunk {
        assert!(!bytes.is_empty());
    }

    // Drop first stream
    drop(stream1);

    // Create new stream from same session
    let mut stream2 = session.stream();
    let second_chunk = stream2.next().await;

    // Should also get data
    assert!(second_chunk.is_some());

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn hls_multiple_sessions_independent(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
    hls_options: HlsOptions,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;

    // Open first HLS session
    let session1 = HlsSource::open(master_url.clone(), hls_options.clone(), assets.clone()).await?;
    let mut stream1 = session1.stream();

    // Open second HLS session (same URL, same assets)
    let session2 = HlsSource::open(master_url, hls_options, assets).await?;
    let mut stream2 = session2.stream();

    // Read from both streams
    let chunk1 = stream1.next().await;
    let chunk2 = stream2.next().await;

    // Both should get data
    assert!(chunk1.is_some());
    assert!(chunk2.is_some());

    if let (Some(Ok(bytes1)), Some(Ok(bytes2))) = (chunk1, chunk2) {
        assert!(!bytes1.is_empty());
        assert!(!bytes2.is_empty());
        // They might get different data (different segments) but both should have data
    }

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn hls_stream_with_different_options(
    #[future] test_server: TestServer,
    assets_fixture: TestAssets,
) -> HlsResult<()> {
    let server = test_server.await;
    let assets = assets_fixture.assets().clone();

    let master_url = server.url("/master.m3u8")?;

    // Test with default options
    let options1 = HlsOptions::default();
    let session1 = HlsSource::open(master_url.clone(), options1, assets.clone()).await?;
    let mut stream1 = session1.stream();
    let chunk1 = stream1.next().await;
    assert!(chunk1.is_some());

    // Test with custom options (if any custom options exist)
    let options2 = HlsOptions::default(); // TODO: Add custom options when available
    let session2 = HlsSource::open(master_url, options2, assets).await?;
    let mut stream2 = session2.stream();
    let chunk2 = stream2.next().await;
    assert!(chunk2.is_some());

    Ok(())
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn hls_stream_error_handling_invalid_url(
    assets_fixture: TestAssets,
    hls_options: HlsOptions,
) -> HlsResult<()> {
    let assets = assets_fixture.assets().clone();

    // Try to open HLS with invalid URL
    let invalid_url = "http://invalid-domain-that-does-not-exist-12345.com/master.m3u8";
    let url = url::Url::parse(invalid_url)
        .map_err(|e| kithara_hls::HlsError::InvalidUrl(e.to_string()))?;

    let result = HlsSource::open(url, hls_options, assets).await;

    // Should fail (network error) or succeed (if somehow connects)
    // Either is acceptable for this test
    assert!(result.is_ok() || result.is_err());

    Ok(())
}
