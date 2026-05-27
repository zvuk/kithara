#![forbid(unsafe_code)]

use std::{
    collections::HashSet,
    num::NonZeroU32,
    sync::{Arc, OnceLock, Weak, atomic::AtomicBool},
};

use dashmap::{DashMap, mapref::entry::Entry};

use crate::{
    error::AssetsResult,
    flush::{FlushHub, Flushable, flush_sync},
};

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
/// run is opened eagerly during `Self::with_persist_at` (native only)
/// for hydration. On wasm32 the index is always ephemeral.
///
/// Three call-sites share a single instance per `cache_dir`:
///   * `LeaseAssets` (pin/unpin on resource lifecycle),
///   * `EvictAssets` (read pinned set when picking eviction candidates),
///   * `DiskAssetDeleter` (drop pin when an `asset_root` is fully removed).
#[derive(Clone)]
pub struct PinsIndex {
    pub(super) inner: Arc<PinsInner>,
}

impl std::fmt::Debug for PinsIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut dbg = f.debug_struct("PinsIndex");
        dbg.field("len", &Some(self.inner.pins.len()));
        #[cfg(not(target_arch = "wasm32"))]
        dbg.field("persist", &self.inner.persist.is_some());
        dbg.finish()
    }
}

pub(super) struct PinsInner {
    pub(super) dirty: AtomicBool,
    pub(super) pins: DashMap<String, NonZeroU32>,
    /// Set by [`FlushHub::register`]. While `None`, mutators flush
    /// synchronously through [`Flushable::flush`] directly — matches
    /// the historical inline-flush path for ad-hoc tests that never
    /// attach a hub.
    pub(super) hub: OnceLock<Arc<FlushHub>>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) persist: Option<super::disk::PinsPersist>,
}

impl PinsIndex {
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
    /// Propagates the underlying [`AssetsError`](crate::error::AssetsError)
    /// when the on-disk flush fails (mmap open, atomic swap, rkyv serialise).
    pub fn add(&self, asset_root: &str) -> AssetsResult<bool> {
        let transitioned = match self.inner.pins.entry(asset_root.to_string()) {
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
            flush_sync(&*self.inner)?;
        }
        Ok(transitioned)
    }

    /// Bind this index to a [`FlushHub`] for coordinated flushing.
    /// Called once per index instance; subsequent calls are no-ops.
    pub(crate) fn attach_to(&self, hub: &Arc<FlushHub>) {
        if self.inner.hub.set(Arc::clone(hub)).is_err() {
            return;
        }
        hub.register(Arc::downgrade(&self.inner) as Weak<dyn Flushable>);
    }

    /// Whether `asset_root` is currently pinned.
    #[cfg(test)]
    pub(crate) fn contains(&self, asset_root: &str) -> bool {
        self.inner.pins.contains_key(asset_root)
    }

    /// Construct an ephemeral index (mem backend mode).
    ///
    /// State lives only as long as this `Arc` is reachable; nothing
    /// is written to disk.
    pub(crate) fn ephemeral() -> Self {
        Self {
            inner: Arc::new(PinsInner {
                pins: DashMap::new(),
                #[cfg(not(target_arch = "wasm32"))]
                persist: None,
                hub: OnceLock::new(),
                dirty: AtomicBool::new(false),
            }),
        }
    }

    /// Force a synchronous flush. Routes through [`FlushHub::flush_now`]
    /// when a hub is attached (drains every dirty source) or runs the
    /// inline serialise+write path otherwise. Used by the eager pin
    /// add/remove path.
    ///
    /// # Errors
    ///
    /// Propagates the first per-source flush error encountered.
    pub fn flush(&self) -> AssetsResult<()> {
        self.inner
            .hub
            .get()
            .map_or_else(|| Flushable::flush(&*self.inner), |hub| hub.flush_now())
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
    /// Propagates the underlying [`AssetsError`](crate::error::AssetsError)
    /// when the on-disk flush fails.
    pub fn remove(&self, asset_root: &str) -> AssetsResult<bool> {
        let transitioned =
            if let Entry::Occupied(mut e) = self.inner.pins.entry(asset_root.to_string()) {
                if let Some(decremented) = NonZeroU32::new(e.get().get() - 1) {
                    *e.get_mut() = decremented;
                    false
                } else {
                    e.remove();
                    true
                }
            } else {
                false
            };
        if transitioned {
            flush_sync(&*self.inner)?;
        }
        Ok(transitioned)
    }

    /// Snapshot of currently pinned `asset_root`s (keys only — refcount stays internal).
    #[must_use]
    pub fn snapshot(&self) -> HashSet<String> {
        self.inner.pins.iter().map(|r| r.key().clone()).collect()
    }
}

impl Flushable for PinsInner {
    fn dirty(&self) -> &AtomicBool {
        &self.dirty
    }

