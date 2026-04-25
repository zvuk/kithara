#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use kithara_bufpool::BytePool;
use kithara_platform::Mutex;
use kithara_storage::{Atomic, MmapResource, ResourceExt, StorageError};
use tokio_util::sync::CancellationToken;

use super::{persist, schema::PinsIndexFile};
use crate::error::{AssetsError, AssetsResult};

/// In-memory + best-effort disk-backed index of pinned `asset_root`s.
///
/// Architecturally symmetric to [`AvailabilityIndex`](super::AvailabilityIndex):
/// the `Arc` is encapsulated **inside** the type, [`Clone`] is cheap
/// (atomic refcount bump), every mutation immediately flushes to the
/// optional disk-backed [`Atomic`] tempfile.
///
/// Persistence is **lazy**: the disk file is materialised only on the
/// first [`Self::flush`]. A pre-existing on-disk file from a previous
/// run is opened eagerly during [`Self::with_persist_at`] for
/// hydration, but a fresh build does not touch the filesystem until a
/// real pin/unpin happens. This matches the documented contract on
/// `_index/pins.bin` ("yes, every pin/unpin").
///
/// Three call-sites share a single instance per `cache_dir`:
///   * `LeaseAssets` (pin/unpin on resource lifecycle),
///   * `EvictAssets` (read pinned set when picking eviction candidates),
///   * `DiskAssetDeleter` (drop pin when an `asset_root` is fully removed).
#[derive(Clone)]
pub(crate) struct PinsIndex {
    inner: Arc<PinsInner>,
}

impl std::fmt::Debug for PinsIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinsIndex")
            .field("len", &self.inner.pins.try_lock().map(|p| p.len()).ok())
            .field("persist", &self.inner.persist.is_some())
            .finish()
    }
}

struct PinsInner {
    pins: Mutex<HashSet<String>>,
    persist: Option<PinsPersist>,
}

struct PinsPersist {
    path: PathBuf,
    cancel: CancellationToken,
    res: OnceLock<Atomic<MmapResource>>,
}

impl PinsIndex {
    /// Construct an ephemeral index (mem backend mode).
    ///
    /// State lives only as long as this `Arc` is reachable; nothing
    /// is written to disk.
    pub(crate) fn ephemeral() -> Self {
        Self {
            inner: Arc::new(PinsInner {
                pins: Mutex::new(HashSet::new()),
                persist: None,
            }),
        }
    }

    /// Construct a disk-backed index rooted at `path`.
    ///
    /// If the file already exists and is non-empty, it is opened and
    /// hydrated synchronously. Otherwise the disk file is **not**
    /// materialised — it appears the first time [`Self::add`] /
    /// [`Self::remove`] flush a real change.
    pub(crate) fn with_persist_at(
        path: PathBuf,
        cancel: CancellationToken,
        pool: &BytePool,
    ) -> Self {
        let (initial, opened) = hydrate_existing(&path, &cancel, pool);
        Self {
            inner: Arc::new(PinsInner {
                pins: Mutex::new(initial),
                persist: Some(PinsPersist {
                    path,
                    cancel,
                    res: opened.map_or_else(OnceLock::new, |a| {
                        let cell = OnceLock::new();
                        cell.set(a)
                            .unwrap_or_else(|_| unreachable!("freshly created cell"));
                        cell
                    }),
                }),
            }),
        }
    }

    /// Snapshot of currently pinned `asset_root`s.
    pub(crate) fn snapshot(&self) -> HashSet<String> {
        self.inner.pins.lock_sync().clone()
    }

    /// Pin an `asset_root`. Returns `true` if newly pinned.
    ///
    /// Disk-backed instances flush the snapshot synchronously before
    /// returning; an `Err` here means the pin is **not** durable.
    /// Ephemeral instances cannot fail.
    ///
    /// # Errors
    ///
    /// Propagates the underlying [`AssetsError`] when the on-disk
    /// flush fails (mmap open, atomic swap, rkyv serialise).
    pub(crate) fn add(&self, asset_root: &str) -> AssetsResult<bool> {
        let inserted = self.inner.pins.lock_sync().insert(asset_root.to_string());
        if inserted {
            self.flush()?;
        }
        Ok(inserted)
    }

    /// Unpin an `asset_root`. Returns `true` if was previously pinned.
    ///
    /// Same durability contract as [`Self::add`].
    ///
    /// # Errors
    ///
    /// Propagates the underlying [`AssetsError`] when the on-disk
    /// flush fails.
    pub(crate) fn remove(&self, asset_root: &str) -> AssetsResult<bool> {
        let removed = self.inner.pins.lock_sync().remove(asset_root);
        if removed {
            self.flush()?;
        }
        Ok(removed)
    }

