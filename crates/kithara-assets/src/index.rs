#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Pin index data persisted by the `LeaseAssets` decorator.
///
/// ## Normative
/// - This file is best-effort metadata; the filesystem is the source of truth.
/// - `pinned` is a *set* of pinned asset roots (no refcounts).
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AssetIndex {
    pub version: u32,
    pub resources: Vec<AssetIndexEntry>,
    /// Pinned asset roots.
    ///
    /// Stored as a list for stable JSON, but treated as a set by higher layers.
    pub pinned: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetIndexEntry {
    pub asset_root: String,
    pub rel_path: String,
    pub status: ResourceStatus,
    pub final_len: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResourceStatus {
    InProgress,
    Ready,
    Failed,
}
