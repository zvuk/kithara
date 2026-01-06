#![forbid(unsafe_code)]

/// Observability events emitted by `kithara-file` streams.
///
/// These are designed to be carried inside `StreamMsg::Event` so applications
/// can subscribe and react (e.g., log, display UI progress).
#[derive(Debug, Clone, PartialEq)]
pub enum FileEvent {
    /// Bytes written to local storage by the downloader.
    ///
    /// Carries the cumulative number of bytes successfully persisted and an optional percent of total.
    DownloadProgress { offset: u64, percent: Option<f32> },

    /// Bytes consumed by the playback reader.
    ///
    /// Carries the cumulative byte position that has been read/played and an optional percent of total.
    PlaybackProgress { position: u64, percent: Option<f32> },
}
