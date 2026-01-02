use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EventError {
    #[error("Event channel closed")]
    ChannelClosed,

    #[error("Invalid event data")]
    InvalidData,
}

#[derive(Clone, Debug)]
pub enum HlsEvent {
    VariantChanged {
        old_variant: usize,
        new_variant: usize,
        reason: VariantChangeReason,
    },

    SegmentStart {
        variant: usize,
        segment_index: usize,
        byte_offset: u64,
    },

    SegmentComplete {
        variant: usize,
        segment_index: usize,
        bytes_transferred: u64,
        duration: Duration,
    },

    KeyFetch {
        key_url: String,
        success: bool,
        cached: bool,
    },

    BufferLevel {
        level_seconds: f32,
    },

    ThroughputSample {
        bytes_per_second: f64,
    },

    Error {
        error: String,
        recoverable: bool,
    },

    EndOfStream,
}

#[derive(Clone, Debug)]
pub enum VariantChangeReason {
    Manual,
    AbrUpSwitch,
    AbrDownSwitch,
    ErrorRecovery,
}

pub struct EventEmitter {
    // In real implementation, this would hold a channel or similar
    // For now, placeholder structure
}

impl EventEmitter {
    pub fn new() -> Self {
        Self {}
    }

    pub fn emit_variant_changed(&self, old: usize, new: usize, reason: VariantChangeReason) {
        // In real implementation, this would send to a channel
        tracing::debug!(
            "Variant changed from {} to {} (reason: {:?}",
            old,
            new,
            reason
        );
    }

    pub fn emit_segment_start(&self, variant: usize, segment: usize, offset: u64) {
        tracing::debug!(
            "Segment start: variant {}, segment {}, offset {}",
            variant,
            segment,
            offset
        );
    }

    pub fn emit_segment_complete(
        &self,
        variant: usize,
        segment: usize,
        bytes: u64,
        duration: Duration,
    ) {
        tracing::debug!(
            "Segment complete: variant {}, segment {}, {} bytes in {:?}",
            variant,
            segment,
            bytes,
            duration
        );
    }

    pub fn emit_key_fetch(&self, key_url: &str, success: bool, cached: bool) {
        tracing::debug!(
            "Key fetch: {} (success: {}, cached: {})",
            key_url,
            success,
            cached
        );
    }

    pub fn emit_buffer_level(&self, level: f32) {
        tracing::debug!("Buffer level: {:.2}s", level);
    }

    pub fn emit_throughput_sample(&self, bps: f64) {
        tracing::debug!("Throughput: {:.2} bytes/s", bps);
    }

    pub fn emit_error(&self, error: &str, recoverable: bool) {
        tracing::error!("HLS error: {} (recoverable: {})", error, recoverable);
    }

    pub fn emit_end_of_stream(&self) {
        tracing::debug!("End of stream");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_creation() {
        let event = HlsEvent::VariantChanged {
            old_variant: 0,
            new_variant: 1,
            reason: VariantChangeReason::AbrUpSwitch,
        };

        match event {
            HlsEvent::VariantChanged {
                old_variant,
                new_variant,
                reason,
            } => {
                assert_eq!(old_variant, 0);
                assert_eq!(new_variant, 1);
                assert!(matches!(reason, VariantChangeReason::AbrUpSwitch));
            }
            _ => panic!("Unexpected event type"),
        }
    }

    #[test]
    fn test_event_emitter() {
        let emitter = EventEmitter::new();

        // These should not panic
        emitter.emit_variant_changed(0, 1, VariantChangeReason::Manual);
        emitter.emit_segment_start(1, 5, 1000);
        emitter.emit_segment_complete(1, 5, 5000, Duration::from_secs(2));
        emitter.emit_key_fetch("http://example.com/key.bin", true, false);
        emitter.emit_buffer_level(15.5);
        emitter.emit_throughput_sample(1000000.0);
        emitter.emit_error("Test error", true);
        emitter.emit_end_of_stream();
    }

    #[test]
    fn test_variant_change_reasons() {
        let reasons = vec![
            VariantChangeReason::Manual,
            VariantChangeReason::AbrUpSwitch,
            VariantChangeReason::AbrDownSwitch,
            VariantChangeReason::ErrorRecovery,
        ];

        for reason in reasons {
            // Verify all reasons can be cloned and debug printed
            let _cloned = reason.clone();
            let _debug = format!("{:?}", reason);
        }
    }
}
