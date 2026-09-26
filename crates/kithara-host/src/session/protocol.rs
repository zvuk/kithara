use firewheel::FirewheelContext;
use kithara_output::OutputGroup;
use kithara_platform::sync::mpsc;
pub(crate) use kithara_play::{
    AllocatedSlot, Cmd, PlayerId, PlayerLevel, Reply, SessionDispatcher, SessionError,
    SessionSampleRate,
};
use kithara_play::{PlayError, player::ResidentLoadObservation};
use kithara_sync::{
    SyncAdmission, SyncError, SyncIntent, SyncOperation, SyncRejected, TopologyOperation,
};
use kithara_warp::BeatGridId;

use crate::{
    PlayerMember,
    api::{DeckSyncState, HostLevel},
};

/// Opens the audio stream a session runs on and hands back the object that
/// owns it. Firewheel no longer holds the backend, so the session keeps the
/// returned stream alive for as long as its context.
pub(crate) type StartStreamFn<T> =
    Box<dyn FnMut(&mut FirewheelContext, u32) -> Result<T, String> + Send + 'static>;

pub(crate) enum HostCmd<S> {
    Play(Cmd<S>),
    Sync(SyncCmd),
    ApplyMix { levels: Box<[HostLevel]> },
    EnableOutput { outputs: OutputGroup },
    Shutdown,
}

pub(crate) enum SyncCmd {
    Transact(SyncOperation<PlayerMember>),
    TransactCurrent(Box<[TopologyOperation<PlayerMember>]>),
    QueryDeckState {
        target: BeatGridId,
    },
    RequestDeckSync {
        target: BeatGridId,
        member: BeatGridId,
        intent: SyncIntent,
        observation: Option<ResidentLoadObservation>,
    },
}

pub(crate) enum HostReply {
    Play(Reply),
    Admission(Result<SyncAdmission, SyncRejected<PlayerMember>>),
    DeckSyncState(DeckSyncState),
    Ok,
    Err(PlayError),
}

pub(crate) struct HostCmdMsg<S> {
    pub(crate) cmd: HostCmd<S>,
    pub(crate) reply_tx: mpsc::Sender<HostReply>,
}

pub(crate) struct HostDispatchError<S> {
    command: Option<Box<HostCmd<S>>>,
    error: PlayError,
}

impl<S> HostDispatchError<S> {
    pub(crate) const fn after_send(error: PlayError) -> Self {
        Self {
            error,
            command: None,
        }
    }

    pub(crate) fn before_send(error: PlayError, command: HostCmd<S>) -> Self {
        Self {
            error,
            command: Some(Box::new(command)),
        }
    }
}

impl<S> From<HostDispatchError<S>> for PlayError {
    fn from(error: HostDispatchError<S>) -> Self {
        error.error
    }
}

impl<S> From<HostDispatchError<S>> for (PlayError, Option<Box<HostCmd<S>>>) {
    fn from(error: HostDispatchError<S>) -> Self {
        (error.error, error.command)
    }
}

pub(crate) trait HostDispatcher<S>: SessionDispatcher<S> {
    fn exec_host(&self, cmd: HostCmd<S>) -> Result<HostReply, HostDispatchError<S>>;

    fn transact(
        &self,
        operation: SyncOperation<PlayerMember>,
    ) -> Result<SyncAdmission, SyncRejected<PlayerMember>> {
        match self.exec_host(HostCmd::Sync(SyncCmd::Transact(operation))) {
            Ok(HostReply::Admission(result)) => result,
            Err(error) => {
                let (reason, command) = error.into();
                if let Some(command) = command
                    && let HostCmd::Sync(SyncCmd::Transact(operation)) = *command
                {
                    return Err(SyncRejected::new(SyncError::OwnerUnavailable, operation));
                }
                owner_thread_fail_fast(&reason)
            }
            Ok(_) => owner_thread_fail_fast("unexpected transaction reply"),
        }
    }

    fn transact_current(
        &self,
        operations: Box<[TopologyOperation<PlayerMember>]>,
    ) -> Result<SyncAdmission, PlayError> {
        match self.exec_host(HostCmd::Sync(SyncCmd::TransactCurrent(operations))) {
            Ok(HostReply::Admission(result)) => result.map_err(|rejected| {
                let (error, _) = <(SyncError, SyncOperation<PlayerMember>)>::from(rejected);
                SessionError::from(error).into()
            }),
            Ok(HostReply::Err(error)) => Err(error),
            Err(error) => {
                let (reason, command) = error.into();
                if command.as_deref().is_some_and(|command| {
                    matches!(command, HostCmd::Sync(SyncCmd::TransactCurrent(_)))
                }) {
                    return Err(reason);
                }
                owner_thread_fail_fast(&reason)
            }
            Ok(_) => owner_thread_fail_fast("unexpected current-topology transaction reply"),
        }
    }
}

fn owner_thread_fail_fast(reason: impl std::fmt::Display) -> ! {
    panic!("canonical host owner thread stopped after accepting transferred ownership: {reason}")
}
