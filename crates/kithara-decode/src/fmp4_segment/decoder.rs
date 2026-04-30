use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

use kithara_bufpool::PcmPool;
use kithara_stream::{SegmentDescriptor, SharedSegmentedSource, StreamContext};

use crate::{
    DecoderConfig,
    backend::BoxedSource,
    error::{DecodeError, DecodeResult},
    fmp4_segment::{
        codec::FrameCodec,
        demux::{Fmp4Frame, Fmp4InitInfo, parse_init, parse_segment_frames},
        source_io::{FillStatus, SegmentReadState, fill_segment_buffer},
    },
    traits::{Decoder, DecoderChunkOutcome, DecoderSeekOutcome},
    types::{PcmChunk, PcmMeta, PcmSpec, TrackMetadata},
};

/// State of the segment-aware decoder's read cursor.
struct SegmentCursor {
    desc: SegmentDescriptor,
    read: SegmentReadState,
    /// Decoded frame index list — `None` until the buffer is fully
    /// loaded and demuxed.
    frames: Option<DecodedFrames>,
}

struct DecodedFrames {
    frames: Vec<Fmp4Frame>,
    next_index: usize,
}

/// Generic segment-by-segment decoder. Bypasses Symphonia's
/// `IsoMp4Reader` / Apple's `AudioFile` whole-stream parsers — each
/// HLS segment is demuxed independently and frames are fed straight to
/// the codec, so a forward seek reads exactly the target segment plus
/// the init segment (no prefix walk).
pub(crate) struct Fmp4SegmentDecoder<C: FrameCodec> {
    init: Fmp4InitInfo,
    codec: C,
    source: BoxedSource,
    segmented: SharedSegmentedSource,
    byte_len_handle: Option<Arc<AtomicU64>>,
    pool: PcmPool,
    spec: PcmSpec,
    epoch: u64,
    stream_ctx: Option<Arc<dyn StreamContext>>,
    /// Position of the *next* byte to read. Drives sequential play.
    next_byte: u64,
    cursor: Option<SegmentCursor>,
    position: Duration,
    frame_offset: u64,
    duration: Option<Duration>,
    /// When `Some`, the next demuxed segment's frames are advanced past
    /// any frame whose decode-time end is `<= target`. Cleared after the
    /// skip applies. Lets `seek(target)` land precisely at `target`
    /// instead of at the segment start, so the audio FSM does not have
    /// to consume + discard the seek-target prelude.
    pending_seek_target: Option<Duration>,
}

impl<C: FrameCodec> Fmp4SegmentDecoder<C> {
    /// Build a new decoder by reading the init segment via `source`,
    /// parsing it, and opening `C` against the resulting init info.
    pub(crate) fn new(
        mut source: BoxedSource,
        segmented: SharedSegmentedSource,
        config: &DecoderConfig,
    ) -> DecodeResult<Self> {
        let init_range = segmented.init_segment_range().ok_or_else(|| {
            DecodeError::InvalidData("HLS init segment range not announced".into())
        })?;
        let mut init_state = SegmentReadState::new(init_range);
        if let FillStatus::Pending(_) = fill_segment_buffer(&mut source, &mut init_state)? {
            return Err(DecodeError::Interrupted);
        }
        let init = parse_init(&init_state.buffer)?;
        let codec = C::open(&init)?;
        let spec = codec.spec();
        let pool = config
            .pcm_pool
            .clone()
            .unwrap_or_else(|| kithara_bufpool::pcm_pool().clone());
        let duration = compute_duration(&segmented, &init);

        Ok(Self {
            init,
            codec,
            source,
            segmented,
            byte_len_handle: config.byte_len_handle.clone(),
            pool,
            spec,
            epoch: config.epoch,
            stream_ctx: config.stream_ctx.clone(),
            next_byte: 0,
            cursor: None,
            position: Duration::ZERO,
            frame_offset: 0,
            duration,
            pending_seek_target: None,
        })
    }

    fn ensure_cursor(&mut self) -> EnsureCursor {
        if self.cursor.is_none() {
            let Some(desc) = self.segmented.segment_after_byte(self.next_byte) else {
                return EnsureCursor::Eof;
            };
            self.next_byte = desc.byte_range.end;
            self.cursor = Some(SegmentCursor {
                read: SegmentReadState::new(desc.byte_range.clone()),
                desc,
                frames: None,
            });
        }
        EnsureCursor::Ready
    }

