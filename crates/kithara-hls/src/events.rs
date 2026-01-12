use std::time::Duration;

use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub enum HlsEvent {
    VariantDecision {
        from_variant: usize,
        to_variant: usize,
        reason: crate::abr::AbrReason,
    },
    VariantApplied {
        from_variant: usize,
        to_variant: usize,
        reason: crate::abr::AbrReason,
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
    DownloadProgress {
        offset: u64,
        percent: Option<f32>,
    },
    PlaybackProgress {
        position: u64,
        percent: Option<f32>,
    },
    Error {
        error: String,
        recoverable: bool,
    },
    EndOfStream,
}

#[derive(Clone)]
pub struct EventEmitter {
    tx: broadcast::Sender<HlsEvent>,
}

impl EventEmitter {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(128);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HlsEvent> {
        self.tx.subscribe()
    }

    pub fn emit_variant_decision(&self, from: usize, to: usize, reason: crate::abr::AbrReason) {
        let _ = self.tx.send(HlsEvent::VariantDecision {
            from_variant: from,
            to_variant: to,
            reason,
        });
    }

    pub fn emit_variant_applied(&self, from: usize, to: usize, reason: crate::abr::AbrReason) {
        let _ = self.tx.send(HlsEvent::VariantApplied {
            from_variant: from,
            to_variant: to,
            reason,
        });
    }

    pub fn emit_segment_start(&self, variant: usize, segment: usize, offset: u64) {
        let _ = self.tx.send(HlsEvent::SegmentStart {
            variant,
            segment_index: segment,
            byte_offset: offset,
        });
    }

    pub fn emit_segment_complete(
        &self,
        variant: usize,
        segment: usize,
        bytes: u64,
        duration: Duration,
    ) {
        let _ = self.tx.send(HlsEvent::SegmentComplete {
            variant,
            segment_index: segment,
            bytes_transferred: bytes,
            duration,
        });
    }

    pub fn emit_key_fetch(&self, key_url: &str, success: bool, cached: bool) {
        let _ = self.tx.send(HlsEvent::KeyFetch {
            key_url: key_url.to_string(),
            success,
            cached,
        });
    }

    pub fn emit_buffer_level(&self, level: f32) {
        let _ = self.tx.send(HlsEvent::BufferLevel {
            level_seconds: level,
        });
    }

    pub fn emit_throughput_sample(&self, bytes_per_second: f64) {
        let _ = self
            .tx
            .send(HlsEvent::ThroughputSample { bytes_per_second });
    }

    pub fn emit_download_progress(&self, offset: u64, percent: Option<f32>) {
        let _ = self.tx.send(HlsEvent::DownloadProgress { offset, percent });
    }

    pub fn emit_playback_progress(&self, position: u64, percent: Option<f32>) {
        let _ = self
            .tx
            .send(HlsEvent::PlaybackProgress { position, percent });
    }

    pub fn emit_error(&self, error: &str, recoverable: bool) {
        let _ = self.tx.send(HlsEvent::Error {
            error: error.to_string(),
            recoverable,
        });
    }

    pub fn emit_end_of_stream(&self) {
        let _ = self.tx.send(HlsEvent::EndOfStream);
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
            reason: crate::abr::AbrReason::UpSwitch,
        };

        match event {
            HlsEvent::VariantApplied {
                from_variant,
                to_variant,
                reason,
            } => {
                assert_eq!(from_variant, 0);
                assert_eq!(to_variant, 1);
                assert!(matches!(reason, crate::abr::AbrReason::UpSwitch));
            }
            _ => panic!("Unexpected event type"),
        }
    }

    #[test]
    fn test_subscribe() {
        let emitter = EventEmitter::new();
        let mut rx = emitter.subscribe();
        emitter.emit_end_of_stream();

        let event = rx.try_recv().ok();
        assert!(matches!(event, Some(HlsEvent::EndOfStream)));
    }
}
