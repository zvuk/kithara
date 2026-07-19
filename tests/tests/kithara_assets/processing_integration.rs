#![forbid(unsafe_code)]

use std::{
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};

use kithara::{
    assets::{
        AcquisitionResult, AssetScope, AssetStoreBuilder, ChunkSink, ProcessCtx, ReadSide,
        ResourceProcessor, StorageBackend, WriteSide,
    },
    platform::{sync::Arc, time::Duration},
};
use kithara_integration_tests::temp_dir;

use super::support::{LiteralLayout, literal_layouts, resource, source};

#[derive(Debug)]
struct XorProcessor {
    call_count: Arc<AtomicUsize>,
    identity: [u8; 1],
    xor_key: u8,
}

impl XorProcessor {
    fn new(xor_key: u8, call_count: Arc<AtomicUsize>) -> Self {
        Self {
            call_count,
            identity: [xor_key],
            xor_key,
        }
    }
}

impl ResourceProcessor for XorProcessor {
    fn identity(&self) -> &[u8] {
        &self.identity
    }

    fn begin(&self) -> Box<dyn ChunkSink> {
        Box::new(XorSink {
            call_count: Arc::clone(&self.call_count),
            xor_key: self.xor_key,
        })
    }
}

struct XorSink {
    call_count: Arc<AtomicUsize>,
    xor_key: u8,
}

impl fmt::Debug for XorSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XorSink").finish_non_exhaustive()
    }
}

impl ChunkSink for XorSink {
    fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        _is_last: bool,
    ) -> Result<usize, String> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        for (i, &b) in input.iter().enumerate() {
            output[i] = b ^ self.xor_key;
        }
        Ok(input.len())
    }
}

fn create_xor_processor(xor_key: u8, call_count: Arc<AtomicUsize>) -> ProcessCtx {
    Arc::new(XorProcessor::new(xor_key, call_count))
}

fn build_test_processing_scope(
    temp_dir: &kithara_integration_tests::TestTempDir,
    asset_root: &str,
) -> AssetScope {
    let builder = AssetStoreBuilder::default();
    #[cfg(not(target_arch = "wasm32"))]
    {
        builder
            .backend(StorageBackend::Disk {
                root: (temp_dir.path()).into(),
            })
            .layouts(literal_layouts())
            .build()
            .scope::<LiteralLayout>(&source(asset_root))
            .expect("scope")
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = temp_dir;
        builder
            .backend(StorageBackend::Memory)
            .layouts(literal_layouts())
            .build()
            .scope::<LiteralLayout>(&source(asset_root))
            .expect("scope")
    }
}

