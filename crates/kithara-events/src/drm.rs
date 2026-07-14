#![forbid(unsafe_code)]

/// Stage where a DRM key fetch failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyFailureStage {
    Network,
    BodyCollect,
    Processor,
    Missing,
}

/// Source that produced the final DRM key bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeySource {
    Network,
    DiskCache,
    MemCache,
}

/// Events emitted during DRM key fetch / decrypt lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DrmEvent {
    KeyFetchFailed {
        key_host: Option<String>,
        stage: KeyFailureStage,
        detail: String,
    },
    KeyAcquired {
        key_host: Option<String>,
        source: KeySource,
        bytes: usize,
        latency_ms: Option<u64>,
    },
    SegmentDecryptFailed {
        variant: u32,
        segment_index: u32,
        detail: String,
    },
}
