use std::{iter, num::NonZeroU32, ops::Range};

use firewheel::node::{ProcInfo, ProcStore};
use kithara_command::{Due, LevelInbox, ScopeId, ScopedInbox};
use kithara_config::LiveConfig;
use kithara_render::bridge::{DeckProtocol, DeckRefusal, SessionInbox};
use kithara_signal::{SessionEpoch, SessionFrame};
use kithara_warp::{SessionAnchor, SessionBeat};
use ringbuf::traits::{Consumer, Producer};
use triple_buffer::Input;

use super::{
    OfflineInbox,
    commit::{SessionGridGeneration, TransportObservation, TransportProcessError},
    inbox::InboxExchange,
};
use crate::{
    api::{SessionTransportSnapshot, Tempo, TransportRevision},
    consts,
    host::{HostSettings, HostSettingsChange},
    session::queue::{HostPart, HostProtocol},
};

#[derive(Debug)]
pub(super) struct TransportFrame {
    pub(super) trajectory: SessionAnchor,
    pub(super) transport_revision: TransportRevision,
    pub(super) session_epoch: SessionEpoch,
}

/// The transport's half of the Host queue: the settings as the render graph
/// applied them, the beat anchor they put on the render clock, both split at
/// the frames of the block the changes applied on, and the inbox the session
/// owner sends changes through.
pub(crate) struct TransportState {
    inbox: Option<ScopedInbox<HostProtocol, DeckProtocol>>,
    inbox_exchange: Option<InboxExchange>,
    /// The spans of the block rendering now that end where a later one
    /// starts, in frame order. Storage holds one per batch the inbox can have
    /// in flight, so the audio thread never grows it.
    closed: Vec<Span>,
    /// The span the block rendering now ends in; the next block starts in it.
    current: Span,
    boundary: Option<SessionFrame>,
    reanchor_beat: Option<SessionBeat>,
    revision: TransportRevision,
    snapshot: Option<SessionTransportSnapshot>,
    session_grid: SessionGridGeneration,
}

/// The settings and the beat anchor the render graph applies from one frame
/// of a block on.
#[derive(Clone, Copy, Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, vis = "pub(crate)")]
pub(crate) struct Span {
    /// Frames from the block start to the span's first frame.
    offset: usize,
    #[field(get(copy))]
    settings: HostSettings,
    /// None until the transport anchors the beats on a stream.
    #[field(get(copy))]
    anchor: Option<SessionAnchor>,
}

/// What one due batch leaves behind once every command of it applied.
#[derive(Clone, Copy)]
struct Staged {
    settings: HostSettings,
    anchor: Option<SessionAnchor>,
    retargeted: bool,
}

#[derive(Debug)]
pub(crate) struct TransportObservationInput(Input<TransportObservation>);

impl TransportObservationInput {
    pub(crate) const fn new(input: Input<TransportObservation>) -> Self {
        Self(input)
    }

    delegate::delegate! {
        to self.0 {
            fn write(&mut self, observation: TransportObservation);
        }
    }
}

pub(super) fn process_transport(
    info: &ProcInfo,
    store: &mut ProcStore,
) -> Result<TransportFrame, TransportProcessError> {
    let result = store
        .try_get_mut::<TransportState>()
        .ok_or(TransportProcessError::MissingState)?
        .process(info);
    publish_observation(store)?;
    result
}

pub(crate) fn restart_transport(store: &mut ProcStore) -> Result<(), TransportProcessError> {
    let result = store
        .try_get_mut::<TransportState>()
        .ok_or(TransportProcessError::MissingState)?
        .restart();
    publish_observation(store)?;
    result
}

pub(crate) fn converge_transport_restart(
    store: &mut ProcStore,
    settings: HostSettings,
    target: SessionGridGeneration,
) -> Result<SessionGridGeneration, TransportProcessError> {
    let result = store
        .try_get_mut::<TransportState>()
        .ok_or(TransportProcessError::MissingState)?
        .converge_restart(settings, target);
    publish_observation(store)?;
    result
}