    /// Whether `asset_root` is currently pinned.
    #[cfg(test)]
    pub(crate) fn contains(&self, asset_root: &str) -> bool {
        self.inner.pins.lock_sync().contains(asset_root)
    }

    /// Persist the in-memory snapshot to disk (best-effort).
    ///
    /// Materialises the on-disk file on first call, reuses the cached
    /// [`Atomic`] handle on subsequent calls.
    ///
    /// # Errors
    ///
    /// Propagates [`AssetsError`] for mmap open / write failures and
    /// rkyv serialisation errors.
    pub(crate) fn flush(&self) -> AssetsResult<()> {
        let Some(persist) = self.inner.persist.as_ref() else {
            return Ok(());
        };
        let snapshot = self.inner.pins.lock_sync().clone();
        let atomic = persist::init_atomic(&persist.res, &persist.path, &persist.cancel)?;
        write_pins(atomic, &snapshot)
    }
}

fn hydrate_existing(
    path: &std::path::Path,
    cancel: &CancellationToken,
    pool: &BytePool,
) -> (HashSet<String>, Option<Atomic<MmapResource>>) {
    let nonempty = fs::metadata(path).is_ok_and(|m| m.len() > 0);
    if !nonempty {
        return (HashSet::new(), None);
    }
    match persist::open_existing(path, cancel) {
        Ok(res) => {
            let atomic = Atomic::new(res);
            let initial = read_pins(&atomic, pool).unwrap_or_default();
            (initial, Some(atomic))
        }
        Err(e) => {
            tracing::debug!("open existing pins.bin failed: {e}");
            (HashSet::new(), None)
        }
    }
}

fn read_pins(res: &Atomic<MmapResource>, pool: &BytePool) -> AssetsResult<HashSet<String>> {
    let mut buf = pool.get();
    let n = res.read_into(&mut buf)?;

    if n == 0 {
        return Ok(HashSet::new());
    }

    let archived = match rkyv::access::<super::schema::ArchivedPinsIndexFile, rkyv::rancor::Error>(
        &buf[..n],
    ) {
        Ok(a) => a,
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

fn write_pins(res: &Atomic<MmapResource>, pins: &HashSet<String>) -> AssetsResult<()> {
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
    res.write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::*;

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn empty_index_has_no_pins() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx =
            PinsIndex::with_persist_at(path.clone(), CancellationToken::new(), crate::byte_pool());
        assert!(idx.snapshot().is_empty());
        assert!(
            !path.exists(),
            "fresh disk-backed pins index must not materialise the file before any pin"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn add_and_remove_persist_immediately() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx =
            PinsIndex::with_persist_at(path.clone(), CancellationToken::new(), crate::byte_pool());

        assert!(idx.add("asset_a").unwrap());
        assert!(path.exists(), "first add must materialise pins.bin");
        assert!(idx.add("asset_b").unwrap());
        assert!(
            !idx.add("asset_a").unwrap(),
            "duplicate add should report false"
        );
        assert!(idx.contains("asset_a"));
        assert_eq!(idx.snapshot().len(), 2);

        assert!(idx.remove("asset_a").unwrap());
        assert!(!idx.remove("asset_a").unwrap());
        assert!(!idx.contains("asset_a"));
        assert!(idx.contains("asset_b"));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn persistence_across_instances() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("shared.bin");

        {
            let idx = PinsIndex::with_persist_at(
                path.clone(),
                CancellationToken::new(),
                crate::byte_pool(),
            );
            idx.add("persistent_asset").unwrap();
        }

        {
            let idx = PinsIndex::with_persist_at(
                path.clone(),
                CancellationToken::new(),
                crate::byte_pool(),
            );
            assert!(idx.contains("persistent_asset"));
            assert_eq!(idx.snapshot().len(), 1);
        }
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn invalid_data_is_treated_as_empty() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("invalid.bin");
        fs::write(&path, b"not valid rkyv data").unwrap();

        let idx = PinsIndex::with_persist_at(path, CancellationToken::new(), crate::byte_pool());
        assert!(idx.snapshot().is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn ephemeral_index_does_not_persist() {
        let idx = PinsIndex::ephemeral();
        assert!(idx.add("asset").unwrap());
        assert!(idx.contains("asset"));
        // No assertion on disk — there is no disk.
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn clone_shares_state() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = PinsIndex::with_persist_at(path, CancellationToken::new(), crate::byte_pool());
        let idx2 = idx.clone();

        idx.add("from_first").unwrap();
        assert!(idx2.contains("from_first"));

        idx2.remove("from_first").unwrap();
        assert!(!idx.contains("from_first"));
    }
}
