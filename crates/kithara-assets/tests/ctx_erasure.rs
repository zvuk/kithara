//! One non-generic `AssetStore` serves both `ctx = None` (file passthrough) and
//! `ctx = Some(ProcessCtx)` (per-acquire processing): covers chunk chaining and
//! per-acquire application of the processor.

use std::{
    fs,
    path::{Path, PathBuf},
};

use kithara_assets::{
    AcquisitionResult, AssetStore, AssetStoreBuilder, ChunkSink, ProcessCtx, ReadSide,
    ResourceProcessor, StorageBackend, WriteSide,
};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_test_utils::kithara;
use tempfile::tempdir;

/// Processing chunk size (64KB); a larger payload spans multiple chunks.
const CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug)]
struct EvolvingXorProcessor {
    identity: Box<[u8]>,
    seed: u8,
}

impl EvolvingXorProcessor {
    fn new(seed: u8, identity: &[u8]) -> Self {
        Self {
            seed,
            identity: identity.to_vec().into_boxed_slice(),
        }
    }
}

impl ResourceProcessor for EvolvingXorProcessor {
    fn begin(&self) -> Box<dyn ChunkSink> {
        Box::new(EvolvingXorSink {
            seed: self.seed,
            chunk: 0,
        })
    }

    fn identity(&self) -> &[u8] {
        &self.identity
    }
}

/// Per-commit chaining state: chunk `i` XORs with `seed + i`.
struct EvolvingXorSink {
    chunk: u8,
    seed: u8,
}

impl ChunkSink for EvolvingXorSink {
    fn process(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        _is_last: bool,
    ) -> Result<usize, String> {
        let key = self.seed.wrapping_add(self.chunk);
        for (i, &b) in input.iter().enumerate() {
            output[i] = b ^ key;
        }
        self.chunk = self.chunk.wrapping_add(1);
        Ok(input.len())
    }
}

/// Reference transform computed independently of the processor: the bytes are
/// split into `CHUNK_SIZE` chunks and chunk `i` is `XORed` with `seed + i`.
fn reference_transform(seed: u8, input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    for (idx, chunk) in input.chunks(CHUNK_SIZE).enumerate() {
        let idx_mod = match u8::try_from(idx % 256) {
            Ok(value) => value,
            Err(_) => unreachable!("idx % 256 always fits in u8"),
        };
        let key = seed.wrapping_add(idx_mod);
        out.extend(chunk.iter().map(|&b| b ^ key));
    }
    out
}

/// Build a `ProcessCtx` trait object from the test processor.
fn processor(seed: u8, identity: &[u8]) -> ProcessCtx {
    Arc::new(EvolvingXorProcessor::new(seed, identity))
}

/// Stream `data` through a Pending writer and commit it.
fn write_commit<W: WriteSide>(acq: AcquisitionResult<W, W::Reader>, data: &[u8]) {
    let AcquisitionResult::Pending(w) = acq else {
        panic!("expected a Pending writer");
    };
    w.write_at(0, data).expect("write_at");
    drop(w.commit(Some(data.len() as u64)).expect("commit"));
}

/// Read the whole committed resource back into a fresh `Vec`.
fn read_all<R: ReadSide>(reader: &R) -> Vec<u8> {
    let mut buf = Vec::new();
    reader.read_into(&mut buf).expect("read_into");
    buf
}

