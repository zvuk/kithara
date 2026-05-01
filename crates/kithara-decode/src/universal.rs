//! `UniversalDecoder<D, C>` — single `Decoder` impl that pairs a
//! [`Demuxer`] with a [`FrameCodec`].
//!
//! Replaces the per-backend `Fmp4SegmentDecoder` / `AppleDecoder` /
//! `SymphoniaDecoder` / `AndroidDecoder` god-types once Phase 5 wires
//! the factory to it.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use kithara_bufpool::{PcmBuf, PcmPool};
use kithara_stream::{ReaderChunkSignal, ReaderSeekSignal, SharedHooks, StreamContext};

use crate::{
    codec::FrameCodec,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer},
    error::DecodeResult,
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

/// Generic decoder built by composition: a [`Demuxer`] feeds raw frames
/// into a [`FrameCodec`] which produces PCM. One implementation, one
/// dispatch path — no per-backend duplication.
pub(crate) struct UniversalDecoder<D: Demuxer, C: FrameCodec> {
    demuxer: D,
    codec: C,
    spec: PcmSpec,
    duration: Option<Duration>,
    pool: PcmPool,
    epoch: u64,
    byte_len_handle: Option<Arc<AtomicU64>>,
    stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Reader-side observer hooks. `None` skips the lock entirely on the
    /// hot path; `Some(_)` emits per-chunk / per-seek signals after the
    /// inner outcome resolves. Folded in from the former `HookedDecoder`
    /// decorator — every decoder is hookable now.
    hooks: Option<SharedHooks>,
    /// Cumulative frame counter. Anchored to `landed_at` on seek (so the
    /// next chunk's `frame_offset / sample_rate ≈ timestamp`) and
    /// incremented by `decoded.frames` per emitted chunk. Tracking it
    /// cumulatively avoids the precision loss that sneaks in if every
    /// chunk recomputes `floor(pts * sample_rate)` from a `Duration`
    /// nanosecond value.
    frame_offset: u64,
    /// When `Some`, frames whose decode-time end is `<= target` are
    /// dropped before the next chunk is emitted. Cleared after the
    /// first frame past the target is consumed. Lets `seek(target)`
    /// land precisely at `target` instead of at the granule boundary.
    pending_seek_target: Option<Duration>,
    /// Set on every seek; the next emitted chunk re-anchors
    /// `frame_offset` to its own pts so that the chunk-level invariant
    /// `frame_offset / sample_rate ≈ timestamp` holds even when the
    /// demuxer snapped to a packet boundary inside / behind the
    /// requested seek target.
    resync_frame_offset_to_pts: bool,
}

impl<D: Demuxer, C: FrameCodec> UniversalDecoder<D, C> {
    /// Build a decoder from a `(demuxer, codec)` pair.
    pub(crate) fn new(
        demuxer: D,
        codec: C,
        pool: PcmPool,
        epoch: u64,
        byte_len_handle: Option<Arc<AtomicU64>>,
        stream_ctx: Option<Arc<dyn StreamContext>>,
        hooks: Option<SharedHooks>,
    ) -> Self {
        let spec = codec.spec();
        let duration = demuxer.duration();
        Self {
            demuxer,
            codec,
            spec,
            duration,
            pool,
            epoch,
            byte_len_handle,
            stream_ctx,
            hooks,
            frame_offset: 0,
            pending_seek_target: None,
            resync_frame_offset_to_pts: false,
        }
    }

    fn emit_chunk_signal(&self, outcome: &DecoderChunkOutcome) {
        let Some(hooks) = self.hooks.as_ref() else {
            return;
        };
        let signal = match outcome {
            DecoderChunkOutcome::Chunk(_) => ReaderChunkSignal::Chunk,
            DecoderChunkOutcome::Pending(reason) => ReaderChunkSignal::Pending(*reason),
            DecoderChunkOutcome::Eof => ReaderChunkSignal::Eof,
        };
        if let Ok(mut h) = hooks.lock() {
            h.on_chunk(signal);
        }
    }

    fn emit_seek_signal(&self, outcome: &DecoderSeekOutcome) {
        let Some(hooks) = self.hooks.as_ref() else {
            return;
        };
        let signal = match outcome {
            DecoderSeekOutcome::Landed { landed_byte, .. } => ReaderSeekSignal::Landed {
                landed_byte: *landed_byte,
            },
            DecoderSeekOutcome::PastEof { .. } => ReaderSeekSignal::PastEof,
        };
        if let Ok(mut h) = hooks.lock() {
            h.on_seek(signal);
        }
    }