/// Every span of the block of `frames` frames rendering now, in frame order,
/// with the block frames it covers; none until the transport is installed.
pub(crate) fn applied_spans(
    store: &ProcStore,
    frames: usize,
) -> Option<impl Iterator<Item = (Range<usize>, Span)> + Clone + '_> {
    store.try_get::<TransportState>().map(move |state| {
        let spans = state
            .closed
            .iter()
            .copied()
            .chain(iter::once(state.current));
        let ends = spans
            .clone()
            .skip(1)
            .map(|span| span.offset)
            .chain(iter::once(frames));
        spans.zip(ends).map(|(span, end)| (span.offset..end, span))
    })
}

fn publish_observation(store: &mut ProcStore) -> Result<(), TransportProcessError> {
    let observation = {
        let state = store
            .try_get::<TransportState>()
            .ok_or(TransportProcessError::MissingState)?;
        TransportObservation::new(state.snapshot, state.session_grid)
    };
    store
        .try_get_mut::<TransportObservationInput>()
        .ok_or(TransportProcessError::MissingObservation)?
        .write(observation);
    Ok(())
}

impl TransportState {
    /// A transport whose inbox holds up to `capacity` batches in flight.
    pub(crate) fn new(
        inbox: ScopedInbox<HostProtocol, DeckProtocol>,
        settings: HostSettings,
        session_grid: SessionGridGeneration,
        capacity: usize,
    ) -> Self {
        Self {
            inbox: Some(inbox),
            inbox_exchange: None,
            session_grid,
            closed: Vec::with_capacity(capacity),
            current: Span {
                offset: 0,
                settings,
                anchor: None,
            },
            boundary: None,
            reanchor_beat: None,
            revision: TransportRevision::first(),
            snapshot: None,
        }
    }

    pub(crate) fn park_offline_inbox(&mut self) -> Result<OfflineInbox, TransportProcessError> {
        self.acquire_inbox()?;
        let inbox = self
            .inbox
            .take()
            .ok_or(TransportProcessError::MissingState)?;
        let (owner, exchange) = OfflineInbox::new(inbox);
        self.inbox_exchange = Some(exchange);
        Ok(owner)
    }

    fn acquire_inbox(&mut self) -> Result<(), TransportProcessError> {
        if self.inbox.is_none()
            && let Some(exchange) = &mut self.inbox_exchange
            && let Some((inbox, frames)) = exchange.from_owner.try_pop()
        {
            self.inbox = Some(inbox);
            exchange.remaining_frames = frames;
        }
        self.inbox
            .as_ref()
            .map(|_| ())
            .ok_or(TransportProcessError::MissingState)
    }

    pub(crate) fn return_inbox(&mut self, frames: usize) -> Result<(), TransportProcessError> {
        let Some(exchange) = &mut self.inbox_exchange else {
            return Ok(());
        };
        exchange.remaining_frames = exchange
            .remaining_frames
            .checked_sub(frames)
            .ok_or(TransportProcessError::MissingState)?;
        if exchange.remaining_frames != 0 {
            return Ok(());
        }
        let inbox = self
            .inbox
            .take()
            .ok_or(TransportProcessError::MissingState)?;
        if let Err(inbox) = exchange.to_owner.try_push(inbox) {
            self.inbox = Some(inbox);
            return Err(TransportProcessError::MissingState);
        }
        Ok(())
    }

    pub(crate) fn retire_closing(&mut self) -> Result<(), TransportProcessError> {
        self.acquire_inbox()?;
        self.inbox
            .as_mut()
            .ok_or(TransportProcessError::MissingState)?
            .retire_closing();
        Ok(())
    }

    /// Puts the beat anchor on the first block of a stream: session beat 0 on
    /// a fresh transport, the beat a restart stopped on otherwise.
    fn anchor_block(&mut self, info: &ProcInfo) -> Result<(), TransportProcessError> {
        if self.current.anchor.is_some() {
            return Ok(());
        }
        let beat = match self.reanchor_beat {
            Some(beat) => beat,
            None => SessionBeat::new(0.0).map_err(|_| TransportProcessError::InvalidBeatRange)?,
        };
        let anchor = Self::build_anchor(
            SessionFrame::new(info.clock_samples.0),
            beat,
            self.current.settings.tempo(),
            info.sample_rate,
        )?;
        let revision = self.session_grid.next_revision()?;
        self.current.anchor = Some(anchor);
        self.reanchor_beat = None;
        self.boundary = None;
        self.session_grid.commit_revision(revision);
        Ok(())
    }

