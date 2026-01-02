mod fixture;
use fixture::*;
use futures::StreamExt;
use kithara_hls::{HlsError, HlsOptions, HlsSource};
use std::sync::Arc;

#[tokio::test]
async fn hls_vod_completes_and_stream_closes() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (cache, net) = create_test_cache_and_net();

    let master_url = server.url("/master.m3u8")?;
    let options = HlsOptions::default();

    // Open HLS session
    let mut session = HlsSource::open(master_url, options, cache, net).await?;

    // Get stream and pin it
    let stream = session.stream();
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

#[tokio::test]
async fn hls_vod_fetches_all_segments_for_selected_variant() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (cache, net) = create_test_cache_and_net();

    let master_url = server.url("/master.m3u8")?;
    let options = HlsOptions::default();

    // Open HLS session
    let mut session = HlsSource::open(master_url, options, cache, net).await?;

    // Get stream
    let stream = session.stream();
    let mut stream = Box::pin(stream);

    // Collect all bytes
    let total_bytes = 0;
    let segment_prefixes: Vec<String> = Vec::new();
    while let Some(result) = stream.next().await {
        result?;
    }

    // Verify all segments for variant 0 were requested at least once
    assert!(
        server.get_request_count("/seg/v0_0.bin") >= 1,
        "Segment 0 not fetched"
    );
    assert!(
        server.get_request_count("/seg/v0_1.bin") >= 1,
        "Segment 1 not fetched"
    );
    assert!(
        server.get_request_count("/seg/v0_2.bin") >= 1,
        "Segment 2 not fetched"
    );

    Ok(())
}

#[tokio::test]
async fn hls_manual_variant_outputs_only_selected_variant_prefixes() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (cache, net) = create_test_cache_and_net();

    let master_url = server.url("/master.m3u8")?;

    // Test each variant
    for variant in 0..3 {
        let mut options = HlsOptions::default();
        options.variant_stream_selector = Some(Arc::new(move |_| Some(variant)));

        // Open HLS session
        let mut session =
            HlsSource::open(master_url.clone(), options, cache.clone(), net.clone()).await?;

        // Get stream and pin it
        let stream = session.stream();
        let mut stream = Box::pin(stream);

        // Read first chunk to verify variant
        if let Some(Ok(first_bytes)) = stream.next().await {
            let prefix = format!("V{}-SEG-0:", variant);
            let prefix_bytes = prefix.as_bytes();

            // Check if the prefix matches expected variant
            assert!(
                first_bytes.starts_with(prefix_bytes),
                "First bytes should start with prefix {:?}, got: {:?}",
                prefix,
                String::from_utf8_lossy(&first_bytes[..std::cmp::min(first_bytes.len(), 20)])
            );
        } else {
            panic!("Failed to get first bytes for variant {}", variant);
        }

        // Drop stream to cancel
        drop(stream);
    }

    Ok(())
}

#[tokio::test]
async fn hls_drop_cancels_driver_and_stops_requests() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (cache, net) = create_test_cache_and_net();

    let master_url = server.url("/master.m3u8")?;
    let options = HlsOptions::default();

    // Open HLS session
    let mut session = HlsSource::open(master_url, options, cache, net).await?;

    // Get stream and pin it, read only first chunk
    let stream = session.stream();
    let mut stream = Box::pin(stream);

    // Read first chunk
    let first_result = stream.next().await;
    assert!(first_result.is_some(), "Should get first chunk");

    // Get request count before drop
    let count_before = server.get_request_count("/seg/v0_0.bin");

    // Drop the stream (simulating consumer cancellation)
    drop(stream);

    // Give driver time to notice cancellation
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Get request count after drop
    let count_after = server.get_request_count("/seg/v0_0.bin");

    // Request count should not increase significantly after drop
    // (might increase by 1 due to in-flight request)
    assert!(
        count_after <= count_before + 1,
        "Requests should stop after drop. Before: {}, After: {}",
        count_before,
        count_after
    );

    Ok(())
}

#[tokio::test]
#[ignore = "Offline mode not implemented yet"]
async fn hls_offline_miss_is_fatal() -> HlsResult<()> {
    let server = TestServer::new().await;
    let (cache, net) = create_test_cache_and_net();

    let master_url = server.url("/master.m3u8")?;

    // Enable offline mode - cache is empty, so should fail
    let mut options = HlsOptions::default();
    options.offline_mode = true;

    // Open HLS session - should fail with OfflineMiss
    let result = HlsSource::open(master_url, options, cache, net).await;

    // Should get OfflineMiss error
    match result {
        Err(HlsError::OfflineMiss) => Ok(()), // Expected
        Err(e) => panic!("Expected OfflineMiss, got: {}", e),
        Ok(_) => panic!("Should have failed with OfflineMiss"),
    }
}
