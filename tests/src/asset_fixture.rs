#![forbid(unsafe_code)]

use std::collections::HashSet;

use kithara_assets::{Assets, AssetsResult, BytePool, index::PinsIndex as InnerPinsIndex};
use kithara_platform::CancellationToken;

/// Test-only handle for interacting with the persisted pins index.
///
/// Wraps the in-memory + disk-backed [`InnerPinsIndex`] handle to expose
/// the historical `open() / load() / store()` API used by integration
/// tests. Each `open` call constructs a fresh disk-backed handle bound
/// to `<root_dir>/_index/pins.bin`.
pub struct PinsIndex {
    inner: InnerPinsIndex,
}

impl PinsIndex {
    /// Open `pins.bin` through the given base assets store. Hydrates
    /// initial state from disk; mutations through `store` flush
    /// immediately.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if the underlying index resource cannot be opened.
    pub fn open<A: Assets>(assets: &A, pool: &BytePool) -> AssetsResult<Self> {
        let path = assets.root_dir().join("_index").join("pins.bin");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Ok(Self {
            inner: InnerPinsIndex::with_persist_at(path, CancellationToken::default(), pool),
        })
    }

    /// Load current pinned-asset set.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if reading from storage fails.
    pub fn load(&self) -> AssetsResult<HashSet<String>> {
        Ok(self.inner.snapshot())
    }

    /// Persist pinned-asset set, replacing any existing state.
    ///
    /// # Errors
    ///
    /// Returns `AssetsError` if writing to storage fails.
    pub fn store(&self, pins: &HashSet<String>) -> AssetsResult<()> {
        let current = self.inner.snapshot();
        for outdated in current.difference(pins) {
            self.inner.remove(outdated)?;
        }
        for added in pins.difference(&current) {
            self.inner.add(added)?;
        }
        self.inner.flush()
    }
}
