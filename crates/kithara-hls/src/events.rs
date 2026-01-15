//! HLS events for monitoring and UI integration.

use std::time::Duration;

use tokio::sync::broadcast;

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

/// Event emitter for broadcasting HLS events.
#[derive(Clone)]
pub struct EventEmitter {
    tx: broadcast::Sender<HlsEvent>,
}

impl Default for EventEmitter {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter {
    /// Create a new event emitter.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(32);
        Self { tx }
    }

    /// Subscribe to events.
    pub fn subscribe(&self) -> broadcast::Receiver<HlsEvent> {
        self.tx.subscribe()
    }

    /// Emit an event.
    pub fn emit(&self, event: HlsEvent) {
        let _ = self.tx.send(event);
    }

    // Convenience methods (call emit internally)

    pub fn emit_variant_applied(&self, from: usize, to: usize, reason: AbrReason) {
        self.emit(HlsEvent::VariantApplied {
            from_variant: from,
            to_variant: to,
            reason,
        });
    }

    pub fn emit_segment_start(&self, variant: usize, segment: usize, offset: u64) {
        self.emit(HlsEvent::SegmentStart {
            variant,
            segment_index: segment,
            byte_offset: offset,
        });
    }

    pub fn emit_end_of_stream(&self) {
        self.emit(HlsEvent::EndOfStream);
    }
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
    fn test_subscribe() {
        let emitter = EventEmitter::new();
        let mut rx = emitter.subscribe();
        emitter.emit(HlsEvent::EndOfStream);

        let event = rx.try_recv().ok();
        assert!(matches!(event, Some(HlsEvent::EndOfStream)));
    }

    #[test]
    fn test_emit_variant_applied() {
        let emitter = EventEmitter::new();
        let mut rx = emitter.subscribe();
        emitter.emit_variant_applied(0, 1, AbrReason::UpSwitch);

        let event = rx.try_recv().ok();
        assert!(matches!(
            event,
            Some(HlsEvent::VariantApplied {
                from_variant: 0,
                to_variant: 1,
                ..
            })
        ));
    }
}
