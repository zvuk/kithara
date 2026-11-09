//! The session timeline the Host accepted: its tempo map and the accepted
//! changes the transport has not answered yet.

use std::mem;

use kithara_command::{Rejection, Seq, When};
use kithara_play::{PlayError, SessionTransportSnapshot};
use kithara_signal::{SessionEpoch, SessionFrame};
use kithara_sync::{TempoTrajectory, TrajectoryError};
use tracing::warn;

use crate::{HostSettingsChange, HostSettled, api::Tempo};

/// Where an accepted change waits for the transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placed {
    /// For the next block the transport renders.
    Next,
    /// For `frame` on the axis of `epoch`.
    At {
        frame: SessionFrame,
        epoch: Option<SessionEpoch>,
    },
}

/// An accepted change the transport has not answered yet.
#[derive(Clone, Copy, Debug, fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Accepted {
    /// The Seq the caller got when the Host accepted the change.
    #[field(get, copy, vis = "pub(crate)")]
    caller: Seq,
    /// The root batch that carries the change now.
    batch: Seq,
    change: HostSettingsChange,
    /// Asked for as soon as possible: a refusal of its frame places it again.
    #[field(get, copy, vis = "pub(crate)")]
    asap: bool,
    placed: Placed,
    /// A newer change took its place; its batch may still wait for an answer.
    #[field(get, copy, vis = "pub(crate)")]
    superseded: bool,
}

impl Accepted {
    /// The caller's one answer: a superseded change reports its replacement
    /// whatever its batch met.
    pub(crate) fn settled(
        &self,
        outcome: Result<SessionFrame, Rejection<PlayError>>,
    ) -> HostSettled {
        HostSettled::Settings {
            seq: self.caller,
            change: self.change,
            outcome: if self.superseded {
                Err(Rejection::Refused(PlayError::Superseded))
            } else {
                outcome
            },
        }
    }

    /// The tempo step its batch puts on the map of `axis`.
    fn step(&self, axis: Option<SessionEpoch>) -> Option<(SessionFrame, Tempo)> {
        match (self.change, self.placed) {
            (HostSettingsChange::Tempo(tempo), Placed::At { frame, epoch })
                if on_axis(epoch, axis) =>
            {
                Some((frame, tempo))
            }
            _ => None,
        }
    }

    fn live_tempo(&self) -> bool {
        matches!(self.change, HostSettingsChange::Tempo(_)) && !self.superseded
    }

    /// Whether this change, newer than `caller`, decides the setting `change`
    /// assigns before `change` placed again at `when` could.
    fn decides(
        &self,
        caller: Seq,
        change: HostSettingsChange,
        when: When<SessionFrame>,
        epoch: Option<SessionEpoch>,
    ) -> bool {
        !self.superseded
            && self.caller > caller
            && same_setting(self.change, change)
            && (self.asap
                || match (self.placed, when) {
                    (Placed::Next, _) => true,
                    (
                        Placed::At {
                            frame,
                            epoch: placed,
                        },
                        When::At(at),
                    ) => placed == epoch && frame <= at,
                    (Placed::At { .. }, When::Next | When::Deferred) => false,
                })
    }

    /// Leaves the setting to `newer` when it is a later change of the same
    /// setting the transport applied after this change's refused frame:
    /// placed again, this change would undo it.
    pub(crate) fn yield_to(&mut self, newer: &Self) {
        if newer.caller > self.caller && same_setting(newer.change, self.change) {
            self.superseded = true;
        }
    }
}

/// Whether a step placed on the axis of `epoch` stands on the map of `axis`;
/// before the transport confirmed an axis, every step does.
fn on_axis(epoch: Option<SessionEpoch>, axis: Option<SessionEpoch>) -> bool {
    axis.is_none() || epoch == axis
}

/// Whether two changes assign the same setting, so the later one decides it.
fn same_setting(a: HostSettingsChange, b: HostSettingsChange) -> bool {
    match (a, b) {
        (HostSettingsChange::Metronome(a), HostSettingsChange::Metronome(b)) => {
            mem::discriminant(&a) == mem::discriminant(&b)
        }
        (a, b) => mem::discriminant(&a) == mem::discriminant(&b),
    }
}

