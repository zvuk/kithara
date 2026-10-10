use super::*;

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

pub(crate) fn receive_message<M>(
    cmd_rx: &mpsc::Receiver<M>,
    active: bool,
    deadline: Instant,
) -> Result<Option<M>, mpsc::RecvTimeoutError> {
    if active {
        match cmd_rx.recv_timeout(deadline) {
            Ok(message) => Ok(Some(message)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(error) => Err(error),
        }
    } else {
        cmd_rx
            .recv()
            .map(Some)
            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
    }
}

pub(super) fn engine_thread<S, O: HostOwner<S>>(
    cmd_rx: &mpsc::Receiver<EngineMsg>,
    mut mailbox: HostMailbox<O::Command>,
    mut owner: O,
) {
    let mut posts = OwnerPosts::new();
    let mut shutdown_completion = None;
    let mut deadline = Instant::now() + consts::SESSION_PUMP_INTERVAL;
    while let Ok(message) = receive_message(cmd_rx, true, deadline) {
        owner.begin_pass();
        let mut next = message;
        let mut shutdown = false;
        loop {
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
        if shutdown {
            break;
        }
        deadline = Instant::now() + consts::SESSION_PUMP_INTERVAL;
    }
    drop(owner);
    drop(shutdown_completion);
}
