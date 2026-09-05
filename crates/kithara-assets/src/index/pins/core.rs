#![forbid(unsafe_code)]

#[cfg(target_arch = "wasm32")]
use std::sync::atomic::Ordering;
use std::{
    collections::HashSet,
    sync::{OnceLock, Weak, atomic::AtomicBool},
};

use dashmap::{DashMap, mapref::entry::Entry};
use kithara_platform::sync::Arc;

use crate::{
    error::AssetsResult,
    index::persistence::{FlushHub, Flushable, flush_sync},
};

/// How long a pin has to outlive its holder.
///
/// Both kinds bar eviction identically while they are held; they differ
/// only in whether the pin reaches `_index/pins.bin`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinDurability {
    /// Outlives the process: an unfinished write whose bytes a later run
    /// must not evict before the pending resource becomes ready. Persisted.
    Durable,
    /// Bounded by the process: a reader holding committed bytes open.
    /// In-memory only — a pin that cannot survive a restart has nothing
    /// to say to the next one.
    Local,
}

/// Per-root pin refcounts, split by what each pin has to outlive.
///
/// Fields are open to `disk`, which picks the persisted subset by `durable`
/// and seeds hydrated roots.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PinCounts {
    pub(super) durable: u32,
    pub(super) local: u32,
}

impl PinCounts {
    const fn is_pinned(self) -> bool {
        self.durable > 0 || self.local > 0
    }

    /// Add one pin; `true` when the durable count went 0→1.
    const fn pin(&mut self, durability: PinDurability) -> bool {
        match durability {
            PinDurability::Durable => {
                self.durable = self.durable.saturating_add(1);
                self.durable == 1
            }
            PinDurability::Local => {
                self.local = self.local.saturating_add(1);
                false
            }
        }
    }

    /// Drop one pin; `true` when the durable count went 1→0.
    const fn unpin(&mut self, durability: PinDurability) -> bool {
        match durability {
            PinDurability::Durable => {
                let held = self.durable;
                self.durable = held.saturating_sub(1);
                held == 1
            }
            PinDurability::Local => {
                self.local = self.local.saturating_sub(1);
                false
            }
        }
    }
}

/// In-memory + best-effort disk-backed index of pinned `asset_root`s.
///
/// Refcounted per root, lazily persisted. Only durable pins reach disk,
/// and only on their 0→1 / 1→0 transitions. See the crate `CONTEXT.md`
/// "Pins index" for the contract.
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
    pub(super) pins: DashMap<String, PinCounts>,
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
    /// Disk-backed instances flush the snapshot synchronously when the
    /// **durable** count goes 0→1; every other increment, and every
    /// [`PinDurability::Local`] pin, stays in memory. An `Err` here means the
    /// pin is **not** durable. Ephemeral instances cannot fail.
    ///
    /// # Errors
    ///
    /// Propagates the underlying [`AssetsError`](crate::error::AssetsError)
    /// when the on-disk flush fails (mmap open, atomic swap, rkyv serialise).
    pub fn add(&self, asset_root: &str, durability: PinDurability) -> AssetsResult<bool> {
        let (newly_pinned, persisted_set_changed) =
            match self.inner.pins.entry(asset_root.to_string()) {
                Entry::Occupied(mut e) => (false, e.get_mut().pin(durability)),
                Entry::Vacant(e) => {
                    let mut counts = PinCounts::default();
                    let changed = counts.pin(durability);
                    e.insert(counts);
                    (true, changed)
                }
            };
        if persisted_set_changed {
            flush_sync(&*self.inner)?;
        }
        Ok(newly_pinned)
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
    /// Same durability contract as [`Self::add`]: the flush fires when the
    /// durable count goes 1→0, not when the last reader lets go.
    ///
    /// # Errors
    ///
    /// Propagates the underlying [`AssetsError`](crate::error::AssetsError)
    /// when the on-disk flush fails.
    pub fn remove(&self, asset_root: &str, durability: PinDurability) -> AssetsResult<bool> {
        let mut persisted_set_changed = false;
        let fully_unpinned =
            if let Entry::Occupied(mut e) = self.inner.pins.entry(asset_root.to_string()) {
                persisted_set_changed = e.get_mut().unpin(durability);
                if e.get().is_pinned() {
                    false
                } else {
                    e.remove();
                    true
                }
            } else {
                false
            };
        if persisted_set_changed {
            flush_sync(&*self.inner)?;
        }
        Ok(fully_unpinned)
    }

    /// Snapshot of currently pinned `asset_root`s (keys only — refcounts stay
    /// internal). Covers both durabilities: eviction must respect a reader's
    /// pin exactly as it respects a writer's.
    #[must_use]
    pub fn snapshot(&self) -> HashSet<String> {
        self.inner.pins.iter().map(|r| r.key().clone()).collect()
    }
}