    /// Build the output `PcmChunk` from a just-filled pool buffer plus
    /// the demuxed frame's metadata. Inlined fields (rather than taking
    /// `&Frame<'_>`) so the caller can release the demuxer borrow before
    /// invoking this — needed because `Frame<'_>` borrows into the
    /// demuxer state and would conflict with `&mut self` here.
    fn build_chunk(
        &mut self,
        buf: PcmBuf,
        frames: u32,
        timestamp: Duration,
        source_bytes: u64,
    ) -> PcmChunk {
        let chunk_secs = if self.spec.sample_rate > 0 {
            f64::from(frames) / f64::from(self.spec.sample_rate)
        } else {
            0.0
        };
        let frame_duration = Duration::from_secs_f64(chunk_secs);
        let end_timestamp = timestamp.saturating_add(frame_duration);

        if self.resync_frame_offset_to_pts {
            self.resync_frame_offset_to_pts = false;
            self.frame_offset = frame_offset_for(timestamp, self.spec.sample_rate);
        }
        let frame_offset = self.frame_offset;
        self.frame_offset = self.frame_offset.saturating_add(u64::from(frames));

        let meta = PcmMeta {
            end_timestamp,
            timestamp,
            segment_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.segment_index()),
            source_byte_offset: None,
            variant_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.variant_index()),
            spec: self.spec,
            frames,
            epoch: self.epoch,
            frame_offset,
            source_bytes,
        };
        PcmChunk::new(meta, buf)
    }

    fn next_chunk_inner(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            // `Frame<'_>` borrows into `self.demuxer`'s internal state
            // (Symphonia `Packet`, fmp4 segment buffer). We carefully
            // touch only disjoint fields (`self.pool`, `self.codec`,
            // `self.pending_seek_target`) directly while the frame is
            // alive — going through `&mut self` methods would conflict.
            let frame = match self.demuxer.next_frame()? {
                DemuxOutcome::Frame(frame) => frame,
                DemuxOutcome::Pending(reason) => {
                    return Ok(DecoderChunkOutcome::Pending(reason));
                }
                DemuxOutcome::Eof => return Ok(DecoderChunkOutcome::Eof),
            };
            // Inline `skip_frame_for_pending_target` to keep the demuxer
            // borrow live alongside the codec call below.
            if let Some(target) = self.pending_seek_target {
                let frame_end = frame.pts.saturating_add(frame.duration);
                if frame_end <= target {
                    continue;
                }
                self.pending_seek_target = None;
            }
            let mut buf = self.pool.get();
            let frames = self.codec.decode_frame(frame.data, frame.pts, &mut buf)?;
            if frames == 0 {
                continue;
            }
            let pts = frame.pts;
            let source_bytes = u64::try_from(frame.data.len()).unwrap_or(u64::MAX);
            // `frame.data` borrow ends here — NLL releases the demuxer
            // borrow so `build_chunk(&mut self, ...)` can run.
            let chunk = self.build_chunk(buf, frames, pts, source_bytes);
            return Ok(DecoderChunkOutcome::Chunk(chunk));
        }
    }

    fn seek_inner(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        match self.demuxer.seek(pos)? {
            DemuxSeekOutcome::Landed {
                landed_at,
                landed_byte,
            } => {
                self.codec.flush();
                // The audio pipeline's timeline anchors at this report;
                // surface the *requested* target when the demuxer
                // snapped backward (e.g. HLS segment start) so the user
                // sees the position they asked for, while
                // `pending_seek_target` makes sure the first emitted
                // chunk's PTS lands at-or-past the target.
                let reported_landed_at = pos.max(landed_at);
                self.pending_seek_target = (landed_at < pos).then_some(pos);
                self.frame_offset = frame_offset_for(reported_landed_at, self.spec.sample_rate);
                self.resync_frame_offset_to_pts = true;
                Ok(DecoderSeekOutcome::Landed {
                    landed_at: reported_landed_at,
                    landed_frame: self.frame_offset,
                    landed_byte,
                })
            }
            DemuxSeekOutcome::PastEof { duration } => {
                self.codec.flush();
                Ok(DecoderSeekOutcome::PastEof { duration })
            }
        }
    }
}

impl<D: Demuxer + 'static, C: FrameCodec> Decoder for UniversalDecoder<D, C> {
    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn metadata(&self) -> TrackMetadata {
        TrackMetadata::default()
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        let outcome = self.next_chunk_inner()?;
        self.emit_chunk_signal(&outcome);
        Ok(outcome)
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let outcome = self.seek_inner(pos)?;
        self.emit_seek_signal(&outcome);
        Ok(outcome)
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn update_byte_len(&self, len: u64) {
        if let Some(handle) = &self.byte_len_handle {
            handle.store(len, Ordering::Release);
        }
    }
}

