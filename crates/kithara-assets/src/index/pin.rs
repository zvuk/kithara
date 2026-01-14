#![forbid(unsafe_code)]

use std::collections::HashSet;

use kithara_storage::{AtomicResource, Resource};

use crate::{cache::Assets, error::AssetsResult};

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
    /// Empty, missing, or invalid JSON is treated as an empty set (best-effort).
    pub async fn load(&self) -> AssetsResult<HashSet<String>> {
        let bytes = self.res.read().await?;

        if bytes.is_empty() {
            return Ok(HashSet::new());
        }

        let file: PinsIndexFile = match serde_json::from_slice(&bytes) {
            Ok(file) => file,
            Err(_) => return Ok(HashSet::new()),
        };

        Ok(file.pinned.into_iter().collect())
    }

    /// Persist the given set to storage.
    pub async fn store(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        // Stored as a list for stable JSON; treated as a set by higher layers.
        let file = PinsIndexFile {
            version: 1,
            pinned: pins.iter().cloned().collect(),
        };

        let bytes = serde_json::to_vec_pretty(&file)?;
        self.res.write(&bytes).await?;
        Ok(())
    }

    /// Add `asset_root` to the set and persist immediately.
    #[allow(dead_code)]
    pub async fn insert(&self, asset_root: &str) -> AssetsResult<()> {
        let mut pins = self.load().await?;
        pins.insert(asset_root.to_string());
        self.store(&pins).await
    }

    /// Remove `asset_root` from the set and persist immediately.
    #[allow(dead_code)]
    pub async fn remove(&self, asset_root: &str) -> AssetsResult<()> {
        let mut pins = self.load().await?;
        pins.remove(asset_root);
        self.store(&pins).await
    }

    /// Check whether `asset_root` is pinned (loads from storage).
    #[allow(dead_code)]
    pub async fn contains(&self, asset_root: &str) -> AssetsResult<bool> {
        let pins = self.load().await?;
        Ok(pins.contains(asset_root))
    }
}
