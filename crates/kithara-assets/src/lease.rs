#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

/// In-memory lease table keyed by `asset_root`.
///
/// Semantics:
/// - `pin(asset_root)` increments the pin count for that asset and returns an RAII guard.
/// - Dropping the guard decrements the count.
/// - While an asset is pinned, eviction must not remove any of its files.
///
/// Notes:
/// - This is intentionally **runtime-only** and not persisted.
/// - Pinning is best-effort cleanup on drop (async unpin).
#[derive(Default)]
pub(crate) struct LeaseTable {
    pins: Mutex<HashMap<String, u64>>,
}

impl LeaseTable {
    /// Create a new empty lease table.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Pin an asset and return an RAII guard.
    pub(crate) async fn pin(self: &Arc<Self>, asset_root: &str) -> LeaseGuard {
        let mut pins = self.pins.lock().await;
        let n = pins.entry(asset_root.to_string()).or_insert(0);
        *n = n.saturating_add(1);

        LeaseGuard {
            table: Arc::clone(self),
            asset_root: asset_root.to_string(),
        }
    }

    /// Return current pin count for an asset (debug/introspection).
    pub(crate) async fn pin_count(&self, asset_root: &str) -> u64 {
        let pins = self.pins.lock().await;
        pins.get(asset_root).copied().unwrap_or(0)
    }

    /// Whether an asset is pinned (debug/introspection).
    pub(crate) async fn is_pinned(&self, asset_root: &str) -> bool {
        self.pin_count(asset_root).await > 0
    }

    async fn unpin(&self, asset_root: &str) {
        let mut pins = self.pins.lock().await;
        match pins.get_mut(asset_root) {
            Some(n) if *n > 1 => *n -= 1,
            Some(_) => {
                pins.remove(asset_root);
            }
            None => {}
        }
    }
}

/// RAII guard for a pin/lease.
///
/// Dropping the guard releases the lease (decrements pin count).
#[derive(Clone)]
pub(crate) struct LeaseGuard {
    table: Arc<LeaseTable>,
    asset_root: String,
}

impl LeaseGuard {
    /// Asset root associated with this lease.
    pub(crate) fn asset_root(&self) -> &str {
        &self.asset_root
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        // Best-effort async cleanup; callers should not rely on immediate unpin.
        let table = Arc::clone(&self.table);
        let asset_root = self.asset_root.clone();

        tokio::spawn(async move {
            table.unpin(&asset_root).await;
        });
    }
}
