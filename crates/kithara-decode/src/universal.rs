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
pub struct UniversalDecoder<D: Demuxer, C: FrameCodec> {
    demuxer: D,
    codec: C,
    spec: PcmSpec,
    duration: Option<Duration>,
    pool: PcmPool,
    epoch: u64,
    byte_len_handle: Option<Arc<AtomicU64>>,
    stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Position of the most recent emitted chunk's end.
    position: Duration,
    /// Cumulative frame count emitted so far.
    frame_offset: u64,
    /// When `Some`, frames whose decode-time end is `<= target` are
    /// dropped before the next chunk is emitted. Cleared after the
    /// first frame past the target is consumed. Lets `seek(target)`
    /// land precisely at `target` instead of at the granule boundary.
    pending_seek_target: Option<Duration>,
}

impl<D: Demuxer, C: FrameCodec> UniversalDecoder<D, C> {
    /// Build a decoder from a `(demuxer, codec)` pair.
    pub fn new(
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
            position: Duration::ZERO,
            frame_offset: 0,
            pending_seek_target: None,
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

        let meta = PcmMeta {
            end_timestamp,
            timestamp,
            segment_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.segment_index()),
            source_byte_offset: None,
            variant_index: self.stream_ctx.as_ref().and_then(|ctx| ctx.variant_index()),
            spec: self.spec,
            frames: decoded.frames,
            epoch: self.epoch,
            frame_offset: self.frame_offset,
            source_bytes: u64::try_from(frame.data.len()).unwrap_or(u64::MAX),
        };
        let chunk = PcmChunk::new(meta, buf);
        self.position = end_timestamp;
        self.frame_offset = self.frame_offset.saturating_add(u64::from(decoded.frames));
        chunk
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
                let final_landed = pos.max(landed_at);
                self.position = final_landed;
                self.frame_offset = frame_offset_for(final_landed, self.spec.sample_rate);
                self.pending_seek_target = (landed_at < pos).then_some(pos);
                Ok(DecoderSeekOutcome::Landed {
                    landed_at: final_landed,
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