    /// Serialise the current pinned set to `_index/pins.bin`. Refcount
    /// is collapsed to mere set-membership on disk — the persisted
    /// format only records keys.
    fn flush(&self) -> AssetsResult<()> {
        self.flush_with_durability(false)
    }

    fn flush_durable(&self) -> AssetsResult<()> {
        self.flush_with_durability(true)
    }

    fn name(&self) -> &'static str {
        "pins"
    }
}

#[cfg(target_arch = "wasm32")]
impl PinsInner {
    fn flush_with_durability(&self, _durable: bool) -> AssetsResult<()> {
        self.dirty
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::fs;

    use kithara_bufpool::BytePool;
    use kithara_platform::time::Duration;
    use kithara_test_utils::kithara;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn empty_index_has_no_pins() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = PinsIndex::with_persist_at(
            path.clone(),
            CancellationToken::new(),
            &BytePool::default(),
        );
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
        let idx = PinsIndex::with_persist_at(
            path.clone(),
            CancellationToken::new(),
            &BytePool::default(),
        );

        assert!(idx.add("asset_a").unwrap(), "first pin reports 0→1");
        assert!(path.exists(), "first add must materialise pins.bin");
        assert!(idx.add("asset_b").unwrap(), "fresh root reports 0→1");
        assert!(
            !idx.add("asset_a").unwrap(),
            "second add of same root reports refcount-only update"
        );
        assert!(idx.contains("asset_a"));
        assert_eq!(idx.snapshot().len(), 2);

        assert!(
            !idx.remove("asset_a").unwrap(),
            "decrement-only remove must not signal a 1→0 transition"
        );
        assert!(
            idx.contains("asset_a"),
            "still pinned by remaining refcount"
        );
        assert!(idx.remove("asset_a").unwrap());
        assert!(!idx.contains("asset_a"));
        assert!(!idx.remove("asset_a").unwrap());
        assert!(idx.contains("asset_b"));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn refcount_coalesces_repeated_pins() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = PinsIndex::with_persist_at(
            path.clone(),
            CancellationToken::new(),
            &BytePool::default(),
        );

        let transitions_in = (0..5).filter(|_| idx.add("hot_asset").unwrap()).count();
        assert_eq!(transitions_in, 1, "only the 0→1 transition counts");
        assert!(idx.contains("hot_asset"));

        let transitions_out = (0..4).filter(|_| idx.remove("hot_asset").unwrap()).count();
        assert_eq!(transitions_out, 0, "intermediate decrements stay in-memory");
        assert!(idx.contains("hot_asset"), "refcount=1 keeps the pin alive");

        assert!(idx.remove("hot_asset").unwrap());
        assert!(!idx.contains("hot_asset"));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn concurrent_leases_keep_pin_alive() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = PinsIndex::with_persist_at(path, CancellationToken::new(), &BytePool::default());

        idx.add("playlist").unwrap();
        idx.add("playlist").unwrap();

        assert!(!idx.remove("playlist").unwrap());
        assert!(idx.contains("playlist"));

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
                &BytePool::default(),
            );
            idx.add("persistent_asset").unwrap();
        }

        {
            let idx = PinsIndex::with_persist_at(
                path.clone(),
                CancellationToken::new(),
                &BytePool::default(),
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

        let idx = PinsIndex::with_persist_at(path, CancellationToken::new(), &BytePool::default());
        assert!(idx.snapshot().is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn ephemeral_index_does_not_persist() {
        let idx = PinsIndex::ephemeral();
        assert!(idx.add("asset").unwrap());
        assert!(idx.contains("asset"));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn clone_shares_state() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = PinsIndex::with_persist_at(path, CancellationToken::new(), &BytePool::default());
        let idx2 = idx.clone();

        idx.add("from_first").unwrap();
        assert!(idx2.contains("from_first"));

        idx2.remove("from_first").unwrap();
        assert!(!idx.contains("from_first"));
    }
}
