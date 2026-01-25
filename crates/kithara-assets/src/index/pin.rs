#![forbid(unsafe_code)]

use std::collections::HashSet;

use kithara_storage::{AtomicResource, Resource};

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

/// `PinsIndex` is a small facade over an atomic storage resource that persists a set of pinned
/// `asset_root`s.
///
/// ## Normative
/// - The set is stored as a `HashSet<String>` in-memory.
/// - Every mutation (`insert`/`remove`) persists the full set immediately.
/// - The underlying storage is an `AtomicResource` (whole-object read/write).
///
/// ## Key selection
/// `PinsIndex` does **not** form keys. The concrete [`Assets`] implementation decides where this
/// index lives by implementing [`Assets::open_pins_index_resource`].
pub struct PinsIndex {
    res: AtomicResource,
}

impl PinsIndex {
    pub(crate) fn new(res: AtomicResource) -> Self {
        Self { res }
    }

    /// Open a `PinsIndex` for the given base assets store.
    pub async fn open<A: Assets>(assets: &A) -> AssetsResult<Self> {
        let res = assets.open_pins_index_resource().await?;
        Ok(Self::new(res))
    }

    /// Load the pins set from storage.
    ///
    /// Empty, missing, or corrupted data is treated as an empty set (best-effort).
    pub async fn load(&self) -> AssetsResult<HashSet<String>> {
        let bytes = self.res.read().await?;

        if bytes.is_empty() {
            return Ok(HashSet::new());
        }

        let file: PinsIndexFile = match bincode::deserialize(&bytes) {
            Ok(file) => file,
            Err(_) => return Ok(HashSet::new()),
        };

        Ok(file.pinned.into_iter().collect())
    }

    /// Persist the given set to storage.
    pub async fn store(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        // Stored as a list for stable serialization; treated as a set by higher layers.
        let file = PinsIndexFile {
            version: 1,
            pinned: pins.iter().cloned().collect(),
        };

        let bytes = bincode::serialize(&file)?;
        self.res.write(&bytes).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, time::Duration};

    use kithara_storage::AtomicOptions;
    use rstest::*;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    // Helper to create AtomicResource for tests
    fn create_test_resource(dir: &TempDir) -> AtomicResource {
        let path = dir.path().join("pins.json");
        AtomicResource::open(AtomicOptions {
            path,
            cancel: CancellationToken::new(),
        })
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_pins_index_new() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);

        let index = PinsIndex::new(res);

        // Index created successfully, load should return empty set
        let pins = index.load().await.unwrap();
        assert!(pins.is_empty());
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_load_empty_resource() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);
        let index = PinsIndex::new(res);

        let pins = index.load().await.unwrap();

        assert!(pins.is_empty());
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_load_invalid_json() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);

        // Write invalid JSON
        res.write(b"not valid json").await.unwrap();

        let index = PinsIndex::new(res);
        let pins = index.load().await.unwrap();

        // Should return empty set on invalid JSON (best-effort)
        assert!(pins.is_empty());
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_store_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);
        let index = PinsIndex::new(res);

        let mut pins = HashSet::new();
        pins.insert("asset1".to_string());
        pins.insert("asset2".to_string());

        index.store(&pins).await.unwrap();

        let loaded = index.load().await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded.contains("asset1"));
        assert!(loaded.contains("asset2"));
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_persistence_across_instances() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("pins.json");

        // First instance
        {
            let res = AtomicResource::open(AtomicOptions {
                path: path.clone(),
                cancel: CancellationToken::new(),
            });
            let index = PinsIndex::new(res);

            let mut pins = HashSet::new();
            pins.insert("persisted-asset".to_string());
            index.store(&pins).await.unwrap();
        }

        // Second instance (new resource, same path)
        {
            let res = AtomicResource::open(AtomicOptions {
                path,
                cancel: CancellationToken::new(),
            });
            let index = PinsIndex::new(res);

            let pins = index.load().await.unwrap();
            assert_eq!(pins.len(), 1);
            assert!(pins.contains("persisted-asset"));
        }
    }

    #[rstest]
    #[timeout(Duration::from_secs(1))]
    #[tokio::test]
    async fn test_pins_index_file_format() {
        let temp_dir = TempDir::new().unwrap();
        let res = create_test_resource(&temp_dir);
        let index = PinsIndex::new(res.clone());

        let mut pins = HashSet::new();
        pins.insert("asset".to_string());
        index.store(&pins).await.unwrap();

        // Read raw bytes and deserialize using bincode
        let bytes = res.read().await.unwrap();
        let file: PinsIndexFile = bincode::deserialize(&bytes).unwrap();

        assert_eq!(file.version, 1);
        assert_eq!(file.pinned.len(), 1);
        assert!(file.pinned.contains(&"asset".to_string()));
    }
}
