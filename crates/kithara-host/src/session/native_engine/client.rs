use super::*;

impl<C: Send + 'static> HostDispatcher<C> for SessionClient<C> {
    fn dispatch(&self, command: C) -> Result<Ticket<PlayError>, HostDispatchError> {
        let ticket = self.postbox.post(command).map_err(not_taken)?;
        Ok(ticket)
    }
    fn shutdown(&self) {
        let (completion, completed) = mpsc::channel();
        let sent = self.cmd_tx.lock().send(EngineMsg::Shutdown(completion));
        if sent.is_ok() {
            let _ = completed.recv();
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