/// Collect every directory named `_index` reachable under `root`.
fn index_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "_index") {
                    found.push(path.clone());
                }
                stack.push(path);
            }
        }
    }
    found
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn one_store_serves_both_none_and_processing_scopes() {
    let dir = tempdir().unwrap();
    let store: AssetStore = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();

    let scope_a = store.scope("scope-a-file");
    let key_a = scope_a.key("media/audio.bin");
    let plain = b"plain file bytes, unprocessed";
    write_commit(
        scope_a
            .store()
            .acquire_resource_with_ctx(&key_a, None, None)
            .unwrap(),
        plain,
    );
    let read_a = read_all(&scope_a.store().open_resource(&key_a, None).unwrap());
    assert_eq!(read_a, plain, "ctx=None scope must round-trip raw bytes");

    let scope_b = store.scope("scope-b-drm");
    let key_b = scope_b.key("segments/0001.m4s");
    let payload = b"encrypted-ish segment payload";
    let proc = processor(0x42, &[0x42]);
    let expected_b = reference_transform(0x42, payload);
    write_commit(
        scope_b
            .store()
            .acquire_resource_with_ctx(&key_b, None, Some(Arc::clone(&proc)))
            .unwrap(),
        payload,
    );
    let read_b = read_all(&scope_b.store().open_resource(&key_b, None).unwrap());
    assert_eq!(
        read_b, expected_b,
        "ctx=Some scope must read back the processed bytes"
    );

    let indexes = index_dirs(dir.path());
    assert_eq!(
        indexes.len(),
        1,
        "exactly one `_index` directory must exist for one store; found {indexes:?}"
    );
    assert_eq!(
        indexes[0],
        dir.path().join("_index"),
        "the single `_index` must live directly under the one cache_dir"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn multi_chunk_chaining_matches_reference() {
    let dir = tempdir().unwrap();
    let store: AssetStore = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = store.scope("chaining");

    // 200KB > 64KB chunk size => 4 chunks => 3 key evolutions.
    let payload: Vec<u8> = (0u8..=u8::MAX).cycle().take(200 * 1024).collect();
    assert!(
        payload.len() > CHUNK_SIZE,
        "payload must span multiple chunks"
    );

    let seed = 0x11u8;
    let proc = processor(seed, &[seed]);
    let expected = reference_transform(seed, &payload);

    let key = scope.key("segments/big.m4s");
    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key, None, Some(Arc::clone(&proc)))
            .unwrap(),
        &payload,
    );

    let got = read_all(&scope.store().open_resource(&key, None).unwrap());
    assert_eq!(
        got, expected,
        "multi-chunk read-back must match the per-chunk-evolving reference; \
         a stateless processor (one fixed key for all chunks) would diverge"
    );

    // `begin()` must reset chaining state per commit: a second key processed
    // with a fresh sink chains from the seed again, not from where the first
    let key2 = scope.key("segments/big2.m4s");
    let payload2: Vec<u8> = (0u8..=u8::MAX).rev().cycle().take(200 * 1024).collect();
    let expected2 = reference_transform(seed, &payload2);
    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key2, None, Some(Arc::clone(&proc)))
            .unwrap(),
        &payload2,
    );
    let got2 = read_all(&scope.store().open_resource(&key2, None).unwrap());
    assert_eq!(
        got2, expected2,
        "begin() must mint fresh per-commit chaining state (counter reset to 0)"
    );
}

#[kithara::test(native, timeout(Duration::from_secs(5)))]
fn per_acquire_processor_applies_to_its_own_resource() {
    let dir = tempdir().unwrap();
    let store: AssetStore = AssetStoreBuilder::default()
        .backend(StorageBackend::Disk {
            root: (dir.path()).into(),
        })
        .build();
    let scope = store.scope("per-acquire");

    let payload = b"shared payload bytes for both keys";

    let key_one = scope.key("segments/one.m4s");
    let proc_one = processor(0x01, &[0x01]);
    let expected_one = reference_transform(0x01, payload);
    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key_one, None, Some(Arc::clone(&proc_one)))
            .unwrap(),
        payload,
    );

    let key_two = scope.key("segments/two.m4s");
    let proc_two = processor(0x02, &[0x02]);
    let expected_two = reference_transform(0x02, payload);
    write_commit(
        scope
            .store()
            .acquire_resource_with_ctx(&key_two, None, Some(Arc::clone(&proc_two)))
            .unwrap(),
        payload,
    );

    let read_one = read_all(&scope.store().open_resource(&key_one, None).unwrap());
    let read_two = read_all(&scope.store().open_resource(&key_two, None).unwrap());

    assert_eq!(
        read_one, expected_one,
        "key_one must read back processor_one's transform"
    );
    assert_eq!(
        read_two, expected_two,
        "key_two must read back processor_two's transform"
    );
    assert_ne!(
        read_one, read_two,
        "distinct-identity processors must not collide: each key keeps its own bytes"
    );

    let reread_one = read_all(&scope.store().open_resource(&key_one, None).unwrap());
    assert_eq!(
        reread_one, expected_one,
        "reopening the same key with the same identity must return consistent bytes"
    );
}
