#![forbid(unsafe_code)]

use std::collections::HashSet;

use kithara_bufpool::BytePool;
use kithara_storage::{Atomic, ResourceExt};

use crate::{base::Assets, error::AssetsResult};

/// Minimal persisted representation of the pins index.
///
/// This struct is intentionally private to keep the on-disk JSON schema as an implementation detail
/// of this crate.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, Default)]
struct PinsIndexFile {
    version: u32,
    pinned: Vec<String>,
}

/// `PinsIndex` is a small facade over a storage resource that persists a set of pinned
/// `asset_root`s.
///
/// ## Normative
/// - The set is stored as a `HashSet<String>` in-memory.
/// - Every mutation (`insert`/`remove`) persists the full set immediately.
/// - The underlying storage uses whole-object read/write via `read_into`/`write_all`.
///
/// ## Key selection
/// `PinsIndex` does **not** form keys. The concrete [`Assets`] implementation decides where this
/// index lives by implementing [`Assets::open_pins_index_resource`].
pub struct PinsIndex<R: ResourceExt> {
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
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the pins index resource cannot be opened.
    pub fn open<A: Assets<IndexRes = R>>(assets: &A, pool: BytePool) -> AssetsResult<Self> {
        let res = assets.open_pins_index_resource()?;
        Ok(Self::new(res, pool))
    }

    /// Load the pins set from storage.
    ///
    /// Empty, missing, or corrupted data is treated as an empty set (best-effort).
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if reading from the underlying storage resource fails.
    pub fn load(&self) -> AssetsResult<HashSet<String>> {
        let mut buf = self.pool.get();
        let n = self.res.read_into(&mut buf)?;

        if n == 0 {
            return Ok(HashSet::new());
        }

        let file: PinsIndexFile =
            match bincode::serde::decode_from_slice(&buf, bincode::config::legacy()) {
                Ok((file, _)) => file,
                Err(_) => return Ok(HashSet::new()),
            };

        Ok(file.pinned.into_iter().collect())
    }

    /// Persist the given set to storage (crash-safe via atomic write-rename).
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if serialization or writing to storage fails.
    pub fn store(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        // Stored as a list for stable serialization; treated as a set by higher layers.
        let file = PinsIndexFile {
            version: 1,
            pinned: pins.iter().cloned().collect(),
        };

        let bytes = bincode::serde::encode_to_vec(&file, bincode::config::legacy())?;
        self.res.write_all(&bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, time::Duration};

    use kithara_storage::{MmapOptions, MmapResource, OpenMode, Resource};
    use rstest::*;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    // Helper to create MmapResource for tests
    fn create_test_resource(dir: &TempDir) -> MmapResource {
        let path = dir.path().join("pins.bin");
        Resource::open(
            CancellationToken::new(),
            MmapOptions {
                path,
                initial_len: Some(4096),
                mode: OpenMode::ReadWrite,
            },
        )
        .unwrap()
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_pins_index_new() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);

        let index = PinsIndex::new(res, crate::byte_pool().clone());

        // Index created successfully, load should return empty set
        let pins = index.load().unwrap();
        assert!(pins.is_empty());
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_load_empty_resource() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);
        let index = PinsIndex::new(res, crate::byte_pool().clone());

        let pins = index.load().unwrap();

        assert!(pins.is_empty());
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_load_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);

        // Write invalid JSON
        res.write_all(b"not valid json").unwrap();

        let index = PinsIndex::new(res, crate::byte_pool().clone());
        let pins = index.load().unwrap();

        // Should return empty set on invalid JSON (best-effort)
        assert!(pins.is_empty());
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_store_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);
        let index = PinsIndex::new(res, crate::byte_pool().clone());

        let mut pins = HashSet::new();
        pins.insert("asset1".to_string());
        pins.insert("asset2".to_string());

        index.store(&pins).unwrap();

        let loaded = index.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains("asset1"));
        assert!(loaded.contains("asset2"));
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_persistence_across_instances() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("pins.bin");

        // First instance
        {
            let res: MmapResource = Resource::open(
                CancellationToken::new(),
                MmapOptions {
                    path: path.clone(),
                    initial_len: Some(4096),
                    mode: OpenMode::ReadWrite,
                },
            )
            .unwrap();
            let index = PinsIndex::new(res, crate::byte_pool().clone());

            let mut pins = HashSet::new();
            pins.insert("persisted-asset".to_string());
            index.store(&pins).unwrap();
        }

        // Second instance (new resource, same path)
        {
            let res: MmapResource = Resource::open(
                CancellationToken::new(),
                MmapOptions {
                    path,
                    initial_len: Some(4096),
                    mode: OpenMode::ReadWrite,
                },
            )
            .unwrap();
            let index = PinsIndex::new(res, crate::byte_pool().clone());

            let pins = index.load().unwrap();
            assert_eq!(pins.len(), 1);
            assert!(pins.contains("persisted-asset"));
        }
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[test]
    fn test_pins_index_file_format() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);
        let index = PinsIndex::new(res.clone(), crate::byte_pool().clone());

        let mut pins = HashSet::new();
        pins.insert("asset".to_string());
        index.store(&pins).unwrap();

        // Read raw bytes and deserialize using bincode
        let mut buf = crate::byte_pool().get();
        res.read_into(&mut buf).unwrap();
        let (file, _): (PinsIndexFile, _) =
            bincode::serde::decode_from_slice(&buf, bincode::config::legacy()).unwrap();

        assert_eq!(file.version, 1);
        assert_eq!(file.pinned.len(), 1);
        assert!(file.pinned.contains(&"asset".to_string()));
    }
}
