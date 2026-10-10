//! Commands executed at segment-relative output frames.
use std::{
    cmp::Ordering,
    convert::Infallible,
    num::{NonZeroU32, NonZeroUsize},
    task::{Context, Poll},
};

use kithara_audio::{AudioSource, TrackFailureKind};
use kithara_bufpool::HasPool;
use kithara_command::{Inbox, Protocol, Seq};
use kithara_dsp::param::SmootherConfig;
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioSpec, FrameCount, SegmentId};
use kithara_warp::{SpeedCurve, StretchKind, WarpRenderer};
use num_traits::ToPrimitive;

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
    pub engine_latency: FrameCount,
    pub ready: Option<SegmentId>,
}

impl Protocol for LaneProtocol {
    type Applied = LaneApplied;
    type Clock = LaneFrame;
    type Command = LaneCommand;
    type Refusal = Infallible;
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
        start: u64,
        frames: usize,
    },
}

impl Jump {
    fn gain(self, at: u64) -> f32 {
        let (start, frames) = match self {
            Self::Down { start, frames, .. } | Self::Up { start, frames } => (start, frames),
        };
        let progress = at.saturating_sub(start).to_f32().unwrap_or(f32::MAX)
            / frames.max(1).to_f32().unwrap_or(f32::MAX);
        match self {
            Self::Down { gain, .. } => gain * (1.0 - progress.min(1.0)),
            Self::Up { .. } => progress.min(1.0),
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

pub(crate) struct Lane {
    inbox: Inbox<LaneProtocol>,
    cursor: LaneFrame,
    position: Option<Duration>,
    preload_chunks: NonZeroUsize,
    declick: SmootherConfig,
    preload: Preload,
    pending: Option<(Seq, FrameCount)>,
    deferred: Option<Seq>,
    ended: bool,
    parked: Option<(LaneFrame, Duration)>,
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
        }
    }

    delegate::delegate! {
        to self {
            #[expr(self.cursor)]
            pub(crate) const fn cursor(&self) -> LaneFrame;
            #[expr(self.position)]
            pub(crate) const fn position(&self) -> Option<Duration>;
        }
    }

    pub(crate) fn declick_frames(&self, sample_rate: NonZeroU32) -> FrameCount {
        crate::rt::declick_frame_count(self.declick, sample_rate)
    }

    pub(crate) fn poll_commands(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.inbox.poll_drain(context)
    }

    pub(crate) fn is_preloaded(&self) -> bool {
        matches!(self.preload, Preload::Ready)
    }

    fn finish_pending(&mut self, ready: Option<SegmentId>) {
        if let Some((seq, latency)) = self.pending.take()
            && let Some(due) = self.inbox.resume(seq, self.cursor, self.cursor)
        {
            due.apply(LaneApplied {
                engine_latency: latency,
                ready,
            });
        }
    }

    pub(crate) fn admitted(&mut self) {
        if let Preload::Filling(admitted) = &mut self.preload {
            *admitted = admitted.saturating_add(1);
            if *admitted >= self.preload_chunks.get() {
                self.finish_preload();
            }
        }
    }

    pub(crate) fn upstream_parked(&mut self) {
        if matches!(self.preload, Preload::Filling(1..)) {
            self.finish_preload();
        }
    }

    pub(crate) fn finish_preload(&mut self) {
        self.preload = Preload::Ready;
        self.finish_pending(Some(self.cursor.segment));
    }

    pub(crate) fn finish_segment(&mut self) {
        self.ended = true;
    }

    pub(crate) fn execute_due<T, S>(
        &mut self,
        source: &mut T,
        warp: &mut WarpRenderer<S>,
        spec: AudioSpec,
    ) -> Result<LaneChange, TrackFailureKind>
    where
        T: AudioSource<Chunk = AudioChunk>,
        S: HasPool<f32>,
    {
        self.inbox.drain();
        loop {
            let next = match self.inbox.next_deferred() {
                Some(deferred) => deferred.park(),
                None => break,
            };
            let previous = self.deferred.replace(next.ok_or(TrackFailureKind::Render)?);
            if let Some(previous) = previous {
                drop(self.inbox.resume(previous, self.cursor, self.cursor));
            }
        }
        let mut changed = LaneChange::None;
        let declick_frames = self.declick_frames(spec.sample_rate).get();
        loop {
            let deferred = self.ended
                && self
                    .inbox
                    .frames_until_due(self.cursor)
                    .is_none_or(|frames| frames > 0);
            let Some(due) = (if deferred && self.deferred.is_some() {
                let seq = self.deferred.take().ok_or(TrackFailureKind::Render)?;
                self.inbox.resume(seq, self.cursor, self.cursor)
            } else {
                self.inbox.next_due(self.cursor, 1)
            }) else {
                break;
            };
            if changed == LaneChange::None {
                changed = LaneChange::Controls;
            }
            let revision = due.seq().get();
            let mut segment = None;
            let mut restored_same = false;
            let mut cancelled_deferred = None;
            for command in due.commands() {
                match command {
                    LaneCommand::SetSpeed(curve) => warp
                        .set_speed(curve.clone(), revision)
                        .map_err(|_| TrackFailureKind::Render)?,
                    LaneCommand::SetKeylock(on) => warp.set_keylock(*on),
                    LaneCommand::SetBackend(kind) => warp.set_backend(*kind),
                    LaneCommand::Jump { to } => {
                        let frames = declick_frames;
                        let gain = self.jump.map_or(1.0, |jump| jump.gain(self.cursor.frame));
                        self.jump = Some(Jump::Down {
                            to: *to,
                            start: self.cursor.frame,
                            frames,
                            gain,
                        });
                    }
                    LaneCommand::Segment { id, from, speed } => {
                        if *id == self.cursor.segment && self.deferred.is_some() {
                            restored_same = true;
                            cancelled_deferred = self.deferred.take();
                            continue;
                        }
                        let restored = self.parked.filter(|(cursor, _)| cursor.segment == *id);
                        if deferred {
                            self.parked = Some((self.cursor, self.position.unwrap_or(*from)));
                        } else {
                            self.parked = None;
                        }
                        cancelled_deferred = self.deferred.take();
                        self.cursor = restored.map_or(
                            LaneFrame {
                                segment: *id,
                                frame: 0,
                            },
                            |(cursor, _)| cursor,
                        );
                        let from = restored.map_or(*from, |(_, position)| position);
                        self.position = Some(landing_position(
                            source
                                .seek(from)
                                .map_err(|error| TrackFailureKind::from(&error))?,
                        ));
                        warp.reset();
                        warp.set_speed(speed.clone(), revision)
                            .map_err(|_| TrackFailureKind::Render)?;
                        self.jump = None;
                        self.ended = false;
                        segment = Some(*id);
                        changed = LaneChange::Source;
                    }
                    LaneCommand::SetHostRate { id, rate } => {
                        cancelled_deferred = self.deferred.take();
                        self.parked = None;
                        source.set_host_sample_rate(*rate);
                        warp.reset();
                        self.cursor = LaneFrame {
                            segment: *id,
                            frame: 0,
                        };
                        self.jump = None;
                        self.ended = false;
                        segment = Some(*id);
                        changed = LaneChange::Source;
                    }
                }
            }
            let next_spec = source.prepare_deferred().unwrap_or(spec);
            let latency = warp
                .prepare_engine_latency(next_spec)
                .map_err(|_| TrackFailureKind::Render)?;
            if segment.is_some() {
                let seq = due.defer();
                self.finish_pending(None);
                self.preload = Preload::Filling(0);
                self.pending = Some((seq, latency));
            } else {
                due.apply(LaneApplied {
                    engine_latency: latency,
                    ready: restored_same.then_some(self.cursor.segment),
                });
            }
            if let Some(seq) = cancelled_deferred {
                drop(self.inbox.resume(seq, self.cursor, self.cursor));
            }
        }
        if let Some(Jump::Down {
            to, start, frames, ..
        }) = self.jump
        {
            let frames = u64::try_from(frames).map_or(u64::MAX, |frames| frames);
            if self.cursor.frame.saturating_sub(start) >= frames {
                self.position = Some(landing_position(
                    source
                        .seek(to)
                        .map_err(|error| TrackFailureKind::from(&error))?,
                ));
                warp.reset();
                let next_spec = source.prepare_deferred().unwrap_or(spec);
                warp.prepare_engine_latency(next_spec)
                    .map_err(|_| TrackFailureKind::Render)?;
                self.jump = Some(Jump::Up {
                    start: self.cursor.frame,
                    frames: usize::try_from(frames).map_or(usize::MAX, |frames| frames),
                });
                changed = LaneChange::Source;
            }
        }
        Ok(changed)
    }

    pub(crate) fn output_limit(&self) -> usize {
        let due = self
            .inbox
            .frames_until_due(self.cursor)
            .map_or(usize::MAX, |frames| {
                usize::try_from(frames).map_or(usize::MAX, |frames| frames)
            });
        let jump = match self.jump {
            Some(Jump::Down { start, frames, .. }) => frames.saturating_sub(
                usize::try_from(self.cursor.frame.saturating_sub(start))
                    .map_or(usize::MAX, |frames| frames),
            ),
            _ => usize::MAX,
        };
        due.min(jump)
    }

    pub(crate) fn stamp(&mut self, chunk: &mut AudioChunk) {
        chunk.meta.segment = self.cursor.segment;
        chunk.meta.lane_frame = self.cursor.frame;
        let channels = usize::from(chunk.spec().channels.max(1));
        if let Some(jump) = &self.jump {
            for (offset, frame) in chunk.samples.chunks_exact_mut(channels).enumerate() {
                let offset = u64::try_from(offset).map_or(u64::MAX, |offset| offset);
                let gain = jump.gain(self.cursor.frame.saturating_add(offset));
                for sample in frame {
                    *sample *= gain;
                }
            }
        }
        let frames = u64::try_from(chunk.frames()).map_or(u64::MAX, |frames| frames);
        self.cursor.frame = self.cursor.frame.saturating_add(frames);
        if let Some(position) = chunk
            .meta
            .source_span
            .and_then(|span| span.position_at(span.output_frames()))
        {
            self.position = Some(position);
        }
        if matches!(self.jump, Some(Jump::Up { start, frames })
            if self.cursor.frame.saturating_sub(start) >= u64::try_from(frames).map_or(u64::MAX, |frames| frames))
        {
            self.jump = None;
        }
    }
}

fn landing_position(outcome: kithara_audio::SeekOutcome) -> Duration {
    match outcome {
        kithara_audio::SeekOutcome::Landed { landed_at, .. } => landed_at,
        kithara_audio::SeekOutcome::PastEof { duration, .. } => duration,
    }
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
        lane.cursor.frame = 64;
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
        assert_eq!(lane.cursor.frame, if pending_repeat { 64 } else { 0 });
        assert_eq!(
            lane.position,
            if pending_repeat {
                None
            } else {
                Some(Duration::from_secs(1))
            }
        );
        assert!(lane.deferred.is_none());
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
