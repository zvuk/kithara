use kithara_bufpool::{HasPool, PoolRegion};
use kithara_platform::{sync::Arc, time::Duration};
use kithara_stream::{AudioCodec, ByteMap, PendingReason, ReaderInput, SegmentDescriptor};
use kithara_test_utils::kithara;

use super::{
    parsing::{Fmp4Frame, Fmp4InitInfo, parse_init, parse_segment_frames},
    reader::{FillStatus, LiveRange, SegmentReadState, fill_segment_buffer},
};
use crate::{
    codec::{CodecPriming, access_unit_frames, seek_warmup_access_units},
    demuxer::{DemuxOutcome, DemuxSeekOutcome, Demuxer, Frame, PrerollHint, TrackInfo},
    error::{DecodeError, DecodeResult},
    traits::BoxedSource,
};

pub(crate) const REQUIRED_INPUT: ReaderInput = ReaderInput::InitOnly;

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
pub(crate) struct Fmp4SegmentDemuxer<S> {
    segments: Arc<dyn ByteMap>,
    source: BoxedSource,
    init: Fmp4InitInfo,
    cursor: Option<SegmentCursor>,
    pending: Option<PendingReason>,
    /// Decode time the landing segment must be advanced to after a seek.
    ///
    /// A segment is the smallest unit the layout can address, so a seek lands
    /// on its first frame however far into it the target sits. Decoding the
    /// frames in between only to drop them costs a whole segment of codec work
    /// per seek and publishes nothing while it runs. The frame table parsed
    /// with the segment already carries each frame's decode time, so the
    /// cursor starts at the frame covering the target instead — backed off by
    /// the codec's own declared pre-roll, which is what [`warmup_backoff`]
    /// subtracts. Cleared by the first segment parsed after the seek.
    seek_skip_to: Option<Duration>,
    seek_target: Option<SegmentDescriptor>,
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
            let next_index = self.seek_skip_to.take().map_or(0, |skip| {
                frame_at_or_before(&frames, skip, self.init.timescale)
            });
            cursor.frames = Some(DecodedFrames { frames, next_index });
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
        pools: PoolRegion<S>,
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
            segments,
            pools,
            next_segment_index: 0,
            cursor: None,
            seek_target: None,
            seek_skip_to: None,
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
    fn current_segment_index(&self) -> Option<u32> {
        self.seek_target.as_ref().map_or_else(
            || self.cursor.as_ref().map(|cursor| cursor.segment_index),
            |desc| Some(desc.segment_index),
        )
    }

    fn current_variant_index(&self) -> Option<usize> {
        self.seek_target.as_ref().map_or_else(
            || self.cursor.as_ref().map(|cursor| cursor.variant_index),
            |desc| Some(desc.variant_index),
        )
    }

    fn duration(&self) -> Option<Duration> {
        self.track_info.duration
    }

    fn next_frame(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        self.prepare_frame()?;
        self.next_frame_prepared()
    }

    #[kithara::probe]
    fn next_frame_prepared(&mut self) -> DecodeResult<DemuxOutcome<'_>> {
        if self.seek_target.is_some() {
            return Ok(DemuxOutcome::Pending(PendingReason::Retry));
        }
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
        if let Some(desc) = self.seek_target.take() {
            self.cursor = Some(SegmentCursor {
                read: SegmentReadState::new(desc.byte_range, self.pools.get::<u8>()),
                frames: None,
                segment_index: desc.segment_index,
                variant_index: desc.variant_index,
            });
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
        let seek_target =
            warmup_backoff(self.track_info.codec, self.track_info.sample_rate, &priming)
                .map_or(target, |backoff| target.saturating_sub(backoff));
        let Some(desc) = self.segments.segment_at_time(seek_target) else {
            return Err(DecodeError::SeekFailed {
                detail: "no segment covers the requested seek time",
            });
        };
        if let Some(duration) = self.track_info.duration
            && desc.decode_time >= duration
        {
            self.seek_skip_to = None;
            return Ok(DemuxSeekOutcome::PastEof { duration });
        }
        let landed_byte = desc.byte_range.start;
        let landed_at = desc.decode_time;
        let segment_index = desc.segment_index;
        self.next_segment_index = segment_index.saturating_add(1);
        let preroll = match compute_preroll_byte(
            &PrerollProbe {
                landed_at,
                target,
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
        let skip_to = intra_segment_landing(
            seek_target,
            &priming,
            self.track_info.codec,
            self.track_info.sample_rate,
        );
        self.seek_skip_to = skip_to.filter(|skip| *skip > landed_at);
        self.seek_target = Some(desc);
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
/// Where inside the landing segment the decode run may start, or `None` when
/// it must start at the segment's first frame.
///
/// Starting at the frame covering the target — rather than replaying the
/// whole segment to reach it — is only the demuxer's call to make for a codec
/// that asks for no back-off of its own. A codec that declares
/// [`CodecPriming`] states its pre-roll in packets and bytes *of the
/// container*, and `warmup_backoff` already spends it by landing in an
/// earlier segment; feeding such a codec from an arbitrary frame instead
/// breaks the contract it declared, and the Apple HE-AACv2 path answers with
/// audible phase drift. A codec that declares none primes itself, and needs
/// only the generic run-up of [`seek_warmup_access_units`].
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
