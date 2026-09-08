use kithara_audio::ConsumerWakeMode;
use kithara_bufpool::SampleBuffer;
use kithara_platform::sync::{Mutex, mpsc};
use kithara_play::{PlayError, SessionDispatcher};
use kithara_worker::TaskControl;

use super::{OfflineMsg, OfflineSessionError};
use crate::session::{
    Cmd, HostCmd, HostDispatcher, HostReply, Reply,
    protocol::{HostCmdMsg, HostDispatchError},
};

pub(crate) struct OfflineSessionClient<S> {
    cmd_tx: Mutex<mpsc::Sender<OfflineMsg<S>>>,
    control: TaskControl,
}

impl<S> OfflineSessionClient<S> {
    pub(super) fn new(cmd_tx: mpsc::Sender<OfflineMsg<S>>, control: TaskControl) -> Self {
        Self {
            control,
            cmd_tx: Mutex::new(cmd_tx),
        }
    }

    fn call(&self, cmd: HostCmd<S>) -> Result<HostReply, HostDispatchError<S>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        let message = OfflineMsg::Host(HostCmdMsg { cmd, reply_tx });
        if let Err(message) = self.send(message) {
            let OfflineMsg::Host(message) = *message else {
                return Err(HostDispatchError::after_send(PlayError::Internal(
                    "offline Host command changed protocol variant before send".into(),
                )));
            };
            return Err(HostDispatchError::before_send(
                PlayError::SessionGone {
                    reason: "offline session stopped accepting commands",
                },
                message.cmd,
            ));
        }
        reply_rx.recv().map_err(|_| {
            HostDispatchError::after_send(PlayError::SessionGone {
                reason: "offline session dropped the reply channel",
            })
        })
    }

    pub(crate) fn position(&self) -> Result<u64, OfflineSessionError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(OfflineMsg::Position { reply_tx })
            .map_err(|_| OfflineSessionError::SessionGone)?;
        reply_rx
            .recv()
            .map_err(|_| OfflineSessionError::SessionGone)
    }

    pub(crate) fn render(
        &self,
        position: u64,
        frames: u32,
    ) -> Result<SampleBuffer, OfflineSessionError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(OfflineMsg::Render {
            position,
            frames,
            reply_tx,
        })
        .map_err(|_| OfflineSessionError::SessionGone)?;
        reply_rx
            .recv()
            .map_err(|_| OfflineSessionError::SessionGone)?
    }

    fn send(&self, message: OfflineMsg<S>) -> Result<(), Box<OfflineMsg<S>>> {
        self.cmd_tx
            .lock()
            .send(message)
            .map_err(|error| Box::new(error.0))?;
        self.control.wake();
        Ok(())
    }
}

impl<S: Send + Sync + 'static> SessionDispatcher<S> for OfflineSessionClient<S> {
    /// Offline render pulls the graph from the session task, an ordinary thread
    /// that may block and read the clock, so a reader wakes its producer inline.
    fn consumer_wake_mode(&self) -> ConsumerWakeMode {
        ConsumerWakeMode::ImmediateOffRt
    }

    fn exec(&self, cmd: Cmd<S>) -> Result<Reply, PlayError> {
        match self.call(HostCmd::Play(cmd)).map_err(PlayError::from)? {
            HostReply::Play(reply) => Ok(reply),
            HostReply::Err(error) => Err(error),
            _ => Err(PlayError::Internal(
                "unexpected offline Host reply for player command".into(),
            )),
        }
    }
}

impl<S: Send + Sync + 'static> HostDispatcher<S> for OfflineSessionClient<S> {
    fn exec_host(&self, cmd: HostCmd<S>) -> Result<HostReply, HostDispatchError<S>> {
        self.call(cmd)
    }
}
