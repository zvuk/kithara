#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[cfg(not(target_arch = "wasm32"))]
use kithara::assets::EvictConfig;
use kithara::{
    assets::{AssetStoreBuilder, ProcessChunkFn, ResourceKey},
    storage::ResourceExt,
};
use kithara_integration_tests::temp_dir;
use kithara_platform::time::Duration;

/// Context for test processing callback.
#[derive(Clone, Debug, Hash, Eq, PartialEq, Default)]
struct TestContext {
    /// XOR key for "encryption/decryption".
    xor_key: u8,
}

/// Create a simple XOR chunk transform callback (no allocation).
fn create_xor_chunk_callback(call_count: Arc<AtomicUsize>) -> ProcessChunkFn<TestContext> {
    Arc::new(
        move |input: &[u8], output: &mut [u8], ctx: &mut TestContext, _is_last: bool| {
            call_count.fetch_add(1, Ordering::SeqCst);
            for (i, &b) in input.iter().enumerate() {
                output[i] = b ^ ctx.xor_key;
            }
            Ok(input.len())
        },
    )
}

fn build_test_processing_store(
    temp_dir: &kithara_integration_tests::TestTempDir,
    asset_root: &str,
    process_fn: ProcessChunkFn<TestContext>,
) -> kithara::assets::AssetStore<TestContext> {
    let builder = AssetStoreBuilder::new()
        .asset_root(Some(asset_root))
        .process_fn(process_fn);
    #[cfg(not(target_arch = "wasm32"))]
    {
        builder
            .root_dir(temp_dir.path())
            .evict_config(EvictConfig {
                max_assets: None,
                max_bytes: None,
            })
            .build()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = temp_dir;
        builder.ephemeral(true).build()
    }
}

fn build_test_store_no_processing(
    temp_dir: &kithara_integration_tests::TestTempDir,
    asset_root: &str,
) -> kithara::assets::AssetStore {
    #[cfg(not(target_arch = "wasm32"))]
    {
        AssetStoreBuilder::new()
            .root_dir(temp_dir.path())
            .asset_root(Some(asset_root))
            .evict_config(EvictConfig {
                max_assets: None,
                max_bytes: None,
            })
            .build()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = temp_dir;
        AssetStoreBuilder::new()
            .ephemeral(true)
            .asset_root(Some(asset_root))
            .build()
    }
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn processing_transforms_data_on_commit(temp_dir: kithara_integration_tests::TestTempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = build_test_processing_store(
        &temp_dir,
        "test-processing",
        create_xor_chunk_callback(Arc::clone(&call_count)),
    );

    let key = ResourceKey::new("data.bin");

    let original_data = b"Hello, World! This is test data for processing.";
    let ctx = TestContext { xor_key: 0x42 };
    {
        let res = store
            .acquire_resource_with_ctx(&key, Some(ctx.clone()))
            .unwrap();
        res.write_at(0, original_data).unwrap();

        res.commit(Some(original_data.len() as u64)).unwrap();
    }

    assert!(call_count.load(Ordering::SeqCst) > 0);

    let processed_res = store.open_resource_with_ctx(&key, Some(ctx)).unwrap();

    let mut buf = vec![0u8; original_data.len()];
    let n = processed_res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, original_data.len());

    let expected: Vec<u8> = original_data.iter().map(|b| b ^ 0x42).collect();
    assert_eq!(buf, expected);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn processing_caches_result_on_subsequent_reads(temp_dir: kithara_integration_tests::TestTempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = build_test_processing_store(
        &temp_dir,
        "test-cache",
        create_xor_chunk_callback(Arc::clone(&call_count)),
    );

    let key = ResourceKey::new("cached.bin");
    let ctx = TestContext { xor_key: 0xAB };

    let original_data = b"Data for caching test";
    {
        let res = store
            .acquire_resource_with_ctx(&key, Some(ctx.clone()))
            .unwrap();
        res.write_at(0, original_data).unwrap();
        res.commit(Some(original_data.len() as u64)).unwrap();
    }
    let count_after_commit = call_count.load(Ordering::SeqCst);

    let processed_res = store
        .open_resource_with_ctx(&key, Some(ctx.clone()))
        .unwrap();
    let mut buf1 = vec![0u8; original_data.len()];
    processed_res.read_at(0, &mut buf1).unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), count_after_commit);

    let mut buf2 = vec![0u8; original_data.len()];
    processed_res.read_at(0, &mut buf2).unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), count_after_commit);

    assert_eq!(buf1, buf2);

    let mut buf3 = vec![0u8; 10];
    processed_res.read_at(5, &mut buf3).unwrap();
    assert_eq!(call_count.load(Ordering::SeqCst), count_after_commit);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn processing_partial_reads_work_correctly(temp_dir: kithara_integration_tests::TestTempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = build_test_processing_store(
        &temp_dir,
        "test-partial",
        create_xor_chunk_callback(Arc::clone(&call_count)),
    );

    let key = ResourceKey::new("partial.bin");
    let ctx = TestContext { xor_key: 0xFF };

    let original_data: Vec<u8> = (0..100).collect();
    {
        let res = store
            .acquire_resource_with_ctx(&key, Some(ctx.clone()))
            .unwrap();
        res.write_at(0, &original_data).unwrap();
        res.commit(Some(original_data.len() as u64)).unwrap();
    }

    let processed_res = store.open_resource_with_ctx(&key, Some(ctx)).unwrap();

    let mut buf = vec![0u8; 20];
    let n = processed_res.read_at(40, &mut buf).unwrap();
    assert_eq!(n, 20);

    let expected: Vec<u8> = (40..60).map(|b: u8| b ^ 0xFF).collect();
    assert_eq!(buf, expected);

    let mut buf_end = vec![0u8; 20];
    let n_end = processed_res.read_at(90, &mut buf_end).unwrap();
    assert_eq!(n_end, 10);

    let expected_end: Vec<u8> = (90..100).map(|b: u8| b ^ 0xFF).collect();
    assert_eq!(&buf_end[..10], &expected_end[..]);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn processing_read_past_end_returns_zero(temp_dir: kithara_integration_tests::TestTempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let store = build_test_processing_store(
        &temp_dir,
        "test-eof",
        create_xor_chunk_callback(Arc::clone(&call_count)),
    );

    let key = ResourceKey::new("eof.bin");
    let ctx = TestContext { xor_key: 0x00 };

    let original_data = b"short";
    {
        let res = store
            .acquire_resource_with_ctx(&key, Some(ctx.clone()))
            .unwrap();
        res.write_at(0, original_data).unwrap();
        res.commit(Some(original_data.len() as u64)).unwrap();
    }

    let processed_res = store.open_resource_with_ctx(&key, Some(ctx)).unwrap();

    let mut buf = vec![0u8; 100];
    let n = processed_res.read_at(100, &mut buf).unwrap();
    assert_eq!(n, 0);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn store_without_processing_works_normally(temp_dir: kithara_integration_tests::TestTempDir) {
    let store = build_test_store_no_processing(&temp_dir, "no-processing");

    let key = ResourceKey::new("test.bin");

    {
        let res = store.acquire_resource(&key).unwrap();
        res.write_at(0, b"data").unwrap();
        res.commit(Some(4)).unwrap();
    }

    let res = store.open_resource(&key).unwrap();
    let mut buf = vec![0u8; 4];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 4);
    assert_eq!(&buf, b"data");
}
