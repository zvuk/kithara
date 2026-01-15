//! HLS events for monitoring and UI integration.

use std::time::Duration;

use crate::abr::AbrReason;

/// Events emitted during HLS playback.
#[derive(Clone, Debug)]
pub enum HlsEvent {
    /// Variant (quality level) changed.
    VariantApplied {
        from_variant: usize,
        to_variant: usize,
        reason: AbrReason,
    },
    /// Segment download started.
    SegmentStart {
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
    },
    /// Segment download completed.
    SegmentComplete {
        variant: usize,
        segment_index: usize,
        bytes_transferred: u64,
        duration: Duration,
    },
    /// Encryption key fetched.
    KeyFetch {
        key_url: String,
        success: bool,
        cached: bool,
    },
    /// Buffer level update.
    BufferLevel { level_seconds: f32 },
    /// Throughput measurement.
    ThroughputSample { bytes_per_second: f64 },
    /// Download progress update.
    DownloadProgress { offset: u64, percent: Option<f32> },
    /// Playback progress update.
    PlaybackProgress { position: u64, percent: Option<f32> },
    /// Error occurred.
    Error { error: String, recoverable: bool },
    /// Stream ended.
    EndOfStream,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_creation() {
        let event = HlsEvent::VariantApplied {
            from_variant: 0,
            to_variant: 1,
            reason: AbrReason::UpSwitch,
        };

        match event {
            HlsEvent::VariantApplied {
                from_variant,
                to_variant,
                reason,
            } => {
                assert_eq!(from_variant, 0);
                assert_eq!(to_variant, 1);
                assert!(matches!(reason, AbrReason::UpSwitch));
            }
            _ => panic!("Unexpected event type"),
        }
    }

    #[test]
    fn test_broadcast_channel() {
        use tokio::sync::broadcast;

        let (tx, mut rx) = broadcast::channel::<HlsEvent>(32);
        let _ = tx.send(HlsEvent::EndOfStream);

        let event = rx.try_recv().ok();
        assert!(matches!(event, Some(HlsEvent::EndOfStream)));
    }
}
