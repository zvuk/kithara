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

use kithara_bufpool::PcmPool;
use kithara_stream::StreamContext;

use crate::{
    codec::{DecodedFrame, FrameCodec},
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame},
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
            frame_offset: 0,
            pending_seek_target: None,
            resync_frame_offset_to_pts: false,
        }
    }

    fn build_chunk(&mut self, decoded: &DecodedFrame, frame: &Frame) -> PcmChunk {
        let mut buf = self.pool.get();
        let _ = buf.ensure_len(decoded.samples.len());
        buf[..decoded.samples.len()].copy_from_slice(&decoded.samples);

        let timestamp = frame.pts;
        let chunk_secs = if self.spec.sample_rate > 0 {
            f64::from(decoded.frames) / f64::from(self.spec.sample_rate)
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
        self.frame_offset = self.frame_offset.saturating_add(u64::from(decoded.frames));

        let meta = PcmMeta {
            end_timestamp,
            timestamp,
            segment_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.segment_index()),
            source_byte_offset: None,
            variant_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.variant_index()),
            spec: self.spec,
            frames: decoded.frames,
            epoch: self.epoch,
            frame_offset,
            source_bytes: u64::try_from(frame.data.len()).unwrap_or(u64::MAX),
        };
        PcmChunk::new(meta, buf)
    }

    fn skip_frame_for_pending_target(&mut self, frame: &Frame) -> bool {
        let Some(target) = self.pending_seek_target else {
            return false;
        };
        let frame_end = frame.pts.saturating_add(frame.duration);
        if frame_end <= target {
            return true;
        }
        self.pending_seek_target = None;
        false
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
        loop {
            match self.demuxer.next_frame()? {
                DemuxOutcome::Frame(frame) => {
                    if self.skip_frame_for_pending_target(&frame) {
                        continue;
                    }
                    let decoded = self.codec.decode_frame(&frame.data, frame.pts)?;
                    if decoded.frames == 0 {
                        continue;
                    }
                    let chunk = self.build_chunk(&decoded, &frame);
                    return Ok(DecoderChunkOutcome::Chunk(chunk));
                }
                DemuxOutcome::Pending(reason) => {
                    return Ok(DecoderChunkOutcome::Pending(reason));
                }
                DemuxOutcome::Eof => return Ok(DecoderChunkOutcome::Eof),
            }
        }
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
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
        let mut decoder = UniversalDecoder::new(demuxer, codec, pcm_pool().clone(), 0, None, None);

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
        let mut decoder = UniversalDecoder::new(demuxer, codec, pcm_pool().clone(), 0, None, None);

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
