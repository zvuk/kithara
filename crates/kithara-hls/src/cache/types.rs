//! Cache module types.

use std::time::Duration;

use url::Url;

use crate::{parsing::ContainerFormat, playlist::SegmentKey};

/// Encryption info for a segment (resolved key URL and IV).
/// Also used as context for decryption callback in `AssetStore`.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct EncryptionInfo {
    pub key_url: Url,
    pub iv: [u8; 16],
}

/// Segment metadata (data is on disk, not in memory).
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub variant: usize,
    pub segment_index: usize,
    pub sequence: u64,
    pub url: Url,
    pub duration: Option<Duration>,
    pub key: Option<SegmentKey>,
    /// Segment size in bytes.
    pub len: u64,
    /// Container format (fMP4, MPEG-TS, etc).
    pub container: Option<ContainerFormat>,
}
