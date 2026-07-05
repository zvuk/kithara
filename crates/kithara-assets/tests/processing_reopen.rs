use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use aes::Aes128;
use cbc::{
    Encryptor,
    cipher::{BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7},
};
use kithara_assets::{
    AcquisitionResult, AssetStoreBuilder, ChunkSink, ProcessCtx, ReadSide, ResourceProcessor,
    StorageBackend, WriteSide,
};
use kithara_drm::{DecryptContext, aes128_cbc_process_chunk};
use kithara_platform::time::Duration;
use kithara_storage::ResourceStatus;
use kithara_test_utils::kithara;
use tempfile::tempdir;

const ROOT: &str = "processed-asset";
const DRM_ROOT: &str = "processed-drm-asset";

/// Stream `data` through a Pending writer and commit it.
fn write_commit<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>, data: &[u8]) {
    let AcquisitionResult::Pending(w) = acq else {
        panic!("expected a Pending writer");
    };
    w.write_at(0, data).expect("write_at");
    drop(w.commit(Some(data.len() as u64)).expect("commit"));
}

/// Fixed-key XOR processor counting its `process` calls.
#[derive(Debug)]
struct XorProcessor {
    call_count: Arc<AtomicUsize>,
}

struct XorSink {
    call_count: Arc<AtomicUsize>,
}

impl ChunkSink for XorSink {
    fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        _is_last: bool,
    ) -> Result<usize, String> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        for (idx, byte) in input.iter().copied().enumerate() {
            output[idx] = byte ^ 0x5A;
        }
        Ok(input.len())
    }
}

impl ResourceProcessor for XorProcessor {
    fn identity(&self) -> &[u8] {
        &[0x5A]
    }

    fn begin(&self) -> Box<dyn ChunkSink> {
        Box::new(XorSink {
            call_count: Arc::clone(&self.call_count),
        })
    }
}

fn xor_processor(call_count: Arc<AtomicUsize>) -> ProcessCtx {
    Arc::new(XorProcessor { call_count })
}

/// Test-local AES-128-CBC processor mirroring the production HLS adapter:
/// `identity` is `key||iv`; `begin()` mints a fresh `CbcSink` so CBC IV
/// chaining restarts from the seed on each commit/reactivate.
#[derive(Debug)]
struct DecryptProcessor {
    ctx: DecryptContext,
    identity: Box<[u8]>,
}

impl DecryptProcessor {
    fn new(ctx: DecryptContext) -> Self {
        let mut identity = Vec::with_capacity(ctx.key.len() + ctx.iv.len());
        identity.extend_from_slice(&ctx.key);
        identity.extend_from_slice(&ctx.iv);
        Self {
            ctx,
            identity: identity.into_boxed_slice(),
        }
    }
}

impl ResourceProcessor for DecryptProcessor {
    fn identity(&self) -> &[u8] {
        &self.identity
    }

    fn begin(&self) -> Box<dyn ChunkSink> {
        Box::new(CbcSink {
            ctx: self.ctx.clone(),
        })
    }
}

struct CbcSink {
    ctx: DecryptContext,
}

impl ChunkSink for CbcSink {
    fn process(&mut self, input: &[u8], output: &mut [u8], is_last: bool) -> Result<usize, String> {
        aes128_cbc_process_chunk(input, output, &mut self.ctx, is_last)
    }
}

