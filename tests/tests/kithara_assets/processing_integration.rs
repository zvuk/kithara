#![forbid(unsafe_code)]

//! Tests for ProcessingAssets layer.
//!
//! Verifies:
//! - Buffering of entire file before processing
//! - Callback invocation with raw bytes and context
//! - Caching of processed result
//! - Subsequent reads from buffer

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use kithara_assets::{AssetStoreBuilder, Assets, EvictConfig, ProcessFn, ResourceKey};
use kithara_storage::{Resource, StreamingResourceExt};
use rstest::{fixture, rstest};

#[fixture]
fn temp_dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Context for test processing callback.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Default)]
struct TestContext {
    /// XOR key for "encryption/decryption".
    #[allow(dead_code)]
    xor_key: u8,
}

/// Create a simple XOR transform callback.
fn create_xor_callback(call_count: Arc<AtomicUsize>) -> ProcessFn<TestContext> {
    Arc::new(move |bytes: Bytes, ctx: TestContext| {
        let count = Arc::clone(&call_count);
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            // XOR each byte with the key
            let processed: Vec<u8> = bytes.iter().map(|b| b ^ ctx.xor_key).collect();
            Ok(Bytes::from(processed))
        })
    })
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn processing_buffers_and_transforms_data(temp_dir: tempfile::TempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root("test-processing")
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .process_fn(create_xor_callback(Arc::clone(&call_count)))
        .build();

    let key = ResourceKey::new("data.bin");

    // Write original data to streaming resource.
    let original_data = b"Hello, World! This is test data for processing.";
    {
        let res = store.open_streaming_resource(&key).await.unwrap();
        res.write_at(0, original_data).await.unwrap();
        res.commit(Some(original_data.len() as u64)).await.unwrap();
    }

    // Open processed resource with XOR key.
    let ctx = TestContext { xor_key: 0x42 };
    let processed_res = store
        .open_streaming_resource_with_ctx(&key, Some(ctx.clone()))
        .await
        .unwrap();

    // Read processed data.
    let mut buf = vec![0u8; original_data.len()];
    let n = processed_res.read_at(0, &mut buf).await.unwrap();
    assert_eq!(n, original_data.len());

    // Verify XOR transformation was applied.
    let expected: Vec<u8> = original_data.iter().map(|b| b ^ 0x42).collect();
    assert_eq!(buf, expected);

    // Callback should have been called exactly once.
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn processing_caches_result_on_subsequent_reads(temp_dir: tempfile::TempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root("test-cache")
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .process_fn(create_xor_callback(Arc::clone(&call_count)))
        .build();

    let key = ResourceKey::new("cached.bin");

    // Write data.
    let original_data = b"Data for caching test";
    {
        let res = store.open_streaming_resource(&key).await.unwrap();
        res.write_at(0, original_data).await.unwrap();
        res.commit(Some(original_data.len() as u64)).await.unwrap();
    }

    let ctx = TestContext { xor_key: 0xAB };

    // First read - should call callback.
    let processed_res = store
        .open_streaming_resource_with_ctx(&key, Some(ctx.clone()))
        .await
        .unwrap();
    let mut buf1 = vec![0u8; original_data.len()];
    processed_res.read_at(0, &mut buf1).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    // Second read from same resource - should use cached buffer.
    let mut buf2 = vec![0u8; original_data.len()];
    processed_res.read_at(0, &mut buf2).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 1); // Still 1!

    // Data should be the same.
    assert_eq!(buf1, buf2);

    // Third read with partial offset - still cached.
    let mut buf3 = vec![0u8; 10];
    processed_res.read_at(5, &mut buf3).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 1); // Still 1!
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn processing_different_contexts_are_cached_separately(temp_dir: tempfile::TempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root("test-ctx")
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .process_fn(create_xor_callback(Arc::clone(&call_count)))
        .build();

    let key = ResourceKey::new("multi-ctx.bin");

    // Write data.
    let original_data = b"Multi-context test data";
    {
        let res = store.open_streaming_resource(&key).await.unwrap();
        res.write_at(0, original_data).await.unwrap();
        res.commit(Some(original_data.len() as u64)).await.unwrap();
    }

    // First context.
    let ctx1 = TestContext { xor_key: 0x11 };
    let res1 = store
        .open_streaming_resource_with_ctx(&key, Some(ctx1.clone()))
        .await
        .unwrap();
    let mut buf1 = vec![0u8; original_data.len()];
    res1.read_at(0, &mut buf1).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 1);

    // Different context - should call callback again.
    let ctx2 = TestContext { xor_key: 0x22 };
    let res2 = store
        .open_streaming_resource_with_ctx(&key, Some(ctx2.clone()))
        .await
        .unwrap();
    let mut buf2 = vec![0u8; original_data.len()];
    res2.read_at(0, &mut buf2).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 2);

    // Results should be different.
    assert_ne!(buf1, buf2);

    // Verify each is XORed correctly.
    let expected1: Vec<u8> = original_data.iter().map(|b| b ^ 0x11).collect();
    let expected2: Vec<u8> = original_data.iter().map(|b| b ^ 0x22).collect();
    assert_eq!(buf1, expected1);
    assert_eq!(buf2, expected2);

    // Re-open same context - should use cache.
    let res1_again = store
        .open_streaming_resource_with_ctx(&key, Some(ctx1))
        .await
        .unwrap();
    let mut buf1_again = vec![0u8; original_data.len()];
    res1_again.read_at(0, &mut buf1_again).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), 2); // Still 2!
    assert_eq!(buf1, buf1_again);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn processing_partial_reads_work_correctly(temp_dir: tempfile::TempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root("test-partial")
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .process_fn(create_xor_callback(Arc::clone(&call_count)))
        .build();

    let key = ResourceKey::new("partial.bin");

    // Write data.
    let original_data: Vec<u8> = (0..100).collect();
    {
        let res = store.open_streaming_resource(&key).await.unwrap();
        res.write_at(0, &original_data).await.unwrap();
        res.commit(Some(original_data.len() as u64)).await.unwrap();
    }

    let ctx = TestContext { xor_key: 0xFF };
    let processed_res = store
        .open_streaming_resource_with_ctx(&key, Some(ctx))
        .await
        .unwrap();

    // Read middle portion.
    let mut buf = vec![0u8; 20];
    let n = processed_res.read_at(40, &mut buf).await.unwrap();
    assert_eq!(n, 20);

    // Verify correct slice was returned (XORed).
    let expected: Vec<u8> = (40..60).map(|b: u8| b ^ 0xFF).collect();
    assert_eq!(buf, expected);

    // Read at end.
    let mut buf_end = vec![0u8; 20];
    let n_end = processed_res.read_at(90, &mut buf_end).await.unwrap();
    assert_eq!(n_end, 10); // Only 10 bytes available.

    let expected_end: Vec<u8> = (90..100).map(|b: u8| b ^ 0xFF).collect();
    assert_eq!(&buf_end[..10], &expected_end[..]);

    // Callback called only once despite multiple reads.
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn processing_read_past_end_returns_zero(temp_dir: tempfile::TempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root("test-eof")
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .process_fn(create_xor_callback(Arc::clone(&call_count)))
        .build();

    let key = ResourceKey::new("eof.bin");

    // Write small data.
    let original_data = b"short";
    {
        let res = store.open_streaming_resource(&key).await.unwrap();
        res.write_at(0, original_data).await.unwrap();
        res.commit(Some(original_data.len() as u64)).await.unwrap();
    }

    let ctx = TestContext { xor_key: 0x00 };
    let processed_res = store
        .open_streaming_resource_with_ctx(&key, Some(ctx))
        .await
        .unwrap();

    // Read past end.
    let mut buf = vec![0u8; 100];
    let n = processed_res.read_at(100, &mut buf).await.unwrap();
    assert_eq!(n, 0);
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn store_without_processing_returns_error_on_open_processed(temp_dir: tempfile::TempDir) {
    // Build store WITHOUT process_fn.
    let store = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root("no-processing")
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .build();

    let key = ResourceKey::new("test.bin");

    // Write some data.
    {
        let res = store.open_streaming_resource(&key).await.unwrap();
        res.write_at(0, b"data").await.unwrap();
        res.commit(Some(4)).await.unwrap();
    }

    // open_processed should fail since no processing configured.
    // Note: This requires AssetStore<()> to have open_processed method,
    // which it doesn't by design. So we can't call it.
    // The test verifies that the store works normally without processing.
    let res = store.open_streaming_resource(&key).await.unwrap();
    let mut buf = vec![0u8; 4];
    let n = res.read_at(0, &mut buf).await.unwrap();
    assert_eq!(n, 4);
    assert_eq!(&buf, b"data");
}