fn frame_offset_for(at: Duration, sample_rate: u32) -> u64 {
    let secs = at.as_secs();
    let subsec_frames = u64::from(at.subsec_nanos()) * u64::from(sample_rate) / 1_000_000_000;
    secs.saturating_mul(u64::from(sample_rate))
        .saturating_add(subsec_frames)
}

#[cfg(all(test, feature = "symphonia"))]
mod smoke_tests {
    //! White-box smoke tests for `UniversalDecoder<SymphoniaDemuxer, SymphoniaCodec>`
    //! on a real MP3 fixture. Validates that the unified composition path emits
    //! non-empty `PcmChunk` values and round-trips a seek to start. Migrated from
    //! `tests/universal_smoke.rs` after the public types were demoted to
    //! `pub(crate)`.

    use std::io::Cursor;

    use kithara_bufpool::pcm_pool;
    use kithara_stream::AudioCodec;
    use kithara_test_utils::kithara;
    use symphonia::core::{
        formats::{FormatOptions, probe::Hint},
        io::{MediaSourceStream, MediaSourceStreamOptions},
        meta::MetadataOptions,
    };

    use super::*;
    use crate::{
        codec::FrameCodec,
        symphonia::{SymphoniaCodec, SymphoniaDemuxer},
        traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    };

    const TEST_MP3_BYTES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/test.mp3"
    ));

    fn build_mp3_demuxer() -> SymphoniaDemuxer {
        let cursor = Cursor::new(TEST_MP3_BYTES.to_vec());
        let mss = MediaSourceStream::new(Box::new(cursor), MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        hint.with_extension("mp3");
        let format_reader = symphonia::default::get_probe()
            .probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )
            .expect("MP3 probe should succeed");
        SymphoniaDemuxer::from_reader(format_reader, None).expect("MP3 demuxer should build")
    }

    #[kithara::test]
    fn mp3_track_info_carries_codec_and_rate() {
        let demuxer = build_mp3_demuxer();
        let info = demuxer.track_info();
        assert_eq!(info.codec, AudioCodec::Mp3);
        assert!(info.sample_rate > 0, "sample rate must be populated");
        assert!(info.channels > 0, "channels must be populated");
    }

    #[kithara::test]
    fn mp3_universal_decoder_emits_non_empty_chunks() {
        let demuxer = build_mp3_demuxer();
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open(&track_info).expect("MP3 codec should open");
        let mut decoder =
            UniversalDecoder::new(demuxer, codec, pcm_pool().clone(), 0, None, None, None);

        let mut got_chunk = false;
        for _ in 0..16 {
            match decoder.next_chunk().expect("next_chunk should not error") {
                DecoderChunkOutcome::Chunk(chunk) => {
                    assert!(chunk.frames() > 0, "Chunk frames must be > 0");
                    assert!(chunk.spec().sample_rate > 0);
                    assert!(chunk.spec().channels > 0);
                    got_chunk = true;
                    break;
                }
                DecoderChunkOutcome::Pending(_) => continue,
                DecoderChunkOutcome::Eof => panic!("MP3 fixture must not EOF in 16 packets"),
            }
        }
        assert!(got_chunk, "UniversalDecoder must emit at least one chunk");
    }

    #[kithara::test]
    fn mp3_universal_decoder_seeks_back_to_start_after_pulling_chunks() {
        let demuxer = build_mp3_demuxer();
        let track_info = demuxer.track_info().clone();
        let codec = SymphoniaCodec::open(&track_info).expect("MP3 codec should open");
        let mut decoder =
            UniversalDecoder::new(demuxer, codec, pcm_pool().clone(), 0, None, None, None);

        for _ in 0..4 {
            let _ = decoder
                .next_chunk()
                .expect("priming chunks should not error");
        }

        let outcome = decoder
            .seek(Duration::ZERO)
            .expect("seek to start must not error");
        match outcome {
            DecoderSeekOutcome::Landed { landed_at, .. } => {
                assert!(
                    landed_at < Duration::from_millis(50),
                    "seek to ZERO should land near 0, got {landed_at:?}"
                );
            }
            DecoderSeekOutcome::PastEof { .. } => {
                panic!("seek(0) on a real MP3 must not be PastEof")
            }
        }
    }
}

#[cfg(test)]
mod hook_tests {
    //! Hook-emission tests for `UniversalDecoder`. Validate that the folded-in
    //! `Option<SharedHooks>` field forwards `next_chunk` / `seek` outcomes to
    //! the registered observer. Migrated from the former `HookedDecoder`
    //! decorator, which has been deleted in favour of native hook support.