fn encrypt_aes128_cbc(plaintext: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    let encryptor = Encryptor::<Aes128>::new(key.into(), iv.into());
    let padded_len = plaintext.len() + (16 - plaintext.len() % 16);
    let mut buf = vec![0u8; padded_len];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    let ct = encryptor
        .encrypt_padded::<Pkcs7>(&mut buf, plaintext.len())
        .expect("encrypt_padded failed");
    ct.to_vec()
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn reopened_committed_resource_after_cache_eviction_is_not_processed_again() {
    let dir = tempdir().unwrap();
    let call_count = Arc::new(AtomicUsize::new(0));
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .build();
    let scope = store.scope(ROOT);
    let proc = xor_processor(Arc::clone(&call_count));

    let key0 = scope.key("segments/0000.bin");
    let key1 = scope.key("segments/0001.bin");
    let plaintext = b"segment-0-payload";
    let expected: Vec<u8> = plaintext.iter().map(|byte| byte ^ 0x5A).collect();

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key0, None, Some(Arc::clone(&proc)))
            .unwrap(),
        plaintext,
    );

    let calls_after_first_commit = call_count.load(Ordering::SeqCst);
    assert!(calls_after_first_commit > 0);

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key1, None, Some(Arc::clone(&proc)))
            .unwrap(),
        b"other-segment",
    );

    let calls_after_eviction = call_count.load(Ordering::SeqCst);
    assert!(calls_after_eviction > calls_after_first_commit);

    let reopened = scope
        .store()
        .open_resource_with_ctx(&key0, None, Some(Arc::clone(&proc)))
        .unwrap();
    assert!(
        matches!(reopened.status(), ResourceStatus::Committed { .. }),
        "reopened processed resource must stay committed after cache eviction"
    );

    let mut buf = Vec::new();
    let read = reopened.read_into(&mut buf).unwrap();

    assert_eq!(read, expected.len());
    assert_eq!(buf, expected);
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        calls_after_eviction,
        "reopen must not invoke the process callback again"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn reopened_committed_processed_resource_without_ctx_reads_committed_bytes() {
    let dir = tempdir().unwrap();
    let call_count = Arc::new(AtomicUsize::new(0));
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .build();
    let scope = store.scope(ROOT);
    let proc = xor_processor(Arc::clone(&call_count));

    let key0 = scope.key("segments/0000.bin");
    let key1 = scope.key("segments/0001.bin");
    let plaintext = b"segment-0-payload";
    let expected: Vec<u8> = plaintext.iter().map(|byte| byte ^ 0x5A).collect();

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key0, None, Some(Arc::clone(&proc)))
            .unwrap(),
        plaintext,
    );

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key1, None, Some(Arc::clone(&proc)))
            .unwrap(),
        b"other-segment",
    );

    let reopened = scope.store().open_resource(&key0, None).unwrap();
    assert!(
        matches!(reopened.status(), ResourceStatus::Committed { .. }),
        "reopened processed resource must stay committed after cache eviction"
    );

    let mut buf = Vec::new();
    let read = reopened.read_into(&mut buf).unwrap();

    assert_eq!(read, expected.len());
    assert_eq!(buf, expected);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn reopened_large_committed_processed_resource_without_ctx_reads_committed_bytes() {
    let dir = tempdir().unwrap();
    let call_count = Arc::new(AtomicUsize::new(0));
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .build();
    let scope = store.scope(ROOT);
    let proc = xor_processor(Arc::clone(&call_count));

    let key0 = scope.key("segments/0000.bin");
    let key1 = scope.key("segments/0001.bin");
    let plaintext: Vec<u8> = (0u8..=u8::MAX).cycle().take(512 * 1024 + 37).collect();
    let expected: Vec<u8> = plaintext.iter().map(|byte| byte ^ 0x5A).collect();

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key0, None, Some(Arc::clone(&proc)))
            .unwrap(),
        &plaintext,
    );

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key1, None, Some(Arc::clone(&proc)))
            .unwrap(),
        b"other-segment",
    );

    let reopened = scope.store().open_resource(&key0, None).unwrap();
    assert!(
        matches!(reopened.status(), ResourceStatus::Committed { .. }),
        "reopened processed resource must stay committed after cache eviction"
    );

    let mut buf = vec![0u8; expected.len()];
    let read = reopened.read_at(0, &mut buf).unwrap();

    assert_eq!(read, expected.len());
    assert_eq!(buf, expected);
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn reopened_large_committed_drm_processed_resource_without_ctx_reads_committed_bytes() {
    let dir = tempdir().unwrap();
    let store = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .build();
    let scope = store.scope(DRM_ROOT);

    let key = [0x41u8; 16];
    let iv = [0x17u8; 16];
    let key0 = scope.key("segments/0000.bin");
    let key1 = scope.key("segments/0001.bin");
    let plaintext: Vec<u8> = (0u8..=u8::MAX).cycle().take(512 * 1024 + 37).collect();
    let ciphertext = encrypt_aes128_cbc(&plaintext, &key, &iv);
    let other_plaintext = b"other-segment";
    let other_ciphertext = encrypt_aes128_cbc(other_plaintext, &key, &iv);
    let proc: ProcessCtx = Arc::new(DecryptProcessor::new(DecryptContext::new(key, iv)));

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key0, None, Some(Arc::clone(&proc)))
            .unwrap(),
        &ciphertext,
    );

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key1, None, Some(Arc::clone(&proc)))
            .unwrap(),
        &other_ciphertext,
    );

    let reopened = scope.store().open_resource(&key0, None).unwrap();
    assert!(
        matches!(reopened.status(), ResourceStatus::Committed { .. }),
        "reopened DRM processed resource must stay committed after cache eviction"
    );

    let mut buf = vec![0u8; plaintext.len()];
    let read = reopened.read_at(0, &mut buf).unwrap();

    assert_eq!(read, plaintext.len());
    assert_eq!(buf, plaintext);
}
