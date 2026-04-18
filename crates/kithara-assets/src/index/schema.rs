#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use rkyv::{Archive, Deserialize, Serialize};

/// Persisted representation of the LRU index.
#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
pub struct LruIndexFile {
    pub version: u32,
    pub clock: u64,
    pub entries: BTreeMap<String, LruEntryFile>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct LruEntryFile {
    pub last_touch: u64,
    pub bytes: Option<u64>,
}

/// Persisted representation of the pins index.
#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
pub struct PinsIndexFile {
    pub version: u32,
    /// Store as a map for stable serialization and fast zero-copy lookups.
    /// The value is true if pinned.
    pub pinned: BTreeMap<String, bool>,
}

/// Persisted representation of the aggregate availability index.
#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
pub struct AvailabilityFile {
    pub version: u32,
    /// Maps `asset_root` to its availability metadata.
    pub assets: BTreeMap<String, AssetAvailabilityFile>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Default, Clone)]
pub struct AssetAvailabilityFile {
    /// Maps `ResourceKey::Relative` string to its availability ranges.
    pub resources: BTreeMap<String, ResourceAvailabilityFile>,
}

#[derive(Archive, Deserialize, Serialize, Debug, Clone)]
pub struct ResourceAvailabilityFile {
    pub ranges: Vec<(u64, u64)>,
    pub final_len: Option<u64>,
    pub committed: bool,
}