    /// Applies the batches due inside the block in time order, each from its
    /// frame on. A batch applies whole or not at all: its commands stage on
    /// copies, and only a batch whose every command staged opens a span on
    /// its frame. Only a batch that re-anchors the beats takes a new
    /// transport revision. A block that does not follow the last one refuses
    /// every tempo change due in it with `continuity`'s error, since no beat
    /// anchor is known on its frames; a metronome or ducking change applies
    /// in any block.
    fn apply_due(
        &mut self,
        info: &ProcInfo,
        continuity: Result<(), TransportProcessError>,
    ) -> Result<(), TransportProcessError> {
        let Self {
            inbox,
            closed,
            current,
            revision,
            session_grid,
            ..
        } = self;
        let mut root = inbox
            .as_mut()
            .ok_or(TransportProcessError::MissingState)?
            .root();
        let start = SessionFrame::new(info.clock_samples.0);
        while let Some(due) = root.next_due(start, info.frames) {
            let staged = Self::stage(&due, current.settings, current.anchor, continuity).and_then(
                |staged| {
                    if !staged.retargeted {
                        return Ok((staged, *revision, None));
                    }
                    let next = revision
                        .checked_next()
                        .ok_or(TransportProcessError::RevisionExhausted)?;
                    Ok((staged, next, Some(session_grid.next_revision()?)))
                },
            );
            match staged {
                Ok((staged, next, grid)) => {
                    let offset = due.offset();
                    if offset > current.offset {
                        debug_assert!(
                            closed.len() < closed.capacity(),
                            "a block opens one span per batch in flight"
                        );
                        closed.push(*current);
                    }
                    *current = Span {
                        offset,
                        settings: staged.settings,
                        anchor: staged.anchor,
                    };
                    *revision = next;
                    if let Some(grid) = grid {
                        session_grid.commit_revision(grid);
                    }
                    due.apply(next);
                }
                Err(error) => due.refuse(error),
            }
        }
        Ok(())
    }

    fn build_anchor(
        frame: SessionFrame,
        beat: SessionBeat,
        tempo: Tempo,
        sample_rate: NonZeroU32,
    ) -> Result<SessionAnchor, TransportProcessError> {
        let anchor = SessionAnchor::new(frame, beat, tempo.beats_per_second(), sample_rate)
            .map_err(|_| TransportProcessError::InvalidBeatRange)?;
        if anchor
            .frame_at(beat)
            .map_err(|_| TransportProcessError::InvalidBeatRange)?
            != frame
        {
            return Err(TransportProcessError::InvalidBeatRange);
        }
        Ok(anchor)
    }

    /// Moves the session grid to the restart `target` with the owner's settled settings.
    fn converge_restart(
        &mut self,
        settings: HostSettings,
        target: SessionGridGeneration,
    ) -> Result<SessionGridGeneration, TransportProcessError> {
        let target_stamp = target.stamp()?;
        let current_stamp = self.session_grid.stamp()?;
        if current_stamp.grid_id() != target_stamp.grid_id() {
            return Err(TransportProcessError::SessionGridGenerationMismatch);
        }
        self.current.settings = settings;
        if self.session_grid.epoch() < target.epoch() {
            let mut successor = self.session_grid;
            successor.advance_restart()?;
            if successor.epoch() != target.epoch() {
                return Err(TransportProcessError::SessionGridGenerationMismatch);
            }
            self.restart()?;
        } else if self.session_grid.epoch() > target.epoch() {
            return Err(TransportProcessError::SessionGridGenerationMismatch);
        }
        let actual_stamp = self.session_grid.stamp()?;
        if self.session_grid.epoch() == target.epoch()
            && actual_stamp.revision() >= target_stamp.revision()
        {
            Ok(self.session_grid)
        } else {
            Err(TransportProcessError::SessionGridGenerationMismatch)
        }
    }

