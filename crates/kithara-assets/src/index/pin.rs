#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashSet};

use kithara_bufpool::BytePool;
use kithara_storage::{Atomic, ResourceExt, StorageError};

use super::schema::PinsIndexFile;
use crate::{
    base::Assets,
    error::{AssetsError, AssetsResult},
};

/// `PinsIndex` is a small facade over a storage resource that persists a set of pinned
/// `asset_root`s.
pub(crate) struct PinsIndex<R: ResourceExt> {
    res: Atomic<R>,
    pool: BytePool,
}

impl<R: ResourceExt> PinsIndex<R> {
    pub(crate) fn new(res: R, pool: BytePool) -> Self {
        Self {
            res: Atomic::new(res),
            pool,
        }
    }

    /// Open a `PinsIndex` for the given base assets store.
    pub(crate) fn open<A: Assets<IndexRes = R>>(assets: &A, pool: BytePool) -> AssetsResult<Self> {
        let res = assets.open_pins_index_resource()?;
        Ok(Self::new(res, pool))
    }

    /// Load the pins set from storage.
    pub(crate) fn load(&self) -> AssetsResult<HashSet<String>> {
        let mut buf = self.pool.get();
        let n = self.res.read_into(&mut buf)?;
        tracing::debug!("PinsIndex::load read {} bytes", n);

        if n == 0 {
            return Ok(HashSet::new());
        }

        let archived = match rkyv::access::<super::schema::ArchivedPinsIndexFile, rkyv::rancor::Error>(
            &buf[..n],
        ) {
            Ok(archived) => archived,
            Err(e) => {
                tracing::debug!("Failed to validate pins index: {}", e);
                return Ok(HashSet::new());
            }
        };

        let mut pinned = HashSet::new();
        for (k, v) in archived.pinned.iter() {
            if *v {
                pinned.insert(k.as_str().to_string());
            }
        }
        Ok(pinned)
    }

    /// Persist the provided pins set.
    pub(crate) fn store(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        let mut map = BTreeMap::new();
        for pin in pins {
            map.insert(pin.clone(), true);
        }
        let file = PinsIndexFile {
            version: 1,
            pinned: map,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&file)
            .map_err(|e| AssetsError::Storage(StorageError::Failed(e.to_string())))?;
        self.res.write_all(&bytes)?;

        Ok(())
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::{collections::HashSet, time::Duration};

    use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource};
    use kithara_test_utils::kithara;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn make_test_index(dir: &tempfile::TempDir, name: &str) -> PinsIndex<MmapResource> {
        let path = dir.path().join(name);
        let res = Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path,
                initial_len: Some(4096),
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap();
        PinsIndex::new(res, crate::byte_pool().clone())
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_pins_index_new() {
        let temp_dir = tempdir().unwrap();
        let idx = make_test_index(&temp_dir, "pins.bin");
        let pins = idx.load().unwrap();
        assert!(pins.is_empty(), "new index should have no pins");
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_load_empty_resource() {
        let temp_dir = tempdir().unwrap();
        let idx = make_test_index(&temp_dir, "empty.bin");
        let pins = idx.load().unwrap();
        assert!(pins.is_empty(), "empty file should yield empty set");
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_load_invalid_data() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("invalid.bin");
        std::fs::write(&path, b"not valid rkyv data").unwrap();

        let res: MmapResource = Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path,
                initial_len: None,
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap();
        let index = PinsIndex::new(res, crate::byte_pool().clone());

        let pins = index.load().unwrap();
        assert!(
            pins.is_empty(),
            "invalid file should gracefully yield empty set"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_store_and_load() {
        let temp_dir = tempdir().unwrap();
        let idx = make_test_index(&temp_dir, "pins.bin");

        let mut to_store = HashSet::new();
        to_store.insert("asset1".to_string());
        to_store.insert("asset2".to_string());

        idx.store(&to_store).unwrap();

        let loaded = idx.load().unwrap();
        assert_eq!(loaded, to_store);
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_persistence_across_instances() {
        let temp_dir = tempdir().unwrap();

        {
            let idx = make_test_index(&temp_dir, "shared.bin");
            let mut pins = HashSet::new();
            pins.insert("persistent_asset".to_string());
            idx.store(&pins).unwrap();
        }

        {
            let idx2 = make_test_index(&temp_dir, "shared.bin");
            let pins = idx2.load().unwrap();
            assert!(pins.contains("persistent_asset"));
            assert_eq!(pins.len(), 1);
        }
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn test_pins_index_file_format() {
        let temp_dir = tempdir().unwrap();
        let idx = make_test_index(&temp_dir, "format.bin");

        let mut pins = HashSet::new();
        pins.insert("asset".to_string());
        idx.store(&pins).unwrap();

        let mut buf = crate::byte_pool().get();
        let n = idx.res.read_into(&mut buf).unwrap();
        tracing::debug!("Bytes read: {}", n);

        let archived = rkyv::access::<
            crate::index::schema::ArchivedPinsIndexFile,
            rkyv::rancor::Error,
        >(&buf[..n])
        .unwrap();

        assert_eq!(archived.version, 1);
        assert_eq!(archived.pinned.len(), 1);
        assert!(*archived.pinned.get("asset").unwrap());
    }
}
