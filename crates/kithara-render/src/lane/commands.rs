use std::task::{Context, Poll};

use kithara_audio::{AudioSource, TrackFailureKind};
use kithara_bufpool::HasPool;
use kithara_command::{Due, Inbox, Seq};
use kithara_platform::time::Duration;
use kithara_signal::{AudioChunk, AudioSpec, FrameCount, SegmentId};
use kithara_sync::SpeedCurve;
use kithara_warp::WarpRenderer;

use super::{
    Jump, Lane, LaneApplied, LaneChange, LaneCommand, LaneFrame, LaneState, Preload, Preparation,
    landing_position,
};

impl Lane {
    pub(crate) fn poll_commands(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.inbox.poll_drain(context)
    }

    fn drain_commands(&mut self) -> Result<(), TrackFailureKind> {
        self.inbox.drain();
        loop {
            let next = match self.inbox.next_deferred() {
                Some(deferred) => deferred.park(),
                None => break,
            };
            let previous = self
                .state
                .deferred
                .replace(next.ok_or(TrackFailureKind::Render)?);
            if let Some(previous) = previous {
                drop(
                    self.inbox
                        .resume(previous, self.state.cursor, self.state.cursor),
                );
            }
        }
        Ok(())
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
        self.drain_commands()?;
        let mut changed = LaneChange::None;
        let declick_frames = self.declick_frames(spec.sample_rate).get();
        loop {
            let deferred = self.state.ended
                && self
                    .inbox
                    .frames_until_due(self.state.cursor)
                    .is_none_or(|frames| frames > 0);
            let Some(due) = next_due(&mut self.inbox, &mut self.state, deferred)? else {
                break;
            };
            if changed == LaneChange::None {
                changed = LaneChange::Controls;
            }
            let at = due.at();
            let effects =
                self.state
                    .execute_commands(&due, source, warp, deferred, declick_frames)?;
            let CommandEffects {
                segment,
                requested_entry,
                restored_same,
                cancelled_deferred,
                refusal,
            } = effects;
            if segment.is_some() {
                changed = LaneChange::Source;
            }
            if let Some(failure) = refusal {
                due.refuse(failure);
                return Err(failure);
            }
            let next_spec = source.prepare_deferred().unwrap_or(spec);
            let latency = warp
                .prepare_engine_latency(next_spec)
                .map_err(|_| TrackFailureKind::Render)?;
            if segment.is_some() {
                let seq = due.defer();
                self.finish_pending(None);
                self.state.preload = Preload::Filling(0);
                self.state.pending = Some(Preparation {
                    seq,
                    at,
                    entry: self.state.cursor,
                    source: None,
                    requested_entry,
                    render_revision: warp.render_revision(),
                    latency,
                });
            } else {
                due.apply(self.state.controls_applied(
                    restored_same,
                    warp.render_revision(),
                    latency,
                ));
            }
            if let Some(seq) = cancelled_deferred {
                drop(self.inbox.resume(seq, self.state.cursor, self.state.cursor));
            }
        }
        if self.land_jump(source, warp, spec)? {
            changed = LaneChange::Source;
        }
        Ok(changed)
    }
}

#[derive(Default)]
struct CommandEffects {
    segment: Option<SegmentId>,
    requested_entry: Option<Duration>,
    restored_same: bool,
    cancelled_deferred: Option<Seq>,
    refusal: Option<TrackFailureKind>,
}

impl LaneState {
    fn controls_applied(
        &mut self,
        restored_same: bool,
        revision: u64,
        latency: FrameCount,
    ) -> LaneApplied {
        if matches!(self.preload, Preload::Filling(0))
            && let Some(pending) = &mut self.pending
            && pending.entry == self.cursor
        {
            pending.render_revision = revision;
            pending.latency = latency;
        }
        LaneApplied {
            source_basis: None,
            engine_latency: latency,
            ready: (restored_same && matches!(self.preload, Preload::Ready))
                .then_some(self.cursor.segment),
        }
    }