    use std::sync::{Arc, Mutex};

    use kithara_bufpool::pcm_pool;
    use kithara_stream::{
        DecoderHooks, PendingReason, ReaderChunkSignal, ReaderSeekSignal, SharedHooks,
    };
    use kithara_test_utils::kithara;

    use super::*;
    use crate::{
        demuxer::{DemuxOutcome, DemuxSeekOutcome, Frame, TrackInfo},
        traits::Decoder,
    };

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

    /// Owned variant of `DemuxOutcome` used to seed the stub. Kept
    /// separate so the borrowed `DemuxOutcome<'_>` returned by
    /// `next_frame` can be backed by the stub's own buffer.
    enum StubOutcome {
        Frame {
            data: Vec<u8>,
            pts: Duration,
            duration: Duration,
        },
        Pending(PendingReason),
    }

    /// Stub demuxer + codec pair driven by canned outcomes. Exists only so
    /// hook tests can construct a `UniversalDecoder` without needing a real
    /// container/codec.
    struct StubDemuxer {
        next: Vec<StubOutcome>,
        seek: Vec<DemuxSeekOutcome>,
        track: TrackInfo,
        /// Current frame's owned bytes — live so the returned
        /// `Frame<'_>` slice has somewhere to point. Replaced on each
        /// `next_frame` call.
        held: Vec<u8>,
    }

    impl Demuxer for StubDemuxer {
        fn track_info(&self) -> &TrackInfo {
            &self.track
        }
        fn duration(&self) -> Option<Duration> {
            None
        }
        fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
            match self.next.pop() {
                Some(StubOutcome::Frame {
                    data,
                    pts,
                    duration,
                }) => {
                    self.held = data;
                    Ok(DemuxOutcome::Frame(Frame {
                        data: &self.held,
                        pts,
                        duration,
                    }))
                }
                Some(StubOutcome::Pending(reason)) => Ok(DemuxOutcome::Pending(reason)),
                None => Ok(DemuxOutcome::Eof),
            }
        }
        fn seek(&mut self, _pos: Duration) -> DecodeResult<DemuxSeekOutcome> {
            Ok(self.seek.pop().unwrap_or(DemuxSeekOutcome::PastEof {
                duration: Duration::ZERO,
            }))
        }
    }

    struct StubCodec {
        spec: PcmSpec,
    }

    impl FrameCodec for StubCodec {
        fn open(_track: &TrackInfo) -> DecodeResult<Self> {
            unreachable!("stub codec is hand-built")
        }
        fn decode_frame(
            &mut self,
            _bytes: &[u8],
            _pts: Duration,
            out: &mut PcmBuf,
        ) -> DecodeResult<u32> {
            let samples = self.spec.channels as usize;
            out.ensure_len(samples)
                .map_err(|e| crate::error::DecodeError::Backend(Box::new(e)))?;
            for slot in out.iter_mut() {
                *slot = 0.0;
            }
            out.truncate(samples);
            Ok(1)
        }
        fn flush(&mut self) {}
        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    fn empty_track() -> TrackInfo {
        TrackInfo {
            codec: kithara_stream::AudioCodec::Mp3,
            sample_rate: 44_100,
            channels: 2,
            extra_data: Vec::new(),
            duration: None,
        }
    }

    fn build(
        demuxer: StubDemuxer,
        log: Arc<Mutex<CallLog>>,
    ) -> UniversalDecoder<StubDemuxer, StubCodec> {
        let codec = StubCodec {
            spec: PcmSpec {
                channels: 2,
                sample_rate: 44_100,
            },
        };
        let hooks: SharedHooks = Arc::new(Mutex::new(LoggingHooks { log }));
        UniversalDecoder::new(
            demuxer,
            codec,
            pcm_pool().clone(),
            0,
            None,
            None,
            Some(hooks),
        )
    }

    #[kithara::test]
    fn next_chunk_emits_chunk_signal() {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer {
            next: vec![StubOutcome::Frame {
                data: vec![0u8; 4],
                pts: Duration::ZERO,
                duration: Duration::from_millis(20),
            }],
            seek: Vec::new(),
            track: empty_track(),
            held: Vec::new(),
        };
        let mut decoder = build(demuxer, Arc::clone(&log));
        let _ = decoder.next_chunk().unwrap();
        assert_eq!(log.lock().unwrap().chunks, vec!["chunk"]);
    }

    #[kithara::test]
    fn next_chunk_emits_pending_signal() {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer {
            next: vec![StubOutcome::Pending(PendingReason::SeekPending)],
            seek: Vec::new(),
            track: empty_track(),
            held: Vec::new(),
        };
        let mut decoder = build(demuxer, Arc::clone(&log));
        let _ = decoder.next_chunk().unwrap();
        assert_eq!(log.lock().unwrap().chunks, vec!["pending"]);
    }

    #[kithara::test]
    fn seek_emits_landed_signal() {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer {
            next: Vec::new(),
            seek: vec![DemuxSeekOutcome::Landed {
                landed_at: Duration::from_secs(1),
                landed_byte: Some(123),
            }],
            track: empty_track(),
            held: Vec::new(),
        };
        let mut decoder = build(demuxer, Arc::clone(&log));
        let _ = decoder.seek(Duration::from_secs(1)).unwrap();
        assert_eq!(log.lock().unwrap().seeks, vec!["landed"]);
    }

    #[kithara::test]
    fn seek_emits_past_eof_signal() {
        let log = Arc::new(Mutex::new(CallLog::default()));
        let demuxer = StubDemuxer {
            next: Vec::new(),
            seek: vec![DemuxSeekOutcome::PastEof {
                duration: Duration::from_secs(10),
            }],
            track: empty_track(),
            held: Vec::new(),
        };
        let mut decoder = build(demuxer, Arc::clone(&log));
        let _ = decoder.seek(Duration::from_secs(15)).unwrap();
        assert_eq!(log.lock().unwrap().seeks, vec!["past_eof"]);
    }
}

