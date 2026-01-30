#![forbid(unsafe_code)]

//! Unified events for the full audio chain.

use std::time::Duration;

use kithara_audio::{AudioEvent, AudioPipelineEvent};
use kithara_decode::PcmSpec;

/// Unified events for the full audio chain.
///
/// Maps events from all layers (audio, file, HLS) into a single enum.
#[derive(Debug, Clone)]
pub enum ResourceEvent {
    // -- Audio events ---------------------------------------------------------
    /// Audio format detected on first decoded chunk.
    FormatDetected { spec: PcmSpec },
    /// Audio format changed (e.g. after ABR switch).
    FormatChanged { old: PcmSpec, new: PcmSpec },
    /// Seek completed.
    SeekComplete { position: Duration },
    /// End of stream reached.
    EndOfStream,

    // -- Common stream events (File + HLS) ------------------------------------
    /// Bytes downloaded.
    DownloadProgress { offset: u64, total: Option<u64> },
    /// Download completed.
    DownloadComplete { total_bytes: u64 },
    /// Download failed.
    DownloadError { error: String },
    /// Playback byte position.
    PlaybackProgress { position: u64, total: Option<u64> },
    /// Stream error.
    StreamError { error: String, recoverable: bool },

    // -- HLS-specific ---------------------------------------------------------
    /// HLS variants discovered from master playlist.
    #[cfg(feature = "hls")]
    VariantsDiscovered {
        variants: Vec<kithara_abr::VariantInfo>,
        initial_variant: usize,
    },
    /// HLS variant (quality level) changed.
    #[cfg(feature = "hls")]
    VariantApplied {
        from_variant: usize,
        to_variant: usize,
        reason: kithara_abr::AbrReason,
    },
    /// HLS segment download started.
    #[cfg(feature = "hls")]
    SegmentStart {
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
    },
    /// HLS segment download completed.
    #[cfg(feature = "hls")]
    SegmentComplete {
        variant: usize,
        segment_index: usize,
        bytes_transferred: u64,
        duration: Duration,
    },
    /// HLS throughput measurement.
    #[cfg(feature = "hls")]
    ThroughputSample { bytes_per_second: f64 },
}

// -- From conversions ---------------------------------------------------------

impl From<AudioEvent> for ResourceEvent {
    fn from(e: AudioEvent) -> Self {
        match e {
            AudioEvent::FormatDetected { spec } => Self::FormatDetected { spec },
            AudioEvent::FormatChanged { old, new } => Self::FormatChanged { old, new },
            AudioEvent::SeekComplete { position } => Self::SeekComplete { position },
            AudioEvent::EndOfStream => Self::EndOfStream,
        }
    }
}

#[cfg(feature = "file")]
impl From<kithara_file::FileEvent> for ResourceEvent {
    fn from(e: kithara_file::FileEvent) -> Self {
        match e {
            kithara_file::FileEvent::DownloadProgress { offset, total } => {
                Self::DownloadProgress { offset, total }
            }
            kithara_file::FileEvent::DownloadComplete { total_bytes } => {
                Self::DownloadComplete { total_bytes }
            }
            kithara_file::FileEvent::DownloadError { error } => Self::DownloadError { error },
            kithara_file::FileEvent::PlaybackProgress { position, total } => {
                Self::PlaybackProgress { position, total }
            }
            kithara_file::FileEvent::Error { error, recoverable } => {
                Self::StreamError { error, recoverable }
            }
            kithara_file::FileEvent::EndOfStream => Self::EndOfStream,
        }
    }
}

#[cfg(feature = "file")]
impl From<AudioPipelineEvent<kithara_file::FileEvent>> for ResourceEvent {
    fn from(e: AudioPipelineEvent<kithara_file::FileEvent>) -> Self {
        match e {
            AudioPipelineEvent::Stream(se) => se.into(),
            AudioPipelineEvent::Audio(de) => de.into(),
        }
    }
}

#[cfg(feature = "hls")]
impl From<kithara_hls::HlsEvent> for ResourceEvent {
    fn from(e: kithara_hls::HlsEvent) -> Self {
        match e {
            kithara_hls::HlsEvent::VariantsDiscovered {
                variants,
                initial_variant,
            } => Self::VariantsDiscovered {
                variants,
                initial_variant,
            },
            kithara_hls::HlsEvent::VariantApplied {
                from_variant,
                to_variant,
                reason,
            } => Self::VariantApplied {
                from_variant,
                to_variant,
                reason,
            },
            kithara_hls::HlsEvent::SegmentStart {
                variant,
                segment_index,
                byte_offset,
            } => Self::SegmentStart {
                variant,
                segment_index,
                byte_offset,
            },
            kithara_hls::HlsEvent::SegmentProgress { offset, .. } => Self::DownloadProgress {
                offset,
                total: None,
            },
            kithara_hls::HlsEvent::SegmentComplete {
                variant,
                segment_index,
                bytes_transferred,
                duration,
            } => Self::SegmentComplete {
                variant,
                segment_index,
                bytes_transferred,
                duration,
            },
            kithara_hls::HlsEvent::ThroughputSample { bytes_per_second } => {
                Self::ThroughputSample { bytes_per_second }
            }
            kithara_hls::HlsEvent::DownloadProgress { offset, total } => {
                Self::DownloadProgress { offset, total }
            }
            kithara_hls::HlsEvent::DownloadComplete { total_bytes } => {
                Self::DownloadComplete { total_bytes }
            }
            kithara_hls::HlsEvent::DownloadError { error } => Self::DownloadError { error },
            kithara_hls::HlsEvent::PlaybackProgress { position, total } => {
                Self::PlaybackProgress { position, total }
            }
            kithara_hls::HlsEvent::Error { error, recoverable } => {
                Self::StreamError { error, recoverable }
            }
            kithara_hls::HlsEvent::EndOfStream => Self::EndOfStream,
        }
    }
}

#[cfg(feature = "hls")]
impl From<AudioPipelineEvent<kithara_hls::HlsEvent>> for ResourceEvent {
    fn from(e: AudioPipelineEvent<kithara_hls::HlsEvent>) -> Self {
        match e {
            AudioPipelineEvent::Stream(se) => se.into(),
            AudioPipelineEvent::Audio(de) => de.into(),
        }
    }
}
