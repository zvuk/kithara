#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use rkyv::{Archive, Deserialize, Serialize};

/// Persisted representation of the LRU index.
#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
pub struct LruIndexFile {
    pub entries: BTreeMap<String, LruEntryFile>,
    pub version: u32,
    pub clock: u64,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct LruEntryFile {
    pub bytes: Option<u64>,
    pub last_touch: u64,
}

/// Persisted representation of the pins index.
#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
pub struct PinsIndexFile {
    /// Store as a map for stable serialization and fast zero-copy lookups.
    /// The value is true if pinned.
    pub pinned: BTreeMap<String, bool>,
    pub version: u32,
}

/// Persisted representation of the aggregate availability index.
#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
pub struct AvailabilityFile {
    /// Maps `asset_root` to its availability metadata.
    pub assets: BTreeMap<String, AssetAvailabilityFile>,
    pub version: u32,
}

#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
pub struct AssetAvailabilityFile {
    /// Maps `ResourceKey::Relative` string to its availability ranges.
    pub resources: BTreeMap<String, ResourceAvailabilityFile>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ResourceAvailabilityFile {
    pub final_len: Option<u64>,
    pub ranges: Vec<(u64, u64)>,
    pub committed: bool,
}