    fn execute_commands<T, S>(
        &mut self,
        due: &Due<'_, super::LaneProtocol>,
        source: &mut T,
        warp: &mut WarpRenderer<S>,
        deferred: bool,
        declick_frames: usize,
    ) -> Result<CommandEffects, TrackFailureKind>
    where
        T: AudioSource<Chunk = AudioChunk>,
        S: HasPool<f32>,
    {
        let revision = due.seq().get();
        let at = due.at();
        let mut effects = CommandEffects::default();
        for command in due.commands() {
            tracing::debug!(
                seq = revision,
                actual_apply_segment = at.segment.get(),
                actual_apply_frame = at.frame,
                current_segment = self.cursor.segment.get(),
                prepared_frontier = self.cursor.frame,
                command = ?command,
                "lane command executing"
            );
            match command {
                LaneCommand::SetSpeed(curve) => warp
                    .set_speed(curve.clone(), revision)
                    .map_err(|_| TrackFailureKind::Render)?,
                LaneCommand::SetKeylock(on) => warp.set_keylock(*on),
                LaneCommand::SetBackend(kind) => warp.set_backend(*kind),
                LaneCommand::Jump { to } => {
                    let gain = self.jump.map_or(1.0, |jump| jump.gain(self.cursor.frame));
                    self.jump = Some(Jump::Down {
                        to: *to,
                        start: self.cursor.frame,
                        frames: declick_frames,
                        gain,
                    });
                }
                LaneCommand::Segment { id, from, speed } => {
                    self.enter_segment(
                        (*id, *from, speed),
                        source,
                        warp,
                        revision,
                        deferred,
                        &mut effects,
                    )?;
                    if effects.refusal.is_some() {
                        break;
                    }
                }
                LaneCommand::SetHostRate { id, rate } => {
                    effects.cancelled_deferred = self.deferred.take();
                    self.parked = None;
                    source.set_host_sample_rate(*rate);
                    warp.reset();
                    self.cursor = LaneFrame {
                        segment: *id,
                        frame: 0,
                    };
                    self.jump = None;
                    self.ended = false;
                    effects.segment = Some(*id);
                }
            }
        }
        Ok(effects)
    }

    fn enter_segment<T, S>(
        &mut self,
        (id, from, speed): (SegmentId, Duration, &SpeedCurve),
        source: &mut T,
        warp: &mut WarpRenderer<S>,
        revision: u64,
        deferred: bool,
        effects: &mut CommandEffects,
    ) -> Result<(), TrackFailureKind>
    where
        T: AudioSource<Chunk = AudioChunk>,
        S: HasPool<f32>,
    {
        if id == self.cursor.segment && self.deferred.is_some() {
            effects.restored_same = true;
            effects.cancelled_deferred = self.deferred.take();
            return Ok(());
        }
        let restored = self.parked.filter(|(cursor, _)| cursor.segment == id);
        let from = match restored {
            Some((_, Some(position))) => position,
            Some((_, None)) => {
                effects.refusal = Some(TrackFailureKind::Render);
                return Ok(());
            }
            None => from,
        };
        if deferred {
            self.parked = Some((self.cursor, self.position));
        } else {
            self.parked = None;
        }
        effects.cancelled_deferred = self.deferred.take();
        self.cursor = restored.map_or(
            LaneFrame {
                segment: id,
                frame: 0,
            },
            |(cursor, _)| cursor,
        );
        effects.requested_entry = Some(from);
        self.position = landing_position(source.seek_for_render(from))?;
        warp.reset();
        warp.set_speed(speed.clone(), revision)
            .map_err(|_| TrackFailureKind::Render)?;
        self.jump = None;
        self.ended = false;
        effects.segment = Some(id);
        Ok(())
    }
}

fn next_due<'a>(
    inbox: &'a mut Inbox<super::LaneProtocol>,
    state: &mut LaneState,
    deferred: bool,
) -> Result<Option<Due<'a, super::LaneProtocol>>, TrackFailureKind> {
    if deferred && state.deferred.is_some() {
        let seq = state.deferred.take().ok_or(TrackFailureKind::Render)?;
        Ok(inbox.resume(seq, state.cursor, state.cursor))
    } else {
        Ok(inbox.next_due(state.cursor, 1))
    }
}