#[cfg(test)]
mod pool_budget_tests {
    //! Pool-budget regression tests for the zero-alloc codec path.
    //!
    //! Construct a `PcmPool` with a small fixed buffer count, pre-warm it
    //! with the chunk size we expect, run N decode iterations through a
    //! stub `FrameCodec`, and assert that `alloc_misses` stays at the
    //! pre-warm count — i.e. no extra allocations beyond the warm-up.
    //! The contract: once warm, the hot path recycles buffers; codecs must
    //! never spawn fresh `Vec<f32>` allocations per frame.
    //!
    //! Backend-specific equivalents (Apple/Android FFI cookie code paths)
    //! live next to those codecs.

    use std::time::Duration;

    use kithara_bufpool::{PcmBuf, PcmPool};
    use kithara_test_utils::kithara;

    use crate::{codec::FrameCodec, demuxer::TrackInfo, error::DecodeResult, types::PcmSpec};

    /// Stub codec that always writes `frames_per_call * channels` samples.
    /// Mimics a generic `FrameCodec` contract without depending on any
    /// specific backend.
    struct ConstFrameCodec {
        spec: PcmSpec,
        frames_per_call: u32,
    }

    impl FrameCodec for ConstFrameCodec {
        fn open(_track: &TrackInfo) -> DecodeResult<Self> {
            unreachable!("hand-built")
        }
        fn decode_frame(
            &mut self,
            _bytes: &[u8],
            _pts: Duration,
            out: &mut PcmBuf,
        ) -> DecodeResult<u32> {
            let samples = self.frames_per_call as usize * self.spec.channels as usize;
            out.ensure_len(samples)
                .map_err(|e| crate::error::DecodeError::Backend(Box::new(e)))?;
            for slot in out.iter_mut() {
                *slot = 0.0;
            }
            out.truncate(samples);
            Ok(self.frames_per_call)
        }
        fn flush(&mut self) {}
        fn spec(&self) -> PcmSpec {
            self.spec
        }
    }

    #[kithara::test]
    fn codec_warm_pool_does_not_grow_alloc_misses() {
        // 32 max buffers split across 8 shards = 4 per shard. Single-test
        // thread routes to one shard, so warm-up size of 4 fits.
        let pool = PcmPool::new(32, 8192);
        // Pre-warm: pull 4 buffers, grow each to the per-call sample budget,
        // then drop them so they recycle into the pool with right capacity.
        for _ in 0..4 {
            let mut buf = pool.get();
            buf.ensure_len(2048).unwrap();
        }
        let warmup_misses = pool.stats().alloc_misses;

        let mut codec = ConstFrameCodec {
            spec: PcmSpec {
                channels: 2,
                sample_rate: 44_100,
            },
            frames_per_call: 1024,
        };
        for _ in 0..200 {
            let mut buf = pool.get();
            let frames = codec
                .decode_frame(&[], Duration::ZERO, &mut buf)
                .expect("decode_frame");
            assert_eq!(frames, 1024);
            // buf drops, returning to pool.
        }

        assert_eq!(
            pool.stats().alloc_misses,
            warmup_misses,
            "no further alloc misses expected after warm-up; \
             codec must not allocate fresh buffers per call"
        );
    }
}
