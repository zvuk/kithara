//! The Host queue: Host settings changes the session transport applies on
//! its render clock, and the receipts that settle them on the session owner.

use std::{convert::Infallible, mem, num::NonZeroU32};

use kithara_command::{LiveError, Outcome, Protocol, Receipt, Rejection, SendError, Seq, When};
use kithara_config::ConfigOwner;
use kithara_play::PlayError;
use kithara_signal::SessionFrame;
use tracing::{error, warn};

use super::{
    SessionError,
    dispatch::invalidate_audio_route,
    state::SessionState,
    transport::{TransportEvent, TransportProcessError, publish_transport_event},
};
use crate::{
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
    /// to place a frame on; with one, it goes to the transport, which applies
    /// it on the asked frame.
    fn exec_live(
        &mut self,
        change: HostSettingsChange,
        at: Self::At,
        _cx: &mut (),
    ) -> Self::Output {
        self.check_when(at)?;
        let Some(channel) = self.channel.as_mut() else {
            self.settings.apply(change)?;
            self.publish_root();
            return Ok(None);
        };
        self.settings
            .send(channel, at, change, HostPart::Settings)
            .map(Some)
            .map_err(|error| match error {
                LiveError::Invalid(error) => error,
                LiveError::Send(SendError::Full(_)) => SessionError::HostQueueFull.into(),
                LiveError::Send(SendError::Closed(_)) => PlayError::Closed,
                LiveError::Send(SendError::Target(_)) => {
                    PlayError::Internal("a host settings batch names a target".to_owned())
                }
            })
    }
}

pub(crate) fn settle_root_receipts<T, S>(state: &mut SessionState<T, S>) {
    while let Some(receipt) = state
        .channel
        .as_mut()
        .and_then(kithara_command::ScopedSender::root_receipt)
    {
        settle_receipt(state, &receipt);
    }
}

/// Settles one root receipt the owner routed here. An applied change moves
/// into the settings the Host reads; a tempo it changed is announced. A
/// change for the next block the transport refused goes out again; any other
/// rejected change is dropped and reported.
pub(crate) fn settle_receipt<T, S>(
    state: &mut SessionState<T, S>,
    receipt: &Receipt<HostProtocol>,
) {
    let before = *state.settings.config();
    let Some(settled) = state.settings.settle(receipt) else {
        return;
    };
    if matches!(receipt.outcome(), Outcome::Rejected(Rejection::Refused(_)))
        && settled.when == When::Next
        && send_again(state, receipt, settled.change).is_some()
    {
        return;
    }
    let outcome = match receipt.outcome() {
        Outcome::Applied { at, .. } => Ok(*at),
        Outcome::Rejected(Rejection::Late) => Err(Rejection::Late),
        Outcome::Rejected(Rejection::Stale) => Err(Rejection::Stale),
        Outcome::Rejected(Rejection::Unanswered) => Err(Rejection::Unanswered),
        Outcome::Rejected(Rejection::Refused(reason)) => {
            Err(Rejection::Refused(PlayError::Internal(reason.to_string())))
        }
    };
    state.settled.push(crate::HostSettled::Settings {
        seq: receipt.seq(),
        change: settled.change,
        outcome,
    });
    match receipt.outcome() {
        Outcome::Applied { data, .. } => {
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
        }
        Outcome::Rejected(Rejection::Unanswered) => {
            error!(change = ?settled.change, "the transport dropped a host settings change unanswered");
        }
        Outcome::Rejected(rejection) => {
            warn!(?rejection, change = ?settled.change, "host settings change was not applied");
            if let When::At(_) = settled.when {
                publish_transport_event(
                    state,
                    &TransportEvent::Failed {
                        revision: None,
                        reason: format!("host settings change was not applied: {rejection:?}"),
                    },
                );
            }
        }
    }
}

/// Sends a change for the next block the transport refused once more, unless
/// a newer change of the same field for the next block is already on its way
/// and decides the setting instead.
fn send_again<T, S>(
    state: &mut SessionState<T, S>,
    receipt: &Receipt<HostProtocol>,
    change: HostSettingsChange,
) -> Option<Seq> {
    let superseded = state.settings.pending().any(|(seq, when, pending)| {
        seq > receipt.seq()
            && when == When::Next
            && mem::discriminant(&pending) == mem::discriminant(&change)
    });
    if superseded {
        return None;
    }
    match state.exec(change, When::Next, &mut ()) {
        Ok(sequence) => sequence,
        Err(error) => {
            warn!(%error, ?change, "a refused host settings change could not be sent again");
            None
        }
    }
}
