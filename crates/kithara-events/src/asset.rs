#![forbid(unsafe_code)]

/// Reason an asset was evicted from the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvictReason {
    QuotaBytes,
    QuotaAssets,
    Displaced,
}

/// Events emitted by the asset cache lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssetEvent {
    Committed {
        asset_root: String,
        rel_path: String,
        final_len: Option<u64>,
    },
    Failed {
        asset_root: String,
        rel_path: String,
        reason: String,
    },
    Evicted {
        asset_root: String,
        reason: EvictReason,
    },
}
