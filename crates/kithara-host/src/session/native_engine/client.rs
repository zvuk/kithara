use super::*;

impl<C: Send + 'static> HostDispatcher<C> for SessionClient<C> {
    fn dispatch(&self, command: C) -> Result<Ticket<PlayError>, HostDispatchError> {
        let ticket = self.postbox.post(command).map_err(not_taken)?;
        Ok(ticket)
    }
    #[kithara_test_utils::kithara::hang_watchdog(ctx = String, timeout = kithara_test_utils::hang::default_timeout().min(kithara_platform::time::Duration::from_secs(4)))]
    fn shutdown(&self) {
        let (completion, completed) = mpsc::channel();
        let sent = self.cmd_tx.lock().send(EngineMsg::Shutdown(completion));
        if sent.is_ok() {
            loop {
                hang_tick!("native session owner has not finished shutdown".to_owned());
                let result = completed.recv_timeout(Instant::now() + __hang_detector.remaining());
                if !matches!(result, Err(mpsc::RecvTimeoutError::Timeout)) {
                    hang_reset!();
                    break;
                }
                hang_tick!("native session owner has not finished shutdown".to_owned());
            }
        }
    }
}

impl<C: Send + 'static> DeckInbox for SessionClient<C> {
    fn post(&self, message: DeckMsg) -> Result<(), PlayError> {
        self.cmd_tx
            .lock()
            .send(EngineMsg::Deck(message))
            .map_err(|_| PlayError::SessionGone {
                reason: "session thread stopped accepting deck wakes",
            })
    }
}
