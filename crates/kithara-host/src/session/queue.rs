//! The Host queue: Host settings changes the session transport applies on
//! its render clock, and the receipts that settle them on the session owner.

use std::{convert::Infallible, num::NonZeroU32};

use kithara_command::{LiveError, Outcome, Protocol, Receipt, Rejection, SendError, Seq, When};
use kithara_config::ConfigOwner;
use kithara_play::PlayError;
use kithara_signal::SessionFrame;
use tracing::{error, warn};

use super::{
    SessionError,
    dispatch::invalidate_audio_route,
    state::SessionState,
    timeline::Accepted,
    transport::{TransportEvent, TransportProcessError, publish_transport_event},
};
use crate::{
    HostSettled,
    api::{Tempo, TransportRevision},
    host::{HostSettingsChange, HostSettingsExec},
};

/// One command the session transport applies.
#[derive(Clone, Copy, Debug)]
pub(crate) enum HostPart {
    /// A change of one Host setting.
    Settings(HostSettingsChange),
}

/// What the session owner and the session transport say to each other.
#[derive(Debug)]
pub(crate) enum HostProtocol {}

impl Protocol for HostProtocol {
    type Applied = TransportRevision;
    type Clock = SessionFrame;
    type Command = HostPart;
    type Refusal = TransportProcessError;
    type Target = Infallible;

    fn frames_since(at: SessionFrame, start: SessionFrame) -> Option<u64> {
        at.frames_since(start)
    }
}

impl<T, S> HostSettingsExec<()> for SessionState<T, S> {
    type At = When<SessionFrame>;
    type Output = Result<Option<Seq>, PlayError>;

    /// The owner restarts the output route at the new rate; the render graph
    /// never sees it, so no frame can carry a rate change. A restart that
    /// fails keeps the rate, and the next restart starts the output at it.
    fn exec_sample_rate(&mut self, value: NonZeroU32, at: Self::At, _cx: &mut ()) -> Self::Output {
        if at != When::Next {
            return Err(PlayError::Untimed);
        }
        self.settings.apply(HostSettingsChange::SampleRate(value))?;
        self.publish_root();
        if self.stream_needs_restart {
            return Ok(None);
        }
        invalidate_audio_route(self, "sample rate change")
            .map(|()| None)
            .map_err(PlayError::from)
    }

    /// The transport re-anchors the session beats on the frame the tempo
    /// changes on.
    fn exec_tempo(&mut self, value: Tempo, at: Self::At, cx: &mut ()) -> Self::Output {
        self.exec_live(HostSettingsChange::Tempo(value), at, cx)
    }

    /// Without a render graph a change applies at once, since no clock runs
    /// to place a frame on. With one, the Host accepts it and the transport
    /// applies it on its frame.
    fn exec_live(
        &mut self,
        change: HostSettingsChange,
        at: Self::At,
        _cx: &mut (),
    ) -> Self::Output {
        self.check_when(at)?;
        if self.channel.is_none() {
            self.settings.apply(change)?;
            if let HostSettingsChange::Tempo(tempo) = change {
                self.timeline.initial_tempo(tempo);
            }
            self.publish_root();
            return Ok(None);
        }
        self.place(None, change, at)
    }
}

impl<T, S> SessionState<T, S> {
    /// Sends an accepted change to the transport. A change for as soon as
    /// possible goes on the first frame the transport can still apply it at
    /// while the pass clock counts frames on the transport's axis, and to the
    /// next block otherwise; a tempo takes its step on the accepted map.
    /// `caller` names the accepted change placed again; `None` then means a
    /// newer accepted change of the same setting decides it instead.
    fn place(
        &mut self,
        caller: Option<Seq>,
        change: HostSettingsChange,
        at: When<SessionFrame>,
    ) -> Result<Option<Seq>, PlayError> {
        let asap = at == When::Next;
        let epoch = self
            .transport_observation
            .as_mut()
            .map(|observation| observation.read().session_grid().epoch());
        let bound = self
            .iteration_clock
            .filter(|_| self.timeline.confirms(epoch))
            .map(|clock| clock.now + clock.delivery);
        let when = match (at, bound) {
            (When::Next, Some(bound)) => When::At(bound),
            (at, _) => at,
        };
        let Some(placement) = self
            .timeline
            .plan(caller, change, when, asap, epoch, bound)?
        else {
            return Ok(None);
        };
        let channel = self.channel.as_mut().ok_or(PlayError::Closed)?;
        let batch = self
            .settings
            .send(channel, placement.when(), change, HostPart::Settings)
            .map_err(|error| match error {
                LiveError::Invalid(error) => error,
                LiveError::Send(SendError::Full(_)) => SessionError::HostQueueFull.into(),
                LiveError::Send(SendError::Closed(_)) => PlayError::Closed,
                LiveError::Send(SendError::Target(_)) => {
                    PlayError::Internal("a host settings batch names a target".to_owned())
                }
            })?;
        self.timeline
            .accept(caller, batch, change, asap, epoch, &placement);
        Ok(Some(batch))
    }
}

