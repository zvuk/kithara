#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    num::NonZeroU32,
    path::PathBuf,
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use kithara_bufpool::BytePool;
use kithara_platform::Mutex;
use kithara_storage::{Atomic, MmapResource, ResourceExt, StorageError};
use tokio_util::sync::CancellationToken;

use super::{
    flush::{FlushHub, Flushable, signal_or_flush_sync},
    persist,
    schema::PinsIndexFile,
};
use crate::error::{AssetsError, AssetsResult};

/// In-memory + best-effort disk-backed index of pinned `asset_root`s.
///
/// Architecturally symmetric to [`AvailabilityIndex`](super::AvailabilityIndex):
/// the `Arc` is encapsulated **inside** the type, [`Clone`] is cheap
/// (atomic refcount bump), every mutation that crosses the
/// pinned/unpinned boundary immediately flushes to the optional
/// disk-backed [`Atomic`] tempfile.
///
/// Each `asset_root` is tracked by a refcount: concurrent leases on the
/// same root increment it, drops decrement it. The on-disk pinned set
/// only changes (and only flushes) on the 0→1 and 1→0 transitions —
/// intermediate increments/decrements are pure in-memory updates.
///
/// Persistence is **lazy**: the disk file is materialised only on the
/// first [`Self::flush`]. A pre-existing on-disk file from a previous
/// run is opened eagerly during [`Self::with_persist_at`] for
/// hydration, but a fresh build does not touch the filesystem until a
/// real pin/unpin transition happens.
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
    pins: Mutex<HashMap<String, NonZeroU32>>,
    persist: Option<PinsPersist>,
    /// Set by [`FlushHub::register`]. While `None`, mutators flush
    /// synchronously through [`Flushable::flush`] directly — matches
    /// the historical inline-flush path for ad-hoc tests that never
    /// attach a hub.
    hub: OnceLock<Arc<FlushHub>>,
    dirty: AtomicBool,
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
                pins: Mutex::new(HashMap::new()),
                persist: None,
                hub: OnceLock::new(),
                dirty: AtomicBool::new(false),
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
                hub: OnceLock::new(),
                dirty: AtomicBool::new(false),
            }),
        }
    }

    /// Snapshot of currently pinned `asset_root`s (keys only — refcount stays internal).
    pub(crate) fn snapshot(&self) -> HashSet<String> {
        self.inner.pins.lock_sync().keys().cloned().collect()
    }

    /// Pin an `asset_root`. Returns `true` on the 0→1 transition (newly pinned),
    /// `false` when an existing pin's refcount was incremented.
    ///
    /// Disk-backed instances flush the snapshot synchronously on the 0→1
    /// transition only; intermediate refcount increments stay in-memory.
    /// An `Err` here means the pin is **not** durable. Ephemeral instances
    /// cannot fail.
    ///
    /// # Errors
    ///
    /// Propagates the underlying [`AssetsError`] when the on-disk
    /// flush fails (mmap open, atomic swap, rkyv serialise).
    pub(crate) fn add(&self, asset_root: &str) -> AssetsResult<bool> {
        use std::collections::hash_map::Entry;
        let transitioned = match self.inner.pins.lock_sync().entry(asset_root.to_string()) {
            Entry::Occupied(mut e) => {
                let count = e.get_mut();
                *count = count.saturating_add(1);
                false
            }
            Entry::Vacant(e) => {
                e.insert(NonZeroU32::MIN);
                true
            }
        };
        if transitioned {
            signal_or_flush_sync(self.inner.hub.get(), &*self.inner)?;
        }
        Ok(transitioned)
    }

    /// Unpin an `asset_root`. Returns `true` on the 1→0 transition (entry
    /// removed), `false` when the refcount was decremented but the asset
    /// is still pinned by other holders or was not pinned at all.
    ///
    /// Same durability contract as [`Self::add`]: flush only fires on
    /// 1→0 transitions.
    ///
    /// # Errors
    ///
    /// Propagates the underlying [`AssetsError`] when the on-disk
    /// flush fails.
    pub(crate) fn remove(&self, asset_root: &str) -> AssetsResult<bool> {
        let transitioned = {
            let mut pins = self.inner.pins.lock_sync();
            let next = pins.get(asset_root).map(|c| NonZeroU32::new(c.get() - 1));
            match next {
                None => false, // not pinned: no-op
                Some(Some(decremented)) => {
                    pins.insert(asset_root.to_string(), decremented);
                    false
                }
                Some(None) => {
                    pins.remove(asset_root);
                    true
                }
            }
        };
        if transitioned {
            signal_or_flush_sync(self.inner.hub.get(), &*self.inner)?;
        }
        Ok(transitioned)
    }

    /// Whether `asset_root` is currently pinned.
    #[cfg(test)]
    pub(crate) fn contains(&self, asset_root: &str) -> bool {
        self.inner.pins.lock_sync().contains_key(asset_root)
    }

    /// Force a synchronous flush. Routes through [`FlushHub::flush_now`]
    /// when a hub is attached (drains every dirty source) or runs the
    /// inline serialise+write path otherwise. Used by explicit
    /// checkpoint paths (`LeaseAssets::flush_pins`, `internal::*`).
    ///
    /// # Errors
    ///
    /// Propagates the first per-source flush error encountered.
    pub(crate) fn flush(&self) -> AssetsResult<()> {
        self.inner
            .hub
            .get()
            .map_or_else(|| Flushable::flush(&*self.inner), |hub| hub.flush_now())
    }

    /// Bind this index to a [`FlushHub`] for coordinated flushing.
    /// Called once per index instance; subsequent calls are no-ops.
    pub(crate) fn attach_to(&self, hub: &Arc<FlushHub>) {
        if self.inner.hub.set(Arc::clone(hub)).is_err() {
            return;
        }
        hub.register(Arc::downgrade(&self.inner) as Weak<dyn Flushable>);
    }
}