impl Flushable for PinsInner {
    fn dirty(&self) -> &AtomicBool {
        &self.dirty
    }

    /// Serialise the durable pinned set to `_index/pins.bin`. Refcount
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
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use std::fs;

    use kithara_platform::{CancelToken, time::Duration};
    use kithara_test_utils::kithara;
    use tempfile::tempdir;

    use super::*;

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn empty_index_has_no_pins() {
        let pools = crate::test_pools::pools();
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = PinsIndex::with_persist_at(
            path.clone(),
            CancelToken::never(),
            crate::test_pools::byte_buffer(&pools),
        );
        assert!(idx.snapshot().is_empty());
        assert!(
            !path.exists(),
            "fresh disk-backed pins index must not materialise the file before any pin"
        );
    }

    fn disk_index(path: &std::path::Path) -> PinsIndex {
        let pools = crate::test_pools::pools();
        PinsIndex::with_persist_at(
            path.to_path_buf(),
            CancelToken::never(),
            crate::test_pools::byte_buffer(&pools),
        )
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn durable_pin_persists_immediately() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = disk_index(&path);

        idx.add("asset_a", PinDurability::Durable).unwrap();

        assert!(path.exists(), "a durable pin must materialise pins.bin");
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn durable_unpin_persists_immediately() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = disk_index(&path);
        idx.add("asset_a", PinDurability::Durable).unwrap();

        idx.remove("asset_a", PinDurability::Durable).unwrap();

        assert!(
            !disk_index(&path).contains("asset_a"),
            "dropping the last durable pin must reach disk at once"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn local_pin_stays_off_disk() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = disk_index(&path);

        idx.add("asset_a", PinDurability::Local).unwrap();

        assert!(
            !path.exists(),
            "a pin that cannot outlive the process must not be written"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn local_pin_bars_eviction_like_any_other() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = disk_index(&path);

        idx.add("asset_a", PinDurability::Local).unwrap();

        assert!(
            idx.snapshot().contains("asset_a"),
            "eviction reads the in-memory set, which covers local pins"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn a_local_pin_alongside_a_durable_one_leaves_the_file_alone() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = disk_index(&path);
        idx.add("asset_a", PinDurability::Durable).unwrap();
        let written = fs::read(&path).unwrap();

        idx.add("asset_a", PinDurability::Local).unwrap();
        idx.remove("asset_a", PinDurability::Local).unwrap();

        assert_eq!(
            fs::read(&path).unwrap(),
            written,
            "local pin churn must not rewrite the durable set"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn a_live_local_pin_survives_the_durable_one() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = disk_index(&path);
        idx.add("asset_a", PinDurability::Durable).unwrap();
        idx.add("asset_a", PinDurability::Local).unwrap();

        idx.remove("asset_a", PinDurability::Durable).unwrap();

        assert!(
            idx.contains("asset_a"),
            "the reader still holds the asset even once the writer is done"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn refcount_coalesces_repeated_pins() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = disk_index(&path);

        let transitions_in = (0..5)
            .filter(|_| idx.add("hot_asset", PinDurability::Durable).unwrap())
            .count();
        assert_eq!(transitions_in, 1, "only the 0→1 transition counts");

        let transitions_out = (0..4)
            .filter(|_| idx.remove("hot_asset", PinDurability::Durable).unwrap())
            .count();
        assert_eq!(transitions_out, 0, "intermediate decrements stay in-memory");
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn concurrent_leases_keep_pin_alive() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = disk_index(&path);

        idx.add("playlist", PinDurability::Durable).unwrap();
        idx.add("playlist", PinDurability::Durable).unwrap();

        assert!(!idx.remove("playlist", PinDurability::Durable).unwrap());
        assert!(idx.contains("playlist"));

        assert!(idx.remove("playlist", PinDurability::Durable).unwrap());
        assert!(!idx.contains("playlist"));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn durable_pins_persist_across_instances() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("shared.bin");
        disk_index(&path)
            .add("persistent_asset", PinDurability::Durable)
            .unwrap();

        let reopened = disk_index(&path);

        assert!(reopened.contains("persistent_asset"));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn local_pins_do_not_cross_instances() {
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("shared.bin");
        disk_index(&path)
            .add("reader_only", PinDurability::Local)
            .unwrap();

        let reopened = disk_index(&path);

        assert!(
            !reopened.contains("reader_only"),
            "a process-local pin must not be resurrected as a phantom"
        );
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn invalid_data_is_treated_as_empty() {
        let pools = crate::test_pools::pools();
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("invalid.bin");
        fs::write(&path, b"not valid rkyv data").unwrap();

        let idx = PinsIndex::with_persist_at(
            path,
            CancelToken::never(),
            crate::test_pools::byte_buffer(&pools),
        );
        assert!(idx.snapshot().is_empty());
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn ephemeral_index_does_not_persist() {
        let idx = PinsIndex::ephemeral();
        assert!(idx.add("asset", PinDurability::Durable).unwrap());
        assert!(idx.contains("asset"));
    }

    #[kithara::test(timeout(Duration::from_secs(1)))]
    fn clone_shares_state() {
        let pools = crate::test_pools::pools();
        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = PinsIndex::with_persist_at(
            path,
            CancelToken::never(),
            crate::test_pools::byte_buffer(&pools),
        );
        let idx2 = idx.clone();

        idx.add("from_first", PinDurability::Durable).unwrap();
        assert!(idx2.contains("from_first"));

        idx2.remove("from_first", PinDurability::Durable).unwrap();
        assert!(!idx.contains("from_first"));
    }

    /// Membership as `pins.bin` holds it right now, read straight off the
    /// file — the cheap check the stress loop below needs thousands of
    /// times, without opening a second index per round.
    fn on_disk_contains(path: &std::path::Path, root: &str) -> bool {
        let Ok(bytes) = fs::read(path) else {
            return false;
        };
        if bytes.is_empty() {
            return false;
        }
        rkyv::access::<crate::index::schema::ArchivedPinsIndexFile, rkyv::rancor::Error>(&bytes)
            .is_ok_and(|archived| {
                archived
                    .pinned
                    .iter()
                    .filter(|(_, v)| **v)
                    .any(|(k, _)| k.as_str() == root)
            })
    }

    /// Two flushes of one `pins.bin` can overlap: the eager mutator path
    /// and a hub flush from the worker or a checkpoint. Whoever renames
    /// last decides the file, so a flush that snapshotted before an unpin
    /// must never publish after it. Measured with the per-file write lock
    /// removed: 3 to 8 resurrections in 200 rounds.
    ///
    /// The barrier releases each flusher into the round it races instead of
    /// letting it spin. Overlap is what the property needs; a free-running
    /// flusher buys it with `fsync`s it issues faster the longer a round
    /// takes, so its cost climbs with the load it is already competing with
    /// — 500 spun rounds took 3.8 s to 16.5 s idle, 200 gated ones 4.2 s to
    /// 5.3 s.
    #[kithara::test(timeout(Duration::from_secs(60)))]
    fn an_eager_unpin_is_never_resurrected_by_a_concurrent_flush() {
        use std::sync::Barrier;

        use crate::index::persistence::{FlushHub, FlushPolicy};

        const ROUNDS: usize = 200;
        const FLUSHERS: usize = 3;

        let temp_dir = tempdir().unwrap();
        let path = temp_dir.path().join("pins.bin");
        let idx = disk_index(&path);
        let hub = FlushHub::new(CancelToken::never(), FlushPolicy::default());
        idx.attach_to(&hub);

        let round = Arc::new(Barrier::new(FLUSHERS + 1));
        let flushers: Vec<_> = (0..FLUSHERS)
            .map(|n| {
                let hub = Arc::clone(&hub);
                let round = Arc::clone(&round);
                kithara_platform::thread::spawn_named(format!("probe-flusher-{n}"), move || {
                    for _ in 0..ROUNDS {
                        round.wait();
                        let _ = hub.flush_now();
                    }
                })
            })
            .collect();

        let mut violations = 0_usize;
        for _ in 0..ROUNDS {
            round.wait();
            idx.add("asset-a", PinDurability::Durable).unwrap();
            idx.remove("asset-a", PinDurability::Durable).unwrap();
            if on_disk_contains(&path, "asset-a") {
                violations += 1;
            }
        }
        for flusher in flushers {
            flusher.join().unwrap();
        }

        assert_eq!(
            violations, 0,
            "an eager unpin must not be resurrected on disk by a concurrent flush ({violations}/{ROUNDS})"
        );
    }
}
