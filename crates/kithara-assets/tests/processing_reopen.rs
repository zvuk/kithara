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
use kithara_assets::{AcquisitionResult, AssetStoreBuilder, ProcessChunkFn, ReadSide, WriteSide};
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
    w.write_at(0, data).expect("scripted write succeeds");
    drop(
        w.commit(Some(data.len() as u64))
            .expect("scripted commit succeeds"),
    );
}

fn xor_process_fn(call_count: Arc<AtomicUsize>) -> ProcessChunkFn<()> {
    Arc::new(move |input, output, _ctx: &mut (), _is_last| {
        call_count.fetch_add(1, Ordering::SeqCst);
        for (idx, byte) in input.iter().copied().enumerate() {
            output[idx] = byte ^ 0x5A;
        }
        Ok(input.len())
    })
}

fn drm_process_fn() -> ProcessChunkFn<DecryptContext> {
    Arc::new(|input, output, ctx: &mut DecryptContext, is_last| {
        aes128_cbc_process_chunk(input, output, ctx, is_last)
    })
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
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .process_fn(xor_process_fn(Arc::clone(&call_count)))
        .build();
    let scope = store.scope(ROOT);

    let key0 = scope.key("segments/0000.bin");
    let key1 = scope.key("segments/0001.bin");
    let plaintext = b"segment-0-payload";
    let expected: Vec<u8> = plaintext.iter().map(|byte| byte ^ 0x5A).collect();

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key0, None, Some(()))
            .unwrap(),
        plaintext,
    );

    let calls_after_first_commit = call_count.load(Ordering::SeqCst);
    assert!(calls_after_first_commit > 0);

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key1, None, Some(()))
            .unwrap(),
        b"other-segment",
    );

    let calls_after_eviction = call_count.load(Ordering::SeqCst);
    assert!(calls_after_eviction > calls_after_first_commit);

    let reopened = scope
        .store()
        .open_resource_with_ctx(&key0, None, Some(()))
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
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .process_fn(xor_process_fn(Arc::clone(&call_count)))
        .build();
    let scope = store.scope(ROOT);

    let key0 = scope.key("segments/0000.bin");
    let key1 = scope.key("segments/0001.bin");
    let plaintext = b"segment-0-payload";
    let expected: Vec<u8> = plaintext.iter().map(|byte| byte ^ 0x5A).collect();

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key0, None, Some(()))
            .unwrap(),
        plaintext,
    );

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key1, None, Some(()))
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
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .process_fn(xor_process_fn(Arc::clone(&call_count)))
        .build();
    let scope = store.scope(ROOT);

    let key0 = scope.key("segments/0000.bin");
    let key1 = scope.key("segments/0001.bin");
    let plaintext: Vec<u8> = (0u8..=u8::MAX).cycle().take(512 * 1024 + 37).collect();
    let expected: Vec<u8> = plaintext.iter().map(|byte| byte ^ 0x5A).collect();

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key0, None, Some(()))
            .unwrap(),
        &plaintext,
    );

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key1, None, Some(()))
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
    let store = AssetStoreBuilder::new()
        .root_dir(dir.path())
        .cache_capacity(NonZeroUsize::new(1).unwrap())
        .process_fn(drm_process_fn())
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

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key0, None, Some(DecryptContext::new(key, iv)))
            .unwrap(),
        &ciphertext,
    );

    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key1, None, Some(DecryptContext::new(key, iv)))
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