fn build_test_scope_no_processing(
    temp_dir: &kithara_integration_tests::TestTempDir,
    asset_root: &str,
) -> AssetScope {
    #[cfg(not(target_arch = "wasm32"))]
    {
        AssetStoreBuilder::default()
            .backend(StorageBackend::Disk {
                root: (temp_dir.path()).into(),
            })
            .layouts(literal_layouts())
            .build()
            .scope::<LiteralLayout>(&source(asset_root))
            .expect("scope")
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = temp_dir;
        AssetStoreBuilder::default()
            .backend(StorageBackend::Memory)
            .layouts(literal_layouts())
            .build()
            .scope::<LiteralLayout>(&source(asset_root))
            .expect("scope")
    }
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn processing_transforms_data_on_commit(temp_dir: kithara_integration_tests::TestTempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let scope = build_test_processing_scope(&temp_dir, "test-processing");

    let key = scope.key(&resource("data.bin")).unwrap();

    let original_data = b"Hello, World! This is test data for processing.";
    let ctx = create_xor_processor(0x42, Arc::clone(&call_count));
    {
        let AcquisitionResult::Pending(writer) = scope
            .store()
            .acquire_resource_with_ctx(&key, None, Some(Arc::clone(&ctx)))
            .unwrap()
        else {
            panic!("fresh acquire must be Pending");
        };
        writer.write_at(0, original_data).unwrap();

        writer.commit(Some(original_data.len() as u64)).unwrap();
    }

    assert!(call_count.load(Ordering::SeqCst) > 0);

    let processed_res = scope
        .store()
        .open_resource_with_ctx(&key, None, Some(ctx))
        .unwrap();

    let mut buf = vec![0u8; original_data.len()];
    let n = processed_res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, original_data.len());

    let expected: Vec<u8> = original_data.iter().map(|b| b ^ 0x42).collect();
    assert_eq!(buf, expected);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn processing_caches_result_on_subsequent_reads(temp_dir: kithara_integration_tests::TestTempDir) {
    let call_count = Arc::new(AtomicUsize::new(0));

    let scope = build_test_processing_scope(&temp_dir, "test-cache");

    let key = scope.key(&resource("cached.bin")).unwrap();
    let ctx = create_xor_processor(0xAB, Arc::clone(&call_count));

    let original_data = b"Data for caching test";
    {
        let AcquisitionResult::Pending(writer) = scope
            .store()
            .acquire_resource_with_ctx(&key, None, Some(Arc::clone(&ctx)))
            .unwrap()
        else {
            panic!("fresh acquire must be Pending");
        };
        writer.write_at(0, original_data).unwrap();
        writer.commit(Some(original_data.len() as u64)).unwrap();
    }
    let count_after_commit = call_count.load(Ordering::SeqCst);

    let processed_res = scope
        .store()
        .open_resource_with_ctx(&key, None, Some(Arc::clone(&ctx)))
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

    let scope = build_test_processing_scope(&temp_dir, "test-partial");

    let key = scope.key(&resource("partial.bin")).unwrap();
    let ctx = create_xor_processor(0xFF, Arc::clone(&call_count));

    let original_data: Vec<u8> = (0..100).collect();
    {
        let AcquisitionResult::Pending(writer) = scope
            .store()
            .acquire_resource_with_ctx(&key, None, Some(Arc::clone(&ctx)))
            .unwrap()
        else {
            panic!("fresh acquire must be Pending");
        };
        writer.write_at(0, &original_data).unwrap();
        writer.commit(Some(original_data.len() as u64)).unwrap();
    }

    let processed_res = scope
        .store()
        .open_resource_with_ctx(&key, None, Some(ctx))
        .unwrap();

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

    let scope = build_test_processing_scope(&temp_dir, "test-eof");

    let key = scope.key(&resource("eof.bin")).unwrap();
    let ctx = create_xor_processor(0x00, Arc::clone(&call_count));

    let original_data = b"short";
    {
        let AcquisitionResult::Pending(writer) = scope
            .store()
            .acquire_resource_with_ctx(&key, None, Some(Arc::clone(&ctx)))
            .unwrap()
        else {
            panic!("fresh acquire must be Pending");
        };
        writer.write_at(0, original_data).unwrap();
        writer.commit(Some(original_data.len() as u64)).unwrap();
    }

    let processed_res = scope
        .store()
        .open_resource_with_ctx(&key, None, Some(ctx))
        .unwrap();

    let mut buf = vec![0u8; 100];
    let n = processed_res.read_at(100, &mut buf).unwrap();
    assert_eq!(n, 0);
}

#[kithara::test(timeout(Duration::from_secs(5)), env(KITHARA_HANG_TIMEOUT_SECS = "1"))]
fn store_without_processing_works_normally(temp_dir: kithara_integration_tests::TestTempDir) {
    let scope = build_test_scope_no_processing(&temp_dir, "no-processing");

    let key = scope.key(&resource("test.bin")).unwrap();

    {
        let AcquisitionResult::Pending(writer) =
            scope.store().acquire_resource(&key, None).unwrap()
        else {
            panic!("fresh acquire must be Pending");
        };
        writer.write_at(0, b"data").unwrap();
        writer.commit(Some(4)).unwrap();
    }

    let res = scope.store().open_resource(&key, None).unwrap();
    let mut buf = vec![0u8; 4];
    let n = res.read_at(0, &mut buf).unwrap();
    assert_eq!(n, 4);
    assert_eq!(&buf, b"data");
}