/// Where an accepted change goes.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct Placement {
    #[field(get, copy, vis = "pub(crate)")]
    when: When<SessionFrame>,
    /// The caller of the accepted tempo it takes the place of.
    replaced: Option<Seq>,
}

/// The accepted tempo map of the session and the accepted changes in flight.
#[derive(fieldwork::Fieldwork)]
#[fieldwork(opt_in, get)]
pub(crate) struct SessionTimeline {
    /// The tempo the transport executed: the confirmed anchor, which a tempo
    /// placed without a frame moves, and the steps it applied.
    applied: TempoTrajectory,
    /// The accepted tempo map: the executed tempo with the steps of the
    /// accepted tempos still in flight.
    #[field(get, vis = "pub(crate)")]
    trajectory: TempoTrajectory,
    /// The transport axis the map starts from, once the transport confirmed one.
    axis: Option<SessionEpoch>,
    entries: Vec<Accepted>,
}

impl SessionTimeline {
    pub(crate) fn new(trajectory: TempoTrajectory) -> Self {
        Self {
            applied: trajectory.clone(),
            trajectory,
            axis: None,
            entries: Vec::new(),
        }
    }

    /// Without a clock to place a frame on, a tempo moves the map's anchor.
    pub(crate) fn initial_tempo(&mut self, tempo: Tempo) {
        self.applied.initial_tempo(tempo);
        self.replan();
    }

    /// Follows the transport onto a new axis: the map starts again from the
    /// anchor the transport confirmed, with the accepted steps placed on it.
    /// A tempo accepted for the next block holds the old axis until it is
    /// answered: the block it lands in is unknown, so the confirmed anchor
    /// may not carry it yet.
    pub(crate) fn observe(&mut self, snapshot: Option<SessionTransportSnapshot>) {
        let Some(snapshot) = snapshot else {
            return;
        };
        let axis = Some(snapshot.session_epoch());
        if self.axis == axis
            || self
                .entries
                .iter()
                .any(|entry| entry.live_tempo() && entry.placed == Placed::Next)
        {
            return;
        }
        self.applied.reaxis_observed(snapshot.anchor());
        self.axis = axis;
        self.replan();
    }

    /// Whether the pass clock counts frames on the axis of `epoch`: the
    /// transport rendered on it, or has confirmed no axis yet. A restarted
    /// stream's clock reads the stopped one's count until its first block.
    pub(crate) fn confirms(&self, epoch: Option<SessionEpoch>) -> bool {
        on_axis(epoch, self.axis)
    }

    /// Plans an accepted change at `when` on the axis of `epoch`, or `None`
    /// when a newer accepted change of the same setting decides it instead of
    /// the `caller` placed again.
    ///
    /// A newer tempo takes the place of an accepted one the transport has not
    /// reached: of the one on its frame, and of the earliest one when it is
    /// earliest itself.
    ///
    /// # Errors
    ///
    /// [`PlayError::Late`] for a tempo at or before a step the map already
    /// executes, and [`PlayError::Internal`] for a step the map cannot
    /// represent.
    pub(crate) fn plan(
        &self,
        caller: Option<Seq>,
        change: HostSettingsChange,
        when: When<SessionFrame>,
        asap: bool,
        epoch: Option<SessionEpoch>,
        bound: Option<SessionFrame>,
    ) -> Result<Option<Placement>, PlayError> {
        if caller.is_some_and(|caller| {
            self.entries
                .iter()
                .any(|entry| entry.decides(caller, change, when, epoch))
        }) {
            return Ok(None);
        }
        let HostSettingsChange::Tempo(tempo) = change else {
            return Ok(Some(Placement {
                when,
                replaced: None,
            }));
        };
        let replaced = self.entries.iter().rev().find(|entry| {
            entry.live_tempo()
                && match (when, entry.placed) {
                    (When::Next, Placed::Next) => asap && entry.asap,
                    (
                        When::At(at),
                        Placed::At {
                            frame,
                            epoch: placed,
                        },
                    ) if placed == epoch => {
                        frame == at
                            || (asap && entry.asap && bound.is_some_and(|bound| frame >= bound))
                    }
                    _ => false,
                }
        });
        let when = match (when, replaced.map(|entry| entry.placed)) {
            (When::At(_), Some(Placed::At { frame, .. })) => When::At(frame),
            (when, _) => when,
        };
        match when {
            When::At(frame) if on_axis(epoch, self.axis) => self.check_step(frame, tempo)?,
            When::Next | When::At(_) => {}
            When::Deferred => {
                return Err(PlayError::Internal(
                    "a tempo has no deferred executor moment".to_owned(),
                ));
            }
        }
        Ok(Some(Placement {
            when,
            replaced: replaced.map(Accepted::caller),
        }))
    }