    fn process(&mut self, info: &ProcInfo) -> Result<TransportFrame, TransportProcessError> {
        self.acquire_inbox()?;
        self.inbox
            .as_mut()
            .ok_or(TransportProcessError::MissingState)?
            .drain();
        self.closed.clear();
        self.current.offset = 0;
        let anchor = self.anchor_block(info);
        let continuity = anchor.and_then(|_| self.validate_frame(info));
        self.apply_due(info, continuity)?;
        continuity?;
        let anchor = self
            .current
            .anchor
            .ok_or(TransportProcessError::InvalidBeatRange)?;
        let (frames, beats) = Self::block_span(anchor, info)?;
        self.boundary = Some(frames.end);
        self.snapshot = Some(SessionTransportSnapshot::new(
            beats.end,
            self.current.settings.tempo(),
            self.revision,
            anchor,
            self.session_grid.stamp()?,
            self.session_grid.epoch(),
        ));
        Ok(TransportFrame {
            trajectory: anchor,
            session_epoch: self.session_grid.epoch(),
            transport_revision: self.revision,
        })
    }

    /// Ends the stream's frame axis: refuses every change waiting for a frame
    /// on it, keeps the ones for the next block, and leaves the beat it
    /// stopped on for the next stream to anchor.
    fn restart(&mut self) -> Result<(), TransportProcessError> {
        self.acquire_inbox()?;
        self.inbox
            .as_mut()
            .ok_or(TransportProcessError::MissingState)?
            .refuse_timed(
                TransportProcessError::SessionAxisRestarted,
                DeckRefusal::AxisRestarted,
            );
        if let Some(snapshot) = self.snapshot.take() {
            self.reanchor_beat = Some(snapshot.position());
        }
        let generation = self.session_grid.advance_restart();
        if generation.is_err() {
            self.reanchor_beat = None;
        }
        self.current.anchor = None;
        self.boundary = None;
        generation
    }

    /// The block's frame range and the session beats it covers.
    fn block_span(
        anchor: SessionAnchor,
        info: &ProcInfo,
    ) -> Result<(Range<SessionFrame>, Range<SessionBeat>), TransportProcessError> {
        let frames =
            i64::try_from(info.frames).map_err(|_| TransportProcessError::InvalidBeatRange)?;
        let start = SessionFrame::new(info.clock_samples.0);
        let end = SessionFrame::new(
            info.clock_samples
                .0
                .checked_add(frames)
                .ok_or(TransportProcessError::InvalidBeatRange)?,
        );
        let at = |frame| {
            anchor
                .beat_at(frame)
                .map_err(|_| TransportProcessError::InvalidBeatRange)
        };
        Ok((start..end, at(start)?..at(end)?))
    }

    fn stage(
        due: &Due<'_, HostProtocol>,
        settings: HostSettings,
        anchor: Option<SessionAnchor>,
        continuity: Result<(), TransportProcessError>,
    ) -> Result<Staged, TransportProcessError> {
        let mut staged = Staged {
            settings,
            anchor,
            retargeted: false,
        };
        for command in due.commands() {
            let HostPart::Settings(change) = *command;
            match change {
                HostSettingsChange::Tempo(tempo) => {
                    continuity?;
                    if tempo == staged.settings.tempo() {
                        continue;
                    }
                    staged.settings.apply_change(change);
                    staged.anchor = Some(
                        staged
                            .anchor
                            .ok_or(TransportProcessError::InvalidBeatRange)?
                            .retarget(
                                due.at(),
                                tempo.beats_per_second(),
                                consts::TEMPO_SMOOTH_SECONDS,
                            )
                            .map_err(|_| TransportProcessError::InvalidBeatRange)?,
                    );
                    staged.retargeted = true;
                }
                HostSettingsChange::SampleRate(_)
                | HostSettingsChange::Metronome(_)
                | HostSettingsChange::Ducking(_) => {
                    staged.settings.apply_change(change);
                }
            }
        }
        Ok(staged)
    }

    fn validate_frame(&self, info: &ProcInfo) -> Result<(), TransportProcessError> {
        if let Some(anchor) = self.current.anchor
            && anchor.sample_rate() != info.sample_rate
        {
            return Err(TransportProcessError::FrameDiscontinuity);
        }
        if let Some(boundary) = self.boundary
            && i64::from(boundary) != info.clock_samples.0
        {
            return Err(TransportProcessError::FrameDiscontinuity);
        }
        Ok(())
    }
}

impl SessionInbox for TransportState {
    fn scope(&mut self, id: ScopeId) -> Option<LevelInbox<'_, DeckProtocol>> {
        self.inbox.as_mut()?.scope(id)
    }
}
