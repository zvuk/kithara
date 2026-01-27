#![forbid(unsafe_code)]

//! Tests for ProcessingAssets layer.
//!
//! Verifies:
//! - Processing on commit (not on read)
//! - Chunk-by-chunk transformation without memory buffering
//! - Caching of processed resources (via CachedAssets)
//! - Reads from disk after processing

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use kithara_assets::{AssetStoreBuilder, Assets, EvictConfig, ProcessChunkFn, ResourceKey};
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
    xor_key: u8,
}

/// Create a simple XOR chunk transform callback (no allocation).
fn create_xor_chunk_callback(call_count: Arc<AtomicUsize>) -> ProcessChunkFn<TestContext> {
    Arc::new(move |input: &[u8], output: &mut [u8], ctx: &TestContext, _is_last: bool| {
        call_count.fetch_add(1, Ordering::SeqCst);
        // XOR each byte with the key
        for (i, &b) in input.iter().enumerate() {
            output[i] = b ^ ctx.xor_key;
        }
        Ok(input.len())
    })
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn processing_transforms_data_on_commit(temp_dir: tempfile::TempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = AssetStoreBuilder::new()
        .root_dir(temp_dir.path())
        .asset_root("test-processing")
        .evict_config(EvictConfig {
            max_assets: None,
            max_bytes: None,
        })
        .process_fn(create_xor_chunk_callback(Arc::clone(&call_count)))
        .build();

    let key = ResourceKey::new("data.bin");

    // Write original data and commit with context.
    let original_data = b"Hello, World! This is test data for processing.";
    let ctx = TestContext { xor_key: 0x42 };
    {
        let res = store
            .open_streaming_resource_with_ctx(&key, Some(ctx.clone()))
            .await
            .unwrap();
        res.write_at(0, original_data).await.unwrap();

        // Processing happens on commit
        res.commit(Some(original_data.len() as u64)).await.unwrap();
    }

    // Verify callback was called during commit
    assert!(call_count.load(Ordering::SeqCst) > 0);

    // Open again and read processed data
    let processed_res = store
        .open_streaming_resource_with_ctx(&key, Some(ctx))
        .await
        .unwrap();

    let mut buf = vec![0u8; original_data.len()];
    let n = processed_res.read_at(0, &mut buf).await.unwrap();
    assert_eq!(n, original_data.len());

    // Verify XOR transformation was applied.
    let expected: Vec<u8> = original_data.iter().map(|b| b ^ 0x42).collect();
    assert_eq!(buf, expected);
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
        .process_fn(create_xor_chunk_callback(Arc::clone(&call_count)))
        .build();

    let key = ResourceKey::new("cached.bin");
    let ctx = TestContext { xor_key: 0xAB };

    // Write and commit data.
    let original_data = b"Data for caching test";
    {
        let res = store
            .open_streaming_resource_with_ctx(&key, Some(ctx.clone()))
            .await
            .unwrap();
        res.write_at(0, original_data).await.unwrap();
        res.commit(Some(original_data.len() as u64)).await.unwrap();
    }
    let count_after_commit = call_count.load(Ordering::SeqCst);

    // First read - data already processed on disk
    let processed_res = store
        .open_streaming_resource_with_ctx(&key, Some(ctx.clone()))
        .await
        .unwrap();
    let mut buf1 = vec![0u8; original_data.len()];
    processed_res.read_at(0, &mut buf1).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), count_after_commit);

    // Second read from same resource - no additional processing
    let mut buf2 = vec![0u8; original_data.len()];
    processed_res.read_at(0, &mut buf2).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), count_after_commit);

    // Data should be the same.
    assert_eq!(buf1, buf2);

    // Third read with partial offset - still no processing
    let mut buf3 = vec![0u8; 10];
    processed_res.read_at(5, &mut buf3).await.unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), count_after_commit);
}

// Note: Test for "different contexts on same file" was removed.
// In "process on write" design, each file is processed once on commit.
// For HLS, each segment has its own path and key - no context conflicts.
// Different contexts on same file is not a supported use case.

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
        .process_fn(create_xor_chunk_callback(Arc::clone(&call_count)))
        .build();

    let key = ResourceKey::new("partial.bin");
    let ctx = TestContext { xor_key: 0xFF };

    // Write data.
    let original_data: Vec<u8> = (0..100).collect();
    {
        let res = store
            .open_streaming_resource_with_ctx(&key, Some(ctx.clone()))
            .await
            .unwrap();
        res.write_at(0, &original_data).await.unwrap();
        res.commit(Some(original_data.len() as u64)).await.unwrap();
    }

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
        .process_fn(create_xor_chunk_callback(Arc::clone(&call_count)))
        .build();

    let key = ResourceKey::new("eof.bin");
    let ctx = TestContext { xor_key: 0x00 };

    // Write small data.
    let original_data = b"short";
    {
        let res = store
            .open_streaming_resource_with_ctx(&key, Some(ctx.clone()))
            .await
            .unwrap();
        res.write_at(0, original_data).await.unwrap();
        res.commit(Some(original_data.len() as u64)).await.unwrap();
    }

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
async fn store_without_processing_works_normally(temp_dir: tempfile::TempDir) {
    // Build store WITHOUT custom process_fn (uses default pass-through).
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

    // Read - should get original data (pass-through processing)
    let res = store.open_streaming_resource(&key).await.unwrap();
    let mut buf = vec![0u8; 4];
    let n = res.read_at(0, &mut buf).await.unwrap();
    assert_eq!(n, 4);
    assert_eq!(&buf, b"data");
}
