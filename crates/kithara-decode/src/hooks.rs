//! Wrapper that bridges decoder outcomes to `kithara-stream` reader hooks.
//!
//! The hook contract (`DecoderHooks`, `ReaderChunkSignal`,
//! `ReaderSeekSignal`, `SharedHooks`) lives in `kithara-stream` and is
//! protocol-agnostic. This wrapper sits at the [`Decoder`] boundary,
//! observes typed `DecoderChunkOutcome` / `DecoderSeekOutcome`, maps
//! each into the corresponding signal, and forwards it to the hook.
//! Backend code (`symphonia` / `apple` / `android`) stays untouched.

use std::time::Duration;

use kithara_stream::{ReaderChunkSignal, ReaderSeekSignal, SharedHooks};

use crate::{
    error::DecodeResult,
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    types::{PcmSpec, TrackMetadata},
};

/// Adapter that wraps any [`Decoder`] and forwards every method
/// to the inner decoder while invoking [`DecoderHooks`](kithara_stream::DecoderHooks)
/// callbacks after `next_chunk` / `seek` produce their typed outcomes.
///
/// One emission point per outcome — no double conversion, no per-byte
/// hook traffic. Backend code stays untouched; the wrapper sits at the
/// [`Decoder`] boundary.
pub struct HookedDecoder {
    inner: Box<dyn Decoder>,
    hooks: SharedHooks,
}

impl HookedDecoder {
    /// Wrap an existing decoder with a hook implementation.
    #[must_use]
    pub fn new(inner: Box<dyn Decoder>, hooks: SharedHooks) -> Self {
        Self { inner, hooks }
    }
}

fn chunk_signal(outcome: &DecoderChunkOutcome) -> ReaderChunkSignal {
    match outcome {
        DecoderChunkOutcome::Chunk(_) => ReaderChunkSignal::Chunk,
        DecoderChunkOutcome::Pending(reason) => ReaderChunkSignal::Pending(*reason),
        DecoderChunkOutcome::Eof => ReaderChunkSignal::Eof,
    }
}

fn seek_signal(outcome: &DecoderSeekOutcome) -> ReaderSeekSignal {
    match outcome {
        DecoderSeekOutcome::Landed { landed_byte, .. } => ReaderSeekSignal::Landed {
            landed_byte: *landed_byte,
        },
        DecoderSeekOutcome::PastEof { .. } => ReaderSeekSignal::PastEof,
    }
}

impl Decoder for HookedDecoder {
    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    fn metadata(&self) -> TrackMetadata {
        self.inner.metadata()
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let outcome = self.inner.next_chunk()?;
        if let Ok(mut hooks) = self.hooks.lock() {
            hooks.on_chunk(chunk_signal(&outcome));
        }
        Ok(outcome)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let outcome = self.inner.seek(pos)?;
        if let Ok(mut hooks) = self.hooks.lock() {
            hooks.on_seek(seek_signal(&outcome));
        }
        Ok(outcome)
    }

    fn spec(&self) -> PcmSpec {
        self.inner.spec()
    }

