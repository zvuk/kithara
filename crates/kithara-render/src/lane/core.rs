//! Commands executed at segment-relative output frames.
use std::{
    cmp::Ordering,
    convert::Infallible,
    num::{NonZeroU32, NonZeroU128, NonZeroUsize},
};

use kithara_audio::{AudioSource, TrackFailureKind};
use kithara_bufpool::HasPool;
use kithara_command::{Inbox, Protocol, Seq};
use kithara_dsp::param::SmootherConfig;
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioChunkInfo, AudioSpec, FrameCount, SegmentId, SourceSpan};
use kithara_sync::SpeedCurve;
use kithara_warp::{StretchKind, WarpRenderError, WarpRenderer};
use num_traits::ToPrimitive;

#[path = "commands.rs"]
mod commands;

/// Commands and receipts of one producer lane.
#[derive(Debug)]
pub enum LaneProtocol {}

/// An output frame within a lane segment.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct LaneFrame {
    pub segment: SegmentId,
    pub frame: u64,
}

/// A change executed at the lane's output cursor.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum LaneCommand {
    SetSpeed(SpeedCurve),
    Jump {
        to: Duration,
    },
    Segment {
        id: SegmentId,
        from: Duration,
        speed: SpeedCurve,
    },
    SetKeylock(bool),
    SetBackend(StretchKind),
    SetHostRate {
        id: SegmentId,
        rate: NonZeroU32,
    },
}

/// Latency of the resulting engine and the segment whose preload is admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaneApplied {
    /// First pre-Warp input geometry, present when preparation observed new PCM.
    pub source_basis: Option<AudioChunkInfo>,
    pub engine_latency: FrameCount,
    pub ready: Option<SegmentId>,
}

impl Protocol for LaneProtocol {
    type Applied = LaneApplied;
    type Clock = LaneFrame;
    type Command = LaneCommand;
    type Refusal = TrackFailureKind;
    type Target = Infallible;

    fn frames_since(at: LaneFrame, start: LaneFrame) -> Option<u64> {
        match at.segment.cmp(&start.segment) {
            Ordering::Less => None,
            Ordering::Equal => at.frame.checked_sub(start.frame),
            Ordering::Greater => Some(u64::MAX),
        }
    }
}

#[derive(Clone, Copy)]
enum Jump {
    Down {
        to: Duration,
        start: u64,
        frames: usize,
        gain: f32,
    },
    Up {
        to: Option<Duration>,
        start: u64,
        frames: usize,
    },
}

