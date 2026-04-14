#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use rkyv::{Archive, Deserialize, Serialize};

/// Persisted representation of the LRU index.
#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
#[archive(check_bytes)]
pub(crate) struct LruIndexFile {
    pub(crate) version: u32,
    pub(crate) clock: u64,
    pub(crate) entries: BTreeMap<String, LruEntryFile>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[archive(check_bytes)]
pub(crate) struct LruEntryFile {
    pub(crate) last_touch: u64,
    pub(crate) bytes: Option<u64>,
}

/// Persisted representation of the pins index.
#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
#[archive(check_bytes)]
pub struct PinsIndexFile {
    pub version: u32,
    /// Store as a map for stable serialization and fast zero-copy lookups.
    /// The value is true if pinned.
    pub pinned: BTreeMap<String, bool>,
}

/// Persisted representation of the aggregate availability index.
#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
#[archive(check_bytes)]
pub(crate) struct AvailabilityFile {
    pub(crate) version: u32,
    /// Maps `asset_root` to its availability metadata.
    pub(crate) assets: BTreeMap<String, AssetAvailabilityFile>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
#[archive(check_bytes)]
pub(crate) struct AssetAvailabilityFile {
    /// Maps `ResourceKey::Relative` string to its availability ranges.
    pub(crate) resources: BTreeMap<String, ResourceAvailabilityFile>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
#[archive(check_bytes)]
pub(crate) struct ResourceAvailabilityFile {
    pub(crate) ranges: Vec<(u64, u64)>,
    pub(crate) final_len: Option<u64>,
    pub(crate) committed: bool,
}