impl Flushable for PinsInner {
    fn name(&self) -> &'static str {
        "pins"
    }

    fn dirty(&self) -> &AtomicBool {
        &self.dirty
    }

    /// Serialise the current pinned set to `_index/pins.bin`. Refcount
    /// is collapsed to mere set-membership on disk — the persisted
    /// format only records keys.
    fn flush(&self) -> AssetsResult<()> {
        let Some(persist) = self.persist.as_ref() else {
            self.dirty.store(false, Ordering::Release);
            return Ok(());
        };
        let snapshot: Vec<String> = self.pins.lock_sync().keys().cloned().collect();
        let atomic = persist::init_atomic(&persist.res, &persist.path, &persist.cancel)?;
        write_pins(atomic, &snapshot)?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }
}

fn hydrate_existing(
    path: &std::path::Path,
    cancel: &CancellationToken,
    pool: &BytePool,
) -> (HashMap<String, NonZeroU32>, Option<Atomic<MmapResource>>) {
    let nonempty = fs::metadata(path).is_ok_and(|m| m.len() > 0);
    if !nonempty {
        return (HashMap::new(), None);
    }
    match persist::open_existing(path, cancel) {
        Ok(res) => {
            let atomic = Atomic::new(res);
            let initial = read_pins(&atomic, pool).unwrap_or_default();
            (initial, Some(atomic))
        }
        Err(e) => {
            tracing::debug!("open existing pins.bin failed: {e}");
            (HashMap::new(), None)
        }
    }
}

fn read_pins(
    res: &Atomic<MmapResource>,
    pool: &BytePool,
) -> AssetsResult<HashMap<String, NonZeroU32>> {
    let mut buf = pool.get();
    let n = res.read_into(&mut buf)?;

    if n == 0 {
        return Ok(HashMap::new());
    }

    let archived = match rkyv::access::<super::schema::ArchivedPinsIndexFile, rkyv::rancor::Error>(
        &buf[..n],
    ) {
        Ok(a) => a,
        Err(e) => {
            tracing::debug!("Failed to validate pins index: {}", e);
            return Ok(HashMap::new());
        }
    };

    // Hydrating from disk: every persisted pin starts at refcount 1. The disk
    // format only records the pinned set (not refcounts) — fresh leases will
    // re-increment as they take their guards.
    let mut pinned = HashMap::new();
    for (k, v) in archived.pinned.iter() {
        if *v {
            pinned.insert(k.as_str().to_string(), NonZeroU32::MIN);
        }
    }
    Ok(pinned)
}

fn write_pins(res: &Atomic<MmapResource>, pins: &[String]) -> AssetsResult<()> {
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

        assert!(idx.add("asset_a").unwrap(), "first pin reports 0→1");
        assert!(path.exists(), "first add must materialise pins.bin");
        assert!(idx.add("asset_b").unwrap(), "fresh root reports 0→1");
        assert!(
            !idx.add("asset_a").unwrap(),
            "second add of same root reports refcount-only update"
        );
        assert!(idx.contains("asset_a"));
        assert_eq!(idx.snapshot().len(), 2);

        // asset_a refcount is 2; the first remove only decrements.
        assert!(
            !idx.remove("asset_a").unwrap(),
            "decrement-only remove must not signal a 1→0 transition"
        );
        assert!(
            idx.contains("asset_a"),
            "still pinned by remaining refcount"
        );
        // The second remove crosses 1→0.
        assert!(idx.remove("asset_a").unwrap());
        assert!(!idx.contains("asset_a"));
        // Removing an already-unpinned root is a no-op.
        assert!(!idx.remove("asset_a").unwrap());
        assert!(idx.contains("asset_b"));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn refcount_coalesces_repeated_pins() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx =
            PinsIndex::with_persist_at(path.clone(), CancellationToken::new(), crate::byte_pool());

        // 5 adds: only the first crosses 0→1.
        let mut transitions_in = 0;
        for _ in 0..5 {
            if idx.add("hot_asset").unwrap() {
                transitions_in += 1;
            }
        }
        assert_eq!(transitions_in, 1, "only the 0→1 transition counts");
        assert!(idx.contains("hot_asset"));

        // 4 removes: refcount 5→1, no transition.
        let mut transitions_out = 0;
        for _ in 0..4 {
            if idx.remove("hot_asset").unwrap() {
                transitions_out += 1;
            }
        }
        assert_eq!(transitions_out, 0, "intermediate decrements stay in-memory");
        assert!(idx.contains("hot_asset"), "refcount=1 keeps the pin alive");

        // Final remove crosses 1→0.
        assert!(idx.remove("hot_asset").unwrap());
        assert!(!idx.contains("hot_asset"));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn concurrent_leases_keep_pin_alive() {
        // Models the HLS pattern where multiple resources of the same
        // asset_root are leased concurrently. Dropping one lease must
        // not unpin the asset while others are still alive.
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = PinsIndex::with_persist_at(path, CancellationToken::new(), crate::byte_pool());

        idx.add("playlist").unwrap(); // resource 1
        idx.add("playlist").unwrap(); // resource 2

        // Drop resource 1 → refcount 2→1, asset still pinned by resource 2.
        assert!(!idx.remove("playlist").unwrap());
        assert!(idx.contains("playlist"));

        // Drop resource 2 → refcount 1→0, finally unpinned.
        assert!(idx.remove("playlist").unwrap());
        assert!(!idx.contains("playlist"));
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