    fn fill_cursor(&mut self) -> DecodeResult<FillStatus> {
        let cursor = self
            .cursor
            .as_mut()
            .expect("ensure_cursor must run before fill_cursor");
        if cursor.frames.is_some() {
            return Ok(FillStatus::Ready);
        }
        let status = fill_segment_buffer(&mut self.source, &mut cursor.read)?;
        if matches!(status, FillStatus::Ready) {
            let frames = parse_segment_frames(&self.init, &cursor.read.buffer)?;
            // Honour `pending_seek_target`: skip frames that end at or
            // before the seek target so the first emitted chunk lands
            // at the requested time, not at the segment boundary.
            let next_index = match self.pending_seek_target.take() {
                Some(target) => skip_count_for_target(&frames, target, self.init.timescale),
                None => 0,
            };
            cursor.frames = Some(DecodedFrames { frames, next_index });
        }
        Ok(status)
    }

    fn build_chunk(
        &mut self,
        decoded: &crate::fmp4_segment::codec::DecodedFrame,
        frame_decode_time: u64,
        frame_byte_offset: u64,
        frame_data_len: u64,
    ) -> PcmChunk {
        let pcm_spec = self.spec;
        let mut buf = self.pool.get();
        // ensure_len fills with zeros; we copy after.
        let _ = buf.ensure_len(decoded.samples.len());
        buf[..decoded.samples.len()].copy_from_slice(&decoded.samples);

        let timestamp = decode_time_to_duration(frame_decode_time, self.init.timescale);
        let chunk_secs = if pcm_spec.sample_rate > 0 {
            f64::from(decoded.frames) / f64::from(pcm_spec.sample_rate)
        } else {
            0.0
        };
        let frame_duration = Duration::from_secs_f64(chunk_secs);
        let end_timestamp = timestamp.saturating_add(frame_duration);

        let meta = PcmMeta {
            end_timestamp,
            timestamp,
            segment_index: self
                .cursor
                .as_ref()
                .map(|c| c.desc.segment_index)
                .or_else(|| self.stream_ctx.as_ref().and_then(|ctx| ctx.segment_index())),
            source_byte_offset: Some(frame_byte_offset),
            variant_index: self
                .cursor
                .as_ref()
                .map(|c| c.desc.variant_index)
                .or_else(|| self.stream_ctx.as_ref().and_then(|ctx| ctx.variant_index())),
            spec: pcm_spec,
            frames: decoded.frames,
            epoch: self.epoch,
            frame_offset: self.frame_offset,
            source_bytes: frame_data_len,
        };
        let chunk = PcmChunk::new(meta, buf);
        // Advance position/frame_offset.
        self.position = end_timestamp;
        self.frame_offset = self.frame_offset.saturating_add(u64::from(decoded.frames));
        chunk
    }
}

enum EnsureCursor {
    Ready,
    Eof,
}

fn compute_duration(segmented: &SharedSegmentedSource, _init: &Fmp4InitInfo) -> Option<Duration> {
    // Avoids relying on the init segment's `mvhd.duration` (often zero
    // for fragmented mp4) — playlist authority wins for HLS. Probe
    // with a far-future timestamp so `find_seek_point_for_time` clamps
    // to the last known segment, then take its end time.
    let last = segmented.segment_at_time(Duration::from_secs(u64::MAX / 2))?;
    Some(last.decode_time.saturating_add(last.duration))
}

/// Number of leading frames whose decode-time end is at or before
/// `target`. The next chunk emitted from `frames[skip..]` covers the
/// requested seek target.
fn skip_count_for_target(frames: &[Fmp4Frame], target: Duration, timescale: u32) -> usize {
    if timescale == 0 {
        return 0;
    }
    frames
        .iter()
        .take_while(|frame| {
            let end_ticks = frame.decode_time.saturating_add(u64::from(frame.duration));
            decode_time_to_duration(end_ticks, timescale) <= target
        })
        .count()
}

