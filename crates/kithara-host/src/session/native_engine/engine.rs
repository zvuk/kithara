use std::task::Wake;

use kithara_platform::{
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use kithara_test_utils::{hang, kithara};

use super::super::{
    decks::DeckMsg,
    dispatch::OwnerPosts,
    protocol::{HostMailbox, HostPostbox},
};
use crate::{HostOwner, consts};

pub(crate) enum EngineMsg {
    Posted,
    Deck(DeckMsg),
    Shutdown(mpsc::Sender<()>),
}

pub(crate) struct SessionClient<C> {
    pub(super) postbox: HostPostbox<C>,
    pub(super) cmd_tx: Mutex<mpsc::Sender<EngineMsg>>,
}

pub(super) struct SessionWake(pub(super) Mutex<mpsc::Sender<EngineMsg>>);
impl Wake for SessionWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        drop(self.0.lock().send(EngineMsg::Posted));
    }
}

#[kithara::hang_watchdog(ctx = String, timeout = hang::default_timeout().min(Duration::from_secs(4)))]
pub(crate) fn receive_message<M>(
    cmd_rx: &mpsc::Receiver<M>,
    active: bool,
    deadline: Instant,
) -> Result<Option<M>, mpsc::RecvTimeoutError> {
    loop {
        hang_tick!(format!("native session message: active={active}"));
        let budget = Instant::now() + __hang_detector.remaining();
        match cmd_rx.recv_timeout(if active { deadline.min(budget) } else { budget }) {
            Ok(message) => {
                hang_reset!();
                return Ok(Some(message));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                hang_tick!(format!("native session message: active={active}"));
                if active {
                    return Ok(None);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[kithara::hang_watchdog(ctx = String, timeout = hang::default_timeout().min(Duration::from_secs(4)))]
pub(super) fn engine_thread<S, O: HostOwner<S>>(
    cmd_rx: &mpsc::Receiver<EngineMsg>,
    mut mailbox: HostMailbox<O::Command>,
    mut owner: O,
) {
    let mut posts = OwnerPosts::new();
    let mut shutdown_completion = None;
    let mut deadline = Instant::now() + consts::SESSION_PUMP_INTERVAL;
    while let Ok(message) = receive_message(cmd_rx, true, deadline) {
        hang_tick!("native engine pass has not completed".to_owned());
        owner.begin_pass();
        let mut next = message;
        let mut shutdown = false;
        loop {
            hang_tick!("native engine is draining queued messages".to_owned());
            match next {
                Some(EngineMsg::Posted) => posts.drain(&mut owner, &mut mailbox),
                Some(EngineMsg::Deck(message)) => message.run(&mut owner),
                Some(EngineMsg::Shutdown(completion)) => {
                    shutdown_completion = Some(completion);
                    shutdown = true;
                    break;
                }
                None => {}
            }
            match cmd_rx.try_recv() {
                Ok(message) => next = Some(message),
                Err(_) => break,
            }
        }
        posts.drain(&mut owner, &mut mailbox);
        posts.pass(&mut owner, true);
        hang_tick!("native engine pass has not completed".to_owned());
        if shutdown {
            break;
        }
        hang_reset!();
        deadline = Instant::now() + consts::SESSION_PUMP_INTERVAL;
    }
    drop(owner);
    drop(shutdown_completion);
}
