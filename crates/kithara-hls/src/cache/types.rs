//! Cache module types.

use std::time::Duration;

use kithara_stream::ContainerFormat;
use url::Url;

use crate::playlist::SegmentKey;

/// Segment type: initialization segment or media segment with index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentType {
    /// Initialization segment (fMP4 only, contains codec metadata).
    Init,
    /// Media segment with index in the playlist.
    Media(usize),
}

impl SegmentType {
    /// Get media segment index, or None for init segment.
    pub fn media_index(self) -> Option<usize> {
        match self {
            SegmentType::Media(idx) => Some(idx),
            SegmentType::Init => None,
        }
    }

    /// Check if this is an init segment.
    pub fn is_init(self) -> bool {
        matches!(self, SegmentType::Init)
    }
}

/// Encryption info for a segment (resolved key URL and IV).
/// Also used as context for decryption callback in `AssetStore`.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct EncryptionInfo {
    pub key_url: Url,
    pub iv: [u8; 16],
}

impl Default for EncryptionInfo {
    fn default() -> Self {
        Self {
            key_url: Url::parse("http://localhost/dummy").expect("valid dummy URL"),
            iv: [0u8; 16],
        }
    }
}

/// Segment metadata (data is on disk, not in memory).
#[derive(Debug, Clone)]
pub struct SegmentMeta {
    pub variant: usize,
    pub segment_type: SegmentType,
    pub sequence: u64,
    pub url: Url,
    pub duration: Option<Duration>,
    pub key: Option<SegmentKey>,
    /// Segment size in bytes.
    pub len: u64,
    /// Container format (fMP4, MPEG-TS, etc).
    pub container: Option<ContainerFormat>,
}
