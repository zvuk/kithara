use std::sync::Arc;

use kithara_bufpool::BytePool;
use kithara_platform::time::Duration;
use kithara_stream::{AudioCodec, ByteMap};
use kithara_test_utils::kithara;

use super::{
    parsing::{CodecConfig, Fmp4Frame, Fmp4InitInfo, parse_init, parse_segment_frames},
    source_io::{FillStatus, LiveRange, SegmentReadState, fill_segment_buffer},
};
use crate::{
    codec::{CodecPriming, access_unit_frames},
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, PrerollHint, TrackInfo},
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

struct SegmentCursor {
    frames: Option<DecodedFrames>,
    read: SegmentReadState,
    segment_index: u32,
    variant_index: usize,
}

struct DecodedFrames {
    frames: Vec<Fmp4Frame>,
    next_index: usize,
}

/// fMP4 segment-aware demuxer.
pub(crate) struct Fmp4SegmentDemuxer {
    segments: Arc<dyn ByteMap>,
    source: BoxedSource,
    init: Fmp4InitInfo,
    cursor: Option<SegmentCursor>,
    duration: Option<Duration>,
    track_info: TrackInfo,
    /// Host byte-buffer pool. Each segment cursor draws its read buffer
    /// from here and returns it on drop, so a steady-state decode loop
    /// recycles one high-water allocation instead of mallocing per
    /// segment (and per variant-switch demuxer recreate).
    byte_pool: BytePool,
    /// Index of the next segment to decode. Sequential playback advances
    /// this by one per segment; a seek sets it to the segment after the
    /// landing segment. Advancing by index (rather than by the previous
    /// segment's `byte_range.end`) keeps the decode cursor stable when the
    /// live byte-layout shifts mid-playback — a segment's size estimate is
    /// corrected to its real length on commit, and a byte-based advance
    /// would then re-resolve to the wrong segment and silently skip one.
    next_segment_index: u32,
}

impl Fmp4SegmentDemuxer {
    fn ensure_cursor(&mut self) -> EnsureCursor {
        if self.cursor.is_some() {
            return EnsureCursor::Ready;
        }
        let Some(desc) = self.segments.segment_at_index(self.next_segment_index) else {
            return EnsureCursor::Eof;
        };
        self.next_segment_index = desc.segment_index.saturating_add(1);
        self.cursor = Some(SegmentCursor {
            read: SegmentReadState::new(desc.byte_range, self.byte_pool.get()),
            frames: None,
            segment_index: desc.segment_index,
            variant_index: desc.variant_index,
        });
        EnsureCursor::Ready
    }

    fn fill_cursor(&mut self) -> DecodeResult<FillStatus> {
        let cursor = self
            .cursor
            .as_mut()
            .expect("BUG: ensure_cursor must run before fill_cursor");
        if cursor.frames.is_some() {
            return Ok(FillStatus::Ready);
        }
        let segments = self.segments.as_ref();
        let status = fill_segment_buffer(
            &mut self.source,
            &mut cursor.read,
            LiveRange::Segment(segments, cursor.segment_index),
        )?;
        if matches!(status, FillStatus::Ready) {
            let frames = parse_segment_frames(&self.init, &cursor.read.buffer)?;
            cursor.frames = Some(DecodedFrames {
                frames,
                next_index: 0,
            });
        }
        Ok(status)
    }

    /// Build a demuxer by fetching + parsing the init segment.
    ///
    /// `source` is the byte-level Read/Seek cursor; `segments` is the
    /// segment-layout handle (typically obtained from
    /// [`kithara_stream::Source::byte_map`]) — the demuxer
    /// queries it for `init_segment_range` / `segment_at_time` /
    /// `segment_after_byte`.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::InvalidData`] when the init segment range
    /// is missing, the init buffer fails to fill, or the parsed init
    /// segment is malformed.
    /// Returns [`DecodeError::Interrupted`] when the source defers the
    /// init read; the caller should retry after the underlying source
    /// becomes ready.
    pub(crate) fn open(
        mut source: BoxedSource,
        segments: Arc<dyn ByteMap>,
        byte_pool: BytePool,
    ) -> DecodeResult<Self> {
        let init_range = segments.init_segment_range();
        if init_range.is_empty() {
            return Err(DecodeError::InvalidData(
                "HLS init segment range not announced".into(),
            ));
        }
        let mut init_state = SegmentReadState::new(init_range, byte_pool.get());
        if let FillStatus::Pending(_) = fill_segment_buffer(
            &mut source,
            &mut init_state,
            LiveRange::Init(segments.as_ref()),
        )? {
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
            duration,
            byte_pool,
            next_segment_index: 0,
            cursor: None,
        })
    }
}

enum EnsureCursor {
    Ready,
    Eof,
}

impl Demuxer for Fmp4SegmentDemuxer {
    fn current_segment_index(&self) -> Option<u32> {
        self.cursor.as_ref().map(|c| c.segment_index)
    }