    fn update_byte_len(&self, len: u64) {
        self.inner.update_byte_len(len);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use kithara_bufpool::pcm_pool;
    use kithara_stream::{DecoderHooks, PendingReason};
    use kithara_test_utils::kithara;

    use super::*;
    use crate::types::{PcmChunk, PcmMeta};

    #[derive(Default)]
    struct CallLog {
        chunks: Vec<&'static str>,
        seeks: Vec<&'static str>,
    }

    struct LoggingHooks {
        log: Arc<Mutex<CallLog>>,
    }

    impl DecoderHooks for LoggingHooks {
        fn on_chunk(&mut self, signal: ReaderChunkSignal) {
            let tag = match signal {
                ReaderChunkSignal::Chunk => "chunk",
                ReaderChunkSignal::Pending(_) => "pending",
                ReaderChunkSignal::Eof => "eof",
                _ => "other",
            };
            self.log.lock().unwrap().chunks.push(tag);
        }

        fn on_seek(&mut self, signal: ReaderSeekSignal) {
            let tag = match signal {
                ReaderSeekSignal::Landed { .. } => "landed",
                ReaderSeekSignal::PastEof => "past_eof",
                _ => "other",
            };
            self.log.lock().unwrap().seeks.push(tag);
        }
    }

    struct StubDecoder {
        next_chunk_outcomes: Vec<DecoderChunkOutcome>,
        seek_outcomes: Vec<DecoderSeekOutcome>,
    }

    impl Decoder for StubDecoder {
        fn duration(&self) -> Option<Duration> {
            None
        }

        fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
            Ok(self
                .next_chunk_outcomes
                .pop()
                .unwrap_or(DecoderChunkOutcome::Eof))
        }

        fn seek(&mut self, _pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
            Ok(self
                .seek_outcomes
                .pop()
                .unwrap_or(DecoderSeekOutcome::PastEof {
                    duration: Duration::ZERO,
                }))
        }

        fn spec(&self) -> PcmSpec {
            PcmSpec {
                channels: 2,
                sample_rate: 44_100,
            }
        }

        fn update_byte_len(&self, _len: u64) {}
    }

    fn dummy_chunk() -> PcmChunk {
        let meta = PcmMeta {
            spec: PcmSpec {
                channels: 1,
                sample_rate: 8_000,
            },
            ..Default::default()
        };
        PcmChunk::new(meta, pcm_pool().get())
    }

    #[kithara::test]
    fn hooked_decoder_calls_on_chunk() {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let hooks: SharedHooks = Arc::new(Mutex::new(LoggingHooks {
            log: Arc::clone(&log),
        }));
        let inner = StubDecoder {
            next_chunk_outcomes: vec![DecoderChunkOutcome::Chunk(dummy_chunk())],
            seek_outcomes: vec![],
        };
        let mut hd = HookedDecoder::new(Box::new(inner), hooks);

        let _ = hd.next_chunk().unwrap();

        assert_eq!(log.lock().unwrap().chunks, vec!["chunk"]);
    }

    #[kithara::test]
    fn hooked_decoder_calls_on_seek_for_landed() {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let hooks: SharedHooks = Arc::new(Mutex::new(LoggingHooks {
            log: Arc::clone(&log),
        }));
        let inner = StubDecoder {
            next_chunk_outcomes: vec![],
            seek_outcomes: vec![DecoderSeekOutcome::Landed {
                landed_at: Duration::from_secs(1),
                landed_frame: 44_100,
                landed_byte: Some(123_456),
            }],
        };
        let mut hd = HookedDecoder::new(Box::new(inner), hooks);

        let _ = hd.seek(Duration::from_secs(1)).unwrap();

        assert_eq!(log.lock().unwrap().seeks, vec!["landed"]);
    }

    #[kithara::test]
    fn hooked_decoder_passes_through_pending_chunks() {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let hooks: SharedHooks = Arc::new(Mutex::new(LoggingHooks {
            log: Arc::clone(&log),
        }));
        let inner = StubDecoder {
            next_chunk_outcomes: vec![DecoderChunkOutcome::Pending(PendingReason::SeekPending)],
            seek_outcomes: vec![],
        };
        let mut hd = HookedDecoder::new(Box::new(inner), hooks);

        let _ = hd.next_chunk().unwrap();

        assert_eq!(log.lock().unwrap().chunks, vec!["pending"]);
    }

    #[kithara::test]
    fn hooked_decoder_passes_through_past_eof_seek() {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let hooks: SharedHooks = Arc::new(Mutex::new(LoggingHooks {
            log: Arc::clone(&log),
        }));
        let inner = StubDecoder {
            next_chunk_outcomes: vec![],
            seek_outcomes: vec![DecoderSeekOutcome::PastEof {
                duration: Duration::from_secs(10),
            }],
        };
        let mut hd = HookedDecoder::new(Box::new(inner), hooks);

        let _ = hd.seek(Duration::from_secs(15)).unwrap();

        assert_eq!(log.lock().unwrap().seeks, vec!["past_eof"]);
    }
}