impl Jump {
    fn gain(self, at: u64) -> f32 {
        let (start, frames) = match self {
            Self::Down { start, frames, .. } | Self::Up { start, frames, .. } => (start, frames),
        };
        let progress = at.saturating_sub(start).to_f32().unwrap_or(f32::MAX)
            / frames.max(1).to_f32().unwrap_or(f32::MAX);
        match self {
            Self::Down { gain, .. } => gain * (1.0 - progress.min(1.0)),
            Self::Up { .. } => progress.min(1.0),
        }
    }
    fn output_limit(self, at: u64) -> usize {
        match self {
            Self::Down { start, frames, .. } => frames
                .saturating_sub(usize::try_from(at.saturating_sub(start)).unwrap_or(usize::MAX)),
            Self::Up { .. } => usize::MAX,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum LaneChange {
    None,
    Controls,
    Source,
}

enum Preload {
    Filling(usize),
    Ready,
}

/// Command ownership and startup settings of a producer lane.
pub struct LaneSetup {
    pub inbox: Inbox<LaneProtocol>,
    pub preload_chunks: NonZeroUsize,
    pub declick: SmootherConfig,
}

#[derive(Debug)]
struct Preparation {
    seq: Seq,
    at: LaneFrame,
    entry: LaneFrame,
    source: Option<AudioChunkInfo>,
    requested_entry: Option<Duration>,
    render_revision: u64,
    latency: FrameCount,
}

impl Preparation {
    fn accepts(&self, meta: &AudioChunkInfo) -> bool {
        let Some(span) = meta.source_span else {
            return false;
        };
        if meta.segment != self.entry.segment
            || meta.lane_frame != self.entry.frame
            || meta.render_revision != self.render_revision
            || span.render_revision() != self.render_revision
            || span.output_frames() != u64::from(meta.frames)
        {
            return false;
        }
        self.source.is_some_and(|source| {
            let expected = source.source_geometry().and_then(|basis| {
                let (numerator, denominator) =
                    source_entry(basis, self.requested_entry, source.visible_origin)?;
                Some((
                    numerator.checked_mul(u128::from(source.spec.sample_rate.get()))?,
                    NonZeroU128::new(
                        denominator
                            .get()
                            .checked_mul(u128::from(basis.sample_rate().get()))?,
                    )?,
                ))
            });
            span.sample_rate() == source.spec.sample_rate
                && expected
                    .and_then(|position| {
                        span.scaled_from(position, (0, NonZeroU128::MIN), (1, NonZeroU128::MIN))
                    })
                    .and_then(|relative| relative.source_ratio_at(0))
                    == Some((0, NonZeroU128::MIN))
        })
    }
}

pub(crate) struct Lane {
    inbox: Inbox<LaneProtocol>,
    state: LaneState,
}

struct LaneState {
    cursor: LaneFrame,
    position: Option<Duration>,
    preload_chunks: NonZeroUsize,
    declick: SmootherConfig,
    preload: Preload,
    pending: Option<Preparation>,
    deferred: Option<Seq>,
    ended: bool,
    parked: Option<(LaneFrame, Option<Duration>)>,
    jump: Option<Jump>,
}

impl Lane {
    pub(crate) fn new(
        inbox: Inbox<LaneProtocol>,
        preload_chunks: NonZeroUsize,
        declick: SmootherConfig,
    ) -> Self {
        Self {
            inbox,
            state: LaneState {
                cursor: LaneFrame::default(),
                position: None,
                preload_chunks,
                declick,
                preload: Preload::Filling(0),
                pending: None,
                deferred: None,
                ended: false,
                parked: None,
                jump: None,
            },
        }
    }

    delegate::delegate! {
        to self {
            #[expr(self.state.cursor)]
            pub(crate) const fn cursor(&self) -> LaneFrame;
            #[expr(self.state.position)]
            pub(crate) const fn position(&self) -> Option<Duration>;
        }
    }

    pub(crate) fn declick_frames(&self, sample_rate: NonZeroU32) -> FrameCount {
        crate::rt::declick_frame_count(self.state.declick, sample_rate)
    }

    pub(crate) fn is_preloaded(&self) -> bool {
        matches!(self.state.preload, Preload::Ready)
    }

    fn finish_pending(&mut self, ready: Option<SegmentId>) {
        if let Some(pending) = self.state.pending.take()
            && let Some(due) = self.inbox.resume(pending.seq, pending.at, pending.at)
        {
            tracing::debug!(
                seq = pending.seq.get(),
                actual_apply_segment = pending.at.segment.get(),
                actual_apply_frame = pending.at.frame,
                prepared_segment = self.state.cursor.segment.get(),
                prepared_frontier = self.state.cursor.frame,
                ready_segment = ?ready,
                "lane preparation finished"
            );
            due.apply(LaneApplied {
                source_basis: ready.and(pending.source),
                engine_latency: pending.latency,
                ready,
            });
        }
    }

    pub(crate) fn observe_input<S: HasPool<f32>>(
        &mut self,
        warp: &mut WarpRenderer<S>,
        meta: AudioChunkInfo,
        visible_origin: Option<Duration>,
    ) -> Result<(), WarpRenderError> {
        if meta.frames > 0
            && matches!(self.state.preload, Preload::Filling(0))
            && let Some(pending) = &mut self.state.pending
            && pending.source.is_none()
        {
            if let Some(to) = pending.requested_entry {
                self.state.position = Some(reanchor_source(warp, meta, to, visible_origin)?);
            }
            pending.source = Some(meta);
        }
        Ok(())
    }

    pub(crate) fn check_admission(&self, meta: &AudioChunkInfo) -> Result<(), TrackFailureKind> {
        if matches!(self.state.preload, Preload::Filling(0))
            && let Some(pending) = self.state.pending.as_ref()
            && !pending.accepts(meta)
        {
            tracing::error!(?pending, incoming = ?meta, "prepared PCM violates lane entry");
            return Err(TrackFailureKind::Render);
        }
        Ok(())
    }

    pub(crate) fn reject_preparation(&mut self, failure: TrackFailureKind) {
        if let Some(pending) = self.state.pending.take()
            && let Some(due) = self.inbox.resume(pending.seq, pending.at, pending.at)
        {
            due.refuse(failure);
        }
    }

    pub(crate) fn admitted(&mut self) {
        if let Preload::Filling(admitted) = &mut self.state.preload {
            *admitted = admitted.saturating_add(1);
            if *admitted >= self.state.preload_chunks.get() {
                self.finish_preload();
            }
        }
    }

    pub(crate) fn upstream_parked(&mut self) {
        if matches!(self.state.preload, Preload::Filling(1..)) {
            self.finish_preload();
        }
    }

    pub(crate) fn finish_preload(&mut self) {
        self.state.preload = Preload::Ready;
        self.finish_pending(Some(self.state.cursor.segment));
    }

    pub(crate) fn finish_segment(&mut self) {
        self.state.ended = true;
    }

    pub(crate) fn prepare_quantum<S: HasPool<f32>>(
        &mut self,
        warp: &mut WarpRenderer<S>,
        meta: AudioChunkInfo,
        remaining: usize,
        visible_origin: Option<Duration>,
    ) -> Result<FrameCount, WarpRenderError> {
        let target = match self.state.jump {
            Some(Jump::Up { to: Some(to), .. }) => {
                Some(visible_origin.map_or(to, |origin| origin.max(to)))
            }
            _ => None,
        };
        if let Some(to) = target {
            reanchor_source(warp, meta, to, visible_origin)?;
        }
        let frames = warp.prepare_source_window(meta, remaining, self.output_limit())?;
        if let Some(Jump::Up { to, .. }) = &mut self.state.jump
            && to.take().is_some()
        {
            self.state.position = target;
        }
        Ok(frames)
    }

    pub(crate) fn output_limit(&self) -> usize {
        let due = self
            .inbox
            .frames_until_due(self.state.cursor)
            .map_or(usize::MAX, |frames| {
                usize::try_from(frames).unwrap_or(usize::MAX)
            });
        self.state.jump.map_or(due, |jump| {
            due.min(jump.output_limit(self.state.cursor.frame))
        })
    }

    pub(crate) fn stamp(&mut self, chunk: &mut AudioChunk) {
        chunk.meta.segment = self.state.cursor.segment;
        chunk.meta.lane_frame = self.state.cursor.frame;
        let channels = usize::from(chunk.spec().channels.max(1));
        if let Some(jump) = &self.state.jump {
            let whole = chunk.samples.len() / channels * channels;
            let mut gains = [0.0; crate::consts::FADE_CHUNK];
            for (block, samples) in chunk.samples[..whole]
                .chunks_mut(crate::consts::FADE_CHUNK)
                .enumerate()
            {
                let start = block * crate::consts::FADE_CHUNK;
                let end = start + samples.len();
                for frame in start / channels..=(end - 1) / channels {
                    let offset = u64::try_from(frame).unwrap_or(u64::MAX);
                    let gain = jump.gain(self.state.cursor.frame.saturating_add(offset));
                    let from = (frame * channels).saturating_sub(start);
                    let to = ((frame + 1) * channels).min(end) - start;
                    gains[from..to].fill(gain);
                }
                let gains = &gains[..samples.len()];
                kithara_dsp::multiply_in_place(samples, gains);
            }
        }
        let frames = u64::try_from(chunk.frames()).unwrap_or(u64::MAX);
        self.state.cursor.frame = self.state.cursor.frame.saturating_add(frames);
        if let Some(position) = chunk
            .meta
            .source_span
            .and_then(|span| span.position_at(span.output_frames()))
        {
            self.state.position = Some(position);
        }
        if matches!(self.state.jump, Some(Jump::Up { start, frames, .. })
            if self.state.cursor.frame.saturating_sub(start) >= u64::try_from(frames).unwrap_or(u64::MAX))
        {
            self.state.jump = None;
        }
    }

    fn land_jump<T, S>(
        &mut self,
        source: &mut T,
        warp: &mut WarpRenderer<S>,
        spec: AudioSpec,
    ) -> Result<bool, TrackFailureKind>
    where
        T: AudioSource<Chunk = AudioChunk>,
        S: HasPool<f32>,
    {
        let Some(Jump::Down {
            to, start, frames, ..
        }) = self.state.jump
        else {
            return Ok(false);
        };
        let frames = u64::try_from(frames).unwrap_or(u64::MAX);
        if self.state.cursor.frame.saturating_sub(start) < frames {
            return Ok(false);
        }
        let visible_origin = source.visible_origin();
        let to = visible_origin.map_or(to, |origin| origin.max(to));
        self.state.position = landing_position(source.seek_for_render(to))?;
        warp.reset();
        let next_spec = source.prepare_deferred().unwrap_or(spec);
        warp.prepare_engine_latency(next_spec)
            .map_err(|_| TrackFailureKind::Render)?;
        self.state.jump = Some(Jump::Up {
            to: Some(to),
            start: self.state.cursor.frame,
            frames: usize::try_from(frames).unwrap_or(usize::MAX),
        });
        Ok(true)
    }
}

fn source_entry(
    basis: SourceSpan,
    requested: Option<Duration>,
    visible_origin: Option<Duration>,
) -> Option<(u128, NonZeroU128)> {
    let Some(to) = requested.map(|to| visible_origin.map_or(to, |origin| origin.max(to))) else {
        return basis.source_ratio_at(0);
    };
    if basis.position_at(0) == Some(to) {
        return basis.source_ratio_at(0);
    }
    Some((
        to.as_nanos()
            .checked_mul(u128::from(basis.sample_rate().get()))?,
        NonZeroU128::new(1_000_000_000)?,
    ))
}

fn reanchor_source<S: HasPool<f32>>(
    warp: &mut WarpRenderer<S>,
    meta: AudioChunkInfo,
    to: Duration,
    visible_origin: Option<Duration>,
) -> Result<Duration, WarpRenderError> {
    let to = visible_origin.map_or(to, |origin| origin.max(to));
    let basis = meta
        .source_geometry()
        .ok_or(WarpRenderError::InvalidSourceGeometry)?;
    if !basis.is_affine() {
        return Err(WarpRenderError::UnsupportedSourceGeometry);
    }
    let entry = source_entry(basis, Some(to), visible_origin)
        .ok_or(WarpRenderError::InvalidSourceGeometry)?;
    let position = basis
        .output_ratio_at_source(meta.frame_offset, entry)
        .ok_or_else(|| {
            tracing::error!(
                ?meta,
                ?to,
                ?visible_origin,
                ?basis,
                "source entry cannot be projected to decoded PCM"
            );
            WarpRenderError::InvalidSourceGeometry
        })?;
    if position.0 / position.1.get() < u128::from(meta.frame_offset) {
        tracing::error!(
            ?meta,
            ?to,
            ?visible_origin,
            ?basis,
            ?position,
            "source entry precedes retained decoded PCM"
        );
        return Err(WarpRenderError::InvalidSourceGeometry);
    }
    warp.reanchor_source(meta, position)?;
    Ok(to)
}

fn landing_position(
    outcome: Result<kithara_audio::SeekOutcome, kithara_audio::AudioReadError>,
) -> Result<Option<Duration>, TrackFailureKind> {
    Ok(
        match outcome.map_err(|error| TrackFailureKind::from(&error))? {
            kithara_audio::SeekOutcome::Pending { .. } => None,
            kithara_audio::SeekOutcome::Landed { landed_at, .. } => Some(landed_at),
            kithara_audio::SeekOutcome::PastEof { duration, .. } => Some(duration),
        },
    )
}

#[cfg(test)]
mod tests {
    use kithara_audio::{AudioReadError, SeekOutcome, TrackStep, WaitingReason};
    use kithara_command::{Batch, ChannelConfig, Sender, When, channel};
    use kithara_dsp::param::SmootherConfig;
    use kithara_test_utils::kithara;
    use kithara_warp::{Warp, WarpConfig};

    use super::*;

    struct JumpSource;

    impl AudioSource for JumpSource {
        type Chunk = AudioChunk;

        fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
            Ok(SeekOutcome::Landed {
                target,
                landed_at: target,
            })
        }

        fn seek_for_render(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
            self.seek(target)
        }

        fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}

        fn host_sample_rate(&self) -> Option<NonZeroU32> {
            None
        }

        fn step_track(&mut self) -> TrackStep<AudioChunk> {
            TrackStep::Blocked(WaitingReason::Waiting)
        }
    }

    fn jump(sender: &mut Sender<LaneProtocol>) {
        sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![LaneCommand::Jump {
                        to: Duration::from_secs(1),
                    }],
                },
            )
            .expect("jump credit");
    }

    #[kithara::test]
    #[case::default_declick(0.005, 221)]
    #[case::configured_declick(0.020, 882)]
    fn a_jump_fades_down_for_the_mixers_configured_declick(
        #[case] seconds: f32,
        #[case] expected: usize,
    ) {
        let spec = AudioSpec::new(1, NonZeroU32::new(44_100).expect("rate"));
        let config = crate::rt::DeckMixerConfig::builder()
            .declick(SmootherConfig {
                smooth_seconds: seconds,
                ..SmootherConfig::default()
            })
            .build();
        assert_eq!(config.declick_frames(spec.sample_rate).get(), expected);
        let pools = crate::test_pools::pools();
        let (mut sender, inbox) = channel(ChannelConfig::builder().build());
        let mut lane = Lane::new(
            inbox,
            NonZeroUsize::new(1).expect("preload"),
            config.declick(),
        );
        let mut warp = Warp::new((), &WarpConfig::builder().build()).renderer(spec, pools);
        jump(&mut sender);
        lane.execute_due(&mut JumpSource, &mut warp, spec)
            .expect("jump accepted");
        assert_eq!(
            lane.output_limit(),
            expected,
            "Jump and mixer must share the configured, rounded declick length"
        );
        struct TerminalSource {
            requested: Option<Duration>,
        }
        impl AudioSource for TerminalSource {
            type Chunk = AudioChunk;
            fn seek(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
                self.requested = Some(target);
                let duration = Duration::from_secs(1);
                Ok(if target >= duration {
                    SeekOutcome::PastEof { target, duration }
                } else {
                    SeekOutcome::Landed {
                        target,
                        landed_at: target,
                    }
                })
            }
            fn seek_for_render(&mut self, target: Duration) -> Result<SeekOutcome, AudioReadError> {
                self.seek(target)
            }
            fn set_host_sample_rate(&mut self, _rate: NonZeroU32) {}
            fn host_sample_rate(&self) -> Option<NonZeroU32> {
                None
            }
            fn step_track(&mut self) -> TrackStep<AudioChunk> {
                TrackStep::Eof
            }
        }
        let requested = Duration::from_nanos(1_000_005_000);
        lane.state.jump = Some(Jump::Down {
            to: requested,
            start: lane.state.cursor.frame,
            frames: 0,
            gain: 1.0,
        });
        let mut source = TerminalSource { requested: None };
        assert!(
            lane.land_jump(&mut source, &mut warp, spec)
                .expect("unknown-end typed seek")
        );
        assert_eq!(source.requested, Some(requested));
        assert_eq!(lane.state.position, Some(Duration::from_secs(1)));
        let first = 44_101;
        let meta = AudioChunkInfo {
            spec,
            frame_offset: first,
            frames: 16,
            timestamp: spec.duration_for(first).expect("actual decoded timestamp"),
            end_timestamp: spec.duration_for(first + 16).expect("actual decoded end"),
            ..AudioChunkInfo::default()
        };
        lane.state.jump = Some(Jump::Up {
            to: Some(Duration::from_secs(1)),
            start: lane.state.cursor.frame,
            frames: expected,
        });
        assert!(matches!(
            lane.prepare_quantum(&mut warp, meta, 16, None),
            Err(WarpRenderError::InvalidSourceGeometry)
        ));
        assert!(matches!(
            lane.state.jump,
            Some(Jump::Up { to: Some(_), .. })
        ));
        lane.state.jump = Some(Jump::Up {
            to: Some(Duration::ZERO),
            start: lane.state.cursor.frame,
            frames: expected,
        });
        warp.prepare_engine_latency(spec)
            .expect("initial-head render engine");
        lane.prepare_quantum(&mut warp, meta, 16, Some(meta.timestamp))
            .expect("observed original visible head clamps a zero request");
        assert_eq!(lane.state.position, Some(meta.timestamp));
        assert!(matches!(lane.state.jump, Some(Jump::Up { to: None, .. })));
    }

    #[kithara::test]
    #[case::during_fade_down(false)]
    #[case::during_fade_up(true)]
    fn a_jump_during_another_jump_keeps_gain_continuous(#[case] during_up: bool) {
        let spec = AudioSpec::new(1, NonZeroU32::new(44_100).expect("rate"));
        let pools = crate::test_pools::pools();
        let (mut sender, inbox) = channel(ChannelConfig::builder().build());
        let mut lane = Lane::new(
            inbox,
            NonZeroUsize::new(1).expect("preload"),
            crate::consts::DEFAULT_DECLICK,
        );
        let mut warp = Warp::new((), &WarpConfig::builder().build()).renderer(spec, pools);
        jump(&mut sender);
        lane.execute_due(&mut JumpSource, &mut warp, spec)
            .expect("first jump");
        if during_up {
            let frames = lane.output_limit();
            let mut down = crate::worker::packet_tests::chunk(
                spec,
                SegmentId::FIRST,
                0,
                0,
                &vec![1.0; frames],
            );
            lane.stamp(&mut down);
            lane.execute_due(&mut JumpSource, &mut warp, spec)
                .expect("landing");
        }
        let mut before = crate::worker::packet_tests::chunk(
            spec,
            SegmentId::FIRST,
            lane.cursor().frame,
            0,
            &[1.0; 100],
        );
        lane.stamp(&mut before);
        let previous = before.samples[99];
        jump(&mut sender);
        lane.execute_due(&mut JumpSource, &mut warp, spec)
            .expect("second jump");
        let mut after = crate::worker::packet_tests::chunk(
            spec,
            SegmentId::FIRST,
            lane.cursor().frame,
            0,
            &[1.0],
        );
        lane.stamp(&mut after);
        assert!(
            (after.samples[0] - previous).abs() < 0.02,
            "retriggering Jump must not reset gain to unity: {previous} -> {}",
            after.samples[0]
        );
    }

    #[kithara::test]
    #[case::seek(false)]
    #[case::cancel_pending_repeat(true)]
    fn a_current_segment_command_only_preserves_the_cursor_when_it_cancels_a_repeat(
        #[case] pending_repeat: bool,
    ) {
        let spec = AudioSpec::new(1, NonZeroU32::new(44_100).expect("rate"));
        let (mut sender, inbox) = channel(ChannelConfig::builder().build());
        let mut lane = Lane::new(
            inbox,
            NonZeroUsize::new(1).expect("preload"),
            crate::consts::DEFAULT_DECLICK,
        );
        lane.state.cursor.frame = 64;
        let mut warp = Warp::new((), &WarpConfig::builder().build())
            .renderer(spec, crate::test_pools::pools());
        if pending_repeat {
            sender
                .send(
                    When::Deferred,
                    Batch {
                        basis: Vec::new(),
                        commands: vec![LaneCommand::Segment {
                            id: SegmentId::FIRST.next(),
                            from: Duration::ZERO,
                            speed: SpeedCurve::Constant(1.0),
                        }],
                    },
                )
                .expect("repeat credit");
        }
        sender
            .send(
                When::Next,
                Batch {
                    basis: Vec::new(),
                    commands: vec![LaneCommand::Segment {
                        id: SegmentId::FIRST,
                        from: Duration::from_secs(1),
                        speed: SpeedCurve::Constant(1.0),
                    }],
                },
            )
            .expect("segment credit");
        lane.execute_due(&mut JumpSource, &mut warp, spec)
            .expect("segment accepted");
        assert_eq!(lane.state.cursor.frame, if pending_repeat { 64 } else { 0 });
        assert_eq!(
            lane.state.position,
            if pending_repeat {
                None
            } else {
                Some(Duration::from_secs(1))
            }
        );
        assert!(lane.state.deferred.is_none());
        if pending_repeat {
            let receipts: Vec<_> = sender.receipts().collect();
            assert!(receipts.iter().any(|receipt| matches!(receipt.outcome(),
                kithara_command::Outcome::Applied { data, .. } if data.ready.is_none())));
            assert!(!lane.is_preloaded());
        }
    }

    #[kithara::test]
    fn jump_stamping_matches_scalar_pcm_and_cursor_for_interleaved_channels() {
        for (channels, shapes) in [
            (1, &[(0, 0), (1, 0), (257, 0), (513, 0)][..]),
            (2, &[(0, 0), (1, 0), (257, 0), (513, 0), (0, 1), (1, 1)][..]),
            (3, &[(0, 0), (1, 0), (257, 0), (513, 0), (0, 2), (1, 2)][..]),
            (
                6,
                &[(0, 0), (1, 0), (257, 0), (513, 0), (0, 5), (257, 5)][..],
            ),
            (257, &[(0, 0), (1, 0), (2, 0), (0, 256), (1, 256)][..]),
        ] {
            let spec = AudioSpec::new(channels, NonZeroU32::new(48_000).expect("rate"));
            let channels = usize::from(spec.channels);
            for frames in [0, 1, 513, (1 << 24) + 9] {
                for up in [false, true] {
                    for &(length, trailing) in shapes {
                        let (_sender, inbox) = channel(ChannelConfig::builder().build());
                        let mut lane =
                            Lane::new(inbox, NonZeroUsize::MIN, crate::consts::DEFAULT_DECLICK);
                        let start = 17_u64;
                        let at = start + if frames > 1 << 24 { 1 << 24 } else { 0 };
                        lane.state.cursor.frame = at;
                        lane.state.jump = Some(if up {
                            Jump::Up {
                                to: None,
                                start,
                                frames,
                            }
                        } else {
                            Jump::Down {
                                to: Duration::ZERO,
                                start,
                                frames,
                                gain: 0.75,
                            }
                        });
                        let samples: Vec<_> = [0.25_f32, -0.0, -0.75]
                            .into_iter()
                            .cycle()
                            .take(length * channels + trailing)
                            .collect();
                        let mut expected = samples.clone();
                        for (offset, frame) in expected.chunks_exact_mut(channels).enumerate() {
                            let position =
                                at.saturating_add(u64::try_from(offset).expect("offset"));
                            let progress =
                                position.saturating_sub(start).to_f32().expect("position")
                                    / frames.max(1).to_f32().expect("frames");
                            let gain = if up {
                                progress.min(1.0)
                            } else {
                                0.75 * (1.0 - progress.min(1.0))
                            };
                            for sample in frame {
                                *sample *= gain;
                            }
                        }
                        let mut chunk = crate::worker::packet_tests::chunk(
                            spec,
                            SegmentId::FIRST,
                            at,
                            0,
                            &samples,
                        );
                        lane.stamp(&mut chunk);
                        assert_eq!(
                            chunk
                                .samples
                                .iter()
                                .map(|x| x.to_bits())
                                .collect::<Vec<_>>(),
                            expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
                        );
                        let end = at + u64::try_from(length).expect("length");
                        assert_eq!(lane.cursor().frame, end);
                        assert_eq!(chunk.meta.lane_frame, at);
                        assert_eq!(
                            lane.state.jump.is_none(),
                            up && end.saturating_sub(start)
                                >= u64::try_from(frames).expect("frames")
                        );
                    }
                }
            }
        }
    }

    #[kithara::test]
    fn frames_since_orders_segments_before_offsets() {
        let start = LaneFrame {
            segment: SegmentId::FIRST.next(),
            frame: 8,
        };
        assert_eq!(
            LaneProtocol::frames_since(
                LaneFrame {
                    segment: SegmentId::FIRST,
                    frame: u64::MAX
                },
                start
            ),
            None
        );
        assert_eq!(LaneProtocol::frames_since(start, start), Some(0));
        assert_eq!(
            LaneProtocol::frames_since(LaneFrame { frame: 7, ..start }, start),
            None
        );
        assert_eq!(
            LaneProtocol::frames_since(LaneFrame { frame: 9, ..start }, start),
            Some(1)
        );
        assert_eq!(
            LaneProtocol::frames_since(
                LaneFrame {
                    segment: start.segment.next(),
                    frame: 0
                },
                start
            ),
            Some(u64::MAX)
        );
    }
}