    fn current_variant_index(&self) -> Option<usize> {
        self.cursor.as_ref().map(|c| c.variant_index)
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    #[kithara::probe]
    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        loop {
            match self.ensure_cursor() {
                EnsureCursor::Ready => {}
                EnsureCursor::Eof => return Ok(DemuxOutcome::Eof),
            }

            match self.fill_cursor()? {
                FillStatus::Ready => {}
                FillStatus::Pending(reason) => return Ok(DemuxOutcome::Pending(reason)),
            }

            let frame_meta = {
                let cursor = self
                    .cursor
                    .as_mut()
                    .expect("BUG: cursor present after ensure_cursor");
                let frames_state = cursor
                    .frames
                    .as_mut()
                    .expect("BUG: frames present after Ready");
                let frame_idx = frames_state.next_index;
                if frame_idx >= frames_state.frames.len() {
                    None
                } else {
                    let frame = frames_state.frames[frame_idx];
                    frames_state.next_index = frame_idx + 1;
                    Some(frame)
                }
            };
            let Some(frame) = frame_meta else {
                self.cursor = None;
                continue;
            };
            let cursor = self.cursor.as_ref().expect("BUG: cursor still present");
            let pts = ticks_to_duration(frame.decode_time, self.init.timescale);
            let dur = ticks_to_duration(u64::from(frame.duration), self.init.timescale);
            let data: &[u8] = &cursor.read.buffer[frame.offset..frame.offset + frame.size];
            return Ok(DemuxOutcome::Frame(Frame {
                data,
                pts,
                duration: dur,
                packet_desc: &[],
            }));
        }
    }

    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        // WHY: back off to `target - warmup` for SBR/PS pre-roll (README "Seek pre-roll and trim").
        let seek_target =
            warmup_backoff(self.track_info.codec, self.track_info.sample_rate, &priming)
                .map_or(target, |backoff| target.saturating_sub(backoff));
        let Some(desc) = self.segments.segment_at_time(seek_target) else {
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
        let landed_byte = desc.byte_range.start;
        let landed_at = desc.decode_time;
        let segment_index = desc.segment_index;
        self.next_segment_index = segment_index.saturating_add(1);
        let variant_index = desc.variant_index;
        let preroll = match compute_preroll_byte(
            &PrerollProbe {
                target,
                landed_at,
                segment_index,
            },
            self.segments.as_ref(),
            &priming,
        ) {
            Some(byte) => PrerollHint::Required(byte),
            None if priming.byte_margin == 0 => PrerollHint::NotNeeded,
            None if segment_index == 0 => PrerollHint::FirstSegment,
            None => PrerollHint::NotNeeded,
        };
        self.cursor = Some(SegmentCursor {
            segment_index,
            variant_index,
            read: SegmentReadState::new(desc.byte_range, self.byte_pool.get()),
            frames: None,
        });
        Ok(DemuxSeekOutcome::Landed {
            landed_at,
            preroll,
            landed_byte: Some(landed_byte),
        })
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}

/// Seek warm-up back-off duration for `codec` at `sample_rate`, derived
/// from the codec's pre-roll packet count. `None` when no pre-roll is
/// required (`packets == 0`) or the codec has no fixed access-unit size.
fn warmup_backoff(codec: AudioCodec, sample_rate: u32, priming: &CodecPriming) -> Option<Duration> {
    if priming.packets == 0 {
        return None;
    }
    let au = access_unit_frames(codec);
    if au == 0 {
        return None;
    }
    let frames = priming.packets.saturating_mul(au);
    Some(Duration::from_secs_f64(
        f64::from(frames) / f64::from(sample_rate.max(1)),
    ))
}

/// Where a seek landed relative to its target: the requested `target`
/// time, the `landed_at` time actually reached, and the `segment_index`
/// it landed in. Inputs to [`compute_preroll_byte`].
struct PrerollProbe {
    target: Duration,
    landed_at: Duration,
    segment_index: u32,
}

fn compute_preroll_byte(
    probe: &PrerollProbe,
    layout: &dyn ByteMap,
    priming: &CodecPriming,
) -> Option<u64> {
    if priming.byte_margin == 0 {
        return None;
    }
    if probe.landed_at < probe.target {
        return None;
    }
    let prev_index = probe.segment_index.checked_sub(1)?;
    let prev = layout.segment_at_index(prev_index)?;
    Some(prev.byte_range.start)
}

fn build_track_info(init: &Fmp4InitInfo, duration: Option<Duration>) -> TrackInfo {
    let extra_data = match &init.config {
        CodecConfig::Aac(bytes) | CodecConfig::Flac(bytes) => bytes.clone(),
    };
    TrackInfo {
        extra_data,
        duration,
        codec: init.codec,
        sample_rate: init.sample_rate,
        channels: init.channels,
        gapless: init.gapless,
    }
}

fn compute_duration(segments: &Arc<dyn ByteMap>) -> Option<Duration> {
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