/// An accepted change for as soon as possible whose frame the transport
/// refused. It goes on a new frame once the receipts read with it are
/// settled: a newer change of its setting the same block refused no longer
/// stands, and one the block applied keeps the setting.
pub(crate) struct Moved {
    accepted: Accepted,
    change: HostSettingsChange,
    when: When<SessionFrame>,
}

pub(crate) fn settle_root_receipts<T, S>(state: &mut SessionState<T, S>) {
    let mut moved = Vec::new();
    while let Some(receipt) = state
        .channel
        .as_mut()
        .and_then(kithara_command::ScopedSender::root_receipt)
    {
        settle_receipt(state, &receipt, &mut moved);
    }
    place_moved(state, moved);
}

/// Settles one root receipt the owner routed here. An applied change moves
/// into the settings the Host reads; a tempo it changed is announced. A
/// change for as soon as possible whose frame the transport refused joins
/// `moved`, for [`place_moved`] once the receipts read with it are settled;
/// any other refused change leaves the accepted map and its caller gets the
/// answer.
pub(crate) fn settle_receipt<T, S>(
    state: &mut SessionState<T, S>,
    receipt: &Receipt<HostProtocol>,
    moved: &mut Vec<Moved>,
) {
    let before = *state.settings.config();
    let Some(settled) = state.settings.settle(receipt) else {
        return;
    };
    let applied = matches!(receipt.outcome(), Outcome::Applied { .. });
    let Some(accepted) = state.timeline.settle(receipt.seq(), applied) else {
        error!(seq = ?receipt.seq(), change = ?settled.change, "a host settings receipt answers no accepted change");
        return;
    };
    let rejection = match receipt.outcome() {
        Outcome::Applied { at, data } => {
            for held in moved.iter_mut() {
                held.accepted.yield_to(&accepted);
            }
            state.settled.push(accepted.settled(Ok(*at)));
            state.publish_root();
            let HostSettingsChange::Tempo(tempo) = settled.change else {
                return;
            };
            if tempo == before.tempo() {
                return;
            }
            publish_transport_event(
                state,
                &TransportEvent::TempoCommitted {
                    revision: u64::from(*data),
                    beats_per_minute: tempo.beats_per_minute(),
                },
            );
            return;
        }
        Outcome::Rejected(rejection) => *rejection,
    };
    let moves = matches!(
        rejection,
        Rejection::Late
            | Rejection::Refused(
                TransportProcessError::SessionAxisRestarted
                    | TransportProcessError::FrameDiscontinuity
            )
    );
    let outcome = match rejection {
        _ if accepted.superseded() => Err(Rejection::Refused(PlayError::Superseded)),
        _ if moves && accepted.asap() => {
            moved.push(Moved {
                accepted,
                change: settled.change,
                when: settled.when,
            });
            return;
        }
        _ if moves => Err(Rejection::Late),
        Rejection::Late => Err(Rejection::Late),
        Rejection::Stale => Err(Rejection::Stale),
        Rejection::Unanswered => Err(Rejection::Unanswered),
        Rejection::Refused(reason) => {
            Err(Rejection::Refused(PlayError::Internal(reason.to_string())))
        }
    };
    answer(state, &accepted, outcome, settled.change, settled.when);
}

/// Places every moved change again as the same accepted change on the next
/// legal frame, unless a newer change of its setting decides it.
pub(crate) fn place_moved<T, S>(state: &mut SessionState<T, S>, moved: Vec<Moved>) {
    for Moved {
        accepted,
        change,
        when,
    } in moved
    {
        let outcome = if accepted.superseded() {
            Err(Rejection::Refused(PlayError::Superseded))
        } else {
            match state.place(Some(accepted.caller()), change, When::Next) {
                Ok(Some(_)) => continue,
                Ok(None) => Err(Rejection::Refused(PlayError::Superseded)),
                Err(PlayError::Late) => Err(Rejection::Late),
                Err(error) => Err(Rejection::Refused(error)),
            }
        };
        answer(state, &accepted, outcome, change, when);
    }
}

/// Gives the caller of `accepted` its one answer and reports a change that
/// was not applied.
fn answer<T, S>(
    state: &mut SessionState<T, S>,
    accepted: &Accepted,
    outcome: Result<SessionFrame, Rejection<PlayError>>,
    change: HostSettingsChange,
    when: When<SessionFrame>,
) {
    let answer = accepted.settled(outcome);
    let HostSettled::Settings {
        outcome: Err(rejection),
        ..
    } = &answer
    else {
        state.settled.push(answer);
        return;
    };
    match rejection {
        Rejection::Refused(PlayError::Superseded) => {}
        Rejection::Unanswered => {
            error!(change = ?change, "the transport dropped a host settings change unanswered");
        }
        rejection => {
            warn!(?rejection, change = ?change, "host settings change was not applied");
            if let When::At(_) = when {
                let reason = format!("host settings change was not applied: {rejection:?}");
                publish_transport_event(
                    state,
                    &TransportEvent::Failed {
                        revision: None,
                        reason,
                    },
                );
            }
        }
    }
    state.settled.push(answer);
}
