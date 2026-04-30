//! `Fmp4SegmentDemuxer` — segment-aware [`Demuxer`] for HLS fMP4 streams.
//!
//! Bypasses Symphonia's whole-stream `IsoMp4Reader` to avoid the prefix
//! walk that broke HLS seeks (see `kithara-decode` crate README §2). Each
//! HLS segment is fetched independently through a [`crate::backend::BoxedSource`]
//! cursor, demuxed in memory via [`crate::fmp4_segment::demux`], and frames are
//! emitted one at a time. Segment layout (init range, decode-time → segment
//! mapping) comes from a sidecar [`Source`] handle — the same one HLS
//! exposes for layout queries — so the demuxer never re-parses MOOF
//! chains or walks the playlist.

use std::{sync::Arc, time::Duration};

use kithara_stream::Source;

use crate::{
    backend::BoxedSource,
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, TrackInfo},
    error::{DecodeError, DecodeResult},
    fmp4_segment::{
        demux::{CodecConfig, Fmp4Frame, Fmp4InitInfo, parse_init, parse_segment_frames},
        source_io::{FillStatus, SegmentReadState, fill_segment_buffer},
    },
};

struct SegmentCursor {
    read: SegmentReadState,
    frames: Option<DecodedFrames>,
}

struct DecodedFrames {
    frames: Vec<Fmp4Frame>,
    next_index: usize,
}

/// fMP4 segment-aware demuxer.
pub struct Fmp4SegmentDemuxer {
    init: Fmp4InitInfo,
    track_info: TrackInfo,
    source: BoxedSource,
    segments: Arc<dyn Source>,
    next_byte: u64,
    cursor: Option<SegmentCursor>,
    duration: Option<Duration>,
}

impl Fmp4SegmentDemuxer {
    /// Build a demuxer by fetching + parsing the init segment.
    ///
    /// `source` is the byte-level Read/Seek cursor; `segments` is the
    /// segment-aware sidecar (typically obtained from
    /// [`Source::as_segment_source`]) — the demuxer queries it for
    /// `init_segment_range` / `segment_at_time` / `segment_after_byte`.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::InvalidData`] when the init segment range
    /// is missing, the init buffer fails to fill, or the parsed init
    /// segment is malformed.
    /// Returns [`DecodeError::Interrupted`] when the source defers the
    /// init read; the caller should retry after the underlying source
    /// becomes ready.
    pub fn open(mut source: BoxedSource, segments: Arc<dyn Source>) -> DecodeResult<Self> {
        let init_range = segments.init_segment_range().ok_or_else(|| {
            DecodeError::InvalidData("HLS init segment range not announced".into())
        })?;
        let mut init_state = SegmentReadState::new(init_range);
        if let FillStatus::Pending(_) = fill_segment_buffer(&mut source, &mut init_state)? {
            return Err(DecodeError::Interrupted);
        }
        let init = parse_init(&init_state.buffer)?;
        let duration = compute_duration(&segments);
        let track_info = build_track_info(&init, duration);
        Ok(Self {
            init,
            track_info,
            source,
            segments,
            next_byte: 0,
            cursor: None,
            duration,
        })
    }

    fn ensure_cursor(&mut self) -> EnsureCursor {
        if self.cursor.is_some() {
            return EnsureCursor::Ready;
        }
        let Some(desc) = self.segments.segment_after_byte(self.next_byte) else {
            return EnsureCursor::Eof;
        };
        self.next_byte = desc.byte_range.end;
        self.cursor = Some(SegmentCursor {
            read: SegmentReadState::new(desc.byte_range),
            frames: None,
        });
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
            cursor.frames = Some(DecodedFrames {
                frames,
                next_index: 0,
            });
        }
        Ok(status)
    }
}

enum EnsureCursor {
    Ready,
    Eof,
}

impl Demuxer for Fmp4SegmentDemuxer {
    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome> {
        loop {
            match self.ensure_cursor() {
                EnsureCursor::Ready => {}
                EnsureCursor::Eof => return Ok(DemuxOutcome::Eof),
            }

            match self.fill_cursor()? {
                FillStatus::Ready => {}
                FillStatus::Pending(reason) => return Ok(DemuxOutcome::Pending(reason)),
            }

            let cursor = self
                .cursor
                .as_mut()
                .expect("cursor present after ensure_cursor");
            let frames_state = cursor.frames.as_mut().expect("frames present after Ready");
            let frame_idx = frames_state.next_index;
            if frame_idx >= frames_state.frames.len() {
                self.cursor = None;
                continue;
            }
            let frame = frames_state.frames[frame_idx];
            frames_state.next_index = frame_idx + 1;
            let frame_data = cursor.read.buffer[frame.offset..frame.offset + frame.size].to_vec();
            let pts = ticks_to_duration(frame.decode_time, self.init.timescale);
            let dur = ticks_to_duration(u64::from(frame.duration), self.init.timescale);
            return Ok(DemuxOutcome::Frame(Frame {
                data: frame_data,
                pts,
                duration: dur,
            }));
        }
    }

    fn seek(&mut self, target: Duration) -> DecodeResult<DemuxSeekOutcome> {
        let Some(desc) = self.segments.segment_at_time(target) else {
            return Err(DecodeError::SeekFailed(format!(
                "no segment for time {}ms",
                target.as_millis()
            )));
        };
        if let Some(duration) = self.duration
            && desc.decode_time >= duration
        {
            return Ok(DemuxSeekOutcome::PastEof { duration });
        }
        self.next_byte = desc.byte_range.end;
        let landed_byte = desc.byte_range.start;
        let landed_at = target.max(desc.decode_time);
        self.cursor = Some(SegmentCursor {
            read: SegmentReadState::new(desc.byte_range),
            frames: None,
        });
        Ok(DemuxSeekOutcome::Landed {
            landed_at,
            landed_byte: Some(landed_byte),
        })
    }
}

fn build_track_info(init: &Fmp4InitInfo, duration: Option<Duration>) -> TrackInfo {
    let extra_data = match &init.config {
        CodecConfig::Aac(bytes) | CodecConfig::Flac(bytes) => bytes.clone(),
    };
    TrackInfo {
        codec: init.codec,
        sample_rate: init.sample_rate,
        channels: init.channels,
        timescale: init.timescale,
        extra_data,
        duration,
    }
}

fn compute_duration(segments: &Arc<dyn Source>) -> Option<Duration> {
    let last = segments.segment_at_time(Duration::from_secs(u64::MAX / 2))?;
    Some(last.decode_time.saturating_add(last.duration))
}

fn ticks_to_duration(ticks: u64, timescale: u32) -> Duration {
    if timescale == 0 {
        return Duration::ZERO;
    }
    let secs = ticks / u64::from(timescale);
    let rem = ticks % u64::from(timescale);
    let nanos = rem.saturating_mul(1_000_000_000) / u64::from(timescale);
    let nanos_u32 = u32::try_from(nanos).unwrap_or(999_999_999);
    Duration::new(secs, nanos_u32)
}
