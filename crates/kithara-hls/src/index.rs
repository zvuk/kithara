//! Segment index for random-access over HLS streams.
//!
//! Maps global byte offsets to individual segment files.

use std::cmp::Ordering;

use url::Url;

/// Encryption info for a segment (resolved key URL and IV).
#[derive(Debug, Clone)]
pub(crate) struct EncryptionInfo {
    pub key_url: Url,
    pub iv: [u8; 16],
}

/// Entry in segment index: maps global byte range to segment file.
#[derive(Debug, Clone)]
pub(crate) struct SegmentEntry {
    pub global_start: u64,
    pub global_end: u64,
    pub url: Url,
    pub encryption: Option<EncryptionInfo>,
}

/// Segment index state for random-access over HLS stream.
pub(crate) struct SegmentIndex {
    segments: Vec<SegmentEntry>,
    total_len: u64,
    finished: bool,
    error: Option<String>,
}

impl SegmentIndex {
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            total_len: 0,
            finished: false,
            error: None,
        }
    }

    pub fn add(&mut self, url: Url, len: u64, encryption: Option<EncryptionInfo>) {
        let global_start = self.total_len;
        let global_end = global_start + len;
        self.segments.push(SegmentEntry {
            global_start,
            global_end,
            url,
            encryption,
        });
        self.total_len = global_end;
    }

    /// Find segment containing the given byte offset using binary search.
    pub fn find(&self, offset: u64) -> Option<&SegmentEntry> {
        self.segments
            .binary_search_by(|s| {
                if offset < s.global_start {
                    Ordering::Greater
                } else if offset >= s.global_end {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .ok()
            .map(|i| &self.segments[i])
    }

    pub fn total_len(&self) -> u64 {
        self.total_len
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn set_finished(&mut self) {
        self.finished = true;
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
        self.finished = true;
    }
}