fn frame_offset_for(at: Duration, sample_rate: u32) -> u64 {
    let secs = at.as_secs();
    let subsec_frames = u64::from(at.subsec_nanos()) * u64::from(sample_rate) / 1_000_000_000;
    secs.saturating_mul(u64::from(sample_rate))
        .saturating_add(subsec_frames)
}

fn decode_time_to_duration(decode_time_ticks: u64, timescale: u32) -> Duration {
    if timescale == 0 {
        return Duration::ZERO;
    }
    let secs = decode_time_ticks / u64::from(timescale);
    let rem = decode_time_ticks % u64::from(timescale);
    let nanos = rem.saturating_mul(1_000_000_000) / u64::from(timescale);
    let nanos_u32 = u32::try_from(nanos).unwrap_or(999_999_999);
    Duration::new(secs, nanos_u32)
}

impl<C: FrameCodec> Decoder for Fmp4SegmentDecoder<C> {
    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn metadata(&self) -> TrackMetadata {
        TrackMetadata::default()
    }

    fn next_chunk(&mut self) -> DecodeResult<DecoderChunkOutcome> {
        loop {
            match self.ensure_cursor() {
                EnsureCursor::Ready => {}
                EnsureCursor::Eof => return Ok(DecoderChunkOutcome::Eof),
            }

            match self.fill_cursor()? {
                FillStatus::Ready => {}
                FillStatus::Pending(reason) => {
                    return Ok(DecoderChunkOutcome::Pending(reason));
                }
            }

            let cursor = self
                .cursor
                .as_mut()
                .expect("cursor present after ensure_cursor");
            let segment_byte_start = cursor.desc.byte_range.start;
            let frames_state = cursor
                .frames
                .as_mut()
                .expect("frames present after fill_cursor Ready");
            let frame_idx = frames_state.next_index;
            if frame_idx >= frames_state.frames.len() {
                // Segment exhausted — drop it and look up the next.
                self.cursor = None;
                continue;
            }
            let frame = frames_state.frames[frame_idx];
            frames_state.next_index = frame_idx + 1;

            let frame_data = &cursor.read.buffer[frame.offset..frame.offset + frame.size];
            let decoded = self.codec.decode_frame(&frame, frame_data)?;
            if decoded.frames == 0 {
                continue;
            }

            let frame_byte_offset = segment_byte_start.saturating_add(frame.offset as u64);
            let frame_data_len = frame.size as u64;
            let chunk = self.build_chunk(
                &decoded,
                frame.decode_time,
                frame_byte_offset,
                frame_data_len,
            );
            return Ok(DecoderChunkOutcome::Chunk(chunk));
        }
    }

    fn seek(&mut self, pos: Duration) -> DecodeResult<DecoderSeekOutcome> {
        let desc = self.segmented.segment_at_time(pos).ok_or_else(|| {
            DecodeError::SeekFailed(format!("no segment for time {}ms", pos.as_millis()))
        })?;
        if let Some(duration) = self.duration
            && desc.decode_time >= duration
        {
            return Ok(DecoderSeekOutcome::PastEof { duration });
        }

        // Reset cursor to the new segment, and arm the per-frame skip
        // so the next demuxed segment drops frames whose decode-time
        // ends at or before `pos`.
        self.next_byte = desc.byte_range.end;
        let landed_byte = desc.byte_range.start;
        // Land at the requested target (clamped to the segment start
        // floor so we never report a position before the segment we
        // chose). Audio FSM uses `landed_at` to anchor the timeline;
        // the actual first emitted chunk's PTS will match this within
        // one frame duration once the segment demuxes.
        let landed_at = pos.max(desc.decode_time);
        self.cursor = Some(SegmentCursor {
            read: SegmentReadState::new(desc.byte_range.clone()),
            desc,
            frames: None,
        });
        self.position = landed_at;
        self.frame_offset = frame_offset_for(landed_at, self.spec.sample_rate);
        self.pending_seek_target = Some(landed_at);
        self.codec.flush();

        Ok(DecoderSeekOutcome::Landed {
            landed_at,
            landed_frame: self.frame_offset,
            landed_byte: Some(landed_byte),
        })
    }

    fn spec(&self) -> PcmSpec {
        self.spec
    }

    fn update_byte_len(&self, len: u64) {
        if let Some(handle) = &self.byte_len_handle {
            handle.store(len, std::sync::atomic::Ordering::Release);
        }
    }
}
