use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{AudioCodec, ByteMap, ConstructionGate, PendingReason};
use kithara_test_utils::kithara;

use super::{
    parsing::{Fmp4Frame, Fmp4InitInfo, parse_init, parse_segment_frames},
    reader::{FillStatus, LiveRange, SegmentReadState, fill_segment_buffer},
};
use crate::{
    PrepareWait,
    codec::{CodecPriming, access_unit_frames, seek_warmup_access_units},
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

struct SegmentSeek {
    target: Duration,
    reach: Duration,
    skip_to: Option<Duration>,
    priming: CodecPriming,
}

struct DecodedFrames {
    frames: Vec<Fmp4Frame>,
    next_index: usize,
}

/// fMP4 segment-aware demuxer.
pub(crate) struct Fmp4SegmentDemuxer<S> {
    segments: Arc<dyn ByteMap>,
    source: BoxedSource,
    construction_gate: Option<ConstructionGate>,
    init: Fmp4InitInfo,
    cursor: Option<SegmentCursor>,
    pending: Option<PendingReason>,
    seek: Option<SegmentSeek>,
    /// Host pool region. Each segment cursor draws its read buffer
    /// from here and returns it on drop, so a steady-state decode loop
    /// recycles one high-water allocation instead of mallocing per
    /// segment (and per variant-switch demuxer recreate).
    pools: PoolRegion<S>,
    track_info: TrackInfo,
    /// Index of the next segment to decode. Sequential playback advances
    /// this by one per segment; a seek sets it to the segment after the
    /// landing segment. Advancing by index (rather than by the previous
    /// segment's `byte_range.end`) keeps the decode cursor stable when the
    /// live byte-layout shifts mid-playback — a segment's size estimate is
    /// corrected to its real length on commit, and a byte-based advance
    /// would then re-resolve to the wrong segment and silently skip one.
    next_segment_index: u32,
}

impl<S> Fmp4SegmentDemuxer<S>
where
    S: HasPool<u8>,
{
    /// Take a cursor for the next segment, or say why there is none.
    ///
    /// [`ByteMap::segment_at_index`] answers about the layout as published so
    /// far, so `None` covers both "past the last segment" and "this one is not
    /// described yet". Reading the second as end-of-stream is how a variant
    /// holding six of its seven segments reported EOF on its first chunk,
    /// which failed the incoming generation of an ABR up-switch and discarded
    /// the transition for good. [`ByteMap::segment_count`] separates the two:
    /// an index the layout counts is a segment still owed.
    fn ensure_cursor(&mut self) -> EnsureCursor {
        if self.cursor.is_some() {
            return EnsureCursor::Ready;
        }
        let Some(desc) = self.segments.segment_at_index(self.next_segment_index) else {
            return if self
                .segments
                .segment_count()
                .is_none_or(|count| self.next_segment_index >= count)
            {
                EnsureCursor::Eof
            } else {
                EnsureCursor::Pending
            };
        };
        self.next_segment_index = desc.segment_index.saturating_add(1);
        self.cursor = Some(SegmentCursor {
            read: SegmentReadState::new(desc.byte_range, self.pools.get::<u8>()),
            frames: None,
            segment_index: desc.segment_index,
            variant_index: desc.variant_index,
        });
        EnsureCursor::Ready
    }

    /// Fill and parse the cursor's segment. After a seek, a segment whose own
    /// first frame starts after the seek reach hands the cursor to the
    /// segment before it; the index only descends, so the walk ends at the
    /// first segment.
    fn fill_cursor(&mut self) -> DecodeResult<FillStatus> {
        loop {
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
            if matches!(status, FillStatus::Pending(_)) {
                return Ok(status);
            }
            let frames = parse_segment_frames(&self.init, &cursor.read.buffer)?;
            let starts_late = self.seek.as_ref().is_some_and(|seek| {
                frames.first().is_some_and(|frame| {
                    ticks_to_duration(frame.decode_time, self.init.timescale) > seek.reach
                })
            });
            if starts_late
                && let Some(earlier) = cursor
                    .segment_index
                    .checked_sub(1)
                    .and_then(|index| segments.segment_at_index(index))
            {
                self.next_segment_index = cursor.segment_index;
                *cursor = SegmentCursor {
                    read: SegmentReadState::new(earlier.byte_range, self.pools.get::<u8>()),
                    frames: None,
                    segment_index: earlier.segment_index,
                    variant_index: earlier.variant_index,
                };
                continue;
            }
            let next_index = self
                .seek
                .as_ref()
                .and_then(|seek| seek.skip_to)
                .map_or(0, |skip| {
                    frame_at_or_before(&frames, skip, self.init.timescale)
                });
            cursor.frames = Some(DecodedFrames { frames, next_index });
            return Ok(status);
        }
    }

    /// Parse the init segment selected by the source's [`kithara_stream::Source::byte_map`].
    ///
    /// # Errors
    /// Returns [`DecodeError::InvalidData`] for missing, incomplete or malformed init data.
    /// A deferred init read returns [`DecodeError::Interrupted`]; retry when the source is ready.
    pub(crate) fn open(
        mut source: BoxedSource,
        segments: Arc<dyn ByteMap>,
        pools: PoolRegion<S>,
        construction_gate: Option<ConstructionGate>,
    ) -> DecodeResult<Self> {
        let init_range = segments.init_segment_range();
        if init_range.is_empty() {
            return Err(DecodeError::InvalidData {
                detail: "HLS init segment range not announced",
            });
        }
        let mut init_state = SegmentReadState::new(init_range, pools.get::<u8>());
        if let FillStatus::Pending(_) = fill_segment_buffer(
            &mut source,
            &mut init_state,
            LiveRange::Init(segments.as_ref()),
        )? {
            return Err(DecodeError::Interrupted);
        }
        let init = parse_init(&init_state.buffer, &pools)?;
        let duration = compute_duration(&segments);
        let track_info = build_track_info(&init, duration);
        Ok(Self {
            init,
            track_info,
            source,
            construction_gate,
            segments,
            pools,
            next_segment_index: 0,
            cursor: None,
            seek: None,
            pending: Some(PendingReason::Retry),
        })
    }
}

enum EnsureCursor {
    Ready,
    /// The layout has not described this index yet and has not published a
    /// final extent, so the segment after it is still owed.
    Pending,
    Eof,
}

impl<S> Demuxer for Fmp4SegmentDemuxer<S>
where
    S: HasPool<u8> + Send + Sync,
{
    delegate::delegate! {
        to self.cursor {
            #[expr($.map(|cursor| cursor.segment_index))]
            #[call(as_ref)]
            fn current_segment_index(&self) -> Option<u32>;
            #[expr($.map(|cursor| cursor.variant_index))]
            #[call(as_ref)]
            fn current_variant_index(&self) -> Option<usize>;
        }
    }

    fn duration(&self) -> Option<crate::DecoderDuration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        self.prepare_frame()?;
        self.next_frame_prepared()
    }

    #[kithara::probe]
    fn next_frame_prepared(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        if let Some(reason) = self.pending {
            return Ok(DemuxOutcome::Pending(reason));
        }
        let Some(cursor) = self.cursor.as_mut() else {
            return Ok(DemuxOutcome::Eof);
        };
        let Some(frames) = cursor.frames.as_mut() else {
            return Ok(DemuxOutcome::Pending(PendingReason::Retry));
        };
        let Some(frame) = frames.frames.get(frames.next_index).copied() else {
            return Ok(DemuxOutcome::Pending(PendingReason::Retry));
        };
        frames.next_index += 1;
        let pts = ticks_to_duration(frame.decode_time, self.init.timescale);
        let dur = ticks_to_duration(u64::from(frame.duration), self.init.timescale);
        let data = &cursor.read.buffer[frame.offset..frame.offset + frame.size];
        Ok(DemuxOutcome::Frame(Frame {
            data,
            pts,
            duration: dur,
            packet_desc: &[],
        }))
    }

    fn prepare_frame(&mut self) -> DecodeResult<()> {
        if self.seek.is_some() {
            return Ok(());
        }
        loop {
            if self.cursor.as_ref().is_some_and(|cursor| {
                cursor
                    .frames
                    .as_ref()
                    .is_some_and(|frames| frames.next_index >= frames.frames.len())
            }) {
                self.cursor = None;
            }
            match self.ensure_cursor() {
                EnsureCursor::Pending => {
                    self.pending = Some(PendingReason::Retry);
                    return Ok(());
                }
                EnsureCursor::Eof => {
                    self.pending = None;
                    return Ok(());
                }
                EnsureCursor::Ready => {}
            }
            match self.fill_cursor()? {
                FillStatus::Pending(reason) => {
                    self.pending = Some(reason);
                    return Ok(());
                }
                FillStatus::Ready => self.pending = None,
            }
            if self.cursor.as_ref().is_some_and(|cursor| {
                cursor
                    .frames
                    .as_ref()
                    .is_some_and(|frames| frames.next_index < frames.frames.len())
            }) {
                return Ok(());
            }
        }
    }

    /// The seek target backs off by the SBR/PS pre-roll warmup before locating the covering
    /// segment.
    fn seek(&mut self, target: Duration, priming: CodecPriming) -> DecodeResult<DemuxSeekOutcome> {
        self.seek_with_wait(target, priming, PrepareWait::Poll)
    }

    fn seek_with_wait(
        &mut self,
        target: Duration,
        priming: CodecPriming,
        wait: PrepareWait,
    ) -> DecodeResult<DemuxSeekOutcome> {
        let seek_target =
            warmup_backoff(self.track_info.codec, self.track_info.sample_rate, &priming)
                .map_or(target, |backoff| target.saturating_sub(backoff));
        let Some(desc) = self.segments.segment_at_time(seek_target) else {
            return Err(DecodeError::SeekFailed {
                detail: "no segment covers the requested seek time",
            });
        };
        if let Some(crate::DecoderDuration::MediaEnd(duration)) = self.track_info.duration
            && desc.decode_time >= duration
        {
            self.seek = None;
            self.cursor = None;
            self.pending = None;
            self.next_segment_index = desc.segment_index.saturating_add(1);
            return Ok(DemuxSeekOutcome::PastEof { duration });
        }
        let skip_to = intra_segment_landing(
            seek_target,
            &priming,
            self.track_info.codec,
            self.track_info.sample_rate,
        );
        self.next_segment_index = desc.segment_index.saturating_add(1);
        self.cursor = Some(SegmentCursor {
            read: SegmentReadState::new(desc.byte_range, self.pools.get::<u8>()),
            frames: None,
            segment_index: desc.segment_index,
            variant_index: desc.variant_index,
        });
        self.seek = Some(SegmentSeek {
            target,
            reach: seek_target,
            skip_to: skip_to.filter(|skip| *skip > desc.decode_time),
            priming,
        });
        self.prepare_seek_with_wait(wait)?
            .ok_or(DecodeError::InvalidData {
                detail: "fMP4 seek operation disappeared before resolution",
            })
    }

    fn prepare_seek(&mut self) -> DecodeResult<Option<DemuxSeekOutcome>> {
        self.prepare_seek_with_wait(PrepareWait::Poll)
    }

    fn prepare_seek_with_wait(
        &mut self,
        wait: PrepareWait,
    ) -> DecodeResult<Option<DemuxSeekOutcome>> {
        if self.seek.is_none() {
            return Ok(None);
        }
        let gate = self.construction_gate.clone();
        let _scope = gate
            .as_ref()
            .filter(|_| wait == PrepareWait::Poll)
            .map(ConstructionGate::nonblocking);
        match self.fill_cursor()? {
            FillStatus::Pending(reason) => {
                self.pending = Some(reason);
                return Ok(Some(DemuxSeekOutcome::Pending(reason)));
            }
            FillStatus::Ready => self.pending = None,
        }
        let seek = self.seek.take().ok_or(DecodeError::InvalidData {
            detail: "fMP4 seek operation disappeared after reading",
        })?;
        let cursor = self.cursor.as_ref().ok_or(DecodeError::InvalidData {
            detail: "fMP4 seek cursor disappeared after reading",
        })?;
        let desc = self.segments.segment_at_index(cursor.segment_index).ok_or(
            DecodeError::InvalidData {
                detail: "resolved fMP4 seek segment is missing from the layout",
            },
        )?;
        let preroll = match compute_preroll_byte(
            &PrerollProbe {
                landed_at: desc.decode_time,
                target: seek.target,
                segment_index: cursor.segment_index,
            },
            self.segments.as_ref(),
            &seek.priming,
        ) {
            Some(byte) => PrerollHint::Required(byte),
            None if seek.priming.byte_margin == 0 => PrerollHint::NotNeeded,
            None if cursor.segment_index == 0 => PrerollHint::FirstSegment,
            None => PrerollHint::NotNeeded,
        };
        Ok(Some(DemuxSeekOutcome::Landed {
            landed_at: desc.decode_time,
            landed_byte: Some(cursor.read.range.start),
            preroll,
        }))
    }

    fn media_origin(&self) -> Option<Duration> {
        Some(Duration::ZERO)
    }

    fn track_info(&self) -> &TrackInfo {
        &self.track_info
    }
}