    /// Refuses a step the map cannot take on `frame`: the transport already
    /// passed a frame at or before an executed step.
    fn check_step(&self, frame: SessionFrame, tempo: Tempo) -> Result<(), PlayError> {
        if self.applied.anchor_at(frame).frame() == frame
            || self.applied.changes_after(frame).next().is_some()
        {
            return Err(PlayError::Late);
        }
        let mut map = self.trajectory.clone();
        map.withdraw(frame);
        map.push(frame, tempo)
            .map_err(|error| PlayError::Internal(error.to_string()))
    }

    /// Records a change the transport now carries in `batch`, placed by
    /// `placement` on the axis of `epoch`.
    pub(crate) fn accept(
        &mut self,
        caller: Option<Seq>,
        batch: Seq,
        change: HostSettingsChange,
        asap: bool,
        epoch: Option<SessionEpoch>,
        placement: &Placement,
    ) {
        if let Some(replaced) = placement.replaced
            && let Some(entry) = self
                .entries
                .iter_mut()
                .find(|entry| entry.caller == replaced)
        {
            entry.superseded = true;
        }
        let placed = match placement.when {
            When::At(frame) => Placed::At { frame, epoch },
            When::Next | When::Deferred => Placed::Next,
        };
        if let (Placed::Next, HostSettingsChange::Tempo(tempo)) = (placed, change) {
            self.applied.initial_tempo(tempo);
        }
        self.entries.push(Accepted {
            caller: caller.unwrap_or(batch),
            batch,
            change,
            asap,
            placed,
            superseded: false,
        });
        self.replan();
    }

    /// Takes the accepted change `batch` carries, whose answer arrived. An
    /// applied tempo step stays on the map whatever replaced it; a refused
    /// one leaves it.
    pub(crate) fn settle(&mut self, batch: Seq, applied: bool) -> Option<Accepted> {
        let index = self.entries.iter().position(|entry| entry.batch == batch)?;
        let entry = self.entries.remove(index);
        if applied && let Some((frame, tempo)) = entry.step(self.axis) {
            put_step(&mut self.applied, frame, tempo);
        }
        self.replan();
        Some(entry)
    }

    /// Answers every accepted change whose executor the owner destroys: no
    /// answer of it comes back.
    pub(crate) fn abandon(&mut self) -> impl Iterator<Item = HostSettled> + use<> {
        self.trajectory = self.applied.clone();
        mem::take(&mut self.entries)
            .into_iter()
            .map(|entry| entry.settled(Err(Rejection::Unanswered)))
    }

    /// Builds the accepted map again: the executed tempo with the step of
    /// every accepted tempo in flight on the current axis.
    fn replan(&mut self) {
        let mut map = self.applied.clone();
        for entry in self.entries.iter().filter(|entry| !entry.superseded) {
            if let Some((frame, tempo)) = entry.step(self.axis) {
                put_step(&mut map, frame, tempo);
            }
        }
        self.trajectory = map;
    }
}

/// Puts the step to `tempo` on `frame` in place of the one there. A step at
/// or before the map's anchor is one the anchor the transport confirmed
/// already carries.
fn put_step(map: &mut TempoTrajectory, frame: SessionFrame, tempo: Tempo) {
    map.withdraw(frame);
    match map.push(frame, tempo) {
        Ok(()) | Err(TrajectoryError::BeforeAnchor { .. }) => {}
        Err(error) => {
            warn!(%error, ?frame, "a tempo step does not fit the accepted map");
        }
    }
}
