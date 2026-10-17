use kithara_bufpool::SampleBuffer;
use kithara_command::Ticket;
use kithara_platform::{
    maybe_send::MaybeSend,
    sync::{Arc, Mutex, mpsc},
};
use kithara_play::PlayError;

use super::{
    OfflineSessionError, OfflineTaskRoute,
    task::{OfflineMsg, OfflineRequest},
};
use crate::session::{
    decks::{DeckInbox, DeckMsg},
    protocol::{HostDispatchError, HostDispatcher, HostPostbox, not_taken},
};

pub(crate) struct OfflineSessionClient<C> {
    postbox: HostPostbox<C>,
    cmd_tx: Arc<Mutex<mpsc::Sender<OfflineMsg>>>,
    control: OfflineTaskRoute,
}

impl<C> OfflineSessionClient<C> {
    pub(super) fn new(
        postbox: HostPostbox<C>,
        cmd_tx: mpsc::Sender<OfflineMsg>,
        control: OfflineTaskRoute,
    ) -> Self {
        Self {
            postbox,
            cmd_tx: Arc::new(Mutex::new(cmd_tx)),
            control,
        }
    }
    pub(crate) fn position(&self) -> Result<u64, OfflineSessionError> {
        let (answer, receipt) = mpsc::channel();
        self.send(OfflineMsg::Request(OfflineRequest::Position(answer)))
            .map_err(|_| OfflineSessionError::SessionGone)?;
        receive(&receipt, "position").map_err(|_| OfflineSessionError::SessionGone)
    }
    pub(crate) fn render(
        &self,
        position: u64,
        frames: u32,
    ) -> Result<SampleBuffer, OfflineSessionError> {
        let (answer, receipt) = mpsc::channel();
        self.send(OfflineMsg::Request(OfflineRequest::Render {
            position,
            frames,
            answer,
        }))
        .map_err(|_| OfflineSessionError::SessionGone)?;
        receive(&receipt, "render").map_err(|_| OfflineSessionError::SessionGone)?
    }
    fn send(&self, message: OfflineMsg) -> Result<(), PlayError> {
        self.cmd_tx
            .lock()
            .send(message)
            .map_err(|_| PlayError::SessionGone {
                reason: "offline session stopped taking commands",
            })?;
        self.control.wake();
        Ok(())
    }
}

impl<C: MaybeSend + 'static> HostDispatcher<C> for OfflineSessionClient<C> {
    fn dispatch(&self, command: C) -> Result<Ticket<PlayError>, HostDispatchError> {
        let ticket = self.postbox.post(command).map_err(not_taken)?;
        self.send(OfflineMsg::Posted)
            .map_err(HostDispatchError::NotTaken)?;
        Ok(ticket)
    }
    fn shutdown(&self) {
        let (completion, completed) = mpsc::channel();
        if self.send(OfflineMsg::Shutdown(completion)).is_ok() {
            #[cfg(not(target_arch = "wasm32"))]
            let _ = receive(&completed, "shutdown");
            #[cfg(target_arch = "wasm32")]
            drop(completed);
        }
    }
}

impl<C: MaybeSend + 'static> DeckInbox for OfflineSessionClient<C> {
    fn post(&self, message: DeckMsg) -> Result<(), PlayError> {
        self.send(OfflineMsg::Deck(message))
    }

    #[cfg(target_arch = "wasm32")]
    fn waker(self: Arc<Self>, message: DeckMsg) -> std::task::Waker {
        std::task::Waker::from(Arc::new(OfflineDeckWake {
            cmd_tx: self.cmd_tx.clone(),
            control: self.control.clone(),
            message,
        }))
    }
}

#[cfg(target_arch = "wasm32")]
struct OfflineDeckWake {
    cmd_tx: Arc<Mutex<mpsc::Sender<OfflineMsg>>>,
    control: OfflineTaskRoute,
    message: DeckMsg,
}

#[cfg(target_arch = "wasm32")]
impl std::task::Wake for OfflineDeckWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        if self
            .cmd_tx
            .lock()
            .send(OfflineMsg::Deck(self.message))
            .is_ok()
        {
            self.control.wake();
        }
    }
}

#[kithara_test_utils::kithara::hang_watchdog(ctx = String, timeout = kithara_test_utils::hang::default_timeout().min(kithara_platform::time::Duration::from_secs(4)))]
fn receive<R>(receipt: &mpsc::Receiver<R>, operation: &str) -> Result<R, mpsc::RecvTimeoutError> {
    loop {
        hang_tick!(format!("offline session has not answered {operation}"));
        let result = receipt
            .recv_timeout(kithara_platform::time::Instant::now() + __hang_detector.remaining());
        if !matches!(result, Err(mpsc::RecvTimeoutError::Timeout)) {
            hang_reset!();
            return result;
        }
        hang_tick!(format!("offline session has not answered {operation}"));
    }
}