/// Choose an intra-segment landing only when the codec declares no [`CodecPriming`].
/// Declared pre-roll is already spent by landing in an earlier segment; an arbitrary frame
/// would break it and cause HE-AAC phase drift. Self-priming codecs use generic warm-up units.
fn intra_segment_landing(
    seek_target: Duration,
    priming: &CodecPriming,
    codec: AudioCodec,
    sample_rate: u32,
) -> Option<Duration> {
    if priming.packets != 0 || priming.byte_margin != 0 || priming.frames != 0 {
        return None;
    }
    let frames = seek_warmup_access_units(codec).saturating_mul(access_unit_frames(codec));
    if frames == 0 {
        return None;
    }
    let warmup = Duration::from_secs_f64(f64::from(frames) / f64::from(sample_rate.max(1)));
    Some(seek_target.saturating_sub(warmup))
}

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
    landed_at: Duration,
    target: Duration,
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
    let extra_data = init.config.as_ref().to_vec();
    TrackInfo {
        extra_data,
        duration: duration.map(crate::DecoderDuration::MediaEnd),
        codec: init.codec,
        sample_rate: init.sample_rate,
        channels: init.channels,
        gapless: init.gapless,
    }
}

fn compute_duration(segments: &Arc<dyn ByteMap>) -> Option<Duration> {
    let last = segments.segment_at_time(Duration::from_secs(u64::MAX / 2))?;
    last.decode_time.checked_add(last.duration)
}

/// Index of the first frame whose decode time covers `skip`.
///
/// Frames are in decode order, so the answer is the last frame that starts at
/// or before `skip`: starting one frame later would drop audio the caller
/// asked for. A `skip` past the segment's last frame keeps that last frame,
/// leaving the cursor with something to emit rather than an empty segment.
fn frame_at_or_before(frames: &[Fmp4Frame], skip: Duration, timescale: u32) -> usize {
    let started_by_skip =
        |frame: &Fmp4Frame| ticks_to_duration(frame.decode_time, timescale) <= skip;
    frames
        .partition_point(started_by_skip)
        .saturating_sub(1)
        .min(frames.len().saturating_sub(1))
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
